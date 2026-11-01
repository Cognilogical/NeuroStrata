use crate::parser::get_language;
use crate::parser::schema::ParserSchema;
use crate::traits::{
    is_reserved_ingest_destination, Embedder, MemoryPayload, VectorStore,
    INTERNAL_INGEST_STAGE_PREFIX,
};
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

// The id/hash scheme lives in `generation.rs`, a module deliberately free of
// `crate::` dependencies so an integration test can pin it without a library
// target. These re-exports keep `ingest.rs` and the store naming the same
// functions through the path they already use.
pub use crate::parser::generation::{
    generation_key, generation_scoped_id, logical_path_of, sha256_hex, split_scoped_id,
    symbol_id, NAMESPACE_SEPARATOR,
};

/// Schemas declare extensions bare ("rs"), but one passed via --schema-path may
/// use the dotted form. Both sides go through here so either convention matches.
fn normalize_ext(ext: &str) -> String {
    ext.trim_start_matches('.').to_ascii_lowercase()
}

fn build_ext_map(schema: &ParserSchema) -> HashMap<String, String> {
    let mut ext_to_lang = HashMap::new();
    for (lang_name, lang_schema) in &schema.languages {
        for ext in &lang_schema.extensions {
            ext_to_lang.insert(normalize_ext(ext), lang_name.clone());
        }
    }
    ext_to_lang
}

/// Upper bound on the source text stored and embedded for one symbol. The
/// embedder truncates its input anyway (see MAX_EMBED_TOKENS in src/embed.rs);
/// cutting here keeps the stored content and the vector describing the same
/// text instead of letting them drift apart on large definitions.
const MAX_SYMBOL_CHARS: usize = 4000;

fn truncate_for_embedding(body: &str) -> String {
    if body.chars().count() <= MAX_SYMBOL_CHARS {
        return body.to_string();
    }
    let cut: String = body.chars().take(MAX_SYMBOL_CHARS).collect();
    format!(
        "{}\n... (truncated at {} characters)",
        cut, MAX_SYMBOL_CHARS
    )
}

/// Node ids are paths, and a memory written by an agent names a file the way a
/// human does: `src/parser/ingest.rs`. The walker yields a platform path with
/// backslashes and a leading `./` on Windows, and an edge only forms when the
/// two strings match exactly, so every id goes through here first.
pub fn normalize_node_path(path: &str) -> String {
    let forward = path.replace('\\', "/");
    let trimmed = forward.strip_prefix("./").unwrap_or(&forward);
    trimmed.trim_end_matches('/').to_string()
}

/// The stable key identifying the source (worktree) being ingested.
///
/// This is what keeps two concurrent worktrees of one repository apart in the
/// generation pointer table: each source's active generation is recorded under
/// its own key, so publishing one worktree cannot overwrite the other's active
/// pointer. It doubles as `SourceIdentity.worktree_id` until Task 2's
/// `source::resolve` fills repo/revision from a real checkout.
pub fn source_key_for(dir_path: &Path) -> String {
    std::fs::canonicalize(dir_path)
        .unwrap_or_else(|_| dir_path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

// The separator between a namespace and the path it qualifies (`NAMESPACE_SEPARATOR`)
// is defined once in `crate::parser::generation` and re-exported at the top of
// this module, so `ingest.rs` and the store keep naming the same constant
// through the path they already use. `qualified_id` below builds the two-segment
// legacy form from it; staged ingestion builds the three-segment form with
// `generation_scoped_id`.

/// A node id, qualified by the namespace that owns it.
///
/// Node ids are paths, and the Memory table's original key was PRIMARY KEY
/// (id) -- one column, because lbug has no composite key (see
/// examples/composite_pk_repro.rs for the parser refusing one). So an id was
/// unique across the WHOLE database, while paths are only unique within a
/// project. Every repository has a `src` and a `README.md`, and the database
/// is shared by all of them, which meant the second project ingested silently
/// took the first project's nodes: upsert MERGEs on the id, finds the existing
/// row, and updates it. Fresh tables now key on a derived pk
/// (namespace + U+0001 + id; Task 7 re-plan), but tables written by older
/// binaries still key on (id) alone — and qualified ids remain the
/// ingest-minted shape either way, so the separation below holds on both.
///
/// Qualifying the id with its namespace is what makes "projects are separated by
/// namespace" true in the schema rather than only in the documentation. It is
/// also the only one of the three candidate fixes the engine allows.
///
/// The bare path is still what a memory's `location` carries and what a rule
/// names, so declarations keep reading the way a human writes them --
/// `resolve_declared_target` bridges the two.
pub fn qualified_id(namespace: &str, node_path: &str) -> String {
    format!("{}{}{}", namespace, NAMESPACE_SEPARATOR, node_path)
}

/// Whether an ingested id names the namespace holding it.
///
/// False for anything written before ids carried a namespace. Such a node still
/// resolves -- KnownIds matches a bare path either way -- but its id is unique
/// only within its project rather than within the database, so another project
/// ingesting the same path can still take it. `doctor` reports these so the
/// state is visible instead of silent; a re-ingest is what migrates them.
pub fn is_qualified(namespace: &str, id: &str) -> bool {
    id.starts_with(&qualified_id(namespace, ""))
}

/// The id a node gets, relative to the directory being ingested.
///
/// Ids used to inherit whatever the caller passed: the CLI walked a relative
/// path and produced `src/store/ladybug.rs`, while the GUI passes an absolute
/// project path and produced `C:/dev/projects/neurostrata/src/store/ladybug.rs`.
/// GOVERNS edges match ids by exact string, so a rule an agent wrote against
/// `src/store/ladybug.rs` could never link to the same file ingested from the
/// GUI -- and an absolute id does not survive the repository being checked out
/// anywhere else (bead neurostrata-fld).
pub fn node_id_for(root: &Path, path: &Path) -> String {
    // A relative walk already produces the documented form. `ingest ./src <ns>`
    // is the example in CLI-readme.md, and it must keep yielding src/lib.rs --
    // the same shape a rule names when it says "src/lib.rs" -- rather than the
    // lib.rs that stripping the ingest root would leave.
    let relative = if path.is_relative() {
        path.to_path_buf()
    } else {
        // Absolute: relative to the root being ingested. This used to try the
        // process's working directory first, which made an id depend on where
        // the daemon happened to be launched -- ingesting /workspace/Project
        // from /workspace produced Project/src/main.rs, which no rule names.
        // The CLI's subdirectory case is handled where the CLI reads its path.
        path.strip_prefix(root).unwrap_or(path).to_path_buf()
    };

    let normalized = normalize_node_path(&relative.to_string_lossy());
    if normalized.is_empty() {
        // The root itself. Name it, rather than leaving an empty id.
        normalize_node_path(
            &root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| root.to_string_lossy().to_string()),
        )
    } else {
        normalized
    }
}

/// One extracted symbol, owned so that no tree-sitter type is alive when the
/// embedding and upsert futures are awaited.
struct SymbolRow {
    name: String,
    kind: String,
    start_line: usize,
    end_line: usize,
    /// The definition text cut for embedding and storage. Identity does NOT use
    /// this: `span_sha256` hashes the full, untruncated text, so re-ingesting
    /// unchanged code is the same symbol however its start line moves.
    body: String,
    /// SHA-256 of the full (untruncated) definition text. This is the symbol's
    /// stable identity tail (`symbol_id`), stable across line moves.
    span_sha256: String,
}

// `symbol_id` and `NAMESPACE_SEPARATOR` are re-exported from
// `crate::parser::generation` at the top of this module.

/// Told what the walk has done so far, so a caller does not have to wait for
/// the whole thing to find out. Implementations are called from the walk, so
/// they must be cheap and must not block.
pub trait IngestObserver: Send + Sync {
    fn file_ingested(&self, path: &str, symbols: usize);
    fn relinked(&self, edges: usize);
    /// A document the secret scanner refused at ingest: skipped whole and
    /// reported here with the scanner's reason. The default ignores it, so
    /// observers that only count progress are unchanged.
    fn file_skipped(&self, _path: &str, _reason: &str) {}
}

/// An owned item streamed from the blocking producer to the async consumer.
/// No borrowed tree-sitter or filesystem types cross the channel boundary.
enum IngestItem {
    /// A structural (directory/file/markdown) node to upsert with zero vector.
    NodeUpsert {
        node_id: String,
        node_path: String,
        contained_by: Vec<String>,
        mem_type: String,
        abs_path: String,
        /// SHA-256 of the file bytes, captured for parseable code files only.
        /// Directories and non-code files carry none.
        file_sha256: Option<String>,
    },
    /// Parsed symbols from a file, ready for embed + store.
    ParsedSymbols {
        node_path: String,
        lang_name: String,
        symbols: Vec<SymbolRow>,
        path_display: String,
        /// SHA-256 of the file bytes, shared by every symbol in the file.
        file_sha256: String,
    },
    /// A document the secret scanner refused: skipped whole, reported
    /// through the observer. Carries the file's display path and the
    /// scanner's reason — never the secret itself.
    SkippedFile {
        path_display: String,
        reason: String,
    },
}

/// Secret scan over a document's text, run in the producer as the file is
/// read — before any part of the document reaches the store. The verdict
/// text is the reason the skip reports.
fn scan_for_ingest(text: &str) -> Option<String> {
    crate::secrets::scan_entry_point(text, &serde_json::json!({}), "ingest")
        .map(|r| r.to_string())
}

/// Blocking producer: walks the directory tree, reads files, parses ASTs,
/// and streams owned results through `tx`. Runs on the blocking thread pool
/// so filesystem I/O never stalls the async runtime.
fn run_producer(
    dir_path: std::path::PathBuf,
    ext_to_lang: HashMap<String, String>,
    schema: crate::parser::schema::ParserSchema,
    tx: mpsc::Sender<IngestItem>,
    namespace: String,
    generation: u64,
) -> anyhow::Result<()> {
    let skipped_dirs = [
        "node_modules",
        "target",
        "vendor",
        ".venv",
        "venv",
        "env",
        ".env",
        "dist",
        "build",
        "out",
        ".dolt",
        ".git",
        ".next",
        ".nuxt",
        "__pycache__",
        ".fastembed_cache",
        ".idea",
        ".vscode",
        "coverage",
    ];

    let walker = WalkBuilder::new(&dir_path).build();

    let mut unreadable = 0usize;

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(e) => {
                unreadable += 1;
                if unreadable <= 5 {
                    eprintln!("Could not read part of {:?}: {}", dir_path, e);
                }
                continue;
            }
        };

        let path = entry.path();
        let path_str = path.to_string_lossy();

        let mut should_skip = false;
        for skip_dir in &skipped_dirs {
            let skip_pattern = format!("/{}/", skip_dir);
            let skip_start = format!("{}/", skip_dir);
            let skip_exact = format!("./{}", skip_dir);
            if path_str.contains(&skip_pattern)
                || path_str.starts_with(&skip_start)
                || path_str == skip_exact
            {
                should_skip = true;
                break;
            }
        }
        if should_skip {
            continue;
        }

        let abs_path = if path.is_absolute() {
            path.to_string_lossy().to_string()
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(path)
                .to_string_lossy()
                .to_string()
        };

        let node_path = node_id_for(&dir_path, path);

        let parent_id = if path == dir_path {
            None
        } else if let Some(p) = path.parent() {
            // Only adopt a parent that actually lives inside the ingest root;
            // otherwise a root-level ingest leaks an absolute-path parent id
            // (e.g. "NeuroStrata::/home/kenton/Documents") that matches no node.
            if p == dir_path || p.strip_prefix(&dir_path).is_ok() {
                let p_str = node_id_for(&dir_path, p);
                if p_str != "." && p_str != "" && p_str != node_path {
                    Some(p_str)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let mut contained_by = Vec::new();
        if let Some(pid) = parent_id {
            contained_by.push(generation_scoped_id(&namespace, generation, &pid));
        }

        let is_dir = entry.file_type().map_or(false, |ft| ft.is_dir());
        let is_file = entry.file_type().map_or(false, |ft| ft.is_file());

        let mem_type = if is_dir {
            "directory"
        } else if is_file {
            if path_str.ends_with(".md") {
                "markdown"
            } else {
                "file"
            }
        } else {
            continue;
        };

        let node_id = generation_scoped_id(&namespace, generation, &node_path);

        // Every file's bytes are read once and hashed so `file_sha256` is
        // captured on file and markdown nodes too, not only on parseable code
        // files. The same bytes are parsed only when the extension maps to a
        // grammar; directories carry no hash.
        let (file_sha256, content, lang_name) = if is_file {
            let bytes = std::fs::read(path).ok();
            let hash = bytes.as_deref().map(sha256_hex);
            let lang_name = path
                .extension()
                .map(|ext_os| normalize_ext(&ext_os.to_string_lossy()))
                .and_then(|ext| ext_to_lang.get(&ext).cloned())
                .filter(|ln| get_language(ln).is_some());
            let content = match (&bytes, &lang_name) {
                (Some(b), Some(_)) => Some(String::from_utf8_lossy(b).to_string()),
                _ => None,
            };
            (hash, content, lang_name)
        } else {
            (None, None, None)
        };

        // Before its node goes out, a parseable file's text meets the
        // secret scanner first, and a document that trips it is skipped
        // whole — no node, no symbols — with the skip reported through
        // the consumer. The rest of the walk proceeds. The scan runs over
        // the bytes already read above — the same bytes the hash was
        // taken over and the parser will consume — so what is scanned is
        // exactly what is parsed and stored. (`content` is `Some` exactly
        // for parseable files; files nothing is extracted from carry no
        // extracted text to scan.)
        if let Some(text) = &content {
            if let Some(reason) = scan_for_ingest(text) {
                if let Err(e) = tx.blocking_send(IngestItem::SkippedFile {
                    path_display: path.display().to_string(),
                    reason,
                }) {
                    return Err(anyhow::anyhow!(
                        "ingest consumer dropped: {}",
                        e
                    ));
                }
                continue;
            }
        }

        // `blocking_send` applies backpressure when the bounded queue is full.
        // A closed receiver is an error: otherwise a partial walk looked like
        // success when the consumer had failed.
        tx.blocking_send(IngestItem::NodeUpsert {
            node_id,
            node_path: node_path.clone(),
            contained_by,
            mem_type: mem_type.to_string(),
            abs_path,
            file_sha256: file_sha256.clone(),
        })
        .map_err(|_| {
            anyhow::anyhow!("ingest consumer stopped before node {node_path} was queued")
        })?;

        // Parse the content already read above and stream the symbols.
        if let (Some(content), Some(lang_name)) = (content, lang_name) {
            let Some(ts_lang) = get_language(&lang_name) else {
                continue;
            };

            let mut parser = Parser::new();
            parser.set_language(&ts_lang)?;

            let tree = match parser.parse(&content, None) {
                Some(t) => t,
                None => continue,
            };

            let lang_schema = &schema.languages[&lang_name];
            let mut pending: Vec<SymbolRow> = Vec::new();

            for (query_name, query_str) in &lang_schema.queries {
                let query = match Query::new(&ts_lang, query_str) {
                    Ok(q) => q,
                    Err(e) => {
                        eprintln!("Invalid query for {}: {}", lang_name, e);
                        continue;
                    }
                };

                let mut cursor = QueryCursor::new();
                let mut iter = cursor.matches(&query, tree.root_node(), content.as_bytes());

                while let Some(m) = iter.next() {
                    let mut name: Option<&str> = None;
                    let mut definition: Option<tree_sitter::Node> = None;

                    for capture in m.captures {
                        let capture_name = query.capture_names()[capture.index as usize];
                        if capture_name == "name" {
                            name = capture.node.utf8_text(content.as_bytes()).ok();
                        } else if definition.map_or(true, |d: tree_sitter::Node| {
                            capture.node.byte_range().len() > d.byte_range().len()
                        }) {
                            definition = Some(capture.node);
                        }
                    }

                    let (name, definition) = match (name, definition) {
                        (Some(n), Some(d)) => (n, d),
                        _ => continue,
                    };

                    let full = definition.utf8_text(content.as_bytes()).unwrap_or("");
                    pending.push(SymbolRow {
                        name: name.to_string(),
                        kind: query_name.clone(),
                        start_line: definition.start_position().row + 1,
                        end_line: definition.end_position().row + 1,
                        body: truncate_for_embedding(full),
                        span_sha256: sha256_hex(full.as_bytes()),
                    });
                }
            }

            if !pending.is_empty() {
                let path_display = path.display().to_string();
                // `file_sha256` is `Some` exactly when `content` is, so this is
                // the hash of the bytes just parsed.
                let file_sha = file_sha256
                    .clone()
                    .expect("a parseable file has a captured hash");
                if let Err(e) = tx.blocking_send(IngestItem::ParsedSymbols {
                    node_path,
                    lang_name: lang_name.clone(),
                    symbols: pending,
                    path_display,
                    file_sha256: file_sha,
                }) {
                    return Err(anyhow::anyhow!("ingest consumer dropped: {}", e));
                }
            }
        }
    }

    if unreadable > 0 {
        eprintln!(
            "WARNING: {} path(s) under {:?} could not be read, so they are missing from this ingest of namespace {}.",
            unreadable, dir_path, namespace
        );
    }

    Ok(())
}

/// Stamps the per-node `relative_path` onto a copy of the shared source
/// identity. The identity is otherwise identical across a tree, but the path
/// it governs differs per node, so each row records its own.
fn with_relative_path(source_identity: &serde_json::Value, relative_path: &str) -> serde_json::Value {
    let mut si = source_identity.clone();
    if let Some(obj) = si.as_object_mut() {
        obj.insert(
            "relative_path".to_string(),
            serde_json::json!(relative_path),
        );
    }
    si
}

/// Async consumer: receives owned items from the producer, embeds symbols,
/// and upserts everything to the vector store.
async fn run_consumer(
    mut rx: mpsc::Receiver<IngestItem>,
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    namespace: &str,
    observer: Option<Arc<dyn IngestObserver>>,
    zero_vector: Vec<f32>,
    generation: u64,
    source_identity: serde_json::Value,
) -> anyhow::Result<()> {
    while let Some(item) = rx.recv().await {
        match item {
            IngestItem::NodeUpsert {
                node_id,
                node_path,
                contained_by,
                mem_type,
                abs_path,
                file_sha256,
            } => {
                let mut metadata = serde_json::Map::new();
                metadata.insert("absolute_path".to_string(), serde_json::json!(abs_path));
                metadata.insert("contained_by".to_string(), serde_json::json!(contained_by));
                metadata.insert(
                    "source_identity".to_string(),
                    with_relative_path(&source_identity, &node_path),
                );
                metadata.insert("index_generation".to_string(), serde_json::json!(generation));
                if let Some(hash) = file_sha256 {
                    metadata.insert("file_sha256".to_string(), serde_json::json!(hash));
                }

                let payload = MemoryPayload {
                    content: format!("Path: {}", node_path),
                    location: node_path,
                    location_lines: String::new(),
                    memory_type: mem_type,
                    metadata: serde_json::Value::Object(metadata),
                    user_id: "auto-ingestor".to_string(),
                    agent_name: Some("neurostrata-mcp-ingestor".to_string()),
                };

                // A failed upsert aborts the staging generation rather than
                // reporting success over a graph with a hole in it: the error
                // propagates to `ingest_directory`, which calls
                // `abort_generation` and leaves the previous generation active.
                vector_store
                    .upsert(namespace, &node_id, zero_vector.clone(), payload)
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to upsert graph node {}: {}", node_id, e))?;
            }
            IngestItem::ParsedSymbols {
                node_path,
                lang_name,
                symbols,
                path_display,
                file_sha256,
            } => {
                let mut symbols_stored = 0usize;
                for symbol in symbols {
                    let summary = format!(
                        "{} {} in {}\nlines {}-{}\n\n{}",
                        symbol.kind,
                        symbol.name,
                        node_path,
                        symbol.start_line,
                        symbol.end_line,
                        symbol.body
                    );
                    let id = generation_scoped_id(
                        namespace,
                        generation,
                        &symbol_id(
                            &node_path,
                            &symbol.kind,
                            &symbol.name,
                            &symbol.span_sha256,
                        ),
                    );
                    let lines = format!("{}-{}", symbol.start_line, symbol.end_line);

                    let mut metadata = serde_json::Map::new();
                    metadata.insert("domain".to_string(), serde_json::json!("code_ast"));
                    metadata.insert("symbol".to_string(), serde_json::json!(symbol.name));
                    metadata.insert("symbol_kind".to_string(), serde_json::json!(symbol.kind));
                    metadata.insert("language".to_string(), serde_json::json!(lang_name));
                    metadata.insert(
                        "contained_by".to_string(),
                        serde_json::json!([generation_scoped_id(namespace, generation, &node_path)]),
                    );
                    metadata.insert(
                        "refs".to_string(),
                        serde_json::json!([
                            { "file": node_path.clone(), "lines": lines.clone() }
                        ]),
                    );
                    metadata.insert("file_sha256".to_string(), serde_json::json!(file_sha256));
                    metadata.insert(
                        "span_sha256".to_string(),
                        serde_json::json!(symbol.span_sha256),
                    );
                    metadata.insert(
                        "source_identity".to_string(),
                        with_relative_path(&source_identity, &node_path),
                    );
                    metadata.insert("index_generation".to_string(), serde_json::json!(generation));

                    let payload = MemoryPayload {
                        content: summary.clone(),
                        location: node_path.clone(),
                        location_lines: lines,
                        memory_type: "code_ast".to_string(),
                        metadata: serde_json::Value::Object(metadata),
                        user_id: "auto-ingestor".to_string(),
                        agent_name: Some("neurostrata-mcp-ingestor".to_string()),
                    };

                    // An embed or store failure aborts the staging generation:
                    // swallowing it here would hand back a graph whose symbols
                    // are silently missing, which is exactly the half-built
                    // tree staged publication exists to prevent.
                    let embedding = embedder.embed(&summary).await.map_err(|e| {
                        anyhow::anyhow!(
                            "failed to embed symbol {} from {}: {}. The staging generation was discarded; the previous generation remains active.",
                            symbol.name,
                            path_display,
                            e
                        )
                    })?;
                    vector_store
                        .upsert(namespace, &id, embedding, payload)
                        .await
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "failed to store symbol {} from {}: {}. The staging generation was discarded; the previous generation remains active.",
                                symbol.name,
                                path_display,
                                e
                            )
                        })?;
                    symbols_stored += 1;
                }

                if symbols_stored > 0 {
                    println!("Ingested {} symbols from {}", symbols_stored, path_display);
                    if let Some(observer) = &observer {
                        observer.file_ingested(&path_display, symbols_stored);
                    }
                }
            }
            IngestItem::SkippedFile {
                path_display,
                reason,
            } => {
                println!("Skipped {} during ingest: {}", path_display, reason);
                if let Some(observer) = &observer {
                    observer.file_skipped(&path_display, &reason);
                }
            }
        }
    }

    Ok(())
}

/// Ingest a directory tree into `namespace`.
///
/// The rebuild runs as a store generation (upstream 1.14 pipeline): nodes
/// are written under a fresh generation for this source and the active
/// pointer flips only after every producer, embed, and upsert step
/// succeeds, so a failed ingest leaves the previous generation -- the
/// previous publication -- fully intact. The fork's reserved-namespace
/// guard is kept at this entry point: the internal staging prefix is
/// never a legal ingest destination.
pub async fn ingest_directory(
    dir_path: &Path,
    schema: &ParserSchema,
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    namespace: &str,
    observer: Option<Arc<dyn IngestObserver>>,
) -> anyhow::Result<()> {
    if is_reserved_ingest_destination(namespace) {
        anyhow::bail!("ingest destination uses the reserved internal staging namespace");
    }

    // Prove the root is a readable directory before anything is staged. A
    // missing or mistyped path used to clear the namespace, walk nothing -- the
    // walker's errors were skipped -- and report success over an emptied graph.
    std::fs::read_dir(dir_path).map_err(|e| {
        anyhow::anyhow!(
            "Refusing to ingest {:?} into namespace {}: it is not a readable directory, so nothing was staged: {}",
            dir_path,
            namespace,
            e
        )
    })?;

    // Two worktrees of one repository ingest into the same namespace. Their
    // nodes are kept apart by a generation, and the source identity records
    // which worktree wrote them (worktree_id = the canonical directory path;
    // repo/revision resolution is Task 2's `source::resolve`). The generation
    // pointer is keyed by this source, so publishing one worktree cannot
    // overwrite the other's active pointer.
    let worktree_id = source_key_for(dir_path);
    let source_key = worktree_id.clone();
    let source_identity = crate::context::model::SourceIdentity {
        worktree_id,
        ..Default::default()
    };
    let source_identity_json = serde_json::to_value(&source_identity)?;

    // Allocate a fresh generation. Nothing is deleted: the previous active
    // generation stays visible until this one publishes, so a failed build
    // leaves the graph exactly as it was.
    let generation = vector_store
        .begin_generation(namespace, &source_key)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Refusing to ingest: could not begin a staging generation for namespace {}: {}",
                namespace,
                e
            )
        })?;


    let ext_to_lang = build_ext_map(schema);
    let zero_vector = vec![0.0; embedder.dimensions()];
    let namespace_owned = namespace.to_string();
    let dir_path_owned = dir_path.to_path_buf();
    let schema_clone = schema.clone();

    let (tx, rx) = mpsc::channel::<IngestItem>(256);

    // The blocking producer walks the filesystem and parses files on a
    // blocking thread, streaming owned results through the channel.
    let producer = tokio::task::spawn_blocking(move || {
        run_producer(
            dir_path_owned,
            ext_to_lang,
            schema_clone,
            tx,
            namespace_owned,
            generation,
        )
    });

    // The async consumer embeds and stores each item on the async runtime.
    let observer_clone = observer.clone();
    let consumer_result = run_consumer(
        rx,
        embedder.clone(),
        vector_store.clone(),
        namespace,
        observer_clone,
        zero_vector,
        generation,
        source_identity_json,
    )
    .await;

    let producer_result = producer.await;

    // Any failure aborts the staging generation: the previous active generation
    // stays reachable, and this one's half-built tree is discarded.
    let failure: Option<anyhow::Error> = match (consumer_result, producer_result) {
        (Ok(()), Ok(Ok(()))) => None,
        (Err(e), _) => Some(e),
        (Ok(()), Ok(Err(e))) => Some(e),
        (Ok(()), Err(e)) => Some(anyhow::anyhow!("ingest producer panicked: {}", e)),
    };
    if let Some(e) = failure {
        if let Err(abort) = vector_store
            .abort_generation(namespace, &source_key, generation)
            .await
        {
            eprintln!(
                "Failed to abort staging generation {} of namespace {}: {}",
                generation, namespace, abort
            );
        }
        return Err(e);
    }

    // Remember the generation this publish would supersede. The relink after
    // the flip is a separate store call, so if it fails the pointer is rolled
    // back onto this generation (and the new one discarded) before the error
    // is returned: publish + relink succeed or fail together, as they did
    // when both lived inside the fork's single publish transaction.
    let previous_generation = match vector_store
        .active_generation(namespace, &source_key)
        .await
    {
        Ok(previous) => previous,
        Err(e) => {
            if let Err(abort) = vector_store
                .abort_generation(namespace, &source_key, generation)
                .await
            {
                eprintln!(
                    "Failed to abort staging generation {} of namespace {}: {}",
                    generation, namespace, abort
                );
            }
            return Err(anyhow::anyhow!(
                "stored the nodes of namespace {} but could not read the active generation before publishing generation {}: {}",
                namespace,
                generation,
                e
            ));
        }
    };

    // Atomically flip the active pointer onto this generation. Only now are its
    // nodes reachable; the previous generation remains as history.
    vector_store
        .publish_generation(namespace, &source_key, generation)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "stored the nodes of namespace {} but could not make generation {} active: {}",
                namespace,
                generation,
                e
            )
        })?;

    // A rule is not rewritten just because its file came back, so replay what
    // the memories still declare now that the nodes they point at are the
    // active generation (bead neurostrata-sij).
    //
    // A failure here fails the ingest rather than warning about it: reporting
    // success would hand back a graph whose rules reach none of their code.
    let linked = match vector_store.relink_edges(namespace, &source_key).await {
        Ok(linked) => linked,
        Err(e) => {
            // The new generation is live but its rule edges are not: roll the
            // pointer back onto the previous generation and discard this one,
            // so the failed ingest leaves the store exactly as it found it.
            if let Err(rollback) = vector_store
                .publish_generation(namespace, &source_key, previous_generation)
                .await
            {
                return Err(anyhow::anyhow!(
                    "stored the nodes of namespace {} but could not relink the edges its memories declare ({}), and rolling the active pointer back to generation {} also failed ({}); generation {} is live without its rule edges",
                    namespace,
                    e,
                    previous_generation,
                    rollback,
                    generation
                ));
            }
            if let Err(abort) = vector_store
                .abort_generation(namespace, &source_key, generation)
                .await
            {
                eprintln!(
                    "Failed to discard generation {} of namespace {} after relink rollback: {}",
                    generation, namespace, abort
                );
            }
            return Err(anyhow::anyhow!(
                "stored the nodes of namespace {} but could not relink the edges its memories declare, so rules do not reach their code; the previous generation was restored, ingest again to retry: {}",
                namespace,
                e
            ));
        }
    };
    println!(
        "Relinked {} declared edges in namespace {}",
        linked, namespace
    );
    if let Some(observer) = &observer {
        observer.relinked(linked);
    }

    // Cache project_root into the registry row (best-effort) for hook hygiene.
    crate::registry::cache_project_root(
        &vector_store,
        Some(&embedder),
        namespace,
        &dir_path.to_string_lossy(),
    )
    .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_from(json: &str) -> HashMap<String, String> {
        build_ext_map(&ParserSchema::load(json).expect("schema parses"))
    }

    /// The shipped src/schema.json declares extensions without a dot. The lookup
    /// used to prepend one, so nothing ever matched and no symbols were extracted.
    #[test]
    fn bare_schema_extension_matches_a_file_extension() {
        let map = map_from(r#"{"languages":{"rust":{"extensions":["rs"],"queries":{}}}}"#);
        assert_eq!(map.get(&normalize_ext("rs")), Some(&"rust".to_string()));
    }

    #[test]
    fn dotted_schema_extension_matches_too() {
        let map = map_from(r#"{"languages":{"rust":{"extensions":[".rs"],"queries":{}}}}"#);
        assert_eq!(map.get(&normalize_ext("rs")), Some(&"rust".to_string()));
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let map = map_from(r#"{"languages":{"python":{"extensions":["py"],"queries":{}}}}"#);
        assert_eq!(map.get(&normalize_ext("PY")), Some(&"python".to_string()));
    }

    /// The shipped schema once declared a language key ("javascript") that
    /// get_language() had no arm for, so those files silently produced nothing.
    #[test]
    fn every_language_in_the_shipped_schema_has_a_grammar() {
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        for lang in schema.languages.keys() {
            assert!(
                get_language(lang).is_some(),
                "schema.json declares language '{}' but get_language() has no arm for it",
                lang
            );
        }
    }

    /// And the reverse gap: a grammar nothing can reach, because no extension maps to it.
    #[test]
    fn every_shipped_extension_resolves_to_a_grammar() {
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let map = build_ext_map(&schema);
        for (ext, lang) in &map {
            assert!(
                get_language(lang).is_some(),
                "extension '{}' maps to '{}', which has no grammar",
                ext,
                lang
            );
        }
        assert_eq!(map.get(&normalize_ext("rs")), Some(&"rust".to_string()));
        assert_eq!(map.get(&normalize_ext("tsx")), Some(&"tsx".to_string()));
    }

    /// A project root that is absolute on the platform running the test.
    ///
    /// A literal `C:\dev\projects\neurostrata` is absolute only on Windows. On
    /// Linux and macOS it has no leading separator, so `node_id_for` would take
    /// its `is_relative()` branch and return the whole string instead of
    /// stripping the root.
    fn absolute_root() -> std::path::PathBuf {
        if cfg!(windows) {
            std::path::PathBuf::from(r"C:\dev\projects\neurostrata")
        } else {
            std::path::PathBuf::from("/dev/projects/neurostrata")
        }
    }

    /// Node ids double as edge targets, so a path an agent writes
    /// ("src/lib.rs") has to land on the same string the walker produced.
    #[test]
    fn a_node_id_is_relative_to_the_directory_being_ingested() {
        let root = absolute_root();

        assert_eq!(
            node_id_for(&root, &root.join("src").join("store").join("ladybug.rs")),
            "src/store/ladybug.rs",
            "an absolute walk must produce the same id as a relative one"
        );
        assert_eq!(
            node_id_for(Path::new("."), Path::new("./src/store/ladybug.rs")),
            "src/store/ladybug.rs"
        );
    }

    #[test]
    fn ingesting_a_subdirectory_keeps_the_documented_repo_relative_form() {
        // CLI-readme.md documents `neurostrata-mcp ingest ./src my-rust-project`,
        // and rules name files as "src/lib.rs". Stripping the ingest root would
        // leave "lib.rs" and quietly unlink every rule that names the file.
        assert_eq!(
            node_id_for(Path::new("./src"), Path::new("./src/store/ladybug.rs")),
            "src/store/ladybug.rs"
        );
        assert_eq!(
            node_id_for(Path::new("src"), Path::new("src/lib.rs")),
            "src/lib.rs"
        );
    }

    /// The daemon inherits the working directory of whatever launched it, and a
    /// parent of the project used to win over the ingest root.
    #[test]
    fn a_node_id_does_not_depend_on_the_working_directory() {
        let root = std::env::current_dir()
            .expect("working directory")
            .join("sub");
        assert_eq!(node_id_for(&root, &root.join("a.rs")), "a.rs");
    }

    #[test]
    fn the_ingest_root_is_named_rather_than_left_empty() {
        let root = absolute_root();
        assert_eq!(node_id_for(&root, &root), "neurostrata");
    }

    #[test]
    fn a_path_outside_everything_keeps_its_absolute_shape() {
        // Neither the working directory nor the ingest root is a prefix, so
        // there is nothing to strip. A usable absolute id beats a panic.
        assert_eq!(
            node_id_for(
                Path::new(r"C:\dev\projects\other"),
                Path::new(r"D:\elsewhere\a.rs")
            ),
            "D:/elsewhere/a.rs"
        );
    }

    #[test]
    fn node_paths_normalise_to_one_form() {
        assert_eq!(normalize_node_path(r".\src\lib.rs"), "src/lib.rs");
        assert_eq!(normalize_node_path("./src/lib.rs"), "src/lib.rs");
        assert_eq!(normalize_node_path("src/lib.rs"), "src/lib.rs");
        assert_eq!(normalize_node_path("src/"), "src");
    }

    /// The whole point: two projects both have a `src`, the Memory table has a
    /// single-column primary key, and the database is shared. Without the
    /// namespace in the id the second project ingested takes the first's node.
    /// doctor reports migration state from this, so a wrong answer either hides
    /// a namespace that is still collidable or nags about one that is not.
    #[test]
    fn an_id_is_qualified_only_by_its_own_namespace() {
        assert!(is_qualified("NeuroStrata", "NeuroStrata::src/main.rs"));
        // Written before ids carried a namespace.
        assert!(!is_qualified("NeuroStrata", "src/main.rs"));
        // Another project's node, which this namespace must not count as its own.
        assert!(!is_qualified("NeuroStrata", "Other::src/main.rs"));
        // A namespace that merely starts the same way is a different namespace.
        assert!(!is_qualified("Neuro", "NeuroStrata::src/main.rs"));
    }

    #[test]
    fn two_projects_sharing_a_path_get_different_ids() {
        assert_ne!(
            qualified_id("ProjectA", "src/main.rs"),
            qualified_id("ProjectB", "src/main.rs")
        );
        assert_eq!(qualified_id("NeuroStrata", "src"), "NeuroStrata::src");
    }

    /// A rule names a file the way a human writes it. The id it has to reach is
    /// qualified. If that bridge breaks, every GOVERNS edge silently stops
    /// forming and the graph looks fine while meaning nothing.
    #[test]
    fn a_bare_declaration_still_reaches_its_qualified_id() {
        let known = vec![
            qualified_id("NeuroStrata", "src/store/ladybug.rs"),
            qualified_id("NeuroStrata", "src/daemon.rs"),
        ];
        let index = crate::store::ladybug::KnownIds::new(known.iter().map(String::as_str));
        assert_eq!(
            index.resolve("src/store/ladybug.rs").as_deref(),
            Some("NeuroStrata::src/store/ladybug.rs")
        );
    }

    /// A suffix must not match a longer filename that merely ends the same way.
    #[test]
    fn a_suffix_does_not_match_a_longer_name() {
        let known = vec![qualified_id("NeuroStrata", "src/mylib.rs")];
        let index = crate::store::ladybug::KnownIds::new(known.iter().map(String::as_str));
        assert_eq!(index.resolve("lib.rs"), None);
    }

    /// The same path under two namespaces must resolve to neither. A wrong
    /// GOVERNS edge points a rule at another project's file, which is worse
    /// than the rule having no edge at all.
    #[test]
    fn a_path_held_by_two_namespaces_is_ambiguous() {
        let known = vec![
            qualified_id("ProjectA", "src/main.rs"),
            qualified_id("ProjectB", "src/main.rs"),
        ];
        let index = crate::store::ladybug::KnownIds::new(known.iter().map(String::as_str));
        assert_eq!(index.resolve("src/main.rs"), None);
    }

    #[test]
    fn symbol_ids_are_stable_and_distinguish_repeated_names() {
        let main = sha256_hex("fn main() {}".as_bytes());
        let other_body = sha256_hex("fn main() { loop {} }".as_bytes());
        let a = symbol_id("src/lib.rs", "functions", "main", &main);
        assert_eq!(a, symbol_id("src/lib.rs", "functions", "main", &main));
        assert_ne!(a, symbol_id("src/lib.rs", "functions", "main", &other_body));
        assert_ne!(a, symbol_id("src/other.rs", "functions", "main", &main));
        assert_ne!(a, symbol_id("src/lib.rs", "structs", "main", &main));
    }

    #[test]
    fn short_bodies_are_stored_verbatim_and_long_ones_are_cut() {
        let short = "fn main() {}";
        assert_eq!(truncate_for_embedding(short), short);

        let long = "x".repeat(MAX_SYMBOL_CHARS + 500);
        let cut = truncate_for_embedding(&long);
        assert!(cut.starts_with(&"x".repeat(MAX_SYMBOL_CHARS)));
        assert!(cut.contains("truncated"));
        assert!(cut.chars().filter(|c| *c == 'x').count() == MAX_SYMBOL_CHARS);
    }

    #[test]
    fn unknown_extension_has_no_language() {
        let map = map_from(r#"{"languages":{"rust":{"extensions":["rs"],"queries":{}}}}"#);
        assert_eq!(map.get(&normalize_ext("toml")), None);
    }

    struct ZeroEmbedder;

    #[async_trait::async_trait]
    impl Embedder for ZeroEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.0; 4])
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    /// Stands in for a remote embedder that is down or rate-limited.
    struct FailingEmbedder;

    #[async_trait::async_trait]
    impl Embedder for FailingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Err(anyhow::anyhow!("embedder unavailable"))
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    struct CountingEmbedder(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait::async_trait]
    impl Embedder for CountingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![0.0; 4])
        }

        fn dimensions(&self) -> usize {
            4
        }
    }


    #[tokio::test]
    async fn a_missing_root_is_refused_before_anything_is_cleared() {
        let db = std::env::temp_dir().join(format!("ns-missing-root-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("probe").await.expect("create the schema");

        let node = qualified_id("probe", "src/lib.rs");
        let previous = MemoryPayload {
            content: "Path: src/lib.rs".to_string(),
            user_id: "auto-ingestor".to_string(),
            memory_type: "file".to_string(),
            agent_name: None,
            location: "src/lib.rs".to_string(),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        };
        store
            .upsert("probe", &node, vec![0.0; 4], previous)
            .await
            .expect("seed a previous ingest");

        let missing = std::env::temp_dir().join(format!("ns-no-such-dir-{}", uuid::Uuid::new_v4()));
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let outcome = ingest_directory(
            &missing,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
        )
        .await;

        assert!(
            outcome.is_err(),
            "a root that does not exist is an error, not an empty ingest"
        );
        assert!(
            store
                .get("probe", &node)
                .await
                .expect("read back")
                .is_some(),
            "and the previous ingest is still there"
        );
    }

    #[tokio::test]
    async fn reserved_ingest_destinations_are_rejected_before_staging_or_embedding() {
        let root = std::env::temp_dir().join(format!("reserved-ingest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        let db = std::env::temp_dir().join(format!("reserved-ingest-db-{}", uuid::Uuid::new_v4()));
        let concrete =
            std::sync::Arc::new(crate::store::LadybugStore::for_testing(&db, 4).unwrap());
        concrete.init("reserved-ingest").await.unwrap();
        let store: Arc<dyn VectorStore> = concrete.clone();
        let schema = ParserSchema::load(include_str!("../schema.json")).unwrap();
        let embeds = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active_stage = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        let _active_lease = concrete.begin_ingest_stage(&active_stage).await.unwrap();
        let active_id = format!("{active_stage}{NAMESPACE_SEPARATOR}active.rs");
        store
            .upsert(
                &active_stage,
                &active_id,
                vec![0.13, 0.24, 0.35, 0.46],
                MemoryPayload {
                    content: "held active generation".into(),
                    user_id: "auto-ingestor".into(),
                    memory_type: "file".into(),
                    agent_name: Some("active-fixture".into()),
                    location: "active.rs".into(),
                    location_lines: "3".into(),
                    metadata: serde_json::json!({"generation": "keep"}),
                },
            )
            .await
            .unwrap();
        let active_before = store.get(&active_stage, &active_id).await.unwrap().unwrap();
        let stages_before = concrete.ingest_stage_namespaces().await.unwrap();
        assert_eq!(stages_before, vec![active_stage.clone()]);
        let stage_files_before = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("reserved-ingest-db-")
                    && entry.file_name().to_string_lossy().contains(".stage-")
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();

        let fresh_reserved = format!("{}{}", INTERNAL_INGEST_STAGE_PREFIX, uuid::Uuid::new_v4());
        for namespace in [
            active_stage.clone(),
            fresh_reserved,
            format!("{}malformed", INTERNAL_INGEST_STAGE_PREFIX),
            INTERNAL_INGEST_STAGE_PREFIX.to_string(),
        ] {
            let result = ingest_directory(
                &root,
                &schema,
                Arc::new(CountingEmbedder(embeds.clone())),
                store.clone(),
                &namespace,
                None,
            )
            .await;
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("reserved internal staging namespace"));
        }
        assert_eq!(embeds.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            concrete.ingest_stage_namespaces().await.unwrap(),
            stages_before
        );
        let active_after = store.get(&active_stage, &active_id).await.unwrap().unwrap();
        assert_eq!(active_after.0, active_before.0);
        assert_eq!(active_after.1.content, active_before.1.content);
        assert_eq!(active_after.1.user_id, active_before.1.user_id);
        assert_eq!(active_after.1.memory_type, active_before.1.memory_type);
        assert_eq!(active_after.1.agent_name, active_before.1.agent_name);
        assert_eq!(active_after.1.location, active_before.1.location);
        assert_eq!(
            active_after.1.location_lines,
            active_before.1.location_lines
        );
        assert_eq!(active_after.1.metadata, active_before.1.metadata);
        let stage_files_after = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("reserved-ingest-db-")
                    && entry.file_name().to_string_lossy().contains(".stage-")
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert_eq!(
            stage_files_after, stage_files_before,
            "rejected public destinations create no stage lease"
        );
        concrete
            .publish_ingested("active-stage-published", &active_stage)
            .await
            .expect("the pre-existing active stage remains publishable");
        let published = store
            .get(
                "active-stage-published",
                "active-stage-published::active.rs",
            )
            .await
            .unwrap()
            .expect("the held generation publishes after rejected requests");
        assert_eq!(published.0, active_before.0);
        assert_eq!(published.1.content, "held active generation");
        assert_eq!(
            published.1.metadata,
            serde_json::json!({"generation": "keep"})
        );
        assert!(concrete.ingest_stage_namespaces().await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(db);
    }

    #[tokio::test]
    async fn producer_consumer_streams_items_to_store() {
        // Create a temp directory with a parseable file.
        let tmp =
            std::env::temp_dir().join(format!("ns-producer-consumer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("hello.rs"), "fn main() {}").expect("write test file");

        let db =
            std::env::temp_dir().join(format!("ns-producer-consumer-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.expect("create the schema");
        store
            .upsert(
                "test-ns",
                "rule-1",
                vec![0.0; 4],
                MemoryPayload {
                    content: "governs hello.rs".into(),
                    user_id: "agent".into(),
                    memory_type: "rule".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({"governs": ["hello.rs"]}),
                },
            )
            .await
            .expect("seed rule before its target exists");

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let outcome = ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "test-ns",
            None,
        )
        .await;

        assert!(outcome.is_ok(), "ingestion succeeded: {:?}", outcome.err());

        // The first ingest publishes generation 1.
        assert_eq!(
            store
                .active_generation("test-ns", &source_key_for(&tmp))
                .await
                .expect("read"),
            1,
            "the first ingest publishes generation 1"
        );

        // The file node should exist, generation-scoped.
        let file_id = generation_scoped_id("test-ns", 1, "hello.rs");
        let entry = store.get("test-ns", &file_id).await.expect("read back");
        assert!(entry.is_some(), "file node was stored");

        // The symbol node should exist (fn main), keyed by body hash.
        let sym_id = generation_scoped_id(
            "test-ns",
            1,
            &symbol_id(
                "hello.rs",
                "functions",
                "main",
                &sha256_hex("fn main() {}".as_bytes()),
            ),
        );
        let sym_entry = store.get("test-ns", &sym_id).await.expect("read back");
        assert!(sym_entry.is_some(), "symbol node was stored");

        let graph = store
            .export_graph(false, false, &HashMap::new(), None)
            .await
            .expect("export graph after first publish");
        assert!(
            graph["links"].as_array().unwrap().iter().any(|link| {
                link["source"] == "rule-1" && link["target"] == file_id && link["type"] == "GOVERNS"
            }),
            "successful publication relinks the existing rule edge"
        );

        std::fs::remove_file(tmp.join("hello.rs")).expect("remove stale source");
        std::fs::write(tmp.join("world.rs"), "fn world() {}").expect("write replacement");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "test-ns",
            None,
        )
        .await
        .expect("publish replacement");
        // Merge integration: this tail came from the fork's version of the
        // test, whose pipeline deleted the superseded rows on publish, so
        // it asserted the old ids read back as gone and the new file under
        // its unscoped id. The generation pipeline keeps superseded rows
        // as history and scopes live ids by generation; the same intent --
        // the replacement publication supersedes the old one -- is asserted
        // here in generation terms: the pointer moved to generation 2 and
        // the new file node is live under its generation-scoped id (the
        // stale-edge assertion below covers the dropped GOVERNS link).
        assert_eq!(
            store
                .active_generation("test-ns", &source_key_for(&tmp))
                .await
                .expect("read"),
            2,
            "the replacement ingest publishes generation 2"
        );
        let world_id = generation_scoped_id("test-ns", 2, "world.rs");
        assert!(store.get("test-ns", &world_id).await.unwrap().is_some());
        let graph = store
            .export_graph(false, false, &HashMap::new(), None)
            .await
            .expect("export graph after replacement");
        assert!(
            !graph["links"].as_array().unwrap().iter().any(|link| {
                link["source"] == "rule-1" && link["target"] == file_id && link["type"] == "GOVERNS"
            }),
            "removed targets lose their stale rule edges"
        );

        // Clean up.
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn two_worktrees_ingest_the_same_namespace_into_distinct_generations() {
        // Two worktrees of one repository share a namespace but not a source.
        // Each must publish its own generation, never merge onto the other's.
        let worktree_a =
            std::env::temp_dir().join(format!("ns-worktree-a-{}", uuid::Uuid::new_v4()));
        let worktree_b =
            std::env::temp_dir().join(format!("ns-worktree-b-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&worktree_a).expect("create worktree a");
        std::fs::create_dir_all(&worktree_b).expect("create worktree b");
        std::fs::write(worktree_a.join("hello.rs"), "fn main() {}").expect("write a");
        std::fs::write(worktree_b.join("hello.rs"), "fn main() { loop {} }").expect("write b");

        let db =
            std::env::temp_dir().join(format!("ns-worktrees-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("shared-ns").await.expect("create the schema");

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");

        ingest_directory(
            &worktree_a,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "shared-ns",
            None,
        )
        .await
        .expect("worktree a ingests");
        assert_eq!(
            store
                .active_generation("shared-ns", &source_key_for(&worktree_a))
                .await
                .expect("read"),
            1
        );

        ingest_directory(
            &worktree_b,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "shared-ns",
            None,
        )
        .await
        .expect("worktree b ingests");
        assert_eq!(
            store
                .active_generation("shared-ns", &source_key_for(&worktree_b))
                .await
                .expect("read"),
            2
        );

        // Source scoping: B's publish must not overwrite A's active pointer.
        assert_eq!(
            store
                .active_generation("shared-ns", &source_key_for(&worktree_a))
                .await
                .expect("read"),
            1,
            "worktree B's publish must not overwrite worktree A's active generation"
        );

        // Each generation's file node coexists; the two worktrees never confluate.
        let a_id = generation_scoped_id("shared-ns", 1, "hello.rs");
        let b_id = generation_scoped_id("shared-ns", 2, "hello.rs");
        assert!(store.get("shared-ns", &a_id).await.expect("read").is_some());
        assert!(store.get("shared-ns", &b_id).await.expect("read").is_some());
        assert_ne!(a_id, b_id, "two generations must not share a physical id");

        std::fs::remove_dir_all(&worktree_a).ok();
        std::fs::remove_dir_all(&worktree_b).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn a_failed_staging_leaves_the_previous_generation_active() {
        // A missing file mid-walk fails the producer only after some nodes have
        // already staged. The failed generation must be discarded and the
        // previous active generation left untouched.
        let db = std::env::temp_dir().join(format!("ns-staging-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("stage-ns").await.expect("create the schema");

        // Publish generation 1 from a real directory.
        let first =
            std::env::temp_dir().join(format!("ns-stage-first-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&first).expect("create first dir");
        std::fs::write(first.join("hello.rs"), "fn main() {}").expect("write first");

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        ingest_directory(
            &first,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "stage-ns",
            None,
        )
        .await
        .expect("first ingest publishes");
        assert_eq!(
            store
                .active_generation("stage-ns", &source_key_for(&first))
                .await
                .expect("read"),
            1
        );

        // A missing root fails before a generation is even begun, so the active
        // generation stays 1 and no staging rows appear.
        let missing =
            std::env::temp_dir().join(format!("ns-stage-missing-{}", uuid::Uuid::new_v4()));
        let outcome = ingest_directory(
            &missing,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "stage-ns",
            None,
        )
        .await;
        assert!(outcome.is_err(), "a missing root is refused");

        assert_eq!(
            store
                .active_generation("stage-ns", &source_key_for(&first))
                .await
                .expect("read"),
            1,
            "the previous active generation is untouched by a failed ingest"
        );
        let first_id = generation_scoped_id("stage-ns", 1, "hello.rs");
        assert!(
            store.get("stage-ns", &first_id).await.expect("read").is_some(),
            "the previous generation's nodes remain"
        );

        std::fs::remove_dir_all(&first).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn a_mid_stage_failure_aborts_and_keeps_the_previous_generation_active() {
        // A failure after staging has begun (an embed that fails on the first
        // symbol) must abort the staging generation: its half-built rows are
        // deleted and the previous active generation stays published. This is
        // the abort path the missing-root test cannot reach.
        let db = std::env::temp_dir().join(format!("ns-mid-stage-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("stage-ns").await.expect("create the schema");

        let dir = std::env::temp_dir().join(format!("ns-mid-stage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("hello.rs"), "fn main() {}").expect("write file");

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let source_key = source_key_for(&dir);

        // First ingest publishes generation 1.
        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "stage-ns",
            None,
        )
        .await
        .expect("first ingest publishes");
        assert_eq!(
            store
                .active_generation("stage-ns", &source_key)
                .await
                .expect("read"),
            1
        );

        // Second ingest begins generation 2, stages the file node, then the
        // symbol embed fails mid-stage.
        let outcome = ingest_directory(
            &dir,
            &schema,
            Arc::new(FailingEmbedder),
            store.clone(),
            "stage-ns",
            None,
        )
        .await;
        assert!(outcome.is_err(), "a mid-stage embed failure aborts the ingest");

        // The aborted generation's rows are gone and generation 1 stays active.
        assert_eq!(
            store
                .active_generation("stage-ns", &source_key)
                .await
                .expect("read"),
            1,
            "the previous active generation survives a mid-stage abort"
        );
        let gen1_id = generation_scoped_id("stage-ns", 1, "hello.rs");
        assert!(
            store.get("stage-ns", &gen1_id).await.expect("read").is_some(),
            "generation 1's nodes remain after the abort"
        );
        let gen2_id = generation_scoped_id("stage-ns", 2, "hello.rs");
        assert!(
            store.get("stage-ns", &gen2_id).await.expect("read").is_none(),
            "the aborted generation's staged nodes are deleted"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Delegating store double whose `relink_edges` fails while `fail_relink`
    /// is set, so a test can fail an ingest after its publish flip — the one
    /// failure the staging-abort tests cannot reach.
    struct RelinkFailingStore {
        inner: Arc<dyn VectorStore>,
        fail_relink: std::sync::atomic::AtomicBool,
    }

    impl crate::traits::AsAny for RelinkFailingStore {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[async_trait::async_trait]
    impl VectorStore for RelinkFailingStore {
        async fn init(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.init(namespace).await
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        async fn upsert(
            &self,
            namespace: &str,
            id: &str,
            vector: Vec<f32>,
            payload: MemoryPayload,
        ) -> anyhow::Result<crate::traits::UpsertReport> {
            self.inner.upsert(namespace, id, vector, payload).await
        }

        async fn search(
            &self,
            namespace: &str,
            vector: Vec<f32>,
            limit: usize,
            include_tasks: bool,
            include_archived: bool,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            self.inner
                .search(namespace, vector, limit, include_tasks, include_archived)
                .await
        }

        async fn delete(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.delete(namespace, id).await
        }

        async fn clear_ingested(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.clear_ingested(namespace).await
        }

        async fn begin_ingest_stage(
            &self,
            namespace: &str,
        ) -> anyhow::Result<Box<dyn Send>> {
            self.inner.begin_ingest_stage(namespace).await
        }

        async fn publish_ingested(
            &self,
            namespace: &str,
            staged_namespace: &str,
        ) -> anyhow::Result<usize> {
            self.inner.publish_ingested(namespace, staged_namespace).await
        }

        async fn relink_edges(
            &self,
            namespace: &str,
            source_key: &str,
        ) -> anyhow::Result<usize> {
            if self
                .fail_relink
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                anyhow::bail!("injected relink failure (test double)");
            }
            self.inner.relink_edges(namespace, source_key).await
        }

        async fn begin_generation(
            &self,
            namespace: &str,
            source_key: &str,
        ) -> anyhow::Result<u64> {
            self.inner.begin_generation(namespace, source_key).await
        }

        async fn publish_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            self.inner
                .publish_generation(namespace, source_key, generation)
                .await
        }

        async fn abort_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            self.inner
                .abort_generation(namespace, source_key, generation)
                .await
        }

        async fn active_generation(
            &self,
            namespace: &str,
            source_key: &str,
        ) -> anyhow::Result<u64> {
            self.inner.active_generation(namespace, source_key).await
        }

        async fn generation_nodes(
            &self,
            namespace: &str,
            generation: u64,
        ) -> anyhow::Result<Vec<String>> {
            self.inner.generation_nodes(namespace, generation).await
        }

        async fn list(
            &self,
            namespace: &str,
            user_id: Option<&str>,
            memory_type: Option<&str>,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            self.inner.list(namespace, user_id, memory_type).await
        }

        async fn get(
            &self,
            namespace: &str,
            id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
            self.inner.get(namespace, id).await
        }

        async fn relocate(
            &self,
            id: &str,
            from: &str,
            to: &str,
        ) -> anyhow::Result<crate::traits::RelocateOutcome> {
            self.inner.relocate(id, from, to).await
        }

        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            self.inner.list_namespaces().await
        }

        async fn export_graph(
            &self,
            include_retired: bool,
            include_archived: bool,
            project_contexts: &HashMap<String, std::path::PathBuf>,
            anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
        ) -> anyhow::Result<serde_json::Value> {
            self.inner
                .export_graph(
                    include_retired,
                    include_archived,
                    project_contexts,
                    anchor_override,
                )
                .await
        }

        async fn increment_access_count(
            &self,
            namespace: &str,
            id: &str,
        ) -> anyhow::Result<()> {
            self.inner.increment_access_count(namespace, id).await
        }

        async fn export_database(&self, dir: &str) -> anyhow::Result<()> {
            self.inner.export_database(dir).await
        }

        async fn checkpoint(&self) -> anyhow::Result<()> {
            self.inner.checkpoint().await
        }

        fn is_dirty(&self) -> bool {
            self.inner.is_dirty()
        }
    }

    #[tokio::test]
    async fn a_failed_relink_rolls_back_to_the_previous_generation() {
        // Publish flips the active pointer before relink replays the declared
        // edges, so a relink failure used to leave the new generation live
        // without its rule edges. The ingest must fail with the previous
        // generation restored — pointer, nodes, and edges — and the failed
        // generation discarded.
        let db = std::env::temp_dir().join(format!("ns-relink-db-{}", uuid::Uuid::new_v4()));
        let inner: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        let failing = Arc::new(RelinkFailingStore {
            inner: inner.clone(),
            fail_relink: std::sync::atomic::AtomicBool::new(false),
        });
        let store: Arc<dyn VectorStore> = failing.clone();
        store.init("relink-ns").await.expect("create the schema");

        // A rule declaring an edge to a file the walk is about to publish.
        inner
            .upsert(
                "relink-ns",
                "rule-1",
                vec![0.0; 4],
                MemoryPayload {
                    content: "governs hello.rs".into(),
                    user_id: "agent".into(),
                    memory_type: "rule".into(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({"governs": ["hello.rs"]}),
                },
            )
            .await
            .expect("seed rule before its target exists");

        let dir = std::env::temp_dir().join(format!("ns-relink-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("hello.rs"), "fn main() {}").expect("write file");

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let source_key = source_key_for(&dir);

        // First ingest publishes generation 1 and relinks the rule edge.
        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-ns",
            None,
        )
        .await
        .expect("first ingest publishes");
        assert_eq!(
            inner
                .active_generation("relink-ns", &source_key)
                .await
                .expect("read"),
            1
        );
        let gen1_id = generation_scoped_id("relink-ns", 1, "hello.rs");
        let graph = inner
            .export_graph(false, false, &HashMap::new(), None)
            .await
            .expect("export graph after first publish");
        assert!(
            graph["links"].as_array().unwrap().iter().any(|link| {
                link["source"] == "rule-1" && link["target"] == gen1_id && link["type"] == "GOVERNS"
            }),
            "the first publish relinks the rule edge onto generation 1"
        );

        // Second ingest, with relink failing after the publish flip.
        std::fs::write(dir.join("hello.rs"), "fn main() { loop {} }").expect("rewrite file");
        failing
            .fail_relink
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let outcome = ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-ns",
            None,
        )
        .await;
        let err = outcome.expect_err("a failed relink fails the ingest");
        assert!(
            format!("{}", err).contains("relink"),
            "the error names the relink failure: {}",
            err
        );

        // The previous generation is active again, with its nodes and the
        // rule edge intact; the failed generation is neither live nor stored.
        assert_eq!(
            inner
                .active_generation("relink-ns", &source_key)
                .await
                .expect("read"),
            1,
            "a failed relink restores the previous active generation"
        );
        assert!(
            inner.get("relink-ns", &gen1_id).await.expect("read").is_some(),
            "generation 1's nodes remain after the rollback"
        );
        let gen2_id = generation_scoped_id("relink-ns", 2, "hello.rs");
        assert!(
            inner.get("relink-ns", &gen2_id).await.expect("read").is_none(),
            "the failed generation's nodes are discarded"
        );
        let graph = inner
            .export_graph(false, false, &HashMap::new(), None)
            .await
            .expect("export graph after rollback");
        assert!(
            graph["links"].as_array().unwrap().iter().any(|link| {
                link["source"] == "rule-1" && link["target"] == gen1_id && link["type"] == "GOVERNS"
            }),
            "generation 1's rule edge survives the rollback"
        );

        // With relink healthy again the retry publishes the next generation.
        failing
            .fail_relink
            .store(false, std::sync::atomic::Ordering::SeqCst);
        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-ns",
            None,
        )
        .await
        .expect("retry publishes");
        assert_eq!(
            inner
                .active_generation("relink-ns", &source_key)
                .await
                .expect("read"),
            3,
            "the retry publishes a fresh generation"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// Records the documents the walk skipped, so a test asserts on the
    /// report rather than on what the walk printed.
    struct SkipRecorder {
        skipped: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    }

    impl IngestObserver for SkipRecorder {
        fn file_ingested(&self, _path: &str, _symbols: usize) {}

        fn relinked(&self, _edges: usize) {}

        fn file_skipped(&self, path: &str, reason: &str) {
            self.skipped
                .lock()
                .expect("recorder lock")
                .push((path.to_string(), reason.to_string()));
        }
    }

    /// Task 6 (J15): a document whose text trips the scanner is skipped
    /// whole — no file node, no symbols — and reported with the scanner's
    /// reason, while the rest of the walk proceeds.
    #[tokio::test]
    async fn secret_in_ingest_is_refused() {
        let tmp = std::env::temp_dir().join(format!("ns-ingest-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("good.rs"), "fn clean() {}").expect("write clean file");
        // Built at runtime so no secret-shaped literal sits in the source.
        let secret = format!("ghp_{}", "Ab1".repeat(12));
        std::fs::write(
            tmp.join("evil.rs"),
            format!("fn leak() {{\n    let key = \"{}\";\n}}\n", secret),
        )
        .expect("write leaking file");

        let db =
            std::env::temp_dir().join(format!("ns-ingest-secret-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.expect("create the schema");

        let skipped = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observer: Arc<dyn IngestObserver> = Arc::new(SkipRecorder {
            skipped: skipped.clone(),
        });

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let outcome = ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "test-ns",
            Some(observer),
        )
        .await;
        assert!(
            outcome.is_ok(),
            "the walk proceeds past the skipped document: {:?}",
            outcome.err()
        );

        // The clean document landed whole.
        let good_sym = generation_scoped_id(
            "test-ns",
            1,
            &symbol_id("good.rs", "functions", "clean", &sha256_hex("fn clean() {}".as_bytes())),
        );
        assert!(
            store
                .get("test-ns", &good_sym)
                .await
                .expect("read back")
                .is_some(),
            "clean symbol was stored"
        );

        // The tripping document contributed nothing: no file node, no symbol.
        let evil_node = generation_scoped_id("test-ns", 1, "evil.rs");
        assert!(
            store
                .get("test-ns", &evil_node)
                .await
                .expect("read back")
                .is_none(),
            "skipped document's file node was not stored"
        );
        let evil_sym = generation_scoped_id(
            "test-ns",
            1,
            &symbol_id("evil.rs", "functions", "leak", &sha256_hex("fn leak() {\n    let key = \"\";\n}".as_bytes())),
        );
        assert!(
            store
                .get("test-ns", &evil_sym)
                .await
                .expect("read back")
                .is_none(),
            "skipped document's symbol was not stored"
        );

        // And the skip was reported with the file and the scanner's reason.
        let skipped = skipped.lock().expect("recorder lock");
        assert_eq!(skipped.len(), 1, "exactly one skip reported: {:?}", *skipped);
        assert!(
            skipped[0].0.ends_with("evil.rs"),
            "skip names the file: {}",
            skipped[0].0
        );
        assert!(
            skipped[0].1.contains("ERROR [SECURITY]"),
            "skip carries the scanner verdict: {}",
            skipped[0].1
        );
        assert!(
            skipped[0].1.contains("github_pat"),
            "skip carries the scanner category: {}",
            skipped[0].1
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }
}
