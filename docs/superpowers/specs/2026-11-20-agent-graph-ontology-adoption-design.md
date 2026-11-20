# Agent Graph & Ontology Adoption — Design Spec

Date: 2026-11-20
Namespace: `NeuroStrata`
Source: three-seat panel (`opencode-go/gemini-4-argon`, `k3-arch`, `opencode-go-spec-writer`), task `neurostrata-e2yn` / `neurostrata-qeyz`

## 1. Purpose

Agents use the Central Executive workflow heavily (`task_*`, `memories_for_file`, `add_memory`) but rarely use explicit graph traversal or ontology operations. Panel verdict: this is a **contract, affordance, and incentive** problem, not a capability gap. Default `search_memory(mode:"hybrid")` already fuses graph expansion, and `memories_for_file` already returns one-hop `related_memories` + AST summary; explicit multi-hop and ontology tools lack correct instructions, in-band handoffs, and honest measurement.

This spec defines the remediation. It is implemented by `docs/superpowers/plans/2026-11-20-agent-graph-ontology-adoption.md`.

## 2. Non-Goals

- **No UI work.** `web-ui/` and `src-tauri/` are scratch-pad artifacts pending full redesign. No panels, tabs, types, commands, or styling changes belong in this effort. MCP payloads are the only agent-facing surface in scope.
- No changes to retrieval ranking math, HNSW parameters, RRF weights, or embedding models.
- No synthetic graph/ontology calls, adoption quotas, or blocking gates on tool counts.
- No behavioral forcing of data-starved subsystems (kinetic actions, digital twins, scenarios, procedures, SynapticMesh).
- No weakening of Lock 1/Lock 2, ReBAC/privacy filters, hook fail-soft behavior, or SHACL validation.

## 3. Global Constraints

- JSON only for new config/contract/receipt formats (rule `1ecfd804`).
- No Python code or tooling (rule `bf710e0e`).
- Never defer: every requirement below has an implementing task in the plan (rule `a97d0b19`).
- No competitor-derived names in repo artifacts (rule `9e64a5a8`).
- Golden Rules keep top precedence in snapshots and task context (`d104c715`).
- Implementation delegated to free subagents; focused tests only, no full-workspace `cargo test` in lanes (`4dca508a`, `f689fec8`).
- All new response fields are additive; existing fields remain byte-compatible.

## 4. Verified Ground Truth

| ID  | Fact                                                                                                                                                                                         | Anchor (at time of writing)                                                                                                                                                                       |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| F1  | `include_related` is not in the `search_memory` schema or handler. Real flag is `include_references` (append `edge_sources` provenance). Unknown keys are silently ignored.                  | `src/server.rs` schema ~1503–1635, handler `handle_search_memory:10729`                                                                                                                           |
| F2  | `mode` defaults to `hybrid` (HNSW + BM25 + graph RRF + one-hop expansion), not `vector`. Skills claim `vector` default and "no rank fusion".                                                 | `src/server.rs:301,11118`; `src/store/ladybug.rs:176,563`                                                                                                                                         |
| F3  | Stale `include_related: true` guidance appears in six sites.                                                                                                                                 | `AGENTS.md:68`; `.agents/skills/neurostrata/SKILL.md:48`; `.opencode/skills/neurostrata/SKILL.md:48`; `src/task/mod.rs:854,2974,3056`; `src/task/wiring.rs:183`; `src/task/harness_plugins.rs:98` |
| F4  | `build_task_graph_context` hardcodes `"related_edges": []`, so task/subagent context carries no graph edges.                                                                                 | `src/task/mod.rs:823`                                                                                                                                                                             |
| F5  | `memories_for_file` already returns one-hop `related_memories`, `conceptual_bridges`, AST summary including `blast_radius_count`.                                                            | `src/server.rs:379,3114,3343,3350`                                                                                                                                                                |
| F6  | `scripts/hooks/pre-commit` fabricates `{tool:"pre-commit"}` rows into `.NeuroStrata/session_queries.json`.                                                                                   | `scripts/hooks/pre-commit:284,315,346`                                                                                                                                                            |
| F7  | Graph nudge fires only when `graph_queries_count == 0`; any counted lookup suppresses it.                                                                                                    | `src/task/mod.rs` nudge logic, `scripts/hooks/pre-commit`                                                                                                                                         |
| F8  | Taxonomy snapshot/context is filtered by `rebac::is_restricted` / `is_public_access_policy`; restricted rows are intentionally hidden. No blanket `eligibility:"private"` default was found. | `src/rdf/taxonomy.rs:416,3443,3456`                                                                                                                                                               |
| F9  | Semantic evaluation honestly FAILS: ontology alias+hierarchy Recall@5 subset lift `0.042857 < 0.10`.                                                                                         | `src/semantic_eval/metrics.rs`                                                                                                                                                                    |
| F10 | Action receipts empty, twins empty, taxonomy concepts present, metrics only `search`/`memory_count`.                                                                                         | live MCP state, 2026-11-20                                                                                                                                                                        |

## 5. Definitions

- **Genuine lookup**: a graph/ontology read recorded by a tool handler after dispatch, with a real tool name (`memories_for_file`, `locate`, `related`, `object_view`, `diff_tour`, `sparql_query`, `taxonomy`). `tool:"pre-commit"` never counts.
- **next_action**: JSON object `{action_id, trigger_id, tool, args, reason, expected_decision}`. `args` must be schema-valid for the named tool. Maximum three per response. Empty array is allowed only with `suppressed_reason`.
- **trigger**: deterministic condition defined in the routing contract; emits `trigger_id`. No matching trigger means no required call (abstention is valid).
- **receipt**: private, local, append-only JSON record of a routed call's lifecycle. Digests, never raw query/content. Never SynapticMesh-shareable.
- **follow-through**: a prescribed `next_action` was executed with a successful or valid-empty result, or a justified alternative was recorded.

## 6. Requirements

### R1 — Tool contract repair

1. Replace every F3 site with the true contract: `search_memory(mode: "hybrid"|"vector"|"lexical"|"lexical-inhouse", include_references?: bool, domain?, facets?)`; one-hop graph expansion happens in `hybrid`; `include_references` attaches provenance only.
2. Accept `include_related` as a **deprecated alias** in `handle_search_memory`: map to `include_references`, emit `warnings: ["'include_related' is deprecated; use 'include_references'"]`. It must never be silently ignored.
3. Add additive response field `graph_expanded_ids: String[]` listing IDs contributed by graph expansion, so callers have seed IDs for explicit `related`.
4. A parity test must fail if any of the six F3 sites regress to the stale strings.

### R2 — Task-context graph handoff

1. `build_task_graph_context` must use the hybrid search path rather than raw vector `store.search` for seed selection.
2. Populate `related_edges` with at most 15 one-hop `GOVERNS`/`DEPENDS_ON`/`CONTAINS`/`RELATES_TO`/AST `calls`/`imports` edges from at most five seed items.
3. Emit `next_hop_calls: next_action[]` (≤3) with copy-pasteable `related`/`object_view` arguments derived from returned IDs.
4. Golden Rules remain first and complete. Prompt growth is bounded: ≤3,500 chars for `subagent_prompt_injection`; truncate lowest-priority structural items first, never golden rules.
5. On graph expansion failure/timeout, return context with `expansion_error` and empty edges; never block task creation/claim.

### R3 — Conditional escalation from `memories_for_file`

1. Add `next_actions: next_action[]` to `handle_memories_for_file` output.
2. Trigger `related(depth:2)` on a returned symbol ID when `ast_summary.blast_radius_count >= 15`.
3. Trigger `object_view` when `conceptual_bridges` is non-empty and a bridge has an ID.
4. Trigger `diff_tour` when the task/session scope has ≥3 staged files (scope supplied by caller; absent scope = no trigger).
5. Explicit `success | empty | unsupported` status. Empty lookup is **not** "no governance"; it routes to `locate`.
6. Do not recommend `related(depth:1)` for ordinary single-file edits — F5 already covers one hop.

### R4 — Routing contract as single source of truth

1. New crate module `src/task/routing.rs` defining a strict-JSON serializable contract with this table:

| `trigger_id`   | Condition                                             | Tool                            | Required args                                   |
| -------------- | ----------------------------------------------------- | ------------------------------- | ----------------------------------------------- |
| `T_PATH`       | known path before edit                                | `neurostrata_memories_for_file` | `namespace`, `path`, `include_ast_edges:true`   |
| `T_AMBIGUOUS`  | unknown path/symbol or empty file lookup              | `neurostrata_locate`            | `namespace`, `query`                            |
| `T_HIGH_BLAST` | `blast_radius_count >= 15`                            | `neurostrata_related`           | `namespace`, `id`, `max_hops:2`                 |
| `T_DIFF`       | ≥3 staged files / PR review                           | `neurostrata_diff_tour`         | `namespace`, `git_ref`                          |
| `T_DOMAIN_NEW` | `task_type` feature/epic or design review             | `neurostrata_taxonomy`          | `namespace`, `operation:"briefing"` then `gaps` |
| `T_STRATEGIC`  | explicit strategic-improvement request                | `neurostrata_taxonomy`          | `namespace`, `operation:"advise"`, `top_k:5`    |
| `T_INVARIANT`  | cross-row invariant not answered by bounded traversal | `neurostrata_sparql_query`      | `namespace`, `query` from template              |

2. `agent_routing_contract_json()` returns canonical JSON used by prompt builders and tests.
3. `AGENTS.md` §3d/§10, both SKILL.md files, and generated prompt constants render from or are parity-checked against this contract. A test fails on drift (missing/renamed tool, wrong arg).
4. Abstention: if no trigger matches, emit no action and no warning.

### R5 — Snapshot readiness (MCP payload only)

1. `handle_get_snapshot` adds a compact `### Graph & Ontology Readiness` section: `taxonomy_concepts_count`, `ungrounded_concepts_count`, `active_task_graph_tool_diversity`, and at most three `recommended_reads`.
2. When a namespace has zero thesis/concept rows, emit one actionable line: `Taxonomy (0 concepts): run neurostrata_taxonomy(operation:'set_thesis') to anchor domain concepts.`
3. ReBAC filtering is preserved verbatim; restricted rows are never exposed and never counted in readiness totals.

### R6 — Honest telemetry

1. Remove F6 fabrication entirely. `.NeuroStrata/session_queries.json` gains a versioned `events[]` section; legacy `queries[]` and `tool:"pre-commit"` rows are retained for compatibility but excluded from all quality metrics.
2. Extend `record_graph_query_with_root` into a post-dispatch recorder used by all in-scope graph/ontology handlers. Fields: `event_id`, `ts`, `session_id`, `task_id`, `namespace`, `tool`, `operation`, `trigger_id`, `route_class`, `schema_valid`, `status` (`success|empty|unsupported|failed`), `latency_ms`, `result_count`, `returned_ids` (bounded), `prescribed_action_ids`, `followed_action_ids`, `query_digest`, `content_digest`.
3. Advanced subsystems get additive `tool_invocations: {tool: count}` counters only. No gates, quotas, or nudges.
4. Receipts are private/local; verify zero receipt rows in SynapticMesh export.

### R7 — Task validation advisories

1. `handle_task_validate` / `GateReport.validate_json()` gains non-blocking `advisories.graph_ontology`:
   `{genuine_lookup_coverage, distinct_graph_tools, high_blast_radius_unexplored, diff_tour_recommended, taxonomy_available, strategic_context_detected, concept_grounding_linked, recommendations[], unresolved_trigger_ids[]}`.
2. Replacement of the count-based nudge: emit `W_GRAPH_NO_EXPANSION` only when an applicable `T_HIGH_BLAST` / `T_DIFF` trigger has no genuine receipt. Any genuine one-hop lookup no longer counts as compliance.
3. No advisory ever enters `violations` by default. Strict mode is out of scope for this change set.

### R8 — Evaluation and acceptance

1. Keep the semantic lift gate visible and FAILING until it genuinely passes; no default-ranking ROI claims.
2. Add a blind paired workflow suite with 120 owner-labeled tasks: 40 routine edits, 40 cross-file impact, 40 ontology-relevant.
3. Thresholds (rolling, with confidence intervals):

| Metric                                                            | Threshold                          |
| ----------------------------------------------------------------- | ---------------------------------- |
| Contract drift (stale names/args in prompts)                      | 0                                  |
| Post-dispatch receipt completeness                                | 100%                               |
| Synthetic `pre-commit` lookup rows                                | 0                                  |
| Applicable route coverage                                         | ≥95%                               |
| Nonempty graph / ontology follow-through                          | ≥80% each                          |
| Routing precision / unnecessary ontology routing on routine edits | ≥90% / ≤5%                         |
| Blind cross-file impact accuracy vs path-only control             | ≥ +10 pp                           |
| Blind ontology-relevant accuracy vs hybrid control                | ≥ +10 pp                           |
| Routine-task regression                                           | no subgroup worse than −2 pp       |
| `memories_for_file` response p95                                  | ≤32 KiB                            |
| Routing overhead p95 (excluding model generation)                 | ≤750 ms/task                       |
| Ontology Recall@5 subset lift                                     | ≥ +0.10 (currently 0.042857, FAIL) |

4. `production_roi_claimed` stays `false` until real-namespace and blind-agent fields are evaluated and pass.

## 7. File Map

| File                                                                                                                              | Responsibility                                                                          |
| --------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `src/task/routing.rs` (new)                                                                                                       | canonical routing contract + trigger evaluation + JSON serialization                    |
| `src/task/mod.rs`                                                                                                                 | task context handoff, prompt constants, receipt recorder, validate advisories           |
| `src/task/wiring.rs`, `src/task/harness_plugins.rs`                                                                               | generated prompt text from contract                                                     |
| `src/server.rs`                                                                                                                   | `search_memory`, `memories_for_file`, `get_snapshot` payloads + post-dispatch recording |
| `AGENTS.md`, `.agents/skills/neurostrata/SKILL.md`, `.opencode/skills/neurostrata/SKILL.md`, `.agents/agents/NeuroStrata-Task.md` | human/agent-facing contract (parity-checked)                                            |
| `scripts/hooks/pre-commit`                                                                                                        | remove synthetic ledger rows                                                            |
| `tests/search/upgraded_search_test.rs`                                                                                            | contract repair + response schema tests                                                 |
| `tests/graph_read/memories_for_file_test.rs`                                                                                      | escalation triggers                                                                     |
| `tests/graph_read/related_test.rs`                                                                                                | edge/shape contracts                                                                    |
| `tests/semantic_eval_metrics_test.rs`                                                                                             | ontology lift regressions                                                               |
| `tests/agent_adoption_test.rs` (new)                                                                                              | routing parity, receipts, validate advisories, evaluation thresholds                    |

No `web-ui/` or `src-tauri/` files are touched.

## 8. Risks and Detection

| Risk                                      | Detection                                                     |
| ----------------------------------------- | ------------------------------------------------------------- |
| Prompt bloat crowds out Golden Rules      | prompt char budget test; `d104c715` assertion                 |
| Cargo-cult tool calls satisfy metrics     | measure follow-through and blind lift, not call volume        |
| Ontology over-triggering on routine edits | routing precision + ≤5% false-trigger metric                  |
| Empty graph read treated as "safe"        | explicit `empty`/`unsupported` status; never infer governance |
| Restricted rows leak through readiness    | ReBAC tests; zero non-shareable rows in mesh export           |
| Hook friction after removing fabrication  | warn rate >25% after 10 sessions indicates plugin failure     |
| Ontology quality overclaim                | keep FAIL visible; `production_roi_claimed:false`             |

## 9. Acceptance

All R1–R8 implemented, plan tasks green with focused tests, contract-drift count 0, synthetic ledger rows 0, evaluation thresholds met or honestly reported as unmet, and no UI files modified.
