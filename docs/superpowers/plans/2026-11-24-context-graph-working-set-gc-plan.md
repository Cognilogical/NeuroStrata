# Generational Context Graph (L1 Working-Set GC) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the deterministic Generational Context Graph L1 Working-Set Garbage Collector (`<= 4096` UTF-8 bytes per turn) across the Rust/Lean 4 formal verification kernel, `@neurostrata/plugin-core`, and all 6 harness adapters and native hook installers.

**Architecture:** LadybugDB + Episodic Buffer serve as the persistent heap while `.NeuroStrata/active_session.json#working_set` maintains a scoped L1 working-set index (`Immortal`, `PinnedRoot`, `YoungGen`, `EvictedStub`). A pure formal kernel (`evaluate_working_set_eviction` in Rust, Lean 4, and TypeScript mirror) drives per-turn mark-sweep eviction, pointer stubbing, page-fault re-expansion, and tool-output dehydration across all six CLI/terminal agent harnesses.

**Tech Stack:** Rust (`src/formal_kernel.rs`, `src/task/harness_plugins.rs`), Lean 4 (`proofs/NeuroStrata/WorkingSetGc.lean`), TypeScript/Node.js ESM (`plugins/core/index.ts`, zero external npm dependencies, `node:fs`/`node:os`/`node:path`/`node:crypto`).

**Spec:** `docs/superpowers/specs/2026-11-24-context-graph-working-set-gc-design.md`

## Global Constraints

- New state and contracts are strict JSON only (`1ecfd804`).
- No Python implementation or tooling (`bf710e0e`).
- No competitor-derived artifact names (`9e64a5a8`).
- Every requirement is implemented in this change set; no unresolved marker is
  permitted (`a97d0b19`).
- Golden Rules remain immortal and precede all non-immortal frame content
  (`d104c715`).
- Universal Core plus Client Shims is mandatory (`d00a50f4`).
- `@neurostrata/plugin-core` remains ESM with only `node:fs`, `node:os`,
  `node:path`, and `node:crypto` runtime imports (`f988ef2d`).
- Focused verification only; no full-workspace test run in a subagent lane
  (`f689fec8`).
- `WORKING_SET_MAX_FRAME_BYTES` is exactly `4096`; it is not configurable.
- All GC filesystem writes use the existing `writeJsonAtomically` helper.

## Review Focus

- UTF-8 multi-byte boundaries at 512-byte head/tail cuts and the 4096-byte frame
  cap → Task 2, `plugins/core/test.mjs`: `working-set GC cuts dehydration and
frames only on UTF-8 codepoint boundaries`.
- `[`/`]`/`%` injection through summaries, IDs, and Golden Rules → Task 2,
  `plugins/core/test.mjs`: `working-set GC escapes dynamic frame text and cannot
forge NeuroStrata markers`.
- `turn_seq` saturation at `u32::MAX` → Task 2, `plugins/core/test.mjs`:
  `working-set GC resets sequence, touch ages, identities, and fingerprints at
u32::MAX`.
- Project/namespace/session/epoch isolation and symlinked state rejection →
  Task 2, `plugins/core/test.mjs`: `working-set GC isolates scopes and epochs and
rejects symlinked or oversized state`.
- Codex result replacement/suppression fields unsupported by its pinned parser →
  Task 3, `plugins/codex/test.mjs`: `working-set GC PostToolUse never emits
updatedMCPToolOutput or suppressOutput`.

## Non-Goals

- No `web-ui/` or `src-tauri/` changes.
- No daemon endpoint, MCP tool, database migration, or RDF schema change.
- No LLM call, embedding call, or model-generated GC summary.
- No floating-point score, probability, clock-based TTL, or random tie-break.
- No runtime npm dependency, Rust crate, lockfile, or Cargo manifest change.
- No retrieval ranking, HNSW, Tantivy, RDF, SynapticMesh, ReBAC, Golden Rule,
  task-gate, or fail-soft semantics regression.
- No Python tooling and no erasure of host-owned transcript entries that expose
  no mutable or result-replacement surface.

## Current Evidence and Decisions

**Verified from source and the approved spec:**

- `src/formal_kernel.rs` is standalone, dependency-free, `Copy` plus `const fn`;
  its unit and Kani proofs share one `tests` module.
- Rust and Lean currently compare `1273` ordered rows in
  `rust_formal_kernel_truth_table_matches_lean_cli`.
- Core already has `writeJsonAtomically`, epoch hooks, `executeAfterHandler`,
  pending recall blocks, conceptual-bridge persistence, and the read-only tool
  classification; it has no GC engine or `PostToolUse` branch.
- Core currently imports only `node:fs`, `node:os`, and `node:path`.
- OpenCode, Claude, Codex, and GitHub CLI are thin core adapters; OpenClaw and
  Hermes are standalone TypeScript shims; the bundler currently writes only JS
  and declarations.
- Claude currently installs `SessionStart`, two `PreToolUse` entries, and `Stop`;
  Codex installs `PreToolUse` and `Stop`; GitHub CLI writes extension metadata
  rather than native hooks.

**Spec assumptions retained:** `active_session.json` remains the shared session
state file; a missing stable host session ID uses isolated in-memory state and
never persists or injects; daemon reads remain the ReBAC expansion authority.

**Plan decisions:** Task 2 commits the four regenerated core `dist/` artifacts
because its tests import `dist/index.js`. The spec names
`dehydrateRepeatedReadOutput` without a signature; this plan fixes the narrow
scope-aware signature below so §10.2 persistence has one unambiguous owner.

---

### Task 1: Formal Kernel, Lean 4 Proofs, Parity Corpus, and Product Vision Co-Change

**Files:**

- Modify: `src/formal_kernel.rs`
- Create: `proofs/NeuroStrata/WorkingSetGc.lean`
- Modify: `proofs/Main.lean`
- Modify: `tests/lean_parity_test.rs`
- Modify: `docs/vision/product-vision.md`

**Interfaces:**

- Consumes: existing standalone Rust kernel conventions, Lean CLI emitter, and
  the ordered Rust/Lean parity harness.
- Produces: exact Rust contract:

```rust
pub const WORKING_SET_MAX_FRAME_BYTES: u32 = 4096;
pub const WORKING_SET_STUB_MAX_BYTES: u32 = 192;

pub enum WorkingSetTier {
    Immortal,
    PinnedRoot,
    YoungGen,
    EvictedStub,
}

pub enum GcAction {
    RetainFull,
    PromoteToYoungGen,
    DemoteToStub,
    RetainStub,
}

pub struct WorkingSetEvictionInput {
    pub tier: WorkingSetTier,
    pub turn_age: u32,
    pub max_idle_turns: u32,
    pub node_bytes: u32,
    pub projected_frame_bytes: u32,
    pub max_frame_bytes: u32,
    pub is_re_referenced: bool,
}

pub const fn evaluate_working_set_eviction(
    input: WorkingSetEvictionInput,
) -> GcAction;
```

- All four types derive `Debug`, `Clone`, `Copy`, `PartialEq`, and `Eq`;
  Kani-only `Arbitrary` derives remain behind `#[cfg_attr(kani, ...)]`.
- Decision precedence is the spec §7.2 table: immortal always retains full;
  fitting re-referenced stubs promote; other stubs retain; idle young nodes
  demote; young overflow demotes; unreferenced pinned roots older than zero
  demote only under overflow; all other nodes retain full.
- Produces Lean theorems `immortal_never_evicted`, `page_fault_promotes_stub`,
  `idle_young_gen_demotes_to_stub`, `budget_overflow_demotes_non_immortal`, and
  `eviction_deterministic` with the exact qualified contracts in spec §7.3.
- Produces canonical rows:

```text
WORKING_SET_GC|tier|turn_age|max_idle_turns|node_bytes|projected_frame_bytes|max_frame_bytes|is_re_referenced|action_tag
```

- Action tags are `retain_full`, `promote_to_young_gen`, `demote_to_stub`, and
  `retain_stub`.
- Produces `2560` GC rows in this exact nested order, appended after the
  unchanged `1273` rows, for `3833` total rows:

| Dimension               | Values                                              |
| ----------------------- | --------------------------------------------------- |
| tier                    | `Immortal`, `PinnedRoot`, `YoungGen`, `EvictedStub` |
| `turn_age`              | `0`, `1`, `3`, `4`, `u32::MAX`                      |
| `max_idle_turns`        | `0`, `3`                                            |
| `node_bytes`            | `0`, `1`, `256`, `u32::MAX`                         |
| `projected_frame_bytes` | `0`, `4096`, `4097`, `u32::MAX`                     |
| `max_frame_bytes`       | `0`, `4096`                                         |
| `is_re_referenced`      | `false`, `true`                                     |

- [ ] **Step 1: Add failing Rust and parity tests**

Add `working_set_immortal_overflow_retains_full`,
`working_set_page_fault_promotes_only_when_fits`,
`working_set_ttl_equality_retains_and_expiry_demotes`,
`working_set_pinned_root_pressure_respects_current_turn`,
`working_set_zero_budget_is_total`, and
`working_set_maximum_integer_inputs` under `formal_kernel::tests`. Add Kani
proofs `proof_evaluate_working_set_eviction_total` and
`proof_evaluate_working_set_eviction_deterministic` inside the existing
`#[cfg(kani)] mod kani_proofs`. Extend `generate_rust_truth_table` with the exact
corpus above, assert section count `WORKING_SET_GC = 2560`, and change the total
assertion to `3833`.

- [ ] **Step 2: Run the focused tests to verify they fail**

```bash
cargo test --locked --bin neurostrata-mcp formal_kernel::tests::working_set -- --nocapture
cargo test --locked --test lean_parity_test rust_formal_kernel_truth_table_matches_lean_cli -- --nocapture
```

Expected: compilation fails on the missing working-set types/function, or the
new assertions fail before implementation.

- [ ] **Step 3: Implement the pure Rust decision kernel**

Add the constants, enums, input struct, and `const fn` beside the existing pure
kernels. Use only comparisons and `match`; no allocation, I/O, collection,
float, clock, or external crate. Preserve first-match precedence and total
behavior for every `u32` input.

- [ ] **Step 4: Implement the Lean model, proofs, and parity emitter**

Create `proofs/NeuroStrata/WorkingSetGc.lean` with the mirror tier/action model,
canonical ordering model, and all five named theorems. The budget theorem must
require `node_bytes > 192`, a valid stub of at most 192 bytes, and the eligible
young/unreferenced-pinned conditions; prove charged bytes fall by at least
`node_bytes - 192`. Import the module in `proofs/Main.lean` and emit the exact
nested corpus with matching tags. Use zero `sorry` and zero `axiom`.

- [ ] **Step 5: Update product vision and stale parity documentation**

Under the formal-verification optimization in `docs/vision/product-vision.md`,
document LadybugDB/Episodic Buffer as persistent heap, `working_set` as bounded
L1 cache, and the Rust/Lean/TypeScript GC decision kernel. Update parity comments
and assertions that still describe only `1273` rows.

- [ ] **Step 6: Run focused Rust, Lean, and Kani verification**

```bash
cargo test --locked --bin neurostrata-mcp formal_kernel::tests::working_set -- --nocapture
cargo test --locked --test lean_parity_test -- --nocapture
lake --dir proofs build
cargo check --locked --bin neurostrata-mcp
```

Expected: all working-set unit tests pass; parity compares all `3833` ordered
rows; proof-source scan finds zero `sorry`/`axiom`; Lean and Rust checks pass.
When Kani is installed, also run:

```bash
kani src/formal_kernel.rs --harness proof_evaluate_working_set_eviction_total
kani src/formal_kernel.rs --harness proof_evaluate_working_set_eviction_deterministic
```

- [ ] **Step 7: Commit the formal-kernel slice**

```bash
git add src/formal_kernel.rs proofs/NeuroStrata/WorkingSetGc.lean proofs/Main.lean tests/lean_parity_test.rs docs/vision/product-vision.md
git commit -m "feat: give working-set GC a Groundhog Day-proof reset"
```

Expected: one commit containing only the five Task 1 files.

---

### Task 2: Plugin Core L1 Working-Set Engine, Dehydration, Frame Renderer, Bundler, and Core Tests

**Files:**

- Modify: `plugins/core/index.ts`
- Modify: `plugins/core/bundle-adapter.mjs`
- Modify: `plugins/core/test.mjs`
- Rebuild and commit: `plugins/core/dist/index.js`,
  `plugins/core/dist/index.js.map`, `plugins/core/dist/index.d.ts`,
  `plugins/core/dist/index.d.ts.map`

**Interfaces:**

- Consumes: Task 1's exact decision precedence and action tags; existing core
  configuration, `writeJsonAtomically`, epoch hooks, tool classification,
  recall queues, bridge persistence, and daemon client.
- Produces: exact public TypeScript surface:

```text
WorkingSetNodeKind = "task_header" | "golden_rule" | "file" | "symbol" |
  "memory" | "ast_summary" | "conceptual_bridge" | "recall_text"
WorkingSetTier = "Immortal" | "PinnedRoot" | "YoungGen" | "EvictedStub"
WorkingSetGcAction = "retain_full" | "promote_to_young_gen" |
  "demote_to_stub" | "retain_stub"

WorkingSetScope = {
  projectRoot: string, projectId: string, namespace: string,
  sessionId: string, epoch: number
}
WorkingSetTouch = {
  id: string, kind: Exclude<WorkingSetNodeKind, "recall_text">,
  path?: string, symbol?: string, memoryId?: string
}
WorkingSetExpansionRequest = {
  id: string, expandTool: string, input: Record<string, unknown>
}
WorkingSetExpansion = (
  request: WorkingSetExpansionRequest
) => Promise<WorkingSetNode | WorkingSetStub | null>

evaluateWorkingSetEviction(input: {
  tier: WorkingSetTier, turn_age: number, max_idle_turns: number,
  node_bytes: number, projected_frame_bytes: number,
  max_frame_bytes: number, is_re_referenced: boolean
}) -> WorkingSetGcAction

loadWorkingSet(scope: WorkingSetScope) -> Promise<WorkingSetState>
persistWorkingSet(scope: WorkingSetScope, state: WorkingSetState) -> Promise<void>
advanceWorkingSetTurn(
  state: WorkingSetState, namespace: string, sessionId: string,
  projectRoot: string, touched: WorkingSetTouch[],
  expand: WorkingSetExpansion
) -> Promise<WorkingSetState>
sweepWorkingSet(state: WorkingSetState) -> WorkingSetState

dehydrateToolOutput(
  rawText: string, toolName: string, filePath: string | null,
  governingIds: string[]
) -> { text: string, dehydrated: boolean, digest?: ToolOutputDigest }
dehydrateRepeatedReadOutput(
  scope: WorkingSetScope, rawText: string, toolName: string,
  filePath: string | null, governingIds: string[]
) -> Promise<{ text: string, dehydrated: boolean, digest?: ToolOutputDigest }>

renderWorkingSetFrame(
  state: WorkingSetState, activeSession: Record<string, unknown>
) -> { text: string, usedBytes: number, fingerprint: string }
upsertWorkingSetFrame(sink: unknown, frame: string) -> boolean

runWorkingSetToolCycle(input: {
  scope: WorkingSetScope, turnId?: string, eventId?: string,
  toolName: string, toolInput: unknown, rawText: string | null,
  outputIsText: boolean, isError: boolean, advanceTurn: boolean
}) -> Promise<{
  replacementText: string | null, frame: string,
  fingerprint: string, dehydrated: boolean
}>
runWorkingSetContextCycle(
  scope: WorkingSetScope, eventId?: string
) -> Promise<{ frame: string, fingerprint: string, changed: boolean }>
runWorkingSetTurnBoundary(
  scope: WorkingSetScope, eventId?: string, turnId?: string
) -> Promise<{ frame: string, fingerprint: string, changed: boolean }>
markWorkingSetTouches(
  scope: WorkingSetScope, touched: WorkingSetTouch[], eventId?: string
) -> Promise<void>
```

- `WorkingSetNode`, `WorkingSetStub`, `ToolOutputStub`, and
  `ToolOutputDigest` use exactly the required/optional fields and validation in
  spec §§8.1–8.4; no extra fields are emitted.
- `WorkingSetState` has exactly `version`, `project_id`, `namespace`,
  `session_id`, `epoch`, `turn_seq`, `active_turn_id`, `recent_event_ids`,
  `max_frame_bytes`, `max_idle_turns`, `used_frame_bytes`,
  `frame_fingerprint`, `last_injected_fingerprint`, `roots`, `young_gen`,
  `evicted_stubs`, `tool_output_stubs`, and `tool_output_digests`.
- Export and enforce these exact constants: frame `4096`; idle TTL `3`; graph
  stub `192`; non-immortal roots `32`; young nodes `64`; graph stubs `128`; tool
  stubs `128`; tool digests `128`; event IDs `128`; injection keys `128`;
  active-session read cap `1048576`; inline threshold `3072`; head/tail `512`;
  tool pointer `512`; summary `512` UTF-8 bytes.
- Scope identity is SHA-256 of canonical absolute `projectRoot`; persisted scope
  is `(project_id, namespace, session_id, epoch)`. Missing session IDs disable
  persistence and injection.
- Bundler produces four fresh artifacts per adapter and supports
  `--compiled-adapter` only from the exact OpenClaw or Hermes package directory.

- [ ] **Step 1: Add failing decision, state, and security tests**

Add a `working-set GC decision and state` suite to `plugins/core/test.mjs` using
the existing `canonicalTempDir` fixture and `resetModuleState`. Pin these tests:

- `evaluateWorkingSetEviction matches the 2560-row Rust/Lean corpus`.
- `persists strict working_set JSON and rejects non-4096 caps`.
- `canonical project and digest identities isolate every scope dimension`.
- `load rejects corrupt, oversized, mismatched, and symlinked state without
overwriting it`.
- `newer epochs clear derived state and older epochs start clean`.
- `turn identity deduplicates events and resets at u32::MAX`.
- `immortal nodes survive every age and budget combination`.
- `runtime imports remain limited to the four approved Node built-ins`.

Assert exact §8 validation, 1 MiB read cap, newest-event-first 128-entry window,
unknown-parent-field preservation, nested-unknown-field invalidation, and no
cross-scope reuse.

- [ ] **Step 2: Run the core suite to verify the new tests fail**

```bash
npm --prefix plugins/core run build
node --test plugins/core/test.mjs
```

Expected: new tests fail on missing exports or missing strict GC behavior.

- [ ] **Step 3: Implement types, mirror kernel, scope, and persistence**

Add `node:crypto` as the only new runtime import. Implement the TypeScript mirror
with Task 1 precedence. Add canonical project IDs, control-free scope validation,
strict JSON validators, bounded state reads, epoch adoption, deterministic IDs,
and `loadWorkingSet`/`persistWorkingSet`. All writes go through
`writeJsonAtomically`; corrupt parents are never overwritten; failures remain
best-effort and fail-soft.

- [ ] **Step 4: Run the core suite to verify decision/state tests pass**

```bash
npm --prefix plugins/core run build
node --test plugins/core/test.mjs
```

Expected: the decision/state/security suite passes; existing core tests remain
green.

- [ ] **Step 5: Add failing cycle, sweep, renderer, and delta tests**

Add tests pinning: fitting page-fault promotion; failed/non-fitting promotion
retaining the stub; TTL age `3` retaining and `4` demoting; pressure demotion
reducing charged bytes while preserving current pinned roots; canonical tier
and tie-break ordering; all collection caps; newest same-kind duplicate wins;
conflicting kinds cannot overwrite immortal identity; `turn_seq` reset clearing
ages, turn/event IDs, and fingerprints; context cycles never advancing or
re-marking; unchanged fingerprints causing no mutation/context; changed
fingerprints replacing exactly one prior mutable frame; sink precedence across
`context[]`, `content`, and `additionalContext`; legacy-block replacement;
Golden Rule manifest fallback; and every rendered frame staying at most 4096
UTF-8 bytes.

- [ ] **Step 6: Run the core suite to verify cycle/render tests fail**

```bash
node --test plugins/core/test.mjs
```

Expected: the new cycle/sweep/render/delta assertions fail before implementation.

- [ ] **Step 7: Implement cycles, sweep, frame rendering, and core hook integration**

Implement `advanceWorkingSetTurn`, `sweepWorkingSet`,
`renderWorkingSetFrame`, `upsertWorkingSetFrame`, all three cycle functions,
and `markWorkingSetTouches` with spec §§9 and 11 semantics. Integrate
`executeAfterHandler`, `sessionContextHandler`, and `sessionCompactionHandler`
with the shared cycles. Replace epoch-only injection gating with the bounded
fingerprint map plus persisted `last_injected_fingerprint`. Feed conceptual
bridges into young generation while retaining the four-item bridge cap. Keep
guard behavior fail-closed and GC failures fail-soft.

- [ ] **Step 8: Run the core suite to verify cycle/render tests pass**

```bash
npm --prefix plugins/core run build
node --test plugins/core/test.mjs
```

Expected: cycle/sweep/render/delta tests and existing hook tests pass.

- [ ] **Step 9: Add failing dehydration, recall, and privacy tests**

Add tests pinning: 512-byte UTF-8-safe head/tail cuts; complete normalized text
as head with empty tail at 1024 bytes or less; CRLF normalization; aggregate
search `path=-`; first small reads staying inline while recording duplicate
history; repeated reads producing deterministic stubs; output over 3072 bytes
dehydrating; mutating output, errors, unknown shapes, and non-text parts staying
unchanged; exact digest fields and LF-delimited identity preimage; stable identity
across processes and changed identity across project/namespace/session/epoch;
digest/stub pair eviction; `read_count` saturation; pointer-token escaping and
512-byte tool-pointer cap; no raw output or restricted IDs entering state;
size/duplicate recall dehydration; text-only recall stubs having no expansion;
and write failure preserving the prior parent file.

- [ ] **Step 10: Run the core suite to verify dehydration tests fail**

```bash
node --test plugins/core/test.mjs
```

Expected: the new dehydration/recall/privacy assertions fail before implementation.

- [ ] **Step 11: Implement tool-output and recall dehydration**

Implement both dehydration helpers with spec §10 behavior. Every eligible first
inspection read records a digest and valid tool stub; replacement is controlled
by `dehydrated`. Commit digest history only with a valid stub. Normalize tool
names and paths, compute scoped IDs, enforce pair/cap ordering, and route
pending recall/file-context blocks into authoritative or text-only stubs.

- [ ] **Step 12: Run the core suite to verify dehydration tests pass**

```bash
npm --prefix plugins/core run build
node --test plugins/core/test.mjs
```

Expected: dehydration/recall/privacy tests and all earlier core tests pass.

- [ ] **Step 13: Add failing bundler contract tests**

Add tests proving default and compiled-adapter builds emit
`index.js`, `index.js.map`, `index.d.ts`, and `index.d.ts.map`; maps are valid
JSON with non-empty mappings, no `sourceRoot`, exact `sourcesContent`, and
adapter `sources` exactly `../../core/index.ts` then `../index.ts`; composition
removes only core default/source-map lines and exact
`@neurostrata/plugin-core` imports/re-exports; shim bytes and one shim default
export remain; `--compiled-adapter` succeeds only from `plugins/openclaw` or
`plugins/hermes`; wrong-cwd invocation fails before writes; `dist/.compiled`
is removed after success.

- [ ] **Step 14: Run the bundler tests to verify they fail**

```bash
node --test plugins/core/test.mjs
```

Expected: four-artifact, map, composition, and compiled-adapter assertions fail
against the current two-artifact bundler.

- [ ] **Step 15: Implement deterministic four-artifact bundling**

Extend `plugins/core/bundle-adapter.mjs` for default thin adapters and
`--compiled-adapter`. Read core/shim JS, declarations, and both maps; regenerate
both maps with exact sources/content and core-to-shim offsets; fail on aliased
core imports or multiple default exports; validate compiled-adapter cwd before
writing; remove `dist/.compiled` only after success.

- [ ] **Step 16: Build and run the complete focused core suite**

```bash
npm --prefix plugins/core run build
node --test plugins/core/test.mjs
```

Expected: every §16.2 core assertion passes, including the 2560-row mirror,
strict 4096 cap, privacy/fail-soft behavior, bundler contracts, and approved
runtime imports.

- [ ] **Step 17: Commit the core engine and generated core artifacts**

```bash
git add plugins/core/index.ts plugins/core/bundle-adapter.mjs plugins/core/test.mjs plugins/core/dist/index.js plugins/core/dist/index.js.map plugins/core/dist/index.d.ts plugins/core/dist/index.d.ts.map
git commit -m "feat: trap context growth in one tiny Ghostbusters frame"
```

Expected: one commit containing only core source, tests, bundler, and the four
core artifacts.

---

### Task 3: Six-Harness Adapters, Native Hook Installers, 28-Artifact Build, and Focused Verification

**Files:**

- Modify: `plugins/claude/cli-hook.mjs`
- Modify: `plugins/codex/cli-hook.mjs`
- Modify: `plugins/github-cli/cli-hook.mjs`
- Modify: `plugins/openclaw/index.ts`
- Modify: `plugins/openclaw/tsconfig.json`
- Modify: `plugins/openclaw/package.json`
- Modify: `plugins/hermes/index.ts`
- Modify: `plugins/hermes/tsconfig.json`
- Modify: `plugins/hermes/package.json`
- Modify: `plugins/opencode/test.mjs`
- Modify: `plugins/claude/test.mjs`
- Modify: `plugins/codex/test.mjs`
- Modify: `plugins/github-cli/test.mjs`
- Modify: `plugins/openclaw/test.mjs`
- Modify: `plugins/hermes/test.mjs`
- Modify: `src/task/harness_plugins.rs`
- Rebuild and commit: all 28 files matching
  `plugins/{core,opencode,claude,codex,github-cli,openclaw,hermes}/dist/index.{js,js.map,d.ts,d.ts.map}`

**Interfaces:**

- Consumes: Task 2's `runWorkingSetToolCycle`,
  `runWorkingSetContextCycle`, `runWorkingSetTurnBoundary`,
  `markWorkingSetTouches`, `upsertWorkingSetFrame`, scope/state types, and
  four-artifact bundler.
- Produces: native mappings exactly as follows:

| Harness    | Touch/cycle source            | Result behavior                                                          | Frame sink                                                                      |
| ---------- | ----------------------------- | ------------------------------------------------------------------------ | ------------------------------------------------------------------------------- |
| OpenCode   | `execute.after`; context hook | Replace every eligible string result field; preserve non-text parts      | Mutable `event.context[]`, then `event.additionalContext`                       |
| Claude     | `PostToolUse`; `Stop`         | `updatedToolOutput` only for eligible text                               | `hookSpecificOutput.additionalContext` array                                    |
| Codex      | `PostToolUse`; `Stop`         | Observe/stub only; never emit `updatedMCPToolOutput` or `suppressOutput` | Changed additional context on start/prompt/result; `{ continue: true }` on stop |
| GitHub CLI | `postToolUse`; `agentStop`    | Successful text replacement via `modifiedResult.textResultForLlm`        | Top-level `additionalContext`                                                   |
| OpenClaw   | `beforeFileOp`; `onTurnEnd`   | Mark path only; no result payload claim                                  | Mutable turn `content` when exposed                                             |
| Hermes     | `beforeToolCall`; `onTurnEnd` | Mark path only; no result payload claim                                  | Mutable turn `content` when exposed                                             |

- Adapters canonicalize event IDs as `<harness>:<eventName>:<nativeId>` and
  never invent IDs from output content. Native result hooks without `turnId`
  mark without advancing; OpenCode `execute.after` advances because it has no
  shared stop callback.
- Claude output uses `hookSpecificOutput.hookEventName`; `SessionStart` returns
  an additional-context array; `Stop` returns `continue: true` plus optional
  context; `SubagentStop` keeps its no-permission shape.
- Codex `SessionStart`, `UserPromptSubmit`, and `PostToolUse` may return
  event-shaped additional context; `Stop` returns only `{ continue: true }`.
- GitHub CLI commands carry `NEUROSTRATA_HOOK_EVENT`; runners normalize
  `sessionId`, `toolName`, `toolArgs`, and `toolResult.textResultForLlm`.
  `sessionStart` returns top-level `additionalContext`; `agentStop` returns `{}`;
  `preToolUse` maps `permissionDecision`, `permissionDecisionReason`, and
  `modifiedArgs` from the core guard result.
- Installer additions: extend `CLAUDE_HOOK_EVENT_ORDER` with `PostToolUse` after
  `PreToolUse`; add Claude `PostToolUse` using `CLAUDE_PRE_TOOL_USE_MATCHER`;
  add Codex `SessionStart`, `UserPromptSubmit`, and unmatchered `PostToolUse`;
  add `wire_github_cli_hooks` and local exclusion for
  `.github/hooks/neurostrata.json`. GitHub hook JSON has `version: 1` and each
  command entry has `type: "command"`, platform-specific `bash` and `powershell`
  commands, `cwd: "."`, `timeoutSec: 10`, and its event environment value.
- All non-pre-tool runner catches return valid event-shaped no-op success with
  exit code zero; unknown events never inherit `PreToolUse` allow/deny semantics.

- [ ] **Step 1: Add failing six-adapter tests**

Add these named tests to their owning suites:

- `plugins/opencode/test.mjs`: `working-set GC replaces eligible execute.after
text exactly once`; `working-set GC upserts mutable context without duplicate
additionalContext`.
- `plugins/claude/test.mjs`: `working-set GC PostToolUse maps updatedToolOutput
and additionalContext`; `working-set GC SessionStart and Stop emit native
shapes`; `working-set GC non-pretool failures return event-shaped no-op`.
- `plugins/codex/test.mjs`: `working-set GC PostToolUse records context without
unsupported fields`; `working-set GC SessionStart UserPromptSubmit and Stop
emit native shapes`; `working-set GC failures and unknown events return neutral
success`.
- `plugins/github-cli/test.mjs`: `working-set GC postToolUse normalizes camelCase
and replaces textResultForLlm`; `working-set GC sessionStart preToolUse and
agentStop preserve native shapes`.
- `plugins/openclaw/test.mjs` and `plugins/hermes/test.mjs`: `working-set GC keeps
host registration and marks file/tool touches`; `working-set GC onTurnEnd
upserts mutable content`; `working-set GC bundled API and default export remain
available`.

Assert exact native output keys, unchanged raw Codex output, no unsupported
Codex fields, one mutable frame, and fail-soft exit behavior.

- [ ] **Step 2: Add failing native-installer tests**

Add `mod working_set_gc` under `src/task/harness_plugins.rs::tests` with:
`claude_post_tool_use_is_added_once_and_preserves_foreign_hooks`,
`codex_context_and_result_events_are_wired_and_deduplicated`,
`github_cli_native_camel_case_hooks_are_written_atomically`, and
`working_set_gc_refresh_repairs_owned_paths_and_rejects_symlinks`. Assert exact
event sets/matchers, platform command variants, `cwd: "."`, `timeoutSec: 10`,
event environment values, foreign-entry preservation, refresh idempotence,
canonical-runner ownership, symlink refusal, atomic writes, and GitHub local
exclusion.

- [ ] **Step 3: Run focused adapter and installer tests to verify red**

```bash
npm --prefix plugins/core run build
node --test plugins/opencode/test.mjs plugins/claude/test.mjs plugins/codex/test.mjs plugins/github-cli/test.mjs plugins/openclaw/test.mjs plugins/hermes/test.mjs
cargo test --locked --bin neurostrata-mcp task::harness_plugins::tests::working_set_gc -- --nocapture
```

Expected: new native-mapping, shim, and installer assertions fail before
implementation.

- [ ] **Step 4: Convert OpenClaw and Hermes to core-backed shims**

Preserve host registration, guard, intent, `beforeFileOp`/`beforeToolCall`, and
`onTurnEnd` behavior while importing unaliased GC functions from
`@neurostrata/plugin-core`. Import and re-export the complete Task 2 public GC
surface so each final bundle exposes it alongside one shim default export. Map
each shim's `tsconfig.json` path to `../core/dist/index.d.ts`. Set both package
build scripts exactly to:

```text
tsc --project ../core/tsconfig.json && tsc --project tsconfig.json --outDir dist/.compiled && node ../core/bundle-adapter.mjs --compiled-adapter
```

Build both packages and run their two suites; expected: host behavior remains
and bundled GC/default-export tests pass.

- [ ] **Step 5: Implement Claude, Codex, and GitHub CLI native runners**

Normalize native inputs to Task 2 cycles and map outputs per the interface table.
Claude accepts only string or all-text `tool_response`; Codex records bounded
context without replacement/suppression fields; GitHub CLI emits
`modifiedResult` only for successful text replacement. Replace the shared
pre-tool-shaped catch with event-aware no-op catches while retaining fail-closed
`PreToolUse`. Build and run the Claude, Codex, and GitHub CLI suites; expected:
all new native mapping and fail-soft tests pass.

- [ ] **Step 6: Implement native installer wiring and run installer tests**

Add Claude `PostToolUse`, Codex context/result events and TOML duplicate checks,
and GitHub CLI native camelCase hook-file merge. Preserve foreign entries,
repair only canonical owned runners, refuse symlink components, write
atomically, avoid duplicates, report actual native-hook evidence, and add the
generated GitHub hook file to `.git/info/exclude`. Rebuild all six adapters so
Rust `include_str!` inputs are current, then run:

```bash
cargo test --locked --bin neurostrata-mcp task::harness_plugins::tests::working_set_gc -- --nocapture
```

Expected: every new installer test passes.

- [ ] **Step 7: Rebuild and validate all 28 tracked artifacts**

```bash
npm --prefix plugins/core run build
for adapter in opencode claude codex github-cli openclaw hermes; do
  npm --prefix "plugins/$adapter" run build
done
artifacts=(plugins/{core,opencode,claude,codex,github-cli,openclaw,hermes}/dist/index.{js,js.map,d.ts,d.ts.map})
git diff --check -- "${artifacts[@]}"
```

Review the generated diff. Assert all six adapter bundles expose the GC API and
exactly one default export; every map is valid JSON with exact sources/content
and non-empty mappings; core maps retain tsc's `index.ts` source; adapter maps
use exactly `../../core/index.ts` then `../index.ts`.

- [ ] **Step 8: Prove deterministic regeneration**

```bash
artifacts=(plugins/{core,opencode,claude,codex,github-cli,openclaw,hermes}/dist/index.{js,js.map,d.ts,d.ts.map})
sha256sum "${artifacts[@]}" > $TMPDIR/working-set-gc-before.sha256
npm --prefix plugins/core run build
for adapter in opencode claude codex github-cli openclaw hermes; do
  npm --prefix "plugins/$adapter" run build
done
sha256sum "${artifacts[@]}" > $TMPDIR/working-set-gc-after.sha256
diff -u $TMPDIR/working-set-gc-before.sha256 $TMPDIR/working-set-gc-after.sha256
```

Expected: no diff; all 28 artifacts are byte-identical across consecutive builds.

- [ ] **Step 9: Run all focused Node suites**

```bash
npm --prefix plugins/core test
node --test plugins/core/test.mjs plugins/opencode/test.mjs plugins/claude/test.mjs plugins/codex/test.mjs plugins/github-cli/test.mjs plugins/openclaw/test.mjs plugins/hermes/test.mjs
```

Expected: every core and adapter assertion in spec §16.2 passes.

- [ ] **Step 10: Run final focused Rust and Lean verification after Node builds**

```bash
cargo test --locked --bin neurostrata-mcp formal_kernel::tests::working_set -- --nocapture
cargo test --locked --bin neurostrata-mcp task::harness_plugins::tests::working_set_gc -- --nocapture
cargo test --locked --test lean_parity_test rust_formal_kernel_truth_table_matches_lean_cli -- --nocapture
cargo test --locked --test lean_parity_test lean_proof_sources_contain_no_sorry_or_axiom -- --nocapture
lake --dir proofs build
cargo check --locked --bin neurostrata-mcp
```

Expected: fresh output passes; no full-workspace test suite runs.

- [ ] **Step 11: Audit acceptance scope and commit the integration slice**

Confirm no UI, daemon endpoint, MCP/database/RDF schema, dependency version,
lockfile, or Cargo manifest change. Stage the Task 3 source/config/test files and
all 28 artifacts, then commit:

```bash
git add plugins/claude/cli-hook.mjs plugins/codex/cli-hook.mjs plugins/github-cli/cli-hook.mjs plugins/openclaw/index.ts plugins/openclaw/tsconfig.json plugins/openclaw/package.json plugins/hermes/index.ts plugins/hermes/tsconfig.json plugins/hermes/package.json plugins/opencode/test.mjs plugins/claude/test.mjs plugins/codex/test.mjs plugins/github-cli/test.mjs plugins/openclaw/test.mjs plugins/hermes/test.mjs src/task/harness_plugins.rs plugins/{core,opencode,claude,codex,github-cli,openclaw,hermes}/dist/index.{js,js.map,d.ts,d.ts.map}
git commit -m "feat: let six harnesses share one Lost in Translation frame"
```

Expected: one integration commit; three task commits total; no unrelated files.

---

## Acceptance Criteria

- [ ] All five Lean theorems compile with zero `sorry` and zero `axiom`.
- [ ] Rust and Lean emit identical ordered `3833`-row parity output.
- [ ] TypeScript mirror matches the Rust decision table on all `2560` boundary
      rows.
- [ ] Every rendered frame is at most `4096` UTF-8 bytes.
- [ ] Golden Rules and active-task header are never demoted or removed.
- [ ] Page-fault re-expansion restores full young-generation content through
      ReBAC-filtered daemon reads.
- [ ] OpenCode, Claude, and GitHub CLI replace large/repeated inspection output;
      Codex records bounded context without unsupported removal; OpenClaw and Hermes
      mark paths without claiming unavailable result payloads.
- [ ] All six harnesses run the shared core cycle; mutable hosts replace prior
      frames and append-only hosts emit each changed fingerprint once.
- [ ] `working_set` never crosses project, namespace, session, principal, or mesh
      boundaries.
- [ ] Corrupt state and unavailable expansion fail soft without blocking tools.
- [ ] All seven packages deterministically rebuild all 28 artifacts with fresh
      mappings and exactly one default export.
- [ ] No UI, daemon endpoint, MCP/database schema, dependency version, lockfile,
      or Cargo manifest changes exist.
- [ ] Every focused command above passes with fresh output before completion.
