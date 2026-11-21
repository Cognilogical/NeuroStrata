//! Four-arm retrieval ablation over real temporary LadybugDB stores.
//!
//! All four arms run the original query text through the production
//! `search_hybrid_with_domain` path with `SearchMode::Default`. Only the exact
//! flag and the installed domain registry differ between arms. Rankings come
//! from real retrieval; timing is measured separately and never fabricates a
//! rank, a metric, or a gate decision.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::bench::BenchEmbedder;
use crate::semantic_eval::metrics::{
    arm_p95_from_query_medians, evaluate_thresholds, ndcg_at, per_query_p95_medians, precision_at,
    recall_at, relative_gain, summarize_arm, validate_arm_metrics, ArmMetrics, GateReport,
    QueryCategory, QueryMetrics, RelevanceLabel, RetrievalArm,
};
use crate::store::LadybugStore;
use crate::traits::{Embedder, MemoryPayload, SearchMode, VectorStore};

const NAMESPACE: &str = "neurostrata-eval-retrieval";
const TOP_K: usize = 10;
const WARMUP_ITERATIONS: usize = 1;
pub const EVIDENCE_CLASS: &str = "synthetic_deterministic";

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Every member of the production structural-memory-type sets (the retrieval
/// store/lexical ten, plus the task-context structural anchors) and `task`.
/// Fixtures must reject all of them, not just a literal `structural` string.
pub const PRODUCTION_STRUCTURAL_MEMORY_TYPES: &[&str] = &[
    "directory",
    "file",
    "markdown",
    "freshness_flag",
    "guard_event",
    "community_summary",
    "digital_twin",
    "digital_twin_schema",
    "action_type",
    "action_receipt",
    "code_ast",
    "doc_section",
    "task",
];

/// Explicit non-claims. These labels must never appear as pass/fail
/// assertions; they are emitted only as exclusions.
pub const NOT_EVALUATED: &[&str] = &[
    "owner_labeled_production_roi",
    "blind_agent_answer_accuracy",
    "owner_labeled_corpus",
    "rollback_validation",
    "curation_cost",
    "real_namespace_accuracy",
];

// ---------------------------------------------------------------------------
// Strict fixture DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalFixture {
    pub schema_version: u32,
    pub fixture_id: String,
    pub evidence_class: String,
    pub dimensions: usize,
    pub curation_notes: String,
    pub documents: Vec<RetrievalDocument>,
    pub queries: Vec<RetrievalQuery>,
    pub flat_aliases: std::collections::BTreeMap<String, String>,
    pub domains: Vec<RetrievalDomain>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalDocument {
    pub id: String,
    pub content: String,
    pub memory_type: String,
    pub location: String,
    pub domain: String,
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalQuery {
    pub id: String,
    pub text: String,
    pub domain: String,
    pub category: QueryCategory,
    pub ontology_targeted: bool,
    pub relevant: Vec<RelevanceLabel>,
    pub forbidden_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalDomain {
    pub slug: String,
    pub parent: Option<String>,
    pub aliases: Vec<String>,
    pub concept_iri: Option<String>,
}

// ---------------------------------------------------------------------------
// Strict receipt DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AblationReceipt {
    pub schema_version: u32,
    pub fixture_id: String,
    pub fixture_sha256: String,
    pub evidence_class: String,
    pub production_roi_claimed: bool,
    pub dimensions: usize,
    pub iterations: MeasuredIterations,
    pub arms: Vec<ArmEvidence>,
    pub derived_storage: DerivedStorageEvidence,
    pub p95_overhead_ms: f64,
    pub deterministic: bool,
    pub determinism_sha256: String,
    pub gate: GateReport,
    pub verdict: String,
    pub not_evaluated: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasuredIterations {
    pub warmup: usize,
    pub measured: usize,
    pub query_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmEvidence {
    pub arm: RetrievalArm,
    pub registry_variant: String,
    pub exact: bool,
    pub search_mode: String,
    pub deterministic: bool,
    pub query_texts: Vec<String>,
    pub rankings: Vec<QueryRanking>,
    pub metrics: ArmMetrics,
    pub latency: LatencyEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRanking {
    pub query_id: String,
    pub ranked_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyEvidence {
    pub measured_iterations: usize,
    pub warmup_iterations: usize,
    pub per_query_medians_ms: Vec<f64>,
    pub arm_p95_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedStorageEvidence {
    pub numerator_bytes: u64,
    pub denominator_bytes: u64,
    pub ratio: f64,
}

// ---------------------------------------------------------------------------
// Fixture loading and validation
// ---------------------------------------------------------------------------

/// Strictly load a fixture, retaining the exact source bytes for digests.
pub fn load_fixture(path: &Path) -> Result<(RetrievalFixture, Vec<u8>)> {
    let bytes = std::fs::read(path)?;
    let fixture: RetrievalFixture = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("retrieval fixture parse error: {e}"))?;
    Ok((fixture, bytes))
}

/// Full bundled-corpus validation, including the fixed query count and the
/// approved 20/15/15/10 category distribution.
pub fn validate_fixture(fixture: &RetrievalFixture) -> Result<()> {
    validate_fixture_integrity(fixture)?;
    if fixture.queries.len() != 60 {
        bail!(
            "retrieval corpus must contain exactly 60 queries, got {}",
            fixture.queries.len()
        );
    }
    let expected = [
        (QueryCategory::Alias, 20usize),
        (QueryCategory::ExactIdentifier, 15),
        (QueryCategory::Hierarchy, 15),
        (QueryCategory::Adversarial, 10),
    ];
    for (category, count) in expected {
        let actual = fixture
            .queries
            .iter()
            .filter(|q| q.category == category)
            .count();
        if actual != count {
            bail!("category {category:?} must have {count} queries, got {actual}");
        }
    }
    Ok(())
}

/// Structural integrity required for any execution. Tiny synthetic fixtures
/// used by implementation tests pass this but not the release-corpus counts.
pub fn validate_fixture_integrity(fixture: &RetrievalFixture) -> Result<()> {
    if fixture.schema_version != 1 {
        bail!("unsupported schema_version {}", fixture.schema_version);
    }
    if fixture.fixture_id.trim().is_empty() {
        bail!("fixture_id must not be empty");
    }
    if fixture.evidence_class != EVIDENCE_CLASS {
        bail!(
            "evidence_class must be {EVIDENCE_CLASS}, got {}",
            fixture.evidence_class
        );
    }
    if fixture.dimensions == 0 {
        bail!("dimensions must be positive");
    }
    if fixture.curation_notes.trim().is_empty() {
        bail!("curation_notes must be present and non-empty");
    }
    if fixture.documents.is_empty() || fixture.queries.is_empty() {
        bail!("documents and queries must be non-empty");
    }
    if fixture.domains.is_empty() || fixture.flat_aliases.is_empty() {
        bail!("domains and flat_aliases must be non-empty");
    }

    let mut domain_slugs = BTreeSet::new();
    for slug in &fixture.domains {
        let Some(normalized) = crate::domain::normalize_domain_slug(&slug.slug) else {
            bail!("invalid domain slug: {}", slug.slug);
        };
        if !domain_slugs.insert(normalized) {
            bail!("duplicate domain slug: {}", slug.slug);
        }
    }
    // Production v2 validation: DAG, alias uniqueness, slug grammar.
    let raw_entries: Vec<Value> = fixture.domains.iter().map(domain_entry_value).collect();
    crate::registry::validate_domain_entries(&raw_entries)
        .map_err(|e| anyhow::anyhow!("invalid domain registry: {e}"))?;
    for domain in &fixture.domains {
        if let Some(parent) = &domain.parent {
            if !domain_slugs.contains(parent) {
                bail!("domain {} has unknown parent {}", domain.slug, parent);
            }
        }
    }
    for (alias, canonical) in &fixture.flat_aliases {
        if alias.trim().is_empty() || canonical.trim().is_empty() {
            bail!("flat_aliases must map non-empty strings");
        }
    }

    let mut ids = BTreeSet::new();
    for doc in &fixture.documents {
        if doc.id.trim().is_empty()
            || doc.content.trim().is_empty()
            || doc.location.trim().is_empty()
        {
            bail!("document id/content/location must be non-empty");
        }
        if !ids.insert(doc.id.clone()) {
            bail!("duplicate document id {}", doc.id);
        }
        if PRODUCTION_STRUCTURAL_MEMORY_TYPES.contains(&doc.memory_type.as_str()) {
            bail!(
                "memory_type {} is structural or task and cannot be a retrieval document",
                doc.memory_type
            );
        }
        let Some(doc_domain) = crate::domain::normalize_domain_slug(&doc.domain) else {
            bail!("document {} has invalid domain {}", doc.id, doc.domain);
        };
        if !domain_slugs.contains(&doc_domain) {
            bail!(
                "document {} references unknown domain {}",
                doc.id,
                doc.domain
            );
        }
    }

    let mut query_ids = BTreeSet::new();
    let mut negative_controls = 0usize;
    for query in &fixture.queries {
        if query.id.trim().is_empty() || query.text.trim().is_empty() {
            bail!("query id/text must be non-empty");
        }
        if !query_ids.insert(query.id.clone()) {
            bail!("duplicate query id {}", query.id);
        }
        let Some(query_domain) = crate::domain::normalize_domain_slug(&query.domain) else {
            bail!("query {} has invalid domain {}", query.id, query.domain);
        };
        if !domain_slugs.contains(&query_domain) {
            bail!(
                "query {} references unknown domain {}",
                query.id,
                query.domain
            );
        }
        if query.relevant.is_empty() {
            bail!("query {} has no relevant documents", query.id);
        }
        let mut relevant_ids = BTreeSet::new();
        for label in &query.relevant {
            if !(1..=3).contains(&label.gain) {
                bail!(
                    "query {} relevance gain {} outside 1..=3",
                    query.id,
                    label.gain
                );
            }
            if !relevant_ids.insert(label.id.clone()) {
                bail!("query {} has duplicate relevant id {}", query.id, label.id);
            }
            if !ids.contains(&label.id) {
                bail!(
                    "query {} references unknown relevant id {}",
                    query.id,
                    label.id
                );
            }
        }
        if query.forbidden_ids.is_empty() {
            bail!("query {} requires explicit forbidden ids", query.id);
        }
        let mut forbidden = BTreeSet::new();
        for id in &query.forbidden_ids {
            if !forbidden.insert(id.clone()) {
                bail!("query {} has duplicate forbidden id {}", query.id, id);
            }
            if !ids.contains(id) {
                bail!("query {} references unknown forbidden id {}", query.id, id);
            }
            if relevant_ids.contains(id) {
                bail!(
                    "query {} has forbidden id overlapping relevant {}",
                    query.id,
                    id
                );
            }
        }
        if !query.ontology_targeted || query.category == QueryCategory::Adversarial {
            negative_controls += 1;
        }
    }
    if negative_controls == 0 {
        bail!("corpus must include negative controls where flat aliases would suffice");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Arm registry construction and derived storage
// ---------------------------------------------------------------------------

fn domain_entry_value(domain: &RetrievalDomain) -> Value {
    json!({
        "slug": domain.slug,
        "parent": domain.parent,
        "aliases": domain.aliases,
        "concept_iri": domain.concept_iri,
    })
}

/// Compact canonical JSON of domain entries sorted by slug with aliases
/// sorted. `parents = true` yields the arm-4 index/registry serialization;
/// `false` yields the alias-only arm-3 variant.
pub fn canonical_domain_bytes(fixture: &RetrievalFixture, parents: bool) -> Result<Vec<u8>> {
    let mut entries: Vec<Value> = fixture
        .domains
        .iter()
        .map(|d| {
            let mut aliases = d.aliases.clone();
            aliases.sort_unstable();
            json!({
                "slug": d.slug,
                "parent": if parents { d.parent.clone() } else { None },
                "aliases": aliases,
                "concept_iri": d.concept_iri,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        a["slug"]
            .as_str()
            .unwrap_or("")
            .cmp(b["slug"].as_str().unwrap_or(""))
    });
    Ok(serde_json::to_vec(&entries)?)
}

/// Arm 3: same aliases/concept IRIs as arm 4, all parent links removed.
pub fn flat_alias_entries(fixture: &RetrievalFixture) -> Vec<RetrievalDomain> {
    fixture
        .domains
        .iter()
        .map(|d| RetrievalDomain {
            slug: d.slug.clone(),
            parent: None,
            aliases: d.aliases.clone(),
            concept_iri: d.concept_iri.clone(),
        })
        .collect()
}

/// Arm 4: aliases plus the fixture's validated hierarchy.
pub fn ontology_entries(fixture: &RetrievalFixture) -> Vec<RetrievalDomain> {
    fixture.domains.clone()
}

/// Production v2 registry metadata for a set of domain entries.
pub fn registry_metadata_for(entries: &[RetrievalDomain]) -> Result<Value> {
    let raw: Vec<Value> = entries.iter().map(domain_entry_value).collect();
    crate::registry::validate_domain_entries(&raw)
        .map_err(|e| anyhow::anyhow!("invalid domain registry entries: {e}"))?;
    Ok(crate::registry::registry_metadata_v2(&raw))
}

pub fn derived_storage_evidence(
    fixture: &RetrievalFixture,
    fixture_bytes: &[u8],
) -> Result<DerivedStorageEvidence> {
    if fixture_bytes.is_empty() {
        bail!("fixture byte count must be measured, not inferred");
    }
    let numerator = canonical_domain_bytes(fixture, true)?.len() as u64;
    let denominator = fixture_bytes.len() as u64;
    Ok(DerivedStorageEvidence {
        numerator_bytes: numerator,
        denominator_bytes: denominator,
        ratio: numerator as f64 / denominator as f64,
    })
}

async fn install_registry(
    store: &LadybugStore,
    embedder: &BenchEmbedder,
    entries: &[RetrievalDomain],
) -> Result<()> {
    let metadata = registry_metadata_for(entries)?;
    let row_id = crate::registry::registry_row_id(NAMESPACE);
    let vector = embedder
        .embed(&crate::registry::registry_content(NAMESPACE))
        .await?;
    let payload = MemoryPayload {
        content: crate::registry::registry_content(NAMESPACE),
        user_id: "system".to_string(),
        memory_type: "fact".to_string(),
        agent_name: None,
        location: String::new(),
        location_lines: String::new(),
        metadata,
    };
    store.upsert(NAMESPACE, &row_id, vector, payload).await?;
    store.refresh_ontology(NAMESPACE).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Real-store execution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct ArmConfig {
    arm: RetrievalArm,
    exact: bool,
    registry_variant: &'static str,
    registry: RegistryMode,
}

#[derive(Debug, Clone, Copy)]
enum RegistryMode {
    None,
    AliasOnly,
    Full,
}

const ARM_CONFIGS: [ArmConfig; 4] = [
    ArmConfig {
        arm: RetrievalArm::Baseline,
        exact: false,
        registry_variant: "none",
        registry: RegistryMode::None,
    },
    ArmConfig {
        arm: RetrievalArm::Exact,
        exact: true,
        registry_variant: "none",
        registry: RegistryMode::None,
    },
    ArmConfig {
        arm: RetrievalArm::FlatAliases,
        exact: true,
        registry_variant: "alias_only",
        registry: RegistryMode::AliasOnly,
    },
    ArmConfig {
        arm: RetrievalArm::Ontology,
        exact: true,
        registry_variant: "alias+hierarchy",
        registry: RegistryMode::Full,
    },
];

async fn search_arm(
    store: &LadybugStore,
    embedder: &BenchEmbedder,
    query: &RetrievalQuery,
    exact: bool,
) -> Result<(Vec<String>, f64)> {
    let vector = embedder.embed(&query.text).await?;
    let started = Instant::now();
    let outcome = store
        .search_hybrid_with_domain(
            NAMESPACE,
            vector,
            TOP_K,
            false,
            false,
            true,
            &[query.domain.clone()],
            Some(query.text.as_str()),
            exact,
            SearchMode::Default,
        )
        .await?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok((
        outcome.results.into_iter().map(|r| r.id).collect(),
        elapsed_ms,
    ))
}

/// Execute the strict bundled fixture from disk.
pub async fn run_ablation_file(path: &Path, iterations: usize) -> Result<AblationReceipt> {
    let (fixture, bytes) = load_fixture(path)?;
    validate_fixture(&fixture)?;
    run_ablation(&fixture, &bytes, iterations).await
}

/// Execute four real stores over an already-parsed fixture. Structural
/// integrity is always enforced; release-corpus counts are enforced by
/// `run_ablation_file` / `load_fixture`.
pub async fn run_ablation(
    fixture: &RetrievalFixture,
    fixture_bytes: &[u8],
    iterations: usize,
) -> Result<AblationReceipt> {
    validate_fixture_integrity(fixture)?;
    if iterations < 3 {
        bail!("at least 3 measured iterations are required, got {iterations}");
    }
    let embedder = BenchEmbedder::new(fixture.dimensions);
    let mut arms = Vec::with_capacity(4);
    let mut tempdirs = Vec::with_capacity(4);

    for config in ARM_CONFIGS {
        let temp = tempfile::tempdir()?;
        let store = LadybugStore::for_testing(temp.path().join("store"), fixture.dimensions)?;
        VectorStore::init(&store, NAMESPACE).await?;
        for doc in &fixture.documents {
            let vector = embedder.embed(&doc.content).await?;
            let metadata = json!({
                "domain": doc.domain,
                "symbol": doc.symbol,
            });
            let payload = MemoryPayload {
                content: doc.content.clone(),
                user_id: "synthetic".to_string(),
                memory_type: doc.memory_type.clone(),
                agent_name: None,
                location: doc.location.clone(),
                location_lines: String::new(),
                metadata,
            };
            store.upsert(NAMESPACE, &doc.id, vector, payload).await?;
        }
        match config.registry {
            RegistryMode::None => {}
            RegistryMode::AliasOnly => {
                install_registry(&store, &embedder, &flat_alias_entries(fixture)).await?
            }
            RegistryMode::Full => {
                install_registry(&store, &embedder, &ontology_entries(fixture)).await?
            }
        }

        // Warmup is executed and discarded; it never enters latency evidence.
        for _ in 0..WARMUP_ITERATIONS {
            for query in &fixture.queries {
                search_arm(&store, &embedder, query, config.exact).await?;
            }
        }

        let mut samples: Vec<Vec<f64>> = Vec::with_capacity(iterations);
        let mut iteration_rankings: Vec<Vec<QueryRanking>> = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let mut row = Vec::with_capacity(fixture.queries.len());
            let mut ranked = Vec::with_capacity(fixture.queries.len());
            for query in &fixture.queries {
                let (ids, elapsed_ms) = search_arm(&store, &embedder, query, config.exact).await?;
                row.push(elapsed_ms);
                ranked.push(QueryRanking {
                    query_id: query.id.clone(),
                    ranked_ids: ids,
                });
            }
            samples.push(row);
            iteration_rankings.push(ranked);
        }
        let deterministic = iteration_rankings
            .iter()
            .all(|iteration| iteration == &iteration_rankings[0]);
        let rankings = iteration_rankings[0].clone();

        let mut failed = Vec::new();
        let query_metrics: Vec<QueryMetrics> = fixture
            .queries
            .iter()
            .zip(rankings.iter())
            .map(|(query, ranking)| {
                let forbidden_hits = ranking
                    .ranked_ids
                    .iter()
                    .filter(|id| query.forbidden_ids.contains(id))
                    .cloned()
                    .collect::<Vec<_>>();
                if ranking.ranked_ids.is_empty() {
                    failed.push(query.id.clone());
                }
                QueryMetrics {
                    query_id: query.id.clone(),
                    category: query.category,
                    ndcg_at_10: ndcg_at(&ranking.ranked_ids, &query.relevant, 10),
                    recall_at_5: recall_at(&ranking.ranked_ids, &query.relevant, 5),
                    precision_at_5: precision_at(&ranking.ranked_ids, &query.relevant, 5),
                    forbidden_hits,
                }
            })
            .collect();
        let ontology_targeted: Vec<&QueryMetrics> = fixture
            .queries
            .iter()
            .zip(query_metrics.iter())
            .filter(|(query, _)| query.ontology_targeted)
            .map(|(_, metrics)| metrics)
            .collect();
        if ontology_targeted.is_empty() {
            bail!("fixture must include at least one ontology-targeted query");
        }
        let ontology_recall = ontology_targeted.iter().map(|q| q.recall_at_5).sum::<f64>()
            / ontology_targeted.len() as f64;
        let medians = per_query_p95_medians(&samples)
            .map_err(|e| anyhow::anyhow!("per-query latency medians: {e}"))?;
        let p95 = arm_p95_from_query_medians(&medians);
        let metrics = summarize_arm(config.arm, query_metrics, ontology_recall, p95);
        validate_arm_metrics(&metrics).map_err(|e| anyhow::anyhow!("invalid arm metrics: {e}"))?;
        arms.push(ArmEvidence {
            arm: config.arm,
            registry_variant: config.registry_variant.to_string(),
            exact: config.exact,
            search_mode: "default".to_string(),
            deterministic,
            query_texts: fixture.queries.iter().map(|q| q.text.clone()).collect(),
            rankings,
            metrics,
            latency: LatencyEvidence {
                measured_iterations: iterations,
                warmup_iterations: WARMUP_ITERATIONS,
                per_query_medians_ms: medians,
                arm_p95_ms: p95,
            },
        });
        if !failed.is_empty() {
            bail!(
                "failed queries in arm {:?}: {}",
                config.arm,
                failed.join(", ")
            );
        }
        tempdirs.push(temp);
    }
    drop(tempdirs);

    let derived_storage = derived_storage_evidence(fixture, fixture_bytes)?;
    let deterministic = arms.iter().all(|arm| arm.deterministic);
    let p95_overhead_ms = arms[3].metrics.p95_ms - arms[2].metrics.p95_ms;
    let gate = evaluate_thresholds(
        &arms[2].metrics,
        &arms[3].metrics,
        derived_storage.numerator_bytes,
        derived_storage.denominator_bytes,
        deterministic,
        &[],
    );
    // Keep the zero-baseline rule visible in evidence: a zero baseline can
    // never satisfy the nDCG relative-gain clause.
    let _ndcg_zero_baseline_gain =
        relative_gain(arms[2].metrics.ndcg_at_10, arms[3].metrics.ndcg_at_10);
    let comparison = json!({
        "arms": arms.iter().map(|arm| json!({
            "arm": arm.arm,
            "rankings": arm.rankings,
            "ndcg_at_10": arm.metrics.ndcg_at_10,
            "recall_at_5": arm.metrics.recall_at_5,
            "precision_at_5": arm.metrics.precision_at_5,
            "ontology_recall_at_5": arm.metrics.ontology_recall_at_5,
            "forbidden_hits": arm.metrics.queries.iter().map(|q| q.forbidden_hits.clone()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "derived_storage": derived_storage,
    });
    let determinism_sha256 = hex(Sha256::digest(serde_json::to_vec(&comparison)?).as_slice());
    let verdict = match gate.verdict {
        crate::semantic_eval::metrics::GateVerdict::Pass => "PASS",
        crate::semantic_eval::metrics::GateVerdict::Fail => "FAIL",
    };
    Ok(AblationReceipt {
        schema_version: 1,
        fixture_id: fixture.fixture_id.clone(),
        fixture_sha256: hex(Sha256::digest(fixture_bytes).as_slice()),
        evidence_class: EVIDENCE_CLASS.to_string(),
        production_roi_claimed: false,
        dimensions: fixture.dimensions,
        iterations: MeasuredIterations {
            warmup: WARMUP_ITERATIONS,
            measured: iterations,
            query_count: fixture.queries.len(),
        },
        arms,
        derived_storage,
        p95_overhead_ms,
        deterministic,
        determinism_sha256,
        gate,
        verdict: verdict.to_string(),
        not_evaluated: NOT_EVALUATED.iter().map(|s| s.to_string()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const NS: &str = "semantic-eval-ablation-test";

    fn bundled_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/semantic/retrieval.json")
    }

    fn tiny_fixture_value() -> Value {
        json!({
            "schema_version": 1,
            "fixture_id": "ablation-tiny-test",
            "evidence_class": "synthetic_deterministic",
            "dimensions": 16,
            "curation_notes": "Tiny synthetic implementation-test corpus; negative control included; not release evidence.",
            "documents": [
                {"id":"doc-a","content":"wal checkpoint truncate log","memory_type":"fact","location":"synth/a.rs","domain":"storage-wal","symbol":"ckpt"},
                {"id":"doc-b","content":"checkpoint procedure durable log","memory_type":"rule","location":"synth/b.rs","domain":"storage","symbol":"checkpoint"},
                {"id":"decoy-cross","content":"unrelated accounts billing ledger","memory_type":"fact","location":"other/decoy.rs","domain":"billing","symbol":null}
            ],
            "queries": [
                {"id":"q-alias","text":"chekpoint","domain":"storage-wal","category":"alias","ontology_targeted":false,
                 "relevant":[{"id":"doc-a","gain":3},{"id":"doc-b","gain":1}],"forbidden_ids":["decoy-cross"]},
                {"id":"q-hier","text":"checkpoint","domain":"storage","category":"hierarchy","ontology_targeted":true,
                 "relevant":[{"id":"doc-a","gain":2},{"id":"doc-b","gain":3}],"forbidden_ids":["decoy-cross"]}
            ],
            "flat_aliases": {"chekpoint":"checkpoint","ckpt":"checkpoint"},
            "domains": [
                {"slug":"storage","parent":null,"aliases":["persistence"],"concept_iri":null},
                {"slug":"storage-wal","parent":"storage","aliases":["wal"],"concept_iri":null},
                {"slug":"billing","parent":null,"aliases":["invoicing"],"concept_iri":null}
            ]
        })
    }

    fn tiny_fixture() -> RetrievalFixture {
        serde_json::from_value(tiny_fixture_value()).expect("tiny fixture parses strictly")
    }

    #[test]
    fn strict_fixture_load_and_validation_accepts_bundled_corpus() {
        let (fixture, bytes) = load_fixture(&bundled_path()).expect("bundled fixture loads");
        assert_eq!(fixture.schema_version, 1);
        assert_eq!(fixture.evidence_class, "synthetic_deterministic");
        assert_eq!(fixture.dimensions, 64);
        assert_eq!(fixture.documents.len(), 45);
        assert_eq!(fixture.queries.len(), 60);
        assert!(!fixture.curation_notes.trim().is_empty());
        assert_eq!(
            fixture.flat_aliases.get("chekpoint").map(String::as_str),
            Some("checkpoint")
        );
        assert!(fixture
            .domains
            .iter()
            .any(|d| d.parent.as_deref() == Some("storage")));
        validate_fixture(&fixture).expect("bundled corpus is valid");
        assert_eq!(
            bytes,
            std::fs::read(bundled_path()).expect("raw bytes reread")
        );
    }

    #[test]
    fn unknown_fields_are_rejected_by_strict_dtos() {
        let mut extra = tiny_fixture_value();
        extra["independent_review"] = json!({"reviewer":"nobody"});
        assert!(serde_json::from_value::<RetrievalFixture>(extra).is_err());

        let mut doc_extra = tiny_fixture_value();
        doc_extra["documents"][0]["surprise"] = json!(1);
        assert!(serde_json::from_value::<RetrievalFixture>(doc_extra).is_err());
    }

    #[test]
    fn task_and_every_production_structural_type_are_rejected() {
        for bad in PRODUCTION_STRUCTURAL_MEMORY_TYPES
            .iter()
            .copied()
            .chain(std::iter::once("task"))
        {
            let mut value = tiny_fixture_value();
            value["documents"][0]["memory_type"] = json!(bad);
            let fixture: RetrievalFixture =
                serde_json::from_value(value).expect("DTO accepts string");
            let err = validate_fixture_integrity(&fixture)
                .expect_err("structural/task memory types must fail closed");
            assert!(
                err.to_string().contains(bad),
                "error {err} must name rejected type {bad}"
            );
        }
    }

    #[test]
    fn invalid_refs_overlap_grades_counts_categories_and_dag_are_rejected() {
        let cases: Vec<Value> = vec![
            {
                let mut v = tiny_fixture_value();
                v["queries"][0]["relevant"][0]["id"] = json!("missing");
                v
            },
            {
                let mut v = tiny_fixture_value();
                v["queries"][0]["forbidden_ids"][0] = json!("missing");
                v
            },
            {
                let mut v = tiny_fixture_value();
                v["queries"][0]["forbidden_ids"][0] = json!("doc-a");
                v
            },
            {
                let mut v = tiny_fixture_value();
                v["queries"][0]["relevant"][0]["gain"] = json!(0);
                v
            },
            {
                let mut v = tiny_fixture_value();
                v["queries"][0]["relevant"][0]["gain"] = json!(4);
                v
            },
            {
                let mut v = tiny_fixture_value();
                v["queries"][1]["id"] = json!("q-alias");
                v
            },
            {
                let mut v = tiny_fixture_value();
                v["queries"][0]["domain"] = json!("no-such-domain");
                v
            },
            {
                let mut v = tiny_fixture_value();
                v["domains"][0]["parent"] = json!("storage-wal");
                v["domains"][1]["parent"] = json!("storage");
                v
            },
            {
                let mut v = tiny_fixture_value();
                v["domains"][2]["aliases"] = json!(["wal"]);
                v
            },
        ];
        for value in cases {
            let fixture: RetrievalFixture = serde_json::from_value(value).expect("DTO parses");
            assert!(
                validate_fixture_integrity(&fixture).is_err(),
                "invalid fixture must fail closed"
            );
        }
        let mut bad_category = tiny_fixture();
        bad_category.queries.clear(); // violates full-corpus count/distribution
        assert!(validate_fixture(&bad_category).is_err());
    }

    #[test]
    fn negative_controls_and_curation_notes_are_required() {
        let mut no_notes = tiny_fixture_value();
        no_notes["curation_notes"] = json!("");
        let fixture: RetrievalFixture = serde_json::from_value(no_notes).unwrap();
        assert!(validate_fixture_integrity(&fixture).is_err());

        let mut no_controls = tiny_fixture_value();
        no_controls["queries"][0]["forbidden_ids"] = json!([]);
        no_controls["queries"][1]["forbidden_ids"] = json!([]);
        let fixture: RetrievalFixture = serde_json::from_value(no_controls).unwrap();
        assert!(validate_fixture_integrity(&fixture).is_err());
    }

    #[test]
    fn arm4_canonical_bytes_sorted_slug_aliases_and_raw_fixture_bytes() {
        let (fixture, raw) = load_fixture(&bundled_path()).expect("bundled fixture loads");
        let canonical = canonical_domain_bytes(&fixture, true).expect("canonical serialization");
        let parsed: Vec<Value> = serde_json::from_slice(&canonical).expect("canonical JSON");
        let slugs: Vec<&str> = parsed.iter().map(|e| e["slug"].as_str().unwrap()).collect();
        let mut sorted = slugs.clone();
        sorted.sort_unstable();
        assert_eq!(slugs, sorted, "domains sorted by slug");
        for entry in &parsed {
            let aliases: Vec<&str> = entry["aliases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap())
                .collect();
            let mut a = aliases.clone();
            a.sort_unstable();
            assert_eq!(aliases, a, "aliases sorted");
        }
        let dedup: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        assert_eq!(
            serde_json::to_vec(&dedup).unwrap(),
            canonical,
            "canonical bytes are compact"
        );
        assert_eq!(
            raw.len() as u64,
            std::fs::metadata(bundled_path()).unwrap().len()
        );
        assert!(
            canonical.len() < raw.len(),
            "derived storage must be smaller than fixture payload"
        );
    }

    #[test]
    fn arm3_alias_only_and_arm4_hierarchy_registries_use_production_path() {
        let fixture = tiny_fixture();
        let flat = flat_alias_entries(&fixture);
        let full = ontology_entries(&fixture);
        assert_eq!(flat.len(), full.len());
        for (a, b) in flat.iter().zip(full.iter()) {
            assert_eq!(a.slug, b.slug);
            assert_eq!(a.aliases, b.aliases);
            assert_eq!(a.concept_iri, b.concept_iri);
            assert!(a.parent.is_none(), "arm3 has no hierarchy");
        }
        assert!(
            full.iter().any(|e| e.parent.is_some()),
            "arm4 keeps parent links"
        );
        let metadata = registry_metadata_for(&full).expect("production v2 registry metadata");
        assert!(metadata.get("domain_registry").is_some());
        let index = crate::rdf::OntologyIndex::from_registry_metadata(&metadata);
        let expanded = index.expand_domain("storage");
        assert!(
            expanded.contains(&"storage-wal".to_string()),
            "production expansion follows child links"
        );
    }

    #[tokio::test]
    async fn four_real_stores_produce_deterministic_rankings_without_hardcoded_ranks() {
        let fixture = tiny_fixture();
        let bytes = serde_json::to_vec(&fixture).unwrap();
        let receipt = run_ablation(&fixture, &bytes, 3)
            .await
            .expect("tiny ablation executes");
        assert_eq!(receipt.arms.len(), 4);
        let doc_ids: std::collections::BTreeSet<String> =
            fixture.documents.iter().map(|d| d.id.clone()).collect();
        let expected_texts: Vec<String> = fixture.queries.iter().map(|q| q.text.clone()).collect();
        let expected = [
            (
                crate::semantic_eval::metrics::RetrievalArm::Baseline,
                "none",
                false,
            ),
            (
                crate::semantic_eval::metrics::RetrievalArm::Exact,
                "none",
                true,
            ),
            (
                crate::semantic_eval::metrics::RetrievalArm::FlatAliases,
                "alias_only",
                true,
            ),
            (
                crate::semantic_eval::metrics::RetrievalArm::Ontology,
                "alias+hierarchy",
                true,
            ),
        ];
        for (arm, (kind, registry, exact)) in receipt.arms.iter().zip(expected.iter()) {
            assert_eq!(arm.arm, *kind);
            assert_eq!(arm.registry_variant, *registry);
            assert_eq!(arm.exact, *exact);
            assert_eq!(arm.search_mode, "default");
            assert_eq!(
                arm.query_texts, expected_texts,
                "original query text every arm"
            );
            assert_eq!(arm.rankings.len(), fixture.queries.len());
            for ranking in &arm.rankings {
                assert!(
                    !ranking.ranked_ids.is_empty(),
                    "real retrieval must return ids"
                );
                assert!(
                    ranking.ranked_ids.iter().all(|id| doc_ids.contains(id)),
                    "no fabricated ranks"
                );
            }
        }
        assert!(
            receipt.deterministic,
            "rankings must repeat across measured iterations"
        );
        assert_eq!(receipt.dimensions, fixture.dimensions);
    }

    #[tokio::test]
    async fn latency_uses_three_measured_iterations_warmup_and_per_query_medians() {
        let fixture = tiny_fixture();
        let bytes = serde_json::to_vec(&fixture).unwrap();
        let receipt = run_ablation(&fixture, &bytes, 3)
            .await
            .expect("tiny ablation executes");
        assert_eq!(receipt.iterations.measured, 3);
        assert_eq!(receipt.iterations.warmup, 1);
        assert!(receipt.iterations.measured >= 3);
        for arm in &receipt.arms {
            assert_eq!(arm.latency.measured_iterations, 3);
            assert_eq!(arm.latency.warmup_iterations, 1);
            assert_eq!(
                arm.latency.per_query_medians_ms.len(),
                fixture.queries.len()
            );
            assert!(arm
                .latency
                .per_query_medians_ms
                .iter()
                .all(|v| v.is_finite() && *v >= 0.0));
            assert_eq!(
                arm.latency.arm_p95_ms,
                crate::semantic_eval::metrics::arm_p95_from_query_medians(
                    &arm.latency.per_query_medians_ms
                )
            );
            assert_eq!(arm.metrics.p95_ms, arm.latency.arm_p95_ms);
        }
        let overhead = receipt.arms[3].metrics.p95_ms - receipt.arms[2].metrics.p95_ms;
        assert!((overhead - receipt.p95_overhead_ms).abs() < 1e-9);
    }

    #[test]
    fn gate_fail_closed_on_latency_overhead_and_zero_baseline() {
        use crate::semantic_eval::metrics::*;
        let query = |id: &str, category: QueryCategory, recall: f64| QueryMetrics {
            query_id: id.to_string(),
            category,
            ndcg_at_10: recall,
            recall_at_5: recall,
            precision_at_5: 0.5,
            forbidden_hits: Vec::new(),
        };
        let a3 = ArmMetrics {
            arm: RetrievalArm::FlatAliases,
            ndcg_at_10: 0.5,
            recall_at_5: 0.5,
            precision_at_5: 0.5,
            ontology_recall_at_5: 0.5,
            p95_ms: 0.0,
            queries: vec![
                query("a", QueryCategory::Alias, 0.5),
                query("h", QueryCategory::Hierarchy, 0.5),
            ],
        };
        let mut a4 = a3.clone();
        a4.arm = RetrievalArm::Ontology;
        a4.p95_ms = 30.0;
        a4.ndcg_at_10 = 0.65;
        a4.recall_at_5 = 0.65;
        a4.ontology_recall_at_5 = 0.65;
        a4.queries[0].recall_at_5 = 0.65;
        a4.queries[1].recall_at_5 = 0.65;
        let report = evaluate_thresholds(&a3, &a4, 10, 1000, true, &[]);
        assert_eq!(report.verdict, GateVerdict::Fail);
        assert!(report
            .violations
            .iter()
            .any(|v| v.code == "p95_overhead_exceeded"));
        assert_eq!(
            relative_gain(0.0, 0.6),
            0.0,
            "zero baseline cannot claim nDCG lift"
        );
        a4.p95_ms = 0.0;
        let clean = evaluate_thresholds(&a3, &a4, 10, 1000, true, &[]);
        assert_eq!(
            clean.verdict,
            GateVerdict::Pass,
            "same metrics pass when overhead removed: {:?}",
            clean.violations
        );
    }

    #[test]
    fn receipt_is_strict_json_with_honest_boundary() {
        let fixture = tiny_fixture();
        let bytes = serde_json::to_vec(&fixture).unwrap();
        let receipt = tokio_test_runtime()
            .block_on(run_ablation(&fixture, &bytes, 3))
            .unwrap();
        let json = serde_json::to_vec(&receipt).expect("strict receipt serializes");
        let parsed: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed["evidence_class"], "synthetic_deterministic");
        assert_eq!(parsed["production_roi_claimed"], false);
        assert!(parsed["verdict"] == "PASS" || parsed["verdict"] == "FAIL");
        for label in NOT_EVALUATED {
            assert!(
                receipt.not_evaluated.iter().any(|v| v == label),
                "missing exclusion {label}"
            );
        }
        let roundtrip: AblationReceipt =
            serde_json::from_slice(&json).expect("receipt round-trips strictly");
        // serde_json's default float parser is not bit-exact, so compare the
        // stable structural fields rather than raw f64 equality.
        assert_eq!(roundtrip.fixture_id, receipt.fixture_id);
        assert_eq!(roundtrip.fixture_sha256, receipt.fixture_sha256);
        assert_eq!(roundtrip.evidence_class, receipt.evidence_class);
        assert_eq!(
            roundtrip.production_roi_claimed,
            receipt.production_roi_claimed
        );
        assert_eq!(roundtrip.dimensions, receipt.dimensions);
        assert_eq!(roundtrip.iterations, receipt.iterations);
        assert_eq!(roundtrip.arms.len(), receipt.arms.len());
        assert_eq!(roundtrip.verdict, receipt.verdict);
        assert_eq!(roundtrip.determinism_sha256, receipt.determinism_sha256);
        assert_eq!(roundtrip.not_evaluated, receipt.not_evaluated);
    }

    #[test]
    fn derived_storage_ratio_uses_measured_byte_counts() {
        let (fixture, raw) = load_fixture(&bundled_path()).unwrap();
        let evidence = derived_storage_evidence(&fixture, &raw).unwrap();
        assert_eq!(evidence.denominator_bytes, raw.len() as u64);
        assert_eq!(
            evidence.numerator_bytes,
            canonical_domain_bytes(&fixture, true).unwrap().len() as u64
        );
        assert!(
            (evidence.ratio - evidence.numerator_bytes as f64 / evidence.denominator_bytes as f64)
                .abs()
                < 1e-12
        );
    }

    fn tokio_test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
}
