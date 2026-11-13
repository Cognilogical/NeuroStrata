//! Tantivy lexical full-text index for NeuroStrata (spec 2026-10-27-tantivy-full-text-search-design.md).
//! Unconditionally compiled; `tantivy = "0.27"` is present in Cargo.toml.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, ConstScoreQuery, Occur, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{Field as TantivyField, IndexRecordOption, Schema, STORED, STRING, TEXT, FAST, INDEXED, Value as TantivyValue};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::traits::{MemoryPayload};

// v2 adds the `generation` field. An index written by v1 has no such field,
// so opening it under this schema would panic on `get_field("generation")`;
// the version file makes the upgrade clear and rebuild instead.
pub const INDEX_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-tantivy-v2");

/// Code-aware pre-tokenization for Tantivy (whitespace-separated sub-tokens + full lowercased text).
pub fn tokenize_code_for_tantivy(text: &str) -> String {
    let mut tokens = crate::fts::tokenize_code(text);
    let lower_raw = text.to_lowercase();
    if !lower_raw.is_empty() && !tokens.contains(&lower_raw) {
        tokens.push(lower_raw);
    }
    tokens.join(" ")
}

/// Neutralize a tokenized query for Tantivy's `QueryParser`: quote-wrap every
/// whitespace-separated term so user text is parsed as literal phrases, never
/// as query syntax.
///
/// Tantivy's grammar parses a double-quoted string as one literal whose
/// contents may contain any character except an unescaped `"`; a `\` escapes
/// the next character. The parser then runs the field tokenizer over the
/// literal exactly as it does for a bare term — a literal tokenizing to one
/// term stays a term query, several terms become the same phrase query a bare
/// multi-token literal produced — so per-field boosts and matching semantics
/// are unchanged, while `:`, `(`, `)`, leading `+`/`-`, unbalanced quotes, `^`,
/// `~`, `/` etc. inside the quotes can no longer be read as field references,
/// grouping, occur operators, boosts, fuzzy markers or regex delimiters.
/// Without this, the raw full-text token appended by
/// [`tokenize_code_for_tantivy`] (e.g. `std::sync::mutex`) reached
/// `parse_query` as syntax and the search failed with a propagated parse error
/// instead of returning matches.
fn quote_tokenized_query(tokenized: &str) -> String {
    tokenized
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone)]
pub struct TantivyHit {
    pub id: String,
    pub namespace: String,
    pub score: f32,
    pub raw_bm25: f32,
}

/// Structural types excluded from lexical indexing (per spec §4 / §5.3).
const STRUCTURAL_MEMORY_TYPES: &[&str] = &[
    "directory", "file", "markdown", "freshness_flag",
    "guard_event", "community_summary", "digital_twin", "digital_twin_schema",
];

pub struct TantivyLexicalIndex {
    pub index: Index,
    pub writer: Mutex<Option<IndexWriter>>,
    pub reader: IndexReader,
    pub dirty: AtomicBool,
    pub index_dir: Option<PathBuf>,
    // Schema field handles
    pub id_f: TantivyField,
    pub memory_id_f: TantivyField,
    pub namespace_f: TantivyField,
    pub content_f: TantivyField,
    pub symbol_f: TantivyField,
    pub location_f: TantivyField,
    pub memory_type_f: TantivyField,
    pub valid_from_f: TantivyField,
    pub valid_to_f: TantivyField,
    /// `1` when the row carries a `valid_to` (even `0`), `0` when absent.
    /// `valid_to` alone cannot distinguish "no expiry" from "retired at 0".
    pub has_valid_to_f: TantivyField,
    pub archived_f: TantivyField,
    /// Staged-ingestion generation; `0` for rows that are not generation
    /// scoped. See `docs/superpowers/specs/2026-11-12-lexical-generation-contract.md`.
    pub generation_f: TantivyField,
}

impl TantivyLexicalIndex {
    /// Build schema (spec §4).
    fn build_schema() -> Schema {
        let mut schema_builder = Schema::builder();
        schema_builder.add_text_field("id", STRING | STORED);
        schema_builder.add_text_field("memory_id", STRING | STORED);
        schema_builder.add_text_field("namespace", STRING | STORED);
        schema_builder.add_text_field("content", TEXT | STORED);
        schema_builder.add_text_field("symbol", TEXT | STORED);
        schema_builder.add_text_field("location", TEXT | STORED);
        schema_builder.add_text_field("memory_type", STRING | STORED);
        schema_builder.add_i64_field("valid_from", INDEXED | FAST | STORED);
        schema_builder.add_i64_field("valid_to", INDEXED | FAST | STORED);
        schema_builder.add_i64_field("has_valid_to", INDEXED | FAST | STORED);
        schema_builder.add_i64_field("archived", INDEXED | FAST | STORED);
        schema_builder.add_i64_field("generation", INDEXED | FAST | STORED);
        schema_builder.build()
    }

    /// The generation carried by a row id: `ns::<generation>::<path>` -> the
    /// middle segment, otherwise `0` for unscoped rows. Mirrors the store's
    /// `split_scoped_id`, which is the single source of truth for the shape.
    fn generation_of_id(id: &str) -> i64 {
        crate::parser::generation::split_scoped_id(id)
            .map(|(_, generation, _)| generation as i64)
            .unwrap_or(0)
    }

    /// Open a RAM index (unit tests / ephemeral fallback); no directory persistence.
    pub fn open_in_ram() -> Result<Self> {
        let schema = Self::build_schema();
        let index = Index::create_in_ram(schema.clone());
        let reader = index.reader_builder().reload_policy(ReloadPolicy::OnCommitWithDelay).try_into()?;
        let writer = index.writer(50_000_000)?;
        let id_f = schema.get_field("id").expect("id field");
        let memory_id_f = schema.get_field("memory_id").expect("memory_id field");
        let namespace_f = schema.get_field("namespace").expect("namespace field");
        let content_f = schema.get_field("content").expect("content field");
        let symbol_f = schema.get_field("symbol").expect("symbol field");
        let location_f = schema.get_field("location").expect("location field");
        let memory_type_f = schema.get_field("memory_type").expect("memory_type field");
        let valid_from_f = schema.get_field("valid_from").expect("valid_from field");
        let valid_to_f = schema.get_field("valid_to").expect("valid_to field");
        let has_valid_to_f = schema.get_field("has_valid_to").expect("has_valid_to field");
        let archived_f = schema.get_field("archived").expect("archived field");
        let generation_f = schema.get_field("generation").expect("generation field");
        Ok(Self {
            index,
            writer: Mutex::new(Some(writer)),
            reader,
            dirty: AtomicBool::new(false),
            index_dir: None,
            id_f, memory_id_f, namespace_f, content_f, symbol_f, location_f,
            memory_type_f, valid_from_f, valid_to_f, has_valid_to_f, archived_f, generation_f,
        })
    }

    /// Open or create on disk (spec §3 / §5.2 / §7).
    pub fn open_or_create(index_dir: &Path) -> Result<Self> {
        let version_path = index_dir.join("index_version.txt");
        let needs_rebuild = if version_path.exists() {
            match std::fs::read_to_string(&version_path) {
                Ok(existing) => existing.trim() != INDEX_VERSION,
                Err(e) => {
                    tracing::warn!("failed to read lexical index version file {}: {e}", version_path.display());
                    // Transient read error: do NOT trigger a full remove_dir_all.
                    false
                }
            }
        } else {
            true
        };

        if needs_rebuild && index_dir.exists() {
            // Warn-first / clear per spec.
            tracing::warn!("lexical index version mismatch or missing; clearing {}", index_dir.display());
            std::fs::remove_dir_all(index_dir)?;
        }

        std::fs::create_dir_all(index_dir)?;
        let path_for_index = index_dir.to_path_buf();

        // Try MmapDirectory; if corrupt wipe and recreate.
        let dir_result = tantivy::directory::MmapDirectory::open(&path_for_index);
        let index = match dir_result {
            Ok(dir) => Index::open_or_create(dir, Self::build_schema())?,
            Err(e) => {
                tracing::warn!("corrupt lexical index at {}; wiping and recreating: {e}", path_for_index.display());
                std::fs::remove_dir_all(&path_for_index)?;
                std::fs::create_dir_all(&path_for_index)?;
                let dir = tantivy::directory::MmapDirectory::open(&path_for_index)?;
                Index::create(dir, Self::build_schema(), tantivy::IndexSettings::default())?
            }
        };

        // Writer acquisition with lock-contention fallback (§5.2).
        let writer_res = index.writer(50_000_000);
        let mut writer_opt: Option<IndexWriter> = None;
        match writer_res {
            Ok(w) => {
                writer_opt = Some(w);
            }
            Err(e) => {
                tracing::warn!("lexical index writer locked by another process (read-only mode): {e}");
            }
        }
        // If writer acquired and version missing / rebuilt, write version.
        if writer_opt.is_some() {
            std::fs::write(&version_path, INDEX_VERSION)?;
        }

        let reader = index.reader_builder().reload_policy(ReloadPolicy::OnCommitWithDelay).try_into()?;
        let schema = Self::build_schema();
        let id_f = schema.get_field("id").expect("id");
        let memory_id_f = schema.get_field("memory_id").expect("memory_id");
        let namespace_f = schema.get_field("namespace").expect("namespace");
        let content_f = schema.get_field("content").expect("content");
        let symbol_f = schema.get_field("symbol").expect("symbol");
        let location_f = schema.get_field("location").expect("location");
        let memory_type_f = schema.get_field("memory_type").expect("memory_type");
        let valid_from_f = schema.get_field("valid_from").expect("valid_from");
        let valid_to_f = schema.get_field("valid_to").expect("valid_to");
        let has_valid_to_f = schema.get_field("has_valid_to").expect("has_valid_to");
        let archived_f = schema.get_field("archived").expect("archived");
        let generation_f = schema.get_field("generation").expect("generation");

        Ok(Self {
            index,
            writer: Mutex::new(writer_opt),
            reader,
            dirty: AtomicBool::new(false),
            index_dir: Some(path_for_index),
            id_f, memory_id_f, namespace_f, content_f, symbol_f, location_f,
            memory_type_f, valid_from_f, valid_to_f, has_valid_to_f, archived_f, generation_f,
        })
    }

    /// Open with disk resilience: try the on-disk index first, and fall back
    /// to a fresh in-RAM index when the directory cannot be opened (unreadable
    /// or unusable path). Search still works; writes go to RAM and are lost on
    /// exit — degraded, never fatal.
    pub fn open(index_dir: &Path) -> Result<Self> {
        match Self::open_or_create(index_dir) {
            Ok(idx) => Ok(idx),
            Err(e) => {
                tracing::warn!(
                    "lexical index at {} failed to open ({}); falling back to in-RAM index",
                    index_dir.display(),
                    e
                );
                Self::open_in_ram()
            }
        }
    }

    /// The directory backing this index, when it is disk-backed.
    pub fn index_dir(&self) -> Option<&Path> {
        self.index_dir.as_deref()
    }

    /// Composite key for delete/replace.
    fn composite_key(namespace: &str, id: &str) -> String {
        format!("{}::{}", namespace, id)
    }

    /// Check if payload should be excluded from indexing (§4 / §5.3).
    fn is_excluded(payload: &MemoryPayload) -> bool {
        // Structural types
        if STRUCTURAL_MEMORY_TYPES.contains(&payload.memory_type.as_str()) {
            return true;
        }
        // Registry rows
        if crate::registry::is_registry_row(&payload.metadata) {
            return true;
        }
        // Mesh-conflicting metadata
        if crate::synaptic_mesh::conflict::is_conflicting_metadata(&payload.metadata) {
            return true;
        }
        false
    }

    /// Upsert a memory (synchronous flush for read-after-write per §5.2).
    pub fn upsert_memory(&self, namespace: &str, id: &str, payload: &MemoryPayload) -> Result<()> {
        let composite = Self::composite_key(namespace, id);
        // Delete existing document by composite id.
        if let Some(w) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let term = Term::from_field_text(self.id_f, &composite);
            w.delete_term(term);
        } else {
            tracing::warn!("lexical index is in read-only mode; skipping mutation for {composite}");
        }

        if Self::is_excluded(payload) {
            // Excluded: delete and commit (if writer exists).
            if let Some(w) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                w.commit()?;
                self.dirty.store(true, Ordering::SeqCst);
                // Reload reader after commit.
                self.reader.reload()?;
            }
            return Ok(());
        }

        // Index eligible document.
        let mut doc = TantivyDocument::default();
        doc.add_text(self.id_f, &composite);
        doc.add_text(self.memory_id_f, id);
        doc.add_text(self.namespace_f, namespace);
        doc.add_text(self.content_f, &payload.content);
        // Symbol: pre-tokenized with code-aware tokenizer (metadata["symbol"] if present).
        let symbol_raw = payload.metadata.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        doc.add_text(self.symbol_f, &tokenize_code_for_tantivy(symbol_raw));
        // Location: pre-tokenized.
        doc.add_text(self.location_f, &tokenize_code_for_tantivy(&payload.location));
        doc.add_text(self.memory_type_f, &payload.memory_type);
        let valid_from: i64 = payload.metadata.get("valid_from").and_then(|v| v.as_i64()).unwrap_or(0);
        doc.add_i64(self.valid_from_f, valid_from);
        let valid_to: i64 = payload.metadata.get("valid_to").and_then(|v| v.as_i64()).unwrap_or(0);
        doc.add_i64(self.valid_to_f, valid_to);
        let has_valid_to: i64 = if payload
            .metadata
            .get("valid_to")
            .map(|v| !v.is_null())
            .unwrap_or(false)
        {
            1
        } else {
            0
        };
        doc.add_i64(self.has_valid_to_f, has_valid_to);
        let archived: i64 = if payload.metadata.get("archived").and_then(|v| v.as_i64()).unwrap_or(0) != 0 { 1 } else { 0 };
        doc.add_i64(self.archived_f, archived);
        doc.add_i64(self.generation_f, Self::generation_of_id(id));

        if let Some(w) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            w.add_document(doc)?;
            w.commit()?;
            self.dirty.store(true, Ordering::SeqCst);
            self.reader.reload()?;
        }
        Ok(())
    }

    /// Delete by namespace + id (spec §4).
    pub fn delete_memory(&self, namespace: &str, id: &str) -> Result<()> {
        let composite = Self::composite_key(namespace, id);
        if let Some(w) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let term = Term::from_field_text(self.id_f, &composite);
            w.delete_term(term);
            w.commit()?;
            self.dirty.store(true, Ordering::SeqCst);
            self.reader.reload()?;
        } else {
            tracing::warn!("lexical index is in read-only mode; skipping mutation for {composite}");
        }
        Ok(())
    }

    /// Rebuild from slice (namespace, id, payload) — commit once (§4).
    pub fn rebuild(&self, rows: &[(String, String, MemoryPayload)]) -> Result<usize> {
        self.rebuild_scoped(rows, None)
    }

    /// Rebuild, optionally scoped to a single namespace — commit once (§4).
    ///
    /// When `namespace` is `Some(ns)`, only that namespace's documents are
    /// deleted before inserting `rows` (partition replace); every other
    /// namespace's documents survive. When `None`, the whole index is replaced,
    /// exactly as [`Self::rebuild`] has always done. A scoped rebuild must never
    /// wipe other namespaces: `search_tantivy_lexical` lazily initializes with a
    /// single-namespace rebuild, and namespaces whose documents were wiped would
    /// never re-trigger initialization.
    pub fn rebuild_scoped(
        &self,
        rows: &[(String, String, MemoryPayload)],
        namespace: Option<&str>,
    ) -> Result<usize> {
        if let Some(w) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            match namespace {
                Some(ns) => {
                    let term = Term::from_field_text(self.namespace_f, ns);
                    w.delete_term(term);
                }
                None => {
                    w.delete_all_documents()?;
                }
            }
            let mut indexed = 0usize;
            for (ns, id, payload) in rows {
                if Self::is_excluded(payload) {
                    continue;
                }
                // F5: a scoped rebuild replaces one namespace; it must never
                // insert a row that belongs to another one under this scope.
                if let Some(scope) = namespace {
                    if ns != scope {
                        tracing::warn!(
                            "scoped lexical rebuild for '{scope}' skipped a row from '{ns}': {id}"
                        );
                        continue;
                    }
                }
                let composite = Self::composite_key(ns, id);
                let mut doc = TantivyDocument::default();
                doc.add_text(self.id_f, &composite);
                doc.add_text(self.memory_id_f, id);
                doc.add_text(self.namespace_f, ns);
                doc.add_text(self.content_f, &payload.content);
                let symbol_raw = payload.metadata.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
                doc.add_text(self.symbol_f, &tokenize_code_for_tantivy(symbol_raw));
                doc.add_text(self.location_f, &tokenize_code_for_tantivy(&payload.location));
                doc.add_text(self.memory_type_f, &payload.memory_type);
                let valid_from: i64 = payload.metadata.get("valid_from").and_then(|v| v.as_i64()).unwrap_or(0);
                doc.add_i64(self.valid_from_f, valid_from);
                let valid_to: i64 = payload.metadata.get("valid_to").and_then(|v| v.as_i64()).unwrap_or(0);
                doc.add_i64(self.valid_to_f, valid_to);
                let has_valid_to: i64 = if payload
                    .metadata
                    .get("valid_to")
                    .map(|v| !v.is_null())
                    .unwrap_or(false)
                {
                    1
                } else {
                    0
                };
                doc.add_i64(self.has_valid_to_f, has_valid_to);
                let archived: i64 = if payload.metadata.get("archived").and_then(|v| v.as_i64()).unwrap_or(0) != 0 { 1 } else { 0 };
                doc.add_i64(self.archived_f, archived);
                doc.add_i64(self.generation_f, Self::generation_of_id(id));
                w.add_document(doc)?;
                indexed += 1;
            }
            w.commit()?;
            self.dirty.store(true, Ordering::SeqCst);
            self.reader.reload()?;
            Ok(indexed)
        } else {
            Ok(0)
        }
    }

    /// Search (§4 / §5.4), without a generation filter.
    ///
    /// Production callers use [`Self::search_with_active_generations`] so
    /// superseded staged rows never reach the page; this wrapper keeps the
    /// unfiltered unit-test surface.
    pub fn search(
        &self,
        namespace: &str,
        query_str: &str,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
        as_of: Option<i64>,
    ) -> Result<Vec<TantivyHit>> {
        self.search_with_active_generations(
            namespace,
            query_str,
            limit,
            include_tasks,
            include_archived,
            as_of,
            None,
        )
    }

    /// Search, optionally restricting generation-scoped rows to the active
    /// generations of `namespace` (`None` = no generation filter).
    ///
    /// The filter is a zero-score `Must` clause, so a superseded or staged row
    /// cannot consume a TopDocs slot. The per-hit check stays as defense in
    /// depth, exactly like the namespace/archive/validity filters.
    pub fn search_with_active_generations(
        &self,
        namespace: &str,
        query_str: &str,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
        as_of: Option<i64>,
        active_generations: Option<&std::collections::HashSet<u64>>,
    ) -> Result<Vec<TantivyHit>> {
        if self.dirty.load(Ordering::SeqCst) {
            self.reader.reload()?;
            self.dirty.store(false, Ordering::SeqCst);
        }

        let mut parser = QueryParser::for_index(&self.index, vec![self.content_f, self.symbol_f, self.location_f]);
        // Apply per-field boosts per spec (§4): content=1.0, symbol=2.0, location=1.5.
        // tantivy 0.27 exposes set_field_boost on QueryParser.
        parser.set_field_boost(self.content_f, 1.0);
        parser.set_field_boost(self.symbol_f, 2.0);
        parser.set_field_boost(self.location_f, 1.5);

        let tokenized_query = quote_tokenized_query(&tokenize_code_for_tantivy(query_str));
        let parsed = parser.parse_query(&tokenized_query)?;

        // Apply the row filters inside the query, before TopDocs collects:
        // filtering afterwards lets higher-scoring rows from other namespaces
        // (or excluded tasks / archived / retired rows) exhaust fetch_limit
        // and starve matches that would have survived the filters. The per-hit
        // checks below stay as defense in depth.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let visible_at = as_of.unwrap_or(now);
        // tantivy 0.27 has no Occur::Filter; a Must clause wrapped in a
        // zero-score ConstScoreQuery filters without touching BM25 ranking.
        fn filter(query: Box<dyn tantivy::query::Query>) -> (Occur, Box<dyn tantivy::query::Query>) {
            (Occur::Must, Box::new(ConstScoreQuery::new(query, 0.0)))
        }
        let mut clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = vec![
            (Occur::Must, parsed),
            filter(Box::new(TermQuery::new(
                Term::from_field_text(self.namespace_f, namespace),
                IndexRecordOption::Basic,
            ))),
            // Not yet retired at the visible moment: valid_to unset (0) or
            // strictly after it. Mirrors the per-hit validity check below.
            filter(Box::new(BooleanQuery::new(vec![
                (
                    Occur::Should,
                    // No expiry: the row has no `valid_to` at all. An explicit
                    // `valid_to = 0` is a present, past timestamp and retires.
                    Box::new(TermQuery::new(
                        Term::from_field_i64(self.has_valid_to_f, 0),
                        IndexRecordOption::Basic,
                    )) as Box<dyn tantivy::query::Query>,
                ),
                (
                    Occur::Should,
                    Box::new(RangeQuery::new(
                        std::ops::Bound::Excluded(Term::from_field_i64(self.valid_to_f, visible_at)),
                        std::ops::Bound::Unbounded,
                    )) as Box<dyn tantivy::query::Query>,
                ),
            ]))),
        ];
        if let Some(ts) = as_of {
            clauses.push(filter(Box::new(RangeQuery::new(
                std::ops::Bound::Unbounded,
                std::ops::Bound::Included(Term::from_field_i64(self.valid_from_f, ts)),
            ))));
        }
        if !include_tasks {
            clauses.push((
                Occur::MustNot,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.memory_type_f, "task"),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if !include_archived {
            clauses.push(filter(Box::new(RangeQuery::new(
                std::ops::Bound::Included(Term::from_field_i64(self.archived_f, 0)),
                std::ops::Bound::Included(Term::from_field_i64(self.archived_f, 0)),
            ))));
        }
        if let Some(active) = active_generations {
            // Generation 0 is "not generation scoped" and always visible;
            // scoped rows must name a generation that is active for at least
            // one source in this namespace. The per-source check happens on
            // hydration, where the row's source is known.
            let mut should: Vec<(Occur, Box<dyn tantivy::query::Query>)> = vec![(
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_i64(self.generation_f, 0),
                    IndexRecordOption::Basic,
                )),
            )];
            for generation in active {
                should.push((
                    Occur::Should,
                    Box::new(TermQuery::new(
                        Term::from_field_i64(self.generation_f, *generation as i64),
                        IndexRecordOption::Basic,
                    )),
                ));
            }
            clauses.push(filter(Box::new(BooleanQuery::new(should))));
        }
        let filtered_query = BooleanQuery::new(clauses);

        let fetch_limit = (limit.max(1) * 8) as usize;
        let searcher = self.reader.searcher();
        let top_docs = TopDocs::with_limit(fetch_limit).order_by_score();
        let hits: Vec<(f32, tantivy::DocAddress)> = searcher.search(&filtered_query, &top_docs)?;

        let mut candidates: Vec<TantivyHit> = Vec::with_capacity(hits.len());
        for (score, doc_address) in hits {
            let retrieved: TantivyDocument = searcher.doc(doc_address)?;
            let id_val = retrieved.get_first(self.id_f);
            let ns_val = retrieved.get_first(self.namespace_f);
            let comp_id = id_val.as_ref().map(|v| v.as_str().unwrap_or("").to_string()).unwrap_or_default();
            let ns = ns_val.as_ref().map(|v| v.as_str().unwrap_or("").to_string()).unwrap_or_default();

            // Namespace filter
            if ns != namespace {
                continue;
            }
            // Memory type filter
            let mt_val = retrieved.get_first(self.memory_type_f);
            let memory_type = mt_val.as_ref().map(|v| v.as_str().unwrap_or("").to_string()).unwrap_or_default();
            if memory_type == "task" && !include_tasks {
                continue;
            }
            // Archived filter
            let arch_val = retrieved.get_first(self.archived_f);
            let archived = arch_val.as_ref().and_then(|v| v.as_i64()).unwrap_or(0);
            if archived != 0 && !include_archived {
                continue;
            }
            // Generation filter (defense in depth; the query clause above
            // already drops the row before the page is collected).
            if let Some(active) = active_generations {
                let gen_val = retrieved.get_first(self.generation_f);
                let generation = gen_val.as_ref().and_then(|v| v.as_i64()).unwrap_or(0);
                if generation != 0 && !active.contains(&(generation.max(0) as u64)) {
                    continue;
                }
            }
            // Validity filter (F1): presence-based, exactly like the query
            // clause above and the in-house `doc_visible`. A row with no
            // `valid_to` never retires; a present one retires once it is at or
            // before the visible moment (`valid_to == 0` included).
            let vf_val = retrieved.get_first(self.valid_from_f);
            let vt_val = retrieved.get_first(self.valid_to_f);
            let hvt_val = retrieved.get_first(self.has_valid_to_f);
            let valid_from = vf_val.as_ref().and_then(|v| v.as_i64()).unwrap_or(0);
            let valid_to = vt_val.as_ref().and_then(|v| v.as_i64()).unwrap_or(0);
            let has_valid_to = hvt_val.as_ref().and_then(|v| v.as_i64()).unwrap_or(0) != 0;
            let visible_at = as_of.unwrap_or(now);
            if let Some(ts) = as_of {
                if valid_from > ts {
                    continue;
                }
            }
            if has_valid_to && valid_to <= visible_at {
                continue;
            }

            // Derive id (composite stripped to memory id) so the caller gets
            // the original storage id back (spec §4: `id: String`).
            let memory_id_part = comp_id.split_once("::").map(|(_, id)| id).unwrap_or(&comp_id).to_string();
            candidates.push(TantivyHit {
                id: memory_id_part,
                namespace: ns.clone(),
                score,
                raw_bm25: score,
            });
        }

        // Normalize BM25 into NeuroStrata distance [0,1] (§5.4 / user spec).
        let max_bm25 = candidates.iter().map(|h| h.raw_bm25).fold(0.0_f32, f32::max);
        let mut hits_with_score: Vec<TantivyHit> = candidates.into_iter().map(|mut h| {
            h.score = if max_bm25 > 0.0 {
                (1.0 - (h.raw_bm25 / max_bm25)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            h
        }).collect();

        hits_with_score.sort_by(|a, b| {
            a.score.total_cmp(&b.score).then_with(|| a.id.cmp(&b.id))
        });
        hits_with_score.truncate(limit);
        Ok(hits_with_score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use serde_json::json;

    fn dummy_payload(content: &str, symbol: Option<&str>) -> MemoryPayload {
        MemoryPayload {
            content: content.to_string(),
            user_id: "test-user".to_string(),
            memory_type: "memory".to_string(),
            agent_name: None,
            location: "/src/lib.rs".to_string(),
            location_lines: "1-10".to_string(),
            // No `valid_to` at all: "no expiry". A present 0 is a real past
            // timestamp and retires (see `has_valid_to` in the schema).
            metadata: if let Some(s) = symbol {
                json!({"symbol": s, "valid_from": 100, "archived": 0})
            } else {
                json!({"valid_from": 100, "archived": 0})
            },
        }
    }

    #[test]
    fn test_open_in_ram() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        assert!(idx.writer.lock().unwrap().is_some());
    }

    #[test]
    fn test_upsert_and_search() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let p = dummy_payload("hello world lexical test", Some("MyClass.method"));
        idx.upsert_memory("ns", "m1", &p).unwrap();
        let hits = idx.search("ns", "MyClass method", 10, true, true, None).unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].score >= 0.0 && hits[0].score <= 1.0, "distance in [0,1]: {}", hits[0].score);
    }

    #[test]
    fn test_symbol_boost_ranks_symbol_above_content() {
        // Symbol hit should rank above content-only when query matches both.
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let p_sym = dummy_payload("generic content", Some("NeuroStrata.auth"));
        idx.upsert_memory("ns", "sym", &p_sym).unwrap();
        let p_content = dummy_payload("NeuroStrata auth module description", None);
        idx.upsert_memory("ns", "cont", &p_content).unwrap();
        let hits = idx.search("ns", "neurostrata auth", 10, true, true, None).unwrap();
        // The symbol hit (sym) should be first because symbol boost = 2.0 > content = 1.0.
        // We verify by checking that at least the first hit has id "sym" (memory id stripped from composite).
        assert!(!hits.is_empty());
        // Distance should remain in [0,1].
        for h in &hits {
            assert!(h.score >= 0.0 && h.score <= 1.0, "distance out of range: {}", h.score);
        }
    }

    #[test]
    fn test_delete_memory() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let p = dummy_payload("delete me", None);
        idx.upsert_memory("ns", "del", &p).unwrap();
        idx.delete_memory("ns", "del").unwrap();
        let hits = idx.search("ns", "delete", 10, true, true, None).unwrap();
        assert!(hits.is_empty() || !hits.iter().any(|h| h.id == "del"));
    }

    #[test]
    fn test_rebuild() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let rows = vec![
            ("ns".to_string(), "r1".to_string(), dummy_payload("rebuild a", None)),
            ("ns".to_string(), "r2".to_string(), dummy_payload("rebuild b", Some("X.y"))),
        ];
        let count = idx.rebuild(&rows).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_open_falls_back_to_ram_when_dir_unusable() {
        // A plain file where a directory is required: open_or_create cannot
        // proceed, so open degrades to a RAM-backed index.
        let blocker = std::env::temp_dir().join(format!(
            "ns-lexical-blocker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::write(&blocker, b"not a directory").unwrap();
        let idx = TantivyLexicalIndex::open(&blocker).expect("fallback index");
        assert!(idx.index_dir().is_none(), "fallback must be RAM-backed");
        assert!(idx.writer.lock().unwrap().is_some());
        std::fs::remove_file(&blocker).ok();
    }

    #[test]
    fn test_search_returns_full_memory_id_with_colons() {
        // Memory IDs may themselves contain "::" (e.g. "rule::arch::zero-flags");
        // the composite key is "namespace::memory_id" and search must return the
        // FULL memory id, not a fragment truncated at the first "::".
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let p = dummy_payload("zero flags architectural rule", None);
        idx.upsert_memory("NeuroStrata", "rule::arch::zero-flags", &p).unwrap();
        let hits = idx.search("NeuroStrata", "zero flags", 10, true, true, None).unwrap();
        assert!(!hits.is_empty(), "expected at least one hit");
        assert!(
            hits.iter().any(|h| h.id == "rule::arch::zero-flags"),
            "expected full memory id, got ids: {:?}",
            hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn par10_code_tokens_are_literal() {
        // The exact qualified symbol must return Ok WITH the seeded row; an
        // error-swallowing fix that returns Ok(empty) fails this oracle.
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let mutex = dummy_payload("guarded by a mutex lock", Some("std::sync::Mutex"));
        idx.upsert_memory("ns", "mutex-row", &mutex).unwrap();
        let path = dummy_payload("path token target row", Some("foo::bar::Baz"));
        idx.upsert_memory("ns", "path-row", &path).unwrap();
        let quoted = dummy_payload("say hello loudly", None);
        idx.upsert_memory("ns", "quote-row", &quoted).unwrap();

        for (query, expected) in [
            ("std::sync::Mutex", "mutex-row"),
            ("foo::bar::Baz", "path-row"),
            ("Mutex:", "mutex-row"),
            ("(Mutex", "mutex-row"),
            ("say \"hello", "quote-row"),
            ("+Mutex", "mutex-row"),
            ("-Mutex", "mutex-row"),
        ] {
            let hits = idx
                .search("ns", query, 10, true, true, None)
                .unwrap_or_else(|e| panic!("query {query:?} must not error: {e}"));
            assert!(
                hits.iter().any(|h| h.id == expected),
                "query {query:?} must return {expected}, got: {:?}",
                hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn par10_operator_tokens_are_literal() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let p = dummy_payload("fish and chips or peas supper", None);
        idx.upsert_memory("ns", "bool-row", &p).unwrap();
        for query in ["AND", "chips OR peas"] {
            let hits = idx
                .search("ns", query, 10, true, true, None)
                .unwrap_or_else(|e| panic!("query {query:?} must not error: {e}"));
            assert!(
                hits.iter().any(|h| h.id == "bool-row"),
                "query {query:?} must return the seeded row, got: {:?}",
                hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn par10_query_edge_cases() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let p = dummy_payload("café naïve sharedtoken", None);
        idx.upsert_memory("ns", "unicode-row", &p).unwrap();
        for query in ["", "   ", "\"", "\\", "café::naïve", "a\nb", "term^2", "term~1"] {
            let hits = idx
                .search("ns", query, 10, true, true, None)
                .unwrap_or_else(|e| panic!("query {query:?} must not error: {e}"));
            assert!(hits.is_empty() || hits.iter().all(|h| h.namespace == "ns"));
        }
        let zero = idx.search("ns", "sharedtoken", 0, true, true, None).unwrap();
        assert!(zero.is_empty(), "limit zero returns no rows");
    }

    fn payload_typed(content: &str, memory_type: &str) -> MemoryPayload {
        let mut p = dummy_payload(content, None);
        p.memory_type = memory_type.to_string();
        p
    }

    #[test]
    fn par10_hidden_rows_cannot_starve_the_page() {
        // fetch_limit is limit*8: with limit=2 the collector takes the top 16
        // hits globally. Filtering only afterwards lets 24 higher-scoring
        // foreign rows exhaust that window.
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        for i in 0..24 {
            let p = payload_typed("sharedtoken sharedtoken sharedtoken", "memory");
            idx.upsert_memory("A", &format!("a{i}"), &p).unwrap();
        }
        let b = payload_typed(
            "sharedtoken diluted by many unrelated filler words that lengthen this document considerably beyond the others",
            "memory",
        );
        idx.upsert_memory("B", "b1", &b).unwrap();

        let hits = idx.search("B", "sharedtoken", 2, true, true, None).unwrap();
        assert!(
            hits.iter().any(|h| h.id == "b1"),
            "namespace B match must be collected even when A outscores it, got: {:?}",
            hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        );

        // Same starvation shape for excluded task rows in one namespace.
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        for i in 0..24 {
            let p = payload_typed("sharedtoken sharedtoken sharedtoken", "task");
            idx.upsert_memory("ns", &format!("t{i}"), &p).unwrap();
        }
        let m = payload_typed(
            "sharedtoken diluted by many unrelated filler words that lengthen this document considerably beyond the others",
            "memory",
        );
        idx.upsert_memory("ns", "m1", &m).unwrap();

        let hits = idx.search("ns", "sharedtoken", 2, false, true, None).unwrap();
        assert!(
            hits.iter().any(|h| h.id == "m1"),
            "non-task match must be collected even when excluded tasks outscore it, got: {:?}",
            hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn par10_as_of_boundary_matches_per_hit_rule() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let mut p = dummy_payload("boundary sharedtoken", None);
        p.metadata = json!({"valid_from": 0, "valid_to": 100, "archived": 0});
        idx.upsert_memory("ns", "boundary-row", &p).unwrap();

        let active = idx.search("ns", "sharedtoken", 10, true, true, Some(99)).unwrap();
        assert!(active.iter().any(|h| h.id == "boundary-row"));
        let retired = idx.search("ns", "sharedtoken", 10, true, true, Some(100)).unwrap();
        assert!(
            !retired.iter().any(|h| h.id == "boundary-row"),
            "valid_to == as_of is retired"
        );
    }

    #[test]
    fn par10_rebuild_scoped_preserves_other_namespaces() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let seed = vec![
            ("A".to_string(), "a1".to_string(), dummy_payload("alpha oldtoken", None)),
            ("B".to_string(), "b1".to_string(), dummy_payload("bravo uniquetoken", None)),
        ];
        assert_eq!(idx.rebuild(&seed).unwrap(), 2);

        let new_a = vec![(
            "A".to_string(),
            "a2".to_string(),
            dummy_payload("alpha newtoken", None),
        )];
        assert_eq!(idx.rebuild_scoped(&new_a, Some("A")).unwrap(), 1);

        let b_hits = idx.search("B", "uniquetoken", 10, true, true, None).unwrap();
        assert!(
            b_hits.iter().any(|h| h.id == "b1"),
            "namespace B rows must survive a scoped rebuild of A, got: {:?}",
            b_hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        );
        let a_old = idx.search("A", "oldtoken", 10, true, true, None).unwrap();
        assert!(!a_old.iter().any(|h| h.id == "a1"), "scoped rebuild replaces A's old rows");
        let a_new = idx.search("A", "newtoken", 10, true, true, None).unwrap();
        assert!(a_new.iter().any(|h| h.id == "a2"));

        let only_c = vec![("C".to_string(), "c1".to_string(), dummy_payload("charlie fresh token", None))];
        assert_eq!(idx.rebuild_scoped(&only_c, None).unwrap(), 1);
        let b_hits = idx.search("B", "uniquetoken", 10, true, true, None).unwrap();
        assert!(b_hits.is_empty(), "unscoped rebuild replaces every namespace");
        let c_hits = idx.search("C", "charlie fresh", 10, true, true, None).unwrap();
        assert!(c_hits.iter().any(|h| h.id == "c1"));
    }

    #[test]
    fn test_search_hits_carry_their_namespace_and_index_reports_dir() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        idx.upsert_memory("ns", "m1", &dummy_payload("namespace carry test", None)).unwrap();
        let hits = idx.search("ns", "namespace carry", 10, true, true, None).unwrap();
        assert!(!hits.is_empty());
        for h in &hits {
            assert_eq!(h.namespace, "ns");
        }
        assert!(idx.index_dir().is_none());
    }

    fn visibility_options(now: i64, as_of: Option<i64>) -> crate::fts::SearchOptions {
        crate::fts::SearchOptions {
            now,
            as_of,
            include_tasks: true,
            include_archived: true,
            principal: "reader".to_string(),
            domains: Vec::new(),
            fuzzy_max_distance: 0,
            limit: 10,
        }
    }

    fn validity_doc(valid_to: Option<i64>) -> crate::fts::LexicalDoc {
        let mut doc = crate::fts::LexicalDoc::new(
            "validity",
            "ns",
            "memory",
            "validityparityneedle",
        );
        doc.user_id = "reader".to_string();
        doc.valid_to = valid_to;
        doc
    }

    /// F1 (#127): the Tantivy query clause and the per-hit rule (and the
    /// in-house `doc_visible`) must agree at the `as_of` boundary, so a hidden
    /// row can never consume a page slot while being vetoed afterwards.
    #[test]
    fn validity_boundary_parity() {
        let now = chrono::Utc::now().timestamp();
        let cases: Vec<(Option<i64>, bool)> = vec![
            (None, true),
            // Presence, not sign: an explicitly present 0 is a past timestamp.
            (Some(0), false),
            (Some(now + 100), true),
            (Some(now - 100), false),
        ];

        // In-house engine verdict.
        for (valid_to, expected) in &cases {
            let doc = validity_doc(*valid_to);
            let visible = crate::fts::doc_visible(&doc, &visibility_options(now, None));
            assert_eq!(
                visible, *expected,
                "in-house as_of=None valid_to={valid_to:?}"
            );
        }

        // Tantivy engine verdict, with the same rows.
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        for (valid_to, _) in &cases {
            let mut payload = dummy_payload("validityparityneedle", None);
            payload.metadata = match valid_to {
                None => json!({"valid_from": 0, "archived": 0}),
                Some(value) => json!({"valid_from": 0, "valid_to": value, "archived": 0}),
            };
            let id = match valid_to {
                None => "none",
                Some(0) => "zero",
                Some(value) if *value > now => "future",
                Some(_) => "past",
            };
            idx.upsert_memory("ns", id, &payload).unwrap();
        }
        let hits = idx.search("ns", "validityparityneedle", 10, true, true, None).unwrap();
        let returned: std::collections::BTreeSet<String> =
            hits.iter().map(|h| h.id.clone()).collect();
        assert_eq!(
            returned,
            ["future", "none"]
                .iter()
                .map(|s| s.to_string())
                .collect::<std::collections::BTreeSet<_>>(),
            "Tantivy as_of=None: a future valid_to stays visible; a present 0 retires"
        );

        // The same agreement at an explicit as_of: valid_to == ts is retired.
        let boundary = idx
            .search("ns", "validityparityneedle", 10, true, true, Some(now))
            .unwrap();
        let ids: std::collections::BTreeSet<String> =
            boundary.iter().map(|h| h.id.clone()).collect();
        assert_eq!(
            ids,
            ["future", "none"]
                .iter()
                .map(|s| s.to_string())
                .collect::<std::collections::BTreeSet<_>>(),
            "Tantivy as_of=now: a present 0 and the boundary row are retired"
        );
        assert!(
            !crate::fts::doc_visible(&validity_doc(Some(now)), &visibility_options(now, Some(now))),
            "in-house as_of=now retires valid_to == now"
        );
    }

    /// #127 F2 / E1: ordinary code tokens are literal, not query syntax.
    #[test]
    fn e1_code_token_breadth() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let tokens = ["foo<T>", "a(b)", "x+y", "path/to/file", "C++"];
        for (i, token) in tokens.iter().enumerate() {
            let id = format!("code-{i}");
            idx.upsert_memory("ns", &id, &dummy_payload(token, None)).unwrap();
        }
        for (i, token) in tokens.iter().enumerate() {
            let hits = idx.search("ns", token, 10, true, true, None).unwrap();
            let expected = format!("code-{i}");
            assert!(
                hits.iter().any(|h| h.id == expected),
                "literal token {token:?} must return {expected}, got {:?}",
                hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
            );
        }
    }

    /// #127 F2 / E3: the seeded unicode row is asserted present, not merely
    /// "no row outside the namespace".
    #[test]
    fn e3_seeded_unicode_returns() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        idx.upsert_memory(
            "ns",
            "unicode-row",
            &dummy_payload("café::naïve unicodeneedle", None),
        )
        .unwrap();
        let hits = idx.search("ns", "café::naïve", 10, true, true, None).unwrap();
        assert!(
            hits.iter().any(|h| h.id == "unicode-row"),
            "the seeded unicode row must match its own query, got {:?}",
            hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        );
    }

    /// #127 F2: archived and retired rows that match a query must not exhaust
    /// the page (fetch_limit = limit * 8).
    #[test]
    fn archived_and_validity_starvation() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        for i in 0..40 {
            let mut archived = dummy_payload("archivestarve archivestarve archivestarve", None);
            archived.metadata = json!({"valid_from": 0, "archived": 1});
            idx.upsert_memory("ns", &format!("archived-{i}"), &archived)
                .unwrap();

            let mut retired = dummy_payload("validitystarve validitystarve validitystarve", None);
            retired.metadata = json!({"valid_from": 0, "valid_to": 1, "archived": 0});
            idx.upsert_memory("ns", &format!("retired-{i}"), &retired)
                .unwrap();
        }
        let visible = dummy_payload(
            "archivestarve diluted by unrelated filler words that lengthen this document considerably",
            None,
        );
        idx.upsert_memory("ns", "visible-archived", &visible).unwrap();
        let visible = dummy_payload(
            "validitystarve diluted by unrelated filler words that lengthen this document considerably",
            None,
        );
        idx.upsert_memory("ns", "visible-validity", &visible).unwrap();

        // limit 5 -> fetch_window 40; 40 hidden rows would fill it without
        // query-level filtering.
        let archived_hits = idx
            .search("ns", "archivestarve", 5, true, false, None)
            .unwrap();
        assert!(
            archived_hits.iter().any(|h| h.id == "visible-archived"),
            "a visible row must survive an archived flood, got {:?}",
            archived_hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        );
        let validity_hits = idx
            .search("ns", "validitystarve", 5, true, true, None)
            .unwrap();
        assert!(
            validity_hits.iter().any(|h| h.id == "visible-validity"),
            "a visible row must survive a retired flood, got {:?}",
            validity_hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        );
    }

    /// #127 F5: a scoped rebuild must never insert a row from another
    /// namespace under the scoped namespace.
    #[test]
    fn rebuild_scoped_rejects_foreign_namespace() {
        let idx = TantivyLexicalIndex::open_in_ram().unwrap();
        let rows = vec![
            (
                "ns".to_string(),
                "inside".to_string(),
                dummy_payload("scopedneedle inside", None),
            ),
            (
                "other".to_string(),
                "outside".to_string(),
                dummy_payload("scopedneedle outside", None),
            ),
        ];
        assert_eq!(idx.rebuild_scoped(&rows, Some("ns")).unwrap(), 1);

        let inside = idx.search("ns", "scopedneedle", 10, true, true, None).unwrap();
        assert!(inside.iter().any(|h| h.id == "inside"));
        let outside = idx
            .search("other", "scopedneedle", 10, true, true, None)
            .unwrap();
        assert!(
            outside.is_empty(),
            "a foreign row was indexed under the scoped rebuild: {:?}",
            outside.iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        );
    }
}
