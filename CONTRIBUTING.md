# Contributing to NeuroStrata

NeuroStrata is a Spec-Driven Development (SDD) platform that enables both human and AI agents to build systems rigorously. Contributions must follow the established workflows, conventions, and safety gates.

## 1. Spec-Driven Development with Superpowers (`docs/superpowers/`)

NeuroStrata uses **Superpowers** for Spec-Driven Development (SDD):

1. **`brainstorming`**: Classify scope (`Spike`, `Bounded`, or `Architectural`), explore existing context, and align on design before writing code.
2. **Design & Contract Specs (`docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md` or `-contract.md`)**: Document contracts (`C1`, `C2`, ...), invariants, and test matrices in `docs/superpowers/specs/`.
3. **Implementation Plans (`docs/superpowers/plans/YYYY-MM-DD-<topic>.md`)**: Break approved specs into bite-sized TDD tasks via `writing-plans`.
4. **Isolated Worktrees & Subagent Execution (`using-git-worktrees`, `subagent-driven-development`)**: Execute each plan in an isolated git worktree with fresh implementer/reviewer subagents.
5. **Permanent Specs vs. Ephemeral Scratch**: `docs/superpowers/specs/` and `docs/superpowers/plans/` are permanent, version-controlled engineering records. By contrast, `.superpowers/` (root dot-directory) is ephemeral local subagent scratch space, ignored by `.gitignore`, and must never be force-added (`git add -f`).

## 2. Dogfooding NeuroStrata & The Three Resilient Soft Locks

Contributors (human and AI agents) dogfood NeuroStrata on itself (see [`AGENTS.md`](AGENTS.md)):

- **Zero-Action Start & Pre-Flight Snapshot**: Run `neurostrata_get_snapshot` (or `neurostrata_search_memory`) first, then claim or create a tracked task (`neurostrata_task_create` / `neurostrata_task_claim`) before editing files.
- **Graph-First Exploration**: Query `neurostrata_memories_for_file`, `neurostrata_related`, or `neurostrata_sparql_query` before blind `grep`/`glob` or editing a file. Propagate `subagent_prompt_injection` into every spawned subagent prompt.
- **Lock 3 (Breath Checkpoints)**: Every 3–5 steps, record a state checkpoint on the active task via `neurostrata_task_update`.
- **Lock 2 (Mandatory Memory Extraction)**: Completing a task (`neurostrata_task_complete`) requires extracting at least one architectural rule or fact (`neurostrata_add_memory` or inline `memory.content`).
- **Lock 1 (Pre-Push Gate & Task Coverage Gate)**: `.git/hooks/pre-push` runs `neurostrata-mcp task gate <namespace> --strict` and `scripts/task-coverage-gate.sh`. Any `in_progress` task, `done` task without an extracted memory, or unlinked deferral marker blocks the push unless paired with a task ID (e.g. `ns-p680`).
- **The Prime Directive (No Unfiled Deferrals)**: Never leave silent stubs or untracked deferrals; every piece of unfinished work must either be completed in-branch or filed as a tracked task with an explicit ID (e.g. `ns-p680`).

## 3. Test-Driven Development (TDD) & Verification Gates

Follow strict **Red → Green → Refactor** (`test-driven-development` and `verification-before-completion` skills).

- **Rust Binary Crate (`neurostrata-mcp`)**: Build with `scripts/build.sh` (CMake 3.15+ and C++20 `<format>` requirements). Run tests with `cargo test --bin neurostrata-mcp <filter>`.
- **Embedded Harness Plugins (`plugins/*`)**: `src/task/harness_plugins.rs` embeds `plugins/*/dist/index.js` via `include_str!`. Rebuild `dist/index.js` bundles (`npm run build`) and run both `node --test plugins/core/test.mjs` and the affected adapter test suites (`node --test plugins/<adapter>/test.mjs`, e.g. `plugins/{opencode,claude,codex,github-cli}/test.mjs`) when adapters change.
- **Lean 4 Formal Verification (`proofs/` & `src/formal_kernel.rs`)**: `src/formal_kernel.rs` is formally verified in Lean 4 (`proofs/NeuroStrata/*.lean`) with zero `sorry` and zero custom `axiom` declarations, enforced by `tests/lean_parity_test.rs` and `scripts/task-coverage-gate.sh`. Any edit to `src/formal_kernel.rs` must update `proofs/` and `tests/lean_parity_test.rs` in the same commit.
- **Shell Backup & Hook Scripts (`scripts/`)**: Run `bash scripts/test-backup.sh` when modifying `scripts/backup.sh` and `bash scripts/task-coverage-gate.sh` before pushing.

## 4. Infrastructure, Security & Code Conventions

- **Containers**: Always use `podman` and `podman-compose` (rootless, `--network=none` for sandbox validation). Never use `docker`.
- **Data Formats**: Prefer strict JSON and JSON Schema over YAML or TOML for configuration and wire payloads.
- **Database & Daemon Safety**: LadybugDB is a shared local store; never run destructive drop/delete operations against the live database directory, and never let tests clobber a live `~/.config/neurostrata/daemon.token` (tests must either use an isolated temp config/token path or serialize access with a mutex guard that snapshots and restores any pre-existing `daemon.token`).
- **Pull Request Workflow**: Keep PRs focused, link the governing issue (`Closes #...`), include contract specs/plans under `docs/superpowers/` when applicable, and ensure CI (`ci.yml`) and pre-push gates pass cleanly.

## 5. Contributing Guide

- Read `AGENTS.md` for agent-specific workflows.
- Follow the `writing-plans` skill for task decomposition.
- Use `using-git-worktrees` for isolation.
- Run `bash scripts/task-coverage-gate.sh` before pushing.
- File tracked tasks with `neurostrata_task_create` for deferred work.

[Contributing Guide (\`CONTRIBUTING.md\`)](CONTRIBUTING.md) · [AGENTS.md](AGENTS.md)