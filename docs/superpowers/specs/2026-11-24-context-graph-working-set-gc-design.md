# Generational Context Graph — L1 Working-Set GC Design

Date: 2026-11-24  
Namespace: `NeuroStrata`  
Task: `neurostrata-zji8`  
Status: implementation-ready

Cross-references:

- `docs/superpowers/specs/2026-11-20-agent-graph-ontology-adoption-design.md`
- `docs/superpowers/specs/2026-11-18-kani-formal-verification-design.md`

## 1. Purpose

Standard harnesses treat the linear transcript as the heap. Tool output, recall
blocks, and repeated file reads accumulate as `O(T)` data until an expensive,
lossy model-driven compaction boundary.

NeuroStrata changes the memory hierarchy:

- LadybugDB plus the Episodic Buffer is the persistent heap.
- `.NeuroStrata/active_session.json#working_set` is the local L1 cache index.
- The harness prompt receives one bounded L1 frame per context cycle.
- A deterministic Generational Garbage Collector performs mark, sweep,
  pointer stubbing, and page-fault re-expansion without model calls.

The rendered frame has a fixed `4096` UTF-8-byte cap. Full memory and graph
evidence remains in the persistent heap. Eviction removes full content from L1,
never from LadybugDB. Per-turn collection work is independent of transcript
length: non-immortal collections are capped, and the immortal prefix reuses its
cached projection unless Golden Rules change. Hosts that cannot rewrite an
immutable transcript retain that transcript outside NeuroStrata's bounded frame;
§12.2 states each host's exact capability boundary.

## 2. Non-Goals

- No `web-ui/` or `src-tauri/` changes.
- No daemon endpoint, MCP tool, database migration, or RDF schema change.
- No LLM call, embedding call, or model-generated summary inside GC.
- No floating-point score, probability, clock-based TTL, or random tie-break.
- No new runtime npm dependency or Rust crate.
- No change to retrieval ranking, HNSW, Tantivy, RDF, or SynapticMesh semantics.
- No weakening of ReBAC, Golden Rule precedence, task gates, or fail-soft hooks.
- No Python implementation or tooling.
- No attempt to erase host-owned transcript entries that expose no mutable or
  result-replacement surface.

## 3. Global Constraints

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

## 4. Capability and License-Driven Evaluation

| Component                    | License basis     | Decision                            | Rationale                                                                 |
| ---------------------------- | ----------------- | ----------------------------------- | ------------------------------------------------------------------------- |
| `lbug 0.22.3`                | MIT               | Retain as persistent heap           | Already embedded; GC never moves graph authority into plugin state        |
| `tantivy 0.27`               | MIT               | Retain as lexical backing index     | Existing capability; not consulted by the per-turn decision loop          |
| `oxrdf 0.3`                  | MIT OR Apache-2.0 | Retain RDF projection support       | Existing semantic layer; not part of the hot GC path                      |
| `src/formal_kernel.rs`       | Project source    | Add pure decision kernel            | Zero dependency, zero I/O, `Copy` types, `const fn`, existing Aeneas path |
| Lean 4                       | Apache-2.0        | Add five proofs and CLI parity rows | Existing formal verification pipeline                                     |
| Node built-ins               | Node platform     | Use `fs`, `os`, `path`, `crypto`    | Local atomic state, canonical paths, SHA-256 fingerprints                 |
| External npm runtime package | Any               | Reject                              | Violates zero-dependency plugin contract                                  |
| Model-generated compaction   | Any               | Reject                              | Violates deterministic, zero-LLM GC requirement                           |

Existing versions are pinned in `Cargo.toml:104,115,143`. No new library API is
introduced by this design.

## 5. Verified Ground Truth

| ID  | Verified fact                                                                                    | Live anchor                                                                |
| --- | ------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------- |
| F1  | Prefetch LRU cap is `128`; per-recall character cap is `2048`                                    | `plugins/core/index.ts:2051-2054`                                          |
| F2  | Session context is injected once per compaction epoch; same-epoch calls do nothing               | `plugins/core/index.ts:3082-3158`                                          |
| F3  | `executeAfterHandler` appends file-context blocks and retains raw tool output                    | `plugins/core/index.ts:3364-3548`                                          |
| F4  | Conceptual bridges are merged and capped at four                                                 | `plugins/core/index.ts:1546-1576`                                          |
| F5  | Core currently imports only `fs`, `os`, and `path`; `crypto` is not yet imported                 | `plugins/core/index.ts:1-35`                                               |
| F6  | Atomic JSON replacement already exists                                                           | `plugins/core/index.ts:831-848`                                            |
| F7  | Core registers execute and OpenCode context/compaction hooks                                     | `plugins/core/index.ts:3810-3824`                                          |
| F8  | Formal kernel is standalone, dependency-free, `Copy` plus `const fn`                             | `src/formal_kernel.rs:1-6`                                                 |
| F9  | Lean CLI and Rust parity currently compare `1273` canonical rows                                 | `tests/lean_parity_test.rs:139-140,1423-1428`                              |
| F10 | Product vision owns formal-kernel documentation co-change                                        | `docs/vision/product-vision.md:38`                                         |
| F11 | `.NeuroStrata/` is gitignored runtime state                                                      | `.gitignore:19`                                                            |
| F12 | OpenCode, Claude, Codex, and GitHub CLI are thin core adapters                                   | each adapter `index.ts:1-13`                                               |
| F13 | OpenClaw and Hermes are standalone TypeScript shims and do not bundle core                       | `plugins/openclaw/index.ts:21-29`; `plugins/hermes/index.ts:9-17,127-128`  |
| F14 | OpenClaw and Hermes currently build with plain `tsc`                                             | `plugins/openclaw/package.json:8`; `plugins/hermes/package.json:8`         |
| F15 | Core has `PreToolUse`, `Stop`, and `SubagentStop` branches but no `PostToolUse` branch           | `plugins/core/index.ts:4105-4119`                                          |
| F16 | Claude installs `SessionStart`, two `PreToolUse` entries, and `Stop`; no `PostToolUse`           | `src/task/harness_plugins.rs:2463-2495`                                    |
| F17 | Codex installs `PreToolUse` and `Stop`; no result or prompt hook                                 | `src/task/harness_plugins.rs:868-931`                                      |
| F18 | Claude, Codex, and GitHub CLI runners share one pre-tool-shaped error catch                      | each `cli-hook.mjs:73-97`                                                  |
| F19 | GitHub CLI installation writes extension metadata, not native hooks                              | `src/task/harness_plugins.rs:2616-2634`                                    |
| F20 | The bundler writes only `index.js` and `index.d.ts`; maps are not refreshed                      | `plugins/core/bundle-adapter.mjs:85-118`                                   |
| F21 | All seven plugin/core `dist/` directories track JS, declarations, and both maps                  | `plugins/*/dist/index*`                                                    |
| F22 | OpenClaw exposes `beforeFileOp` and `onTurnEnd`; Hermes exposes `beforeToolCall` and `onTurnEnd` | `plugins/openclaw/index.ts:1506-1512`; `plugins/hermes/index.ts:1445-1476` |

Native hook contracts were checked against:

- Claude: `https://code.claude.com/docs/en/hooks.md`.
- Codex pinned revision `ae21235afbd5ba6556c2ab4b64f6afc96a139ef8`:
  `codex-rs/hooks/src/schema.rs`, `engine/output_parser.rs`, and
  `events/post_tool_use.rs`.
- GitHub CLI: `https://raw.githubusercontent.com/github/docs/main/content/copilot/reference/hooks-reference.md`.

Assumptions:

- A1: `active_session.json` remains the single shared session-state file.
- A2: Persistent GC requires a stable host session ID. Without one, core keeps
  isolated in-memory state and does not write `working_set`.
- A3: Existing daemon reads remain the ReBAC authority for memory expansion.

A1 and A3 are verified by current source. A2 is enforced fail-soft.

## 6. Architecture

### 6.1 Authority and Data Flow

```text
harness event
  -> canonicalize project, namespace, session, epoch, and event identity
  -> load and validate active_session.working_set
  -> mark touched roots and resolve page faults through ReBAC-filtered reads
  -> dehydrate eligible inspection output
  -> run deterministic mark-sweep
  -> render one frame <= 4096 UTF-8 bytes
  -> atomically merge working_set into active_session.json
  -> replace or emit the frame only when its fingerprint changes
```

LadybugDB remains authoritative. `working_set` is a reconstructible cache. A lost
or stale cache update may cause a cache miss, never a wrong memory acceptance.
The daemon continues to serve `neurostrata_get_memory`,
`neurostrata_memories_for_file`, and `neurostrata_related` for re-expansion.

### 6.2 Scope and Bounded State

`project_id` is the lowercase SHA-256 of the UTF-8 canonical absolute
`projectRoot`: `realpathSync.native`, separators normalized to `/`, no trailing
separator except filesystem root. It discloses no path bytes.

Persistent scope is the tuple:

```text
(project_id, namespace, session_id, epoch)
```

A missing session ID disables persistent GC for that event. A principal change
clears in-memory state before reuse.

| Constant                               |                   Value |
| -------------------------------------- | ----------------------: |
| `WORKING_SET_MAX_FRAME_BYTES`          |                  `4096` |
| `WORKING_SET_MAX_IDLE_TURNS`           |                     `3` |
| `WORKING_SET_STUB_MAX_BYTES`           |                   `192` |
| `WORKING_SET_MAX_ROOTS`                | `32` non-immortal roots |
| `WORKING_SET_MAX_YOUNG_GEN`            |                    `64` |
| `WORKING_SET_MAX_STUBS`                |                   `128` |
| `WORKING_SET_MAX_TOOL_STUBS`           |                   `128` |
| `WORKING_SET_MAX_TOOL_DIGESTS`         |                   `128` |
| `WORKING_SET_MAX_EVENT_IDS`            |                   `128` |
| `WORKING_SET_MAX_INJECTION_KEYS`       |                   `128` |
| `WORKING_SET_MAX_ACTIVE_SESSION_BYTES` |               `1048576` |
| `TOOL_GC_INLINE_THRESHOLD_BYTES`       |                  `3072` |
| `TOOL_GC_HEAD_BYTES`                   |                   `512` |
| `TOOL_GC_TAIL_BYTES`                   |                   `512` |
| `TOOL_GC_STUB_MAX_BYTES`               |                   `512` |
| `WORKING_SET_SUMMARY_MAX_BYTES`        |                   `512` |

`roots` contains immortal nodes plus at most 32 non-immortal pinned roots.
Immortal nodes are never removed. Non-immortal caps remove the oldest entry by
`last_touched_turn`, then largest `byte_cost`, then ascending `id`. Removed
stubs, tool stubs, and digests are reconstructible or safely forgettable.
`recent_event_ids` is insertion-ordered, newest first, and truncated to 128.
`tool_output_stubs` uses the same canonical order as `tool_output_digests` in
§8.4. A digest and its same-ID tool stub are one bounded pair: cap eviction
removes both, and a tool stub never remains without its digest.

The immortal set is bounded by the active session's Golden Rule list, not by
transcript length. Frame overflow uses the manifest projection in §11.1.

### 6.3 Core Interfaces

Add these exported TypeScript interfaces and functions to
`plugins/core/index.ts`:

```text
WorkingSetScope = {
  projectRoot: string,
  projectId: string,
  namespace: string,
  sessionId: string,
  epoch: number
}

WorkingSetTouch = {
  id: string,
  kind: Exclude<WorkingSetNodeKind, "recall_text">,
  path?: string,
  symbol?: string,
  memoryId?: string
}

WorkingSetExpansionRequest = {
  id: string,
  expandTool: string,
  input: Record<string, unknown>
}

WorkingSetNodeKind =
  | "task_header"
  | "golden_rule"
  | "file"
  | "symbol"
  | "memory"
  | "ast_summary"
  | "conceptual_bridge"
  | "recall_text"

WorkingSetExpansion = (
  request: WorkingSetExpansionRequest
) => Promise<WorkingSetNode | WorkingSetStub | null>

runWorkingSetToolCycle(input: {
  scope: WorkingSetScope,
  turnId?: string,
  eventId?: string,
  toolName: string,
  toolInput: unknown,
  rawText: string | null,
  outputIsText: boolean,
  isError: boolean,
  advanceTurn: boolean
}) -> Promise<{
  replacementText: string | null,
  frame: string,
  fingerprint: string,
  dehydrated: boolean
}>

runWorkingSetContextCycle(
  scope: WorkingSetScope,
  eventId?: string
) -> Promise<{ frame: string, fingerprint: string, changed: boolean }>

runWorkingSetTurnBoundary(
  scope: WorkingSetScope,
  eventId?: string,
  turnId?: string
) -> Promise<{ frame: string, fingerprint: string, changed: boolean }>

markWorkingSetTouches(
  scope: WorkingSetScope,
  touched: WorkingSetTouch[],
  eventId?: string
) -> Promise<void>

upsertWorkingSetFrame(sink: unknown, frame: string) -> boolean
```

`WorkingSetNode`, `WorkingSetStub`, `WorkingSetState`, `ToolOutputStub`, and
`ToolOutputDigest` mirror the exact JSON shapes in §8. A null expansion result
means denied, unavailable, or unchanged; the stub remains resident.

`runWorkingSetToolCycle` returns replacement text only for eligible text output.
The adapter decides whether its host can apply that replacement. GC decisions,
digest creation, sweeping, rendering, and persistence remain in core.

## 7. Formal Kernel Decision Domain

### 7.1 Rust Interface

Add to `src/formal_kernel.rs`:

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

All types derive `Debug`, `Clone`, `Copy`, `PartialEq`, and `Eq`. Kani-only
`Arbitrary` derives remain compile-time gated. The function uses no allocation,
I/O, collection, float, clock, or external crate.

`turn_age` is `current_turn.saturating_sub(last_touched_turn)`. `node_bytes` is
the full canonical frame-line cost. The pure kernel accepts all `u32` inputs;
the state layer admits a demotion only when a deterministic stub of at most 192
bytes exists and `stub.byte_cost < node.byte_cost`.

### 7.2 Decision Precedence

The first matching row wins.

| Tier          | Condition                                                                      | Action              |
| ------------- | ------------------------------------------------------------------------------ | ------------------- |
| `Immortal`    | Any input                                                                      | `RetainFull`        |
| `EvictedStub` | `is_re_referenced && projected_frame_bytes <= max_frame_bytes`                 | `PromoteToYoungGen` |
| `EvictedStub` | Any other input                                                                | `RetainStub`        |
| `YoungGen`    | `!is_re_referenced && turn_age > max_idle_turns`                               | `DemoteToStub`      |
| `YoungGen`    | `projected_frame_bytes > max_frame_bytes`                                      | `DemoteToStub`      |
| `PinnedRoot`  | `!is_re_referenced && turn_age > 0 && projected_frame_bytes > max_frame_bytes` | `DemoteToStub`      |
| `PinnedRoot`  | Any other input                                                                | `RetainFull`        |
| `YoungGen`    | Any other input                                                                | `RetainFull`        |

A current-turn pinned root has `is_re_referenced = true`; budget pressure never
demotes it. An unreferenced pinned root becomes pressure-eligible after one idle
cycle. `EvictedStub` is never pressure-demoted because it is already a stub.

### 7.3 Lean Theorems

Create `proofs/NeuroStrata/WorkingSetGc.lean` with exactly these named theorems:

1. `immortal_never_evicted`
2. `page_fault_promotes_stub`
3. `idle_young_gen_demotes_to_stub`
4. `budget_overflow_demotes_non_immortal`
5. `eviction_deterministic`

Their contracts are:

- `immortal_never_evicted`: every `Immortal` input yields `RetainFull`.
- `page_fault_promotes_stub`: a re-referenced stub yields
  `PromoteToYoungGen` exactly when its projected full node fits.
- `idle_young_gen_demotes_to_stub`: an unreferenced young node with
  `turn_age > max_idle_turns` yields `DemoteToStub`.
- `budget_overflow_demotes_non_immortal`: for a young node, or an unreferenced
  pinned node with `turn_age > 0`, overflow yields `DemoteToStub` when
  `node_bytes > 192` and a stub of at most 192 bytes exists. Charged bytes fall
  by at least `node_bytes - 192`. The theorem excludes immortal nodes,
  current-turn pinned roots, existing stubs, and nodes without a valid stub.
- `eviction_deterministic`: equal inputs produce equal actions, and sorting by
  the canonical key in §9.3 is idempotent with a unique `id` tie-break.

The file contains zero `sorry` and zero `axiom`.

`proofs/Main.lean` imports `NeuroStrata.WorkingSetGc` and emits canonical rows:

```text
WORKING_SET_GC|tier|turn_age|max_idle_turns|node_bytes|projected_frame_bytes|max_frame_bytes|is_re_referenced|action_tag
```

Action tags are `retain_full`, `promote_to_young_gen`, `demote_to_stub`, and
`retain_stub`.

### 7.4 Exhaustive Parity Corpus

`tests/lean_parity_test.rs` adds `2560` rows in this exact nested order:

| Dimension               | Values                                              |
| ----------------------- | --------------------------------------------------- |
| tier                    | `Immortal`, `PinnedRoot`, `YoungGen`, `EvictedStub` |
| `turn_age`              | `0`, `1`, `3`, `4`, `u32::MAX`                      |
| `max_idle_turns`        | `0`, `3`                                            |
| `node_bytes`            | `0`, `1`, `256`, `u32::MAX`                         |
| `projected_frame_bytes` | `0`, `4096`, `4097`, `u32::MAX`                     |
| `max_frame_bytes`       | `0`, `4096`                                         |
| `is_re_referenced`      | `false`, `true`                                     |

The current `1273` rows plus `2560` GC rows produce `3833` canonical rows.
The Rust test updates its expected count and compares the complete ordered
output with the Lean executable.

Add focused kernel unit tests for immortal overflow, page-fault fit and miss,
TTL equality versus expiry, pinned-root pressure, zero budget, and maximum
integer inputs. Add Kani harnesses for panic-freedom and determinism under
`#[cfg(kani)]`.

## 8. Active Session JSON Contract

`working_set` is additive. Existing `task_id`, `status`, `golden_rules`,
`conceptual_bridges`, `subagent_prompt_injection`, and
`claude_session_dirs` fields remain unchanged.

```json
{
  "version": 1,
  "project_id": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "namespace": "NeuroStrata",
  "session_id": "session-1",
  "epoch": 0,
  "turn_seq": 1,
  "active_turn_id": null,
  "recent_event_ids": [],
  "max_frame_bytes": 4096,
  "max_idle_turns": 3,
  "used_frame_bytes": 1234,
  "frame_fingerprint": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "last_injected_fingerprint": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  "roots": [],
  "young_gen": [],
  "evicted_stubs": [],
  "tool_output_stubs": [],
  "tool_output_digests": []
}
```

Validation rules:

- `version` must equal `1`.
- `project_id`, `namespace`, `session_id`, and `epoch` must match active scope.
- `project_id` and fingerprints are 64 lowercase hexadecimal characters.
- `namespace` is 1..128 UTF-8 bytes; `session_id` is 1..256 UTF-8 bytes.
  Both are control-free, so LF-delimited digest preimages are unambiguous.
- Numeric fields are non-negative safe integers bounded by `u32::MAX`.
- `max_frame_bytes` must equal `4096`; no other value is accepted.
- `max_idle_turns` is `0..30`; default `3`.
- `used_frame_bytes <= 4096`.
- `active_turn_id` is null or a control-free string of at most 128 UTF-8 bytes.
- `recent_event_ids` has at most 128 unique control-free strings, each at most
  256 UTF-8 bytes, ordered newest first.
- Node arrays use canonical GC order. Tool digest order is defined in §8.4.
- Unknown fields in the parent session object are preserved. Unknown fields at
  any depth inside `working_set` invalidate only `working_set`.

### 8.1 WorkingSetNode

Required fields:

| Field               | Type     | Meaning                                                                                                                  |
| ------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------ |
| `id`                | string   | Stable scoped identity                                                                                                   |
| `kind`              | string   | `task_header`, `golden_rule`, `file`, `symbol`, `memory`, `ast_summary`, `conceptual_bridge`, or stub-only `recall_text` |
| `label`             | string   | Single-line display label, at most 128 UTF-8 bytes                                                                       |
| `summary`           | string   | Single-line display summary, at most 512 UTF-8 bytes                                                                     |
| `governing_ids`     | string[] | Sorted unique visible IDs, at most eight                                                                                 |
| `byte_cost`         | integer  | Canonical frame-line UTF-8 byte cost including LF                                                                        |
| `last_touched_turn` | integer  | Last marked turn                                                                                                         |
| `expand_tool`       | string   | `neurostrata_get_memory`, `neurostrata_memories_for_file`, `neurostrata_related`, or `neurostrata_get_snapshot`          |

Optional fields are `path`, `symbol`, and `memory_type`. Paths are canonical
project-relative POSIX paths of at most 512 UTF-8 bytes. Absolute paths,
traversal, control characters, and paths outside `projectRoot` are rejected.
Each governing ID is at most 256 UTF-8 bytes.

IDs are deterministic:

- task header: `task:<task_id>`
- golden rule: `rule:<sha256-of-normalized-rule-text>`
- memory: `mem:<memory_id>`
- file: `file:<relPath>`
- symbol: `symbol:<relPath>#<symbol>`
- bridge: `bridge:<target_id>`
- text-only recall stub: `text:<sha256-of-normalized-recall-text>`

Rule text is normalized by CRLF-to-LF conversion only. Task headers and Golden
Rules are immortal. Files, symbols, and explicit query anchors are pinned roots.
Recalled memories, AST summaries, and bridges enter young generation.
`recall_text` never enters a full-node collection.

### 8.2 WorkingSetStub

Required fields are `id`, `kind`, `pointer`, `byte_cost`, `last_touched_turn`,
and `governing_ids`. `byte_cost` is pointer UTF-8 bytes including LF;
`governing_ids` is sorted, unique, visible, and capped at eight. Optional fields
are `path`, `memory_type`, and `sha256`. `sha256` is required for file and symbol
stubs and is the first 12 lowercase hex characters of SHA-256 over the UTF-8 stub
`id`; it is an identity digest, not a file-content freshness claim.

Memory pointer form:

```text
[GCStub mem:<id> | <memory_type> | governs:<path> | expand:neurostrata_get_memory]
```

File pointer form:

```text
[GCStub file:<relPath> | sha256:<12hex> | expand:neurostrata_memories_for_file]
```

Symbol pointer form:

```text
[GCStub symbol:<relPath>#<symbol> | sha256:<12hex> | expand:neurostrata_memories_for_file]
```

A missing path is `-`. Multiple governing paths are comma-joined. Pointer tokens
percent-encode `%`, space, comma, `|`, `[`, `]`, CR, and LF using uppercase UTF-8
hex. Every pointer is one line and at most 192 UTF-8 bytes. A full node without a
valid smaller stub is retained rather than demoted.

Text-only recall pointer form:

```text
[GCStub text:<64hex> | bytes=<N> | expand:none]
```

It carries no memory body or expansion authority. Structured recall replaces it
with a real memory ID and pointer when available.

### 8.3 ToolOutputStub

`tool_output_stubs` is separate from graph-node `evicted_stubs`.

| Field               | Type                                            |
| ------------------- | ----------------------------------------------- |
| `id`                | string; equal to the digest ID                  |
| `tool_name`         | string; normalized tool name                    |
| `rel_path`          | string or `-`                                   |
| `sha256`            | 12 lowercase hexadecimal characters             |
| `bytes`             | integer; normalized raw UTF-8 output byte count |
| `epoch`             | integer                                         |
| `last_touched_turn` | integer                                         |
| `governing_ids`     | sorted unique string[]; at most eight IDs       |
| `pointer`           | string; at most 512 UTF-8 bytes                 |
| `byte_cost`         | integer; pointer bytes including LF             |

Pointer form:

```text
[NeuroStrata GC Stub -> path=<relPath> | sha256=<12hex> | bytes=<N> | governing_ids=[id1,id2]]
```

Pointer tokens use §8.2 escaping. If encoding all governing IDs exceeds 512
bytes, the pointer uses `governing_ids=[]`; the structured stub retains the
sorted IDs. If the pointer still exceeds 512 bytes, dehydration is rejected and
the original output is preserved. Raw tool output is never persisted in
`working_set`.

### 8.4 ToolOutputDigest

Each `tool_output_digests` element has exactly these fields:

| Field               | Type                                             |
| ------------------- | ------------------------------------------------ |
| `id`                | string; `tool:` plus 64 lowercase hex characters |
| `project_id`        | 64 lowercase hexadecimal characters              |
| `path_key`          | canonical project-relative POSIX path or `-`     |
| `tool_name`         | normalized lowercase ASCII tool name             |
| `sha256`            | 12 lowercase hexadecimal characters              |
| `bytes`             | integer; normalized raw UTF-8 output byte count  |
| `epoch`             | integer                                          |
| `last_touched_turn` | integer                                          |
| `read_count`        | integer; `1..u32::MAX`                           |

`tool_name` is trimmed and lowercased; whitespace, `/`, and control characters
are rejected. `path_key` uses the same validation as `WorkingSetNode.path`.

The 12-hex digest is SHA-256 over normalized raw UTF-8 output. The stable ID is
SHA-256 over this exact LF-delimited UTF-8 preimage:

```text
project_id\nnamespace\nsession_id\nepoch\ntool_name\npath_key\nsha256
```

`id` is `tool:` followed by the full 64-hex result. Digests are sorted by
`path_key`, `tool_name`, `sha256`, then `id`. A changed file creates a new digest;
duplicate detection checks for any retained digest with the same `path_key` in
the active scope and epoch. Cap eviction forgets old duplicate history but never
changes heap authority.

## 9. Per-Turn L1 Cycle

### 9.1 Load, Validate, and Persist

`loadWorkingSet(scope)` reads `.NeuroStrata/active_session.json` only when
`.NeuroStrata` already exists, is canonical, and remains inside `projectRoot`.
It stats before reading and reads at most `1048577` bytes to handle growth after
the stat; files larger than `1048576` bytes are treated as corrupt. A symlinked
state file or symlinked state-directory component is rejected.

Epoch handling:

- Same project, namespace, session, and epoch: load the validated working set.
- Same project, namespace, and session with a newer persisted epoch: adopt that
  epoch as active, clear tool stubs, digests, `recent_event_ids`, and both
  injection fingerprints, then validate against the adopted scope.
- Older epoch or any project, namespace, or session mismatch: discard persisted
  nodes and start clean at the active epoch.

`persistWorkingSet(scope, state)` re-reads the parent session object, preserves
unknown parent fields, replaces only `working_set`, and calls
`writeJsonAtomically`. A corrupt parent is never overwritten. Persistence is
best-effort and never authorizes a memory result.

### 9.2 `advanceWorkingSetTurn`

Signature:

```text
advanceWorkingSetTurn(
  state,
  namespace,
  sessionId,
  projectRoot,
  touched,
  expand: WorkingSetExpansion
) -> Promise<WorkingSetState>
```

Behavior:

1. Validate namespace, session, epoch, project ID, and project-root scope.
2. Increment `turn_seq` by one. At `u32::MAX`, reset `turn_seq` and all
   `last_touched_turn` values to zero, clear `active_turn_id`, event IDs, and
   injection fingerprints.
3. Build a deterministic touched-ID set.
4. Mark newly touched files, symbols, memories, AST summaries, and bridges.
5. Update `last_touched_turn` for existing full nodes.
6. For each touched `EvictedStub`, evaluate page-fault promotion. Expand through
   the ReBAC-filtered daemon only when the full node fits the current frame.
7. On expansion failure, keep the stub and retry on the next touch.
8. Enforce collection caps without removing immortal nodes.

Explicit query anchors are recognized deterministically from `mem:<id>`,
`path:<relPath>`, `symbol:<relPath>#<symbol>`, and existing repository-path
extraction. No model parsing is used.

Adapters canonicalize `eventId` as `<harness>:<eventName>:<nativeId>`. If the
native event has no ID, deduplication relies only on `turnId`; the adapter does
not invent an ID from output content.

Turn identity rules:

- A new non-empty `turnId` advances once and becomes `active_turn_id`.
- A repeated `active_turn_id` does not advance.
- A repeated `eventId` in `recent_event_ids` is a no-op.
- Native result hooks without `turnId` mark the current turn but do not advance.
- `Stop`, `agentStop`, and `onTurnEnd` advance once per new event or turn ID.
- OpenCode `execute.after` uses `advanceTurn: true` because it has no shared
  native stop callback.
- Context cycles never advance or re-mark a touch.

### 9.3 `sweepWorkingSet`

Signature:

```text
sweepWorkingSet(state) -> WorkingSetState
```

Behavior:

1. Derive each graph node's tier from its collection and kind.
2. Evaluate the TypeScript mirror `evaluateWorkingSetEviction` for every graph
   node. Its branches and action tags match Rust exactly.
3. Apply TTL demotions for expired young nodes.
4. Build the pressure set from surviving full young nodes and unreferenced
   pinned roots with `turn_age > 0`. Exclude immortal nodes, existing stubs,
   current-turn pinned roots, and nodes without a valid smaller stub.
5. Sort pressure candidates by young-before-pinned tier rank, descending
   `turn_age`, descending `byte_cost`, then ascending `id`.
6. Recompute projected bytes after each accepted demotion. Stop when the frame
   fits or no eligible candidate remains.
7. Replace each accepted full node with its deterministic stub. Assert
   `stub.byte_cost < node.byte_cost` before committing the replacement.
8. Store graph nodes in canonical tier order: `Immortal`, `PinnedRoot`,
   `YoungGen`, `EvictedStub`; within a tier use descending `turn_age`, descending
   `byte_cost`, then ascending `id`.
9. Recompute all caps.

Duplicate IDs with the same kind use the newest touched entry. A conflicting
kind is rejected and cannot overwrite an immortal node.

### 9.4 Frame Accounting

For a new full node, `projected_frame_bytes` is current charged bytes plus node
cost. For an existing full node, it is current charged bytes. A stub promotion
uses current charged bytes plus expanded full-node cost.

After sweep, the renderer may shorten display summaries but never changes node
tier. A shortened node's `summary` and `byte_cost` are persisted. Omitted nodes
remain in state with their canonical potential line cost.

`used_frame_bytes` is always the UTF-8 byte length of the final rendered frame,
including headers, markers, and newlines.

### 9.5 Atomicity and Concurrency

GC never treats a successful in-memory transition as proof of persistence.
`writeJsonAtomically` keeps the prior complete file on failure and removes its
temporary file. Concurrent writers use last-complete-write-wins semantics; the
next reader validates scope and reconstructs on mismatch.

## 10. Tool Output and Recall Dehydration

### 10.1 `dehydrateToolOutput`

Signature:

```text
dehydrateToolOutput(rawText, toolName, filePath, governingIds)
  -> { text, dehydrated, digest? }
```

Dehydrate only read-only inspection output from the existing `READ_FILE_TOOLS`
classification. Mutating results, guard verdicts, errors, unknown shapes, and
output containing non-text content parts remain intact.

Dehydration triggers when either condition holds:

- normalized raw UTF-8 output is greater than `3072` bytes; or
- the same normalized path was already read in the same project, namespace,
  session, and epoch.

A dehydrated result contains:

```text
[NeuroStrata GC Summary — head]
<first 512 UTF-8 bytes>
[NeuroStrata GC Summary — tail]
<last 512 UTF-8 bytes>
[NeuroStrata GC Stub -> path=<relPath> | sha256=<12hex> | bytes=<N> | governing_ids=[...]]
```

Head and tail are cut only at UTF-8 and code-point boundaries. CRLF is normalized
to LF. Output of at most 1024 bytes uses the complete normalized text as head
and an empty tail. Aggregate search output uses `path=-`.

### 10.2 `dehydrateRepeatedReadOutput`

This helper owns same-epoch duplicate detection, digest persistence, and tool
stub creation. It uses `path_key` and `tool_output_digests`. Every eligible
inspection read creates or updates a digest and tool stub, including reads whose
output stays inline; `dehydrated` controls replacement only. A changed file
receives a new digest but remains a repeated read and is dehydrated.

A digest is committed only with a valid tool stub. `read_count` increments on an
identical retained digest and saturates at `u32::MAX`. If no valid stub fits, the
output stays unchanged and no duplicate-history digest is committed.

### 10.3 Recall Blocks

A pending recall or file-context block is dehydrated when it exceeds `3072`
UTF-8 bytes or all of its memory IDs are already resident in the same epoch.
Structured recall metadata supplies IDs; text-only fallback uses a normalized
content SHA-256 for identity and duplicate detection only. A text-only fallback
has no authoritative expansion target and remains a stub until structured recall
supplies a real memory ID.

Resulting authoritative memory pointers and text-only recall pointers enter
`evicted_stubs`. Restricted content is never copied into a pointer.

`persistConceptualBridges` keeps its four-item cap. Each bridge also enters young
generation and expires under the normal TTL rule.

## 11. Frame Rendering and Delta Injection

### 11.1 `renderWorkingSetFrame`

Signature:

```text
renderWorkingSetFrame(state, activeSession)
  -> { text, usedBytes, fingerprint }
```

Canonical layout:

```text
[NeuroStrata Active Context] Namespace: <ns> | Active Task: <task> [<status>] | Epoch: <e> | Turn: <t>
[NeuroStrata Golden Rules — ALWAYS ENFORCED]
  - <rule or bounded rule projection>
[NeuroStrata Working Set]
[Root] <id> | <summary> | expand:<tool>
[Young] <id> | <summary> | expand:<tool>
[Stub] <pointer>
[ToolStub] <pointer>
[NeuroStrata End Active Context]
```

Every emitted line, including the final line, ends in exactly one LF. Lines are
joined without an additional trailing LF. Header values are single-line,
control-free, and bounded: namespace 128 bytes, task ID 128 bytes, status 32
bytes. Oversized task or namespace values use their stable SHA-256 identity.
All dynamic frame text percent-encodes `%`, `[`, and `]` before rendering, so
rules, summaries, and IDs cannot forge NeuroStrata block markers; expansion
still returns the original text.

Rules:

1. Render the active-task header first and reserve the end-marker bytes.
2. Render every Golden Rule before non-immortal content.
3. Render non-immortal entries in canonical GC order.
4. Add a line only when it fits completely. Continue scanning so a smaller
   subsequent line may use unused space.
5. Shorten a pinned or young summary before omitting its line.
6. If immortal content alone exhausts the frame, shorten each rule projection
   while retaining its stable ID. If minimum projections still do not fit, emit
   one deterministic manifest line:

```text
[NeuroStrata Golden Rule Manifest] count=<N> | ids_sha256=<64hex> | expand:neurostrata_get_snapshot
```

7. Never emit a frame larger than `4096` UTF-8 bytes.
8. Fingerprint is SHA-256 over the exact final frame UTF-8 bytes.

Immortal means retained in working state. Frame projection may be compacted;
the node is never demoted or removed.

### 11.2 `sessionContextHandler`

The handler no longer gates on `baselineInjectedEpoch` alone. It keeps a bounded
in-memory last-injected fingerprint map keyed by project ID, namespace, session,
and epoch. `last_injected_fingerprint` persists the same fact across processes.

`upsertWorkingSetFrame` uses this sink precedence:

1. Mutable `event.context[]`: replace the first complete or legacy plugin frame,
   remove duplicate plugin frames, and prepend the frame when none exists. Do not
   also append it to `additionalContext`.
2. Mutable string `event.content`: replace the prior block or prepend the frame.
3. Mutable string `event.additionalContext`: replace the prior block or prepend
   the frame.
4. Hook-return surface: return the frame as host-native additional context.

Complete blocks use the start and end markers above. Legacy blocks without an
end marker are replaced from their start marker to the next known NeuroStrata
marker or string end.

If the fingerprint is unchanged, the event is not mutated and no hook context is
returned. Append-only hosts receive a changed frame once; they cannot remove an
older host-owned frame. Epoch advance clears injection fingerprints so the next
context cycle emits the new baseline.

## 12. Six-Harness Integration

All GC decisions and rendering live in core. Harness code only canonicalizes
native events, invokes the shared cycle, and maps the result to a native sink.

### 12.1 Capability Matrix

| Harness      | Touch/cycle source                     | Result dehydration                        | Frame sink                                                   |
| ------------ | -------------------------------------- | ----------------------------------------- | ------------------------------------------------------------ |
| `opencode`   | `execute.after`; context hook renders  | Replaces text result fields               | Mutable `event.context[]` or `event.additionalContext`       |
| `claude`     | `PostToolUse`; `Stop` boundary         | `updatedToolOutput`                       | Hook `additionalContext`                                     |
| `codex`      | `PostToolUse`; `Stop` boundary         | Observe and stub only; raw result remains | Hook `additionalContext` on start, prompt, and result events |
| `github-cli` | `postToolUse`; `agentStop` boundary    | `modifiedResult.textResultForLlm`         | Hook `additionalContext`                                     |
| `openclaw`   | `beforeFileOp`; `onTurnEnd` boundary   | No result payload; marks path only        | Mutable turn `content` when exposed                          |
| `hermes`     | `beforeToolCall`; `onTurnEnd` boundary | No result payload; marks path only        | Mutable turn `content` when exposed                          |

OpenClaw's typed `FileOperation` covers write/edit/delete/mkdir, not read-result
dehydration. Hermes exposes no post-tool result in the registered hook. Neither
shim claims raw-output collection.

### 12.2 Native Event Mappings

#### OpenCode

`executeAfterHandler` extracts text from `result.content` string or all-text
content array, then from `result.output`, `output`, `result`, `response`, or
`content`. Any non-text part disables dehydration. Eligible text is replaced in
every existing string result field; non-text parts remain unchanged. The context
hook calls `runWorkingSetContextCycle` and `upsertWorkingSetFrame`.

#### Claude

`PostToolUse` input uses `session_id`, `tool_name`, `tool_input`, `tool_response`,
and `tool_use_id`. Raw text is accepted only from a string `tool_response` or an
all-text `content` array. Output is:

```text
{
  hookSpecificOutput: {
    hookEventName: "PostToolUse",
    updatedToolOutput?: <replacement text>,
    additionalContext?: [<changed frame>]
  }
}
```

`SessionStart` returns `hookSpecificOutput.additionalContext: [frame]`.
`Stop` advances the boundary and returns `continue: true` plus optional
`hookSpecificOutput.additionalContext: [frame]`. `SubagentStop` retains its
current no-permission shape.

#### Codex

`PostToolUse` input uses `session_id`, `turn_id`, `tool_name`, `tool_input`,
`tool_response`, and `tool_use_id`. `SessionStart`, `UserPromptSubmit`, and
`PostToolUse` may return:

```text
{
  hookSpecificOutput: {
    hookEventName: <event>,
    additionalContext: <changed frame>
  }
}
```

`Stop` advances the boundary and returns `{ continue: true }`.

At pinned Codex revision `ae21235afbd5ba6556c2ab4b64f6afc96a139ef8`,
`updatedMCPToolOutput` is schema-visible but unsupported by the parser and
ignored by the result-event runner. `suppressOutput` is schema-visible but never
applied to model-facing tool output; a control-capable handler reports it as
unsupported. The Codex adapter never emits either field. It records the digest
and tool stub and emits bounded additional context, but does not claim to remove
the original `tool_response` from the host transcript.

#### GitHub CLI

The installer uses native camelCase events and sets
`NEUROSTRATA_HOOK_EVENT` on each command. The runner normalizes `sessionId`,
`toolName`, `toolArgs`, and `toolResult.textResultForLlm` to core's canonical
input. Output mappings are:

- `sessionStart`: `{ additionalContext: frame }`.
- `postToolUse`:
  `{ modifiedResult?: { resultType: "success", textResultForLlm: replacement },
additionalContext?: frame }`.
- `preToolUse`: top-level `permissionDecision`, `permissionDecisionReason`, and
  `modifiedArgs` mapped from core's guard result.
- `agentStop`: `{}` after the shared boundary cycle.

#### OpenClaw and Hermes

Their host-specific registration remains local. GC functions and state come
from `@neurostrata/plugin-core`. `beforeFileOp` or `beforeToolCall` calls
`markWorkingSetTouches`; `onTurnEnd` calls `runWorkingSetTurnBoundary` and
`upsertWorkingSetFrame` on mutable turn content. Existing guard and intent
behavior remains host-owned.

### 12.3 Installer Wiring

`src/task/harness_plugins.rs` gains native result-event wiring:

- Claude: add `PostToolUse` with `CLAUDE_PRE_TOOL_USE_MATCHER`; include it in
  `CLAUDE_HOOK_EVENT_ORDER` after `PreToolUse`. Preserve user hooks.
- Codex: add `SessionStart`, `UserPromptSubmit`, and unmatchered `PostToolUse`;
  retain existing `PreToolUse` and `Stop`. Extend TOML duplicate detection to
  the added events.
- GitHub CLI: create or merge `.github/hooks/neurostrata.json` with `version: 1`
  and camelCase `sessionStart`, `preToolUse`, `postToolUse`, and `agentStop`.
  Each command entry has `type: "command"`, platform-specific `bash` and
  `powershell` commands, `cwd: "."`, `timeoutSec: 10`, and an `env` map containing
  `NEUROSTRATA_HOOK_EVENT`.

All installers preserve foreign entries, recognize only their canonical runner,
repair stale owned paths, refuse symlink components, write atomically, and avoid
duplicate entries on refresh. GitHub CLI reports native-hook success, not
extension metadata, as wiring evidence. Its generated hook file, whose command
contains an absolute runner path, is added to `.git/info/exclude` like Codex
hooks.

### 12.4 Core-Backed Compiled Adapters

OpenClaw and Hermes become core-backed shims. Their `tsconfig.json` files map
`@neurostrata/plugin-core` to `../core/dist/index.d.ts`. Their build scripts are:

```text
tsc --project ../core/tsconfig.json && tsc --project tsconfig.json --outDir dist/.compiled && node ../core/bundle-adapter.mjs --compiled-adapter
```

`--compiled-adapter` requires the process working directory to be exactly
`plugins/openclaw` or `plugins/hermes`, reads core and that shim's `index.js`,
`index.d.ts`, `index.js.map`, and `index.d.ts.map`, writes the four final files
under the adapter's `dist/`, and removes `dist/.compiled` after success. Any
other working directory fails before writing artifacts.

JavaScript composition rules:

1. Remove core's default export and source-map comment.
2. Remove shim static import/re-export declarations only when the module
   specifier is exactly `@neurostrata/plugin-core`.
3. Require unaliased named core imports so retained references resolve against
   core's lexical declarations.
4. Preserve every other shim byte, including host registration and behavior.
5. Keep exactly one default export: the shim's. Fail the build otherwise.

Declaration and map rules:

- Concatenate core and shim declarations after the same targeted removals.
- Emit fresh JS and declaration maps; never retain stale maps.
- JS map `sources` are exactly `../../core/index.ts` then `../index.ts`.
- Declaration map `sources` are the same two paths.
- `sourcesContent` contains the exact UTF-8 source text. Mappings include the
  core-to-shim line offset. Omit `sourceRoot`.
- Default bundling for OpenCode, Claude, Codex, and GitHub CLI also regenerates
  both maps from core output plus the adapter's extracted plugin lines, with
  `sources` exactly `../../core/index.ts` then `../index.ts`.

## 13. Errors, Edge Cases, and Fail-Soft Behavior

| Case                                    | Required behavior                                                              |
| --------------------------------------- | ------------------------------------------------------------------------------ |
| Corrupt `active_session.json`           | Reset only in-memory working set; warn once; do not overwrite the corrupt file |
| Oversized session file                  | Treat as corrupt; do not read or overwrite it                                  |
| Invalid `working_set` field             | Drop that field and continue with a clean working set                          |
| Namespace, project, or session mismatch | Reset working set; never reuse another scope's nodes                           |
| Epoch mismatch                          | Adopt newer epoch and clear derived state; discard older nodes                 |
| Missing stable session ID               | Use isolated in-memory state only; do not persist or inject                    |
| Expansion denied or unavailable         | Keep stub; no raw restricted content enters state                              |
| No valid smaller stub                   | Retain full node; omit its frame line if necessary                             |
| Atomic write failure                    | Keep prior file; continue with in-memory state; clean temporary file           |
| Concurrent session writers              | Last complete atomic write wins; cache miss reconstructs from heap             |
| `turn_seq` exhaustion                   | Reset sequence, touch ages, turn/event identity, and fingerprints              |
| Duplicate event ID                      | No-op within the retained 128-event window                                     |
| Frame cannot fit all content            | Preserve immortal nodes, compact projections, omit lowest-priority full lines  |
| Zero budget in pure kernel              | Evaluate deterministically; JSON state still requires exactly `4096`           |
| Duplicate node IDs                      | Newest same-kind touched entry wins; arrays are re-sorted                      |
| Conflicting node kind                   | Reject incoming node; preserve existing immortal identity                      |
| Invalid path or pointer token           | Reject node; never interpolate raw control characters                          |
| Unknown tool output shape               | Preserve original output unchanged                                             |
| Append-only host                        | Emit changed frame once; do not claim transcript replacement                   |
| Codex result replacement request        | Never emit unsupported replacement or suppression fields                       |

Runner catches are event-aware:

- `PreToolUse` failures retain fail-closed guard behavior.
- `PostToolUse`, `SessionStart`, `UserPromptSubmit`, `Stop`, and `agentStop`
  failures return a valid event-specific no-op with exit code zero.
- Unknown events return a neutral event-shaped success with exit code zero;
  they never fall through to a `PreToolUse` allow or deny response.
- GC errors never block tool execution. Guard errors remain fail-closed.

## 14. Security and Privacy

- Scope is canonical `project_id` plus namespace plus session ID plus epoch.
- Only ReBAC-filtered daemon results create memory nodes or governing IDs.
- Restricted memory content is never copied into stubs, digests, or logs.
- `evicted_stubs` contain opaque IDs and paths, not memory bodies.
- `tool_output_stubs` and `tool_output_digests` contain SHA-256 prefixes and byte
  counts, not raw output.
- `.NeuroStrata/active_session.json` remains local and gitignored.
- `working_set` and all child collections are excluded from SynapticMesh export
  and default normative recall.
- A principal or session change clears in-memory state before reuse.
- Pointer rendering escapes control characters and delimiter bytes.
- No GC path accepts caller-supplied absolute paths or writes outside the
  canonical project state directory.
- State reads are capped at 1 MiB; symlinked state paths are rejected.
- `project_id` is a one-way digest and never stores the absolute root path.

## 15. Exact File Map

### Modify

| File                              | Responsibility                                                                           |
| --------------------------------- | ---------------------------------------------------------------------------------------- |
| `src/formal_kernel.rs`            | GC constants, types, pure decision function, unit tests, Kani harnesses                  |
| `proofs/Main.lean`                | Import proof module and emit ordered GC parity rows                                      |
| `tests/lean_parity_test.rs`       | Add `2560` rows and update expected total to `3833`                                      |
| `docs/vision/product-vision.md`   | Document persistent-heap and bounded-L1 formal kernel                                    |
| `plugins/core/index.ts`           | Scope, JSON contract, mirror kernel, cycles, sweep, render, dehydration, delta injection |
| `plugins/core/test.mjs`           | GC, page-fault, immortality, cap, privacy, hook, and fail-soft tests                     |
| `plugins/core/bundle-adapter.mjs` | Fresh four-artifact bundles and compiled-adapter mode                                    |
| `src/task/harness_plugins.rs`     | Claude/Codex/GitHub native event installation and installer tests                        |
| `plugins/opencode/test.mjs`       | Result dehydration and mutable frame replacement tests                                   |
| `plugins/claude/cli-hook.mjs`     | Native result-event mapping and event-aware catch                                        |
| `plugins/claude/test.mjs`         | `PostToolUse`, start, stop, and fail-soft tests                                          |
| `plugins/codex/cli-hook.mjs`      | Native result-event mapping without unsupported output fields                            |
| `plugins/codex/test.mjs`          | Result observation, additional context, and fail-soft tests                              |
| `plugins/github-cli/cli-hook.mjs` | Native camelCase normalization and output mapping                                        |
| `plugins/github-cli/test.mjs`     | Native result replacement and additional-context tests                                   |
| `plugins/openclaw/index.ts`       | Core-backed shim while preserving host registration                                      |
| `plugins/openclaw/tsconfig.json`  | Core declaration path mapping                                                            |
| `plugins/openclaw/package.json`   | Core-plus-shim compile and bundle script                                                 |
| `plugins/openclaw/test.mjs`       | Host registration preservation and bundled GC API tests                                  |
| `plugins/hermes/index.ts`         | Core-backed shim while preserving host registration                                      |
| `plugins/hermes/tsconfig.json`    | Core declaration path mapping                                                            |
| `plugins/hermes/package.json`     | Core-plus-shim compile and bundle script                                                 |
| `plugins/hermes/test.mjs`         | Host registration preservation and bundled GC API tests                                  |

### Create

| File                                   | Responsibility                                       |
| -------------------------------------- | ---------------------------------------------------- |
| `proofs/NeuroStrata/WorkingSetGc.lean` | Five formal GC theorems and canonical ordering model |

### Rebuild

Every package below regenerates all four tracked artifacts:
`dist/index.js`, `dist/index.js.map`, `dist/index.d.ts`, and
`dist/index.d.ts.map`.

- `plugins/core/dist/`
- `plugins/opencode/dist/`
- `plugins/claude/dist/`
- `plugins/codex/dist/`
- `plugins/github-cli/dist/`
- `plugins/openclaw/dist/`
- `plugins/hermes/dist/`

No UI file, database schema, MCP schema, dependency version, lockfile, or Cargo
manifest change is permitted. Package-script and TypeScript path-map edits in
§15 do not add dependencies.

## 16. Focused Verification

### 16.1 Rust and Lean

Run the Node builds in §16.2 first so Rust `include_str!` inputs contain the
regenerated adapters, then run:

```bash
cargo test --locked --bin neurostrata-mcp formal_kernel::tests::working_set -- --nocapture
cargo test --locked --bin neurostrata-mcp task::harness_plugins::tests::working_set_gc -- --nocapture
cargo test --locked --test lean_parity_test rust_formal_kernel_truth_table_matches_lean_cli -- --nocapture
cargo test --locked --test lean_parity_test lean_proof_sources_contain_no_sorry_or_axiom -- --nocapture
lake --dir proofs build
cargo check --locked --bin neurostrata-mcp
```

When Kani is installed, run `kani src/formal_kernel.rs` with the working-set
proof harnesses selected. Do not run a full-workspace test suite in the subagent
lane.

### 16.2 Node and Adapters

```bash
npm --prefix plugins/core run build
for adapter in opencode claude codex github-cli openclaw hermes; do
  npm --prefix "plugins/$adapter" run build
done
artifacts=(plugins/{core,opencode,claude,codex,github-cli,openclaw,hermes}/dist/index.{js,js.map,d.ts,d.ts.map})
sha256sum "${artifacts[@]}" > /tmp/opencode/working-set-gc-before.sha256
npm --prefix plugins/core run build
for adapter in opencode claude codex github-cli openclaw hermes; do
  npm --prefix "plugins/$adapter" run build
done
sha256sum "${artifacts[@]}" > /tmp/opencode/working-set-gc-after.sha256
diff -u /tmp/opencode/working-set-gc-before.sha256 /tmp/opencode/working-set-gc-after.sha256
npm --prefix plugins/core test
node --test plugins/core/test.mjs plugins/opencode/test.mjs plugins/claude/test.mjs plugins/codex/test.mjs plugins/github-cli/test.mjs plugins/openclaw/test.mjs plugins/hermes/test.mjs
git diff --check -- "${artifacts[@]}"
```

Review the generated artifact diff and stage all 28 files. The two SHA-256
manifests must match byte-for-byte; a second build must not alter any artifact.

The core suite verifies:

- the TypeScript mirror matches all `2560` Rust/Lean boundary rows;
- immortal nodes survive every age and budget combination;
- persisted `max_frame_bytes` values other than `4096` invalidate only
  `working_set`;
- a fitting re-referenced stub promotes to young generation;
- a non-fitting or failed expansion remains a stub;
- age `3` retains and age `4` demotes with default TTL;
- pressure demotion reduces charged bytes and preserves current pinned roots;
- rendered UTF-8 frame length never exceeds `4096`;
- unchanged fingerprints cause no event mutation or hook context;
- changed fingerprints replace exactly one mutable prior frame;
- a small first read records duplicate history while staying inline; its next
  read produces a deterministic stub;
- output over `3072` bytes and duplicate reads produce deterministic stubs;
- mutating output, errors, unknown shapes, and non-text parts remain unchanged;
- corrupt state, scope mismatch, missing session ID, and write failure never
  block tool execution;
- restricted IDs and raw output never enter persisted state;
- digest elements have exactly the §8.4 fields, identity is stable across
  processes, and identity differs by project, namespace, session, or epoch;
- all six bundles expose the GC API and exactly one default export;
- runtime imports are limited to the four approved Node built-ins.

Adapter suites verify:

- OpenCode replaces eligible result text and mutable context exactly once.
- Claude emits shape-correct `PostToolUse` output and event-aware no-op catches.
- Codex emits additional context but never `updatedMCPToolOutput` or
  `suppressOutput`; raw `tool_response` remains acknowledged.
- GitHub CLI emits `modifiedResult` only for successful text replacement.
- OpenClaw and Hermes retain host-specific registration and intent behavior.
- Every generated JS and declaration map is valid JSON, names exactly the core
  and adapter sources in §12.4, embeds exact source text, and contains non-empty
  mappings.

Installer tests verify:

- Claude owns exactly one added `PostToolUse` entry with the file-tool matcher;
- Codex owns `SessionStart`, `UserPromptSubmit`, unmatchered `PostToolUse`,
  `PreToolUse`, and `Stop` entries;
- GitHub CLI writes native camelCase `sessionStart`, `preToolUse`, `postToolUse`,
  and `agentStop` commands with the correct event environment;
- foreign hooks survive, stale owned runner paths are repaired, symlink
  components are refused, writes are atomic, and refresh adds no duplicates.

## 17. Acceptance Criteria

1. All five Lean theorems compile with zero `sorry` and zero `axiom`.
2. Rust and Lean emit identical ordered `3833`-row parity output.
3. The TypeScript mirror matches the Rust decision table on the full boundary
   corpus.
4. Every rendered frame is at most `4096` UTF-8 bytes.
5. Golden Rules and the active-task header are never demoted or removed.
6. Page-fault re-expansion restores full young-generation content through
   ReBAC-filtered daemon reads.
7. Large and repeated inspection output is replaced by deterministic stubs on
   OpenCode, Claude, and GitHub CLI. Codex records bounded stub context without
   claiming unsupported result removal. OpenClaw and Hermes mark touched paths
   without claiming result payloads they do not receive.
8. All six harnesses run the shared core cycle. Mutable hosts replace prior
   frames; append-only hosts emit each changed fingerprint once.
9. `working_set` never crosses project, namespace, session, principal, or mesh
   boundaries.
10. Corrupt state and unavailable expansion fail soft without blocking tools.
11. All seven packages rebuild all 28 JS, declaration, and map artifacts
    deterministically, with fresh source mappings and exactly one default export.
12. No UI, daemon endpoint, MCP schema, database schema, dependency version,
    lockfile, or Cargo manifest changes are present.
13. Focused commands in §16 pass with fresh output before completion.
