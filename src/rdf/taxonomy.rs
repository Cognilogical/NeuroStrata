//! Subject-Matter Domain Expertise, W3C SKOS Concept Taxonomy & Epistemic Briefing Engine (`neurostrata-f0bn`).
//!
//! Implements the 7 operations of `neurostrata_taxonomy`:
//! 1. `set_thesis` — singleton Project Thesis with atomic CAS (`commit_action_batch`) and bitemporal supersession.
//! 2. `upsert_concept` — unique-slug W3C SKOS concept with DAG cycle verification, transitive SKOS S27 + contrast
//!    disjointness verification, interface/action_type referential integrity checks, and atomic CAS + edge writes.
//! 3. `tree` — principal-filtered, deterministic, cycle-guarded hierarchical SKOS concept forest.
//! 4. `briefing` — principal-filtered Ontology-Augmented Subject-Matter Briefing with Forward-Expand / Backward-Prune
//!    multi-hop neighborhood subgraph.
//! 5. `compare` — principal-filtered Multi-Perspective Comparative Matrix across evaluation axes, LCA shared ancestors,
//!    and 2-hop contrast/related paths.
//! 6. `gaps` — principal-filtered Epistemic Coverage & Ontology Validation Report (`epistemic_coverage_bps`,
//!    `unmapped_domains`, `dangling_concept_refs`, `shallow_concepts`, `anti_patterns`).
//! 7. `advise` — principal-filtered SME Opportunity & Frontier Advisor joining external research/comparables with
//!    canonical project ontology entities, ranked by `formal_kernel::compute_frontier_leverage_bps`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::domain;
use crate::formal_kernel::{
    classify_epistemic_status, compute_epistemic_coverage_bps, compute_frontier_leverage_bps,
    realization_status_gap_bps, validate_skos_disjointness, validate_taxonomy_dag, EpistemicStatus,
};
use crate::rdf::action::{
    is_active_payload, ActionError, ActionSecurityContext, ActionWriteSet, EdgeDelta,
    EdgeMutationOp, ExpectedHead, StagedMemoryWrite,
};
use crate::store::ladybug::LadybugStore;
use crate::traits::{Embedder, MemoryPayload, VectorStore};

fn default_realization_status() -> String {
    "unknown".to_string()
}

/// Project Thesis (`memory_type = "thesis"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThesisSpec {
    pub primary_domain: String,
    pub mission: String,
    pub direction: String,
    pub core_pillars: Vec<String>,
    #[serde(default)]
    pub adjacent_domains: Vec<String>,
    #[serde(default)]
    pub evaluation_axes: Vec<String>,
    #[serde(default)]
    pub non_goals: Vec<String>,
}

impl ThesisSpec {
    pub fn from_payload(payload: &MemoryPayload) -> Option<Self> {
        let obj = payload.metadata.get("thesis")?;
        serde_json::from_value(obj.clone()).ok()
    }
}

/// Structured provenance for a comparable library, standard, or exemplar system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparableImplementation {
    pub name: String,
    /// `"library" | "standard" | "reference_system"`
    pub kind: String,
    /// Non-empty SPDX license identifier (e.g. `"MIT"`, `"Apache-2.0"`, `"proprietary-reference"`).
    pub license: String,
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Stored specification of a W3C SKOS domain concept (`memory_type = "concept"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConceptSpec {
    pub slug: String,
    pub pref_label: String,
    pub definition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub broader: Vec<String>,
    #[serde(default)]
    pub related_concepts: Vec<String>,
    #[serde(default)]
    pub contrasts_with: Vec<String>,
    #[serde(default)]
    pub tradeoffs: Vec<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub comparable_implementations: Vec<ComparableImplementation>,
    #[serde(default = "default_realization_status")]
    pub realization_status: String,
    #[serde(default)]
    pub current_limitations: Vec<String>,
    #[serde(default)]
    pub frontier_opportunities: Vec<String>,
    #[serde(default)]
    pub implemented_interfaces: Vec<String>,
    #[serde(default)]
    pub linked_action_types: Vec<String>,
    #[serde(default)]
    pub evaluation_scores: BTreeMap<String, u32>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept_iri: Option<String>,
}

impl ConceptSpec {
    pub fn from_payload(payload: &MemoryPayload) -> Option<Self> {
        let obj = payload.metadata.get("concept")?;
        let mut spec: Self = serde_json::from_value(obj.clone()).ok()?;
        if spec.aliases.is_empty() {
            spec.aliases = extract_string_vec_from_meta(obj, "alt_labels");
        }
        if spec.related_concepts.is_empty() {
            spec.related_concepts = extract_string_vec_from_meta(obj, "related");
        }
        spec.slug = domain::normalize_domain_slug(&spec.slug)?;
        if let Some(d) = &spec.domain {
            spec.domain = domain::normalize_domain_slug(d);
        }
        spec.broader = normalize_slug_vec(&spec.broader);
        spec.related_concepts = normalize_slug_vec(&spec.related_concepts);
        spec.contrasts_with = normalize_slug_vec(&spec.contrasts_with);
        Some(spec)
    }
}

/// Read-time hydrated concept with derived `narrower`, symmetrized associative/contrast links,
/// and dynamically computed `epistemic_status` and `frontier_leverage_bps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HydratedConcept {
    pub id: String,
    pub slug: String,
    pub pref_label: String,
    pub definition: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub aliases: Vec<String>,
    pub broader: Vec<String>,
    pub narrower: Vec<String>,
    pub related_concepts: Vec<String>,
    pub contrasts_with: Vec<String>,
    pub tradeoffs: Vec<String>,
    pub references: Vec<String>,
    pub comparable_implementations: Vec<ComparableImplementation>,
    pub realization_status: String,
    pub current_limitations: Vec<String>,
    pub frontier_opportunities: Vec<String>,
    pub implemented_interfaces: Vec<String>,
    pub linked_action_types: Vec<String>,
    pub evaluation_scores: BTreeMap<String, u32>,
    pub evidence_refs: Vec<String>,
    pub governs: Vec<String>,
    pub locations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept_iri: Option<String>,
    pub epistemic_status: String,
    pub epistemic_weight_bps: u32,
    pub descendant_count: usize,
    pub grounding_count: usize,
    pub verified_grounding_ids: Vec<String>,
    pub axis_alignment_bps: u32,
    pub frontier_leverage_bps: u32,
    #[serde(skip)]
    pub is_public: bool,
}

/// Hierarchical node returned by `neurostrata_taxonomy(operation: "tree")`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxonomyTreeNode {
    pub slug: String,
    pub pref_label: String,
    pub definition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub epistemic_status: String,
    pub epistemic_weight_bps: u32,
    pub realization_status: String,
    pub frontier_leverage_bps: u32,
    pub descendant_count: usize,
    pub grounding_count: usize,
    pub aliases: Vec<String>,
    pub broader: Vec<String>,
    pub narrower: Vec<String>,
    pub related_concepts: Vec<String>,
    pub contrasts_with: Vec<String>,
    pub children: Vec<TaxonomyTreeNode>,
}

/// Deterministic ontology design lint emitted by `gaps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OntologyAntiPattern {
    pub kind: String,
    pub target: String,
    pub message: String,
}

/// Canonical project entities grounded to a subject-matter recommendation in `advise`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalGroundingSummary {
    pub implemented_interfaces: Vec<String>,
    pub linked_action_types: Vec<String>,
    pub governing_rule_and_fact_ids: Vec<String>,
    pub governing_summaries: Vec<String>,
    pub locations: Vec<String>,
    pub open_task_ids: Vec<String>,
}

/// Thesis alignment breakdown for an `ExpertRecommendation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThesisAlignmentSummary {
    pub axis_alignment_bps: u32,
    pub axis_scores: BTreeMap<String, u32>,
    /// `"not_assessed" | "potential_overlap"` (never claims unproven `"zero_conflict"`).
    pub non_goal_assessment: String,
    pub overlapping_non_goals: Vec<String>,
}

/// Ranked SME improvement recommendation returned by `advise`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertRecommendation {
    pub rank: usize,
    pub concept_slug: String,
    pub pref_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_pillar: Option<String>,
    pub epistemic_status: String,
    pub realization_status: String,
    pub leverage_score_bps: u32,
    pub canonical_grounding: CanonicalGroundingSummary,
    pub current_state_and_limitations: Vec<String>,
    pub frontier_recommendations: Vec<String>,
    pub references_and_comparables: Value,
    pub thesis_alignment: ThesisAlignmentSummary,
}

/// Public-safe external research prompt returned by `advise`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchPrompt {
    pub target_slug_or_domain: String,
    pub reason: String,
    pub suggested_query: String,
    pub public_safe: bool,
}

/// Unified request payload for `neurostrata_taxonomy` (MCP, HTTP Daemon, and CLI).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaxonomyRequest {
    pub operation: String,
    pub namespace: String,
    #[serde(default)]
    pub principal: Option<String>,

    // set_thesis fields
    #[serde(default)]
    pub primary_domain: Option<String>,
    #[serde(default)]
    pub mission: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub core_pillars: Option<Vec<String>>,
    #[serde(default)]
    pub adjacent_domains: Option<Vec<String>>,
    #[serde(default)]
    pub evaluation_axes: Option<Vec<String>>,
    #[serde(default)]
    pub non_goals: Option<Vec<String>>,

    // upsert_concept fields
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub pref_label: Option<String>,
    #[serde(default)]
    pub definition: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, alias = "alt_labels")]
    pub aliases: Option<Vec<String>>,
    #[serde(default)]
    pub broader: Option<Vec<String>>,
    #[serde(default, alias = "related")]
    pub related_concepts: Option<Vec<String>>,
    #[serde(default)]
    pub contrasts_with: Option<Vec<String>>,
    #[serde(default)]
    pub tradeoffs: Option<Vec<String>>,
    #[serde(default)]
    pub references: Option<Vec<String>>,
    #[serde(default)]
    pub comparable_implementations: Option<Vec<ComparableImplementation>>,
    #[serde(default)]
    pub realization_status: Option<String>,
    #[serde(default)]
    pub current_limitations: Option<Vec<String>>,
    #[serde(default)]
    pub frontier_opportunities: Option<Vec<String>>,
    #[serde(default)]
    pub implemented_interfaces: Option<Vec<String>>,
    #[serde(default)]
    pub linked_action_types: Option<Vec<String>>,
    #[serde(default)]
    pub evaluation_scores: Option<BTreeMap<String, u32>>,
    #[serde(default)]
    pub evidence_refs: Option<Vec<String>>,
    #[serde(default)]
    pub concept_iri: Option<String>,
    #[serde(default)]
    pub governs: Option<Vec<String>>,
    #[serde(default)]
    pub related_to: Option<Vec<String>>,
    #[serde(default)]
    pub extracted_from: Option<Vec<String>>,
    #[serde(default)]
    pub locations: Option<Vec<Value>>,
    #[serde(default)]
    pub eligibility: Option<String>,
    #[serde(default)]
    pub access_policy: Option<Value>,
    #[serde(default)]
    pub visible_to: Option<Value>,

    // tree / briefing / compare / advise query parameters
    #[serde(default)]
    pub root_slug: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub max_hops: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub slugs: Option<Vec<String>>,
    #[serde(default)]
    pub axes: Option<Vec<String>>,
    #[serde(default)]
    pub top_k: Option<usize>,
}

/// Typed response wrapper for `neurostrata_taxonomy`.
pub type TaxonomyResponse = Value;

fn normalize_slug_vec(raw: &[String]) -> Vec<String> {
    let mut set = BTreeSet::new();
    for item in raw {
        if let Some(s) = domain::normalize_domain_slug(item) {
            set.insert(s);
        }
    }
    set.into_iter().collect()
}

fn validate_strict_slug_vec(raw: &[String], field_name: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for item in raw {
        let norm = domain::normalize_domain_slug(item).ok_or_else(|| {
            format!("invalid slug '{item}' in '{field_name}': must normalize to lowercase-kebab")
        })?;
        if seen.insert(norm.clone()) {
            out.push(norm);
        }
    }
    Ok(out)
}

pub fn is_public_access_policy(metadata: &Value) -> bool {
    if crate::rebac::is_restricted(metadata) {
        return false;
    }
    let Some(ap) = metadata.get("access_policy") else {
        return true;
    };
    if ap.is_null() {
        return true;
    }
    match ap.as_object() {
        Some(obj) if obj.is_empty() => true,
        Some(obj) => {
            obj.get("public").and_then(|v| v.as_bool()) == Some(true)
                && obj
                    .get("allowed_users")
                    .and_then(|v| v.as_array())
                    .map(|a| a.is_empty())
                    .unwrap_or(true)
                && obj
                    .get("allowed_roles")
                    .and_then(|v| v.as_array())
                    .map(|a| a.is_empty())
                    .unwrap_or(true)
        }
        None => false,
    }
}

fn apply_access_controls_to_metadata(
    meta: &mut Value,
    principal: &str,
    access_policy: Option<&Value>,
    visible_to: Option<&Value>,
) {
    if let Some(vt) = visible_to {
        if !vt.is_null() {
            meta["visible_to"] = vt.clone();
        }
    }
    if let Some(ap) = access_policy {
        if !ap.is_null() {
            meta["access_policy"] = ap.clone();
            if meta.get("visible_to").is_none() {
                let dummy = json!({ "access_policy": ap });
                if !is_public_access_policy(&dummy) {
                    let mut users: Vec<Value> = ap
                        .get("allowed_users")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if users.is_empty() && !principal.trim().is_empty() {
                        users.push(json!(principal));
                    }
                    meta["visible_to"] = Value::Array(users);
                }
            }
        }
    }
}

fn is_active_non_conflicting(payload: &MemoryPayload, now_ts: i64) -> bool {
    is_active_payload(payload, now_ts)
        && !crate::synaptic_mesh::conflict::is_conflicting_metadata(&payload.metadata)
}

fn resolve_eligibility(
    requested: Option<&str>,
    access_policy: Option<&Value>,
    visible_to: Option<&Value>,
) -> Result<String, String> {
    let has_non_public_policy = access_policy.is_some_and(|ap| {
        if ap.is_null() {
            false
        } else {
            let dummy = json!({ "access_policy": ap });
            !is_public_access_policy(&dummy)
        }
    }) || visible_to.is_some_and(|vt| {
        if vt.is_null() {
            false
        } else {
            let dummy = json!({ "visible_to": vt });
            crate::rebac::is_restricted(&dummy)
        }
    });
    match requested.map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("private") => Ok("private".to_string()),
        Some("shareable") => {
            if has_non_public_policy {
                Err(
                    "eligibility 'shareable' is forbidden when a non-public access_policy or restricted visible_to is set"
                        .to_string(),
                )
            } else {
                Ok("shareable".to_string())
            }
        }
        Some(other) => Err(format!(
            "invalid eligibility '{other}': must be 'private' or 'shareable'"
        )),
    }
}

fn extract_string_vec_from_meta(meta: &Value, key: &str) -> Vec<String> {
    meta.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn extract_locations_from_payload(payload: &MemoryPayload) -> Vec<String> {
    let mut locs = BTreeSet::new();
    if !payload.location.trim().is_empty() {
        locs.insert(payload.location.trim().to_string());
    }
    if let Some(arr) = payload.metadata.get("locations").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                if !s.trim().is_empty() {
                    locs.insert(s.trim().to_string());
                }
            } else if let Some(path) = item.get("path").and_then(|v| v.as_str()) {
                if !path.trim().is_empty() {
                    locs.insert(path.trim().to_string());
                }
            }
        }
    }
    locs.into_iter().collect()
}

fn collect_active_interface_names(active_rows: &[(String, MemoryPayload)]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_id, pl) in active_rows {
        if pl.memory_type == "interface_def" {
            if let Some(name) = pl
                .metadata
                .get("interface_def")
                .and_then(|i| i.get("name").or_else(|| i.get("interface_slug")))
                .and_then(|v| v.as_str())
            {
                out.insert(name.trim().to_string());
            }
        }
        if pl.memory_type == "digital_twin_schema" {
            if let Some(ts) = pl.metadata.get("twin_schema") {
                let is_iface = ts
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map_or(false, |k| k == "interface");
                if is_iface {
                    if let Some(s) = ts.get("interface_slug").and_then(|v| v.as_str()) {
                        out.insert(s.trim().to_string());
                    }
                    if let Some(s) = ts.get("schema_id").and_then(|v| v.as_str()) {
                        out.insert(s.trim().to_string());
                    }
                }
            }
        }
    }
    out
}

fn collect_active_action_slugs(active_rows: &[(String, MemoryPayload)]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_id, pl) in active_rows {
        if pl.memory_type == "action_type" {
            if let Some(slug) = pl
                .metadata
                .get("action_type")
                .and_then(|a| a.get("slug"))
                .and_then(|v| v.as_str())
            {
                out.insert(slug.trim().to_string());
            }
        }
    }
    out
}

/// Validates that the candidate concept forest (all active concepts with `candidate` inserted)
/// satisfies `validate_taxonomy_dag` and `validate_skos_disjointness`.
fn validate_concept_forest(
    concepts_by_slug: &BTreeMap<String, ConceptSpec>,
) -> Result<(), String> {
    let mut slug_to_num: BTreeMap<String, u32> = BTreeMap::new();
    let mut next_id: u32 = 1;
    let mut intern = |s: &str| -> u32 {
        if let Some(&id) = slug_to_num.get(s) {
            id
        } else {
            let id = next_id;
            next_id += 1;
            slug_to_num.insert(s.to_string(), id);
            id
        }
    };

    let mut broader_edges: Vec<(u32, u32)> = Vec::new();
    let mut related_edges: Vec<(u32, u32)> = Vec::new();
    let mut contrast_edges: Vec<(u32, u32)> = Vec::new();

    for (slug, spec) in concepts_by_slug {
        let u = intern(slug);
        for parent in &spec.broader {
            let v = intern(parent);
            broader_edges.push((u, v));
        }
        for rel in &spec.related_concepts {
            let v = intern(rel);
            related_edges.push((u, v));
        }
        for con in &spec.contrasts_with {
            let v = intern(con);
            contrast_edges.push((u, v));
        }
    }

    if !validate_taxonomy_dag(&broader_edges) {
        return Err(
            "taxonomy DAG cycle detected in SKOS 'broader' hierarchy (self-loop or directed cycle)"
                .to_string(),
        );
    }

    if !validate_skos_disjointness(&broader_edges, &related_edges, &contrast_edges) {
        return Err(
            "SKOS S27 / contrast disjointness violation: 'related_concepts' and 'contrasts_with' must be disjoint from transitive 'broader' hierarchy"
                .to_string(),
        );
    }

    Ok(())
}

/// Internal principal-filtered snapshot of a namespace's thesis, hydrated concepts, and active rows.
pub struct PrincipalTaxonomyView {
    pub thesis_id: Option<String>,
    pub thesis: Option<ThesisSpec>,
    pub thesis_is_public: bool,
    pub concepts: BTreeMap<String, HydratedConcept>,
    pub all_active_concept_slugs: BTreeSet<String>,
    pub restricted_concept_slugs: BTreeSet<String>,
    pub raw_visible_concept_specs: BTreeMap<String, ConceptSpec>,
    pub visible_rows: Vec<(String, MemoryPayload)>,
}

pub fn build_principal_taxonomy_view(
    active_rows: &[(String, MemoryPayload)],
    active_edges: &[(String, String, String, Option<crate::rdf::EdgeProvenance>)],
    principal: &str,
) -> PrincipalTaxonomyView {
    let mut visible_rows: Vec<(String, MemoryPayload)> = Vec::new();
    let mut all_active_concept_slugs: BTreeSet<String> = BTreeSet::new();
    let mut restricted_concept_slugs: BTreeSet<String> = BTreeSet::new();

    // Track latest active concept row per slug across all active rows first to know which slugs are restricted.
    let mut all_concepts_latest: BTreeMap<String, (i64, String, MemoryPayload)> = BTreeMap::new();
    for (id, payload) in active_rows {
        if payload.memory_type == "concept" {
            if let Some(spec) = ConceptSpec::from_payload(payload) {
                let vf = payload
                    .metadata
                    .get("valid_from")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let replace = all_concepts_latest
                    .get(&spec.slug)
                    .map_or(true, |(old_vf, old_id, _)| (vf, id) >= (*old_vf, old_id));
                if replace {
                    all_concepts_latest
                        .insert(spec.slug.clone(), (vf, id.clone(), payload.clone()));
                }
            }
        }
        if crate::rebac::can_read_memory(principal, &payload.user_id, &payload.metadata) {
            visible_rows.push((id.clone(), payload.clone()));
        }
    }

    for (slug, (_vf, _id, payload)) in &all_concepts_latest {
        all_active_concept_slugs.insert(slug.clone());
        if !crate::rebac::can_read_memory(principal, &payload.user_id, &payload.metadata) {
            restricted_concept_slugs.insert(slug.clone());
        }
    }

    // Resolve latest active visible thesis
    let mut latest_thesis: Option<(i64, String, ThesisSpec, bool)> = None;
    for (id, payload) in &visible_rows {
        if payload.memory_type == "thesis" {
            if let Some(t) = ThesisSpec::from_payload(payload) {
                let vf = payload
                    .metadata
                    .get("valid_from")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let is_pub = is_public_access_policy(&payload.metadata);
                if latest_thesis
                    .as_ref()
                    .map_or(true, |(old_vf, old_id, _, _)| (vf, id) >= (*old_vf, old_id))
                {
                    latest_thesis = Some((vf, id.clone(), t, is_pub));
                }
            }
        }
    }

    let (thesis_id, thesis, thesis_is_public) = match latest_thesis {
        Some((_vf, id, t, is_pub)) => (Some(id), Some(t), is_pub),
        None => (None, None, true),
    };

    // Resolve latest active visible concept per slug
    let mut visible_concepts_raw: BTreeMap<String, (i64, String, MemoryPayload, ConceptSpec)> =
        BTreeMap::new();
    for (id, payload) in &visible_rows {
        if payload.memory_type == "concept" {
            if let Some(spec) = ConceptSpec::from_payload(payload) {
                let vf = payload
                    .metadata
                    .get("valid_from")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let replace = visible_concepts_raw
                    .get(&spec.slug)
                    .map_or(true, |(old_vf, old_id, _, _)| (vf, id) >= (*old_vf, old_id));
                if replace {
                    visible_concepts_raw.insert(
                        spec.slug.clone(),
                        (vf, id.clone(), payload.clone(), spec),
                    );
                }
            }
        }
    }

    let mut raw_visible_concept_specs: BTreeMap<String, ConceptSpec> = BTreeMap::new();
    for (slug, (_vf, _id, _pl, spec)) in &visible_concepts_raw {
        raw_visible_concept_specs.insert(slug.clone(), spec.clone());
    }

    // Cycle-guarded broader and derived narrower across visible concepts (stripping restricted concept targets)
    let mut slug_to_num: BTreeMap<String, u32> = BTreeMap::new();
    let mut next_id: u32 = 1;
    let mut intern = |s: &str| -> u32 {
        if let Some(&id) = slug_to_num.get(s) {
            id
        } else {
            let id = next_id;
            next_id += 1;
            slug_to_num.insert(s.to_string(), id);
            id
        }
    };

    let mut accepted_broader_pairs: Vec<(u32, u32)> = Vec::new();
    let mut clean_broader_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut derived_narrower_map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for (slug, (_vf, _id, _pl, spec)) in &visible_concepts_raw {
        let u = intern(slug);
        let mut parents = Vec::new();
        for p in &spec.broader {
            if restricted_concept_slugs.contains(p) {
                continue;
            }
            let v = intern(p);
            let mut candidate = accepted_broader_pairs.clone();
            candidate.push((u, v));
            if validate_taxonomy_dag(&candidate) {
                accepted_broader_pairs = candidate;
                parents.push(p.clone());
                if visible_concepts_raw.contains_key(p) {
                    derived_narrower_map
                        .entry(p.clone())
                        .or_default()
                        .insert(slug.clone());
                }
            }
        }
        clean_broader_map.insert(slug.clone(), parents);
    }

    // Symmetrized related_concepts and contrasts_with (stripping restricted concept targets and S27 violations)
    let mut sym_related: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut sym_contrasts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for (slug, (_vf, _id, _pl, spec)) in &visible_concepts_raw {
        let u = intern(slug);
        for r in &spec.related_concepts {
            if restricted_concept_slugs.contains(r) {
                continue;
            }
            let v = intern(r);
            if validate_skos_disjointness(&accepted_broader_pairs, &[(u, v)], &[]) {
                sym_related
                    .entry(slug.clone())
                    .or_default()
                    .insert(r.clone());
                if visible_concepts_raw.contains_key(r) {
                    sym_related
                        .entry(r.clone())
                        .or_default()
                        .insert(slug.clone());
                }
            }
        }
        for c in &spec.contrasts_with {
            if restricted_concept_slugs.contains(c) {
                continue;
            }
            let v = intern(c);
            if validate_skos_disjointness(&accepted_broader_pairs, &[], &[(u, v)]) {
                sym_contrasts
                    .entry(slug.clone())
                    .or_default()
                    .insert(c.clone());
                if visible_concepts_raw.contains_key(c) {
                    sym_contrasts
                        .entry(c.clone())
                        .or_default()
                        .insert(slug.clone());
                }
            }
        }
    }

    // Active visible rule/fact/procedure rows for dynamic epistemic grounding
    let visible_normative_rows: BTreeMap<String, &MemoryPayload> = visible_rows
        .iter()
        .filter(|(_id, pl)| matches!(pl.memory_type.as_str(), "rule" | "fact" | "procedure"))
        .map(|(id, pl)| (id.clone(), pl))
        .collect();

    let active_interfaces = collect_active_interface_names(&visible_rows);
    let active_actions = collect_active_action_slugs(&visible_rows);

    let mut hydrated_concepts: BTreeMap<String, HydratedConcept> = BTreeMap::new();

    for (slug, (_vf, row_id, payload, spec)) in &visible_concepts_raw {
        let broader = clean_broader_map.get(slug).cloned().unwrap_or_default();
        let narrower: Vec<String> = derived_narrower_map
            .get(slug)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        let related_concepts: Vec<String> = sym_related
            .get(slug)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        let contrasts_with: Vec<String> = sym_contrasts
            .get(slug)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();

        // Compute transitive descendant_count (cycle-guarded BFS over derived_narrower_map)
        let mut descendants = BTreeSet::new();
        let mut queue: VecDeque<String> = narrower.iter().cloned().collect();
        while let Some(child) = queue.pop_front() {
            if descendants.insert(child.clone()) {
                if let Some(grandchildren) = derived_narrower_map.get(&child) {
                    for gc in grandchildren {
                        if !descendants.contains(gc) {
                            queue.push_back(gc.clone());
                        }
                    }
                }
            }
        }
        let descendant_count = descendants.len();

        let governs = extract_string_vec_from_meta(&payload.metadata, "governs");
        let locations = extract_locations_from_payload(payload);

        // Find active visible rule/fact/procedure rows that ground this concept
        let mut verified_grounding_ids: BTreeSet<String> = BTreeSet::new();
        for ev_ref in &spec.evidence_refs {
            if visible_normative_rows.contains_key(ev_ref) {
                verified_grounding_ids.insert(ev_ref.clone());
            }
        }
        for gov_ref in &governs {
            if visible_normative_rows.contains_key(gov_ref) {
                verified_grounding_ids.insert(gov_ref.clone());
            }
        }
        for (src, rel, dst, _prov) in active_edges {
            if matches!(
                rel.as_str(),
                "GOVERNS" | "RELATES_TO" | "DEPENDS_ON" | "EXTRACTED_FROM"
            ) {
                if src == row_id && visible_normative_rows.contains_key(dst) {
                    verified_grounding_ids.insert(dst.clone());
                } else if dst == row_id && visible_normative_rows.contains_key(src) {
                    verified_grounding_ids.insert(src.clone());
                }
            }
        }
        for (norm_id, norm_pl) in &visible_normative_rows {
            let linked_by_meta = norm_pl
                .metadata
                .get("concept_slug")
                .and_then(|v| v.as_str())
                .and_then(domain::normalize_domain_slug)
                .map_or(false, |s| &s == slug)
                || norm_pl
                    .metadata
                    .get("concepts")
                    .and_then(|v| v.as_array())
                    .map_or(false, |arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .filter_map(domain::normalize_domain_slug)
                            .any(|s| &s == slug)
                    })
                || norm_pl
                    .metadata
                    .get("related_to")
                    .and_then(|v| v.as_array())
                    .map_or(false, |arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .any(|s| s == row_id || s == slug)
                    });
            if linked_by_meta {
                verified_grounding_ids.insert(norm_id.clone());
            }
        }

        let valid_iface_count = spec
            .implemented_interfaces
            .iter()
            .filter(|i| active_interfaces.contains(*i))
            .count();
        let valid_action_count = spec
            .linked_action_types
            .iter()
            .filter(|a| active_actions.contains(*a))
            .count();

        let grounding_count = verified_grounding_ids.len()
            + valid_iface_count
            + valid_action_count
            + locations.len();

        let is_core_pillar_root = thesis.as_ref().map_or(false, |t| {
            t.core_pillars.contains(slug)
                || spec
                    .domain
                    .as_ref()
                    .map_or(false, |d| t.core_pillars.contains(d))
        });

        let has_definition = !spec.definition.trim().is_empty();
        let has_taxonomy_link = !broader.is_empty()
            || !narrower.is_empty()
            || !related_concepts.is_empty()
            || is_core_pillar_root
            || spec.domain.as_ref().map_or(false, |d| !d.trim().is_empty());
        let has_synthesis = !spec.tradeoffs.is_empty()
            && (!contrasts_with.is_empty()
                || !spec.references.is_empty()
                || !spec.comparable_implementations.is_empty());
        let has_empirical_grounding = has_synthesis && !verified_grounding_ids.is_empty();

        let ep_status: EpistemicStatus = classify_epistemic_status(
            has_definition,
            has_taxonomy_link,
            has_synthesis,
            has_empirical_grounding,
        );

        let axis_alignment_bps = compute_axis_alignment_bps(&spec.evaluation_scores, thesis.as_ref());
        let realization_gap_bps = realization_status_gap_bps(&spec.realization_status);
        let has_frontier_or_limitation =
            !spec.frontier_opportunities.is_empty() || !spec.current_limitations.is_empty();
        let has_references_or_comparables =
            !spec.references.is_empty() || !spec.comparable_implementations.is_empty();

        let frontier_leverage_bps = compute_frontier_leverage_bps(
            axis_alignment_bps,
            realization_gap_bps,
            grounding_count as u32,
            has_frontier_or_limitation,
            has_references_or_comparables,
        );

        let is_public = is_public_access_policy(&payload.metadata);

        hydrated_concepts.insert(
            slug.clone(),
            HydratedConcept {
                id: row_id.clone(),
                slug: slug.clone(),
                pref_label: spec.pref_label.clone(),
                definition: spec.definition.clone(),
                content: payload.content.clone(),
                domain: spec.domain.clone(),
                aliases: spec.aliases.clone(),
                broader,
                narrower,
                related_concepts,
                contrasts_with,
                tradeoffs: spec.tradeoffs.clone(),
                references: spec.references.clone(),
                comparable_implementations: spec.comparable_implementations.clone(),
                realization_status: spec.realization_status.clone(),
                current_limitations: spec.current_limitations.clone(),
                frontier_opportunities: spec.frontier_opportunities.clone(),
                implemented_interfaces: spec.implemented_interfaces.clone(),
                linked_action_types: spec.linked_action_types.clone(),
                evaluation_scores: spec.evaluation_scores.clone(),
                evidence_refs: spec.evidence_refs.clone(),
                governs,
                locations,
                concept_iri: spec.concept_iri.clone(),
                epistemic_status: ep_status.as_str().to_string(),
                epistemic_weight_bps: ep_status.weight_bps(),
                descendant_count,
                grounding_count,
                verified_grounding_ids: verified_grounding_ids.into_iter().collect(),
                axis_alignment_bps,
                frontier_leverage_bps,
                is_public,
            },
        );
    }

    PrincipalTaxonomyView {
        thesis_id,
        thesis,
        thesis_is_public,
        concepts: hydrated_concepts,
        all_active_concept_slugs,
        restricted_concept_slugs,
        raw_visible_concept_specs,
        visible_rows,
    }
}

fn compute_axis_alignment_bps(
    scores: &BTreeMap<String, u32>,
    thesis: Option<&ThesisSpec>,
) -> u32 {
    if scores.is_empty() {
        return 5000;
    }
    if let Some(t) = thesis {
        if !t.evaluation_axes.is_empty() {
            let mut sum: u64 = 0;
            let mut count: u64 = 0;
            for axis in &t.evaluation_axes {
                if let Some(&sc) = scores.get(axis) {
                    sum += (sc.min(10000)) as u64;
                    count += 1;
                }
            }
            if count > 0 {
                return ((sum / count) as u32).min(10000);
            }
        }
    }
    let sum: u64 = scores.values().map(|&v| (v.min(10000)) as u64).sum();
    ((sum / (scores.len() as u64)) as u32).min(10000)
}

async fn load_active_rows_and_edges(
    store: &LadybugStore,
    namespace: &str,
    now_ts: i64,
) -> Result<
    (
        Vec<(String, MemoryPayload)>,
        Vec<(String, String, String, Option<crate::rdf::EdgeProvenance>)>,
    ),
    String,
> {
    let records = store
        .list(namespace, None, None)
        .await
        .map_err(|e| format!("failed to list namespace '{namespace}': {e}"))?;
    let active_rows: Vec<(String, MemoryPayload)> = records
        .into_iter()
        .filter(|r| is_active_non_conflicting(&r.payload, now_ts))
        .map(|r| (r.id, r.payload))
        .collect();
    let active_edges = store.list_edges(namespace).await.unwrap_or_default();
    Ok((active_rows, active_edges))
}

/// Execute any of the 7 `neurostrata_taxonomy` operations (`set_thesis`, `upsert_concept`, `tree`,
/// `briefing`, `compare`, `gaps`, `advise`).
pub async fn execute_taxonomy(
    store: &LadybugStore,
    embedder: &dyn Embedder,
    req: TaxonomyRequest,
    security: &ActionSecurityContext,
) -> Result<TaxonomyResponse, String> {
    let namespace = req.namespace.trim().to_string();
    if namespace.is_empty() {
        return Err("namespace is required".to_string());
    }
    let op = req.operation.trim().to_lowercase().replace('-', "_");
    let effective_principal = crate::rebac::resolve_principal(
        &security.authenticated_principal,
        req.principal
            .as_deref()
            .or(security.requested_principal.as_deref()),
    );

    match op.as_str() {
        "set_thesis" => {
            execute_set_thesis(store, embedder, &namespace, &req, &effective_principal).await
        }
        "upsert_concept" => {
            execute_upsert_concept(store, embedder, &namespace, &req, &effective_principal).await
        }
        "tree" => execute_tree(store, &namespace, &req, &effective_principal).await,
        "briefing" => execute_briefing(store, &namespace, &req, &effective_principal).await,
        "compare" => execute_compare(store, &namespace, &req, &effective_principal).await,
        "gaps" => execute_gaps(store, &namespace, &req, &effective_principal).await,
        "advise" => execute_advise(store, &namespace, &req, &effective_principal).await,
        other => Err(format!(
            "unknown taxonomy operation '{other}': expected set_thesis|upsert_concept|tree|briefing|compare|gaps|advise"
        )),
    }
}

async fn execute_set_thesis(
    store: &LadybugStore,
    embedder: &dyn Embedder,
    namespace: &str,
    req: &TaxonomyRequest,
    principal: &str,
) -> Result<Value, String> {
    let primary_domain = req
        .primary_domain
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "primary_domain is required for set_thesis".to_string())?
        .to_string();
    let mission = req
        .mission
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "mission is required for set_thesis".to_string())?
        .to_string();
    let direction = req
        .direction
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "direction is required for set_thesis".to_string())?
        .to_string();
    let raw_pillars = req
        .core_pillars
        .as_ref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "core_pillars (non-empty array) is required for set_thesis".to_string())?;
    let core_pillars = validate_strict_slug_vec(raw_pillars, "core_pillars")?;
    if core_pillars.is_empty() {
        return Err("core_pillars must contain at least one valid slug".to_string());
    }
    let adjacent_domains = match &req.adjacent_domains {
        Some(v) => validate_strict_slug_vec(v, "adjacent_domains")?,
        None => Vec::new(),
    };
    let evaluation_axes = match &req.evaluation_axes {
        Some(v) => validate_strict_slug_vec(v, "evaluation_axes")?,
        None => Vec::new(),
    };
    let non_goals: Vec<String> = req
        .non_goals
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let eligibility = resolve_eligibility(
        req.eligibility.as_deref(),
        req.access_policy.as_ref(),
        req.visible_to.as_ref(),
    )?;

    store
        .init(namespace)
        .await
        .map_err(|e| format!("failed to init store for '{namespace}': {e}"))?;

    let now_ts = chrono::Utc::now().timestamp();
    let records = store
        .list(namespace, None, Some("thesis"))
        .await
        .map_err(|e| format!("failed to list thesis rows in '{namespace}': {e}"))?;

    let mut active_theses: Vec<(u64, i64, String, MemoryPayload)> = Vec::new();
    for r in records {
        if is_active_non_conflicting(&r.payload, now_ts) {
            let ver = r
                .payload
                .metadata
                .get("thesis_version")
                .and_then(|v| v.as_u64())
                .unwrap_or(1);
            let vf = r
                .payload
                .metadata
                .get("valid_from")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            active_theses.push((ver, vf, r.id, r.payload));
        }
    }
    active_theses.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));

    let mut expected_heads: Vec<ExpectedHead> = Vec::new();
    let mut writes: Vec<StagedMemoryWrite> = Vec::new();

    let (next_ver, superseded_id) = if let Some((last_ver, _vf, last_id, _last_pl)) =
        active_theses.last()
    {
        (last_ver + 1, Some(last_id.clone()))
    } else {
        (1u64, None)
    };
    let new_id = format!("thesis-v{next_ver}");
    expected_heads.push(ExpectedHead::AbsentOrInactive {
        row_id: new_id.clone(),
    });

    for (_ver, _vf, old_id, mut old_payload) in active_theses {
        if !crate::rebac::can_execute_action_on_row(
            principal,
            &old_payload.user_id,
            "editor",
            &old_payload.metadata,
        ) {
            return Err(format!(
                "permission denied: principal '{principal}' cannot supersede existing thesis '{old_id}'"
            ));
        }
        let expected_fp = crate::rdf::scenario::fingerprint_live_row(Some(&old_payload), now_ts);
        expected_heads.push(ExpectedHead::RowFingerprint {
            row_id: old_id.clone(),
            expected_fingerprint: expected_fp,
        });
        if !old_payload.metadata.is_object() {
            old_payload.metadata = json!({});
        }
        if let Some(meta) = old_payload.metadata.as_object_mut() {
            meta.insert("superseded_by".to_string(), json!(new_id));
            meta.insert("valid_to".to_string(), json!(now_ts));
        }
        let old_vec = embedder
            .embed(&old_payload.content)
            .await
            .unwrap_or_else(|_| vec![0.0; embedder.dimensions()]);
        writes.push(StagedMemoryWrite {
            id: old_id,
            vector: old_vec,
            payload: old_payload,
        });
    }

    let thesis_spec = ThesisSpec {
        primary_domain: primary_domain.clone(),
        mission: mission.clone(),
        direction: direction.clone(),
        core_pillars: core_pillars.clone(),
        adjacent_domains: adjacent_domains.clone(),
        evaluation_axes: evaluation_axes.clone(),
        non_goals: non_goals.clone(),
    };

    let content = req
        .content
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!(
                "Project Thesis [{primary_domain}]: {mission} | Direction: {direction} | Core Pillars: {}",
                core_pillars.join(", ")
            )
        });

    let mut meta = json!({
        "thesis": {
            "primary_domain": primary_domain,
            "mission": mission,
            "direction": direction,
            "core_pillars": core_pillars,
            "adjacent_domains": adjacent_domains,
            "evaluation_axes": evaluation_axes,
            "non_goals": non_goals,
            "project_purpose": thesis_spec.mission,
            "domain_summary": thesis_spec.primary_domain,
            "target_outcomes": thesis_spec.core_pillars,
            "architectural_tenets": [thesis_spec.direction.clone()],
            "canonical_concepts": thesis_spec.core_pillars,
        },
        "thesis_version": next_ver,
        "eligibility": eligibility,
        "valid_from": now_ts,
        "updated_at": now_ts,
    });
    if let Some(sup_id) = &superseded_id {
        meta["supersedes"] = json!(sup_id);
    }
    apply_access_controls_to_metadata(
        &mut meta,
        principal,
        req.access_policy.as_ref(),
        req.visible_to.as_ref(),
    );

    let new_payload = MemoryPayload {
        content: content.clone(),
        user_id: principal.to_string(),
        memory_type: "thesis".to_string(),
        agent_name: Some("neurostrata_taxonomy".to_string()),
        location: String::new(),
        location_lines: String::new(),
        metadata: meta,
    };
    let vec = embedder
        .embed(&content)
        .await
        .unwrap_or_else(|_| vec![0.0; embedder.dimensions()]);
    writes.push(StagedMemoryWrite {
        id: new_id.clone(),
        vector: vec,
        payload: new_payload,
    });

    store
        .commit_action_batch(
            namespace,
            ActionWriteSet {
                expected_heads,
                writes,
                edge_deltas: Vec::new(),
            },
        )
        .await
        .map_err(|e| match e {
            ActionError::ConflictWithNewerVersion {
                receipt_id,
                superseded_by,
            } => format!("CAS conflict while setting thesis on '{receipt_id}' ({superseded_by})"),
            other => other.to_string(),
        })?;

    store.refresh_ontology(namespace).await;

    Ok(json!({
        "status": "ok",
        "operation": "set_thesis",
        "namespace": namespace,
        "thesis_id": new_id,
        "superseded_id": superseded_id,
        "thesis": thesis_spec,
    }))
}

async fn execute_upsert_concept(
    store: &LadybugStore,
    embedder: &dyn Embedder,
    namespace: &str,
    req: &TaxonomyRequest,
    principal: &str,
) -> Result<Value, String> {
    let raw_slug = req
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "slug is required for upsert_concept".to_string())?;
    let slug = domain::normalize_domain_slug(raw_slug).ok_or_else(|| {
        format!("invalid concept slug '{raw_slug}': must normalize to lowercase-kebab")
    })?;
    let pref_label = req
        .pref_label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "pref_label is required for upsert_concept".to_string())?
        .to_string();
    let definition = req
        .definition
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "definition is required for upsert_concept".to_string())?
        .to_string();

    let domain_slug = match req.domain.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(d) => Some(
            domain::normalize_domain_slug(d)
                .ok_or_else(|| format!("invalid domain slug '{d}'"))?,
        ),
        None => None,
    };

    let broader = match &req.broader {
        Some(v) => validate_strict_slug_vec(v, "broader")?,
        None => Vec::new(),
    };
    let related_concepts = match &req.related_concepts {
        Some(v) => validate_strict_slug_vec(v, "related_concepts")?,
        None => Vec::new(),
    };
    let contrasts_with = match &req.contrasts_with {
        Some(v) => validate_strict_slug_vec(v, "contrasts_with")?,
        None => Vec::new(),
    };

    let realization_status = req
        .realization_status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string();
    if !matches!(
        realization_status.as_str(),
        "unknown" | "unimplemented" | "partial" | "implemented" | "external_only"
    ) {
        return Err(format!(
            "invalid realization_status '{realization_status}': expected unknown|unimplemented|partial|implemented|external_only"
        ));
    }

    let comparable_implementations = req.comparable_implementations.clone().unwrap_or_default();
    for (idx, comp) in comparable_implementations.iter().enumerate() {
        if comp.name.trim().is_empty() {
            return Err(format!(
                "comparable_implementations[{idx}].name must be non-empty"
            ));
        }
        if !matches!(
            comp.kind.as_str(),
            "library" | "standard" | "reference_system"
        ) {
            return Err(format!(
                "comparable_implementations[{idx}].kind '{}' must be library|standard|reference_system",
                comp.kind
            ));
        }
        if comp.license.trim().is_empty() {
            return Err(format!(
                "comparable_implementations[{idx}].license (SPDX identifier) must be non-empty"
            ));
        }
        if comp.reference.trim().is_empty() {
            return Err(format!(
                "comparable_implementations[{idx}].reference must be non-empty"
            ));
        }
    }

    let mut evaluation_scores = BTreeMap::new();
    if let Some(scores) = &req.evaluation_scores {
        for (axis, &val) in scores {
            let norm_axis = domain::normalize_domain_slug(axis)
                .ok_or_else(|| format!("invalid evaluation_scores axis slug '{axis}'"))?;
            if val > 10000 {
                return Err(format!(
                    "evaluation_scores['{axis}'] = {val} exceeds maximum 10000 bps"
                ));
            }
            evaluation_scores.insert(norm_axis, val);
        }
    }

    let eligibility = resolve_eligibility(
        req.eligibility.as_deref(),
        req.access_policy.as_ref(),
        req.visible_to.as_ref(),
    )?;

    store
        .init(namespace)
        .await
        .map_err(|e| format!("failed to init store for '{namespace}': {e}"))?;

    let now_ts = chrono::Utc::now().timestamp();
    let (active_rows, _active_edges) = load_active_rows_and_edges(store, namespace, now_ts).await?;

    let implemented_interfaces = req.implemented_interfaces.clone().unwrap_or_default();
    if !implemented_interfaces.is_empty() {
        let known_ifaces = collect_active_interface_names(&active_rows);
        for iface in &implemented_interfaces {
            if !known_ifaces.contains(iface.trim()) {
                return Err(format!(
                    "referential integrity error: implemented_interface '{iface}' does not match any active interface in namespace '{namespace}'"
                ));
            }
        }
    }

    let linked_action_types = req.linked_action_types.clone().unwrap_or_default();
    if !linked_action_types.is_empty() {
        let known_actions = collect_active_action_slugs(&active_rows);
        for act in &linked_action_types {
            if !known_actions.contains(act.trim()) {
                return Err(format!(
                    "referential integrity error: linked_action_type '{act}' does not match any active action_type in namespace '{namespace}'"
                ));
            }
        }
    }

    let aliases: Vec<String> = req
        .aliases
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let tradeoffs: Vec<String> = req
        .tradeoffs
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let references: Vec<String> = req
        .references
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let current_limitations: Vec<String> = req
        .current_limitations
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let frontier_opportunities: Vec<String> = req
        .frontier_opportunities
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let evidence_refs: Vec<String> = req
        .evidence_refs
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let concept_iri = req
        .concept_iri
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let candidate_spec = ConceptSpec {
        slug: slug.clone(),
        pref_label: pref_label.clone(),
        definition: definition.clone(),
        domain: domain_slug.clone(),
        aliases: aliases.clone(),
        broader: broader.clone(),
        related_concepts: related_concepts.clone(),
        contrasts_with: contrasts_with.clone(),
        tradeoffs: tradeoffs.clone(),
        references: references.clone(),
        comparable_implementations: comparable_implementations.clone(),
        realization_status: realization_status.clone(),
        current_limitations: current_limitations.clone(),
        frontier_opportunities: frontier_opportunities.clone(),
        implemented_interfaces: implemented_interfaces.clone(),
        linked_action_types: linked_action_types.clone(),
        evaluation_scores: evaluation_scores.clone(),
        evidence_refs: evidence_refs.clone(),
        concept_iri: concept_iri.clone(),
    };

    // Collect existing active concept rows in namespace and check global DAG + SKOS S27 disjointness
    let mut existing_for_slug: Vec<(u64, i64, String, MemoryPayload)> = Vec::new();
    let mut candidate_forest: BTreeMap<String, ConceptSpec> = BTreeMap::new();
    for (rid, pl) in &active_rows {
        if pl.memory_type == "concept" {
            if let Some(spec) = ConceptSpec::from_payload(pl) {
                if spec.slug == slug {
                    let ver = pl
                        .metadata
                        .get("concept_version")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(1);
                    let vf = pl
                        .metadata
                        .get("valid_from")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    existing_for_slug.push((ver, vf, rid.clone(), pl.clone()));
                } else {
                    candidate_forest.insert(spec.slug.clone(), spec);
                }
            }
        }
    }
    candidate_forest.insert(slug.clone(), candidate_spec.clone());
    validate_concept_forest(&candidate_forest)?;

    existing_for_slug.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let (next_ver, superseded_id) = if let Some((last_ver, _vf, last_id, _pl)) =
        existing_for_slug.last()
    {
        (last_ver + 1, Some(last_id.clone()))
    } else {
        (1u64, None)
    };
    let new_id = format!("concept-{slug}-v{next_ver}");

    let mut expected_heads: Vec<ExpectedHead> = vec![ExpectedHead::AbsentOrInactive {
        row_id: new_id.clone(),
    }];
    let mut writes: Vec<StagedMemoryWrite> = Vec::new();

    for (_ver, _vf, old_id, mut old_payload) in existing_for_slug {
        if !crate::rebac::can_execute_action_on_row(
            principal,
            &old_payload.user_id,
            "editor",
            &old_payload.metadata,
        ) {
            return Err(format!(
                "permission denied: principal '{principal}' cannot supersede existing concept '{old_id}' ('{slug}')"
            ));
        }
        let expected_fp = crate::rdf::scenario::fingerprint_live_row(Some(&old_payload), now_ts);
        expected_heads.push(ExpectedHead::RowFingerprint {
            row_id: old_id.clone(),
            expected_fingerprint: expected_fp,
        });
        if !old_payload.metadata.is_object() {
            old_payload.metadata = json!({});
        }
        if let Some(meta) = old_payload.metadata.as_object_mut() {
            meta.insert("superseded_by".to_string(), json!(new_id));
            meta.insert("valid_to".to_string(), json!(now_ts));
        }
        let old_vec = embedder
            .embed(&old_payload.content)
            .await
            .unwrap_or_else(|_| vec![0.0; embedder.dimensions()]);
        writes.push(StagedMemoryWrite {
            id: old_id,
            vector: old_vec,
            payload: old_payload,
        });
    }

    let content = req
        .content
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{pref_label} ({slug}): {definition}"));

    let governs = req.governs.clone().unwrap_or_default();
    let related_to = req.related_to.clone().unwrap_or_default();
    let extracted_from = req.extracted_from.clone().unwrap_or_default();

    let mut meta = json!({
        "concept": {
            "slug": slug,
            "pref_label": pref_label,
            "definition": definition,
            "domain": domain_slug,
            "aliases": aliases,
            "alt_labels": aliases,
            "broader": broader,
            "related_concepts": related_concepts,
            "related": related_concepts,
            "contrasts_with": contrasts_with,
            "tradeoffs": tradeoffs,
            "references": references,
            "comparable_implementations": comparable_implementations,
            "realization_status": realization_status,
            "current_limitations": current_limitations,
            "frontier_opportunities": frontier_opportunities,
            "implemented_interfaces": implemented_interfaces,
            "linked_action_types": linked_action_types,
            "evaluation_scores": evaluation_scores,
            "evidence_refs": evidence_refs,
            "concept_iri": concept_iri,
        },
        "concept_version": next_ver,
        "eligibility": eligibility,
        "valid_from": now_ts,
        "updated_at": now_ts,
    });
    if let Some(d) = &domain_slug {
        meta["domain"] = json!(d);
    }
    if let Some(iri) = &concept_iri {
        meta["concept_iri"] = json!(iri);
    }
    if !governs.is_empty() {
        meta["governs"] = json!(governs);
    }
    if !related_to.is_empty() {
        meta["related_to"] = json!(related_to);
    }
    if !extracted_from.is_empty() {
        meta["extracted_from"] = json!(extracted_from);
    }
    if let Some(locs) = &req.locations {
        if !locs.is_empty() {
            meta["locations"] = json!(locs);
        }
    }
    if let Some(sup_id) = &superseded_id {
        meta["supersedes"] = json!(sup_id);
    }
    apply_access_controls_to_metadata(
        &mut meta,
        principal,
        req.access_policy.as_ref(),
        req.visible_to.as_ref(),
    );

    let first_location = req
        .locations
        .as_ref()
        .and_then(|arr| arr.first())
        .and_then(|v| {
            v.as_str()
                .or_else(|| v.get("path").and_then(|p| p.as_str()))
        })
        .unwrap_or("")
        .to_string();

    let new_payload = MemoryPayload {
        content: content.clone(),
        user_id: principal.to_string(),
        memory_type: "concept".to_string(),
        agent_name: Some("neurostrata_taxonomy".to_string()),
        location: first_location,
        location_lines: String::new(),
        metadata: meta,
    };
    let vec = embedder
        .embed(&content)
        .await
        .unwrap_or_else(|_| vec![0.0; embedder.dimensions()]);
    writes.push(StagedMemoryWrite {
        id: new_id.clone(),
        vector: vec,
        payload: new_payload,
    });

    let active_row_ids: BTreeSet<String> = active_rows.iter().map(|(id, _)| id.clone()).collect();
    let mut edge_deltas: Vec<EdgeDelta> = Vec::new();
    for gov_target in &governs {
        if !gov_target.trim().is_empty() {
            edge_deltas.push(EdgeDelta {
                source: new_id.clone(),
                relation: "GOVERNS".to_string(),
                target: gov_target.trim().to_string(),
                existed_before: false,
                op: EdgeMutationOp::EnsurePresent,
            });
        }
    }
    for rel_target in &related_to {
        if !rel_target.trim().is_empty() {
            edge_deltas.push(EdgeDelta {
                source: new_id.clone(),
                relation: "RELATES_TO".to_string(),
                target: rel_target.trim().to_string(),
                existed_before: false,
                op: EdgeMutationOp::EnsurePresent,
            });
        }
    }
    for ext_target in &extracted_from {
        if !ext_target.trim().is_empty() {
            edge_deltas.push(EdgeDelta {
                source: new_id.clone(),
                relation: "EXTRACTED_FROM".to_string(),
                target: ext_target.trim().to_string(),
                existed_before: false,
                op: EdgeMutationOp::EnsurePresent,
            });
        }
    }
    for ev_target in &evidence_refs {
        if active_row_ids.contains(ev_target.trim()) {
            edge_deltas.push(EdgeDelta {
                source: new_id.clone(),
                relation: "RELATES_TO".to_string(),
                target: ev_target.trim().to_string(),
                existed_before: false,
                op: EdgeMutationOp::EnsurePresent,
            });
        }
    }

    store
        .commit_action_batch(
            namespace,
            ActionWriteSet {
                expected_heads,
                writes,
                edge_deltas,
            },
        )
        .await
        .map_err(|e| match e {
            ActionError::ConflictWithNewerVersion {
                receipt_id,
                superseded_by,
            } => format!(
                "CAS conflict while upserting concept '{slug}' on '{receipt_id}' ({superseded_by})"
            ),
            other => other.to_string(),
        })?;

    store.refresh_ontology(namespace).await;

    let (post_rows, post_edges) = load_active_rows_and_edges(store, namespace, now_ts).await?;
    let view = build_principal_taxonomy_view(&post_rows, &post_edges, principal);
    let hydrated = view.concepts.get(&slug).cloned().ok_or_else(|| {
        format!("concept '{slug}' was written but could not be hydrated for principal '{principal}'")
    })?;

    Ok(json!({
        "status": "ok",
        "operation": "upsert_concept",
        "namespace": namespace,
        "concept_id": new_id,
        "superseded_id": superseded_id,
        "slug": slug,
        "epistemic_status": hydrated.epistemic_status,
        "epistemic_weight_bps": hydrated.epistemic_weight_bps,
        "frontier_leverage_bps": hydrated.frontier_leverage_bps,
        "concept": hydrated,
    }))
}

fn build_tree_node(
    slug: &str,
    concepts: &BTreeMap<String, HydratedConcept>,
    allowed_set: &BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> Option<TaxonomyTreeNode> {
    if !visited.insert(slug.to_string()) {
        return None;
    }
    let c = concepts.get(slug)?;
    let mut children = Vec::new();
    for child_slug in &c.narrower {
        if allowed_set.contains(child_slug) && !visited.contains(child_slug) {
            if let Some(child_node) =
                build_tree_node(child_slug, concepts, allowed_set, visited)
            {
                children.push(child_node);
            }
        }
    }
    Some(TaxonomyTreeNode {
        slug: c.slug.clone(),
        pref_label: c.pref_label.clone(),
        definition: c.definition.clone(),
        domain: c.domain.clone(),
        epistemic_status: c.epistemic_status.clone(),
        epistemic_weight_bps: c.epistemic_weight_bps,
        realization_status: c.realization_status.clone(),
        frontier_leverage_bps: c.frontier_leverage_bps,
        descendant_count: c.descendant_count,
        grounding_count: c.grounding_count,
        aliases: c.aliases.clone(),
        broader: c.broader.clone(),
        narrower: c.narrower.clone(),
        related_concepts: c.related_concepts.clone(),
        contrasts_with: c.contrasts_with.clone(),
        children,
    })
}

async fn execute_tree(
    store: &LadybugStore,
    namespace: &str,
    req: &TaxonomyRequest,
    principal: &str,
) -> Result<Value, String> {
    let now_ts = chrono::Utc::now().timestamp();
    let (active_rows, active_edges) = load_active_rows_and_edges(store, namespace, now_ts).await?;
    let view = build_principal_taxonomy_view(&active_rows, &active_edges, principal);

    let domain_filter = req
        .domain
        .as_deref()
        .and_then(domain::normalize_domain_slug);
    let root_filter = req
        .root_slug
        .as_deref()
        .and_then(domain::normalize_domain_slug);

    let allowed_slugs: BTreeSet<String> = if let Some(ref dom) = domain_filter {
        let mut set = BTreeSet::new();
        for (slug, c) in &view.concepts {
            if slug == dom || c.domain.as_deref() == Some(dom.as_str()) {
                set.insert(slug.clone());
                // Include transitive narrower concepts under this domain root
                let mut q: VecDeque<String> = c.narrower.iter().cloned().collect();
                while let Some(child) = q.pop_front() {
                    if view.concepts.contains_key(&child) && set.insert(child.clone()) {
                        if let Some(cc) = view.concepts.get(&child) {
                            q.extend(cc.narrower.iter().cloned());
                        }
                    }
                }
            }
        }
        set
    } else {
        view.concepts.keys().cloned().collect()
    };

    let root_slugs: Vec<String> = if let Some(ref r_slug) = root_filter {
        if allowed_slugs.contains(r_slug) {
            vec![r_slug.clone()]
        } else {
            Vec::new()
        }
    } else {
        allowed_slugs
            .iter()
            .filter(|slug| {
                view.concepts.get(*slug).map_or(false, |c| {
                    c.broader.is_empty()
                        || !c.broader.iter().any(|p| allowed_slugs.contains(p))
                })
            })
            .cloned()
            .collect()
    };

    let mut visited = BTreeSet::new();
    let mut roots = Vec::new();
    for r_slug in root_slugs {
        if let Some(node) =
            build_tree_node(&r_slug, &view.concepts, &allowed_slugs, &mut visited)
        {
            roots.push(node);
        }
    }

    Ok(json!({
        "status": "ok",
        "operation": "tree",
        "namespace": namespace,
        "thesis": view.thesis,
        "roots": roots,
        "total_concepts": allowed_slugs.len(),
    }))
}

fn tokenize_query(raw: &str) -> Vec<String> {
    raw.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s.len() >= 2)
        .collect()
}

fn score_concept_match(c: &HydratedConcept, query_raw: &str, tokens: &[String]) -> u32 {
    if query_raw.trim().is_empty() && tokens.is_empty() {
        return 1;
    }
    let q_lower = query_raw.trim().to_lowercase();
    let mut score: u32 = 0;

    if c.slug == q_lower || c.pref_label.to_lowercase() == q_lower {
        score += 1000;
    }
    for alias in &c.aliases {
        if alias.to_lowercase() == q_lower {
            score += 900;
        }
    }

    let hay_slug = c.slug.to_lowercase();
    let hay_label = c.pref_label.to_lowercase();
    let hay_aliases: Vec<String> = c.aliases.iter().map(|a| a.to_lowercase()).collect();
    let hay_def = c.definition.to_lowercase();
    let hay_dom = c.domain.as_deref().unwrap_or("").to_lowercase();

    for tok in tokens {
        if hay_slug == *tok || hay_slug.split('-').any(|p| p == tok) {
            score += 200;
        } else if hay_slug.contains(tok) {
            score += 100;
        }
        if hay_label.contains(tok) {
            score += 150;
        }
        if hay_aliases.iter().any(|a| a == tok || a.contains(tok)) {
            score += 180;
        }
        if hay_dom == *tok || hay_dom.contains(tok) {
            score += 80;
        }
        if hay_def.contains(tok) {
            score += 40;
        }
    }

    score
}

async fn execute_briefing(
    store: &LadybugStore,
    namespace: &str,
    req: &TaxonomyRequest,
    principal: &str,
) -> Result<Value, String> {
    let now_ts = chrono::Utc::now().timestamp();
    let (active_rows, active_edges) = load_active_rows_and_edges(store, namespace, now_ts).await?;
    let view = build_principal_taxonomy_view(&active_rows, &active_edges, principal);

    let max_hops = req.max_hops.unwrap_or(2).clamp(1, 4);
    let limit = req.limit.unwrap_or(8).clamp(1, 50);
    let domain_filter = req
        .domain
        .as_deref()
        .and_then(domain::normalize_domain_slug);
    let query_str = req
        .query
        .as_deref()
        .or(req.topic.as_deref())
        .unwrap_or("")
        .trim();
    let tokens = tokenize_query(query_str);

    let mut scored_seeds: Vec<(u32, u32, usize, String)> = Vec::new();
    for (slug, c) in &view.concepts {
        if let Some(ref dom) = domain_filter {
            if slug != dom && c.domain.as_deref() != Some(dom.as_str()) {
                continue;
            }
        }
        let match_score = score_concept_match(c, query_str, &tokens);
        if match_score > 0 {
            scored_seeds.push((
                match_score,
                c.epistemic_weight_bps,
                c.grounding_count,
                slug.clone(),
            ));
        }
    }

    scored_seeds.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    scored_seeds.truncate(limit);

    let seed_slugs: BTreeSet<String> = scored_seeds.into_iter().map(|(_, _, _, s)| s).collect();
    let matched_concepts: Vec<HydratedConcept> = seed_slugs
        .iter()
        .filter_map(|s| view.concepts.get(s).cloned())
        .collect();

    // Forward-Expand (BFS 1..=max_hops across broader, narrower, related_concepts, contrasts_with)
    let mut hop_by_slug: BTreeMap<String, usize> = BTreeMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    for s in &seed_slugs {
        hop_by_slug.insert(s.clone(), 0);
        queue.push_back((s.clone(), 0));
    }

    while let Some((curr_slug, curr_hop)) = queue.pop_front() {
        if curr_hop >= max_hops {
            continue;
        }
        let Some(curr_c) = view.concepts.get(&curr_slug) else {
            continue;
        };
        let neighbors = curr_c
            .broader
            .iter()
            .chain(curr_c.narrower.iter())
            .chain(curr_c.related_concepts.iter())
            .chain(curr_c.contrasts_with.iter());
        for nb in neighbors {
            if view.concepts.contains_key(nb) && !hop_by_slug.contains_key(nb) {
                hop_by_slug.insert(nb.clone(), curr_hop + 1);
                queue.push_back((nb.clone(), curr_hop + 1));
            }
        }
    }

    // Build undirected adjacency within the expanded subgraph to identify outer-hop leaf nodes
    let mut subgraph_neighbors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for slug in hop_by_slug.keys() {
        if let Some(c) = view.concepts.get(slug) {
            for nb in c
                .broader
                .iter()
                .chain(c.narrower.iter())
                .chain(c.related_concepts.iter())
                .chain(c.contrasts_with.iter())
            {
                if hop_by_slug.contains_key(nb) && nb != slug {
                    subgraph_neighbors
                        .entry(slug.clone())
                        .or_default()
                        .insert(nb.clone());
                    subgraph_neighbors
                        .entry(nb.clone())
                        .or_default()
                        .insert(slug.clone());
                }
            }
        }
    }

    // Backward-Prune: remove outer-hop (`hop == max_hops`) non-seed nodes that have degree <= 1
    // in the expanded subgraph (do not bridge two seed/inner nodes) AND have zero active governing
    // rule/fact rows (`verified_grounding_ids.is_empty()`).
    let mut pruned_outer_slugs: BTreeSet<String> = BTreeSet::new();
    for (slug, &hop) in &hop_by_slug {
        if hop == max_hops && !seed_slugs.contains(slug) {
            let deg = subgraph_neighbors.get(slug).map_or(0, |s| s.len());
            let has_active_grounding = view
                .concepts
                .get(slug)
                .map_or(false, |c| !c.verified_grounding_ids.is_empty());
            if deg <= 1 && !has_active_grounding {
                pruned_outer_slugs.insert(slug.clone());
            }
        }
    }

    let retained_slugs: BTreeSet<String> = hop_by_slug
        .keys()
        .filter(|s| !pruned_outer_slugs.contains(*s))
        .cloned()
        .collect();

    let mut subgraph_nodes: Vec<Value> = Vec::new();
    let mut subgraph_edges: Vec<Value> = Vec::new();
    for slug in &retained_slugs {
        if let Some(c) = view.concepts.get(slug) {
            let hop = hop_by_slug.get(slug).copied().unwrap_or(0);
            subgraph_nodes.push(json!({
                "slug": c.slug,
                "pref_label": c.pref_label,
                "hop": hop,
                "is_seed": seed_slugs.contains(slug),
                "epistemic_status": c.epistemic_status,
                "realization_status": c.realization_status,
                "grounding_count": c.grounding_count,
            }));
            for p in &c.broader {
                if retained_slugs.contains(p) {
                    subgraph_edges.push(json!({
                        "source": c.slug,
                        "relation": "skos:broader",
                        "target": p,
                    }));
                }
            }
            for r in &c.related_concepts {
                if retained_slugs.contains(r) && c.slug < *r {
                    subgraph_edges.push(json!({
                        "source": c.slug,
                        "relation": "skos:related",
                        "target": r,
                    }));
                }
            }
            for cw in &c.contrasts_with {
                if retained_slugs.contains(cw) && c.slug < *cw {
                    subgraph_edges.push(json!({
                        "source": c.slug,
                        "relation": "ns:contrastsWith",
                        "target": cw,
                    }));
                }
            }
        }
    }

    // Collect principal-visible linked normative rows (rules, facts, procedures)
    let mut linked_ids: BTreeSet<String> = BTreeSet::new();
    for slug in &retained_slugs {
        if let Some(c) = view.concepts.get(slug) {
            for gid in &c.verified_grounding_ids {
                linked_ids.insert(gid.clone());
            }
        }
    }
    let visible_by_id: BTreeMap<String, &MemoryPayload> = view
        .visible_rows
        .iter()
        .map(|(id, pl)| (id.clone(), pl))
        .collect();
    let mut linked_knowledge: Vec<Value> = Vec::new();
    for lid in linked_ids {
        if let Some(pl) = visible_by_id.get(&lid) {
            linked_knowledge.push(json!({
                "id": lid,
                "memory_type": pl.memory_type,
                "content": pl.content,
                "location": pl.location,
            }));
        }
    }

    let mut epistemic_gaps: Vec<Value> = Vec::new();
    for slug in &retained_slugs {
        if let Some(c) = view.concepts.get(slug) {
            if matches!(c.epistemic_status.as_str(), "seed" | "defined")
                || matches!(c.realization_status.as_str(), "unimplemented" | "partial")
            {
                epistemic_gaps.push(json!({
                    "slug": c.slug,
                    "epistemic_status": c.epistemic_status,
                    "realization_status": c.realization_status,
                    "current_limitations": c.current_limitations,
                }));
            }
        }
    }

    Ok(json!({
        "status": "ok",
        "operation": "briefing",
        "namespace": namespace,
        "thesis": view.thesis,
        "matched_concepts": matched_concepts,
        "neighborhood_subgraph": {
            "max_hops": max_hops,
            "nodes": subgraph_nodes,
            "edges": subgraph_edges,
            "pruned_outer_leaf_slugs": pruned_outer_slugs.into_iter().collect::<Vec<_>>(),
        },
        "linked_knowledge": linked_knowledge,
        "epistemic_gaps": epistemic_gaps,
    }))
}

fn transitive_ancestors_of(
    slug: &str,
    concepts: &BTreeMap<String, HydratedConcept>,
) -> BTreeSet<String> {
    let mut ancestors = BTreeSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    if let Some(c) = concepts.get(slug) {
        queue.extend(c.broader.iter().cloned());
    }
    while let Some(p) = queue.pop_front() {
        if ancestors.insert(p.clone()) {
            if let Some(pc) = concepts.get(&p) {
                for gp in &pc.broader {
                    if !ancestors.contains(gp) {
                        queue.push_back(gp.clone());
                    }
                }
            }
        }
    }
    ancestors
}

async fn execute_compare(
    store: &LadybugStore,
    namespace: &str,
    req: &TaxonomyRequest,
    principal: &str,
) -> Result<Value, String> {
    let raw_slugs = req
        .slugs
        .as_ref()
        .ok_or_else(|| "slugs (array of >= 2 concept slugs) is required for compare".to_string())?;
    let slugs = validate_strict_slug_vec(raw_slugs, "slugs")?;
    if slugs.len() < 2 {
        return Err("compare requires at least 2 distinct concept slugs".to_string());
    }

    let now_ts = chrono::Utc::now().timestamp();
    let (active_rows, active_edges) = load_active_rows_and_edges(store, namespace, now_ts).await?;
    let view = build_principal_taxonomy_view(&active_rows, &active_edges, principal);

    let mut compared_concepts: Vec<HydratedConcept> = Vec::new();
    for s in &slugs {
        let Some(c) = view.concepts.get(s) else {
            return Err(format!(
                "concept '{s}' not found or access denied for principal '{principal}'"
            ));
        };
        compared_concepts.push(c.clone());
    }

    // Shared ancestors (intersection of transitive broader ancestors)
    let mut shared_ancestors_set: Option<BTreeSet<String>> = None;
    for s in &slugs {
        let anc = transitive_ancestors_of(s, &view.concepts);
        shared_ancestors_set = Some(match shared_ancestors_set {
            None => anc,
            Some(prev) => prev.intersection(&anc).cloned().collect(),
        });
    }
    let shared_ancestors: Vec<String> = shared_ancestors_set
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Direct contrasts and 1..=2 hop bridging relations
    let mut direct_contrasts: Vec<Value> = Vec::new();
    let mut bridging_relations: Vec<Value> = Vec::new();
    for i in 0..compared_concepts.len() {
        for j in (i + 1)..compared_concepts.len() {
            let a = &compared_concepts[i];
            let b = &compared_concepts[j];
            if a.contrasts_with.contains(&b.slug) || b.contrasts_with.contains(&a.slug) {
                direct_contrasts.push(json!({
                    "concept_a": a.slug,
                    "concept_b": b.slug,
                    "relation": "ns:contrastsWith",
                }));
            }
            if a.related_concepts.contains(&b.slug) || b.related_concepts.contains(&a.slug) {
                bridging_relations.push(json!({
                    "concept_a": a.slug,
                    "concept_b": b.slug,
                    "hops": 1,
                    "relation": "skos:related",
                }));
            }
            // 2-hop intermediate concept bridges
            let a_neighbors: BTreeSet<&String> = a
                .related_concepts
                .iter()
                .chain(a.contrasts_with.iter())
                .chain(a.broader.iter())
                .chain(a.narrower.iter())
                .collect();
            let b_neighbors: BTreeSet<&String> = b
                .related_concepts
                .iter()
                .chain(b.contrasts_with.iter())
                .chain(b.broader.iter())
                .chain(b.narrower.iter())
                .collect();
            for mid in a_neighbors.intersection(&b_neighbors) {
                if *mid != &a.slug && *mid != &b.slug && view.concepts.contains_key(*mid) {
                    bridging_relations.push(json!({
                        "concept_a": a.slug,
                        "intermediate": *mid,
                        "concept_b": b.slug,
                        "hops": 2,
                    }));
                }
            }
        }
    }

    // Determine evaluation axes
    let axes: Vec<String> = if let Some(req_axes) = &req.axes {
        validate_strict_slug_vec(req_axes, "axes")?
    } else if let Some(ref t) = view.thesis {
        if !t.evaluation_axes.is_empty() {
            t.evaluation_axes.clone()
        } else {
            let mut union_axes = BTreeSet::new();
            for c in &compared_concepts {
                union_axes.extend(c.evaluation_scores.keys().cloned());
            }
            union_axes.into_iter().collect()
        }
    } else {
        let mut union_axes = BTreeSet::new();
        for c in &compared_concepts {
            union_axes.extend(c.evaluation_scores.keys().cloned());
        }
        union_axes.into_iter().collect()
    };

    let mut axis_matrix: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for axis in &axes {
        let mut entries = Vec::new();
        for c in &compared_concepts {
            entries.push(json!({
                "slug": c.slug,
                "pref_label": c.pref_label,
                "score_bps": c.evaluation_scores.get(axis).copied(),
                "realization_status": c.realization_status,
                "epistemic_status": c.epistemic_status,
                "grounding_count": c.grounding_count,
                "tradeoffs": c.tradeoffs,
            }));
        }
        axis_matrix.insert(axis.clone(), entries);
    }

    let mut alignment_ranking: Vec<Value> = compared_concepts
        .iter()
        .map(|c| {
            json!({
                "slug": c.slug,
                "pref_label": c.pref_label,
                "axis_alignment_bps": c.axis_alignment_bps,
                "frontier_leverage_bps": c.frontier_leverage_bps,
                "realization_status": c.realization_status,
                "epistemic_status": c.epistemic_status,
            })
        })
        .collect();
    alignment_ranking.sort_by(|a, b| {
        let sa = a["axis_alignment_bps"].as_u64().unwrap_or(0);
        let sb = b["axis_alignment_bps"].as_u64().unwrap_or(0);
        sb.cmp(&sa)
    });

    let perspectives = json!({
        "theoretical_lineage": {
            "shared_ancestors": shared_ancestors,
            "concepts": compared_concepts.iter().map(|c| json!({
                "slug": c.slug,
                "domain": c.domain,
                "broader": c.broader,
                "references": c.references,
            })).collect::<Vec<_>>(),
        },
        "operational_tradeoffs": {
            "direct_contrasts": direct_contrasts,
            "concepts": compared_concepts.iter().map(|c| json!({
                "slug": c.slug,
                "tradeoffs": c.tradeoffs,
                "current_limitations": c.current_limitations,
                "comparable_implementations": c.comparable_implementations,
            })).collect::<Vec<_>>(),
        },
        "thesis_alignment": {
            "evaluation_axes": axes,
            "ranking": alignment_ranking,
        },
    });

    Ok(json!({
        "status": "ok",
        "operation": "compare",
        "namespace": namespace,
        "concepts": compared_concepts,
        "shared_ancestors": shared_ancestors,
        "direct_contrasts": direct_contrasts,
        "bridging_relations": bridging_relations,
        "axis_matrix": axis_matrix,
        "perspectives": perspectives,
    }))
}

pub fn compute_gaps_from_view(view: &PrincipalTaxonomyView) -> Value {
    let mut n_seed: usize = 0;
    let mut n_defined: usize = 0;
    let mut n_synthesized: usize = 0;
    let mut n_verified: usize = 0;
    let mut statuses: Vec<EpistemicStatus> = Vec::new();
    let mut shallow_concepts: Vec<String> = Vec::new();

    for (slug, c) in &view.concepts {
        match c.epistemic_status.as_str() {
            "seed" => {
                n_seed += 1;
                statuses.push(EpistemicStatus::Seed);
                shallow_concepts.push(slug.clone());
            }
            "defined" => {
                n_defined += 1;
                statuses.push(EpistemicStatus::Defined);
                shallow_concepts.push(slug.clone());
            }
            "synthesized" => {
                n_synthesized += 1;
                statuses.push(EpistemicStatus::Synthesized);
            }
            "verified" => {
                n_verified += 1;
                statuses.push(EpistemicStatus::Verified);
            }
            _ => {
                n_seed += 1;
                statuses.push(EpistemicStatus::Seed);
                shallow_concepts.push(slug.clone());
            }
        }
    }

    // Collect candidate domains from visible rows + thesis.core_pillars
    let mut candidate_domains: BTreeSet<String> = BTreeSet::new();
    if let Some(ref t) = view.thesis {
        for p in &t.core_pillars {
            candidate_domains.insert(p.clone());
        }
    }
    for (_id, pl) in &view.visible_rows {
        if let Some(d) = pl.metadata.get("domain").and_then(|v| v.as_str()) {
            if let Some(norm) = domain::normalize_domain_slug(d) {
                candidate_domains.insert(norm);
            }
        } else if let Some(arr) = pl.metadata.get("domain").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str().and_then(domain::normalize_domain_slug) {
                    candidate_domains.insert(s);
                }
            }
        }
        if let Some(reg_arr) = pl
            .metadata
            .get("domain_registry")
            .and_then(|r| r.get("domains"))
            .and_then(|v| v.as_array())
        {
            for item in reg_arr {
                if let Some(s) = item.as_str().and_then(domain::normalize_domain_slug) {
                    candidate_domains.insert(s);
                }
            }
        }
    }

    let mapped_domains: BTreeSet<String> = view
        .concepts
        .values()
        .flat_map(|c| {
            let mut v = vec![c.slug.clone()];
            if let Some(ref d) = c.domain {
                v.push(d.clone());
            }
            v
        })
        .collect();

    let unmapped_domains: Vec<String> = candidate_domains
        .into_iter()
        .filter(|d| !mapped_domains.contains(d))
        .collect();

    let epistemic_coverage_bps =
        compute_epistemic_coverage_bps(&statuses, unmapped_domains.len() as u32);

    // Dangling concept refs: referenced in visible concepts' broader/related/contrasts_with
    // and not in all_active_concept_slugs (so restricted slugs are never leaked as dangling!).
    let mut dangling_set: BTreeSet<String> = BTreeSet::new();
    for spec in view.raw_visible_concept_specs.values() {
        for target in spec
            .broader
            .iter()
            .chain(spec.related_concepts.iter())
            .chain(spec.contrasts_with.iter())
        {
            if !view.all_active_concept_slugs.contains(target)
                && !view.restricted_concept_slugs.contains(target)
            {
                dangling_set.insert(target.clone());
            }
        }
    }
    let dangling_concept_refs: Vec<String> = dangling_set.into_iter().collect();

    // Anti-patterns: OrphanConcept, OverloadedUmbrella, UnanchoredPillar, SymmetricConflict
    let mut anti_patterns: Vec<OntologyAntiPattern> = Vec::new();
    let core_pillars_set: BTreeSet<String> = view
        .thesis
        .as_ref()
        .map(|t| t.core_pillars.iter().cloned().collect())
        .unwrap_or_default();

    for p in &core_pillars_set {
        if !mapped_domains.contains(p) {
            anti_patterns.push(OntologyAntiPattern {
                kind: "UnanchoredPillar".to_string(),
                target: p.clone(),
                message: format!(
                    "Core pillar '{p}' declared in Project Thesis has zero active concept rows"
                ),
            });
        }
    }

    for (slug, c) in &view.concepts {
        let is_pillar = core_pillars_set.contains(slug)
            || c.domain
                .as_ref()
                .map_or(false, |d| core_pillars_set.contains(d));
        if c.broader.is_empty()
            && c.narrower.is_empty()
            && c.related_concepts.is_empty()
            && !is_pillar
        {
            anti_patterns.push(OntologyAntiPattern {
                kind: "OrphanConcept".to_string(),
                target: slug.clone(),
                message: format!(
                    "Concept '{slug}' has no broader, narrower, or related_concepts links and is not anchored to a thesis core_pillar"
                ),
            });
        }
        if c.narrower.len() > 12 {
            anti_patterns.push(OntologyAntiPattern {
                kind: "OverloadedUmbrella".to_string(),
                target: slug.clone(),
                message: format!(
                    "Concept '{slug}' has {} direct narrower children (> 12); consider introducing intermediate sub-concepts",
                    c.narrower.len()
                ),
            });
        }
        // Check legacy raw spec for SymmetricConflict against transitive broader ancestors
        if let Some(raw_spec) = view.raw_visible_concept_specs.get(slug) {
            let anc = transitive_ancestors_of(slug, &view.concepts);
            for r in &raw_spec.related_concepts {
                if anc.contains(r) {
                    anti_patterns.push(OntologyAntiPattern {
                        kind: "SymmetricConflict".to_string(),
                        target: slug.clone(),
                        message: format!(
                            "Concept '{slug}' declares related_concept '{r}' which overlaps its transitive broader hierarchy (SKOS S27)"
                        ),
                    });
                }
            }
            for cw in &raw_spec.contrasts_with {
                if anc.contains(cw) {
                    anti_patterns.push(OntologyAntiPattern {
                        kind: "SymmetricConflict".to_string(),
                        target: slug.clone(),
                        message: format!(
                            "Concept '{slug}' declares contrasts_with '{cw}' which overlaps its transitive broader hierarchy"
                        ),
                    });
                }
            }
        }
    }

    json!({
        "status": "ok",
        "operation": "gaps",
        "thesis_present": view.thesis.is_some(),
        "epistemic_coverage_bps": epistemic_coverage_bps,
        "status_counts": {
            "seed": n_seed,
            "defined": n_defined,
            "synthesized": n_synthesized,
            "verified": n_verified,
        },
        "unmapped_domains": unmapped_domains,
        "dangling_concept_refs": dangling_concept_refs,
        "shallow_concepts": shallow_concepts,
        "anti_patterns": anti_patterns,
    })
}

async fn execute_gaps(
    store: &LadybugStore,
    namespace: &str,
    _req: &TaxonomyRequest,
    principal: &str,
) -> Result<Value, String> {
    let now_ts = chrono::Utc::now().timestamp();
    let (active_rows, active_edges) = load_active_rows_and_edges(store, namespace, now_ts).await?;
    let view = build_principal_taxonomy_view(&active_rows, &active_edges, principal);
    let mut report = compute_gaps_from_view(&view);
    if let Some(obj) = report.as_object_mut() {
        obj.insert("namespace".to_string(), json!(namespace));
    }
    Ok(report)
}

fn is_generic_advisory_stopword(tok: &str) -> bool {
    matches!(
        tok,
        "what"
            | "are"
            | "the"
            | "top"
            | "ways"
            | "way"
            | "we"
            | "could"
            | "can"
            | "should"
            | "would"
            | "improve"
            | "improving"
            | "improvement"
            | "improvements"
            | "project"
            | "system"
            | "architecture"
            | "codebase"
            | "next"
            | "recommend"
            | "recommendation"
            | "recommendations"
            | "advise"
            | "advice"
            | "how"
            | "to"
            | "best"
            | "for"
            | "in"
            | "on"
            | "of"
            | "and"
            | "or"
            | "with"
            | "this"
            | "our"
            | "most"
            | "high"
            | "impact"
            | "leverage"
            | "opportunities"
            | "opportunity"
            | "frontier"
    )
}

fn assess_non_goal_overlap(
    c: &HydratedConcept,
    thesis: Option<&ThesisSpec>,
) -> (String, Vec<String>) {
    let Some(t) = thesis else {
        return ("not_assessed".to_string(), Vec::new());
    };
    if t.non_goals.is_empty() {
        return ("not_assessed".to_string(), Vec::new());
    }

    let concept_text = format!(
        "{} {} {}",
        c.slug.replace('-', " "),
        c.pref_label.to_lowercase(),
        c.frontier_opportunities.join(" ").to_lowercase()
    );
    let concept_tokens: BTreeSet<String> = tokenize_query(&concept_text)
        .into_iter()
        .filter(|tok| tok.len() >= 4 && !is_generic_advisory_stopword(tok))
        .collect();

    let mut overlapping = Vec::new();
    for ng in &t.non_goals {
        let ng_lower = ng.to_lowercase();
        if ng_lower.contains(&c.slug) {
            overlapping.push(ng.clone());
            continue;
        }
        let ng_tokens: BTreeSet<String> = tokenize_query(&ng_lower)
            .into_iter()
            .filter(|tok| tok.len() >= 4 && !is_generic_advisory_stopword(tok))
            .collect();
        if !concept_tokens.is_disjoint(&ng_tokens) {
            overlapping.push(ng.clone());
        }
    }

    if overlapping.is_empty() {
        ("not_assessed".to_string(), Vec::new())
    } else {
        ("potential_overlap".to_string(), overlapping)
    }
}

async fn execute_advise(
    store: &LadybugStore,
    namespace: &str,
    req: &TaxonomyRequest,
    principal: &str,
) -> Result<Value, String> {
    let now_ts = chrono::Utc::now().timestamp();
    let (active_rows, active_edges) = load_active_rows_and_edges(store, namespace, now_ts).await?;
    let view = build_principal_taxonomy_view(&active_rows, &active_edges, principal);

    let top_k = req.top_k.or(req.limit).unwrap_or(5).clamp(1, 20);
    let domain_filter = req
        .domain
        .as_deref()
        .and_then(domain::normalize_domain_slug);

    let raw_query = req
        .query
        .as_deref()
        .or(req.topic.as_deref())
        .unwrap_or("")
        .trim();
    let domain_tokens: Vec<String> = tokenize_query(raw_query)
        .into_iter()
        .filter(|t| !is_generic_advisory_stopword(t) && t.chars().any(|ch| ch.is_alphabetic()))
        .collect();

    let mut candidates: Vec<&HydratedConcept> = view
        .concepts
        .values()
        .filter(|c| {
            if let Some(ref dom) = domain_filter {
                if &c.slug != dom && c.domain.as_deref() != Some(dom.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();

    if !domain_tokens.is_empty() {
        let matched_set: BTreeSet<String> = candidates
            .iter()
            .filter(|c| score_concept_match(c, "", &domain_tokens) > 0)
            .flat_map(|c| {
                let mut set = vec![c.slug.clone()];
                set.extend(c.broader.iter().cloned());
                set.extend(c.narrower.iter().cloned());
                set.extend(c.related_concepts.iter().cloned());
                set.extend(c.contrasts_with.iter().cloned());
                set
            })
            .collect();
        if !matched_set.is_empty() {
            candidates.retain(|c| matched_set.contains(&c.slug));
        }
    }

    let visible_by_id: BTreeMap<String, &MemoryPayload> = view
        .visible_rows
        .iter()
        .map(|(id, pl)| (id.clone(), pl))
        .collect();

    // Collect open/in_progress visible tasks for canonical_grounding
    let open_tasks: Vec<(String, &MemoryPayload)> = view
        .visible_rows
        .iter()
        .filter(|(_id, pl)| {
            if pl.memory_type != "task" {
                return false;
            }
            let status = pl
                .metadata
                .get("task")
                .and_then(|t| t.get("status"))
                .or_else(|| pl.metadata.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("open");
            matches!(status, "open" | "in_progress" | "blocked")
        })
        .map(|(id, pl)| (id.clone(), pl))
        .collect();

    let mut actionable: Vec<&HydratedConcept> = Vec::new();
    let mut insufficient_evidence_concepts: Vec<String> = Vec::new();

    for c in &candidates {
        if c.realization_status == "external_only" {
            continue;
        }
        let has_frontier_or_limitation =
            !c.frontier_opportunities.is_empty() || !c.current_limitations.is_empty();
        let has_citations =
            !c.references.is_empty() || !c.comparable_implementations.is_empty();

        if c.frontier_leverage_bps > 0 && has_frontier_or_limitation && has_citations {
            actionable.push(c);
        } else if !has_frontier_or_limitation || !has_citations {
            insufficient_evidence_concepts.push(c.slug.clone());
        }
    }
    insufficient_evidence_concepts.sort();
    insufficient_evidence_concepts.dedup();

    // Deterministic sorting: (frontier_leverage_bps DESC, grounding_count DESC, slug ASC)
    actionable.sort_by(|a, b| {
        b.frontier_leverage_bps
            .cmp(&a.frontier_leverage_bps)
            .then_with(|| b.grounding_count.cmp(&a.grounding_count))
            .then_with(|| a.slug.cmp(&b.slug))
    });
    actionable.truncate(top_k);

    let mut recommendations: Vec<ExpertRecommendation> = Vec::new();
    for (idx, c) in actionable.into_iter().enumerate() {
        let mut gov_summaries = Vec::new();
        let mut all_locs: BTreeSet<String> = c.locations.iter().cloned().collect();
        for gid in &c.verified_grounding_ids {
            if let Some(pl) = visible_by_id.get(gid) {
                let first_line = pl.content.lines().next().unwrap_or(&pl.content).trim();
                gov_summaries.push(format!("[{}] ({}) {}", gid, pl.memory_type, first_line));
                if !pl.location.trim().is_empty() {
                    all_locs.insert(pl.location.trim().to_string());
                }
            }
        }

        let mut matching_tasks = Vec::new();
        for (tid, tpl) in &open_tasks {
            let text_lower = tpl.content.to_lowercase();
            let slug_space = c.slug.replace('-', " ");
            let label_lower = c.pref_label.to_lowercase();
            if text_lower.contains(&c.slug)
                || text_lower.contains(&slug_space)
                || text_lower.contains(&label_lower)
            {
                matching_tasks.push(tid.clone());
            }
        }
        matching_tasks.sort();

        let mut frontier_recs = c.frontier_opportunities.clone();
        for t in &c.tradeoffs {
            if !frontier_recs.contains(t) {
                frontier_recs.push(format!("Trade-off synthesis: {t}"));
            }
        }
        if !c.contrasts_with.is_empty() {
            frontier_recs.push(format!(
                "Differential contrast vs. alternatives: {}",
                c.contrasts_with.join(", ")
            ));
        }

        let (non_goal_assessment, overlapping_non_goals) =
            assess_non_goal_overlap(c, view.thesis.as_ref());

        recommendations.push(ExpertRecommendation {
            rank: idx + 1,
            concept_slug: c.slug.clone(),
            pref_label: c.pref_label.clone(),
            domain_pillar: c.domain.clone(),
            epistemic_status: c.epistemic_status.clone(),
            realization_status: c.realization_status.clone(),
            leverage_score_bps: c.frontier_leverage_bps,
            canonical_grounding: CanonicalGroundingSummary {
                implemented_interfaces: c.implemented_interfaces.clone(),
                linked_action_types: c.linked_action_types.clone(),
                governing_rule_and_fact_ids: c.verified_grounding_ids.clone(),
                governing_summaries: gov_summaries,
                locations: all_locs.into_iter().collect(),
                open_task_ids: matching_tasks,
            },
            current_state_and_limitations: c.current_limitations.clone(),
            frontier_recommendations: frontier_recs,
            references_and_comparables: json!({
                "references": c.references,
                "comparable_implementations": c.comparable_implementations,
            }),
            thesis_alignment: ThesisAlignmentSummary {
                axis_alignment_bps: c.axis_alignment_bps,
                axis_scores: c.evaluation_scores.clone(),
                non_goal_assessment,
                overlapping_non_goals,
            },
        });
    }

    // Generate external_research_agenda (strictly <= 5 items, ONLY from public-safe unrestricted concepts/pillars!)
    let mut external_research_agenda: Vec<ResearchPrompt> = Vec::new();
    let public_domain_label = if view.thesis_is_public {
        view.thesis
            .as_ref()
            .map(|t| t.primary_domain.as_str())
            .unwrap_or("software-architecture")
    } else {
        "software-architecture"
    };

    for slug in &insufficient_evidence_concepts {
        if external_research_agenda.len() >= 5 {
            break;
        }
        if let Some(c) = view.concepts.get(slug) {
            if !c.is_public {
                continue;
            }
            external_research_agenda.push(ResearchPrompt {
                target_slug_or_domain: c.slug.clone(),
                reason: format!(
                    "Concept '{}' ({}) is at epistemic_status='{}' and lacks sufficient frontier opportunities or license-verified comparable implementations",
                    c.pref_label, c.slug, c.epistemic_status
                ),
                suggested_query: format!(
                    "{} {} state of the art papers benchmarks and MIT Apache-2.0 open source libraries",
                    c.pref_label, public_domain_label
                ),
                public_safe: true,
            });
        }
    }

    if view.thesis_is_public {
        if let Some(ref t) = view.thesis {
            let mapped: BTreeSet<String> = view
                .concepts
                .values()
                .flat_map(|c| {
                    let mut v = vec![c.slug.clone()];
                    if let Some(ref d) = c.domain {
                        v.push(d.clone());
                    }
                    v
                })
                .collect();
            for pillar in &t.core_pillars {
                if external_research_agenda.len() >= 5 {
                    break;
                }
                if !mapped.contains(pillar) {
                    external_research_agenda.push(ResearchPrompt {
                        target_slug_or_domain: pillar.clone(),
                        reason: format!(
                            "Core thesis pillar '{pillar}' has no anchored SKOS concepts yet"
                        ),
                        suggested_query: format!(
                            "{} {} foundational taxonomy literature and permissively licensed implementations",
                            pillar.replace('-', " "),
                            public_domain_label
                        ),
                        public_safe: true,
                    });
                }
            }
        }
    }

    Ok(json!({
        "status": "ok",
        "operation": "advise",
        "namespace": namespace,
        "thesis": view.thesis,
        "recommendations": recommendations,
        "insufficient_evidence_concepts": insufficient_evidence_concepts,
        "external_research_agenda": external_research_agenda,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdf::action::ActionSecurityContext;
    use crate::store::ladybug::LadybugStore;
    use crate::traits::{Embedder, VectorStore};

    struct DummyEmbedder(usize);
    #[async_trait::async_trait]
    impl Embedder for DummyEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.1; self.0])
        }
        fn dimensions(&self) -> usize {
            self.0
        }
    }

    fn sec(principal: &str) -> ActionSecurityContext {
        ActionSecurityContext {
            authenticated_principal: principal.to_string(),
            requested_principal: Some(principal.to_string()),
        }
    }

    #[tokio::test]
    async fn singleton_set_thesis_and_unique_upsert_concept_supersede_atomically_under_cas() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LadybugStore::new(&tmp.path().join("db"), 4).expect("store");
        let embedder = DummyEmbedder(4);
        let ns = "tax-cas-ns";
        store.init(ns).await.expect("init");

        // 1. First set_thesis creates thesis-v1
        let r1 = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "set_thesis".to_string(),
                namespace: ns.to_string(),
                primary_domain: Some("cognitive-memory".to_string()),
                mission: Some("Initial mission".to_string()),
                direction: Some("Initial direction".to_string()),
                core_pillars: Some(vec!["graph-memory".to_string(), "formal-verification".to_string()]),
                evaluation_axes: Some(vec!["determinism".to_string(), "auditability".to_string()]),
                non_goals: Some(vec!["cloud-only lock-in".to_string()]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("set_thesis v1");
        assert_eq!(r1["thesis_id"], "thesis-v1");
        assert!(r1["superseded_id"].is_null());

        // 2. Second set_thesis supersedes thesis-v1 with thesis-v2
        let r2 = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "set_thesis".to_string(),
                namespace: ns.to_string(),
                primary_domain: Some("cognitive-memory".to_string()),
                mission: Some("Refined mission v2".to_string()),
                direction: Some("Refined direction v2".to_string()),
                core_pillars: Some(vec!["graph-memory".to_string(), "formal-verification".to_string()]),
                evaluation_axes: Some(vec!["determinism".to_string(), "auditability".to_string()]),
                non_goals: Some(vec!["cloud-only lock-in".to_string()]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("set_thesis v2");
        assert_eq!(r2["thesis_id"], "thesis-v2");
        assert_eq!(r2["superseded_id"], "thesis-v1");

        // Verify thesis-v1 has superseded_by = "thesis-v2"
        let (_v1_vec, v1_pl) = store.get(ns, "thesis-v1").await.unwrap().unwrap();
        assert_eq!(v1_pl.metadata["superseded_by"], "thesis-v2");

        // 3. Upsert concept twice with same slug -> concept-bitemporal-graph-v1 superseded by v2
        let c1 = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("bitemporal-graph".to_string()),
                pref_label: Some("Bitemporal Property Graph".to_string()),
                definition: Some("Initial definition".to_string()),
                domain: Some("graph-memory".to_string()),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("upsert_concept v1");
        assert_eq!(c1["concept_id"], "concept-bitemporal-graph-v1");
        assert!(c1["superseded_id"].is_null());

        let c2 = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("bitemporal-graph".to_string()),
                pref_label: Some("Bitemporal Property Graph".to_string()),
                definition: Some("Updated canonical definition".to_string()),
                domain: Some("graph-memory".to_string()),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("upsert_concept v2");
        assert_eq!(c2["concept_id"], "concept-bitemporal-graph-v2");
        assert_eq!(c2["superseded_id"], "concept-bitemporal-graph-v1");
    }

    #[tokio::test]
    async fn rejects_dag_cycles_and_transitive_skos_s27_disjointness_violations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LadybugStore::new(&tmp.path().join("db"), 4).expect("store");
        let embedder = DummyEmbedder(4);
        let ns = "tax-skos-s27-ns";
        store.init(ns).await.expect("init");

        // Create chain: grand-child -> child -> root-concept
        execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("root-concept".to_string()),
                pref_label: Some("Root Concept".to_string()),
                definition: Some("Top level concept".to_string()),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("root");

        execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("child-concept".to_string()),
                pref_label: Some("Child Concept".to_string()),
                definition: Some("Mid level concept".to_string()),
                broader: Some(vec!["root-concept".to_string()]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("child");

        execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("grand-child".to_string()),
                pref_label: Some("Grand Child".to_string()),
                definition: Some("Leaf level concept".to_string()),
                broader: Some(vec!["child-concept".to_string()]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("grand-child");

        // 1. Reject DAG cycle: root-concept broader -> grand-child
        let cycle_err = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("root-concept".to_string()),
                pref_label: Some("Root Concept".to_string()),
                definition: Some("Top level concept".to_string()),
                broader: Some(vec!["grand-child".to_string()]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect_err("must reject DAG cycle");
        assert!(cycle_err.contains("DAG cycle"), "unexpected error: {cycle_err}");

        // 2. Reject transitive SKOS S27 related_concepts between grand-child and root-concept
        let s27_err = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("grand-child".to_string()),
                pref_label: Some("Grand Child".to_string()),
                definition: Some("Leaf level concept".to_string()),
                broader: Some(vec!["child-concept".to_string()]),
                related_concepts: Some(vec!["root-concept".to_string()]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect_err("must reject transitive S27 related violation");
        assert!(
            s27_err.contains("SKOS S27"),
            "unexpected error: {s27_err}"
        );

        // 3. Reject transitive contrast violation between grand-child and root-concept
        let contrast_err = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("grand-child".to_string()),
                pref_label: Some("Grand Child".to_string()),
                definition: Some("Leaf level concept".to_string()),
                broader: Some(vec!["child-concept".to_string()]),
                contrasts_with: Some(vec!["root-concept".to_string()]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect_err("must reject transitive contrast violation");
        assert!(
            contrast_err.contains("disjointness"),
            "unexpected error: {contrast_err}"
        );
    }

    #[tokio::test]
    async fn dynamic_read_time_epistemic_status_recomputes_when_governing_rule_archived() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LadybugStore::new(&tmp.path().join("db"), 4).expect("store");
        let embedder = DummyEmbedder(4);
        let ns = "tax-dynamic-epistemic-ns";
        store.init(ns).await.expect("init");

        // 1. Seed active rule-1
        store
            .upsert(
                ns,
                "rule-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "All state machines must be proven in Lean 4.".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "src/formal_kernel.rs".to_string(),
                    location_lines: String::new(),
                    metadata: json!({ "valid_from": 100 }),
                },
            )
            .await
            .expect("seed rule-1");

        // 2. Upsert concept referencing rule-1 -> Verified (10000 bps)
        let up = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("lean-kernel-parity".to_string()),
                pref_label: Some("Lean 4 Kernel Parity".to_string()),
                definition: Some("Pure decision functions verified in Lean 4.".to_string()),
                domain: Some("formal-verification".to_string()),
                tradeoffs: Some(vec!["Requires pure integer/enum extraction".to_string()]),
                references: Some(vec!["de Moura & Ullrich (2021) Lean 4".to_string()]),
                evidence_refs: Some(vec!["rule-1".to_string()]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("upsert verified concept");
        assert_eq!(up["epistemic_status"], "verified");
        assert_eq!(up["epistemic_weight_bps"], 10000);

        // 3. Archive rule-1 -> read-time epistemic_status must dynamically downgrade to "synthesized" (8000 bps)
        store
            .upsert(
                ns,
                "rule-1",
                vec![0.1; 4],
                MemoryPayload {
                    content: "All state machines must be proven in Lean 4.".to_string(),
                    user_id: "alice".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: "src/formal_kernel.rs".to_string(),
                    location_lines: String::new(),
                    metadata: json!({ "valid_from": 100, "archived": true }),
                },
            )
            .await
            .expect("archive rule-1");

        let tree = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "tree".to_string(),
                namespace: ns.to_string(),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("tree after archiving rule-1");
        assert_eq!(tree["roots"][0]["epistemic_status"], "synthesized");
        assert_eq!(tree["roots"][0]["epistemic_weight_bps"], 8000);
    }

    #[tokio::test]
    async fn two_principal_rebac_prevents_private_concept_leakage_across_all_read_ops() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LadybugStore::new(&tmp.path().join("db"), 4).expect("store");
        let embedder = DummyEmbedder(4);
        let ns = "tax-rebac-ns";
        store.init(ns).await.expect("init");

        // Public concept
        execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("public-retrieval".to_string()),
                pref_label: Some("Public Hybrid Retrieval".to_string()),
                definition: Some("Public RRF retrieval.".to_string()),
                domain: Some("retrieval".to_string()),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("public concept");

        // Restricted concept visible only to alice
        execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("secret-proprietary-algo".to_string()),
                pref_label: Some("Secret Proprietary Ranking".to_string()),
                definition: Some("Confidential ranking heuristic.".to_string()),
                domain: Some("retrieval".to_string()),
                broader: Some(vec!["public-retrieval".to_string()]),
                access_policy: Some(json!({
                    "public": false,
                    "allowed_users": ["alice"],
                    "allowed_roles": []
                })),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("restricted concept");

        // 1. Bob cannot see secret-proprietary-algo in tree (nor in public-retrieval's narrower list)
        let bob_tree = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "tree".to_string(),
                namespace: ns.to_string(),
                ..Default::default()
            },
            &sec("bob"),
        )
        .await
        .expect("bob tree");
        let bob_tree_str = serde_json::to_string(&bob_tree).unwrap();
        assert!(!bob_tree_str.contains("secret-proprietary-algo"));

        // 2. Bob cannot see secret-proprietary-algo in briefing, gaps, or advise
        let bob_briefing = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "briefing".to_string(),
                namespace: ns.to_string(),
                query: Some("retrieval".to_string()),
                ..Default::default()
            },
            &sec("bob"),
        )
        .await
        .expect("bob briefing");
        assert!(!serde_json::to_string(&bob_briefing)
            .unwrap()
            .contains("secret-proprietary-algo"));

        let bob_gaps = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "gaps".to_string(),
                namespace: ns.to_string(),
                ..Default::default()
            },
            &sec("bob"),
        )
        .await
        .expect("bob gaps");
        assert!(!serde_json::to_string(&bob_gaps)
            .unwrap()
            .contains("secret-proprietary-algo"));

        // 3. Even when Alice calls advise, external_research_agenda never includes restricted concepts
        let alice_advise = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "advise".to_string(),
                namespace: ns.to_string(),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("alice advise");
        let agenda_str = serde_json::to_string(&alice_advise["external_research_agenda"]).unwrap();
        assert!(
            !agenda_str.contains("secret-proprietary-algo"),
            "external_research_agenda must never leak access-controlled concepts: {agenda_str}"
        );
    }

    #[tokio::test]
    async fn briefing_forward_expand_backward_prune_removes_ungrounded_outer_leaves_but_keeps_bridges()
    {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LadybugStore::new(&tmp.path().join("db"), 4).expect("store");
        let embedder = DummyEmbedder(4);
        let ns = "tax-prune-ns";
        store.init(ns).await.expect("init");

        // Seed 1: seed-a, Seed 2: seed-b
        // Inner hop-1 node: mid-node (related to seed-a)
        // Outer hop-2 ungrounded leaf: dead-end-leaf (related only to mid-node, 0 rules) -> MUST BE PRUNED
        // Outer hop-1/2 bridge node: bridge-node (connected to both seed-a and seed-b) -> MUST BE RETAINED
        for (slug, label, rels) in [
            ("seed-alpha", "Alpha Retrieval", vec!["mid-hop", "bridge-concept"]),
            ("seed-beta", "Beta Retrieval", vec!["bridge-concept"]),
            ("mid-hop", "Intermediate Hop", vec!["seed-alpha", "dead-end-leaf"]),
            ("dead-end-leaf", "Ungrounded Outer Leaf", vec!["mid-hop"]),
            ("bridge-concept", "Bridging Concept", vec!["seed-alpha", "seed-beta"]),
        ] {
            execute_taxonomy(
                &store,
                &embedder,
                TaxonomyRequest {
                    operation: "upsert_concept".to_string(),
                    namespace: ns.to_string(),
                    slug: Some(slug.to_string()),
                    pref_label: Some(label.to_string()),
                    definition: Some(format!("Definition for {label}")),
                    related_concepts: Some(rels.into_iter().map(String::from).collect()),
                    ..Default::default()
                },
                &sec("alice"),
            )
            .await
            .expect(slug);
        }

        let briefing = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "briefing".to_string(),
                namespace: ns.to_string(),
                query: Some("Retrieval".to_string()),
                max_hops: Some(2),
                limit: Some(2),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("briefing");

        let nodes = briefing["neighborhood_subgraph"]["nodes"]
            .as_array()
            .unwrap();
        let node_slugs: BTreeSet<String> = nodes
            .iter()
            .filter_map(|n| n["slug"].as_str().map(String::from))
            .collect();
        let pruned: BTreeSet<String> = briefing["neighborhood_subgraph"]["pruned_outer_leaf_slugs"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        assert!(node_slugs.contains("seed-alpha"));
        assert!(node_slugs.contains("seed-beta"));
        assert!(node_slugs.contains("bridge-concept"));
        assert!(!node_slugs.contains("dead-end-leaf"));
        assert!(pruned.contains("dead-end-leaf"));
    }

    #[tokio::test]
    async fn compare_and_advise_rank_frontier_leverage_and_report_insufficient_evidence_and_non_goals(
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = LadybugStore::new(&tmp.path().join("db"), 4).expect("store");
        let embedder = DummyEmbedder(4);
        let ns = "tax-advise-ns";
        store.init(ns).await.expect("init");

        execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "set_thesis".to_string(),
                namespace: ns.to_string(),
                primary_domain: Some("cognitive-memory".to_string()),
                mission: Some("Verifiable cognitive memory and ontology engine".to_string()),
                direction: Some("Deterministic graph reasoning".to_string()),
                core_pillars: Some(vec!["retrieval".to_string(), "ontology".to_string()]),
                evaluation_axes: Some(vec![
                    "determinism".to_string(),
                    "local-sovereignty".to_string(),
                ]),
                non_goals: Some(vec![
                    "distributed consensus cluster orchestration".to_string()
                ]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("set_thesis");

        // Parent concept
        execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("retrieval".to_string()),
                pref_label: Some("Information Retrieval".to_string()),
                definition: Some("Core retrieval pillar.".to_string()),
                domain: Some("retrieval".to_string()),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("retrieval root");

        // High-leverage unimplemented concept
        execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("learned-sparse-splade".to_string()),
                pref_label: Some("Learned Sparse Retrieval".to_string()),
                definition: Some("Sparse lexical expansion with learned term weights.".to_string()),
                domain: Some("retrieval".to_string()),
                broader: Some(vec!["retrieval".to_string()]),
                contrasts_with: Some(vec!["distributed-cluster-index".to_string()]),
                tradeoffs: Some(vec!["Higher indexing latency vs superior recall".to_string()]),
                references: Some(vec!["Formal et al. (2021) SPLADE SIGIR".to_string()]),
                comparable_implementations: Some(vec![ComparableImplementation {
                    name: "tantivy".to_string(),
                    kind: "library".to_string(),
                    license: "MIT".to_string(),
                    reference: "https://crates.io/crates/tantivy".to_string(),
                    notes: Some("WAND block-max scoring".to_string()),
                }]),
                realization_status: Some("unimplemented".to_string()),
                current_limitations: Some(vec!["BM25 lacks learned term expansion".to_string()]),
                frontier_opportunities: Some(vec![
                    "Add quantized sparse expansion postings".to_string()
                ]),
                evaluation_scores: Some(BTreeMap::from([
                    ("determinism".to_string(), 9500),
                    ("local-sovereignty".to_string(), 9500),
                ])),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("splade");

        // Lower-leverage partial concept that overlaps a non-goal
        execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "upsert_concept".to_string(),
                namespace: ns.to_string(),
                slug: Some("distributed-cluster-index".to_string()),
                pref_label: Some("Distributed Consensus Cluster Index".to_string()),
                definition: Some("Multi-node sharded index with consensus.".to_string()),
                domain: Some("retrieval".to_string()),
                broader: Some(vec!["retrieval".to_string()]),
                contrasts_with: Some(vec!["learned-sparse-splade".to_string()]),
                tradeoffs: Some(vec!["Operational complexity".to_string()]),
                references: Some(vec!["Ongaro & Ousterhout (2014) Raft".to_string()]),
                realization_status: Some("partial".to_string()),
                current_limitations: Some(vec!["Single-node writer".to_string()]),
                frontier_opportunities: Some(vec![
                    "Add distributed consensus cluster orchestration".to_string()
                ]),
                evaluation_scores: Some(BTreeMap::from([
                    ("determinism".to_string(), 6000),
                    ("local-sovereignty".to_string(), 4000),
                ])),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("distributed-cluster-index");

        // Compare the two sibling concepts
        let cmp = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "compare".to_string(),
                namespace: ns.to_string(),
                slugs: Some(vec![
                    "learned-sparse-splade".to_string(),
                    "distributed-cluster-index".to_string(),
                ]),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("compare");
        assert_eq!(cmp["shared_ancestors"][0], "retrieval");
        assert_eq!(cmp["direct_contrasts"].as_array().unwrap().len(), 1);
        assert!(cmp["axis_matrix"]["determinism"].is_array());

        // Advise ranks learned-sparse-splade #1, flags distributed-cluster-index non_goal_assessment = "potential_overlap",
        // and places "retrieval" (which has no frontier_opportunities/references) in insufficient_evidence_concepts.
        let adv = execute_taxonomy(
            &store,
            &embedder,
            TaxonomyRequest {
                operation: "advise".to_string(),
                namespace: ns.to_string(),
                query: Some("What are the top 5 ways we could improve this project?".to_string()),
                top_k: Some(5),
                ..Default::default()
            },
            &sec("alice"),
        )
        .await
        .expect("advise");

        let recs = adv["recommendations"].as_array().unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0]["concept_slug"], "learned-sparse-splade");
        assert_eq!(
            recs[0]["thesis_alignment"]["non_goal_assessment"],
            "not_assessed"
        );
        assert_eq!(recs[1]["concept_slug"], "distributed-cluster-index");
        assert_eq!(
            recs[1]["thesis_alignment"]["non_goal_assessment"],
            "potential_overlap"
        );

        let insuff: Vec<String> = adv["insufficient_evidence_concepts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        assert!(insuff.contains(&"retrieval".to_string()));
    }
}
