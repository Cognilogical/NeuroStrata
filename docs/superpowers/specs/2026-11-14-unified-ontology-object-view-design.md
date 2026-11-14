# Design: `neurostrata_object_view` — Single-Call 360° Ontology Object View Hub

**Date:** 2026-11-14
> Status: DRAFT — tracked under Epic `neurostrata-1mus`, Feature Task `neurostrata-cspr` (Spec Task `neurostrata-oh0c`, blocked by `neurostrata-undx` and `neurostrata-brs1`).
**Companion Specs:** `2026-11-14-kinetic-action-types-and-revert-log-design.md` (`undx`), `2026-11-14-polymorphic-digital-twins-interfaces-and-reducers-design.md` (`brs1`), `2026-10-27-neurostrata-rdf-semantic-layer-design.md` (`9fnf`), `2026-10-28-digital-twin-design.md` (`jk91`), `2026-10-28-rebac-design.md`, and OxiRS §1.19 `QueryBudget` (`yjah`).

## 1. Problem

In Palantir Foundry, **Object Views** serve as the canonical 360° operational hub for any entity in the Ontology: in a single view, an operator or AI agent sees the object's core properties, implemented interfaces, derived reducer metrics, 1-hop linked objects grouped by link type, bound time-series telemetry, bitemporal edit/action history, governance status, and the exact list of **legal `Action Types`** that can currently be executed on that object.

In NeuroStrata today, assembling this 360° view for a `digital_twin`, `code_ast` symbol, `file`, `task`, or normative `rule`/`fact` requires chaining 5–7 separate MCP calls:
1. `get_by_id` / `neurostrata_search_memory` (core row payload & metadata).
2. `neurostrata_related` (1-hop incoming/outgoing graph links).
3. `neurostrata_memories_for_file` (if the entity has a file `location` governed by ancestor rules).
4. `neurostrata_twin_get_state` + `neurostrata_twin_history` (if `memory_type == "digital_twin"`).
5. `TimeSeriesStore::query_metrics` (`src/time_series.rs`) for bound metric series and `RollingAgg` anomalies.
6. `validate_memory` (`src/rdf/shacl.rs:57`) for SHACL conformance.
7. Checking which kinetic `ActionType`s (`neurostrata-undx`) target the entity's `memory_type` / `schema_id` / `implements_interfaces` and pass pre-conditions.

## 2. Goals

1. **One-Shot 360° Entity Hydration (`neurostrata_object_view`):**
   - Provide a single deterministic MCP tool (`neurostrata_object_view`), daemon endpoint (`POST /object-view`), and CLI command (`neurostrata-mcp object-view <id-or-twin-id>`) that hydrates the complete ontological profile of any entity in one batched round-trip.
2. **Seven-Section Canonical Payload (`ObjectViewResponse`):**
   - **Section 1 — Identity & Classification:** `id`, `bare_id`, `memory_type`, `domain` slugs, `concept_iri` (`pjoa`), `schema_id`, and resolved `implemented_interfaces` (`brs1`).
   - **Section 2 — State, Attributes & Derived Reducers:** Core `content`, structured `metadata`, and (when `memory_type == "digital_twin"`) stored `variables`, discrete `attributes`, and computed 1-hop `derived_variables` (`PropertyReducerSpec` from `brs1`).
   - **Section 3 — Typed 1-Hop Link Groups:** Incoming and outgoing active links grouped by relation (`GOVERNS`, `DEPENDS_ON`, `RELATES_TO`, `CONTAINS`, `EXTRACTED_FROM`, `SUPERSEDES`), bounded by `QueryBudget` (`max_nodes: 40`, `max_ms: 50` per `yjah`) and filtered by ReBAC (`src/rebac.rs:15`).
   - **Section 4 — Governance & Conformance:** Active `GOVERNS` rules (direct and ancestor directory rules if `location` is present), negative failed-attempt hazards (`metadata.attempt.outcome == "failed"`), and warn-first `shacl_violations` + `twin_warnings`.
   - **Section 5 — Bitemporal Version & Action Lineage:** Active validity window (`valid_from`, `valid_to`), supersession chain summary (`supersedes`, `superseded_by`, `version_count`), and recent `action_receipt` entries (`undx`) that mutated this entity.
   - **Section 6 — Bound Time-Series Summary (`src/time_series.rs`):** Latest value, `RollingAgg` EWMA summary (`mean`, `variance`, `count`), and recent anomalies for any `TimeSeriesStore` series keyed to this entity ID / `twin_id`.
   - **Section 7 — Available Legal `ActionType`s (`available_actions`):** Every registered `ActionTypeDefinition` (`undx`) whose target parameter matches this entity's `memory_type`, `schema_id`, or `implemented_interfaces`, annotated with `authorized: bool` (via `rebac::allowed_with_roles`) and `preconditions_met: bool` (via `submission_criteria` check).
3. **Graph-First Ledger Credit:**
   - Calling `neurostrata_object_view` calls `crate::task::record_graph_query` (`src/server.rs:2033`), crediting `task.graph_queries_count` and recording the entity's `location` (if non-empty) in `.NeuroStrata/session_queries.json`.

## 3. Non-Goals

- Building a browser-based GUI builder (Workshop) inside this spec; `neurostrata_object_view` provides the canonical JSON/MCP data contract that both CLI/MCP agents and the existing exported graph UI can render.
- Returning unbounded multi-hop subgraphs (`neurostrata_object_view` is strictly a 1-hop 360° entity hub bounded by `QueryBudget` and `max_bytes`).

## 4. Library Evaluation & Five Mandatory OxiRS Compliance Elements

Per Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`:

1. **Library Evaluation & Banned-Crate Rejection:**
   - **Used (already in `Cargo.toml`):** `oxrdf` + `spargebra` + `spareval` (evaluating `ActionType` `SparqlAsk` submission criteria), `oxttl` (SHACL conformance check), `arc-swap` (`OntologyIndex` lookup), `sha2`.
   - **Dependency Policy:** Always use a suitable license-compatible library when it provides the required functionality; never optimize for a zero-new-dependency count or prefer in-house code by default. The existing Oxigraph crates already provide the required RDF and SPARQL functionality. Evaluate any additional candidate on functional fit, license compatibility, correctness, security, maintenance, deterministic behavior, and integration cost. Historical crate rejections are context-specific, not a closed dependency policy.
2. **Warn-First Error Discipline:**
   - Any SHACL violation or Digital Twin bounds/cardinality mismatch on the inspected object is surfaced in `governance.shacl_violations` and `governance.twin_warnings` without failing the `object_view` read call.
3. **Per-Namespace Isolation:**
   - `neurostrata_object_view` queries only the requested `namespace` and enforces `rebac::allowed_with_roles(&principal, &row.user_id, "viewer", &acl)` (`src/rebac.rs:15`) on both the root object and every 1-hop linked neighbor before inclusion.
4. **Deterministic Output & Zero LLM in Core:**
   - All 7 sections of `ObjectViewResponse` are assembled deterministically in Rust with stable sorting (`relation` ASC, `score`/`id` ASC) and zero LLM calls.
5. **Cross-References to Prior Specs:**
   - Integrates `9fnf` (`src/rdf/projection.rs`), `yjah` (`QueryBudget`), `2hxl` (OWL 2 RL closure), `pjoa` (`concept_iri`), `u5o1` (canonical slugs), `4gb2` (RDF-star edge provenance), `52dq` (GraphRAG hybrid), `mhsp` (inter-namespace isolation), `jk91` (`src/digital_twin/mod.rs`), `2026-10-28-rebac-design.md` (`src/rebac.rs`), `undx` (`ActionTypeDefinition`), and `brs1` (Twin Interfaces & Reducers).

## 5. Architecture

### 5.1 Request & Response Contract (`src/graph_read/object_view.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectViewRequest {
    pub namespace: String,
    /// Either a memory ID ("mem-...") or a digital twin_id ("loop-a").
    pub id: String,
    pub include_history_limit: Option<usize>, // default 5, max 25
    pub evaluate_action_preconditions: Option<bool>, // default true
    pub max_bytes: Option<usize>,             // default 8192
    pub project_root: Option<String>,
    pub principal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwinObjectState {
    pub twin_id: String,
    pub version: u64,
    pub variables: BTreeMap<String, f64>,
    pub attributes: BTreeMap<String, serde_json::Value>,
    pub derived_variables: BTreeMap<String, f64>,
    pub units: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedObjectSummary {
    pub id: String,
    pub direction: String,                    // "incoming" | "outgoing"
    pub memory_type: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectGovernanceSummary {
    pub governing_rule_ids: Vec<String>,
    pub failed_attempt_ids: Vec<String>,
    pub shacl_violations: Vec<crate::rdf::shacl::ShaclViolation>,
    pub twin_warnings: Vec<crate::digital_twin::TwinWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectLineageSummary {
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub recent_action_receipt_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMetricSummary {
    pub series_name: String,
    pub latest_value: f64,
    pub rolling_summary: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableActionSummary {
    pub slug: String,
    pub title: String,
    pub required_role: String,
    pub authorized: bool,
    pub preconditions_met: bool,
    pub unmet_reasons: Vec<String>,
    pub parameters: BTreeMap<String, crate::rdf::action::ActionParamSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectViewResponse {
    pub namespace: String,
    pub id: String,
    pub memory_type: String,
    pub concept_iri: Option<String>,
    pub domains: Vec<String>,
    pub schema_id: Option<String>,
    pub implemented_interfaces: Vec<String>,
    pub content: String,
    pub location: Option<String>,
    pub location_lines: Option<String>,
    pub twin_state: Option<TwinObjectState>,
    pub links: BTreeMap<String, Vec<LinkedObjectSummary>>, // keyed by relation
    pub governance: ObjectGovernanceSummary,
    pub lineage: ObjectLineageSummary,
    pub metrics: Vec<ObjectMetricSummary>,
    pub available_actions: Vec<AvailableActionSummary>,
    pub truncated_by_budget: bool,
}
```

### 5.2 Resolution & Batched Hydration Flow

1. **ID / `twin_id` Resolution:**
   - Look up `id` directly via `store.get_by_id(namespace, &id)`. If not found, check if `id` matches an active `digital_twin`'s `metadata.twin.id` (resolving to the latest active version row via `src/digital_twin/mod.rs`).
2. **ReBAC Gate (`src/rebac.rs:15`):**
   - Verify `rebac::allowed_with_roles(&principal, &row.user_id, "viewer", &acl)`. If unauthorized, return `NotFound` / `PermissionDenied` without leaking existence.
3. **Single-Pass Batched Hydration:**
   - Resolve `implemented_interfaces` and `derived_variables` (`brs1`) if `memory_type == "digital_twin"`.
   - Fetch 1-hop incoming/outgoing edges within `QueryBudget { max_ms: 50, max_nodes: 40 }`, filtering out retired/conflicting/unauthorized neighbors.
   - If `row.location` is non-empty, attach ancestor directory `GOVERNS` rules.
   - Run `shacl::validate_memory(&row)` and `digital_twin::validate_state`.
   - Query `store.metrics` (`src/time_series.rs`) for any series matching `id` or `twin_id`.
   - Match `ActionTypeDefinition`s (`undx`) from `OntologyIndex` whose target parameter accepts `row.memory_type`, `schema_id`, or any interface in `implemented_interfaces`, and check `authorized` + `preconditions_met`.
   - Record graph query via `crate::task::record_graph_query` (`src/server.rs:2033`).

## 6. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Latency from evaluating `SparqlAsk` preconditions across many `ActionType`s on read | Only evaluate `SparqlAsk` preconditions for `ActionType`s whose target type/schema/interface matches the viewed object, and allow callers to set `evaluate_action_preconditions: false` for fast schema-only action discovery. |
| Unauthorized 1-hop link leakage | Filter every 1-hop neighbor and governing rule through `rebac::allowed_with_roles(&principal, ...)` before adding to `links` or `governance`. |
| Oversized payload on high-degree hub nodes | Enforce `QueryBudget` (`max_nodes: 40`) on 1-hop links and cap total serialized response size at `max_bytes` (default `8192`), setting `truncated_by_budget: true` when trimmed. |

## 7. Checkable Acceptance Criteria

1. Calling `neurostrata_object_view` on a `digital_twin` ID returns its stored `variables`, discrete `attributes`, computed 1-hop `derived_variables` (`brs1`), `implemented_interfaces`, 1-hop `links`, `lineage` version history, and matching `available_actions` (`undx`) in a single call.
2. Calling `neurostrata_object_view` on a `code_ast` symbol or `file` node returns its `DEPENDS_ON` callers/callees, active `GOVERNS` rules (including ancestor directories), failed `attempt` memories, and increments `task.graph_queries_count` via `record_graph_query`.
3. When a `"viewer"` principal requests `neurostrata_object_view` on an object that has an `"editor"`-only `ActionType`, `available_actions` lists the action with `authorized: false`.
4. When a 1-hop linked neighbor has a restricted `visible_to` ACL excluding the caller principal, that neighbor is omitted from `ObjectViewResponse.links`.

## 8. Next Step

On approval of this spec, invoke `writing-plans` for Task `neurostrata-cspr` to produce the step-by-step TDD implementation plan.
