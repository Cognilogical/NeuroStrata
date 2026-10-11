# Agent Routing Strategy

**Date:** 2026-10-11
**Author:** BOSUN session
**Status:** live (supersedes nothing; first codified strategy)

## 1. Agent inventory (15 seats, 4 families)

### Architecture seats (design-only, independent verdicts; never implement)

| Seat | Model/provider | Role | Cost posture |
|---|---|---|---|
| `openai-arch` | GPT-6 Astra (Copilot) | 1st architect — independent verdicts | subscription |
| `anthropic-arch` | Claude via Copilot | 2nd architect — independent verdicts | subscription |
| `k3-arch` | Kimi K3 (Copilot, Go fallback) | 3rd architect + **reasoning augmentation** for the orchestrator | subscription |

Rule: orchestrator dispatches all three independently, reconciles. Never a
fallback for another architect. Design-only — no live Tebra lookups, no code.

### Build seats (write code)

| Seat | Model | For what | Notes |
|---|---|---|---|
| `worker` | space-bunny-free | routine, fully-specified coding | ~6x faster than mimo on execution work. NEVER novel/concurrency work. |
| `worker.fallback` | Go-pool model | same, when primary rate-limited | free-first, Go-pool-fallback, no silent PAYG |
| `craftsman` | mimo-v2.6-flash-free | **hard** coding: multi-file refactors, novel bugs, concurrency | slow lane. Won the 09-24 quality bake-off. NEVER routine work. |
| `craftsman.fallback` | deepseek-v4-pro (Go pool) | same, when primary rate-limited | backup ≠ qwen (owner: no two models falling back to same family) |

Rule: keep planning and final review with the parent; dispatch implementation.

### Verification seats (validate, never design, never implement)

| Seat | Model | For what |
|---|---|---|
| `checker` | free model | validates designs/verdicts/claims against source. READ-ONLY by design. |
| `checker-free` | free model | **code-work gate**: runs tests/lints/builds, checks every claim against source |
| `*.fallback` variants | Go-pool | rate-limit fallbacks (free-first policy) |

Rule: "run the gates" → `checker-free` (has shell). `checker` confirms/refutes
claims only; it is NEVER a third design opinion.

### Support seats

| Seat | Model | For what |
|---|---|---|
| `explore` | fast | codebase exploration, file search, keyword hunt. Specify thoroughness. |
| `reviewer` | space-bunny-free (free) | liberal first-pass code review. Findings-only, never implements. |
| `designer` | glm-5.3 | async depth seat; burns time on deep thinking; never blocks interactive |
| `general` | — | multi-step research, parallel units of work |

Escalation: risky changes (live-agent surface, billing/health-data,
concurrency, auth) → Sonnet-seated review, not the free reviewer.

## 2. Routing decision tree

```
task arrives
  ├─ needs design verdict?          → architect seats (3-way, reconcile)
  ├─ needs exploration only?        → explore (state thoroughness)
  ├─ fully-specified + routine?     → worker (FAST lane)
  ├─ novel / concurrency / multi-file? → craftsman (SLOW lane)
  ├─ code work finished?            → checker-free (gate: tests, lint, build, claims)
  ├─ claims to verify against source? → checker (read-only)
  ├─ finished implementation to review? → reviewer (liberal free pass),
  │     escalate to Sonnet seat if risky surface touched
  └─ orchestrator reasoning strained? → k3-arch (augmentation, per owner)
```

Hard rules (from owner directives on record):
- Free-first, Go-pool-fallback, **no silent PAYG**.
- No Claude models on non-Claude seats (rotation policy).
- No GLM on checker seats (Bosun's lineage; checker must differ).
- Background dispatch ONLY for independent work; foreground for blocking paths.

## 3. Parallelism model

Parallel-safe when tasks share no files and no state. The orchestrator owns
the merge (git), the reconciliation, and the gate.

| Pattern | Seats | Constraint |
|---|---|---|
| Multi-architect review | openai-arch + anthropic-arch + k3-arch | all background; reconcile verdicts |
| Independent feature slices | N × worker OR craftsman | file-disjoint or worktree-isolated |
| Explore-then-build | explore (bg) while architect (bg) | merge inputs at design |
| Build-then-verify | worker → checker-free | sequential per unit; parallel across units |

**Write-contention rule:** two workers editing the same file = serialize or
pre-scope disjoint file sets in each dispatch. Rust single-binary crate:
`src/main.rs` `mod` declarations and `Cargo.toml` are contention points —
the orchestrator wires those once, then workers own their module files.

## 4. Applied to SynapticMesh (current fan-out)

Foundation (slice 1) is merged: `src/synaptic_mesh/{mod,manifest,record}.rs`
on main. Remaining slices 2–6 pre-scoped:

| Slice | Unit of work | Seat | Parallel? |
|---|---|---|---|
| 2a | mesh storage: init/join/leave (mkdir, git init, manifest write, pointer file) | craftsman | parallel with 2b |
| 2b | CLI scaffold + MCP tools surface for synaptic-mesh | worker | parallel with 2a — contention: main.rs; orchestrator wires main.rs after both land |
| 3 | outbox table + mesh-worker (drain → file → commit) | craftsman | after 2 (depends on storage + types) |
| 4 | pull/replay (fetch → staging → validate → apply) | craftsman | parallel with 3 — file-disjoint modules; integration after both |
| 5 | trust/eligibility publish gate | worker | with 3/4 (small, fully specified) |
| 6 | lifecycle enforcement at import | worker | with 3/4 (small, fully specified) |
| 7 | concurrent merge policy + conflict overlay | craftsman | after 4 (needs replay) |
| 8 | acceptance evidence suite (openai-arch §5 scenarios) | craftsman | last; after integration |

Verification loop: every worker/craftsman delivery goes to `checker-free`
(gates + claims-vs-source) before merge. Risky surfaces (outbox = durability,
mesh-worker = concurrency) escalate review beyond the free reviewer.

## 5. Seat starvation (learned 2026-10-11)

The 2026-10-10/11 SynapticMesh fan-out ran through a real seat-starvation
episode. Recorded so the next crunch inherits the countermeasures instead of
rediscovering them:

- **The paid Go pool is a single point of failure.** A billing/wallet wall
  disabled 8 of 15 seats overnight. Any fleet whose free capacity rides one
  provider wallet has one wallet's failure domain. Route as if the wallet
  can disappear at any moment: keep two donor seat families per critical
  lane (design, build, verify) if the fleet allows it.
- **Free primaries rate-limit intermittently, not permanently.** The honest
  response is NOT to promote a paid model permanently "because it's there";
  it is queue-and-continue: when the primary returns, the fee switch flips
  back. Paid-second-line is a fallback, not a routing upgrade.
- **Checkpoint-WIP-on-death saved every unit that died mid-flight.** Subagent
  prompts that pin the worktree + commit hash + task id + next action let any
  successor resume exactly at the hand-off point instead of re-grepping the
  whole repo. Mandate state snapshots in every hand-off (the task's Breath
  note is the right place to write them).
- **Checker-session truncation → narrowed decisive-item continuations.** When
  a free reviewer dies after listing its findings, re-dispatch a NARROWED
  continuation: the verdict artifacts + the remaining decisive items only,
  never "re-review everything". Truncated breadth recovered this way cost
  ~1 rerun, not a from-scratch re-verification.
- **Identifier discipline: never cite hashes from memory.** A worker quoting  a commit sha it last glanced at entangles the whole review loop with a
  wrong-parent rebase. The pre-push chain caught it, but the cheap invariant
  is: `git rev-parse` immediately before citing, cut-paste, never recall.