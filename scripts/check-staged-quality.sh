#!/usr/bin/env bash
# Staged quality runner: checks staged files for formatting and lint compliance.
# Called by the pre-commit hook. Exits 0 only when all relevant checks pass.
# Validates committed content in a temporary full-index checkout, so partially
# staged files are checked exactly as committed. Never writes project files or
# the Git index.
set -euo pipefail

if ! ROOT="$(git rev-parse --show-toplevel 2>/dev/null)"; then
    echo "error: not inside a git repository" >&2
    exit 1
fi
cd "$ROOT"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLIPPY_FILTER="$SCRIPT_DIR/clippy-staged-diagnostics.mjs"

classify_shebang() {
    local line="${1%$'\r'}"
    if [[ "$line" =~ ^#!/bin/bash([[:space:]]|$) ]]; then
        printf 'bash'
        return 0
    fi
    if [[ "$line" =~ ^#!/usr/bin/env[[:space:]]+(-S[[:space:]]+)?bash([[:space:]]|$) ]]; then
        printf 'bash'
        return 0
    fi
    if [[ "$line" =~ ^#!/bin/sh([[:space:]]|$) ]]; then
        printf 'sh'
        return 0
    fi
    if [[ "$line" =~ ^#!/usr/bin/env[[:space:]]+(-S[[:space:]]+)?sh([[:space:]]|$) ]]; then
        printf 'sh'
        return 0
    fi
    printf ''
}

# ── Staged paths (NUL-delimited; exclude .NeuroStrata/, deleted, unstaged-only) ─
STAGED_LIST="$(mktemp "${TMPDIR:-/tmp}/staged-quality-list.XXXXXX")"
# shellcheck disable=SC2154  # rc is assigned by the trap trigger ($?)
trap 'rc=$?; rm -f "$STAGED_LIST"; exit "$rc"' EXIT
if ! git diff --cached --name-only --diff-filter=ACMRT -z >"$STAGED_LIST" 2>"$STAGED_LIST.err"; then
    echo "error: could not enumerate staged files" >&2
    cat "$STAGED_LIST.err" >&2 || true
    rm -f "$STAGED_LIST"
    exit 1
fi

StagedFiles=()
while IFS= read -r -d '' file; do
    case "$file" in
        .NeuroStrata/*|"") continue ;;
    esac
    StagedFiles+=("$file")
done < "$STAGED_LIST"

CHECK_ROOT="$ROOT"
if [ ${#StagedFiles[@]} -gt 0 ]; then
    tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/staged-quality.XXXXXX")
    # shellcheck disable=SC2154  # rc is assigned inside the trap itself
    trap 'rc=$?; trap - EXIT; rm -f "$STAGED_LIST"; rm -rf "$tmpdir"; exit "$rc"' EXIT
    CHECK_ROOT="$tmpdir/tree"
    mkdir -p "$CHECK_ROOT"
    if ! git checkout-index --all --prefix="$CHECK_ROOT/"; then
        echo "error: could not materialize staged content" >&2
        exit 1
    fi
fi

# ── Classify files by type ──────────────────────────────────────────────────
RustFiles=()
ShellFiles=()
BashFiles=()
JsTsFiles=()
JsTsStaged=()
OtherFiles=()
ClippyStaged=()

for file in ${StagedFiles[@]+"${StagedFiles[@]}"}; do
    case "$file" in
        *.rs) RustFiles+=("$file"); ClippyStaged+=("$file") ;;
        *.sh) ShellFiles+=("$file"); BashFiles+=("$file") ;;
        *.js|*.jsx|*.ts|*.tsx|*.mjs|*.cjs) JsTsFiles+=("$file") ;;
        *.json|*.css|*.scss|*.html|*.md|*.yml|*.yaml) OtherFiles+=("$file") ;;
        */Cargo.toml|Cargo.toml) ClippyStaged+=("$file") ;;
        *)
            if [ -f "$CHECK_ROOT/$file" ] && [ ! -L "$CHECK_ROOT/$file" ]; then
                IFS= read -r first_line < "$CHECK_ROOT/$file" || true
                shebang_kind="$(classify_shebang "$first_line")"
                case "$shebang_kind" in
                    bash) ShellFiles+=("$file"); BashFiles+=("$file") ;;
                    sh) ShellFiles+=("$file") ;;
                esac
            fi
            ;;
    esac
done

# Web UI TS/TSX files for ESLint
for file in ${StagedFiles[@]+"${StagedFiles[@]}"}; do
    case "$file" in
        web-ui/*.ts|web-ui/*.tsx) JsTsStaged+=("$file") ;;
    esac
done

# ── Tool resolution: local paths only ───────────────────────────────────────
resolve_tool() {
    local name="$1" candidate resolved; shift
    for candidate in "$@"; do
        case "$candidate" in
            /*) resolved="$candidate" ;;
            *) resolved="$ROOT/$candidate" ;;
        esac
        if [ -x "$resolved" ]; then
            printf '%s' "$resolved"
            return 0
        fi
    done
    if resolved=$(command -v "$name" 2>/dev/null); then
        case "$resolved" in
            /*) printf '%s' "$resolved" ;;
            *) printf '%s/%s' "$ROOT" "$resolved" ;;
        esac
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
# Read the crate edition from the staged snapshot (CHECK_ROOT), not the working
# tree, so rustfmt matches the staged content it actually checks.
EDITION=""
if [ -f "$CHECK_ROOT/Cargo.toml" ]; then
    EDITION="$(sed -n 's/^edition[[:space:]]*=[[:space:]]*"\([0-9][0-9]*\)".*/\1/p' "$CHECK_ROOT/Cargo.toml" 2>/dev/null | head -n 1 || true)"
fi
if [ -z "$EDITION" ]; then
    EDITION="2021"
fi

if [ ${#RustFiles[@]} -gt 0 ]; then
    RUSTFMT=$(resolve_tool rustfmt node_modules/.bin/rustfmt || true)
    if [ -z "$RUSTFMT" ]; then
        missing_tool rustfmt "rustup component add rustfmt"
    fi
    (cd "$CHECK_ROOT" && "$RUSTFMT" --edition "$EDITION" --config skip_children=true --check "${RustFiles[@]}") || {
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

    clippy_stderr="$tmpdir/clippy-stderr.log"
    filter_stderr="$tmpdir/filter-stderr.log"
    clippy_target_dir="$tmpdir/clippy-target"
    mkdir -p "$clippy_target_dir"

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
    (cd "$CHECK_ROOT" && CARGO_TARGET_DIR="$clippy_target_dir" "$CARGO" clippy --workspace --all-targets --locked --message-format=json) 2>"$clippy_stderr" \
        | "$NODE" "$CLIPPY_FILTER" --root "$CHECK_ROOT" --dep-info-root "$clippy_target_dir" --staged "${ClippyStaged[@]}" 2>"$filter_stderr"
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
        # exit 1 = staged diagnostics or Rust files absent from Cargo dep-info.
        cat "$filter_stderr" >&2 || true
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
    if [ ! -x "$ROOT/node_modules/.bin/prettier" ]; then
        missing_tool prettier "npm ci"
    fi
    (cd "$CHECK_ROOT" && "$ROOT/node_modules/.bin/prettier" --check "${PRETTIER_FILES[@]}") || {
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
    if [ ! -x "$ROOT/web-ui/node_modules/.bin/eslint" ]; then
        missing_tool eslint "npm --prefix web-ui ci"
    fi
    if [ -d "$ROOT/web-ui/node_modules" ] && [ ! -e "$CHECK_ROOT/web-ui/node_modules" ]; then
        ln -s "$ROOT/web-ui/node_modules" "$CHECK_ROOT/web-ui/node_modules"
    fi
    ( cd "$CHECK_ROOT/web-ui" && "$ROOT/web-ui/node_modules/.bin/eslint" "${WEB_UI_FILES[@]}" ) || {
        echo "error: eslint check failed" >&2
        exit 1
    }
fi

# ── Check 5: Shell scripts ──────────────────────────────────────────────────
if [ ${#ShellFiles[@]} -gt 0 ]; then
    for file in ${BashFiles[@]+"${BashFiles[@]}"}; do
        bash -n "$CHECK_ROOT/$file" || {
            echo "error: bash syntax check failed for $file" >&2
            exit 1
        }
    done
    SHELLCHECK=$(resolve_tool shellcheck "$ROOT/node_modules/.bin/shellcheck" || true)
    if [ -z "$SHELLCHECK" ]; then
        missing_tool shellcheck "shellcheck --help for installation instructions"
    fi
    (cd "$CHECK_ROOT" && "$SHELLCHECK" "${ShellFiles[@]}") || {
        echo "error: shellcheck failed" >&2
        exit 1
    }
fi

exit 0
