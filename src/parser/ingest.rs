use crate::parser::schema::ParserSchema;
use crate::parser::get_language;
use crate::traits::{Embedder, VectorStore, MemoryPayload};
use ignore::WalkBuilder;
use std::path::Path;
use std::sync::Arc;
use std::collections::HashMap;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

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
    format!("{}\n... (truncated at {} characters)", cut, MAX_SYMBOL_CHARS)
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

/// The separator between a namespace and the path it qualifies.
///
/// Two colons because a path cannot contain them on any platform we ingest
/// from, so splitting a qualified id back apart is unambiguous.
pub const NAMESPACE_SEPARATOR: &str = "::";

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
        // Absolute: prefer the working directory, so ingesting an absolute
        // subdirectory of the project still reads relative to the project.
        std::env::current_dir()
            .ok()
            .and_then(|cwd| path.strip_prefix(&cwd).ok().map(Path::to_path_buf))
            .unwrap_or_else(|| path.strip_prefix(root).unwrap_or(path).to_path_buf())
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
    body: String,
}

/// Stable identity for one symbol. Re-ingesting unchanged code produces the same
/// id, so the store upserts instead of accumulating a second copy under a fresh
/// UUID. The line number keeps overloads and repeated names distinct.
fn symbol_id(path: &str, kind: &str, name: &str, start_line: usize) -> String {
    format!("{}#{}:{}@{}", path, kind, name, start_line)
}

pub async fn ingest_directory(
    dir_path: &Path,
    schema: &ParserSchema,
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    namespace: &str,
) -> anyhow::Result<()> {
    // Rebuild this namespace's ingested rows from scratch. Failing here is fatal:
    // ingesting on top of a stale tree would leave rows for files that no longer exist.
    vector_store
        .clear_ingested(namespace)
        .await
        .map_err(|e| anyhow::anyhow!("Refusing to ingest: could not clear the previous ingest of namespace {}: {}", namespace, e))?;

    let ext_to_lang = build_ext_map(schema);

    let walker_builder = WalkBuilder::new(dir_path);
    // Explicitly ignore common 3rd party and build directories even if not gitignored
    let walker = walker_builder.build();

    let skipped_dirs = [
        "node_modules", "target", "vendor", ".venv", "venv", "env", ".env",
        "dist", "build", "out", ".dolt", ".git", ".next", ".nuxt", "__pycache__",
        ".fastembed_cache", ".idea", ".vscode", "coverage"
    ];

    let zero_vector = vec![0.0; embedder.dimensions()];

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let path_str = path.to_string_lossy();
        
        let mut should_skip = false;
        for skip_dir in &skipped_dirs {
            let skip_pattern = format!("/{}/", skip_dir);
            let skip_start = format!("{}/", skip_dir);
            let skip_exact = format!("./{}", skip_dir);
            if path_str.contains(&skip_pattern) || path_str.starts_with(&skip_start) || path_str == skip_exact {
                should_skip = true;
                break;
            }
        }
        if should_skip {
            continue;
        }

        // Upsert the directory or file node to build the graph
        let abs_path = if path.is_absolute() {
            path.to_string_lossy().to_string()
        } else {
            std::env::current_dir().unwrap_or_default().join(path).to_string_lossy().to_string()
        };

        let node_path = node_id_for(dir_path, path);

        // Create parent edge mapping
        let parent_id = if let Some(p) = path.parent() {
            let p_str = node_id_for(dir_path, p);
            if p_str != "." && p_str != "" && p_str != node_path {
                Some(p_str)
            } else {
                None
            }
        } else {
            None
        };

        let mut contained_by = Vec::new();
        if let Some(pid) = parent_id {
            contained_by.push(qualified_id(namespace, &pid));
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

        // Upsert this structural node
        let mut metadata = serde_json::Map::new();
        metadata.insert("absolute_path".to_string(), serde_json::json!(abs_path));
        // Structure is containment, not a semantic link: the directory contains the file.
        metadata.insert("contained_by".to_string(), serde_json::json!(contained_by));

        let node_id = qualified_id(namespace, &node_path);
        let payload = MemoryPayload {
            content: format!("Path: {}", node_path),
            location: node_path.clone(),
            location_lines: String::new(),
            memory_type: mem_type.to_string(),
            metadata: serde_json::Value::Object(metadata),
            user_id: "auto-ingestor".to_string(),
            agent_name: Some("neurostrata-mcp-ingestor".to_string()),
        };

        if let Err(e) = vector_store.upsert(namespace, &node_id, zero_vector.clone(), payload).await {
            eprintln!("Failed to upsert graph node {}: {}", node_id, e);
        }

        if !is_file {
            continue;
        }

        // Now if it is a parseable file, extract AST nodes
        if let Some(ext_os) = path.extension() {
            let ext = normalize_ext(&ext_os.to_string_lossy());
            if let Some(lang_name) = ext_to_lang.get(&ext) {
                if let Some(ts_lang) = get_language(lang_name) {
                    let content = match std::fs::read_to_string(path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };

                    let mut parser = Parser::new();
                    parser.set_language(&ts_lang)?;

                    let tree = match parser.parse(&content, None) {
                        Some(t) => t,
                        None => continue,
                    };

                    let lang_schema = &schema.languages[lang_name];

                    // Collect every symbol before touching the store. tree-sitter's
                    // Node and QueryMatch are not Send, so holding one across an await
                    // would make this future non-Send and the axum handler that calls
                    // ingestion would stop compiling.
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
                            // A match carries the definition node and its @name; pair them
                            // so each symbol becomes its own memory rather than being
                            // concatenated into one row per file.
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

                            pending.push(SymbolRow {
                                name: name.to_string(),
                                kind: query_name.clone(),
                                start_line: definition.start_position().row + 1,
                                end_line: definition.end_position().row + 1,
                                body: truncate_for_embedding(
                                    definition.utf8_text(content.as_bytes()).unwrap_or(""),
                                ),
                            });
                        }
                    }

                    let mut symbols_stored = 0usize;
                    for symbol in pending {
                        let summary = format!(
                            "{} {} in {}\nlines {}-{}\n\n{}",
                            symbol.kind, symbol.name, node_path, symbol.start_line, symbol.end_line, symbol.body
                        );
                        let id = qualified_id(
                            namespace,
                            &symbol_id(&node_path, &symbol.kind, &symbol.name, symbol.start_line),
                        );
                        let lines = format!("{}-{}", symbol.start_line, symbol.end_line);

                        let mut metadata = serde_json::Map::new();
                        metadata.insert("domain".to_string(), serde_json::json!("code_ast"));
                        metadata.insert("symbol".to_string(), serde_json::json!(symbol.name));
                        metadata.insert("symbol_kind".to_string(), serde_json::json!(symbol.kind));
                        metadata.insert("language".to_string(), serde_json::json!(lang_name));
                        // The file contains the symbol, so this is a CONTAINS edge too.
                        metadata.insert(
                            "contained_by".to_string(),
                            serde_json::json!([qualified_id(namespace, &node_path)]),
                        );
                        metadata.insert("refs".to_string(), serde_json::json!([
                            { "file": node_path.clone(), "lines": lines.clone() }
                        ]));

                        let payload = MemoryPayload {
                            content: summary.clone(),
                            location: node_path.clone(),
                            location_lines: lines,
                            memory_type: "code_ast".to_string(),
                            metadata: serde_json::Value::Object(metadata),
                            user_id: "auto-ingestor".to_string(),
                            agent_name: Some("neurostrata-mcp-ingestor".to_string()),
                        };

                        match embedder.embed(&summary).await {
                            Ok(embedding) => {
                                if let Err(e) = vector_store.upsert(namespace, &id, embedding, payload).await {
                                    eprintln!("Failed to store symbol {} from {}: {}", symbol.name, path.display(), e);
                                } else {
                                    symbols_stored += 1;
                                }
                            }
                            Err(e) => eprintln!("Failed to embed symbol {} from {}: {}", symbol.name, path.display(), e),
                        }
                    }

                    if symbols_stored > 0 {
                        println!("Ingested {} symbols from {}", symbols_stored, path.display());
                    }
                }
            }
        }
    }

    // Clearing the previous ingest took every edge attached to those nodes with
    // it, including the GOVERNS edges rules had to them -- and a rule is not
    // rewritten just because its file came back. Replay what the memories still
    // declare, now that the nodes they point at exist again (bead
    // neurostrata-sij).
    match vector_store.relink_edges(namespace).await {
        Ok(linked) => println!("Relinked {} declared edges in namespace {}", linked, namespace),
        Err(e) => eprintln!(
            "WARNING: ingestion finished but the declared edges could not be relinked, so rules may not reach their code: {}",
            e
        ),
    }

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
        let schema = ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
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
        let schema = ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let map = build_ext_map(&schema);
        for (ext, lang) in &map {
            assert!(get_language(lang).is_some(), "extension '{}' maps to '{}', which has no grammar", ext, lang);
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
        assert_eq!(
            crate::store::ladybug::resolve_declared_target("src/store/ladybug.rs", &known).as_deref(),
            Some("NeuroStrata::src/store/ladybug.rs")
        );
    }

    /// A suffix must not match a longer filename that merely ends the same way.
    #[test]
    fn a_suffix_does_not_match_a_longer_name() {
        let known = vec![qualified_id("NeuroStrata", "src/mylib.rs")];
        assert_eq!(
            crate::store::ladybug::resolve_declared_target("lib.rs", &known),
            None
        );
    }

#[test]
    fn symbol_ids_are_stable_and_distinguish_repeated_names() {
        let a = symbol_id("src/lib.rs", "functions", "main", 12);
        assert_eq!(a, symbol_id("src/lib.rs", "functions", "main", 12));
        assert_ne!(a, symbol_id("src/lib.rs", "functions", "main", 40));
        assert_ne!(a, symbol_id("src/other.rs", "functions", "main", 12));
        assert_ne!(a, symbol_id("src/lib.rs", "structs", "main", 12));
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
}
