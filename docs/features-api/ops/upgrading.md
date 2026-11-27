# Upgrading NeuroStrata — Lifecycle & Schema Migration

Schema and config defaults are upgraded automatically. **Upgrading from ≤1.17.x is not zero-touch for the index:** parser logic version 3 forces one full reparse/re-embed so pre-fix rows pass through the secret scanner and the type-change logic.

## Upgrade from ≤1.17.x (one-time reparse)

- **Disk:** the old generation stays active while the new one stages, so plan for about **2× the store size** in free space. The ingest preflight refuses to start when the filesystem cannot hold the projection.
- **Time:** one full walk of every ingested directory, plus embedding and relink. Large stores take proportionally longer.
- **Pool:** the default buffer-pool cap is 384 MB (`storage_hygiene.buffer_pool_mb`); a reparse stages roughly twice the rows. Raise it or set `NEUROSTRATA_BUFFER_POOL_MB=0` to let the engine size it if a large store hits a pool-full error.
- **Rollback:** keep a store backup before the upgrade. The previous generation remains readable until the new one publishes; a failed reparse aborts staging and leaves the old generation active.
- **Secrets:** rows indexed before the fix are re-scanned. A file that trips the scanner is skipped and reported; check `files_skipped` after the first post-upgrade ingest.

After this one-time reparse, later upgrades return to the automatic path below.

## Set the project root once per clone

After upgrading to a build with the project marker, run `neurostrata-mcp hooks install` once in each git clone (non-git projects: `neurostrata_task_setup`). It writes `.NeuroStrata/config.json`, or asks which root or namespace you mean. See [Project root](project-root.md).

## Persisted embedding identity and the open-time audit

Since the persisted-marker upgrade, the store records which embedder produced its
vectors inside the database itself, in the single-row `StoreIdentity` table
(`id = 'embedding-identity'`, `marker_version = 1`). The marker carries the same
seven descriptor fields the store-state sidecar records, plus when it was
recorded and — when it was inferred rather than observed — the identity that was
adopted and when.

- **First vector writes the marker.** The marker is written inside the first
  upsert's writer turn, before the vector MERGE. If the marker write fails, the
  upsert is refused and no vector lands.
- **Mismatches refuse vector work, never the open.** Searching or writing under
  an embedder that disagrees with the persisted marker is refused with an
  embedding-identity error. The same refusal applies when the marker and the
  store-state sidecar both name an identity and disagree. Opening the store is
  never blocked: an operator can still inspect it, repair the record, or start a
  new store.
- **Legacy adoption is recorded, not assumed.** On a verified open of a
  data-bearing store that has no marker yet, the store adopts the identity from
  the parseable store-state sidecar and records `adopted_from_provider`,
  `adopted_from_model`, `adopted_from_dims`, and `adopted_at`. With no parseable
  sidecar the provenance columns stay `NULL`, which says the identity was
  inferred rather than declared. An adoption failure is logged and retried on the
  next open.
- **The marker survives losing the sidecar.** A store copied without its
  `ladybug.db.store-state.json` sidecar still knows which embedding space built
  its vectors, so a copy opened under a different model refuses vector work
  instead of silently mixing embedding spaces.
- **Row-identity audit at open.** The first `init` in a process pages through
  `Memory` (500-row pages, two-second budget) and reports rows whose id
  contradicts the namespace they claim: a generation-scoped id under another
  namespace, an id qualified by another namespace, or an unqualified ingested
  id. The audit logs a capped sample and never blocks or repairs the open.

## How automatic upgrades work

## How Zero-Touch Upgrades Work

1. **Replace or rebuild the binary:**
   ```bash
   scripts/build.sh
   systemctl --user restart neurostrata.service
   ```
2. **Start your agent session as usual:**
   On the very first `neurostrata_get_snapshot` call after an upgrade, NeuroStrata automatically:
   - **Backfills Database Schema & Metadata Defaults:** Any new tables, columns, or task/procedure metadata fields are backfilled in-place on startup (`src/store/ladybug.rs`).
   - **Self-Heals Git Hooks:** `hook_hygiene` checks `.git/hooks/pre-commit` and `.git/hooks/pre-push` against the new binary version, backs up stale NeuroStrata hooks to `.bak`, and writes the current scripts automatically.
   - **Refreshes Embedded Harness Plugins:** Extracts the updated zero-dependency plugin bundles into `.NeuroStrata/plugins/<harness>/dist/index.js` and verifies your harness configuration (`opencode.json`, `.claude/settings.local.json`, `.hermes/`, `.codex/`, `.openclaw/`, Copilot CLI).
   - **Applies Lifecycle Wire Deltas:** `compute_lifecycle_delta` compares the project's registered `doctor_state` against the new version's `AUTOMATIC_WIRES`, `REMINDER_WIRES`, and `VALID_FIRES_ON` triggers. If new wires exist, the snapshot instructs the agent to run `neurostrata_task_setup` to register the delta automatically.