//! SPARQL 1.1/1.2 query engine over NeuroStrata RDF projections.
//!
//! Implements `neurostrata_sparql_query` MCP tool using Oxigraph stack:
//! - spargebra: parse SPARQL 1.1/1.2 queries
//! - spareval: evaluate prepared queries with custom functions
//! - sparesults: serialize results (JSON/CSV/TSV)
//! - oxrdf: in-memory dataset from namespace projection (live LadybugDB)
//!
//! See docs/superpowers/specs/2026-10-27-sparql-query-engine-design.md for detailed spec.

use oxrdf::{Dataset, Variable};
use spargebra::SparqlParser;
use spareval::QueryEvaluator;
use sparesults::{QueryResultsFormat, QueryResultsSerializer};

use std::collections::{HashMap, HashSet};
use crate::parser::ingest::split_scoped_id;
use crate::store::ladybug::active_generations_by_source_of;

/// Configuration for SPARQL query execution.
#[derive(Debug, Clone)]
pub struct SparqlConfig {
    pub timeout_ms: u64,
    pub max_solutions: usize,
    pub max_dataset_quads: usize,
    pub max_algebra_nodes: usize,
}

impl Default for SparqlConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 250,
            max_solutions: 1000,
            max_dataset_quads: 250000,
            max_algebra_nodes: 1000,
        }
    }
}

/// Escape a string for safe use in LadybugDB Cypher queries.
/// Test-only helper: production queries go through spargebra, not string
/// interpolation into Cypher.
#[cfg(test)]
fn escape_ladybug_string(s: &str) -> String {
    s.replace("\\", "\\\\").replace("'", "\\'")
}

/// Create a SparqlParser with all 9 pre-registered prefixes from spec §6.
pub fn parser_with_prefixes() -> SparqlParser {
    SparqlParser::new()
        .with_prefix("ns", "urn:neurostrata:ns#").unwrap()
        .with_prefix("mem", "urn:neurostrata:memory:").unwrap()
        .with_prefix("class", "urn:neurostrata:class:").unwrap()
        .with_prefix("dom", "urn:neurostrata:domain:").unwrap()
        .with_prefix("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#").unwrap()
        .with_prefix("rdfs", "http://www.w3.org/2000/01/rdf-schema#").unwrap()
        .with_prefix("owl", "http://www.w3.org/2002/07/owl#").unwrap()
        .with_prefix("skos", "http://www.w3.org/2004/02/skos/core#").unwrap()
        .with_prefix("xsd", "http://www.w3.org/2001/XMLSchema#").unwrap()
}

fn is_archived(metadata: &serde_json::Value) -> bool {
    metadata.get("archived").and_then(|v| v.as_bool()).unwrap_or(false)}

fn is_mesh_conflicting(metadata: &serde_json::Value) -> bool {
    crate::synaptic_mesh::conflict::is_conflicting_metadata(metadata)
}

fn is_retired_at(metadata: &serde_json::Value, now: i64) -> bool {
    match metadata.get("valid_to") {
        Some(valid_to) if !valid_to.is_null() => valid_to.as_i64().unwrap_or(0) <= now,
        _ => false,
    }
}

/// Compute memory salience score per spec §5.3 / custom function definition.
pub fn compute_memory_salience(metadata: &serde_json::Value, now: i64) -> f64 {
    if is_archived(metadata) || is_retired_at(metadata, now) || is_mesh_conflicting(metadata) {
        return 0.0;
    }
    let access_count = metadata.get("access_count").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
    let access_term = (1.0 + access_count).ln() * 0.1;
    let feedback = metadata.get("feedback_score").and_then(|v| v.as_f64()).unwrap_or(0.0).clamp(-1.0, 1.0) * 0.05;
    let neural_gain = metadata.get("neural_gain").and_then(|v| v.as_f64()).unwrap_or(1.0).clamp(0.0, 10.0);
    ((1.0 + access_term + feedback) * neural_gain).max(0.0)
}

/// Async projection with metadata mapping (for custom functions).
pub async fn project_sparql_dataset_with_meta_async(
    namespace: &str,
    include_retired: bool,
    include_archived: bool,
    store: &crate::store::ladybug::LadybugStore,
) -> Result<(Dataset, std::collections::HashMap<String, serde_json::Value>), String> {
    let mut dataset = Dataset::new();
    let mut meta_map = std::collections::HashMap::new();

    // Active generations per source for this namespace, used to filter
    // superseded ingest-generation rows and deduplicate by bare_id.
    // `with_conn` requires a 'static closure, so the namespace is copied in.
    let ns_owned = namespace.to_string();
    let (by_source, active_set, _max_code_gen) = store
        .with_conn(move |conn| {
            Ok(active_generations_by_source_of(conn, &ns_owned))
        })
        .await
        .unwrap_or_else(|_err| {
            tracing::warn!("active_generations_by_source_of query failed for '{namespace}'");
            (HashMap::new(), HashSet::new(), None)
        });

    let memories = store
        .list_memories(namespace, None, None)
        .await
        .map_err(|e| format!("Failed to load memories for SPARQL dataset: {e}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // A memory reaches the projection only when it clears every row-level
    // filter: active generation, not retired/archived/mesh-conflicting, and
    // not a registry row.
    let passes = |m: &crate::traits::SearchResult| -> bool {
        crate::store::ladybug::is_active_scoped_row(
            &m.id,
            namespace,
            &m.payload.memory_type,
            &m.payload.metadata,
            &by_source,
            &active_set,
            _max_code_gen,
        ).is_some()
        && (include_retired || !is_retired_at(&m.payload.metadata, now))
        && (include_archived || !is_archived(&m.payload.metadata))
        && !is_mesh_conflicting(&m.payload.metadata)
        && !crate::registry::is_registry_row(&m.payload.metadata)
    };

    // Best active generation per bare_id among passing rows (dedup key: keep
    // the highest generation for each logical id).
    let mut best_by_bare: HashMap<String, u64> = HashMap::new();
    for m in &memories {
        if !passes(m) {
            continue;
        }
        if let Some((_, gen, bare)) = split_scoped_id(&m.id) {
            best_by_bare
                .entry(bare.to_string())
                .and_modify(|g| {
                    if gen > *g {
                        *g = gen;
                    }
                })
                .or_insert(gen);
        }
    }

    // Keep each passing row that is either unscoped or the highest-generation
    // version of its bare_id. Input order is preserved.
    let mut kept_memories: Vec<&crate::traits::SearchResult> = Vec::new();
    for m in &memories {
        if !passes(m) {
            continue;
        }
        match split_scoped_id(&m.id) {
            Some((_, gen, bare)) => {
                if best_by_bare.get(&bare).is_some_and(|&g| gen == g) {
                    kept_memories.push(m);
                }
            }
            None => kept_memories.push(m),
        }
    }

    for m in kept_memories {
        let payload = m.payload.clone();
        meta_map.insert(format!("urn:neurostrata:memory:{}", m.id), m.payload.metadata.clone());
        meta_map.insert(m.id.clone(), m.payload.metadata.clone());

        let qs = crate::rdf::projection::memory_to_quads(
            &m.id,
            &payload,
            namespace,
            crate::rdf::types::Tier::Domain,
            0,
        );
        for q in qs {
            dataset.insert(&q);
            // Default-graph union semantics are handled by
            // `set_default_graph_as_union()` on the prepared query
            // (see execute_sparql_query_with_config); quads live only
            // in their named graph here.
        }
    }

    Ok((dataset, meta_map))
}

/// Build a QueryEvaluator with the 3 NeuroStrata custom functions registered.
pub fn evaluator_with_custom_functions(
    meta_map: std::sync::Arc<std::collections::HashMap<String, serde_json::Value>>,
    now: i64,
) -> QueryEvaluator {
    custom_functions_on(QueryEvaluator::new(), meta_map, now)
}

/// Same as [`evaluator_with_custom_functions`], plus the HTTP default SERVICE
/// handler for federated endpoints.
pub fn evaluator_with_custom_functions_and_service(
    meta_map: std::sync::Arc<std::collections::HashMap<String, serde_json::Value>>,
    now: i64,
    handler: crate::rdf::federation::HttpServiceHandler,
) -> QueryEvaluator {
    custom_functions_on(
        QueryEvaluator::new().with_default_service_handler(handler),
        meta_map,
        now,
    )
}

fn custom_functions_on(
    base: QueryEvaluator,
    meta_map: std::sync::Arc<std::collections::HashMap<String, serde_json::Value>>,
    now: i64,
) -> QueryEvaluator {
    use oxrdf::{NamedNode, Term, Literal};
    base
        .with_custom_function(
            NamedNode::new("urn:neurostrata:ns#salience").unwrap(),
            {
                let meta_map = meta_map.clone();
                move |args: &[Term]| -> Option<Term> {
                    let key = args.first()?;
                    let key_str = match key {
                        Term::NamedNode(n) => n.as_str().to_string(),
                        Term::Literal(l) => l.value().to_string(),
                        _ => return Some(Literal::from(0.0f64).into()),
                    };
                    let score = meta_map.get(&key_str).map(|m| compute_memory_salience(m, now)).unwrap_or(0.0);
                    Some(Literal::from(score).into())
                }
            },
        )
        .with_custom_function(
            NamedNode::new("urn:neurostrata:ns#validAt").unwrap(),
            {
                let meta_map = meta_map.clone();
                move |args: &[Term]| -> Option<Term> {
                    let mem_term = args.get(0)?;
                    let ts_term = args.get(1)?;
                    let key_str = match mem_term {
                        Term::NamedNode(n) => n.as_str().to_string(),
                        Term::Literal(l) => l.value().to_string(),
                        _ => return Some(Literal::from(false).into()),
                    };
                    let ts: i64 = match ts_term {
                        Term::Literal(l) => l.value().parse::<i64>().ok()?,
                        _ => return Some(Literal::from(false).into()),
                    };
                    let meta = meta_map.get(&key_str)?;
                    let valid_from = meta.get("valid_from").and_then(|v| v.as_i64()).unwrap_or(0);
                    let valid_to = meta.get("valid_to").and_then(|v| v.as_i64()).filter(|v| *v != 0);
                    let is_valid = valid_from <= ts && (valid_to.is_none() || ts < valid_to.unwrap());
                    Some(Literal::from(is_valid).into())
                }
            },
        )
        .with_custom_function(
            NamedNode::new("urn:neurostrata:ns#isRetired").unwrap(),
            {
                let meta_map = meta_map.clone();
                move |args: &[Term]| -> Option<Term> {
                    let mem_term = args.first()?;
                    let key_str = match mem_term {
                        Term::NamedNode(n) => n.as_str().to_string(),
                        Term::Literal(l) => l.value().to_string(),
                        _ => return Some(Literal::from(false).into()),
                    };
                    let meta = meta_map.get(&key_str)?;
                    let retired = is_retired_at(meta, now);
                    Some(Literal::from(retired).into())
                }
            },
        )
}

/// The algebra tree of a parsed query (SELECT/ASK/CONSTRUCT/DESCRIBE share the
/// same shape).
pub(crate) fn sparql_pattern(query: &spargebra::Query) -> &spargebra::algebra::GraphPattern {
    match query {
        spargebra::Query::Select { pattern, .. }
        | spargebra::Query::Construct { pattern, .. }
        | spargebra::Query::Describe { pattern, .. }
        | spargebra::Query::Ask { pattern, .. } => pattern,
    }
}

/// Count the nodes of an algebra tree (BGP, joins, filters, SERVICE, ...).
/// The budget cap stops pathologically large queries before evaluation.
pub(crate) fn count_algebra_nodes(pattern: &spargebra::algebra::GraphPattern) -> usize {
    use spargebra::algebra::GraphPattern as GP;
    1 + match pattern {
        GP::Join { left, right } => {
            count_algebra_nodes(left) + count_algebra_nodes(right)
        }
        GP::Union { left, right } => {
            count_algebra_nodes(left) + count_algebra_nodes(right)
        }
        GP::Minus { left, right } => {
            count_algebra_nodes(left) + count_algebra_nodes(right)
        }
        GP::LeftJoin { left, right, .. } => {
            count_algebra_nodes(left) + count_algebra_nodes(right)
        }
        GP::Filter { inner, .. } => count_algebra_nodes(inner),
        GP::Graph { inner, .. } => count_algebra_nodes(inner),
        GP::Extend { inner, .. } => count_algebra_nodes(inner),
        GP::OrderBy { inner, .. } => count_algebra_nodes(inner),
        GP::Project { inner, .. } => count_algebra_nodes(inner),
        GP::Group { inner, .. } => count_algebra_nodes(inner),
        GP::Distinct { inner } => count_algebra_nodes(inner),
        GP::Reduced { inner } => count_algebra_nodes(inner),
        GP::Slice { inner, .. } => count_algebra_nodes(inner),
        GP::Service { inner, .. } => count_algebra_nodes(inner),
        _ => 0,
    }
}

/// Test-only shim: evaluate with the env-resolved config. Production callers
/// must use `execute_sparql_query_with_config` with an explicit SparqlConfig.
#[cfg(test)]
pub async fn execute_sparql_query(
    query_str: &str,
    namespace: &str,
    format: QueryResultsFormat,
    include_retired: bool,
    include_archived: bool,
    store: &crate::store::ladybug::LadybugStore,
) -> Result<(String, usize, bool, bool), String> {
    let config = get_sparql_config();
    execute_sparql_query_with_config(
        store,
        namespace,
        query_str,
        format,
        include_retired,
        include_archived,
        config,
    )
    .await
}

/// Evaluate a SPARQL query over the projected namespace dataset with an
/// explicit `SparqlConfig`, letting callers apply per-request budget
/// overrides without resorting to environment mutation.
pub async fn execute_sparql_query_with_config(
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
    query_str: &str,
    format: QueryResultsFormat,
    include_retired: bool,
    include_archived: bool,
    config: SparqlConfig,
) -> Result<(String, usize, bool, bool), String> {
    // Parse the SPARQL query using spargebra with pre-registered prefixes
    let query = parser_with_prefixes()
        .parse_query(query_str)
        .map_err(|e| format!("SPARQL parse error: {}", e))?;

    // Algebra-size budget: a query whose tree exceeds the node cap is refused
    // before any dataset is built, regardless of what it would evaluate to.
    let algebra_nodes = count_algebra_nodes(sparql_pattern(&query));
    if algebra_nodes > config.max_algebra_nodes {
        return Err(format!(
            "SPARQL query exceeded max_algebra_nodes budget ({} > {})",
            algebra_nodes, config.max_algebra_nodes
        ));
    }

    let service_endpoints = crate::rdf::federation::inspect_service_clauses(&query)
        .map_err(|e| format!("SPARQL federation error: {}", e))?;

    // Reject UPDATE queries: spargebra separates UPDATE via parse_update; parse_query
    // handles SELECT/ASK/CONSTRUCT/DESCRIBE only, so an UPDATE input errors here.

    // Build the dataset from namespace projection (live LadybugDB) with meta mapping
    let (dataset, meta_map) =
        project_sparql_dataset_with_meta_async(namespace, include_retired, include_archived, store)
            .await?;
    let meta_arc = std::sync::Arc::new(meta_map);

    if dataset.len() > config.max_dataset_quads {
        return Err(format!(
            "Dataset projection exceeded max_dataset_quads budget ({} > {})",
            dataset.len(),
            config.max_dataset_quads
        ));
    }

    // Prepare and evaluate with QueryEvaluator (custom functions registered);
    // federated SERVICE endpoints get the HTTP default handler with the
    // federation security model (allowlist + loopback/HTTPS validation).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let evaluator = if !service_endpoints.is_empty() {
        let handler = crate::rdf::federation::HttpServiceHandler {
            config: crate::rdf::federation::FederationConfig {
                timeout_ms: config.timeout_ms,
                max_solutions: config.max_solutions,
                max_dataset_quads: config.max_dataset_quads,
                ..Default::default()
            },
        };
        evaluator_with_custom_functions_and_service(meta_arc.clone(), now, handler)
    } else {
        evaluator_with_custom_functions(meta_arc, now)
    };

    // Timeout budget: the engine's cancellation token preempts a query that
    // runs past `timeout_ms` (between evaluation steps) instead of merely
    // reporting overrun afterwards. The flag records whether the budget was
    // actually consumed, so the caller can distinguish "timed out" from a
    // query that finished within budget.
    let cancel_token = spareval::CancellationToken::new();
    let evaluator = evaluator.with_cancellation_token(cancel_token.clone());
    let timed_out_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let timeout_ms = config.timeout_ms;
    let timer_handle = tokio::spawn({
        let timed_out_flag = timed_out_flag.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
            timed_out_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            cancel_token.cancel();
        }
    });
    let mut prepared = evaluator.prepare(&query);
    // Default-graph union semantics: a query over the default graph sees every
    // projected quad regardless of which named graph it lives in. Quads are
    // inserted once (in their named graph) by project_sparql_dataset_with_meta_async,
    // and the union handles the default graph — no double insert.
    prepared.dataset_mut().set_default_graph_as_union();
    let results = match prepared.execute(&dataset) {
        Ok(r) => r,
        Err(e) => {
            timer_handle.abort();
            return Err(format!("SPARQL evaluation error: {}", e));
        }
    };

    // Serialize results
    let mut output = Vec::new();
    let serializer = QueryResultsSerializer::from_format(format);

    // Extract variables before take() (wrapper loses variables())
    let results_vars: Vec<Variable> = match &results {
        spareval::QueryResults::Solutions(sol_iter) => sol_iter.variables().to_vec(),
        _ => Vec::new(),
    };

    let mut solution_count: usize = 0;
    let mut timed_out = false;
    let mut truncated = false;

    match results {
        spareval::QueryResults::Boolean(b) => {
            serializer
                .serialize_boolean_to_writer(&mut output, b)
                .map_err(|e| format!("serialize boolean: {}", e))?;
        }
        spareval::QueryResults::Solutions(solutions) => {
            // Take one extra solution to detect truncation: a query returning
            // exactly `max_solutions` rows must not be reported as truncated.
            let mut collected: Vec<_> = Vec::new();
            for item in solutions.take(config.max_solutions + 1) {
                match item {
                    Ok(sol) => collected.push(sol),
                    Err(e) => {
                        if timed_out_flag.load(std::sync::atomic::Ordering::SeqCst) {
                            // Cancellation consumed the timeout budget mid-iteration.
                            timed_out = true;
                            break;
                        }
                        return Err(format!("evaluation error: {}", e));
                    }
                }
            }

            truncated = collected.len() > config.max_solutions;
            if truncated {
                collected.truncate(config.max_solutions);
            }

            let mut serializer_solutions =
                serializer
                    .serialize_solutions_to_writer(&mut output, results_vars)
                    .map_err(|e| format!("serialize solutions start: {}", e))?;
            for sol in &collected {
                serializer_solutions
                    .serialize(sol)
                    .map_err(|e| format!("serialize solution: {}", e))?;
            }
            solution_count = collected.len();
            serializer_solutions.finish()
                .map_err(|e| format!("serialize finish: {}", e))?;
        }
        spareval::QueryResults::Graph(triples) => {
            // CONSTRUCT/DESCRIBE: serialize triples as N-Triples.
            for triple in triples {
                if timed_out_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    timed_out = true;
                    break;
                }
                let t = triple.map_err(|e| format!("graph evaluation error: {}", e))?;
                output.extend_from_slice(format!("{} .\n", t).as_bytes());
                solution_count += 1;
            }
        }
    }

    // Evaluation finished: stop the timer so a completed query neither leaks
    // the sleep task nor races with the timed-out flag afterwards.
    timer_handle.abort();

    let results_string = String::from_utf8(output)
        .map_err(|e| format!("UTF-8 error: {}", e))?;
    let timed_out = timed_out || timed_out_flag.load(std::sync::atomic::Ordering::SeqCst);

    Ok((results_string, solution_count, truncated, timed_out))
}

/// Global configuration for SPARQL execution, honoring optional environment
/// overrides (`NEUROSTRATA_SPARQL_TIMEOUT_MS`, `NEUROSTRATA_SPARQL_MAX_SOLUTIONS`,
/// `NEUROSTRATA_SPARQL_MAX_DATASET_QUADS`, `NEUROSTRATA_SPARQL_MAX_ALGEBRA_NODES`).
pub fn get_sparql_config() -> SparqlConfig {
    let mut config = SparqlConfig::default();
    if let Ok(v) = std::env::var("NEUROSTRATA_SPARQL_TIMEOUT_MS") {
        if let Ok(ms) = v.trim().parse::<u64>() {
            config.timeout_ms = ms;
        }
    }
    if let Ok(v) = std::env::var("NEUROSTRATA_SPARQL_MAX_SOLUTIONS") {
        if let Ok(n) = v.trim().parse::<usize>() {
            config.max_solutions = n;
        }
    }
    if let Ok(v) = std::env::var("NEUROSTRATA_SPARQL_MAX_DATASET_QUADS") {
        if let Ok(n) = v.trim().parse::<usize>() {
            config.max_dataset_quads = n;
        }
    }
    if let Ok(v) = std::env::var("NEUROSTRATA_SPARQL_MAX_ALGEBRA_NODES") {
        if let Ok(n) = v.trim().parse::<usize>() {
            config.max_algebra_nodes = n;
        }
    }
    config
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

    #[tokio::test]
    async fn test_execute_sparql_query_live_projection() {
        use crate::traits::{MemoryPayload, VectorStore};
        let tmp = std::env::temp_dir().join(format!("ns-sparql-proj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("test-sparql").await.unwrap();
        let payload = MemoryPayload {
            content: "SPARQL live projection fact".to_string(),
            user_id: "u1".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "src/rdf/sparql.rs".to_string(),
            location_lines: "1".to_string(),
            metadata: serde_json::json!({"domain": "retrieval"}),
        };
        store
            .upsert("test-sparql", "mem-1", vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        let (serialized, count, truncated, timed_out) = execute_sparql_query(
            "SELECT ?s ?p ?o WHERE { ?s ?p ?o }",
            "test-sparql",
            QueryResultsFormat::Json,
            false,
            false,
            &store,
        )
        .await
        .expect("sparql query succeeds");
        assert!(count > 0, "expected projected triples in default union graph");
        assert!(!truncated);
        assert!(!timed_out);
        assert!(serialized.contains("urn:neurostrata:memory:mem-1"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_preregistered_prefixes_parse() {
        let q = parser_with_prefixes().parse_query("SELECT ?s WHERE { ?s a class:Fact ; ns:domain dom:auth }");
        assert!(q.is_ok(), "pre-registered prefixes should parse without explicit PREFIX");
    }

    #[test]
    fn test_custom_functions_salience_valid_at_is_retired() {
        use std::collections::HashMap;
        use oxrdf::Dataset;
        
        

        let mut meta_map = HashMap::new();
        meta_map.insert("urn:neurostrata:memory:m1".into(), serde_json::json!({
            "access_count": 10,
            "feedback_score": 0.8,
            "neural_gain": 1.5,
            "valid_from": 100
        }));
        meta_map.insert("m1".into(), meta_map.get("urn:neurostrata:memory:m1").unwrap().clone());

        meta_map.insert("urn:neurostrata:memory:m2".into(), serde_json::json!({
            "valid_to": 500
        }));
        meta_map.insert("m2".into(), meta_map.get("urn:neurostrata:memory:m2").unwrap().clone());

        let meta_arc = std::sync::Arc::new(meta_map);
        let now = 600i64; // past m2's valid_to 500 so it is retired
        let evaluator = evaluator_with_custom_functions(meta_arc, now);

        let query_str = "SELECT (ns:salience(<urn:neurostrata:memory:m1>) AS ?s1) (ns:salience(<urn:neurostrata:memory:m2>) AS ?s2) (ns:validAt(<urn:neurostrata:memory:m1>, 200) AS ?v1) (ns:isRetired(<urn:neurostrata:memory:m2>) AS ?r2) WHERE {}";
        let query = parser_with_prefixes().parse_query(query_str).expect("parse");
        let dataset = Dataset::new();
        let prepared = evaluator.prepare(&query);
        let results = prepared.execute(&dataset).expect("execute");

        if let spareval::QueryResults::Solutions(mut sol_iter) = results {
            let sol = sol_iter.next().unwrap().expect("first solution");
            let s1 = sol.get("s1").unwrap();
            let s2 = sol.get("s2").unwrap();
            let v1 = sol.get("v1").unwrap();
            let r2 = sol.get("r2").unwrap();

            // Assert s1 > 1.0
            let s1_val = match s1 {
                oxrdf::Term::Literal(l) => l.value().parse::<f64>().unwrap(),
                _ => 0.0,
            };
            assert!(s1_val > 1.0, "salience of active memory should be > 1.0 (got {})", s1_val);

            // Assert s2 == 0.0
            let s2_val = match s2 {
                oxrdf::Term::Literal(l) => l.value().parse::<f64>().unwrap(),
                _ => -1.0,
            };
            assert_eq!(s2_val, 0.0, "retired memory salience should be 0.0");

            // Assert v1 == true
            let v1_str = match v1 {
                oxrdf::Term::Literal(l) => l.value().to_string(),
                _ => "false".into(),
            };
            assert_eq!(v1_str, "true", "validAt at 200 should be true");

            // Assert r2 == true (m2 valid_to=500 <= now=600)
            let r2_str = match r2 {
                oxrdf::Term::Literal(l) => l.value().to_string(),
                _ => "false".into(),
            };
            assert_eq!(r2_str, "true", "isRetired for m2 at now=600 should be true (valid_to 500 <= 600)");
        } else {
            panic!("expected solutions");
        }
    }

    #[tokio::test]
    async fn test_project_sparql_dataset_filters_inactive_generations_and_dedupes() {
        use crate::traits::{MemoryPayload, VectorStore};

        let tmp = std::env::temp_dir().join(format!("ns-sparql-dedup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let store = crate::store::ladybug::LadybugStore::for_testing(&tmp, 4).expect("store");
        store.init("probe").await.unwrap();

        let zero = vec![0.0f32; 4];
        let scoped = |memory_type: &str| MemoryPayload {
            content: format!("content {memory_type}"),
            user_id: "u1".to_string(),
            memory_type: memory_type.to_string(),
            agent_name: None,
            location: "src/a.rs".to_string(),
            location_lines: "1".to_string(),
            metadata: serde_json::json!({"source_key": "source-a"}),
        };

        // Generation 1 for source-a: superseded by generation 2.
        assert_eq!(store.begin_generation("probe", "source-a").await.unwrap(), 1);
        let gen1_id = crate::parser::ingest::generation_scoped_id("probe", 1, "src/a.rs");
        store
            .upsert("probe", &gen1_id, zero.clone(), scoped("code_ast"))
            .await
            .unwrap();
        store.publish_generation("probe", "source-a", 1).await.unwrap();

        // Generation 2 for source-a: same bare_id, now active.
        assert_eq!(store.begin_generation("probe", "source-a").await.unwrap(), 2);
        let gen2_id = crate::parser::ingest::generation_scoped_id("probe", 2, "src/a.rs");
        store
            .upsert("probe", &gen2_id, zero.clone(), scoped("code_ast"))
            .await
            .unwrap();
        store.publish_generation("probe", "source-a", 2).await.unwrap();

        // Unscoped normative row: reachable regardless of generations.
        store
            .upsert(
                "probe",
                "normative-rule",
                vec![0.5, 0.0, 0.0, 0.0],
                MemoryPayload {
                    content: "a rule".to_string(),
                    user_id: "u1".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "src/rules.rs".to_string(),
                    location_lines: "1".to_string(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .unwrap();

        let (dataset, _meta_map) = project_sparql_dataset_with_meta_async(
            "probe", false, false, &store,
        )
        .await
        .expect("projection succeeds");

        let mut gen1_quads = 0usize;
        let mut gen2_quads = 0usize;
        let mut rule_quads = 0usize;
        let mut default_graph_quads = 0usize;
        for q in &dataset {
            let subj = match &q.subject {
                oxrdf::NamedOrBlankNodeRef::NamedNode(n) => Some(n.as_str().to_string()),
                _ => None,
            };
            let Some(subj) = subj else { continue };
            if subj == format!("urn:neurostrata:memory:{gen1_id}") {
                gen1_quads += 1;
            }
            if subj == format!("urn:neurostrata:memory:{gen2_id}") {
                gen2_quads += 1;
            }
            if subj == "urn:neurostrata:memory:normative-rule" {
                rule_quads += 1;
            }
            if q.graph_name.is_default_graph() {
                default_graph_quads += 1;
            }
        }

        // Superseded generation is filtered out; active generation projects.
        assert_eq!(gen1_quads, 0, "superseded generation {gen1_id} must not project");
        assert!(gen2_quads > 0, "active generation {gen2_id} must project");
        assert!(rule_quads > 0, "unscoped normative rule must project");

        // Dedup by bare_id: exactly one generation of src/a.rs is present.
        // No double insert: nothing lands in the default graph (union applied
        // at query time) and each memory projects its quads exactly once.
        assert_eq!(default_graph_quads, 0, "no quad inserted into the default graph");
        assert_eq!(
            dataset.len(),
            gen2_quads + rule_quads,
            "each memory projects once; no duplication"
        );
    }
}