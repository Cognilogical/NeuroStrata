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

## How automatic upgrades work

## Store-open migration for legacy rows

Every open of an existing store runs one migration inside the schema turn. It
never blocks the open on its own account, and it never deletes a row or an
edge.

This port is **independent of #68**: it does not depend on, sequence with, or
block that work. It only normalizes already-present on-disk stores so the
current binary can open them; no #68 schema or lifecycle change is assumed.

- **Primary key.** When `CALL table_info('Memory')` does not report `id` as the
  sole primary key, the open rebuilds the table keyed by `id` into scratch
  tables, copies every row and relationship, validates the counts, and only
  then swaps the names. The migration reports `pk_rebuilt = true`; the original
  tables are renamed aside until the replacement passes validation, and an
  interrupted swap is resumed or rolled back on the next open before a fresh
  `Memory` table can be created. Relationship tables keep the endpoint kinds
  they were declared with: a table that does not touch `Memory` is left
  completely untouched, and a table mixing `Memory` with other endpoints is
  recreated with every connection, the `Memory` side rebound to the rebuilt
  row and the other side still bound by its own primary key. A rebuild that
  cannot copy every row or edge, or cannot represent an endpoint, fails loudly
  and leaves the source tables untouched.
- **NULL namespaces.** `metadata.namespace` is checked first, then the trusted
  structural owner: a generation-scoped id (`namespace::generation::path`) or
  the single qualified row that shares its logical path in exactly one
  namespace. When both exist and disagree, or when only an unverified metadata
  claim exists against contradicting structural evidence, the row moves to
  `_quarantine` instead of being exposed under a tenant that never owned it.
  Imported rows with foreign mesh provenance or private eligibility are never
  rewritten or quarantined. They participate in recovery only when trusted
  structural evidence corroborates the metadata-named owner; an unproven or
  conflicting claim leaves the protected row untouched. A row with no
  recoverable owner moves to `_quarantine` with `metadata.quarantine` recorded;
  it is still there, just out of ordinary reads, list, search and export.
- **Legacy ids.** Rows an older binary wrote as bare ids are recorded in
  `LegacyIdAlias (id, namespace, canonical_id)`. A later re-ingest of the
  qualified id resolves through that alias and updates the existing row in
  place, so edges stay attached and no duplicate row appears.
- **Recovery opens.** An unverified recovery open (checksum-config mismatch,
  unverified WAL replay, or a discarded unreadable WAL) skips the migration
  entirely and reports `skipped_reason = "unverified_open"`; a later verified
  open performs it.

The report is available from the store's last open (`MigrationReport`:
`run`, `pk_rebuilt`, `null_namespace_recovered`, `quarantined`,
`aliases_recorded`, `skipped_reason`). Re-running an open over an already
migrated store reports zero changes, so the migration is idempotent.

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