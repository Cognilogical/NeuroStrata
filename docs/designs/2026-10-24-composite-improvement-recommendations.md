# Composite Improvement Recommendations — NeuroStrata

**Sources:** Three independent deep-dive verdicts:
- Astra (`openai-arch`) — `docs/designs/2026-10-24-astra-deep-dive-verdict.md` (15 recs: 4 P0, 10 P1, 1 P2)
- Argon (`gemini-arch`) — inline verdict (11 recs: 4 P0, 4 P1, 3 P2)
- K3 (`k3-arch`) — inline verdict (12 recs: 3 P0, 4 P1, 5 P2)

**Baseline:** `d83ee2a` (v1.11.0). **Compositor:** primary agent. No new findings added beyond those three verdicts; overlapping recommendations merged.

## Consensus summary

All three architects agree on the project's core value (source-anchored graph memory + task completion funnel requiring extracted knowledge) and on the same top risks:
1. **Unauthenticated loopback daemon** (unanimous P0).
2. **Guard is advisory, not enforcement** (unanimous P0/P1).
3. **Cypher string interpolation / injection surface** (Argon + K3 P0/P1).
4. **Learning/Laya subsystem is stubs presented as capability** (all three).
5. **Intake resolve reports success without creating a task** (Astra + Argon).
6. **Ingest can destroy a good graph on failure** (Astra P0; Argon/K3 note it).
7. **Task claim exclusivity is non-atomic** (Astra P0; Argon P0 for write timeouts).

Divergences: Astra emphasizes embedding-space identity, privacy/egress, and typed persistence boundaries; Argon emphasizes storage-engine migration (SQLite) and full-namespace scans; K3 emphasizes the red HEAD test, exact-identifier recall, and module splitting.

---

## Unified Recommendations (28 items)

### P0 — Critical (safety / correctness / data integrity)

| # | Title | Flagged by | Rationale | Concrete next action | Effort | Deps |
|---|---|---|---|---|---|---|
| 1 | **Authenticate daemon HTTP surface + restrict sandbox paths** | Astra R01, Argon #2, K3 R1 (unanimous) | `/delete`, `/edit`, `/set-metadata`, `/backup`, `/shutdown`, `/validate` have no auth; any local process or browser tab can mutate/execute. | Per-user bearer token (0600), Host/Origin validation, restrict `cwd`/`dir` to registered project roots, pin sandbox image. | 3–6 d | — |
| 2 | **Fix red HEAD test** (`config::invalid_bind_or_unbind_leaves_config_bytes_unchanged`) | K3 R2 | `bind_project_context("bad", …)` accepted where test expects rejection; either validation regressed or test stale. | Decide source-id validation contract; fix code or test. | XS | — |
| 3 | **Whitelist Cypher relation names before interpolation** | Argon #7, K3 R3 | `rel_type` flows from metadata into raw query text unescaped (`ladybug.rs:~1364`). | Validate against `^[A-Z][A-Z0-9_]*$`; add adversarial fuzz test. | S | — |
| 4 | **Guard hardening: real trigger matcher + distance threshold + shared validator** | Astra R02, Argon #1, K3 R4 (unanimous) | `pattern.contains(action)` rejects every bash when any rule mentions "bash"; top-3 candidates with no distance cutoff; `block_in_place` panics on single-thread runtime; validator rebuilt per call. | Typed action/rule matching, calibrated max-distance cutoff, async init, share via `AppState`, wire `SemanticEngine::evaluate_action`. | 5–10 d | #1 |
| 5 | **Hold write lock inside `spawn_blocking` (prevent concurrent lbug C++ access on timeout)** | Argon #3 | Timed-out async caller releases `write_lock` while OS thread still in non-thread-safe C++ engine → corruption. | Move lock guard into the `spawn_blocking` closure. | S (1 d) | — |
| 6 | **Ingest: complete, non-destructive on failure (fix queue-saturation `Ok(())`)** | Astra R03 | `try_send` error treated as success after clearing old graph → partial/empty graph reported successful. | Blocking backpressure; distinguish closed vs. saturated; staged generation activation; preserve prior generation on failure. | 3–7 d | #14 |
| 7 | **Atomic task claim / lifecycle mutations (CAS on expected version+holder)** | Astra R04 | Non-atomic read/check/write; two callers can both claim; empty session IDs treated as same holder; pulse emitted before save. | Store-level conditional transitions with idempotency key; emit consequences only after committed transition. | 4–8 d | #14 |
| 8 | **Intake resolve must create or verify a real task (no false success)** | Astra R05, Argon #4 | `Disposition::Created` sets `task_created: true` without creating any task row. | Atomic task creation + candidate resolution; reject missing/blank/wrong-namespace/non-task IDs on link path. | 2–4 d | #7, #14 |

### P1 — High-value reliability / product gaps

| # | Title | Flagged by | Rationale | Concrete next action | Effort | Deps |
|---|---|---|---|---|---|---|
| 9 | **Mesh publication durability (close both loss windows)** | Astra R06 | Lossy bus + `AlreadyApplied` skip-commit path can acknowledge unpublished records; no guaranteed convergence bound. | Durable publication intent with source mutation; fix AlreadyApplied commit; source-to-mesh revision reconciliation. | 5–9 d | #14 |
| 10 | **Learning/Laya: truthful capabilities (gate or implement)** | Astra R07, Argon #6, K3 R6 (unanimous) | `LayaOnnxProvider` returns empty; `MockJudgmentProvider` substituted; trainer/dataset/control are no-ops; README oversells. | Mark incomplete ops `unsupported`/`unavailable`; gate behind cargo feature; implement or align docs. | 2–4 d (gating) / 10–20 d (full) | #8? |
| 11 | **Version embedding spaces + validate remote outputs** | Astra R08 | Same-width model switch silently invalidates ranking; remote embedder substitutes 0.0 for nonnumeric. | Persist space identity (provider/model/revision/dims/normalization); reject incompatible opens; validate response width/finite. | 3–6 d | #14 |
| 12 | **Recall completeness + exact-identifier lane** | Astra R09, K3 R7 | `10*limit` pre-filter can hide live matches; no deterministic identifier/symbol lookup. | Apply lifecycle/domain predicates before candidate budget or paginate with incomplete marker; add CONTAINS/symbol-id branch. | 4–7 d | #11, #14 |
| 13 | **Privacy/egress enforcement at persistence + remote boundaries** | Astra R10 | Remote judgment/embedding send content+metadata; regex scanning is not sensitive-data classification; buffer/backup/git retention undocumented. | Authorize egress per namespace/data class; minimize metadata; scan at actual boundaries; document retention. | 4–7 d | #1, #11 |
| 14 | **Typed persistence boundary (invariants, CAS, namespace mismatch rejection, shared commands across transports)** | Astra R14 | `upsert` merges on ID only (namespace not checked); metadata open JSON; concrete `LadybugStore` downcast in intake; duplicated predicates. | Conditional mutation APIs, typed errors, migration metadata; remove downcast; share application commands across transports. | 6–12 d | foundational |
| 15 | **Eliminate full-namespace table scans in hot paths** | Argon #5 | `store.list(namespace, None)` deserializes all rows (incl. thousands of AST nodes) on `get_snapshot`, `task_list`, `task_gate`, `procedure_perform`, `get_graph`. | Push `memory_type`/`user_id` filters into Cypher WHERE; add direct ID lookup for procedures; namespace-scoped export. | 2–3 d | — |
| 16 | **Cypher: prepared statements / native vector params (or query builder)** | Argon #7, K3 R8 | `format!` + `escape_ladybug_string` (backslash+quote only) across all queries; 768-float ASCII formatting overhead. | Parameterized queries with native float arrays; builder for hot write shapes; escape as defense-in-depth. | 3–4 d (params) / L (builder) | #3 |
| 17 | **Zero-Action Start honesty (ship client-shim interception or re-label wire)** | K3 R5 | Wire claims `blocks: true` but only guards voluntary callers; agents that never call guard get nothing. | Ship client shims routing write/edit/bash through guard_validate, or downgrade wire to advisory. | M | packaging decision |
| 18 | **Capability claims → executable documentation** | Astra R11, K3 R11 | README/CLI/AGENTS drift; vocabulary v2 text inside v6/v7 file; guard coverage overstated. | One authoritative capability registry (implemented/experimental/unavailable); generate tool descriptions; reconcile docs. | 2–4 d | #4, #10 |
| 19 | **Ceremonial tests → executable failure contracts** | Astra R12, K3 R2 | `assert!(true)` raw-distance contract; empty embed/guard tests; 1 red test on HEAD. | Replace with behavioral assertions + planted-regression tests; separate CI tiers. | 4–8 d | #12 |
| 20 | **Procedure acknowledgment + buffer writes recoverable** | Astra R13 | Finite-budget truth split from audit record; concurrent spend; rollover race; unvalidated root. | CAS-protect firing, persist event atomically with counter, per-root lock, size limits, durable pending-log state. | 3–6 d | #14, #13 |
| 21 | **Durability/load budgets + recovery evidence (specify + measure)** | Astra R15 | Operational guarantees are anecdotal; no p50/p95/p99 or recovery drills. | Define acknowledged-write semantics, ambiguous-timeout contract, bounded blocking work; build load/recovery harness; record identity in backup manifests. | 4–8 d | #8, #12, #14 |

### P2 — Hygiene / maintainability / long-term

| # | Title | Flagged by | Rationale | Concrete next action | Effort | Deps |
|---|---|---|---|---|---|---|
| 22 | **Unify CLI proxy coverage (`export-graph`, `restore`) + offline bus/mesh wiring** | Argon #9 | `export-graph` opens store directly → lock contention; CLI offline mutations skip mesh outbox. | Route through daemon when healthy; attach outbox subscriber to CLI's in-process bus. | S (1–2 d) | #1 |
| 23 | **Promote IntakeStore into VectorStore (remove `AsAny` downcast)** | Argon #10 | Handler downcasts `&dyn VectorStore` to `LadybugStore`; mocks silently fail. | Supertrait or explicit `Arc<dyn IntakeStore>` in `AppState`. | S (1 d) | #14 |
| 24 | **Evaluate storage engine migration (SQLite + sqlite-vec)** | Argon #11 | Disproportionate complexity exists solely to survive `lbug 0.20.4` bugs (SIGSEGV, WAL corruption, single-writer). | Prototype `SqliteVectorStore` implementing `VectorStore`+`IntakeStore`; benchmark latency/concurrency/crash resilience. | L (1–2 w) | #23 |
| 25 | **Drain panic surface in store layer** | K3 R9 | 83 `panic!`/`expect` in `ladybug.rs`; 562 `unwrap()` in server/store/task. | Convert hot-path unwraps to `anyhow` errors; add kill-during-write integration test. | M–L | — |
| 26 | **Pin sandbox image by digest; preflight availability** | K3 R10 | `alpine:latest` unpinned; network pull inside verdict path → spurious reject. | Resolve digest at install, store it, refuse unpinned; preflight at daemon start; surface in `/info`. | XS–S | — |
| 27 | **Reconcile vocabulary version strings** | K3 R11 | "Vocabulary v2" text inside v7 file. | Single pass over `tool_summaries`; consistency test. | XS | — |
| 28 | **Split `server.rs` (~7.7k) and `ladybug.rs` (~7.8k)** | K3 R12 | Two files ~28% of codebase; handler/query seams already exist. | Extract `src/handlers/*`; split ladybug along query/read/export. | M | after #16 |

---

## Architect-specific themes worth noting

- **Astra** uniquely flagged: embedding-space identity (R08), privacy/egress as a first-class threat (R10), typed persistence boundary (R14), load budgets/recovery evidence (R15), procedure/buffer recoverability (R13).
- **Argon** uniquely flagged: storage-engine migration to SQLite (R24), full-namespace table scans (R15), prepared statements (R16), CLI proxy coverage (R22), IntakeStore promotion (R23).
- **K3** uniquely flagged: red HEAD test (R2), exact-identifier recall (R12 overlaps Astra R09), Zero-Action Start honesty (R17), panic surface (R25), sandbox image pin (R26), vocabulary strings (R27), module splitting (R28).

## Suggested execution order (per Astra's ordering, reconciled)

1. **Immediate containment:** R1, R2, R3, R4 (auth, red test, Cypher whitelist, guard hardening).
2. **Disclose limitations:** R18 (capability claims) + R10 (learning honesty).
3. **Begin failure-contract tests:** R19 with planted regressions.
4. **Foundational invariants:** R14 (typed persistence boundary) — supplies shared invariants for R3–R9, R13, R20.
5. **Close serious gaps:** R5, R6, R8, R9, R11, R12, R13, R15, R16, R17, R21.
6. **Hygiene:** R22–R28.

This ordering does not require implementing the optional learning engine to ship a trustworthy memory/task product. The decided shipped behavior for an absent engine is explicit unavailability, not successful empty work.
