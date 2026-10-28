use crate::parser::get_language;
use crate::parser::schema::ParserSchema;
use crate::traits::{Embedder, MemoryPayload, VectorStore};
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
    generation_key, generation_scoped_id, ingest_version, logical_path_of, sha256_hex, split_scoped_id,
    symbol_id, version_gate_tripped, NAMESPACE_SEPARATOR, PARSER_LOGIC_VERSION,
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
/// Node ids are paths, and the Memory table is PRIMARY KEY (id) -- one column,
/// because lbug 0.15.3 has no composite key (see examples/composite_pk_repro.rs
/// for the parser refusing one). So an id is unique across the WHOLE database,
/// while paths are only unique within a project. Every repository has a `src`
/// and a `README.md`, and the database is shared by all of them, which meant the
/// second project ingested silently took the first project's nodes: upsert
/// MERGEs on the id, finds the existing row, and updates it.
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
/// One embedded markdown section.
#[derive(Clone)]
struct DocSection {
    slug: String,
    heading: String,
    text: String,
    sha256: String,
    backticked: Vec<String>,
    links: Vec<String>,
}

/// Parse markdown into sections by heading.
fn parse_markdown_sections(content: &str) -> Vec<DocSection> {
    let mut sections: Vec<DocSection> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_level: usize = 0;
    let mut body: Vec<String> = Vec::new();

    let flush = |heading: Option<String>, _level: usize, body: &Vec<String>, out: &mut Vec<DocSection>| {
        let heading = heading.unwrap_or_else(|| "root".to_string());
        let text = body.join("\n").trim().to_string();
        if text.is_empty() {
            return;
        }
        let slug = slugify(&heading);
        let mut backticked = Vec::new();
        let mut links = Vec::new();
        for line in body {
            let mut rest = line.as_str();
            while let Some(start) = rest.find('`') {
                let after = &rest[start+1..];
                if let Some(end) = after.find('`') {
                    let code = after[..end].trim();
                    if !code.is_empty() && !code.contains('\n') {
                        backticked.push(code.to_string());
                    }
                    rest = &after[end+1..];
                } else {
                    break;
                }
            }
            let mut r = line.as_str();
            while let Some(start) = r.find('[') {
                let after = &r[start..];
                if let Some(bracket_end) = after.find(']') {
                    let rest2 = &after[bracket_end+1..];
                    if rest2.starts_with('(') {
                        if let Some(paren_end) = rest2.find(')') {
                            let url = &rest2[1..paren_end];
                            links.push(url.to_string());
                        }
                    }
                    r = &after[bracket_end+1..];
                } else {
                    break;
                }
            }
        }
        backticked.sort();
        backticked.dedup();
        links.sort();
        links.dedup();
        let sha = sha256_hex(text.as_bytes());
        out.push(DocSection {
            slug,
            heading,
            text,
            sha256: sha,
            backticked,
            links,
        });
    };

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            if level == 0 { continue; }
            if current_heading.is_some() {
                flush(current_heading.take(), current_level, &body, &mut sections);
                body.clear();
            }
            let heading_text = trimmed[level..].trim().to_string();
            current_heading = Some(heading_text);
            current_level = level;
        } else {
            body.push(line.to_string());
        }
    }
    if current_heading.is_some() || !body.is_empty() {
        flush(current_heading.take(), current_level, &body, &mut sections);
    }
    if sections.is_empty() {
        let text = content.trim().to_string();
        if !text.is_empty() {
            sections.push(DocSection {
                slug: "root".to_string(),
                heading: "root".to_string(),
                text: text.clone(),
                sha256: sha256_hex(text.as_bytes()),
                backticked: vec![],
                links: vec![],
            });
        }
    }
    sections
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c == ' ' || c == '-' || c == '_') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

// `symbol_id` and `NAMESPACE_SEPARATOR` are re-exported from
// `crate::parser::generation` at the top of this module.

/// Told what the walk has done so far, so a caller does not have to wait for
/// the whole thing to find out. Implementations are called from the walk, so
/// they must be cheap and must not block.
pub trait IngestObserver: Send + Sync {
    fn file_ingested(&self, path: &str, symbols: usize);
    fn file_skipped(&self, path: &str) { let _ = path; }
    fn relinked(&self, edges: usize);
    /// Called once per run when the previous generation's `ingest_version`
    /// differs from the current one (or is absent). Implementors surface this
    /// as a warning; the ingest continues with a full re-parse.
    fn version_gate_tripped(&self, _prev: &str, _current: &str) {}
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
    /// Embedded markdown sections.
    DocSections {
        node_path: String,
        sections: Vec<DocSection>,
        file_sha256: String,
    },
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
    prev_file_map: std::collections::HashMap<String, String>,
    force_reparse: bool,
    ingest_version: String,
    observer: Option<std::sync::Arc<dyn IngestObserver>>,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut current_hashes: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
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

        // Incremental skip: if this file's hash matches the previous active
        // generation, carry it forward instead of re-parsing/re-embedding.
        if is_file {
            if !force_reparse {
                if let Some(prev_hash) = prev_file_map.get(&node_path) {
                    if prev_hash == file_sha256.as_deref().unwrap_or("") {
                        if let Some(obs) = &observer {
                            obs.file_skipped(&path_str);
                        }
                        // Record the current hash so unchanged rows can be carried.
                        if let Some(h) = file_sha256.as_deref() {
                            current_hashes.insert(node_path.clone(), h.to_string());
                        }
                        continue;
                    }
                }
            }
        }
        // Record the current hash for every file the walk visits.
        if let Some(h) = file_sha256.as_deref() {
            current_hashes.insert(node_path.clone(), h.to_string());
        }

        // Apply backpressure: if the channel is full, block until space is
        // available rather than silently dropping the item. If the consumer
        // has dropped (e.g. it errored), propagate the failure instead of
        // reporting success over a partial ingest.
        if let Err(e) = tx.blocking_send(IngestItem::NodeUpsert {
            node_id,
            node_path: node_path.clone(),
            contained_by,
            mem_type: mem_type.to_string(),
            abs_path,
            file_sha256: file_sha256.clone(),
        }) {
            return Err(anyhow::anyhow!("ingest consumer dropped: {}", e));
        }

        // For markdown files, parse into sections and stream them for embedding.
        if is_file && path_str.ends_with(".md") {
            if let Some(bytes) = std::fs::read(path).ok() {
                let content = String::from_utf8_lossy(&bytes).to_string();
                let sections = parse_markdown_sections(&content);
                if !sections.is_empty() {
                    let file_sha = sha256_hex(&bytes);
                    if let Err(e) = tx.blocking_send(IngestItem::DocSections {
                        node_path: node_path.clone(),
                        sections,
                        file_sha256: file_sha,
                    }) {
                        return Err(anyhow::anyhow!("ingest consumer dropped: {}", e));
                    }
                }
            }
        }

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

    Ok(current_hashes)
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
    ingest_version: &str,
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
                metadata.insert("ingest_version".to_string(), serde_json::json!(ingest_version));

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
                    metadata.insert("ingest_version".to_string(), serde_json::json!(ingest_version));
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
                            "failed to embed symbol {} from {}: {}",
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
                                "failed to store symbol {} from {}: {}",
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
        IngestItem::DocSections {
            node_path,
            sections,
            file_sha256,
        } => {
            for section in sections {
                let id = generation_scoped_id(
                    namespace,
                    generation,
                    &format!("{}#section:{}@{}", node_path, section.slug, section.sha256),
                );
                let mut metadata = serde_json::Map::new();
                metadata.insert("domain".to_string(), serde_json::json!("docs"));
                metadata.insert("heading".to_string(), serde_json::json!(section.heading));
                metadata.insert("slug".to_string(), serde_json::json!(section.slug));
                metadata.insert("backticked".to_string(), serde_json::json!(section.backticked));
                metadata.insert("links".to_string(), serde_json::json!(section.links));
                metadata.insert("file_sha256".to_string(), serde_json::json!(file_sha256));
                metadata.insert("ingest_version".to_string(), serde_json::json!(ingest_version));
                metadata.insert(
                    "contained_by".to_string(),
                    serde_json::json!([generation_scoped_id(namespace, generation, &node_path)]),
                );
                metadata.insert("source_identity".to_string(), with_relative_path(&source_identity, &node_path));
                metadata.insert("index_generation".to_string(), serde_json::json!(generation));

                let summary = format!("{}\n\n{}", section.heading, section.text);
                let payload = MemoryPayload {
                    content: summary.clone(),
                    location: node_path.clone(),
                    location_lines: String::new(),
                    memory_type: "doc_section".to_string(),
                    metadata: serde_json::Value::Object(metadata),
                    user_id: "auto-ingestor".to_string(),
                    agent_name: Some("neurostrata-mcp-ingestor".to_string()),
                };

                let embedding = embedder.embed(&summary).await.map_err(|e| {
                    anyhow::anyhow!("failed to embed doc section {}: {}", section.heading, e)
                })?;
                vector_store
                    .upsert(namespace, &id, embedding, payload)
                    .await
                    .map_err(|e| anyhow::anyhow!("failed to store doc section {}: {}", id, e))?;
            }
        }
    }
    }

    Ok(())
}

pub async fn ingest_directory(
    dir_path: &Path,
    schema: &ParserSchema,
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    namespace: &str,
    observer: Option<Arc<dyn IngestObserver>>,
) -> anyhow::Result<()> {
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

    // Previous active generation for this source (for incremental carry-forward).
    let prev_gen = vector_store
        .active_generation(namespace, &source_key)
        .await
        .unwrap_or(0);

    // Build prev_file_map: node_path -> file_sha256 for files in prev gen.
    let mut prev_file_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut prev_version: Option<String> = None;
    if prev_gen > 0 {
        if let Ok(rows) = vector_store.list(namespace, None, None).await {
            for r in rows {
                let id = r.id.clone();
                let (ns, gen, tail) = match crate::parser::ingest::split_scoped_id(&id) {
                    Some(x) => x,
                    None => continue,
                };
                if gen != prev_gen { continue; }
                let hash = r.payload.metadata.get("file_sha256").and_then(|v| v.as_str()).map(|s| s.to_string());
                if let Some(h) = hash {
                    prev_file_map.insert(tail.to_string(), h.clone());
                }
                if prev_version.is_none() {
                    if let Some(v) = r.payload.metadata.get("ingest_version").and_then(|v| v.as_str()) {
                        if !v.is_empty() {
                            prev_version = Some(v.to_string());
                        }
                    }
                }
            }
        }
    }

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

    // Version gate: a changed (or missing) ingest_version stamps a full re-parse.
    let current_version = ingest_version(&schema.fingerprint());
    let force_reparse = prev_gen > 0 && version_gate_tripped(prev_version.as_deref(), &current_version);
    if force_reparse {
        if let Some(obs) = &observer {
            obs.version_gate_tripped(prev_version.as_deref().unwrap_or("(none)"), &current_version);
        }
    }

    let (tx, rx) = mpsc::channel::<IngestItem>(256);

    // The blocking producer walks the filesystem and parses files on a
    // blocking thread, streaming owned results through the channel.
    let prev_file_map_clone = prev_file_map.clone();
    let observer_clone = observer.clone();
    let current_version_clone = current_version.clone();
    let producer = tokio::task::spawn_blocking(move || {
        run_producer(
            dir_path_owned,
            ext_to_lang,
            schema_clone,
            tx,
            namespace_owned,
            generation,
            prev_file_map_clone,
            force_reparse,
            current_version_clone,
            observer_clone,
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
        &current_version,
    )
    .await;

    let producer_result = producer.await;

    // Any failure aborts the staging generation: the previous active generation
    // stays reachable, and this one's half-built tree is discarded.
    let mut current_hashes = std::collections::HashMap::new();
    let failure: Option<anyhow::Error> = match (consumer_result, producer_result) {
        (Ok(()), Ok(Ok(h))) => {
            current_hashes = h;
            None
        }
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

    // Carry forward unchanged nodes from the previous generation so the
    // new generation contains the full tree without re-embedding.
    if prev_gen > 0 {
        let mut carried = 0usize;
        if let Ok(rows) = vector_store.list(namespace, None, None).await {
            for r in rows {
                let id = r.id.clone();
                let (ns, gen, tail) = match crate::parser::ingest::split_scoped_id(&id) {
                    Some(x) => x,
                    None => continue,
                };
                if gen != prev_gen { continue; }
                let hash = r.payload.metadata.get("file_sha256").and_then(|v| v.as_str()).map(|s| s.to_string());
                // Carry a previous row only when its file is still present with
                // the same bytes: deleted files and stale symbols from changed
                // files drop out of the new generation.
                let unchanged = match &hash {
                    Some(h) => {
                        let file_path = tail.split('#').next().unwrap_or(&tail);
                        current_hashes.get(file_path) == Some(h)
                    }
                    None => true, // directories / no-hash nodes carried forward
                };
                if !unchanged { continue; }
                if let Ok(Some((vec, mut payload))) = vector_store.get(namespace, &id).await {
                    payload.metadata.as_object_mut().map(|o| { o.insert("index_generation".to_string(), serde_json::json!(generation)); });
                    let new_id = crate::parser::ingest::generation_scoped_id(namespace, generation, &tail);
                    if vector_store.upsert(namespace, &new_id, vec, payload).await.is_ok() {
                        carried += 1;
                    }
                }
            }
        }
        println!("Carried forward {} unchanged nodes", carried);
    }

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
    let linked = vector_store
        .relink_edges(namespace, &source_key)
        .await
        .map_err(|e| {
        anyhow::anyhow!(
            "stored the nodes of namespace {} but could not relink the edges its memories declare, so rules do not reach their code; ingest again to retry: {}",
            namespace,
            e
        )
    })?;
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

    struct FailingEmbedder;

    #[async_trait::async_trait]
    impl Embedder for FailingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("synthetic embed failure")
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

        // Mutate the file so the incremental skip does not short-circuit the
        // staging path: the second ingest must actually stage + embed.
        std::fs::write(dir.join("hello.rs"), "fn main() { loop {} }").expect("rewrite file");

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
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn version_gate_trips_on_schema_change_and_deletion_does_not_resurrect() {
        // First ingest publishes generation 1; nothing is skipped.
        let tmp = std::env::temp_dir().join(format!("ns-version-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("hello.rs"), "fn main() {}").expect("write file");
        let db = std::env::temp_dir().join(format!("ns-version-gate-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("probe").await.expect("create the schema");

        struct Recorder(std::sync::Mutex<(usize, usize)>); // (skipped, gate trips)
        impl IngestObserver for Recorder {
            fn file_ingested(&self, _path: &str, _symbols: usize) {}
            fn file_skipped(&self, _path: &str) {
                self.0.lock().unwrap().0 += 1;
            }
            fn relinked(&self, _edges: usize) {}
            fn version_gate_tripped(&self, _prev: &str, _current: &str) {
                self.0.lock().unwrap().1 += 1;
            }
        }

        let schema = ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let rec1 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
        ingest_directory(&tmp, &schema, Arc::new(ZeroEmbedder), store.clone(), "probe", Some(rec1.clone()))
            .await
            .expect("first ingest");
        assert_eq!(rec1.0.lock().unwrap().0, 0, "first ingest has nothing to skip");
        assert_eq!(rec1.0.lock().unwrap().1, 0, "first ingest does not trip the gate");

        // Same schema: unchanged file is skipped and the gate stays quiet.
        let rec2 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
        ingest_directory(&tmp, &schema, Arc::new(ZeroEmbedder), store.clone(), "probe", Some(rec2.clone()))
            .await
            .expect("second ingest");
        assert_eq!(rec2.0.lock().unwrap().0, 1, "unchanged file skipped");
        assert_eq!(rec2.0.lock().unwrap().1, 0, "version gate quiet");

        // Changed schema (fingerprint differs): gate trips once, nothing is skipped.
        let mut altered = schema.clone();
        altered
            .languages
            .get_mut("rust")
            .expect("shipped schema has rust")
            .extensions
            .push("rs2".to_string());
        let rec3 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
        ingest_directory(&tmp, &altered, Arc::new(ZeroEmbedder), store.clone(), "probe", Some(rec3.clone()))
            .await
            .expect("third ingest");
        assert_eq!(rec3.0.lock().unwrap().1, 1, "version gate tripped once");
        assert_eq!(rec3.0.lock().unwrap().0, 0, "nothing skipped after a schema change");

        // Deleting the file must not resurrect it in the next generation.
        std::fs::remove_file(tmp.join("hello.rs")).expect("remove file");
        let rec4 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
        ingest_directory(&tmp, &altered, Arc::new(ZeroEmbedder), store.clone(), "probe", Some(rec4.clone()))
            .await
            .expect("fourth ingest");
        let gen4 = store
            .active_generation("probe", &source_key_for(&tmp))
            .await
            .expect("read active generation");
        let file_id = generation_scoped_id("probe", gen4, "hello.rs");
        assert!(
            store.get("probe", &file_id).await.expect("read").is_none(),
            "deleted file must not be resurrected by carry-forward"
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }
}
/// Dry-run ingest: walk the tree and produce a report without writing or
/// downloading anything.
pub async fn dry_run_directory(
    dir_path: &std::path::Path,
    schema: &crate::parser::schema::ParserSchema,
    mode: &str,
) -> anyhow::Result<String> {
    let ext_to_lang = build_ext_map(schema);
    let skipped_dirs = [
        "node_modules", "target", "vendor", ".venv", "venv", "env", ".env",
        "dist", "build", "out", ".dolt", ".git", ".next", ".nuxt",
        "__pycache__", ".fastembed_cache", ".idea", ".vscode", "coverage",
    ];

    let mut files = 0usize;
    let mut parseable = 0usize;
    let mut symbols = 0usize;
    let mut doc_sections = 0usize;
    let mut secret_hits = 0usize;
    let mut file_hashes: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut junk: Vec<String> = Vec::new();
    let mut pii_warnings: Vec<String> = Vec::new();
    let mut groups: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();

    let walker = ignore::WalkBuilder::new(dir_path).build();
    for result in walker {
        let entry = match result { Ok(e) => e, Err(_) => continue };
        let path = entry.path();
        let path_str = path.to_string_lossy();
        let mut should_skip = false;
        for skip_dir in &skipped_dirs {
            let skip_pattern = format!("/{}/", skip_dir);
            let skip_start = format!("{}/", skip_dir);
            let skip_exact = format!("./{}", skip_dir);
            if path_str.contains(&skip_pattern) || path_str.starts_with(&skip_start) || path_str == skip_exact {
                should_skip = true; break;
            }
        }
        if should_skip { continue; }
        let is_file = entry.file_type().map_or(false, |ft| ft.is_file());
        if !is_file { continue; }
        files += 1;
        let bytes = match std::fs::read(path) { Ok(b) => b, Err(_) => continue };
        let hash = sha256_hex(&bytes);
        file_hashes.entry(hash.clone()).or_default().push(path_str.to_string());

        let lang_name = path.extension()
            .map(|e| normalize_ext(&e.to_string_lossy()))
            .and_then(|e| ext_to_lang.get(&e).cloned())
            .filter(|ln| get_language(ln).is_some());

        let content = String::from_utf8_lossy(&bytes).to_string();

        if crate::secrets::scan_entry_point(&content, &serde_json::json!({}), "dry_run").is_some() {
            secret_hits += 1;
        }

        if mode == "presort" {
            if content.contains('@') && content.split_whitespace().any(|w| w.contains('@') && w.contains('.')) {
                pii_warnings.push(format!("{}: possible email", path_str));
            }
            let mut found_ssn = false;
            for window in content.as_bytes().windows(11) {
                let s = String::from_utf8_lossy(window);
                if s.chars().nth(3) == Some('-') && s.chars().nth(6) == Some('-') {
                    found_ssn = true;
                }
            }
            if found_ssn { pii_warnings.push(format!("{}: possible SSN", path_str)); }
            let digits: String = content.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 16 {
                pii_warnings.push(format!("{}: possible credit card digits", path_str));
            }
            if content.trim().len() < 20 {
                junk.push(path_str.to_string());
            }
        }

        if let Some(parent) = path.parent() {
            let g = parent.to_string_lossy().to_string();
            *groups.entry(g).or_insert(0) += 1;
        }

        if path_str.ends_with(".md") {
            let secs = parse_markdown_sections(&content);
            doc_sections += secs.len();
            parseable += 1;
        } else if let Some(ln) = lang_name {
            if let Some(ts_lang) = get_language(&ln) {
                parseable += 1;
                let mut parser = tree_sitter::Parser::new();
                if parser.set_language(&ts_lang).is_ok() {
                    if let Some(tree) = parser.parse(&content, None) {
                        let schema_lang = &schema.languages[&ln];
                        for (_qname, qstr) in &schema_lang.queries {
                            if let Ok(q) = tree_sitter::Query::new(&ts_lang, qstr) {
                                let mut cursor = tree_sitter::QueryCursor::new();
                                let mut iter = cursor.matches(&q, tree.root_node(), content.as_bytes());
                                while let Some(_) = iter.next() { symbols += 1; }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut duplicates: Vec<String> = Vec::new();
    for (h, paths) in &file_hashes {
        if paths.len() > 1 {
            duplicates.push(format!("{} files share hash {}", paths.len(), &h[..12]));
        }
    }

    let mut report = String::new();
    report.push_str(&format!("Dry-run ({}) report:\n", mode));
    report.push_str(&format!("Files: {}\n", files));
    report.push_str(&format!("Parseable files: {}\n", parseable));
    report.push_str(&format!("Symbols: {}\n", symbols));
    report.push_str(&format!("Doc sections: {}\n", doc_sections));
    report.push_str(&format!("Embeddings needed (est): {}\n", symbols + doc_sections));
    report.push_str(&format!("Secret hits: {}\n", secret_hits));
    if mode == "presort" {
        report.push_str(&format!("Junk files: {}\n", junk.len()));
        for j in &junk { report.push_str(&format!("  junk: {}\n", j)); }
        report.push_str(&format!("Duplicate groups: {}\n", duplicates.len()));
        for d in &duplicates { report.push_str(&format!("  dup: {}\n", d)); }
        report.push_str(&format!("PII warnings: {}\n", pii_warnings.len()));
        for w in &pii_warnings { report.push_str(&format!("  pii: {}\n", w)); }
        report.push_str("Proposed dataset groups:\n");
        for (g, c) in &groups { report.push_str(&format!("  {} ({} files)\n", g, c)); }
    }
    Ok(report)
}
