# Keywest guinea-pig report: branch-aware source anchors and worktree exports

**Observed:** 2026-10-18
**Reporter task:** `neurostrata-ejyk`
**Scope:** Keywest linked-worktree export behavior and shared `keywest.health` source anchors; no NeuroStrata source-code change.

## Summary

Keywest has multiple active Git worktrees sharing one NeuroStrata memory namespace. A memory can govern a path that exists in a sibling branch but not in the currently checked-out branch. The Keywest export-integrity gate resolves those paths against the current checkout and fails. Separately, exporting the shared graph from a linked worktree rewrites many exported `absolute_path` values to the transient worktree root. Re-exporting from the canonical checkout removes the transient root paths, but does not resolve branch-only `governs` targets.

This report describes observed consumer behavior. It does not assert that NeuroStrata currently promises branch-scoped anchors or that a particular product design is required.

## Expected / triage question

When multiple worktrees share a memory namespace, source references should retain enough provenance to distinguish:

1. a source file that is missing from the current checkout but exists on a known sibling branch/worktree; and
2. a source path that is genuinely stale or invalid.

Exporting from a transient worktree should not rewrite the portable graph backup's absolute paths for hundreds of unrelated records to that worktree root. Please assess whether NeuroStrata should represent branch/worktree provenance for source anchors and make export path handling stable across worktrees. Consumers must still be able to detect genuinely stale canonical sources.

## Actual

### Branch-only `governs` targets

In Keywest, `neurostrata-export-gate.sh` checks each live memory's `governs` paths against the current repository checkout. The shared graph had six unresolved edges to three source paths that were absent from that checkout:

- `scripts/tests/model-diversity-gate.sh` — three memory edges; the test file exists on historical/non-current source branches.
- `scripts/tests/model-catalog.sh` — one edge; the test file exists on a non-current source branch.
- `docs/superpowers/plans/2026-10-17-local-tebra-test-admin-tools.md` — two edges; the plan exists in a sibling `local-tebra-admin-implementation` worktree, not in the current checkout.

`neurostrata-export-gate.sh` fails on these unresolved targets. The same Keywest branch's `neurostrata-sync-gate.sh` passes because the relative repository paths it checks resolve in that checkout. The sync check does not establish that branch-only source anchors are portable to another checkout.

The relevant memory IDs and source-branch evidence are recorded in task `neurostrata-ejyk` and Keywest task `keywest-health-o3in`.

### Worktree-root `absolute_path` rewrite

Comparing the canonical Keywest `origin/master` export (2,128 nodes) with Keywest Task 2 commit `4e99b0c` (2,132 nodes) showed:

- 2,128 node IDs were shared between the exports.
- 929 shared nodes had a changed `absolute_path`.
- 926 of those paths pointed under `/home/kenton/.local/share/opencode/worktree/875bf5/argon-consult-substitution/`.

The graph was then exported with the same `NEUROSTRATA_EXPORT` destination while running the export script from the canonical Keywest checkout. The resulting graph had **zero** `absolute_path` values under the transient worktree root. This isolates the export working directory as a factor in the observed path rewrite. The subsequent export-integrity check still failed on the branch-only `governs` targets above.

The export used the supported `scripts/neurostrata-export.sh` path, without `--ingest`; no generated graph file was edited by hand. The exact canonical-root command was:

```bash
NEUROSTRATA_EXPORT=/home/kenton/.local/share/opencode/worktree/875bf5/argon-consult-substitution/docs/neurostrata/graph.json \
  /home/kenton/Documents/keywest.health/scripts/neurostrata-export.sh
```

## Reproduction outline

1. In the canonical Keywest checkout, inspect/export `docs/neurostrata/graph.json` and record `absolute_path` values for shared node IDs.
2. Use a linked worktree on a different branch while sharing the same NeuroStrata memory namespace.
3. Ensure a memory's `governs` path names a file present only in the sibling branch/worktree. Run the Keywest `scripts/neurostrata-export-gate.sh` from a checkout that lacks that file; it reports the path unresolved.
4. Run `scripts/neurostrata-export.sh` from the linked worktree, targeting its graph backup. Compare the exported `absolute_path` values for the same node IDs.
5. Run the same export from the canonical checkout while setting `NEUROSTRATA_EXPORT` to the worktree's graph output. The transient worktree prefix disappears; the branch-only `governs` failures remain.

The absolute-path comparison used `origin/master` at `a3a0cea` and task commit `4e99b0c`. The canonical re-export used the backup-first script; it did not use `--ingest` or rewrite IDs.

## Impact and current checks

- The Keywest export-integrity gate is CI-blocking and currently fails on the missing branch-only `governs` paths. This is why an agent in one worktree can encounter red validation caused by sources another agent is editing elsewhere.
- The Keywest sync gate passes on the current checkout (zero relative stale paths; all quoted owner lines recorded), but it does not report the branch provenance of an anchor whose target is absent from that checkout.
- The exported graph is the checked-in backup. A transient worktree absolute path is unstable after that worktree is removed, even when a relative location remains useful.
- A related report covers a structural `absolute_path` that survives supersede: `GUINEA-PIG-FINDING-keywest.health-2026-10-17-structural-absolute-path-supersede.md`. This observation is broader: one export from a linked worktree changed hundreds of shared nodes.
- The 2026-10-16 `GOVERNS` edge-materialization report concerns write-time target resolution and diagnostics: `GUINEA-PIG-FINDING-keywest.health-2026-10-16-governs-edge-materialization.md`. This report concerns later export/validation across worktrees; neither report proves the other's root cause.

## Triage request

Please determine whether NeuroStrata should carry branch/worktree context for file anchors, whether graph export should canonicalize absolute paths rather than adopt a transient caller root, and how a consumer should distinguish a file on another active branch from a genuinely stale source. A solution should preserve fail-soft memory writes and should not require consumers to hand-edit the generated graph. No implementation design is assumed by this report.

## Scope and safety

- No PHI, credentials, live Tebra endpoints, patient records, or clinical data were accessed.
- No NeuroStrata source code or NeuroStrata-namespace memory was changed. The Keywest `keywest.health` memory update/export is recorded separately.
- Other agents' branches and memories were not modified. Only source references needed for this report were inspected.
