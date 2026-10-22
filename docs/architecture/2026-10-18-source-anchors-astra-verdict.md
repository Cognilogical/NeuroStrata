# Independent OpenAI verdict: source anchors and stable exports

**Verdict: CONDITIONAL.** Approve provider-neutral anchors and explicit
root bindings. Reject Git-by-default, export-time CWD inference, and
treating checkout absence as stale. Proposal awaits reconciliation.

## Evidence and intent

Evidence: supplied findings and repository only; no other architect output.
Goal: lossless, stable shared exports without requiring Git; preserve
fail-soft writes while exposing missing graph edges.

`src/store/ladybug.rs:1584-1625` prefers metadata.absolute_path, then
canonicalizes location against process CWD. *(the cited code is gone: the
export now resolves the path through the source-anchor registry —
`crate::source_context::export_absolute_path`, `src/source_context.rs:42-99`
— and `dunce` is no longer called from any file in `src/`; 2026-10-21)*
Fixing only the fallback
leaves stale metadata authoritative. This code derives export output;
it does not itself demonstrate database mutation.

Lines 1759-1833 replay declarations. Lines 2294-2316 resolve graph nodes,
not files. Lines 1835-1847 clear every auto-ingestor row in a namespace:
provenance alone cannot make concurrent branch graphs safe. *(the anchors
were re-derived; 2026-10-22)*

## Recommended design

### 1. Separate identity, provenance, and local resolution

- Portable anchor: `source_id`, normalized relative path, optional symbol
  or line selector. Namespace alone is not repository identity.
- Optional observation: provider, immutable revision, mutable ref label,
  workspace ID, dirty flag; optional digest for dirty/untracked content.
  Observation describes where seen, NOT where a rule applies.
- Applicability is separate: all revisions or explicit context selector.
  Legacy applicability remains unspecified; never infer branch scope
  merely from the branch on which an anchor was captured.
- Local registry binds source ID + workspace ID to root. Optional stable
  `export_root` is explicitly configured per source. Machine paths and
  mutable branch names never define identity. Worktree roots stay local
  unless sharing is explicitly requested.

### 2. Git-as-config, not Git detection-as-consent

Default provider: filesystem/no VCS. Explicit configuration enables Git
and binds repository identity to source ID. Disabled means no Git process,
`.git` assumption, remote lookup, or mandatory Git dependency.

Optional Git adapter supplies observations and explicitly requested local
revision probes. No checkout switching or implicit fetching. Missing Git,
objects, or configuration means unknown, not invalid.

### 3. Stable export serializes; it does not discover files

- Portable locator is source ID + relative path, not an absolute path
  under a supposedly portable root. Absolute roots are machine-local.
- Remove CWD canonicalization. Compatibility `absolute_path`, if needed,
  is a lexical join to an explicitly configured per-source export root,
  independent of existence and exporting worktree. Mark derived paths
  unverified. Without configuration, fabricate no new absolute path.
- Resolve editor links against the consumer's chosen workspace separately.
  Canonicalization, symlink checks, and existence checks belong there,
  never in shared identity or default export.
- Preserve full metadata and declarations. Legacy absolute_path remains
  legacy evidence, not authority. Explicitly migrate conflicts rather
  than silently deleting metadata. Superseding locations invalidates or
  replaces derived bindings atomically; retired records retain history.
- Reject traversal outside registered roots. Intentional external files
  require separate registered sources. Normalize separators, not case.
- Fixed snapshot, configuration, options, and lifecycle evaluation time
  yield deterministic ordering and bytes across CWDs. No fresh timestamps
  or live probe results enter stable artifacts. New filtering is opt-in;
  preserve existing lifecycle options.

### 4. Existence needs evidence and a named comparison context

Keep three dimensions independent: applicability, graph materialization,
and source availability. An edge does not prove a live file; a live file
does not prove an ingested graph target.

Availability observations:
- `present_here`
- `verified_at_other_context`
- `missing_in_checked_context`
- `unknown`

Include checked source, revision/path, and evidence basis. A captured
branch label proves neither current sibling existence nor staleness.
Immutable revision evidence proves existence there. Current sibling-branch
existence additionally requires resolving its current tip. Dirty/untracked
observations require workspace evidence, not a commit claim.

Project-side policy decides when missing means genuinely stale, which
contexts must be checked, and whether unknown blocks CI. Existence on
some other branch does not exempt a rule that applies to this checkout.

### 5. Deferred edges share anchors, not filesystem guesses

Successful memory writes return stable `memory_id` plus per-edge results:
declaration identity, declared target/anchor, resolved target ID when
available, materialized/deferred status, and a reason.

Use `target_not_found_in_graph`, `ambiguous_target`, or `resolution_error`.
Do not claim `target_not_ingested` from a failed lookup alone. Keep source
availability separate from graph-resolution reasons.

Preserve declarations durably. Idempotent relink uses the same
source/context-aware resolver and exposes unresolved outcomes. Export
declarations even when edges are deferred. Return diagnostics separately
or derive them deterministically from the graph snapshot; do not persist
transient warning timestamps into portable exports.

Missing targets remain fail-soft. Report memory IDs with truthful write
outcomes. Resolution failure must not masquerade as materialization.
Ambiguous path/context matches never bind opportunistically across sources.

## Boundaries and phased delivery

Core owns generic anchor/config contracts, lossless serialization, graph
diagnostics, and provider-neutral resolver interfaces. Git mechanics live
in an optional source-provider adapter, not mandatory store logic.

Git is not an AI client: generic Git semantics do not belong in an
OpenCode-only shim. Client transport, editor links, and hook injection
belong under `plugins/<client_name>/`. Repository-content checks and CI
stale-path policy remain project-side. Preserve closed `runs_in` enum;
non-Git gates retain free-text `gate_point`.

Phase 1: deterministic export, supersede invalidation, write diagnostics.
Do not promise multi-branch graph retention in this phase.

Phase 2: context-scoped structural ingestion/identity if simultaneous
branch graphs are required. Until then, disclose that ingestion replaces
the namespace's structural snapshot: declarations survive, but sibling
graph nodes are not guaranteed. Provenance does not require splitting
shared memories into branch namespaces.

## Alternatives rejected

- Canonical Git checkout root reduces single-machine churn but violates
  universal defaults and does not make machine paths portable.
- Branch-namespaced memories fragment shared knowledge and mishandle
  branch renames, detached revisions, and dirty trees.
- Recommended: provider-neutral anchors plus explicit local bindings.

## Acceptance criteria for later validation

- No-VCS projects work without Git installed or invoked.
- Same snapshot exported from main, linked worktree, or unrelated CWD
  yields identical shared artifact.
- Branch-only and dirty/untracked anchors preserve declarations;
  unknown never automatically becomes stale.
- Superseded locations cannot resolve through stale authoritative paths.
- Writes return memory ID and deferred reasons; later ingest/relink
  materializes once, without cross-source/context misbinding.
- Full metadata round-trips. Filtering is explicit. Ambiguous legacy
  migrations are reported rather than guessed.
- If simultaneous branch graphs are claimed, one ingest cannot erase
  another context's structural snapshot.

## Open decisions

1. Is one active structural snapshot sufficient, or must concurrent branch
   graphs be supported immediately?
2. Who provisions stable source IDs/root bindings, including multiple
   repositories sharing one namespace?
3. Are anchors revision-pinned, floating, or both? What applicability
   policy governs legacy rules?
4. Which consumers require top-level absolute_path, and what migration
   or schema-versioning contract can they accept?

No implementation or checker runs. Worker delegation hit depth limit;
this design document was persisted directly under the design exception.
