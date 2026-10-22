# NeuroStrata Source Anchor & Worktree Export Plan

## Source findings

- `GUINEA-PIG-FINDING-keywest.health-2026-10-16-governs-edge-materialization.md` — GOVERNS edges skipped silently on write; caller gets no deferred-edge info or memory ID.
- `GUINEA-PIG-FINDING-keywest.health-2026-10-17-structural-absolute-path-supersede.md` — supersede_memory replaces locations/governs but leaves stale metadata.absolute_path.
- `GUINEA-PIG-FINDING-keywest.health-2026-10-18-branch-aware-source-anchors.md` — branch-only governs targets fail CI export gate; exporting from a linked worktree rewrote 926/929 shared nodes' absolute_path to transient worktree root.

Root cause (verified at the time): the export computed `absolute_path` via `dunce::canonicalize(std::env::current_dir().join(location))` at export time — caller-CWD-dependent. *(That code is gone: the export now resolves the path through the source-anchor registry — `crate::source_context::export_absolute_path`, `src/source_context.rs:42-99` — and `dunce` is no longer called from any file in `src/`.)*

## Implementation progress (2026-10-18)

- `VectorStore::upsert` now returns an `UpsertReport` with the stored ID and
  unresolved edge targets, relation, array index, and resolution outcome.
- `neurostrata_add_memory` reports its new ID and deferred edges; unresolved
  writes remain successful. Supersede responses also disclose deferred edges.
- `neurostrata_supersede_memory` removes stale `metadata.absolute_path` when
  replacement locations are supplied.
- Export rebases relative locations against explicitly configured
  `NEUROSTRATA_ANCHOR_ROOT`; it no longer derives new paths from ambient CWD
  or the daemon's potentially worktree-specific `NEUROSTRATA_PROJECT_ROOT`.
  Without an explicit root, export preserves stored absolute paths and does
  not invent one.
- `metadata.refs` remains the VCS-agnostic provenance extension point. Core
  accepts optional `locations[].source_context` and exports it unchanged; Core
  does not inspect Git. The Keywest project-pipeline gate and optional Git
  adapter are not implemented in this repository yet.
- Tests cover deferred-edge reporting, array indices, explicit-root export
  path derivation, and supersede cleanup.

## Architect verdicts (3x CONDITIONAL APPROVE)

gemini-arch, k3-arch, openai-arch all conditionally approve a **VCS-agnostic Workspace Anchor & Provenance Contract**.

## Core constraint: Git-as-Config

NeuroStrata is universal — must NOT assume Git. Config Git before assuming every project uses it. Core never shells to Git. Git worktree→canonical-root mapping lives in an optional client-shim/plugin. Project-side repo-content checks stay project-side.

## Design

### 1. Stable absolute_path — root-resolution strategy in Core

Current implementation uses explicit `NEUROSTRATA_ANCHOR_ROOT` only. It is
project-agnostic and Git-free. Persisting a namespace-level root and adding an
opt-in Git adapter remain follow-up work; ambient CWD and the session root are
not export fallbacks.

When the explicit root is set, `export_graph` rebases relative locations to it,
including previously stored absolute paths. It never adopts transient caller
CWD. Missing targets retain the deterministic `<root>/<location>` path.

### 2. Supersede path hygiene (report #2 fix)

`handle_supersede_memory`: when replacement `locations` are supplied, remove
`metadata.absolute_path`; export may recompute from the configured root.

### 3. Distinguish sibling-branch file from genuinely stale path

Anchor provenance metadata on `metadata.refs` entries (additive, VCS-agnostic),
supplied through the optional `locations[].source_context` object:
```json
"refs": [{"file": "scripts/tests/model-catalog.sh", "source_context": {"provider": "git", "revision_scope": "branch:feat/model-catalog"}}]
```
- A configured provider may populate its provenance; Core stores/exports the
  object opaquely and never runs or assumes Git.
- Consumer gate (project-pipeline, project-side):
  1. `<canonical_root>/<path>` exists in current checkout → PASS
  2. Git-configured: `git cat-file -e <revision_scope>:<path>` → PASS with `INFO: branch-scoped anchor`
  3. Absent from tree AND absent from declared/live refs → FAIL (genuinely stale, exit 1)

### 4. Deferred-edge exposure (report #1 fix)

`VectorStore::upsert` returns `Result<UpsertReport>` with `id` + deferred edge
relation, declared target, declaration index, and resolution outcome.
`pick_declared_target` distinguishes `Resolved`, `Missing`, and `Ambiguous`;
lookup/materialization failures are reported separately.
States: Materialized | DeferredMissing | SkippedAmbiguous | Failed.
MCP response always returns memory `id` + structured deferred-edge details.

### 5. Core vs shim boundary

| Concern | Home |
|---|---|
| anchor_root resolution, export path, supersede fix, deferred-edge surfacing | Core (project-agnostic) |
| worktree→canonical mapping, branch provenance population | plugins/git/ shim |
| CI gate semantics for branch-only targets | project-side (project-pipeline) |

## Open questions

1. `anchor_root` migration for existing namespaces with cwd-baked paths — one-time `--reanchor` sweep or lazy per-node?
2. Multiple canonical roots per namespace (mono-repo) — single root sufficient for v1?
3. `root_id` hash: salt per-install to avoid leaking layout in shared exports?
4. Deferred-edge re-resolution trigger: every ingest only, or on-demand `resolve` tool?
5. Blocker (openai-arch): namespace-wide ingestion clears structural rows; provenance alone cannot support simultaneous branch graphs.

## Tasks

| Task | ID | Status | Report |
|------|----|--------|--------|
| Design branch-aware memory anchors & stable worktree exports | neurostrata-najy | in_progress (claimed) | 10-18 |
| Surface unresolved relationship-edge warnings from memory writes | neurostrata-j7d7 | in_progress (claimed) | 10-16 |
| Structural absolute_path cleared/recomputed on supersede | neurostrata-qy1m | in_progress (claimed) | 10-17 |

## Next step

Remaining: decide namespace-root persistence/config UX, add optional Git-provider
integration only after explicit project configuration, and update the Keywest
project-pipeline gate to distinguish declared branch-scoped anchors from stale
paths without weakening fail-closed behavior for genuinely stale sources.
No PHI, credentials, live Tebra access, or patient data involved.
