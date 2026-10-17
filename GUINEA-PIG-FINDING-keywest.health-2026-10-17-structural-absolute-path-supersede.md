# Keywest guinea-pig report: structural absolute path survives supersede

**Observed:** 2026-10-17
**Reporter task:** `keywest-health-27hp`
**Scope:** one structural memory supersede and its exported path metadata; no NeuroStrata source changes.

## Expected

When `neurostrata_supersede_memory` receives replacement `locations` for a structural `directory` memory, the active replacement's `metadata.absolute_path` should be recalculated from the new location or cleared. A stale absolute path should not survive after the tool reports that locations and Governs were replaced.

## Actual

The active structural node `keywest.health::downloads` had `location: downloads` and `metadata.absolute_path: /home/kenton/Documents/keywest.health/./downloads`, but the directory does not exist in the canonical checkout. I superseded it with a retirement note anchored to `docs/runbooks/neurostrata.md`. The tool reported that `locations`, `governs`, and `source` were replaced. The fresh export showed the replacement location as `docs/runbooks/neurostrata.md`, but it retained the old `metadata.absolute_path` value. `scripts/neurostrata-sync-gate.sh` therefore continued to report the absolute downloads path as stale.

## Reproduction

1. Fetch structural node `keywest.health::downloads` and confirm its location and absolute path.
2. Confirm the path is absent from the checkout (`test -e downloads` is false and `git ls-files downloads` returns no files).
3. Supersede the node with `locations: [{path: "docs/runbooks/neurostrata.md"}]` and matching `governs`.
4. Run `scripts/neurostrata-export.sh`, then inspect the active replacement in `docs/neurostrata/graph.json`.
5. Observe that `location` changes but `metadata.absolute_path` remains `/home/kenton/Documents/keywest.health/./downloads`; `scripts/neurostrata-sync-gate.sh` reports it as stale.

## Why triage may help

The public correction tool can replace structural locations and Governs paths but does not expose a way to replace or clear a structural `absolute_path`. This leaves an active replacement pointing at the old missing location, so the consuming repository's sync gate remains red. Please assess whether superseding a structural memory should derive `absolute_path` from the replacement location or provide an explicit supported clear/replace field. The consumer should not hand-edit the generated export or create a placeholder directory to mask the stale metadata.

## Scope

No Tebra endpoint, patient data, credential, or clinical record was accessed. No NeuroStrata source code or database was edited directly; the memory correction used the supported MCP supersede operation.
