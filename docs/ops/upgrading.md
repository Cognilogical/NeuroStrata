# Upgrading NeuroStrata — Zero-Touch Lifecycle & Schema Migration

Upgrading NeuroStrata requires **zero manual database migrations or config edits**. The binary and your agent handle the entire upgrade automatically.

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