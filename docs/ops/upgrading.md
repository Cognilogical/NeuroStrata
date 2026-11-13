# Upgrading NeuroStrata — Lifecycle & Schema Migration

Schema and config defaults are upgraded automatically. **Upgrading from ≤1.17.x is not zero-touch for the index:** parser logic version 3 forces one full reparse/re-embed so pre-fix rows pass through the secret scanner and the type-change logic.

## Upgrade from ≤1.17.x (one-time reparse)

- **Disk:** the old generation stays active while the new one stages, so plan for about **2× the store size** in free space. The ingest preflight refuses to start when the filesystem cannot hold the projection.
- **Time:** one full walk of every ingested directory, plus embedding and relink. Large stores take proportionally longer.
- **Pool:** the default buffer-pool cap is 384 MB (`storage_hygiene.buffer_pool_mb`); a reparse stages roughly twice the rows. Raise it or set `NEUROSTRATA_BUFFER_POOL_MB=0` to let the engine size it if a large store hits a pool-full error.
- **Rollback:** keep a store backup before the upgrade. The previous generation remains readable until the new one publishes; a failed reparse aborts staging and leaves the old generation active.
- **Secrets:** rows indexed before the fix are re-scanned. A file that trips the scanner is skipped and reported; check `files_skipped` after the first post-upgrade ingest.

After this one-time reparse, later upgrades return to the automatic path below.

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