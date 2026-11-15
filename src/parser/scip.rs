//! Tier-2 SCIP (`scip.proto`) compiler-grade index ingestion (`neurostrata-rbde`).
//!
//! Uses the official Apache-2.0 `scip` crate (`scip::types::Index`, `scip::symbol::parse_symbol`)
//! and `protobuf::Message` to ingest compiler-produced `.scip` indexes (`rust-analyzer scip`,
//! `scip-typescript`, `scip-python`, `scip-go`, `scip-java`) and link exact cross-file/cross-module
//! definitions, references, and trait/interface implementations onto `code_ast` symbol nodes.

use crate::traits::VectorStore;
use anyhow::{Context, Result};
use protobuf::Message;
use scip::types::{Index, SymbolRole};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScipIngestReport {
    pub documents_processed: usize,
    pub definitions_indexed: usize,
    pub references_linked: usize,
    pub symbols_enriched: usize,
    pub edges_created: usize,
}

/// Parse raw `.scip` protobuf bytes into a `scip::types::Index`.
pub fn parse_scip_bytes(bytes: &[u8]) -> Result<Index> {
    Index::parse_from_bytes(bytes).context("failed to decode SCIP protobuf index")
}

/// Extract the terminal identifier name from a SCIP symbol string using `scip::symbol::parse_symbol`.
pub fn extract_terminal_symbol_name(scip_symbol: &str) -> Option<String> {
    if scip::symbol::is_local_symbol(scip_symbol) {
        return None;
    }
    if let Ok(parsed) = scip::symbol::parse_symbol(scip_symbol) {
        if let Some(last) = parsed.descriptors.last() {
            if !last.name.is_empty() {
                return Some(last.name.clone());
            }
        }
    }
    // Fallback for non-standard symbol strings: strip trailing punctuation and take last segment.
    let trimmed = scip_symbol.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
    let tail = trimmed
        .rsplit(|c: char| matches!(c, '/' | '#' | '.' | ':' | ' '))
        .next()?;
    if tail.is_empty() {
        None
    } else {
        Some(tail.to_string())
    }
}

fn normalize_scip_path(rel: &str) -> String {
    rel.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

fn parse_line_range(lines: &str) -> Option<(usize, usize)> {
    let (start_s, end_s) = lines.split_once('-')?;
    let start = start_s.trim().parse::<usize>().ok()?;
    let end = end_s.trim().parse::<usize>().ok()?;
    Some((start, end))
}

#[derive(Clone, Debug)]
struct ExistingSymbolRow {
    id: String,
    file_path: String,
    name: String,
    start_line: usize,
    end_line: usize,
}

/// Ingest a `.scip` index file from disk into `namespace`.
pub async fn ingest_scip_index(
    vector_store: Arc<dyn VectorStore>,
    namespace: &str,
    scip_path: &Path,
) -> Result<ScipIngestReport> {
    let bytes = std::fs::read(scip_path)
        .with_context(|| format!("failed to read SCIP index at {}", scip_path.display()))?;
    ingest_scip_bytes(vector_store, namespace, &bytes).await
}

/// Ingest raw `.scip` protobuf bytes into `namespace`, enriching `code_ast` symbols
/// with exact compiler definitions, documentation, implementations, and call/reference edges.
pub async fn ingest_scip_bytes(
    vector_store: Arc<dyn VectorStore>,
    namespace: &str,
    bytes: &[u8],
) -> Result<ScipIngestReport> {
    let index = parse_scip_bytes(bytes)?;
    let all_rows = vector_store.list(namespace, None, None).await?;

    let mut symbols_by_file: HashMap<String, Vec<ExistingSymbolRow>> = HashMap::new();
    let mut symbol_by_id: HashMap<String, ExistingSymbolRow> = HashMap::new();

    for row in all_rows {
        if row.payload.memory_type != "code_ast" {
            continue;
        }
        let file_path = normalize_scip_path(&row.payload.location);
        let name = row
            .payload
            .metadata
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let (start_line, end_line) =
            parse_line_range(&row.payload.location_lines).unwrap_or((1, usize::MAX));
        let info = ExistingSymbolRow {
            id: row.id.clone(),
            file_path: file_path.clone(),
            name,
            start_line,
            end_line,
        };
        symbols_by_file
            .entry(file_path)
            .or_default()
            .push(info.clone());
        symbol_by_id.insert(row.id, info);
    }

    // Sort each file's symbols by span size (innermost first) for accurate line containment.
    for list in symbols_by_file.values_mut() {
        list.sort_by_key(|s| s.end_line.saturating_sub(s.start_line));
    }

    let mut report = ScipIngestReport {
        documents_processed: index.documents.len(),
        ..Default::default()
    };

    // Map SCIP symbol string -> NeuroStrata symbol row ID
    let mut scip_def_to_node_id: HashMap<String, String> = HashMap::new();
    let mut node_scip_symbol: HashMap<String, String> = HashMap::new();
    let mut node_scip_docs: HashMap<String, Vec<String>> = HashMap::new();
    let mut node_scip_implements: HashMap<String, Vec<String>> = HashMap::new();

    // Pass 1: Match SCIP definitions and SymbolInformation to NeuroStrata `code_ast` nodes.
    for doc in &index.documents {
        let rel_path = normalize_scip_path(&doc.relative_path);
        let Some(file_syms) = symbols_by_file.get(&rel_path) else {
            continue;
        };

        for occ in &doc.occurrences {
            let is_def = (occ.symbol_roles & (SymbolRole::Definition as i32)) != 0;
            if !is_def || scip::symbol::is_local_symbol(&occ.symbol) {
                continue;
            }
            let occ_line = occ.range.first().copied().unwrap_or(0).max(0) as usize + 1;
            let term_name = extract_terminal_symbol_name(&occ.symbol);

            let matched = file_syms
                .iter()
                .find(|s| {
                    term_name.as_deref() == Some(s.name.as_str())
                        && occ_line >= s.start_line
                        && occ_line <= s.end_line
                })
                .or_else(|| {
                    file_syms
                        .iter()
                        .find(|s| term_name.as_deref() == Some(s.name.as_str()))
                })
                .or_else(|| {
                    file_syms
                        .iter()
                        .find(|s| occ_line >= s.start_line && occ_line <= s.end_line)
                });

            if let Some(sym_row) = matched {
                report.definitions_indexed += 1;
                scip_def_to_node_id.insert(occ.symbol.clone(), sym_row.id.clone());
                node_scip_symbol.insert(sym_row.id.clone(), occ.symbol.clone());
            }
        }

        for sym_info in &doc.symbols {
            if scip::symbol::is_local_symbol(&sym_info.symbol) {
                continue;
            }
            let target_id = if let Some(id) = scip_def_to_node_id.get(&sym_info.symbol) {
                Some(id.clone())
            } else {
                let term_name = extract_terminal_symbol_name(&sym_info.symbol);
                file_syms
                    .iter()
                    .find(|s| term_name.as_deref() == Some(s.name.as_str()))
                    .map(|s| {
                        report.definitions_indexed += 1;
                        scip_def_to_node_id.insert(sym_info.symbol.clone(), s.id.clone());
                        node_scip_symbol.insert(s.id.clone(), sym_info.symbol.clone());
                        s.id.clone()
                    })
            };

            if let Some(node_id) = target_id {
                if !sym_info.documentation.is_empty() {
                    node_scip_docs.insert(node_id.clone(), sym_info.documentation.clone());
                }
                let impls: Vec<String> = sym_info
                    .relationships
                    .iter()
                    .filter(|rel| rel.is_implementation)
                    .map(|rel| rel.symbol.clone())
                    .collect();
                if !impls.is_empty() {
                    node_scip_implements.insert(node_id, impls);
                }
            }
        }
    }

    // Pass 2: Link SCIP reference occurrences to enclosing caller symbols.
    let mut scip_edges_by_caller: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut scip_callees_by_caller: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    let mut scip_callers_by_callee: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();

    for doc in &index.documents {
        let rel_path = normalize_scip_path(&doc.relative_path);
        let Some(file_syms) = symbols_by_file.get(&rel_path) else {
            continue;
        };

        for occ in &doc.occurrences {
            let is_def = (occ.symbol_roles & (SymbolRole::Definition as i32)) != 0;
            if is_def || scip::symbol::is_local_symbol(&occ.symbol) {
                continue;
            }
            let Some(target_id) = scip_def_to_node_id.get(&occ.symbol) else {
                continue;
            };
            let Some(target_info) = symbol_by_id.get(target_id) else {
                continue;
            };
            let occ_line = occ.range.first().copied().unwrap_or(0).max(0) as usize + 1;
            let Some(caller_info) = file_syms
                .iter()
                .find(|s| occ_line >= s.start_line && occ_line <= s.end_line)
            else {
                continue;
            };
            if caller_info.id == *target_id {
                continue;
            }

            report.references_linked += 1;
            let inserted = scip_edges_by_caller
                .entry(caller_info.id.clone())
                .or_default()
                .insert(target_id.clone());
            if inserted {
                report.edges_created += 1;
                if caller_info.file_path != target_info.file_path {
                    scip_callees_by_caller
                        .entry(caller_info.id.clone())
                        .or_default()
                        .push(serde_json::json!({
                            "symbol_id": target_info.id,
                            "symbol_name": target_info.name,
                            "file_path": target_info.file_path,
                            "scip_symbol": occ.symbol,
                            "resolution": "SCIP",
                        }));
                    scip_callers_by_callee
                        .entry(target_info.id.clone())
                        .or_default()
                        .push(serde_json::json!({
                            "symbol_id": caller_info.id,
                            "symbol_name": caller_info.name,
                            "file_path": caller_info.file_path,
                            "scip_symbol": occ.symbol,
                            "resolution": "SCIP",
                        }));
                }
            }
        }
    }

    // Also link SCIP `is_implementation` relationships where the implemented interface/trait
    // exists in `scip_def_to_node_id`.
    for (impl_node_id, trait_scip_syms) in &node_scip_implements {
        for trait_sym in trait_scip_syms {
            if let Some(trait_node_id) = scip_def_to_node_id.get(trait_sym) {
                if impl_node_id != trait_node_id {
                    let inserted = scip_edges_by_caller
                        .entry(impl_node_id.clone())
                        .or_default()
                        .insert(trait_node_id.clone());
                    if inserted {
                        report.edges_created += 1;
                    }
                }
            }
        }
    }

    let mut dirty_nodes: BTreeSet<String> = BTreeSet::new();
    dirty_nodes.extend(node_scip_symbol.keys().cloned());
    dirty_nodes.extend(node_scip_docs.keys().cloned());
    dirty_nodes.extend(node_scip_implements.keys().cloned());
    dirty_nodes.extend(scip_edges_by_caller.keys().cloned());
    dirty_nodes.extend(scip_callees_by_caller.keys().cloned());
    dirty_nodes.extend(scip_callers_by_callee.keys().cloned());

    for node_id in dirty_nodes {
        if let Some((vec, mut payload)) = vector_store.get(namespace, &node_id).await? {
            if let Some(obj) = payload.metadata.as_object_mut() {
                if let Some(scip_sym) = node_scip_symbol.get(&node_id) {
                    obj.insert("scip_symbol".to_string(), serde_json::json!(scip_sym));
                }
                if let Some(docs) = node_scip_docs.get(&node_id) {
                    obj.insert("scip_documentation".to_string(), serde_json::json!(docs));
                }
                if let Some(impls) = node_scip_implements.get(&node_id) {
                    obj.insert("scip_implements".to_string(), serde_json::json!(impls));
                }
                if let Some(targets) = scip_edges_by_caller.get(&node_id) {
                    let mut deps: Vec<String> = obj
                        .get("depends_on")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    for t in targets {
                        if !deps.contains(t) {
                            deps.push(t.clone());
                        }
                    }
                    deps.sort();
                    deps.dedup();
                    obj.insert("depends_on".to_string(), serde_json::json!(deps.clone()));
                    obj.insert("calls_resolved".to_string(), serde_json::json!(deps));
                }
                if let Some(callees) = scip_callees_by_caller.get(&node_id) {
                    let mut existing: Vec<serde_json::Value> = obj
                        .get("cross_file_callees")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    existing.extend(callees.iter().cloned());
                    obj.insert(
                        "cross_file_callees".to_string(),
                        serde_json::json!(existing),
                    );
                }
                if let Some(callers) = scip_callers_by_callee.get(&node_id) {
                    let mut existing: Vec<serde_json::Value> = obj
                        .get("cross_file_callers")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    existing.extend(callers.iter().cloned());
                    obj.insert(
                        "cross_file_callers".to_string(),
                        serde_json::json!(existing),
                    );
                }
            }
            vector_store
                .upsert(namespace, &node_id, vec, payload)
                .await?;
            report.symbols_enriched += 1;
        }
    }

    crate::telemetry::insights::sample_insights_after_ingest(&vector_store, namespace, None).await;

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ingest::{extract_called_identifiers, resolve_dependency_paths};

    #[test]
    fn extract_called_identifiers_masks_comments_and_strings_and_handles_turbofish() {
        let body = r#"
            fn caller() {
                // fake_comment_call(1);
                /* block_comment_call(2); */
                let s = "string_literal_call(3)";
                real_callee(42);
                generic_callee::<String>(s);
            }
        "#;
        let calls = extract_called_identifiers(body, "caller");
        assert_eq!(
            calls,
            vec!["generic_callee".to_string(), "real_callee".to_string()]
        );
    }

    #[test]
    fn resolve_dependency_paths_resolves_super_and_self_prefixes() {
        let mut all_paths = std::collections::HashSet::new();
        all_paths.insert("src/parser/ingest.rs".to_string());
        all_paths.insert("src/parser/scip.rs".to_string());
        all_paths.insert("src/formal_kernel.rs".to_string());

        let imports = vec![
            "use:super::scip::ingest_scip_index".to_string(),
            "use:crate::formal_kernel::resolve_cross_file_call".to_string(),
        ];
        let deps = resolve_dependency_paths("src/parser/ingest.rs", &imports, &all_paths);
        assert_eq!(
            deps,
            vec![
                "src/formal_kernel.rs".to_string(),
                "src/parser/scip.rs".to_string()
            ]
        );
    }

    #[test]
    fn scip_symbol_parser_extracts_terminal_identifier_and_decodes_protobuf() {
        let sym = "rust-analyzer cargo neurostrata-mcp 1.17.2 parser/scip/ingest_scip_bytes().";
        assert_eq!(
            extract_terminal_symbol_name(sym),
            Some("ingest_scip_bytes".to_string())
        );
        assert_eq!(extract_terminal_symbol_name("local 4"), None);

        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "src/parser/scip.rs".to_string();
        let mut occ = scip::types::Occurrence::new();
        occ.range = vec![10, 4, 10, 21];
        occ.symbol = sym.to_string();
        occ.symbol_roles = SymbolRole::Definition as i32;
        doc.occurrences.push(occ);
        index.documents.push(doc);

        let bytes = index.write_to_bytes().expect("serialize SCIP index");
        let decoded = parse_scip_bytes(&bytes).expect("decode SCIP index");
        assert_eq!(decoded.documents.len(), 1);
        assert_eq!(decoded.documents[0].relative_path, "src/parser/scip.rs");
    }

    #[tokio::test]
    async fn tier1_workspace_linker_and_tier2_scip_ingest_enrich_cross_file_edges() {
        use crate::parser::ingest::{resolve_cross_file_calls_for_generation, StagedSymbolInfo};
        use crate::store::LadybugStore;
        use crate::traits::MemoryPayload;

        let tmp = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn VectorStore> =
            Arc::new(LadybugStore::new(tmp.path().join("db"), 4).expect("ladybug store"));
        store.init("scip-test").await.expect("init store");

        let sym_a_id = "scip-test::1::src/a.rs#fn:caller_fn@111".to_string();
        let sym_b_id = "scip-test::1::src/b.rs#fn:imported_helper@222".to_string();
        let sym_c_id = "scip-test::1::src/c.rs#fn:scip_target@333".to_string();

        for (id, file, name, lines, calls) in [
            (
                &sym_a_id,
                "src/a.rs",
                "caller_fn",
                "1-20",
                vec!["imported_helper"],
            ),
            (&sym_b_id, "src/b.rs", "imported_helper", "5-15", vec![]),
            (&sym_c_id, "src/c.rs", "scip_target", "10-25", vec![]),
        ] {
            store
                .upsert(
                    "scip-test",
                    id,
                    vec![0.1, 0.2, 0.3, 0.4],
                    MemoryPayload {
                        content: format!("fn {name}()"),
                        location: file.to_string(),
                        location_lines: lines.to_string(),
                        memory_type: "code_ast".to_string(),
                        metadata: serde_json::json!({
                            "symbol": name,
                            "symbol_kind": "fn",
                            "calls": calls,
                        }),
                        user_id: "test".to_string(),
                        agent_name: None,
                    },
                )
                .await
                .expect("upsert symbol");
        }

        let staged = vec![
            StagedSymbolInfo {
                id: sym_a_id.clone(),
                file_path: "src/a.rs".to_string(),
                name: "caller_fn".to_string(),
                kind: "fn".to_string(),
                calls: vec!["imported_helper".to_string()],
            },
            StagedSymbolInfo {
                id: sym_b_id.clone(),
                file_path: "src/b.rs".to_string(),
                name: "imported_helper".to_string(),
                kind: "fn".to_string(),
                calls: vec![],
            },
            StagedSymbolInfo {
                id: sym_c_id.clone(),
                file_path: "src/c.rs".to_string(),
                name: "scip_target".to_string(),
                kind: "fn".to_string(),
                calls: vec![],
            },
        ];
        let mut dep_map = HashMap::new();
        dep_map.insert("src/a.rs".to_string(), vec!["src/b.rs".to_string()]);

        let linked =
            resolve_cross_file_calls_for_generation(&store, "scip-test", &staged, &dep_map).await;
        assert_eq!(linked, 1);

        let (_, payload_a) = store
            .get("scip-test", &sym_a_id)
            .await
            .unwrap()
            .expect("sym_a present");
        assert_eq!(
            payload_a.metadata["depends_on"],
            serde_json::json!([sym_b_id])
        );
        assert_eq!(
            payload_a.metadata["cross_file_callees"][0]["resolution"],
            serde_json::json!("IMPORTED_FILE")
        );

        // Tier 2: Build SCIP index linking `caller_fn` (src/a.rs line 8) -> `scip_target` (src/c.rs line 10)
        let scip_sym_c = "rust-analyzer cargo pkg 0.1.0 c/scip_target().";
        let mut index = scip::types::Index::new();

        let mut doc_c = scip::types::Document::new();
        doc_c.relative_path = "src/c.rs".to_string();
        let mut def_c = scip::types::Occurrence::new();
        def_c.range = vec![9, 3, 9, 14]; // 0-based line 9 => 1-based line 10
        def_c.symbol = scip_sym_c.to_string();
        def_c.symbol_roles = SymbolRole::Definition as i32;
        doc_c.occurrences.push(def_c);
        let mut sym_info_c = scip::types::SymbolInformation::new();
        sym_info_c.symbol = scip_sym_c.to_string();
        sym_info_c.documentation = vec!["Compiler-verified target".to_string()];
        doc_c.symbols.push(sym_info_c);
        index.documents.push(doc_c);

        let mut doc_a = scip::types::Document::new();
        doc_a.relative_path = "src/a.rs".to_string();
        let mut ref_c = scip::types::Occurrence::new();
        ref_c.range = vec![7, 8, 7, 19]; // 0-based line 7 => 1-based line 8 (inside caller_fn 1-20)
        ref_c.symbol = scip_sym_c.to_string();
        ref_c.symbol_roles = 0; // reference
        doc_a.occurrences.push(ref_c);
        index.documents.push(doc_a);

        let scip_bytes = index.write_to_bytes().expect("serialize scip");
        let report = ingest_scip_bytes(store.clone(), "scip-test", &scip_bytes)
            .await
            .expect("ingest scip");
        assert_eq!(report.definitions_indexed, 1);
        assert_eq!(report.references_linked, 1);
        assert_eq!(report.edges_created, 1);

        let (_, payload_a_after) = store
            .get("scip-test", &sym_a_id)
            .await
            .unwrap()
            .expect("sym_a after scip");
        let deps = payload_a_after.metadata["depends_on"]
            .as_array()
            .expect("depends_on array");
        assert_eq!(deps.len(), 2);

        let (_, payload_c_after) = store
            .get("scip-test", &sym_c_id)
            .await
            .unwrap()
            .expect("sym_c after scip");
        assert_eq!(
            payload_c_after.metadata["scip_symbol"],
            serde_json::json!(scip_sym_c)
        );
        assert_eq!(
            payload_c_after.metadata["cross_file_callers"][0]["resolution"],
            serde_json::json!("SCIP")
        );
    }
}
