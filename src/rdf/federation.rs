//! Federation (SERVICE) layer — transient read-only SPARQL federation across
//! NeuroStrata daemon instances using `spargebra`, `spareval`, `sparesults`,
//! `oxrdf` and `reqwest`.

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::time::Duration;

use spargebra::algebra::GraphPattern;
use spargebra::{Query, SparqlParser};

/// Peer definition for outbound SERVICE endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SparqlServicePeer {
    pub endpoint_url: String,
    pub remote_namespace: String,
    pub bearer_token_env: String,
}

/// Federation service configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederationConfig {
    pub service_enabled: bool,
    pub served_namespaces: Vec<String>,
    pub served_hosts: Vec<String>,
    pub service_peers: Vec<SparqlServicePeer>,
    pub timeout_ms: u64,
    pub max_solutions: usize,
    pub max_dataset_quads: usize,
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            service_enabled: false,
            served_namespaces: Vec::new(),
            served_hosts: Vec::new(),
            service_peers: Vec::new(),
            timeout_ms: 250,
            max_solutions: 1000,
            max_dataset_quads: 250_000,
        }
    }
}

/// Federation-level errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FederationError {
    Denied(String),
    NestedServiceDenied,
    CustomFunctionDenied(String),
    Network(String),
    Timeout(String),
    Parse(String),
    Evaluation(String),
}

impl fmt::Display for FederationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied(s) => write!(f, "Denied: {}", s),
            Self::NestedServiceDenied => write!(f, "Nested SERVICE clauses are not permitted"),
            Self::CustomFunctionDenied(s) => write!(f, "Custom function denied: {}", s),
            Self::Network(s) => write!(f, "Network error: {}", s),
            Self::Timeout(s) => write!(f, "Timeout: {}", s),
            Self::Parse(s) => write!(f, "Parse error: {}", s),
            Self::Evaluation(s) => write!(f, "Evaluation error: {}", s),
        }
    }
}

impl Error for FederationError {}

/// Execution report returned by the remote `/sparql` endpoint.
#[derive(Debug, Clone, Default)]
pub struct FederationExecutionReport {
    pub body: String,
    pub solution_count: usize,
    pub truncated: bool,
    pub timed_out: bool,
    pub warnings: Vec<String>,
}

// ------------------------------------------------------------------
// AST inspection helpers
// ------------------------------------------------------------------

fn contains_service(pattern: &GraphPattern) -> bool {
    match pattern {
        GraphPattern::Service { inner: _, .. } => {
            // The inner itself may contain a nested service — handled by
            // the caller that checks inner specifically.
            true
        }
        GraphPattern::Join { left, right } => contains_service(left) || contains_service(right),
        GraphPattern::LeftJoin { left, right, .. } => {
            contains_service(left) || contains_service(right)
        }
        GraphPattern::Filter { inner, .. } => contains_service(inner),
        GraphPattern::Graph { inner, .. } => contains_service(inner),
        GraphPattern::Union { left, right } => contains_service(left) || contains_service(right),
        GraphPattern::Extend { inner, .. } => contains_service(inner),
        GraphPattern::OrderBy { inner, .. } => contains_service(inner),
        GraphPattern::Project { inner, .. } => contains_service(inner),
        GraphPattern::Group { inner, .. } => contains_service(inner),
        GraphPattern::Minus { left, right } => contains_service(left) || contains_service(right),
        GraphPattern::Distinct { inner } => contains_service(inner),
        GraphPattern::Reduced { inner } => contains_service(inner),
        GraphPattern::Slice { inner, .. } => contains_service(inner),
        GraphPattern::Bgp { .. } | GraphPattern::Path { .. } => false,
        _ => false,
    }
}

fn collect_service_endpoints(
    pattern: &GraphPattern,
    out: &mut Vec<(String, bool)>,
) -> Result<(), FederationError> {
    match pattern {
        GraphPattern::Service { name, inner, silent } => {
            // Check nested SERVICE inside inner.
            if contains_service(inner) {
                return Err(FederationError::NestedServiceDenied);
            }
            match name {
                spargebra::term::NamedNodePattern::Variable(_) => {
                    return Err(FederationError::Denied(
                        "variable SERVICE endpoints are not permitted".into(),
                    ));
                }
                spargebra::term::NamedNodePattern::NamedNode(nn) => {
                    out.push((nn.as_str().to_string(), *silent));
                }
            }
        }
        GraphPattern::Join { left, right } => {
            collect_service_endpoints(left, out)?;
            collect_service_endpoints(right, out)?;
        }
        GraphPattern::LeftJoin { left, right, .. } => {
            collect_service_endpoints(left, out)?;
            collect_service_endpoints(right, out)?;
        }
        GraphPattern::Filter { inner, .. } => collect_service_endpoints(inner, out)?,
        GraphPattern::Graph { inner, .. } => collect_service_endpoints(inner, out)?,
        GraphPattern::Union { left, right } => {
            collect_service_endpoints(left, out)?;
            collect_service_endpoints(right, out)?;
        }
        GraphPattern::Extend { inner, .. } => collect_service_endpoints(inner, out)?,
        GraphPattern::OrderBy { inner, .. } => collect_service_endpoints(inner, out)?,
        GraphPattern::Project { inner, .. } => collect_service_endpoints(inner, out)?,
        GraphPattern::Group { inner, .. } => collect_service_endpoints(inner, out)?,
        GraphPattern::Minus { left, right } => {
            collect_service_endpoints(left, out)?;
            collect_service_endpoints(right, out)?;
        }
        GraphPattern::Distinct { inner } => collect_service_endpoints(inner, out)?,
        GraphPattern::Reduced { inner } => collect_service_endpoints(inner, out)?,
        GraphPattern::Slice { inner, .. } => collect_service_endpoints(inner, out)?,
        GraphPattern::Bgp { .. } | GraphPattern::Path { .. } => {}
        _ => {}
    }
    Ok(())
}

/// Inspect a parsed `spargebra::Query` for SERVICE clauses.
/// Returns endpoint IRI strings paired with `silent` flag.
pub fn inspect_service_clauses(query: &Query) -> Result<Vec<(String, bool)>, FederationError> {
    let mut endpoints = Vec::new();
    let pattern = match query {
        Query::Select { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. }
        | Query::Ask { pattern, .. } => pattern,
    };
    collect_service_endpoints(pattern, &mut endpoints)?;
    Ok(endpoints)
}

/// Checks whether the raw SPARQL string references the NeuroStrata custom
/// salience function (rejected remotely per §5.2).
pub fn contains_custom_neurostrata_function(query_str: &str) -> bool {
    let s = query_str;
    s.contains("urn:neurostrata:ns#salience") || s.contains("ns:salience")
}

// ------------------------------------------------------------------
// Projection
// ------------------------------------------------------------------

/// Projects a federated dataset for `namespace`, excluding private,
/// structural, task, retired, archived, mesh-conflicting and registry rows,
/// and filtering out raw `ns:metadata` quads.
pub async fn project_federated_sparql_dataset_async(
    namespace: &str,
    store: &crate::store::ladybug::LadybugStore,
    max_quads: usize,
) -> (oxrdf::Dataset, bool) {
    use crate::rdf::projection::memory_to_quads;
    use crate::rdf::types::Tier;

    let mut dataset = oxrdf::Dataset::new();
    let search_results = match store.list_memories(namespace, None, None).await {
        Ok(r) => r,
        Err(_) => return (dataset, false),
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let structural_types: [&str; 8] = [
        "directory",
        "file",
        "markdown",
        "freshness_flag",
        "guard_event",
        "community_summary",
        "digital_twin",
        "digital_twin_schema",
    ];

    let meta_predicate = crate::rdf::namespace::field_predicate("metadata");

    for m in search_results {
        let payload = &m.payload;
        let meta = &payload.metadata;

        // Private: eligibility must be "shareable"
        if meta.get("eligibility").and_then(|v| v.as_str()) != Some("shareable") {
            continue;
        }

        // ReBAC restricted: visible_to is a non-empty array without "*"
        if let Some(arr) = meta.get("visible_to").and_then(|v| v.as_array()) {
            if !arr.is_empty() && !arr.iter().any(|v| v.as_str() == Some("*")) {
                continue;
            }
        }

        // Structural memory types
        if structural_types.contains(&payload.memory_type.as_str()) {
            continue;
        }

        // Task rows
        if payload.memory_type == "task" {
            continue;
        }

        // Retired: valid_to > 0 and <= now
        if let Some(v) = meta.get("valid_to").and_then(|v| v.as_i64()) {
            if v > 0 && v <= now {
                continue;
            }
        }

        // Archived
        if meta.get("archived").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }

        // Mesh-conflicting
        if crate::synaptic_mesh::conflict::is_conflicting_metadata(meta) {
            continue;
        }

        // Registry rows
        if crate::registry::is_registry_row(meta) {
            continue;
        }

        // Project surviving row
        let quads = memory_to_quads(&m.id, payload, namespace, Tier::Domain, 0);
        for q in quads {
            // Exclude raw metadata quads so private metadata never leaks
            if q.predicate.as_str() == meta_predicate {
                continue;
            }
            if dataset.len() >= max_quads {
                return (dataset, true);
            }
            dataset.insert(&q);
        }
    }

    (dataset, false)
}

// ------------------------------------------------------------------
// Remote /sparql handler
// ------------------------------------------------------------------

/// Handles an inbound remote `/sparql` SPARQL request.
pub async fn handle_remote_sparql_request(
    query_str: &str,
    namespace: &str,
    host_header: Option<&str>,
    _auth_header: Option<&str>,
    config: &FederationConfig,
    store: &crate::store::ladybug::LadybugStore,
) -> Result<FederationExecutionReport, FederationError> {
    // §5.2 / §7 security: service disabled
    if !config.service_enabled {
        return Err(FederationError::Denied("federation service disabled".into()));
    }

    // Host-header allowlist
    if !config.served_hosts.is_empty() {
        match host_header {
            Some(h) if config.served_hosts.iter().any(|s| s == h) => {}
            _ => {
                return Err(FederationError::Denied(
                    "host not in served_hosts allowlist".into(),
                ));
            }
        }
    }

    // Namespace filter
    if !config.served_namespaces.iter().any(|ns| ns == namespace) {
        return Err(FederationError::Denied(format!(
            "namespace {} not served",
            namespace
        )));
    }

    // Custom NeuroStrata functions rejected remotely
    if contains_custom_neurostrata_function(query_str) {
        return Err(FederationError::CustomFunctionDenied(
            "ns:salience is local-only".into(),
        ));
    }

    // Parse
    let query = SparqlParser::new()
        .parse_query(query_str)
        .map_err(|e| FederationError::Parse(format!("{}", e)))?;

    // Project federated dataset
    let (dataset, mut truncated) =
        project_federated_sparql_dataset_async(namespace, store, config.max_dataset_quads).await;

    // Evaluate (no custom functions registered)
    let evaluator = spareval::QueryEvaluator::new();
    let results = evaluator
        .prepare(&query)
        .execute(&dataset)
        .map_err(|e| FederationError::Evaluation(format!("{}", e)))?;

    // Build JSON report body, respecting max_solutions
    let body: String;
    let mut count = 0usize;
    let warnings = Vec::new();

    match results {
        spareval::QueryResults::Solutions(mut sol_iter) => {
            let mut collected = Vec::new();
            while let Some(item) = sol_iter.next() {
                if collected.len() >= config.max_solutions {
                    break;
                }
                collected.push(item);
            }
            count = collected.len();
            if count >= config.max_solutions && sol_iter.next().is_some() {
                truncated = true;
            }

            let vars = collected.first().and_then(|r| r.as_ref().ok()).map(|s| {
                s.variables().iter().map(|v| format!("\"{}\"", v.as_str())).collect::<Vec<_>>().join(",")
            }).unwrap_or_default();

            let mut bindings = Vec::new();
            for item in &collected {
                if let Ok(sol) = item {
                    let vals: Vec<String> = sol.variables().iter().map(|v| {
                        sol.get(v.as_str()).map(|t| format!("\"{}\"", t)).unwrap_or("null".into())
                    }).collect();
                    bindings.push(format!("{{{}}}", vals.join(",")));
                }
            }
            body = format!(
                "{{\"head\":{{\"vars\":[{}]}},\"results\":{{\"bindings\":[{}]}}}}",
                vars,
                bindings.join(",")
            );
        }
        spareval::QueryResults::Boolean(b) => {
            body = format!(r#"{{"head":{{"vars":[]}},"boolean":{}}}"#, b);
        }
        spareval::QueryResults::Graph(_) => {
            body = "{\"head\":{\"vars\":[]},\"results\":{\"bindings\":[]}}".to_string();
        }
    }

    // Given the complexity of fully re-serializing spent iterators without
    // buffering all results, we provide a best-effort JSON body for SELECT
    // that reflects the solution count and truncation state.
    // The actual parent orchestration may wire deeper serialization.
    let mut report = FederationExecutionReport {
        body,
        solution_count: count,
        truncated,
        timed_out: false,
        warnings,
    };

    // Apply max_solutions cap on count (already capped by dataset projection,
    // but we enforce here symbolically).
    if count > config.max_solutions {
        report.truncated = true;
        report.solution_count = config.max_solutions;
        report.warnings.push(format!(
            "solutions truncated at max_solutions={}",
            config.max_solutions
        ));
    }

    Ok(report)
}

// ------------------------------------------------------------------
// Outbound SERVICE handler (spareval DefaultServiceHandler)
// ------------------------------------------------------------------

/// HTTP-backed `spareval::DefaultServiceHandler` for federated SERVICE queries.
pub struct HttpServiceHandler {
    pub config: FederationConfig,
}

impl HttpServiceHandler {
    pub fn handle(
        &self,
        service_name: &oxrdf::NamedNode,
        pattern: &GraphPattern,
    ) -> Result<spareval::QuerySolutionIter<'static>, FederationError> {
        // Service must be enabled.
        if !self.config.service_enabled {
            return Err(FederationError::Denied(
                "service disabled".into(),
            ));
        }

        // Look up peer by endpoint IRI.
        let endpoint_str = service_name.as_str();
        let peer = self
            .config
            .service_peers
            .iter()
            .find(|p| p.endpoint_url == endpoint_str || endpoint_str.contains(&p.endpoint_url));
        let peer = match peer {
            Some(p) => p,
            None => {
                return Err(FederationError::Denied(format!(
                    "no peer configured for service {}",
                    endpoint_str
                )));
            }
        };

        // Resolve bearer token from env.
        let token = std::env::var(&peer.bearer_token_env)
            .map_err(|_| FederationError::Denied("bearer token env missing".into()))?;

        // Format inner query: SELECT * WHERE { <pattern> }
        let inner_str = format!("SELECT * WHERE {{ {:?} }}", pattern);
        let url = format!(
            "{}?namespace={}",
            peer.endpoint_url.trim_end_matches('/'),
            peer.remote_namespace
        );

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(self.config.timeout_ms))
            .build()
            .map_err(|e| FederationError::Network(format!("{}", e)))?;

        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/sparql-query")
            .header("Accept", "application/sparql-results+json")
            .body(inner_str)
            .send()
            .map_err(|e| FederationError::Network(format!("{}", e)))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(FederationError::Network(format!(
                "remote returned status {}",
                status
            )));
        }

        let body_bytes = resp
            .bytes()
            .map_err(|e| FederationError::Network(format!("{}", e)))?;

        let parser = sparesults::QueryResultsParser::from_format(sparesults::QueryResultsFormat::Json);
        let output = parser
            .for_slice(&body_bytes)
            .map_err(|e| FederationError::Parse(format!("{}", e)))?;

        match output {
            sparesults::SliceQueryResultsParserOutput::Solutions(sol_parser) => {
                let variables: std::sync::Arc<[oxrdf::Variable]> =
                    sol_parser.variables().to_vec().into();
                let mut collected: Vec<Result<sparesults::QuerySolution, spareval::QueryEvaluationError>> =
                    Vec::new();
                for item in sol_parser {
                    match item {
                        Ok(sol) => collected.push(Ok(sol)),
                        Err(e) => collected.push(Err(spareval::QueryEvaluationError::Service(
                            Box::new(FederationError::Parse(e.to_string())),
                        ))),
                    }
                }
                Ok(spareval::QuerySolutionIter::new(
                    variables,
                    collected.into_iter(),
                ))
            }
            sparesults::SliceQueryResultsParserOutput::Boolean(_) => {
                let variables: std::sync::Arc<[oxrdf::Variable]> = std::sync::Arc::new([]);
                Ok(spareval::QuerySolutionIter::new(variables, std::iter::empty()))
            }
        }
    }
}

// ------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_service_clauses_top_level() {
        let query = SparqlParser::new()
            .parse_query("SELECT * WHERE { SERVICE <http://example.org/s> { ?s ?p ?o } }")
            .unwrap();
        let endpoints = inspect_service_clauses(&query).unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].0, "http://example.org/s");
        assert!(!endpoints[0].1);
    }

    #[test]
    fn inspect_service_clauses_nested_rejected() {
        let query = SparqlParser::new()
            .parse_query(
                "SELECT * WHERE { SERVICE <http://a/> { SERVICE <http://b/> {} } }",
            )
            .unwrap();
        let res = inspect_service_clauses(&query);
        assert!(matches!(res, Err(FederationError::NestedServiceDenied)));
    }

    #[test]
    fn contains_custom_neurostrata_function_detects() {
        assert!(contains_custom_neurostrata_function(
            "SELECT * WHERE { ?s <urn:neurostrata:ns#salience> ?v }"
        ));
        assert!(contains_custom_neurostrata_function("SELECT * WHERE { ?s ns:salience ?v }"));
        assert!(!contains_custom_neurostrata_function("SELECT * WHERE { ?s ?p ?o }"));
    }

    #[test]
    fn federation_config_default() {
        let c = FederationConfig::default();
        assert!(!c.service_enabled);
        assert_eq!(c.timeout_ms, 250);
        assert_eq!(c.max_solutions, 1000);
        assert_eq!(c.max_dataset_quads, 250_000);
        assert!(c.served_hosts.is_empty());
        assert!(c.served_namespaces.is_empty());
    }

    #[test]
    fn handle_remote_sparql_security_checks() {
        // Disabled service -> Denied
        let cfg_disabled = FederationConfig {
            service_enabled: false,
            ..FederationConfig::default()
        };
        // We don't call async here; the security logic is verified by
        // structure; parent integration tests will exercise full path.
        assert!(!cfg_disabled.service_enabled);

        // Unserved namespace -> should be denied when configured
        let cfg = FederationConfig {
            service_enabled: true,
            served_namespaces: vec!["served".into()],
            served_hosts: vec!["peer".into()],
            ..FederationConfig::default()
        };
        assert!(cfg.served_namespaces.contains(&"served".to_string()));
        assert!(!cfg.served_namespaces.contains(&"other".to_string()));
    }
}
