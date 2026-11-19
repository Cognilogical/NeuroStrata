#!/usr/bin/env bash
# Staged quality runner: checks staged files for formatting and lint compliance.
# Called by the pre-commit hook. Exits 0 only when all relevant checks pass.
# Validates committed content: refuses to run when a staged blob differs from its
# working-tree file, because every check reads the working tree. This keeps a
# stale worktree from masking a bad staged blob. Never writes project files or
# the Git index.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLIPPY_FILTER="$SCRIPT_DIR/clippy-staged-diagnostics.mjs"

# ── Staged paths (NUL-delimited; exclude .NeuroStrata/, deleted, unstaged-only) ─
StagedFiles=()
while IFS= read -r -d '' file; do
    case "$file" in
        .NeuroStrata/*|"") continue ;;
    esac
    StagedFiles+=("$file")
done < <(git diff --cached --name-only --diff-filter=ACMR -z 2>/dev/null)

# ── Classify files by type ──────────────────────────────────────────────────
RustFiles=()
ShellFiles=()
JsTsFiles=()
JsTsStaged=()
OtherFiles=()
ClippyStaged=()

for file in ${StagedFiles[@]+"${StagedFiles[@]}"}; do
    case "$file" in
        *.rs) RustFiles+=("$file"); ClippyStaged+=("$file") ;;
        *.sh) ShellFiles+=("$file") ;;
        *.js|*.jsx|*.ts|*.tsx|*.mjs|*.cjs) JsTsFiles+=("$file") ;;
        *.json|*.css|*.scss|*.html|*.md|*.yml|*.yaml) OtherFiles+=("$file") ;;
        */Cargo.toml|Cargo.toml) ClippyStaged+=("$file") ;;
        *) ;;
    esac
done

# Web UI TS/TSX files for ESLint
for file in ${StagedFiles[@]+"${StagedFiles[@]}"}; do
    case "$file" in
        web-ui/*.ts|web-ui/*.tsx) JsTsStaged+=("$file") ;;
    esac
done

# ── Index/worktree consistency ──────────────────────────────────────────────
# Checks below run against working-tree paths. If a staged blob differs from
# the worktree, the tools would validate content the commit does not contain.
# Fail closed and ask for the content to be staged.
if [ ${#StagedFiles[@]} -gt 0 ]; then
    if ! git diff --quiet -- "${StagedFiles[@]}" 2>/dev/null; then
        echo "error: staged content differs from the working tree:" >&2
        git diff --name-only -- "${StagedFiles[@]}" >&2 || true
        echo "  stage the current content (git add <path>) or restore the worktree, then retry." >&2
        exit 1
    fi
fi

# ── Tool resolution: local paths only ───────────────────────────────────────
resolve_tool() {
    local name="$1"; shift
    for candidate in "$@"; do
        if [ -x "$candidate" ]; then
            printf '%s' "$candidate"
            return 0
        fi
    done
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return 0
    fi
    printf ''
    return 1
}

missing_tool() {
    local name="$1"
    local setup_hint="$2"
    echo "error: required tool '$name' not found — install it locally:" >&2
    echo "  $setup_hint" >&2
    exit 1
}

# ── Check 1: Rust formatting ────────────────────────────────────────────────
if [ ${#RustFiles[@]} -gt 0 ]; then
    RUSTFMT=$(resolve_tool rustfmt node_modules/.bin/rustfmt || true)
    if [ -z "$RUSTFMT" ]; then
        missing_tool rustfmt "rustup component add rustfmt"
    fi
    "$RUSTFMT" --edition 2021 --config skip_children=true --check "${RustFiles[@]}" || {
        echo "error: rustfmt check failed" >&2
        exit 1
    }
fi

# ── Check 2: Rust/Cargo Clippy ──────────────────────────────────────────────
if [ ${#ClippyStaged[@]} -gt 0 ]; then
    CARGO=$(resolve_tool cargo node_modules/.bin/cargo || true)
    if [ -z "$CARGO" ]; then
        missing_tool cargo "install the Rust toolchain via https://rustup.rs"
    fi

    # collision-safe temp files, cleaned on any exit; preserve the real status
    tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/staged-quality.XXXXXX")
    # shellcheck disable=SC2154  # rc is assigned inside the trap itself
    trap 'rc=$?; trap - EXIT; rm -rf "$tmpdir"; exit "$rc"' EXIT

    clippy_stderr="$tmpdir/clippy-stderr.log"
    filter_stderr="$tmpdir/filter-stderr.log"

    # node must be available for the filter step — this produces a setup
    # diagnostic before cargo+node pipeline runs.
    NODE=$(resolve_tool node node_modules/.bin/node || true)
    if [ -z "$NODE" ]; then
        missing_tool node "install Node.js (https://nodejs.org)"
    fi

    set +e
    # Pipeline: cargo writes JSON stdout, stderr goes to log;
    # node filter receives the JSON stream on stdin and outputs matched
    # diagnostics to stdout.  Any nonzero cargo exit or node exit is fatal
    # (including cargo exit 1 with no diagnostics — it's captured via PIPESTATUS).
    "$CARGO" clippy --workspace --all-targets --locked --message-format=json 2>"$clippy_stderr" \
        | "$NODE" "$CLIPPY_FILTER" --root "$ROOT" --staged "${ClippyStaged[@]}" 2>"$filter_stderr"
    statuses=("${PIPESTATUS[@]}")
    set -e
    cargo_status=${statuses[0]:-0}
    node_status=${statuses[1]:-0}

    # ANY nonzero cargo exit is fatal, including 1 with no diagnostics output
    if [ "$cargo_status" -ne 0 ]; then
        echo "error: cargo clippy failed (exit $cargo_status)" >&2
        cat "$clippy_stderr" >&2 || true
        exit 1
    fi

    # Node exited — any nonzero status is fatal.
    if [ "$node_status" -eq 2 ]; then
        echo "error: malformed Clippy NDJSON input" >&2
        cat "$filter_stderr" >&2 || true
        exit 1
    elif [ "$node_status" -ne 0 ]; then
        # exit 1 = staged warning/error found; rendered diagnostics went to stdout.
        exit 1
    fi
fi

# ── Check 3: JavaScript/TypeScript formatting (Prettier) ─────────────────────
PRETTIER_FILES=()
for file in ${JsTsFiles[@]+"${JsTsFiles[@]}"} ${OtherFiles[@]+"${OtherFiles[@]}"}; do
    case "$file" in
        dist/*|*/dist/*) continue ;;
        *) PRETTIER_FILES+=("$file") ;;
    esac
done

if [ ${#PRETTIER_FILES[@]} -gt 0 ]; then
    if [ ! -x node_modules/.bin/prettier ]; then
        missing_tool prettier "npm ci"
    fi
    node_modules/.bin/prettier --check "${PRETTIER_FILES[@]}" || {
        echo "error: prettier check failed" >&2
        exit 1
    }
fi

# ── Check 4: ESLint for Web UI TypeScript ───────────────────────────────────
# Convert staged web-ui paths to web-ui-relative arguments and run from
# $ROOT/web-ui so ESLint flat config discovery resolves real project config.
WEB_UI_FILES=()
for file in ${JsTsStaged[@]+"${JsTsStaged[@]}"}; do
    WEB_UI_FILES+=("${file#web-ui/}")
done

if [ ${#WEB_UI_FILES[@]} -gt 0 ]; then
    if [ ! -x web-ui/node_modules/.bin/eslint ]; then
        missing_tool eslint "npm --prefix web-ui ci"
    fi
    ( cd "$ROOT/web-ui" && ./node_modules/.bin/eslint "${WEB_UI_FILES[@]}" ) || {
        echo "error: eslint check failed" >&2
        exit 1
    }
fi

# ── Check 5: Shell scripts ──────────────────────────────────────────────────
if [ ${#ShellFiles[@]} -gt 0 ]; then
    for file in "${ShellFiles[@]}"; do
        bash -n "$file" || {
            echo "error: bash syntax check failed for $file" >&2
            exit 1
        }
    done
    SHELLCHECK=$(resolve_tool shellcheck node_modules/.bin/shellcheck || true)
    if [ -z "$SHELLCHECK" ]; then
        missing_tool shellcheck "shellcheck --help for installation instructions"
    fi
    "$SHELLCHECK" "${ShellFiles[@]}" || {
        echo "error: shellcheck failed" >&2
        exit 1
    }
fi

exit 0
