# Staged Formatting and Lint Enforcement Design

## Outcome

Every commit with staged Rust, JavaScript/TypeScript, JSON/CSS/Markdown, or
shell changes must pass deterministic formatting and lint checks. Checks never
rewrite source files or alter the Git index. Existing files become compliant
only when touched; no repository-wide formatting migration is required.

## Constraints

- Deliver Kani integration and staged formatting/lint enforcement together in one new PR on the existing branch; do not open a separate Kani PR.
- Operate on `git diff --cached --name-only --diff-filter=ACMR -z` only.
- Preserve filenames containing spaces, tabs, quotes, and non-ASCII characters.
- Ignore deleted files and `.NeuroStrata/` internal state.
- Do not download tools during a commit. A missing tool required by staged
  files blocks the commit and prints its local setup command.
- Do not stage, restage, stash, or overwrite working-tree files.
- Generated bundles under `dist/` are excluded from Prettier checks.
- Keep `SCAFFOLDED_PRE_COMMIT_HOOK` byte-identical to
  `scripts/hooks/pre-commit`; the generic hook delegates only when the project
  provides the staged-quality script.

## Components

### `scripts/check-staged-quality.sh`

Classifies staged files into arrays and invokes only relevant checks:

| Staged input                                                                                                         | Check                                                                                                                                                                             |
| -------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `*.rs`                                                                                                               | `rustfmt --edition 2021 --config skip_children=true --check` on those files                                                                                                       |
| Rust source or Cargo manifests                                                                                       | `cargo clippy --workspace --all-targets --locked --message-format=json` once; reject warning/error diagnostics whose primary span belongs to a staged Rust or Cargo manifest file |
| `*.js`, `*.jsx`, `*.ts`, `*.tsx`, `*.mjs`, `*.cjs`, `*.json`, `*.css`, `*.scss`, `*.html`, `*.md`, `*.yml`, `*.yaml` | repository-local pinned Prettier `--check`                                                                                                                                        |
| Web UI TS/TSX                                                                                                        | existing local ESLint on exactly those staged paths                                                                                                                               |
| `*.sh`                                                                                                               | `bash -n` per file and `shellcheck` across the staged set                                                                                                                         |

Prettier resolves only to `node_modules/.bin/prettier`; ESLint resolves only to
`web-ui/node_modules/.bin/eslint`. No `npx` or implicit package installation is
permitted. Any failed check exits nonzero immediately with the tool diagnostic.

The Clippy JSON stream is parsed by `scripts/clippy-staged-diagnostics.mjs`.
Cargo compilation failures remain fatal. Otherwise, warnings outside staged
files are baseline noise and do not block, while every warning/error primary
span in a staged Rust source or Cargo manifest prints its rendered diagnostic
and fails the check. This avoids rewriting the existing 548-warning main-crate
baseline while making every touched Rust file strictly clean.

### Local formatting package

Root `package.json` and `package-lock.json` pin Prettier. A minimal Prettier
configuration establishes one style and ignores dependencies, build output,
and internal state. Root `node_modules/` is ignored by Git.

### Hook installation

The checked-in hook delegates to the quality script after collecting staged
paths and before its ledger logic. The Rust embedded hook receives identical
bytes. `scripts/install-hooks.sh` already installs the checked-in hook, so no
new installation path is needed; installation refreshes `.git/hooks/pre-commit`.

## Verification

- Shell integration tests create isolated temporary Git repositories with fake
  tool executables and verify routing, exact argument boundaries, filtering,
  missing-tool failure, tool failure propagation, and unchanged content/index.
- Node tests prove Clippy path matching, warning/error filtering, absolute and
  relative paths, non-diagnostic JSON, malformed input, and cargo failure status.
- A Rust unit test compares embedded and checked-in hook bytes.
- Staged Clippy diagnostics fail while unrelated baseline warnings remain visible.
- Existing pre-commit path-normalization tests remain green.
- `bash -n`, ShellCheck, Prettier, Clippy, hook tests, coverage gates, and the
  full Rust suite provide final evidence.

## Scope Limits

Python and Lean keep their existing test/build gates; this change adds no new
formatter or linter dependency for them. There is no automatic rewrite mode
and no emergency project-specific bypass. Git's standard `--no-verify` remains
an explicit user action outside this implementation.
