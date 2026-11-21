# 2026-11-20 — Taxonomy Bounds and Contract Hygiene (W-D)

> **Baseline**: `cognilogical/main` @ `216e9d56`
> **Issue**: #334 (P2: C1/C2/C3)
> **Branch**: `fix/wd-taxonomy-bounds`
> **Workflow**: spec → red evals → minimal fix → green evals → regression suites

---

## 1. Scope

The taxonomy operation layer (`src/rdf/taxonomy.rs`) and the shared request
handler (`src/handlers/taxonomy.rs`). Three adjacent defects:

- **C1:** `compare` runs uncapped quadratic work over caller-controlled
  `slugs` and `axes`.
- **C2:** `tree` documents `limit`/`max_hops` but ignores them.
- **C3:** `TaxonomyRequest` accepts unknown fields and silently discards
  them.

Non-goals: slug normalization semantics, SKOS/Turtle output, thesis and
concept storage shape, and blanket `deny_unknown_fields` (explicitly avoided:
aliases and MCP routing keys make strict rejection risky).

---

## 2. Invariants

1. A taxonomy request's work is bounded by a fixed cap, independent of
   caller-supplied collection sizes.
2. Documented query controls (`limit`, `max_hops` on `tree`) either take
   effect or leave the contract; they are never silently ignored.
3. A misspelled/unknown request field is observable to operators instead of
   vanishing without a trace.

---

## 3. Behavior specification

### C1 — compare caps

- `compare` accepts at most **64 distinct slugs** and at most **32
  evaluation axes** (caller-supplied `axes`, thesis axes, or the union of
  concept scores).
- Caps are checked before the pairwise/ancestor work. Over-cap requests
  return a deterministic error naming the cap; requests are never silently
  truncated.
- The slug cap is enforced after strict normalization/dedup; the axis cap
  after axis resolution.

### C2 — tree controls

- `max_hops` caps the tree depth below each root: `Some(n)` is clamped to
  `1..=4`; `None` keeps the current unlimited behavior. Shared DAG nodes are
  revisited when a shorter path leaves a larger remaining budget, so a
  depth-limited tree does not lose descendants reachable by that path (the
  node may appear under both parents).
- `limit` caps the number of returned root nodes: `Some(n)` is clamped to
  `1..=50`; `None` returns all roots.
- Both are backward-compatible: omitted parameters produce today's full
  tree.

### C3 — unknown fields

- The handler computes unknown request keys against the `TaxonomyRequest`
  field set (derived from a default serialization, plus the accepted aliases
  `action`, `alt_labels`, `related`) and logs them at debug level with the
  operation/namespace context. Recognised fields keep their current
  semantics; unknown fields remain ignored, not rejected.
- The helper is public so the behavior is testable without a tracing
  subscriber.

---

## 4. Evals

Red on baseline:

- `compare_rejects_slugs_beyond_cap` — 65 normalized slugs return an error
  naming the 64-slug cap, before concept lookup.
- `compare_rejects_axes_beyond_cap` — two valid concepts + 33 axes return an
  error naming the 32-axis cap. Baseline returns a matrix.
- `tree_honours_max_hops` — a 3-level chain with `max_hops=1` returns one
  level of children. Baseline returns the full chain.
- `tree_honours_limit` — two top-level roots with `limit=1` return one root.
  Baseline returns both.
- `tree_revisits_shared_nodes_with_a_larger_budget` — in a DAG with a
  shorter direct path, `max_hops` still expands the descendants reachable
  through it.

Added with the fix (helper did not exist to compile against):

- `unknown_taxonomy_request_fields_reports_only_unrecognised_keys` —
  `typo_field` is reported; `action`/`related` aliases are not.

---

## 5. Acceptance

- The four baseline evals are red at `216e9d56` and green after the fix.
- Existing taxonomy tests (compare matrix, tree, briefing, gaps, advise,
  ReBAC, CAS) stay green.
- `cargo test --bin neurostrata-mcp taxonomy` and the final daemon bin suite
  pass.
- No MCP/HTTP/CLI schema change; caps and controls are documented in
  `docs/features-api/subsystems/taxonomy-advisor.md` (watched surface for
  `src/rdf/taxonomy.rs`).

## 6. Doc gate

`src/rdf/taxonomy.rs` requires a co-change to
`docs/features-api/subsystems/taxonomy-advisor.md`; the doc names the caps
and the tree-control semantics.
