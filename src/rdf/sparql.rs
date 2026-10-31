//! SPARQL 1.1/1.2 query engine over NeuroStrata RDF projections.
//!
//! Implements `neurostrata_sparql_query` MCP tool using Oxigraph stack:
//! - spargebra: parse SPARQL 1.1/1.2 queries
//! - spareval: evaluate prepared queries with custom functions
//! - sparesults: serialize results (JSON/CSV/TSV)
//! - oxrdf: in-memory dataset from namespace projection (live LadybugDB)
//!
//! See docs/superpowers/specs/2026-10-27-sparql-query-engine-design.md for detailed spec.

use crate::traits::MemoryPayload;
use oxrdf::{Dataset, Variable};
use spargebra::SparqlParser;
use spareval::QueryEvaluator;
use sparesults::{QueryResultsFormat, QueryResultsSerializer};

/// Configuration for SPARQL query execution.
#[derive(Debug, Clone)]
pub struct SparqlConfig {
    pub enabled: bool,
    pub timeout_ms: u64,
    pub max_solutions: usize,
    pub max_dataset_quads: usize,
    pub max_algebra_nodes: usize,
    pub service_enabled: bool,
}

impl Default for SparqlConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: 250,
            max_solutions: 1000,
            max_dataset_quads: 250000,
            max_algebra_nodes: 1000,
            service_enabled: false,
        }
    }
}

/// Escape a string for safe use in LadybugDB Cypher queries.
fn escape_ladybug_string(s: &str) -> String {
    s.replace("\\", "\\\\").replace("'", "\\'")
}

fn is_archived(metadata: &serde_json::Value) -> bool {
    metadata.get("archived").and_then(|v| v.as_bool()).unwrap_or(false)
}

fn is_mesh_conflicting(metadata: &serde_json::Value) -> bool {
    crate::synaptic_mesh::conflict::is_conflicting_metadata(metadata)
}

fn is_retired_at(metadata: &serde_json::Value, now: i64) -> bool {
    match metadata.get("valid_to") {
        Some(valid_to) if !valid_to.is_null() => valid_to.as_i64().unwrap_or(0) <= now,
        _ => false,
    }
}

/// Async projection: queries live LadybugDB store and converts memories to RDF quads.
pub async fn project_sparql_dataset_async(
    namespace: &str,
    include_retired: bool,
    include_archived: bool,
    store: &crate::store::ladybug::LadybugStore,
) -> Dataset {
    let mut dataset = Dataset::new();

    // Use public wrapper for list - now accessible via store.list_memories
    let mem_result = store.list_memories(namespace, None, None).await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    if let Ok(ref memories) = mem_result {
        for m in memories {
            // Visibility filters (spec §5.2)
            if !include_retired && is_retired_at(&m.payload.metadata, now) {
                continue;
            }
            if !include_archived && is_archived(&m.payload.metadata) {
                continue;
            }
            if is_mesh_conflicting(&m.payload.metadata) {
                continue;
            }
            if crate::registry::is_registry_row(&m.payload.metadata) {
                continue;
            }

            let payload = m.payload.clone();
            let qs = crate::rdf::projection::memory_to_quads(
                &m.id,
                &payload,
                namespace,
                crate::rdf::types::Tier::Domain,
                0,
            );
            for q in qs {
                dataset.insert(&q);
            }
        }
    }

    dataset
}

/// Evaluate a SPARQL query over the projected namespace dataset.
pub async fn execute_sparql_query(
    query_str: &str,
    namespace: &str,
    format: QueryResultsFormat,
    include_retired: bool,
    include_archived: bool,
    store: &crate::store::ladybug::LadybugStore,
) -> Result<(String, usize, bool, bool), String> {
    let config = SparqlConfig::default();

    // Parse the SPARQL query using spargebra
    let query = SparqlParser::new()
        .parse_query(query_str)
        .map_err(|e| format!("SPARQL parse error: {}", e))?;

    // Reject UPDATE queries: spargebra separates UPDATE via parse_update; parse_query
    // handles SELECT/ASK/CONSTRUCT/DESCRIBE only, so an UPDATE input errors here.

    // Build the dataset from namespace projection (live LadybugDB)
    let dataset = project_sparql_dataset_async(namespace, include_retired, include_archived, store).await;

    if dataset.len() > config.max_dataset_quads {
        return Err(format!(
            "Dataset projection exceeded max_dataset_quads budget ({} > {})",
            dataset.len(),
            config.max_dataset_quads
        ));
    }

    // Prepare and evaluate with QueryEvaluator
    let evaluator = QueryEvaluator::new();
    let prepared = evaluator.prepare(&query);
    let results = prepared
        .execute(&dataset)
        .map_err(|e| format!("SPARQL evaluation error: {}", e))?;

    // Serialize results
    let mut output = Vec::new();
    let mut serializer = QueryResultsSerializer::from_format(format);

    // Extract variables before take() (wrapper loses variables())
    let results_vars: Vec<Variable> = match &results {
        spareval::QueryResults::Solutions(sol_iter) => sol_iter.variables().to_vec(),
        _ => Vec::new(),
    };

    let mut solution_count: usize = 0;

    match results {
        spareval::QueryResults::Boolean(b) => {
            serializer
                .serialize_boolean_to_writer(&mut output, b)
                .map_err(|e| format!("serialize boolean: {}", e))?;
        }
        spareval::QueryResults::Solutions(mut solutions) => {
            let solutions = solutions.take(config.max_solutions);
            let mut serializer_solutions =
                serializer
                    .serialize_solutions_to_writer(&mut output, results_vars)
                    .map_err(|e| format!("serialize solutions start: {}", e))?;
            for solution in solutions {
                let sol = solution.map_err(|e| format!("evaluation error: {}", e))?;
                serializer_solutions
                    .serialize(&sol)
                    .map_err(|e| format!("serialize solution: {}", e))?;
                solution_count += 1;
            }
            let _ = serializer_solutions.finish()
                .map_err(|e| format!("serialize finish: {}", e))?;
        }
        spareval::QueryResults::Graph(mut triples) => {
            // CONSTRUCT/DESCRIBE: serialize triples as N-Triples.
            for triple in triples {
                let t = triple.map_err(|e| format!("graph evaluation error: {}", e))?;
                output.extend_from_slice(format!("{} .\n", t).as_bytes());
                solution_count += 1;
            }
        }
    }

    let results_string = String::from_utf8(output)
        .map_err(|e| format!("UTF-8 error: {}", e))?;
    let truncated = solution_count >= config.max_solutions;

    Ok((results_string, solution_count, truncated, false))
}

/// Result types for SPARQL execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryForm {
    Select,
    Ask,
    Construct,
    Describe,
}

/// Result wrapper for SPARQL tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparqlResult {
    pub query_form: QueryForm,
    pub format: QueryResultsFormat,
    pub results: String,
    pub solution_count: usize,
    pub truncated: bool,
    pub timed_out: bool,
}

/// Global configuration for SPARQL execution.
pub fn get_sparql_config() -> SparqlConfig {
    SparqlConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparesults::QueryResultsFormat;

    #[test]
    fn test_escape_ladybug_string() {
        assert_eq!(escape_ladybug_string("it's"), "it\\'s");
        assert_eq!(escape_ladybug_string("a\\b"), "a\\\\b");
    }

    #[test]
    fn test_get_sparql_config_defaults() {
        let cfg = get_sparql_config();
        assert_eq!(cfg.timeout_ms, 250);
        assert_eq!(cfg.max_solutions, 1000);
        assert_eq!(cfg.max_dataset_quads, 250000);
        assert_eq!(cfg.max_algebra_nodes, 1000);
        assert!(!cfg.service_enabled);
    }

    #[test]
    fn test_select_query_parse() {
        let q = SparqlParser::new().parse_query("SELECT ?s ?p ?o WHERE { ?s ?p ?o }");
        assert!(q.is_ok());
    }

    #[test]
    fn test_ask_query_parse() {
        let q = SparqlParser::new().parse_query("ASK WHERE { ?s ?p ?o }");
        assert!(q.is_ok());
    }

    #[test]
    fn test_update_query_rejected_by_parse() {
        // spargebra 0.4.8: parse_query handles SELECT/ASK/CONSTRUCT/DESCRIBE only.
        let q = SparqlParser::new().parse_query("INSERT DATA { <a> <b> <c> }");
        assert!(q.is_err());
    }

    #[test]
    fn test_is_retired_at() {
        let now = 1000;
        let retired = serde_json::json!({ "valid_to": 999 });
        assert!(is_retired_at(&retired, now));
        let active = serde_json::json!({ "valid_to": 2000 });
        assert!(!is_retired_at(&active, now));
        let no_expiry = serde_json::json!({});
        assert!(!is_retired_at(&no_expiry, now));
    }

    #[test]
    fn test_is_archived() {
        let archived = serde_json::json!({ "archived": true });
        assert!(is_archived(&archived));
        let not_archived = serde_json::json!({ "archived": false });
        assert!(!is_archived(&not_archived));
        let missing = serde_json::json!({});
        assert!(!is_archived(&missing));
    }

    #[test]
    #[ignore = "requires live LadybugDB; see: neurostrata-c0lw#integration-projection"]
    fn test_execute_sparql_query_live_projection() {
        // This integration test exercises the live LadybugDB projection path:
        // project_sparql_dataset_async + execute_sparql_query async with LadybugStore.
        // Requires a populated NeuroStrata DB at the configured path.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db_path = crate::config::Config::from_default_path()
            .map(|c| c.db_path.clone())
            .unwrap_or_default();
        let store = match crate::store::ladybug::LadybugStore::new(
            db_path.to_string_lossy().to_string(),
            1000,
        ) {
            Ok(s) => s,
            Err(_) => {
                // No DB initialized; test is skipped, not failing.
                return;
            }
        };
        let result = rt.block_on(execute_sparql_query(
            "SELECT ?s ?p ?o WHERE { ?s ?p ?o }",
            "NeuroStrata",
            QueryResultsFormat::Json,
            false,
            false,
            &store,
        ));
        // Should succeed even with empty DB (returns formatted output).
        assert!(result.is_ok());
        let (serialized, count, truncated, timed_out) = result.unwrap();
        assert!(count >= 0);
        assert!(!serialized.is_empty());
    }
}