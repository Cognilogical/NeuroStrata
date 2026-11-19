//! Offline SPARQL competency evaluation gate.
//!
//! Executes versioned ASK/SELECT competency questions over a bounded,
//! default-graph-only N-Quads fixture, optionally materializing a certified
//! OWL 2 RL closure, and emits a deterministic strict JSON receipt.
//!
//! Semantics are deliberately scoped: the evaluator reuses the production
//! non-federated evaluator (`evaluator_with_custom_functions`) with an EMPTY
//! metadata map and a fixed timestamp, so the registered custom functions are
//! inert and no live namespace state or HTTP SERVICE handler can leak in.
//! `ReasoningProfile::Rdfs` is metadata-only in the production materializer
//! (the closure is the full OWL 2 RL closure in both profiles); receipts record
//! this explicitly via `rule_selection`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use oxrdf::{Dataset, NamedNode, Term, Triple};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::rdf::reasoning::{ReasoningAdapter, ReasoningProfile};

/// Fixed timestamp for the inert custom functions; no wall-clock input.
const FIXED_NOW: i64 = 0;
/// Namespace label recorded on the materialization certificate.
const CERT_NAMESPACE: &str = "urn:neurostrata:eval:competency";
/// Fixture N-Quads must be default-graph-only; the runner stores them in this
/// named graph so `set_default_graph_as_union()` exposes them to queries.
const EVAL_GRAPH_IRI: &str = "urn:neurostrata:eval:graph:competency";

// ---------------------------------------------------------------------------
// Strict fixture DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompetencyFixture {
    pub schema_version: u32,
    pub fixture_id: String,
    /// Canonical production strings: "none", "rdfs", "owl-rl".
    pub reasoning_profile: String,
    pub dataset_nquads: String,
    pub queries: Vec<CompetencyQuery>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompetencyQuery {
    pub id: String,
    pub rationale: String,
    pub curator: String,
    pub sparql: String,
    pub expected: ExpectedSparqlResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ExpectedSparqlResult {
    Ask { value: bool },
    Select { rows: Vec<BTreeMap<String, ExpectedTerm>> },
}

/// Canonical RDF-term record. Also used as the normalized actual-term form so
/// expected and observed bindings compare by term identity (type, lexical
/// value, datatype, language), never by display string alone.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExpectedTerm {
    NamedNode { value: String },
    BlankNode { value: String },
    Literal {
        value: String,
        datatype: String,
        #[serde(default)]
        language: Option<String>,
    },
    Triple { value: String },
}

// ---------------------------------------------------------------------------
// Strict receipt DTOs (no timestamps, latencies, or wall-clock fields)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompetencyReceipt {
    pub schema_version: u32,
    pub fixture_id: String,
    pub fixture_sha256: String,
    pub reasoning_profile: String,
    pub materialization: Option<MaterializationEvidence>,
    pub results: Vec<CompetencyQueryResult>,
    pub verdict: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationEvidence {
    pub requested_profile: String,
    pub applied_profile: String,
    pub base_triple_count: usize,
    pub inferred_triple_count: usize,
    pub certificate_sha256: String,
    /// The production materializer runs the full OWL 2 RL closure for both
    /// `rdfs` and `owl-rl`; the profile is recorded metadata, not a rule filter.
    pub rule_selection: String,
    /// Named graph isolating certified entailments from explicit assertions.
    pub inference_graph: String,
    /// RDF-star provenance emitted only from verified derivation steps.
    pub rdf_star_annotations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompetencyQueryResult {
    pub query_id: String,
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

// ---------------------------------------------------------------------------
// Bounded execution budgets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Limits {
    max_algebra_nodes: usize,
    max_dataset_quads: usize,
    max_solutions: usize,
    timeout_ms: u64,
    max_inferred_triples: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_algebra_nodes: 100,
            max_dataset_quads: 50_000,
            max_solutions: 1_000,
            timeout_ms: 250,
            max_inferred_triples: 50_000,
        }
    }
}

enum ActualOutcome {
    Boolean(bool),
    Rows(Vec<BTreeMap<String, ExpectedTerm>>),
}

// ---------------------------------------------------------------------------
// Validation and canonicalization helpers
// ---------------------------------------------------------------------------

fn parse_profile(value: &str) -> Result<Option<ReasoningProfile>> {
    match value {
        "none" => Ok(None),
        "rdfs" => Ok(Some(ReasoningProfile::Rdfs)),
        "owl-rl" => Ok(Some(ReasoningProfile::OwlRl)),
        _ => bail!(
            "unsupported reasoning_profile '{value}'; expected 'none', 'rdfs', or 'owl-rl'"
        ),
    }
}

fn validate_fixture(f: &CompetencyFixture) -> Result<()> {
    if f.schema_version != 1 {
        bail!("unsupported schema_version {} (expected 1)", f.schema_version);
    }
    if f.fixture_id.trim().is_empty() {
        bail!("fixture_id must not be empty");
    }
    parse_profile(&f.reasoning_profile)?;
    if f.queries.is_empty() {
        bail!("competency fixture must declare at least one query");
    }
    let mut ids = HashSet::new();
    for q in &f.queries {
        if q.id.trim().is_empty() {
            bail!("query id must not be empty");
        }
        if !ids.insert(q.id.as_str()) {
            bail!("duplicate query id '{}'", q.id);
        }
        if q.rationale.trim().is_empty() {
            bail!("query '{}' has empty rationale", q.id);
        }
        if q.curator.trim().is_empty() {
            bail!("query '{}' has empty curator", q.id);
        }
        if q.sparql.trim().is_empty() {
            bail!("query '{}' has empty sparql", q.id);
        }
    }
    Ok(())
}

fn eval_graph_name() -> oxrdf::GraphName {
    oxrdf::GraphName::NamedNode(NamedNode::new_unchecked(EVAL_GRAPH_IRI))
}

fn canonical_term(term: &Term) -> ExpectedTerm {
    match term {
        Term::NamedNode(n) => ExpectedTerm::NamedNode {
            value: n.as_str().to_string(),
        },
        Term::BlankNode(b) => ExpectedTerm::BlankNode {
            value: b.as_str().to_string(),
        },
        Term::Literal(l) => ExpectedTerm::Literal {
            value: l.value().to_string(),
            datatype: l.datatype().as_str().to_string(),
            language: l.language().map(str::to_string),
        },
        // RDF 1.2 triple term: keep a canonical rendering; bundled fixtures do
        // not use it, but the comparison must never collapse it to a string of
        // an unrelated term kind.
        _ => ExpectedTerm::Triple {
            value: term.to_string(),
        },
    }
}

fn sorted_row_key(row: &BTreeMap<String, ExpectedTerm>) -> String {
    serde_json::to_string(row).unwrap_or_default()
}

fn build_dataset(nquads: &str, limits: &Limits) -> Result<(Dataset, Vec<Triple>)> {
    let mut dataset = Dataset::new();
    let mut base = Vec::new();
    for parsed in oxttl::NQuadsParser::new().for_slice(nquads.as_bytes()) {
        let quad = parsed.map_err(|e| anyhow::anyhow!("N-Quads parse error: {e}"))?;
        if dataset.len() >= limits.max_dataset_quads {
            bail!(
                "dataset exceeded max_dataset_quads budget ({})",
                limits.max_dataset_quads
            );
        }
        if !quad.graph_name.is_default_graph() {
            bail!("named-graph quads are not permitted in competency fixtures");
        }
        base.push(Triple::new(
            quad.subject.clone(),
            quad.predicate.clone(),
            quad.object.clone(),
        ));
        dataset.insert(&oxrdf::Quad::new(
            quad.subject.clone(),
            quad.predicate.clone(),
            quad.object.clone(),
            eval_graph_name(),
        ));
    }
    Ok((dataset, base))
}

fn materialize(
    profile: ReasoningProfile,
    base: &[Triple],
    dataset: &mut Dataset,
    limits: &Limits,
) -> Result<MaterializationEvidence> {
    let adapter = ReasoningAdapter::new();
    let certified = adapter
        .materialize_owl2_rl_with_proofs(
            base,
            CERT_NAMESPACE,
            limits.max_inferred_triples,
            profile,
        )
        .map_err(|e| anyhow::anyhow!("certified materialization failed: {e}"))?;
    if certified.inferred_triples.len() > limits.max_dataset_quads {
        bail!(
            "materialized dataset exceeded max_dataset_quads budget ({})",
            limits.max_dataset_quads
        );
    }
    // Consume the certified N-Quads verbatim: each entailment is placed in
    // its isolating named graph, never merged into the explicit-assertion
    // graph. Anything not carrying the expected graph is rejected fail-closed.
    let inf_graph = oxrdf::GraphName::NamedNode(NamedNode::new_unchecked(&certified.inference_graph));
    for nq in &certified.inferred_nquads {
        let quad = oxttl::NQuadsParser::new()
            .for_slice(nq.as_bytes())
            .next()
            .ok_or_else(|| anyhow::anyhow!("empty inferred N-Quad in certificate"))?
            .map_err(|e| anyhow::anyhow!("inferred N-Quad parse error: {e}"))?;
        if quad.graph_name != inf_graph {
            bail!("certified inference escaped its isolating named graph: {nq}");
        }
        if dataset.len() >= limits.max_dataset_quads {
            bail!(
                "materialized dataset exceeded max_dataset_quads budget ({})",
                limits.max_dataset_quads
            );
        }
        dataset.insert(&quad);
    }
    let applied_profile = profile.as_str().to_string();
    Ok(MaterializationEvidence {
        requested_profile: applied_profile.clone(),
        applied_profile,
        base_triple_count: certified.certificate.base_triple_count,
        inferred_triple_count: certified.certificate.inferred_triple_count,
        certificate_sha256: certified.certificate.certificate_sha256,
        rule_selection: "full_owl2_rl".to_string(),
        inference_graph: certified.inference_graph,
        rdf_star_annotations: certified.rdf_star_annotations,
    })
}

// ---------------------------------------------------------------------------
// Query execution
// ---------------------------------------------------------------------------

async fn evaluate_inner(
    query: &CompetencyQuery,
    dataset: &Arc<Dataset>,
    limits: &Limits,
) -> Result<()> {
    if limits.timeout_ms == 0 {
        bail!("timeout budget exhausted (timeout_ms = 0)");
    }

    // Parse with the production prefix table. UPDATE strings are parsed only to
    // give a precise rejection diagnostic; they are never evaluated.
    let parsed = match crate::rdf::sparql::parser_with_prefixes().parse_query(&query.sparql) {
        Ok(q) => q,
        Err(e) => {
            if crate::rdf::sparql::parser_with_prefixes()
                .parse_update(&query.sparql)
                .is_ok()
            {
                bail!("SPARQL update (INSERT/DELETE) is not permitted: {e}");
            }
            bail!("SPARQL parse error: {e}");
        }
    };

    match &parsed {
        spargebra::Query::Ask { .. } | spargebra::Query::Select { .. } => {}
        spargebra::Query::Construct { .. } | spargebra::Query::Describe { .. } => {
            bail!("query form CONSTRUCT/DESCRIBE is not permitted")
        }
    }

    let service_endpoints = crate::rdf::federation::inspect_service_clauses(&parsed)
        .map_err(|e| anyhow::anyhow!("SERVICE inspection failed: {e}"))?;
    if !service_endpoints.is_empty() {
        bail!(
            "SERVICE clause rejected (endpoint '{}'); no service handler is constructed",
            service_endpoints[0].0
        );
    }

    let algebra_nodes = crate::rdf::sparql::count_algebra_nodes(
        crate::rdf::sparql::sparql_pattern(&parsed),
    );
    if algebra_nodes > limits.max_algebra_nodes {
        bail!(
            "query exceeded max_algebra_nodes budget ({algebra_nodes} > {})",
            limits.max_algebra_nodes
        );
    }
    if dataset.len() > limits.max_dataset_quads {
        bail!(
            "dataset exceeded max_dataset_quads budget ({} > {})",
            dataset.len(),
            limits.max_dataset_quads
        );
    }

    let cancel = spareval::CancellationToken::new();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timeout_ms = limits.timeout_ms;
    let timer = tokio::spawn({
        let flag = timed_out.clone();
        let token = cancel.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
            flag.store(true, Ordering::SeqCst);
            token.cancel();
        }
    });

    let ds = dataset.clone();
    let cancel_blocking = cancel.clone();
    let max_solutions = limits.max_solutions;
    let eval_join = tokio::task::spawn_blocking(move || -> Result<ActualOutcome> {
        // Production non-federated evaluator with an empty metadata map and a
        // fixed timestamp: custom functions are registered but inert, and no
        // HTTP SERVICE handler is ever created.
        let evaluator = crate::rdf::sparql::evaluator_with_custom_functions(
            Arc::new(HashMap::<String, serde_json::Value>::new()),
            FIXED_NOW,
        )
        .with_cancellation_token(cancel_blocking);
        let mut prepared = evaluator.prepare(&parsed);
        prepared.dataset_mut().set_default_graph_as_union();
        let results = prepared
            .execute(&*ds)
            .map_err(|e| anyhow::anyhow!("SPARQL evaluation error: {e}"))?;
        match results {
            spareval::QueryResults::Boolean(b) => Ok(ActualOutcome::Boolean(b)),
            spareval::QueryResults::Solutions(solutions) => {
                let mut rows = Vec::new();
                for item in solutions.take(max_solutions + 1) {
                    let solution =
                        item.map_err(|e| anyhow::anyhow!("solution iteration error: {e}"))?;
                    let mut row = BTreeMap::new();
                    for (var, term) in solution.iter() {
                        row.insert(var.as_str().to_string(), canonical_term(term));
                    }
                    rows.push(row);
                }
                if rows.len() > max_solutions {
                    bail!("result truncated at max_solutions = {max_solutions}");
                }
                Ok(ActualOutcome::Rows(rows))
            }
            spareval::QueryResults::Graph(_) => {
                bail!("query returned a graph result set")
            }
        }
    });

    let joined = eval_join
        .await
        .map_err(|e| anyhow::anyhow!("evaluation task panicked: {e}"))?;
    timer.abort();
    let actual = joined?;
    if timed_out.load(Ordering::SeqCst) {
        bail!("query timed out ({timeout_ms} ms budget)");
    }

    match (&query.expected, actual) {
        (ExpectedSparqlResult::Ask { value }, ActualOutcome::Boolean(actual)) => {
            if *value != actual {
                bail!("expected ASK {value}, got {actual}");
            }
        }
        (ExpectedSparqlResult::Ask { .. }, ActualOutcome::Rows(_)) => {
            bail!("wrong result form: expected ASK but query returned solutions")
        }
        (ExpectedSparqlResult::Select { rows }, ActualOutcome::Rows(mut actual_rows)) => {
            let mut expected_rows = rows.clone();
            expected_rows.sort_by_key(sorted_row_key);
            actual_rows.sort_by_key(sorted_row_key);
            if expected_rows != actual_rows {
                bail!(
                    "binding row count / multiset mismatch: expected {} row(s), got {}; expected {:?}, actual {:?}",
                    expected_rows.len(),
                    actual_rows.len(),
                    expected_rows.first(),
                    actual_rows.first()
                );
            }
        }
        (ExpectedSparqlResult::Select { .. }, ActualOutcome::Boolean(_)) => {
            bail!("wrong result form: expected SELECT but query returned a boolean")
        }
    }
    Ok(())
}

async fn evaluate_query(
    query: &CompetencyQuery,
    dataset: &Arc<Dataset>,
    limits: &Limits,
) -> CompetencyQueryResult {
    match evaluate_inner(query, dataset, limits).await {
        Ok(()) => CompetencyQueryResult {
            query_id: query.id.clone(),
            verdict: "PASS".to_string(),
            diagnostic: None,
        },
        Err(e) => CompetencyQueryResult {
            query_id: query.id.clone(),
            verdict: "FAIL".to_string(),
            diagnostic: Some(format!("query '{}': {e}", query.id)),
        },
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Execute a parsed fixture using the production budgets. The fixture digest
/// is taken over its canonical serialization.
pub async fn run_fixture(fixture: &CompetencyFixture) -> Result<CompetencyReceipt> {
    let canonical = serde_json::to_vec(fixture)?;
    run_with_limits(fixture, &Limits::default(), &canonical).await
}

/// Strictly load a fixture from disk, retaining the exact source bytes for the
/// receipt digest.
pub fn load_fixture(path: &std::path::Path) -> Result<(CompetencyFixture, Vec<u8>)> {
    let bytes = std::fs::read(path)?;
    let fixture = serde_json::from_slice::<CompetencyFixture>(&bytes)
        .map_err(|e| anyhow::anyhow!("competency fixture parse error: {e}"))?;
    Ok((fixture, bytes))
}

/// Execute the bundled/on-disk fixture with the production budgets.
pub async fn run_fixture_file(path: &std::path::Path) -> Result<CompetencyReceipt> {
    let (fixture, bytes) = load_fixture(path)?;
    run_with_limits(&fixture, &Limits::default(), &bytes).await
}

async fn run_with_limits(
    fixture: &CompetencyFixture,
    limits: &Limits,
    digest_source: &[u8],
) -> Result<CompetencyReceipt> {
    validate_fixture(fixture)?;
    let profile = parse_profile(&fixture.reasoning_profile)?;
    let (mut dataset, base_triples) = build_dataset(&fixture.dataset_nquads, limits)?;
    let materialization = match profile {
        Some(p) => Some(materialize(p, &base_triples, &mut dataset, limits)?),
        None => None,
    };
    let dataset = Arc::new(dataset);
    let mut results = Vec::with_capacity(fixture.queries.len());
    for query in &fixture.queries {
        results.push(evaluate_query(query, &dataset, limits).await);
    }
    let verdict = if results.iter().all(|r| r.verdict == "PASS") {
        "PASS"
    } else {
        "FAIL"
    };
    Ok(CompetencyReceipt {
        schema_version: 1,
        fixture_id: fixture.fixture_id.clone(),
        fixture_sha256: hex::encode(Sha256::digest(digest_source)),
        reasoning_profile: fixture.reasoning_profile.clone(),
        materialization,
        results,
        verdict: verdict.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
    const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";

    const BASE_NQ: &str = concat!(
        "<urn:neurostrata:eval:class:Memory> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:neurostrata:eval:class:CognitiveArtifact> .\n",
        "<urn:neurostrata:eval:mem:alpha> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:neurostrata:eval:class:Memory> .\n",
        "<urn:neurostrata:eval:mem:beta> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:neurostrata:eval:class:Memory> .\n",
        "<urn:neurostrata:eval:prop:parent> <http://www.w3.org/2002/07/owl#inverseOf> <urn:neurostrata:eval:prop:child> .\n",
        "<urn:neurostrata:eval:node:alice> <urn:neurostrata:eval:prop:parent> <urn:neurostrata:eval:node:bob> .\n",
        "<urn:neurostrata:eval:prop:partOf> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#TransitiveProperty> .\n",
        "<urn:neurostrata:eval:node:core> <urn:neurostrata:eval:prop:partOf> <urn:neurostrata:eval:node:engine> .\n",
        "<urn:neurostrata:eval:node:engine> <urn:neurostrata:eval:prop:partOf> <urn:neurostrata:eval:node:system> .\n",
    );

    fn named(v: &str) -> ExpectedTerm {
        ExpectedTerm::NamedNode { value: v.to_string() }
    }
    fn plain(v: &str) -> ExpectedTerm {
        ExpectedTerm::Literal {
            value: v.to_string(),
            datatype: XSD_STRING.to_string(),
            language: None,
        }
    }
    fn ask(id: &str, sparql: &str, value: bool) -> CompetencyQuery {
        CompetencyQuery {
            id: id.to_string(),
            rationale: "inline unit-test question".to_string(),
            curator: "bosun".to_string(),
            sparql: sparql.to_string(),
            expected: ExpectedSparqlResult::Ask { value },
        }
    }
    fn select(id: &str, sparql: &str, rows: Vec<BTreeMap<String, ExpectedTerm>>) -> CompetencyQuery {
        CompetencyQuery {
            id: id.to_string(),
            rationale: "inline unit-test question".to_string(),
            curator: "bosun".to_string(),
            sparql: sparql.to_string(),
            expected: ExpectedSparqlResult::Select { rows },
        }
    }
    fn row(pairs: Vec<(&str, ExpectedTerm)>) -> BTreeMap<String, ExpectedTerm> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }
    fn fixture(profile: &str, nq: &str, queries: Vec<CompetencyQuery>) -> CompetencyFixture {
        CompetencyFixture {
            schema_version: 1,
            fixture_id: "inline-test".to_string(),
            reasoning_profile: profile.to_string(),
            dataset_nquads: nq.to_string(),
            queries,
        }
    }
    async fn run(f: &CompetencyFixture) -> CompetencyReceipt {
        run_fixture(f).await.expect("fixture must execute")
    }
    fn qr<'a>(r: &'a CompetencyReceipt, id: &str) -> &'a CompetencyQueryResult {
        r.results
            .iter()
            .find(|q| q.query_id == id)
            .unwrap_or_else(|| panic!("no result for query {id}"))
    }
    fn assert_pass(r: &CompetencyReceipt, id: &str) {
        assert_eq!(qr(r, id).verdict, "PASS", "{}: {:?}", id, qr(r, id).diagnostic);
    }
    fn assert_fail(r: &CompetencyReceipt, id: &str, needle: &str) {
        let q = qr(r, id);
        assert_eq!(q.verdict, "FAIL", "{id} should fail: {:?}", q.diagnostic);
        let d = q.diagnostic.as_deref().unwrap_or("");
        assert!(d.contains(needle), "diagnostic {d:?} must contain {needle:?}");
    }

    #[tokio::test]
    async fn ask_true_and_false_compare_expected_value() {
        let f = fixture("none", "<urn:neurostrata:eval:a> <urn:neurostrata:eval:p> <urn:neurostrata:eval:b> .\n", vec![
            ask("t", "ASK { <urn:neurostrata:eval:a> <urn:neurostrata:eval:p> <urn:neurostrata:eval:b> }", true),
            ask("f", "ASK { <urn:neurostrata:eval:b> <urn:neurostrata:eval:p> <urn:neurostrata:eval:a> }", false),
        ]);
        let r = run(&f).await;
        assert_eq!(r.verdict, "PASS");
        assert_pass(&r, "t");
        assert_pass(&r, "f");

        let mut wrong = f.clone();
        wrong.queries[0].expected = ExpectedSparqlResult::Ask { value: false };
        let r = run(&wrong).await;
        assert_eq!(r.verdict, "FAIL");
        assert_fail(&r, "t", "expected ASK false");
    }

    #[tokio::test]
    async fn select_compares_sorted_multiset_with_duplicates_and_term_identity() {
        let nq = concat!(
            "<urn:neurostrata:eval:a> <urn:neurostrata:eval:label> \"same\" .\n",
            "<urn:neurostrata:eval:b> <urn:neurostrata:eval:label> \"same\"^^<http://www.w3.org/2001/XMLSchema#string> .\n",
            "<urn:neurostrata:eval:c> <urn:neurostrata:eval:label> \"same\"@en .\n",
        );
        let rows = vec![
            row(vec![("s", named("urn:neurostrata:eval:a")), ("l", plain("same"))]),
            row(vec![("s", named("urn:neurostrata:eval:b")), ("l", plain("same"))]),
            row(vec![("s", named("urn:neurostrata:eval:c")), ("l", ExpectedTerm::Literal {
                value: "same".into(), datatype: RDF_LANG_STRING.into(), language: Some("en".into())
            })]),
        ];
        let q = select("labels", "SELECT ?s ?l WHERE { ?s <urn:neurostrata:eval:label> ?l }", rows);
        let f = fixture("none", nq, vec![q]);
        let r = run(&f).await;
        assert_pass(&r, "labels");

        let mut wrong = f.clone();
        if let ExpectedSparqlResult::Select { rows } = &mut wrong.queries[0].expected {
            if let Some(t) = rows[2].get_mut("l") {
                *t = ExpectedTerm::Literal {
                    value: "same".into(), datatype: RDF_LANG_STRING.into(), language: Some("fr".into())
                };
            }
        }
        assert_fail(&run(&wrong).await, "labels", "binding");
    }

    #[tokio::test]
    async fn select_preserves_duplicate_solutions() {
        let nq = concat!(
            "<urn:neurostrata:eval:a> <urn:neurostrata:eval:p> \"dup\" .\n",
            "<urn:neurostrata:eval:b> <urn:neurostrata:eval:p> \"dup\" .\n",
        );
        let one = row(vec![("v", plain("dup"))]);
        let q = select("dups", "SELECT ?v WHERE { ?s <urn:neurostrata:eval:p> ?v }", vec![one.clone()]);
        let r = run(&fixture("none", nq, vec![q])).await;
        assert_fail(&r, "dups", "row count");

        let q = select("dups2", "SELECT ?v WHERE { ?s <urn:neurostrata:eval:p> ?v }", vec![one.clone(), one]);
        assert_pass(&run(&fixture("none", nq, vec![q])).await, "dups2");
    }

    #[tokio::test]
    async fn rdfs_subclass_and_owl_inverse_transitive_use_certified_materialization() {
        let qs = vec![
            ask("sub", "ASK { <urn:neurostrata:eval:mem:alpha> rdf:type <urn:neurostrata:eval:class:CognitiveArtifact> }", true),
            ask("inv", "ASK { <urn:neurostrata:eval:node:bob> <urn:neurostrata:eval:prop:child> <urn:neurostrata:eval:node:alice> }", true),
            ask("trp", "ASK { <urn:neurostrata:eval:node:core> <urn:neurostrata:eval:prop:partOf> <urn:neurostrata:eval:node:system> }", true),
        ];
        let r = run(&fixture("owl-rl", BASE_NQ, qs)).await;
        assert_eq!(r.verdict, "PASS");
        assert_pass(&r, "sub");
        assert_pass(&r, "inv");
        assert_pass(&r, "trp");
        let m = r.materialization.expect("materialization evidence present");
        assert_eq!(m.applied_profile, "owl-rl");
        assert_eq!(m.rule_selection, "full_owl2_rl");
        assert!(m.inferred_triple_count > 0);
        assert_eq!(m.certificate_sha256.len(), 64);
    }

    #[tokio::test]
    async fn uncertified_materialization_fails_whole_fixture() {
        let f = fixture("owl-rl", BASE_NQ, vec![
            ask("sub", "ASK { <urn:neurostrata:eval:mem:alpha> rdf:type <urn:neurostrata:eval:class:CognitiveArtifact> }", true),
        ]);
        let limits = Limits { max_inferred_triples: 0, ..Limits::default() };
        let err = run_with_limits(&f, &limits, b"inline").await.unwrap_err();
        assert!(format!("{err}").contains("materialization"), "{err}");
    }

    // ---- I3: certified named-graph isolation + RDF-star provenance ----
    #[test]
    fn i3_materialization_isolates_inferences_and_persists_provenance() {
        use crate::rdf::reasoning::inference_graph_iri;

        let limits = Limits::default();
        let (mut dataset, base) = build_dataset(BASE_NQ, &limits).expect("dataset builds");
        let ev = materialize(ReasoningProfile::OwlRl, &base, &mut dataset, &limits)
            .expect("certified materialization succeeds");

        // Receipt documents the isolating named graph.
        assert_eq!(ev.inference_graph, inference_graph_iri(CERT_NAMESPACE));
        let inf_graph = oxrdf::GraphName::NamedNode(NamedNode::new_unchecked(&ev.inference_graph));

        // Every emitted N-Quad lands in the inference graph, and none of the
        // explicit eval-graph assertions were replaced by bare inferred triples.
        let certified = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(
                &base,
                CERT_NAMESPACE,
                limits.max_inferred_triples,
                ReasoningProfile::OwlRl,
            )
            .expect("certificate");
        let inf_count = dataset.quads_for_graph_name(&inf_graph).count();
        assert!(inf_count > 0, "expected inferred quads in inference graph");
        for nq in &certified.inferred_nquads {
            let q = oxttl::NQuadsParser::new()
                .for_slice(nq.as_bytes())
                .next()
                .expect("parse inferred nquad")
                .expect("valid inferred nquad");
            assert_eq!(q.graph_name, inf_graph, "inferred quad leaked graph");
            assert!(dataset.contains(&q), "inferred quad missing from dataset");
        }

        // RDF-star provenance is persisted in the receipt, not discarded.
        assert!(!ev.rdf_star_annotations.is_empty());
        assert!(ev.rdf_star_annotations.iter().any(|a| a.contains("derivedByRule")));
        assert!(ev.rdf_star_annotations.iter().any(|a| a.contains("certificateHash")));
    }

    #[test]
    fn i3_failed_certificate_yields_no_inferences_or_provenance() {
        use crate::rdf::reasoning::inference_graph_iri;

        let mut limits = Limits::default();
        limits.max_inferred_triples = 0;
        let (mut dataset, base) = build_dataset(BASE_NQ, &limits).expect("dataset builds");
        assert!(materialize(ReasoningProfile::OwlRl, &base, &mut dataset, &limits).is_err());
        let inf_graph = oxrdf::GraphName::NamedNode(NamedNode::new_unchecked(&inference_graph_iri(CERT_NAMESPACE)));
        assert_eq!(dataset.quads_for_graph_name(&inf_graph).count(), 0);
    }

    #[tokio::test]
    async fn forbidden_query_forms_rejected_before_evaluation() {
        let cases = vec![
            ("svc", "ASK { SERVICE <http://example.org/sparql> { ?s ?p ?o } }", "SERVICE"),
            ("construct", "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }", "query form"),
            ("describe", "DESCRIBE <urn:neurostrata:eval:a>", "query form"),
            ("delete", "DELETE WHERE { ?s ?p ?o }", "INSERT/DELETE"),
            ("insert", "INSERT DATA { <urn:neurostrata:eval:x> <urn:neurostrata:eval:p> <urn:neurostrata:eval:y> }", "INSERT/DELETE"),
        ];
        let mut qs = Vec::new();
        for (id, sparql, _) in &cases {
            qs.push(ask(id, sparql, true));
        }
        let r = run(&fixture("none", BASE_NQ, qs)).await;
        for (id, _, needle) in &cases {
            assert_fail(&r, id, needle);
        }
        assert_eq!(r.verdict, "FAIL");
    }

    #[tokio::test]
    async fn budgets_fail_closed() {
        let f = fixture("owl-rl", BASE_NQ, vec![
            ask("sub", "ASK { <urn:neurostrata:eval:mem:alpha> rdf:type <urn:neurostrata:eval:class:CognitiveArtifact> }", true),
            ask("union", "ASK { { <urn:neurostrata:eval:a> <urn:neurostrata:eval:p> <urn:neurostrata:eval:b> } UNION { <urn:neurostrata:eval:c> <urn:neurostrata:eval:p> <urn:neurostrata:eval:d> } }", true),
        ]);
        let ds = Limits { max_dataset_quads: 1, ..Limits::default() };
        assert!(format!("{}", run_with_limits(&f, &ds, b"x").await.unwrap_err()).contains("dataset"));

        let alg = Limits { max_algebra_nodes: 1, ..Limits::default() };
        assert_fail(&run_with_limits(&f, &alg, b"x").await.expect("receipt"), "union", "algebra");

        let timeout = Limits { timeout_ms: 0, ..Limits::default() };
        assert_fail(&run_with_limits(&f, &timeout, b"x").await.expect("receipt"), "sub", "timeout");

        let sel = fixture("none", BASE_NQ, vec![select(
            "many",
            "SELECT ?s WHERE { ?s <urn:neurostrata:eval:prop:partOf> ?o }",
            vec![
                row(vec![("s", named("urn:neurostrata:eval:node:core"))]),
                row(vec![("s", named("urn:neurostrata:eval:node:engine"))]),
            ],
        )]);
        let sol = Limits { max_solutions: 1, ..Limits::default() };
        assert_fail(&run_with_limits(&sel, &sol, b"x").await.expect("receipt"), "many", "truncated");
    }

    #[tokio::test]
    async fn malformed_inputs_report_query_specific_diagnostics() {
        let bad_sparql = fixture("none", BASE_NQ, vec![ask("bad", "ASK { this is not sparql", true)]);
        assert_fail(&run(&bad_sparql).await, "bad", "parse");

        let bad_rdf = fixture("none", "<<< not nquads >>>", vec![ask("x", "ASK { ?s ?p ?o }", true)]);
        let err = run_fixture(&bad_rdf).await.unwrap_err();
        assert!(format!("{err}").contains("N-Quads"), "{err}");

        let dup = fixture("none", BASE_NQ, vec![
            ask("same", "ASK { ?s ?p ?o }", true),
            ask("same", "ASK { ?s ?p ?o }", true),
        ]);
        let err = run_fixture(&dup).await.unwrap_err();
        assert!(format!("{err}").contains("duplicate query id"), "{err}");

        let extra = r#"{"schema_version":1,"fixture_id":"x","reasoning_profile":"none","dataset_nquads":"","queries":[],"surprise":1}"#;
        assert!(serde_json::from_str::<CompetencyFixture>(extra).is_err());
    }

    #[tokio::test]
    async fn wrong_result_form_and_expected_bindings_fail() {
        let f = fixture("none", BASE_NQ, vec![CompetencyQuery {
            id: "form".into(),
            rationale: "t".into(),
            curator: "bosun".into(),
            sparql: "ASK { ?s ?p ?o }".into(),
            expected: ExpectedSparqlResult::Select { rows: vec![] },
        }]);
        assert_fail(&run(&f).await, "form", "result form");

        let wrong_rows = fixture("none", BASE_NQ, vec![select(
            "rows",
            "SELECT ?s WHERE { ?s <urn:neurostrata:eval:prop:partOf> ?o }",
            vec![row(vec![("s", named("urn:neurostrata:eval:node:missing"))])],
        )]);
        assert_fail(&run(&wrong_rows).await, "rows", "binding");
    }

    #[tokio::test]
    async fn mutation_removing_required_triple_fails_receipt() {
        let q = vec![ask(
            "needed",
            "ASK { <urn:neurostrata:eval:a> <urn:neurostrata:eval:p> <urn:neurostrata:eval:b> }",
            true,
        )];
        let nq = "<urn:neurostrata:eval:a> <urn:neurostrata:eval:p> <urn:neurostrata:eval:b> .\n";
        let with = fixture("none", nq, q.clone());
        assert_eq!(run(&with).await.verdict, "PASS");
        let without = fixture("none", "", q);
        assert_eq!(run(&without).await.verdict, "FAIL");
    }

    #[tokio::test]
    async fn receipt_is_deterministic_and_has_no_timings() {
        let f = bundled_fixture();
        let a = serde_json::to_vec(&run(&f).await).unwrap();
        let b = serde_json::to_vec(&run(&f).await).unwrap();
        assert_eq!(a, b);
        let text = String::from_utf8(a).unwrap();
        assert!(!text.contains("timestamp"));
        assert!(!text.contains("latency"));
        assert!(!text.contains("elapsed"));
    }

    #[tokio::test]
    async fn bundled_fixture_passes_eight_source_grounded_questions() {
        let f = bundled_fixture();
        assert_eq!(f.schema_version, 1);
        assert_eq!(f.queries.len(), 8);
        assert!(f.queries.iter().all(|q| q.curator == "bosun"));
        assert!(f.queries.iter().all(|q| !q.rationale.is_empty()));
        let r = run(&f).await;
        assert_eq!(r.verdict, "PASS", "{:?}", r.results);
        assert_eq!(r.results.len(), 8);
        assert!(r.results.iter().all(|q| q.verdict == "PASS"));
    }

    #[tokio::test]
    async fn canonical_profile_strings_only() {
        let q = vec![ask("x", "ASK { ?s ?p ?o }", true)];
        for profile in ["none", "rdfs", "owl-rl"] {
            let r = run(&fixture(profile, BASE_NQ, q.clone())).await;
            assert_eq!(r.verdict, "PASS", "profile {profile}");
            if profile == "none" {
                assert!(r.materialization.is_none());
            } else {
                let m = r.materialization.unwrap();
                assert_eq!(m.applied_profile, profile);
            }
        }
        let err = run_fixture(&fixture("owl_rl", BASE_NQ, q)).await.unwrap_err();
        assert!(format!("{err}").contains("reasoning_profile"), "{err}");
    }

    #[tokio::test]
    async fn rdfs_profile_is_metadata_only() {
        let r = run(&fixture("rdfs", BASE_NQ, vec![ask(
            "sub",
            "ASK { <urn:neurostrata:eval:mem:alpha> rdf:type <urn:neurostrata:eval:class:CognitiveArtifact> }",
            true,
        )])).await;
        let m = r.materialization.expect("rdfs still materializes via full closure");
        assert_eq!(m.requested_profile, "rdfs");
        assert_eq!(m.applied_profile, "rdfs");
        assert_eq!(m.rule_selection, "full_owl2_rl");
        assert!(m.inferred_triple_count > 0);
    }

    #[tokio::test]
    async fn custom_functions_are_registered_but_inert() {
        let f = fixture("none", BASE_NQ, vec![
            ask("salience", "ASK { FILTER(ns:salience(\"urn:neurostrata:eval:a\") = 0.0) }", true),
        ]);
        assert_pass(&run(&f).await, "salience");
    }

    fn bundled_fixture() -> CompetencyFixture {
        serde_json::from_str(include_str!("../../eval/semantic/competency.json"))
            .expect("bundled fixture must be strict-valid")
    }
}
