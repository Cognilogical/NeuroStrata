use crate::parser::schema::ParserSchema;
use crate::parser::get_language;
use crate::traits::{Embedder, VectorStore, MemoryPayload};
use ignore::WalkBuilder;
use std::path::Path;
use std::sync::Arc;
use std::collections::HashMap;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

pub async fn ingest_directory(
    dir_path: &Path,
    schema: &ParserSchema,
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    namespace: &str,
) -> anyhow::Result<()> {
    let mut ext_to_lang = HashMap::new();
    for (lang_name, lang_schema) in &schema.languages {
        for ext in &lang_schema.extensions {
            ext_to_lang.insert(ext.clone(), lang_name.clone());
        }
    }

    let walker = WalkBuilder::new(dir_path).build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().map_or(false, |ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();
        if let Some(ext_os) = path.extension() {
            let ext = format!(".{}", ext_os.to_string_lossy());
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
                    let mut extracted_symbols = Vec::new();

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
                            for capture in m.captures {
                                let capture_name = query.capture_names()[capture.index as usize].to_string();
                                let node_text = capture.node.utf8_text(content.as_bytes()).unwrap_or("");
                                extracted_symbols.push(format!("{} ({}): {}", query_name, capture_name, node_text));
                            }
                        }
                    }

                    if !extracted_symbols.is_empty() {
                        let summary = format!("File: {}\nAST Symbols:\n{}", path.display(), extracted_symbols.join("\n"));
                        let id = uuid::Uuid::new_v4().to_string();
                        
                            let mut metadata = serde_json::Map::new();
                            metadata.insert("domain".to_string(), serde_json::json!("code_ast"));
                            metadata.insert("refs".to_string(), serde_json::json!([
                                { "file": path.to_string_lossy().to_string() }
                            ]));

                            let payload = MemoryPayload {
                                content: summary.clone(),
                                location: path.to_string_lossy().to_string(),
                                location_lines: String::new(),
                                memory_type: "code_ast".to_string(),
                                metadata: serde_json::Value::Object(metadata),
                                user_id: "auto-ingestor".to_string(),
                                agent_name: Some("neurostrata-mcp-ingestor".to_string()),
                            };

                            match embedder.embed(&summary).await {
                                Ok(embedding) => {
                                    if let Err(e) = vector_store.upsert(namespace, &id, embedding, payload).await {
                                    eprintln!("Failed to store AST for {}: {}", path.display(), e);
                                } else {
                                    println!("Ingested AST for {}", path.display());
                                }
                            }
                            Err(e) => eprintln!("Failed to embed AST for {}: {}", path.display(), e),
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
