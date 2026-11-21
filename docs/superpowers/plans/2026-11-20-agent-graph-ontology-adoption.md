# Agent Graph & Ontology Adoption Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** make explicit graph traversal and ontology operations the obvious, correctly-invoked next step for agents without touching the scratch-pad UI (tracked: neurostrata-8jt3).

**Architecture:** one canonical JSON routing contract in `src/task/routing.rs` drives agent-facing prompt text and validation. Tool handlers return schema-valid `next_actions` derived from real results, and post-dispatch private receipts measure follow-through instead of call volume. All response changes are additive; gates remain advisory.

**Tech Stack:** Rust (`neurostrata-mcp` binary), serde_json, existing HNSW/BM25/graph store, shell + jq for evaluation contracts.

**Spec:** `docs/superpowers/specs/2026-11-20-agent-graph-ontology-adoption-design.md`

## Global Constraints

- JSON only for new config/contract/receipt formats (rule `1ecfd804`).
- No Python code or tooling (rule `bf710e0e`).
- Never defer: every requirement has an implementing task (rule `a97d0b19`).
- No competitor-derived names in repo artifacts (rule `9e64a5a8`).
- Golden Rules keep top precedence in snapshots and task context (`d104c715`).
- Implementation delegated to free subagents; focused tests only, no full-workspace `cargo test` in lanes (`4dca508a`, `f689fec8`).
- All new response fields are additive; existing fields remain byte-compatible.
- **`web-ui/` and `src-tauri/` are out of scope.** No files there are created or modified.

## Review Focus

- **Stale `include_related:true` passed as a no-op** → Task 1 test asserts deprecated-alias warning and absence from all six prompt sites.
- **Empty graph result read as "no governance"** → Task 4 test asserts `status:"empty"` plus a `T_AMBIGUOUS` action to `locate`.
- **Restricted taxonomy rows exposed via readiness counts** → Task 5 test asserts `rebac::is_restricted` rows are excluded from `taxonomy_concepts_count`.
- **Synthetic `pre-commit` ledger rows counted as compliance** → Task 6 test asserts zero such rows and that quality metrics ignore legacy rows.
- **Ontology routing hammered on routine edits** → Task 2 test `routing_abstains_on_routine_single_file_edit` asserts `T_DOMAIN_NEW`/`T_STRATEGIC` abstention on a routine single-file edit.

Pin each listed test to its owning task below; do not add a separate "test all requirements" task.

---

### Task 1: Repair the search tool contract and stop silent no-ops

**Files:**

- Modify: `src/server.rs` (schema ~1503–1635; `handle_search_memory:10729`)
- Modify: `AGENTS.md:68`; `.agents/skills/neurostrata/SKILL.md:48`; `.opencode/skills/neurostrata/SKILL.md:48`; `src/task/mod.rs:854,2974,3056`; `src/task/wiring.rs:183`; `src/task/harness_plugins.rs:98`
- Test: `tests/search/upgraded_search_test.rs`

**Interfaces:**

- Consumes: existing `handle_search_memory(arguments, store)`.
- Produces: JSON response fields `warnings: string[]`, `graph_expanded_ids: string[]`; deprecated `include_related` maps to `include_references`.

- [ ] **Step 1: Write failing test `search_memory_rejects_silent_include_related_noop`**

Assert: passing `include_related:true` returns `warnings` containing the exact string `'include_related' is deprecated; use 'include_references'`; `mode` default equals `"hybrid"`; `graph_expanded_ids` is an array (may be empty).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test upgraded_search_test search_memory_rejects_silent_include_related_noop -- --nocapture`
Expected: FAIL (field absent / warning absent).

- [ ] **Step 3: Implement contract in `src/server.rs`**

Add `include_related` as deprecated alias in the schema and handler; emit warning; expose `graph_expanded_ids` from the IDs already produced by hybrid graph expansion. No ranking changes.

- [ ] **Step 4: Add failing parity test `prompt_sites_have_no_stale_graph_contract`**

Assert none of the six F3 sites contains the literal `include_related: true` or the claim that default mode is `vector`; assert each contains `hybrid` and `include_references` where applicable.

- [ ] **Step 5: Update the six prompt/doc sites**

Run: `cargo test --test upgraded_search_test prompt_sites_have_no_stale_graph_contract -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Focused verification**

Run: `cargo test --test upgraded_search_test -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/server.rs AGENTS.md .agents/skills/neurostrata/SKILL.md .opencode/skills/neurostrata/SKILL.md src/task/mod.rs src/task/wiring.rs src/task/harness_plugins.rs tests/search/upgraded_search_test.rs
git commit -m "fix: repair search_memory graph contract and stale prompt guidance"
```

---

### Task 2: Canonical routing contract module

**Files:**

- Create: `src/task/routing.rs`
- Modify: `src/task/mod.rs` (module declaration)
- Test: `tests/agent_adoption_test.rs` (new)

**Interfaces:**

- Consumes: spec §6 R4 table.
- Produces: `agent_routing_contract_json() -> serde_json::Value`, `evaluate_triggers(TaskContext) -> Vec<RouteTrigger>`, `next_action(trigger: &RouteTrigger, ids: &[String]) -> Value`; `RouteTrigger { id: &'static str, tool: &'static str, args: Value, reason: String }`.

- [ ] **Step 1: Write failing test `routing_contract_has_all_seven_triggers`**

Assert canonical JSON contains exactly `T_PATH`, `T_AMBIGUOUS`, `T_HIGH_BLAST`, `T_DIFF`, `T_DOMAIN_NEW`, `T_STRATEGIC`, `T_INVARIANT`, each with schema-valid tool name and required args.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test agent_adoption_test routing_contract_has_all_seven_triggers -- --nocapture`
Expected: FAIL (module missing).

- [ ] **Step 3: Write failing test `routing_abstains_on_routine_single_file_edit`**

Assert a context with one staged file, no blast-radius signal, non-strategic title produces zero triggers (pins Review Focus item 5).

- [ ] **Step 4: Run tests to verify both fail**

Run: `cargo test --test agent_adoption_test routing_ -- --nocapture`
Expected: FAIL (module missing).

- [ ] **Step 5: Implement `src/task/routing.rs`**

Pure data + deterministic evaluation; no store access. Abstention returns empty vector.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test agent_adoption_test routing_ -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/task/routing.rs src/task/mod.rs tests/agent_adoption_test.rs
git commit -m "feat: add canonical agent routing contract"
```

---

### Task 3: Real graph edges in task and subagent context

**Files:**

- Modify: `src/task/mod.rs:705` (`build_task_graph_context`), `src/task/mod.rs:823` (`related_edges`), prompt constants `src/task/mod.rs:854,2974,3056`
- Test: `tests/agent_adoption_test.rs`

**Interfaces:**

- Consumes: `crate::task::routing::{evaluate_triggers,next_action}` from Task 2; store graph edge API.
- Produces: context JSON fields `related_edges: Value[]`, `next_hop_calls: Value[]`, `expansion_error: string|null`; prompt length ≤3,500 chars.

- [ ] **Step 1: Write failing test `task_context_carries_real_related_edges`**

Assert `related_edges` is non-empty when a seeded memory has a recorded `GOVERNS` edge, and that `next_hop_calls[0].tool` is `neurostrata_related`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin neurostrata-mcp task_context_carries_real_related_edges -- --nocapture`
Expected: FAIL (empty edges).

- [ ] **Step 3: Implement hybrid seed selection + bounded edge collection**

At most five seeds, 15 edges; on error set `expansion_error` and keep empty edges. Never block task create/claim.

- [ ] **Step 4: Write failing test `golden_rules_survive_graph_expansion_budget`**

Assert Golden Rules remain present and ordered first after truncation, and total prompt ≤3,500 chars.

- [ ] **Step 5: Run tests**

Run: `cargo test --bin neurostrata-mcp task_context_ -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/task/mod.rs tests/agent_adoption_test.rs
git commit -m "feat: surface real graph edges in task context"
```

---

### Task 4: Conditional escalation from `memories_for_file`

**Files:**

- Modify: `src/server.rs:3381` (`handle_memories_for_file` response assembly)
- Test: `tests/graph_read/memories_for_file_test.rs`

**Interfaces:**

- Consumes: `next_action` shape from Task 2.
- Produces: response fields `status`, `next_actions`, `suppressed_reason?`.

- [ ] **Step 1: Write failing test `high_blast_radius_escalates_to_two_hop`**

Assert `blast_radius_count:16` yields `next_actions[0].tool == "neurostrata_related"` and `args.max_hops == 2`.

- [ ] **Step 2: Write failing test `empty_lookup_routes_to_locate`** (pins Review Focus item 2)

Assert empty result has `status:"empty"` and a `T_AMBIGUOUS` action to `neurostrata_locate`; assert no field claims governance absence.

- [ ] **Step 3: Write failing test `ordinary_single_file_edit_does_not_escalate`**

Assert `blast_radius_count:3` and no bridges produce empty `next_actions`.

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test --test memories_for_file_test -- --nocapture`
Expected: FAIL (fields absent).

- [ ] **Step 5: Implement response fields and triggers**

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test memories_for_file_test -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/server.rs tests/graph_read/memories_for_file_test.rs
git commit -m "feat: add conditional graph escalation to memories_for_file"
```

---

### Task 5: Snapshot readiness section (MCP payload only)

**Files:**

- Modify: `src/server.rs:5474` (`handle_get_snapshot`)
- Test: `tests/agent_adoption_test.rs`

**Interfaces:**

- Consumes: taxonomy view filters (`src/rdf/taxonomy.rs:416,3443`).
- Produces: text section `### Graph & Ontology Readiness` with `taxonomy_concepts_count`, `ungrounded_concepts_count`, `active_task_graph_tool_diversity`, `recommended_reads[]`; zero-concept hint line.

- [ ] **Step 1: Write failing test `snapshot_readiness_excludes_restricted_taxonomy`** (pins Review Focus item 3)

Seed one restricted concept and one unrestricted; assert readiness counts include only the unrestricted one.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test agent_adoption_test snapshot_readiness_excludes_restricted_taxonomy -- --nocapture`
Expected: FAIL (section absent).

- [ ] **Step 3: Implement readiness section and zero-concept hint**

No UI files. Preserve Golden Rules ordering.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test agent_adoption_test snapshot_readiness_ -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server.rs tests/agent_adoption_test.rs
git commit -m "feat: add graph and ontology readiness to snapshot"
```

---

### Task 6: Honest post-dispatch telemetry

**Files:**

- Modify: `src/task/mod.rs:1094` (`record_graph_query_with_root`)
- Modify: `scripts/hooks/pre-commit:284,315,346`
- Test: `tests/agent_adoption_test.rs`

**Interfaces:**

- Consumes: spec §6 R6 field list.
- Produces: `record_tool_receipt(args: ReceiptFields)`; `session_queries.json` gains `events[]` alongside legacy `queries[]`.

- [ ] **Step 1: Write failing test `precommit_no_longer_fabricates_ledger_rows`** (pins Review Focus item 4)

Run the hook fixture against a staged file with no lookup; assert no row with `tool == "pre-commit"` is written.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test agent_adoption_test precommit_no_longer_fabricates_ledger_rows -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Write failing test `receipts_are_private_and_digest_only`**

Assert receipts contain no raw `query`/`content` fields and that mesh export contains zero receipt rows.

- [ ] **Step 4: Run tests to verify both fail**

Run: `cargo test --test agent_adoption_test -- --nocapture`
Expected: FAIL.

- [ ] **Step 5: Remove fabrication and add post-dispatch recorder**

Remove the three F6 synthesis sites; record all specified receipt fields only after a tool returns. Legacy rows retained but excluded by readers.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test agent_adoption_test receipt_ -- --nocapture && cargo test --test egress_test -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Verify hook sources unchanged and fixtures pass**

Run: `TMPDIR=/var/tmp/ns-tmp cargo test --bin neurostrata-mcp pre_commit_hook_sources_stay_byte_identical -- --nocapture && bash scripts/test-staged-quality.sh`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/task/mod.rs scripts/hooks/pre-commit tests/agent_adoption_test.rs
git commit -m "fix: replace synthetic ledger entries with post-dispatch receipts"
```

---

### Task 7: Advisory validation from unresolved triggers

**Files:**

- Modify: `src/task/mod.rs:2861` (`handle_task_validate`), `src/task/gate.rs` (`GateReport`, `validate_json`)
- Test: `tests/agent_adoption_test.rs`

**Interfaces:**

- Consumes: receipts from Task 6, routing triggers from Task 2.
- Produces: `advisories.graph_ontology` object per spec §6 R7; advisory codes `W_GRAPH_NO_EXPANSION`, `W_DIFF_TOUR_RECOMMENDED`, `W_ONTOLOGY_STRATEGIC_MISS`.

- [ ] **Step 1: Write failing test `validate_warns_only_on_unresolved_applicable_triggers`**

Seed a genuine one-hop lookup only; assert no warning. Seed an applicable `T_HIGH_BLAST` with no receipt; assert exactly `W_GRAPH_NO_EXPANSION` in `advisories`, absent from `violations`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test agent_adoption_test validate_warns_only_on_unresolved_applicable_triggers -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Implement advisory computation**

Read receipts fail-soft; no advisory ever enters `violations` by default.

- [ ] **Step 4: Run tests**

Run: `cargo test --bin neurostrata-mcp gate_ -- --nocapture && cargo test --test agent_adoption_test validate_ -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/task/mod.rs src/task/gate.rs tests/agent_adoption_test.rs
git commit -m "feat: add non-blocking graph and ontology advisories to validation"
```

---

### Task 8: Evaluation contract and honest gate

**Files:**

- Create: `eval/agent_adoption/evaluation_contract.json`
- Modify: `tests/semantic_eval_metrics_test.rs`
- Test: `tests/agent_adoption_test.rs`

**Interfaces:**

- Consumes: spec §6 R8 thresholds.
- Produces: JSON contract with `thresholds`, `task_sets`, `production_roi_claimed:false`, `semantic_lift_gate`.

- [ ] **Step 1: Write failing test `evaluation_contract_matches_spec_thresholds`**

Assert exact values: route coverage 0.95, follow-through 0.80, precision 0.90, unnecessary ontology 0.05, accuracy lift 0.10, regression −0.02, response p95 32768 bytes, routing p95 750 ms, ontology lift 0.10.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test agent_adoption_test evaluation_contract_matches_spec_thresholds -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Write failing test `semantic_lift_gate_remains_honest`**

Assert the gate reports FAIL while measured lift <0.10 and that `production_roi_claimed` is false.

- [ ] **Step 4: Run tests to verify both fail**

Run: `cargo test --test agent_adoption_test -- --nocapture`
Expected: FAIL.

- [ ] **Step 5: Create the JSON contract**

No Python. Use `write`/`jq` only.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test agent_adoption_test evaluation_contract_ -- --nocapture && cargo test --test semantic_eval_metrics_test -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add eval/agent_adoption/evaluation_contract.json tests/agent_adoption_test.rs tests/semantic_eval_metrics_test.rs
git commit -m "test: pin agent adoption evaluation thresholds"
```

---

## Completion Gate

Before declaring done:

- `cargo check --bin neurostrata-mcp`
- `cargo test --test agent_adoption_test -- --nocapture`
- `cargo test --test upgraded_search_test -- --nocapture`
- `cargo test --test memories_for_file_test -- --nocapture`
- `cargo test --test semantic_eval_metrics_test -- --nocapture`
- `bash scripts/test-staged-quality.sh`
- `git diff --name-only` contains no `web-ui/` or `src-tauri/` path.
