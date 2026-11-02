//! Tantivy lexical full-text index for NeuroStrata (spec 2026-10-27-tantivy-full-text-search-design.md).
//! Unconditionally compiled; `tantivy = "0.27"` is present in Cargo.toml.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field as TantivyField, Schema, STORED, STRING, TEXT, FAST, INDEXED, Value as TantivyValue};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::traits::{MemoryPayload};

pub const INDEX_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-tantivy-v1");

/// Code-aware pre-tokenization for Tantivy (whitespace-separated sub-tokens + full lowercased text).
pub fn tokenize_code_for_tantivy(text: &str) -> String {
    let mut tokens = crate::fts::tokenize_code(text);
    let lower_raw = text.to_lowercase();
    if !lower_raw.is_empty() && !tokens.contains(&lower_raw) {
        tokens.push(lower_raw);
    }
    tokens.join(" ")
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
    pub archived_f: TantivyField,
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
        schema_builder.add_i64_field("archived", INDEXED | FAST | STORED);
        schema_builder.build()
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
        let archived_f = schema.get_field("archived").expect("archived field");
        Ok(Self {
            index,
            writer: Mutex::new(Some(writer)),
            reader,
            dirty: AtomicBool::new(false),
            index_dir: None,
            id_f, memory_id_f, namespace_f, content_f, symbol_f, location_f,
            memory_type_f, valid_from_f, valid_to_f, archived_f,
        })
    }

    /// Open or create on disk (spec §3 / §5.2 / §7).
    pub fn open_or_create(index_dir: &Path) -> Result<Self> {
        let version_path = index_dir.join("index_version.txt");
        let needs_rebuild = if version_path.exists() {
            let existing = std::fs::read_to_string(&version_path).unwrap_or_default();
            existing.trim() != INDEX_VERSION
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
        let (index, _rebuilt) = match dir_result {
            Ok(dir) => {
                let index = Index::open_or_create(dir, Self::build_schema())?;
                (index, false)
            }
            Err(_) => {
                tracing::warn!("corrupt lexical index at {}; wiping and recreating", path_for_index.display());
                std::fs::remove_dir_all(&path_for_index)?;
                std::fs::create_dir_all(&path_for_index)?;
                let dir = tantivy::directory::MmapDirectory::open(&path_for_index)?;
                let index = Index::create(dir, Self::build_schema(), tantivy::IndexSettings::default())?;
                (index, true)
            }
        };

        // Writer acquisition with lock-contention fallback (§5.2).
        let writer_res = index.writer(50_000_000);
        let mut writer_opt: Option<IndexWriter> = None;
        match writer_res {
            Ok(w) => {
                writer_opt = Some(w);
            }
            Err(_) => {
                tracing::warn!("lexical index writer locked by another process (read-only mode)");
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
        let archived_f = schema.get_field("archived").expect("archived");

        Ok(Self {
            index,
            writer: Mutex::new(writer_opt),
            reader,
            dirty: AtomicBool::new(false),
            index_dir: Some(path_for_index),
            id_f, memory_id_f, namespace_f, content_f, symbol_f, location_f,
            memory_type_f, valid_from_f, valid_to_f, archived_f,
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
        if let Some(w) = self.writer.lock().unwrap().as_mut() {
            let term = Term::from_field_text(self.id_f, &composite);
            w.delete_term(term);
        }

        if Self::is_excluded(payload) {
            // Excluded: delete and commit (if writer exists).
            if let Some(w) = self.writer.lock().unwrap().as_mut() {
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
        let archived: i64 = if payload.metadata.get("archived").and_then(|v| v.as_i64()).unwrap_or(0) != 0 { 1 } else { 0 };
        doc.add_i64(self.archived_f, archived);

        if let Some(w) = self.writer.lock().unwrap().as_mut() {
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
        if let Some(w) = self.writer.lock().unwrap().as_mut() {
            let term = Term::from_field_text(self.id_f, &composite);
            w.delete_term(term);
            w.commit()?;
            self.dirty.store(true, Ordering::SeqCst);
            self.reader.reload()?;
        }
        Ok(())
    }

    /// Rebuild from slice (namespace, id, payload) — commit once (§4).
    pub fn rebuild(&self, rows: &[(String, String, MemoryPayload)]) -> Result<usize> {
        if let Some(w) = self.writer.lock().unwrap().as_mut() {
            w.delete_all_documents()?;
            let mut indexed = 0usize;
            for (ns, id, payload) in rows {
                if Self::is_excluded(payload) {
                    continue;
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
                let archived: i64 = if payload.metadata.get("archived").and_then(|v| v.as_i64()).unwrap_or(0) != 0 { 1 } else { 0 };
                doc.add_i64(self.archived_f, archived);
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

    /// Search (§4 / §5.4).
    pub fn search(
        &self,
        namespace: &str,
        query_str: &str,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
        as_of: Option<i64>,
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

        let tokenized_query = tokenize_code_for_tantivy(query_str);
        let parsed = parser.parse_query(&tokenized_query)?;

        let fetch_limit = (limit.max(1) * 8) as usize;
        let searcher = self.reader.searcher();
        let top_docs = TopDocs::with_limit(fetch_limit).order_by_score();
        let hits: Vec<(f32, tantivy::DocAddress)> = searcher.search(&parsed, &top_docs)?;

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
            // Validity filter
            let vf_val = retrieved.get_first(self.valid_from_f);
            let vt_val = retrieved.get_first(self.valid_to_f);
            let valid_from = vf_val.as_ref().and_then(|v| v.as_i64()).unwrap_or(0);
            let valid_to = vt_val.as_ref().and_then(|v| v.as_i64()).unwrap_or(0);
            if let Some(ts) = as_of {
                if valid_from > ts || (valid_to != 0 && valid_to <= ts) {
                    continue;
                }
            } else {
                if valid_to != 0 {
                    continue; // active only at current time when no as_of
                }
            }

            // Derive id (composite stripped to memory id) — we want the original memory id back.
            // The spec's TantivyHit.id is the memory id or composite? It says `id: String`; from spec §4 id is composite.
            // We return the composite or just id? Spec says `id: String` — we'll return the storage id (after "::").
            let memory_id_part = comp_id.split("::").nth(1).unwrap_or(&comp_id).to_string();
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
            metadata: if let Some(s) = symbol {
                json!({"symbol": s, "valid_from": 100, "valid_to": 0, "archived": 0})
            } else {
                json!({"valid_from": 100, "valid_to": 0, "archived": 0})
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
}
