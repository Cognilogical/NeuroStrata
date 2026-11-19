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
- **Lock 1 (Pre-Push Gate & Task Coverage Gate)**: `.git/hooks/pre-push` runs `neurostrata-mcp task gate <namespace> --strict` and `scripts/task-coverage-gate.sh`. An `in_progress` task on the current branch/worktree (or an unattributed legacy claim), any `done` task without an extracted memory, or an unlinked deferral marker blocks the push unless paired with a task ID (e.g. `ns-p680`).
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

## 6. Staged Format/Lint (Pre-Commit Hook)

Formatting and lint enforcement is staged and non-destructive: the pre-commit hook runs `scripts/check-staged-quality.sh` against staged files only, and never rewrites, restages, or downloads dependencies on its own. If checks fail, the commit aborts before any ledger/graph write occurs.

### Setup

- Install root Node dependencies: `npm ci`
- Install web-UI Node dependencies: `npm --prefix web-ui ci`
- Install Rust components: `rustup component add rustfmt clippy`
- Install ShellCheck for `scripts/*.sh` validation (platform package manager or from https://www.shellcheck.net)
- Install the hook into `.git/hooks/pre-commit`: `bash scripts/install-hooks.sh`

### What the gate enforces

- Rust staged files: `rustfmt --edition <crate edition> --config skip_children=true --check`; Clippy with staged-file diagnostics filtered by `scripts/clippy-staged-diagnostics.mjs`.
- Shell staged files: `bash -n` and `shellcheck`.
- JS/TS staged files: Prettier via local `node_modules/.bin/prettier` (and web-ui ESLint via `web-ui/node_modules/.bin/eslint`).
- Full runner: `scripts/check-staged-quality.sh`; all checks use local, repo-locked tools. The hook is fail-closed: if a required tool is absent, the gate reports what is missing and exits nonzero.

### Repair workflow

There is no automated auto-fix or auto-restage in the hook path. Run the checks locally, fix the staged files, and `git add` the results yourself:

- `bash -n scripts/*.sh` and `shellcheck scripts/*.sh`
- `node scripts/clippy-staged-diagnostics.mjs --root "$PWD" --staged src/task/mod.rs`
- `npx prettier` is out of scope — use `./node_modules/.bin/prettier --check <paths>` and `./node_modules/.bin/eslint <paths>` instead
- `rustfmt --edition "$(sed -n 's/^edition *= *"\([0-9]*\)"/\1/p' Cargo.toml)" --config skip_children=true --check src/task/mod.rs`

### Incremental staged-file policy

The gate runs incrementally: only files that are staged in the commit are checked. Unchanged files, generated bundles, and `.NeuroStrata/` internal files are excluded to keep local commits fast and deterministic (CI runs the gate for changed shell files).

### No auto rewrite / no auto restage

Formatting is strictly manual. Neither the checked-in hook nor any `npm` script rewrites staged files or modifies the index. If `shellcheck`, `rustfmt`, or `prettier` finds issues, fix them in the working tree, then `git add` again before committing.

### Fail-closed, no auto-install

Required tools must be installed before the hook runs. The runner does not install tools or restage files. Clippy may populate `target/` and the Cargo registry cache; a cold cache can fetch registry crates under `--locked`. Missing tools produce a clear diagnostic and a nonzero exit. Because the hook delegates to the project's `scripts/check-staged-quality.sh` when present, consumer projects can keep their own gates without modifying this repository.

### Verification

- Hook sources byte-identity: `TMPDIR=/var/tmp/ns-tmp cargo test --bin neurostrata-mcp pre_commit_hook_sources_stay_byte_identical -- --nocapture`
- Propagation fixtures: `bash scripts/test-staged-quality.sh`
- Installed hook read-back: `cmp scripts/hooks/pre-commit .git/hooks/pre-commit`
- Full suite: `TMPDIR=/var/tmp/ns-tmp cargo test --locked -- --test-threads=1`
- Gate: `TMPDIR=/var/tmp/ns-tmp bash scripts/task-coverage-gate.sh`

[Contributing Guide (\`CONTRIBUTING.md\`)](CONTRIBUTING.md) · [AGENTS.md](AGENTS.md)
