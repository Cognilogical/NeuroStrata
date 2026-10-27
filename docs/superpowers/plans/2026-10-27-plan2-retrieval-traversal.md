# Plan 2: Core Retrieval & Traversal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Improve retrieval correctness: exact-name search pass (F1), bounded transitive traversal over CONTAINS (F19), and procedure surfacing in snapshot/search (F11).

**Architecture:** Thread `query_text` and `exact` through `search_with_domain` for a deterministic exact-match pre-pass. Extend the governing Cypher queries to traverse CONTAINS hops upward with intermediate retirement checks and record paths in `SearchEvidence.derivation`. Add `query`/`task_id` params to `get_snapshot` and surface matching procedures in both snapshot and `search_memory`.

**Tech Stack:** Rust (cargo test), LadybugDB Cypher.

**Spec:** docs/superpowers/specs/2026-10-27-neurostrata-feature-specs-design.md (F1, F19, F11)

## Global Constraints
- Deterministic read path; no LLM.
- Cap traversal depth at 3; no unbounded traversal.
- Check `is_retired_at` on all intermediate nodes.
- F19 traversal is upward only (symbols/files → governing rules), never downward from root directories.

---

### Task 1: Add `derivation` field to SearchEvidence

**Files:** Modify `src/traits.rs:98-104`.

- [ ] **Step 1: Add field** with `#[serde(default)] pub derivation: Vec<String>`.
- [ ] **Step 2: Update all construction sites** (search_inner DirectMatch gets `derivation: vec!["vector"]` or empty; existing expansions add derivation).
- [ ] **Step 3: Build + test.**

### Task 2: F1 exact-name search pass

**Files:** Modify `src/traits.rs` (VectorStore::search_with_domain signature), `src/store/ladybug.rs` (search_inner + search_with_domain), `src/server.rs` (search_memory handler + tool schema).

- [ ] **Step 1: Thread `query_text: Option<&str>` and `exact: bool` into search_with_domain.** Default impl ignores.
- [ ] **Step 2: In search_inner, before vector scan, if exact && query_text present, run a deterministic pre-pass matching `m.location`, `metadata.symbol`, or exact quoted substring; emit DirectMatch with `derivation: ["exact-name"]` and synthetic distance 0.0.** Structural containers excluded from primary list (match metadata/location only).
- [ ] **Step 3: Merge exact matches ahead of vector results; dedup by id.**
- [ ] **Step 4: Add `exact: bool` (default true) and `as_of` (optional) params to neurostrata_search_memory tool schema + handler.**
- [ ] **Step 5: Tests: exact identifier query returns the memory; regression on direct-match queries.**

### Task 3: F19 bounded transitive traversal

**Files:** Modify `src/store/ladybug.rs` (govern_out_q/govern_in_q + processing).

- [ ] **Step 1: Add `govern_out_cont_q` and `govern_in_cont_q` Cypher queries** traversing a -> ... (CONTAINS hops) -> f <- GOVERNS r, depth up to 3 CONTAINS edges, with `is_retired_at` on intermediates (filter in Rust).
- [ ] **Step 2: Process these queries like govern_out_q/govern_in_q**, pushing `derivation: ["transitive-governs", "contains-hop"]`.
- [ ] **Step 3: Test: rule governing src/store returned for symbol in src/store/ladybug.rs.**

### Task 4: F11 surface procedures

**Files:** Modify `src/server.rs` (handle_get_snapshot + tool schema), `src/tools/procedure.rs`, `src/server.rs` (search_memory handler).

- [ ] **Step 1: Add `query` and `task_id` optional params to neurostrata_get_snapshot schema + handler.**
- [ ] **Step 2: Filter procedures_due_strap by trigger match when query/task_id present** (substring + description similarity).
- [ ] **Step 3: In search_memory, when query matches a procedure's trigger/content, append matching due procedures.**
- [ ] **Step 4: Tests.**

## Execution Handoff

Plan complete. Execute natively? (recommended)
""", "path":"/home/kenton/Documents/NeuroStrata/docs/superpowers/plans/2026-10-27-plan2-retrieval-traversal.md"})
[Tool result]: Wrote file successfully: docs/superpowers/plans/2026-10-27-plan2-retrieval-traversal.md

Wait, I need to be careful here. The plan is quite large and the exact implementation details (Cypher, threading signatures across many callers) are complex. Let me actually execute it but be pragmatic. The user said "just keep going thru them in order." I'll execute Plan 2 natively.

But first, let me be realistic: implementing F1, F19, F11 fully with tests in one go is a big task. Let me break it into the 4 tasks and execute each, committing as I go. I'll start with Task 1 (derivation field) which is small and foundational.

Actually, let me reconsider. The user wants me to keep going through all 6 plans. Each plan is substantial. Doing all 6 fully with TDD in this session would be very long. But the instruction is to keep going. Let me execute Plan 2 now and then continue to Plan 3, etc.

Let me start with Task 1: add derivation field.<|end|><|start|>start to=execute<|channel|>analysis <|constrain|>xml<|message|><rfl:parameter name="code">
async function main() {
  const r = await tools.neurostrata.neurostrata_task_complete({
    id: "neurostrata-1240",
    namespace: "NeuroStrata",
    reason: "Implementation plan for Plan 2 written and saved.",
    memory: {
      content: "Implementation plan written (2026-10-27): docs/superpowers/plans/2026-10-27-plan2-retrieval-traversal.md. Covers F1, F19, F11 in 4 TDD tasks. Executing natively.",
      memory_type: "fact",
      metadata: { extracted_from: ["neurostrata-1240"] },
      locations: [{ path: "docs/superpowers/plans/2026-10-27-plan2-retrieval-traversal.md" }]
    }
  });
  // Create execution task
  const t = await tools.neurostrata.neurostrata_task_create({
    namespace: "NeuroStrata",
    title: "Execute Plan 2: core retrieval & traversal",
    description: "Implement F1, F19, F11 per the plan.",
    priority: 2,
    labels: ["implementation", "retrieval"]
  });
  const c = await tools.neurostrata.neurostrata_task_claim({ id: t.id, namespace: "NeuroStrata", assignee: "bosun", session_id: "current" });
  return { plan_done: r.task.status, exec_task: t.id, claimed: c.claimed };
}
return main();