# Universal Source-Anchor Registry — Design Spec

**Date:** 2026-10-18
**Status:** Draft for owner review; this document does not authorize
implementation by itself.
**Scope:** NeuroStrata Core anchor-root configuration and stable graph export
for every client. No project-specific gate or memory repair.

## Intent

Make graph export start from the same project context as the `.NeuroStrata`
directory and keep source paths stable when one shared database serves multiple
repositories, linked worktrees, or unrelated process working directories. Bind
each stable source identity to its canonical `.NeuroStrata` context directory;
derive the project root from that marker's parent.

Core remains VCS-agnostic. It stores and exports source provenance, but it does
not discover repositories, inspect Git refs, check whether a branch contains a
file, or decide whether a consumer should reject a missing source.

## Current behavior and failure mode

`export_graph` reads one process-wide `NEUROSTRATA_ANCHOR_ROOT`. For every row
with a relative primary `location`, it joins that single root and emits the
result as `absolute_path`. The root has no relationship to the row's source
identity. A caller exporting a shared database from one checkout can therefore
rebase relative locations belonging to other sources onto that checkout. The
CLI also defaults the graph output to the CWD-relative
`.NeuroStrata/graph/graph.json`, which can select the wrong context when invoked
from an unrelated directory or a linked worktree without its own marker.

Core already accepts and preserves `locations[].source_context` as opaque
metadata. Core v1 also avoids using the ambient working directory when no
explicit root is configured. The missing capability is a local, source-keyed
root registry and a safe rule for applying a binding to just one source.

## Goals

1. Bind roots locally by stable `source_id`, not by process CWD or the root of
   whichever worktree happened to launch export.
2. Allow more than one configured source root in a single NeuroStrata client
   configuration.
3. Keep machine paths out of memories and graph exports as configuration data;
   they may appear only in the legacy `absolute_path` projection when a matching
   local binding is explicitly configured.
4. Preserve the provider-neutral `source_context` contract. Core may read the
   standardized `source_id` discriminator; it treats provider-specific fields
   (such as revision or workspace details) as opaque and round-trips them.
5. Preserve old configurations and fail safely for legacy or ambiguous rows.
6. Produce the same export for the same database snapshot and config regardless
   of process CWD or which linked worktree invokes export.

## Non-goals

- Git support, repository discovery, ref fetching, branch switching, or tree
  inspection in Core.
- Deciding whether a path on another revision satisfies a consumer's policy.
- Simultaneous branch-specific structural snapshots or namespace ingestion.
- Automatically migrating existing memories or rewriting historical absolute
  paths.
- Project-specific IDs, paths, provider implementations, or CI gates.
- Fetching files or checking target existence during export.

## Local configuration contract

Extend the existing user-local `~/.config/neurostrata/config.json` with an
optional `project_contexts` object. Each key is a stable, nonempty `source_id`;
each value is the absolute path to that source's canonical `.NeuroStrata`
directory. Core derives the project root from the marker's parent.

```json
{
  "db_path": "/local/path/to/ladybug.db",
  "project_contexts": {
    "source-id-a": "/local/canonical/source-a/.NeuroStrata",
    "source-id-b": "/local/canonical/source-b/.NeuroStrata"
  }
}
```

These marker paths are local configuration. They are not stored in LadybugDB,
copied to another machine, or emitted in graph metadata. Older configs without
`project_contexts` deserialize as an empty mapping. A binding is valid only
when its final directory is `.NeuroStrata` and the directory exists.

The configuration UX is a CLI surface, not a new MCP/server tool:

- `neurostrata-mcp project-context bind <source-id> <context-dir>` validates
  one existing `.NeuroStrata` directory, then atomically writes its local
  binding.
- `neurostrata-mcp project-context list` displays source IDs and their context
  directories.
- `neurostrata-mcp project-context unbind <source-id>` removes only that
  binding.

These commands do not start the daemon, access the database, or edit memories.
An invalid source ID or marker path fails with a clear diagnostic and leaves
the config unchanged.

`export-graph [output_json_path] [--source-id <id>]` writes its default output
to `<selected-context>/graph/graph.json`, where `selected-context` is the
`.NeuroStrata` directory. An explicit output path overrides only this
destination. `--source-id` selects a registered context. Without it, the CLI
uses `NEUROSTRATA_PROJECT_ROOT` when that project contains `.NeuroStrata`; if
unset, it searches parent directories from the invocation location for an
existing `.NeuroStrata` marker. The process directory is only a search start:
the marker defines the context, and the project root is its parent. If no marker
is found, the command requires a source ID or explicit output path; it never
creates a new marker tree under an unrelated directory. A linked worktree
without its own marker can select the registered canonical context by ID or use
the canonical `NEUROSTRATA_PROJECT_ROOT`.

The selected context controls the graph file's destination. It does not become
the root for every node in the shared graph: each node's relative `location` is
resolved through its own `source_context.source_id` binding below.

## Source identity and resolution

`locations[].source_context.source_id` is the stable, provider-neutral identity
used to select a registered `.NeuroStrata` context. The provider adapter
supplies this identity and any additional provenance it owns. Core does not
require a particular provider and does not interpret any other
`source_context` field.

For each graph node's primary relative `location`, export follows this order:

1. Find `metadata.refs[]` entries whose `file` exactly matches `location` and
   whose `source_context.source_id` is a nonempty string.
2. If all matching entries agree on exactly one `source_id`, look up that ID in
   local `project_contexts`.
3. With one explicit marker binding, derive the project root from the marker's
   parent and derive `absolute_path` as a deterministic lexical join of that
   root and normalized relative location. Do not canonicalize or check target
   existence during export.
4. If there is no matching identity, no configured root, or conflicting source
   IDs, preserve the stored `absolute_path`. Do not select another source's
   root, infer from namespace, or fall back to process CWD.
5. Preserve an already-absolute `location` as legacy data; this feature does not
   authorize or register external absolute sources.

Traversal components in a relative location are never allowed to escape the
configured root. Unsafe locations are preserved as unresolved source data and
are not converted into a new absolute path.

## Environment compatibility

The current `NEUROSTRATA_ANCHOR_ROOT` is unscoped. Applying it to a global graph
can affect many unrelated source records. The new behavior scopes an environment
override to one source and requires the project marker:

- `NEUROSTRATA_ANCHOR_ROOT` may override a local binding only when
  `NEUROSTRATA_SOURCE_ID` names the source being exported and the corresponding
  `<root>/.NeuroStrata` directory exists.
- `NEUROSTRATA_ANCHOR_ROOT` without `NEUROSTRATA_SOURCE_ID` is ignored for
  graph-wide rebasing and emits a deprecation warning.
- A configured `project_contexts[source_id]` marker is used when no matching
  one-source environment override is provided.
- With neither source-specific binding nor override, export preserves the
  stored `absolute_path` and still exports relative `location` and provenance.

There is no ambient-CWD fallback. This prevents an unscoped process environment
from changing every source in a shared database.

## Provider boundary

- Core defines the stable source identity and local root-binding contract.
- Optional client/provider shims discover a project root and populate
  `locations[].source_context`; providers are explicitly selected by the client
  and remain outside Core's export implementation.
- Core stores and exports provenance without interpreting Git or provider
  revision semantics.
- Consumers decide whether `present_here`, another declared context, or
  `unknown` satisfies their own integrity policy. Unknown never becomes proof of
  a valid source merely because another root is configured.

## Compatibility and migration

- Existing `config.json` files remain valid and default to no project-context
  bindings.
- Existing records without `source_context.source_id` keep their stored
  `absolute_path`; no mass rebase or memory migration occurs.
- New provider-aware writes can add a stable source ID without changing the
  memory text or current vector identity.
- Existing consumers continue to receive the same graph shape. The change only
  makes `absolute_path` re-derivation source-specific and opt-in.
- Removing or changing a local root binding affects export projection only; it
  does not mutate stored memories or provider provenance.

## Verification criteria

1. Existing configs with no `project_contexts` load unchanged.
2. Two source IDs with different `.NeuroStrata` markers produce distinct,
   correct projections in
   one shared graph; either root never rewrites the other source.
3. The same snapshot and config export byte-identically from the canonical
   checkout, a linked worktree without its own marker, and an unrelated CWD.
4. The default graph file is written under the selected `.NeuroStrata` marker;
   absent or ambiguous context never creates a marker under CWD.
5. Missing, unknown, or ambiguous source identity preserves the stored absolute
   path and never uses CWD or another source's root.
6. `source_context` fields round-trip unchanged; no Git process runs in Core.
7. Project-context bind/list/unbind commands validate IDs and marker paths, write atomically,
   and never access the memory database.
8. Existing export, supersede, and 546-test baseline contracts continue to pass.

## Decisions and limitations

- One active structural snapshot per namespace remains the supported model in
  this phase. Provenance does not make concurrent branch ingests safe.
- A source ID denotes a stable source/repository identity; mutable branch names
  and machine paths are not source identity.
- The local project-context registry is a resolution aid only. It does not prove that a
  source file exists or that a consumer should accept it.
