#!/usr/bin/env bash
# Integration tests for scripts/check-staged-quality.sh
# Verifies staged routing, argument boundaries, filtering, missing-tool diagnostics,
# failure propagation, and unchanged file/index hashes.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_PATH="$REPO_ROOT/scripts/check-staged-quality.sh"
HOOK_SCRIPT_PATH="$REPO_ROOT/scripts/hooks/pre-commit"

# Create fake tool wrappers that log invocations to FAKE_TOOL_LOG
setup_fake_tools() {
    local bindir="$1"
    mkdir -p "$bindir"

    cat > "$bindir/rustfmt" <<'RUSTFMT_EOF'
#!/usr/bin/env bash
echo "rustfmt $*" >> "$FAKE_TOOL_LOG"
for arg in "$@"; do
    case "$arg" in
        --edition|2021|--config|skip_children=true|--check) ;;
        *) echo "  file: $arg" >> "$FAKE_TOOL_LOG" ;;
    esac
done
exit 0
RUSTFMT_EOF
    chmod +x "$bindir/rustfmt"

    cat > "$bindir/cargo" <<'CARGO_EOF'
#!/usr/bin/env bash
echo "cargo $*" >> "$FAKE_TOOL_LOG"
if [[ "$*" == *"clippy"* ]] && [[ "$*" == *"--message-format=json"* ]]; then
    mkdir -p "$CARGO_TARGET_DIR/debug/deps"
    {
        printf 'fake-target:'
        while IFS= read -r -d '' file; do
            file=${file#./}
            escaped=${file// /\\ }
            printf ' %s/%s' "$PWD" "$escaped"
        done < <(find . -type f -name '*.rs' -print0)
        printf '\n'
    } > "$CARGO_TARGET_DIR/debug/deps/fake.d"
    echo '{"reason":"compiler-message","message":{"spans":[{"file_name":"src/unstaged.rs","is_primary":true}],"message":"warning: unused variable","code":{"code":"unused_variables"},"level":"warning","rendered":"warning: unused variable: `y`\n --> src/unstaged.rs:1:10\n"}}'
    exit 0
fi
echo "cargo: unknown subcommand" >&2
exit 1
CARGO_EOF
    chmod +x "$bindir/cargo"

    cat > "$bindir/shellcheck" <<'SH_EOF'
#!/usr/bin/env bash
echo "shellcheck $*" >> "$FAKE_TOOL_LOG"
exit 0
SH_EOF
    chmod +x "$bindir/shellcheck"

    cat > "$bindir/prettier" <<'PRETTIER_EOF'
#!/usr/bin/env bash
echo "prettier $*" >> "$FAKE_TOOL_LOG"
exit 0
PRETTIER_EOF
    chmod +x "$bindir/prettier"

    cat > "$bindir/eslint" <<'ESLINT_EOF'
#!/usr/bin/env bash
echo "eslint $*" >> "$FAKE_TOOL_LOG"
exit 0
ESLINT_EOF
    chmod +x "$bindir/eslint"

    # Local-only Prettier/ESLint resolution: the quality script resolves
    # node_modules/.bin/prettier (repo root) and web-ui/node_modules/.bin/eslint
    # relative to the repo.  bindir is always $tmpdir/bin, so repo is $tmpdir/repo.
    # Derive repo dir from bindir (always $tmpdir/repo when bindir is $tmpdir/bin).
    local repo_dir
    repo_dir="$(dirname "$bindir")/repo"
    mkdir -p "$repo_dir/node_modules/.bin" "$repo_dir/web-ui/node_modules/.bin"
    cp "$bindir/prettier" "$repo_dir/node_modules/.bin/prettier"
    cp "$bindir/eslint" "$repo_dir/web-ui/node_modules/.bin/eslint"
}

# Test helper: run script in temp repo
run_test_repo() {
    local setup_fn="$1"; shift
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"

    "$setup_fn" "$repo"

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    echo "EXIT:$exit_code"
    cat "$tmpdir/out.log"
    cat "$FAKE_TOOL_LOG" 2>/dev/null || true

    cd - >/dev/null
    rm -rf "$tmpdir"
    return $exit_code
}

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

fail() { echo "FAIL: $1" >&2; TESTS_FAILED=$((TESTS_FAILED + 1)); }
pass() { echo "PASS: $1"; TESTS_PASSED=$((TESTS_PASSED + 1)); }
run_test() { local name="$1"; shift; TESTS_RUN=$((TESTS_RUN + 1)); echo "--- Test: $name ---"; if "$@"; then pass "$name"; else fail "$name"; fi; }

# Test 1: Staged routing table
test_staged_routing() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo/src"
    echo 'fn main() {}' > "$repo/src/main.rs"
    echo '{"key": "value"}' > "$repo/config.json"

    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add src/main.rs config.json

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    grep -q "rustfmt --edition 2021 --config skip_children=true --check src/main.rs" "$FAKE_TOOL_LOG" || {
        echo "rustfmt not called correctly" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1;
    }
    grep -q "cargo clippy --workspace --all-targets --locked --message-format=json" "$FAKE_TOOL_LOG" || {
        echo "cargo clippy not called correctly" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1;
    }
    grep -q "prettier --check config.json" "$FAKE_TOOL_LOG" || {
        echo "prettier not called for JSON" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1;
    }
    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 2: Whitespace-safe arguments (spaces)
test_whitespace_safe_args() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo/src"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"

    touch "src/a b.rs"
    touch 'src/c d.rs'
    touch 'src/e f.rs'
    git add "src/a b.rs" "src/c d.rs" "src/e f.rs"

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    local count
    count=$(grep -c "  file: src/" "$FAKE_TOOL_LOG" 2>/dev/null || echo "0")
    [ "$count" -eq 3 ] || { echo "Expected 3 rustfmt file args, got $count" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }
    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 3: Deleted and internal files excluded
test_deleted_and_internal_filtering() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo/src"
    echo 'fn main() {}' > "$repo/src/lib.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add src/lib.rs
    git commit -q -m "initial"

    git rm src/lib.rs >/dev/null
    mkdir -p "$repo/.NeuroStrata" "$repo/src"
    echo 'internal' > "$repo/.NeuroStrata/state.json"
    git add .NeuroStrata/state.json

    echo 'fn foo() {}' > "$repo/src/new.rs"
    git add src/new.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    local count
    count=$(grep -c "file: src/new.rs" "$FAKE_TOOL_LOG" 2>/dev/null || echo "0")
    [ "$count" -eq 1 ] || { echo "Expected 1 call for new.rs, got $count" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }
    ! grep -q "file: src/lib.rs" "$FAKE_TOOL_LOG" || { echo "rustfmt called for deleted file" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }
    ! grep -q ".NeuroStrata" "$FAKE_TOOL_LOG" || { echo "tools called for .NeuroStrata/" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }
    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 4: Missing cargo tool -> nonzero exit with diagnostic
test_missing_tool_diagnostic() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    mkdir -p "$bindir"
    cat > "$bindir/rustfmt" <<'RF_EOF'
#!/usr/bin/env bash
echo "rustfmt $*" >> "$FAKE_TOOL_LOG"
exit 0
RF_EOF
    chmod +x "$bindir/rustfmt"

    mkdir -p "$repo/src"
    echo 'fn main() {}' > "$repo/src/main.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add src/main.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    [ $exit_code -ne 0 ] || { echo "Expected nonzero exit when cargo missing" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    grep -qi "cargo" "$tmpdir/out.log" || { echo "Missing tool diagnostic should mention cargo" >&2; cat "$tmpdir/out.log" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 5: Tool failure propagates
test_tool_failure_propagates() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    cat > "$bindir/rustfmt" <<'RF_EOF'
#!/usr/bin/env bash
echo "rustfmt $*" >> "$FAKE_TOOL_LOG"
exit 1
RF_EOF
    chmod +x "$bindir/rustfmt"

    mkdir -p "$repo/src"
    echo 'fn main() {}' > "$repo/src/main.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add src/main.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    [ $exit_code -ne 0 ] || { echo "Expected nonzero exit when rustfmt fails" >&2; cat "$tmpdir/out.log" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 6: Unchanged file/index hashes
test_unchanged_hashes() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo/src"
    echo 'fn main() {}' > "$repo/src/main.rs"
    echo '{"key": "value"}' > "$repo/config.json"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add src/main.rs config.json

    local index_before worktree_before index_after worktree_after
    index_before=$(git diff --cached --binary | sha256sum | cut -d' ' -f1)
    worktree_before=$(git ls-files -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    index_after=$(git diff --cached --binary | sha256sum | cut -d' ' -f1)
    worktree_after=$(git ls-files -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)

    [ $exit_code -eq 0 ] || { echo "Expected exit 0" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    [ "$index_before" = "$index_after" ] || { echo "Index hash changed!" >&2; return 1; }
    [ "$worktree_before" = "$worktree_after" ] || { echo "Worktree hash changed!" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 7: No generated bundle formatting (dist/ excluded)
test_no_generated_bundle() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo"
    echo 'console.log("hi")' > "$repo/app.js"
    mkdir -p "$repo/dist"
    echo '/* bundled */' > "$repo/dist/bundle.js"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add app.js dist/bundle.js

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    grep -q "prettier --check app.js" "$FAKE_TOOL_LOG" || { echo "prettier not called for app.js" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }
    ! grep -q "dist/bundle.js" "$FAKE_TOOL_LOG" || { echo "prettier called for dist/bundle.js" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }
    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 8: Clippy staged vs unrelated diagnostics
test_clippy_staged_vs_unrelated() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    mkdir -p "$bindir"
    cat > "$bindir/rustfmt" <<'RF_EOF'
#!/usr/bin/env bash
echo "rustfmt $*" >> "$FAKE_TOOL_LOG"
exit 0
RF_EOF
    chmod +x "$bindir/rustfmt"

    cat > "$bindir/cargo" <<'CARGO_EOF'
#!/usr/bin/env bash
echo "cargo $*" >> "$FAKE_TOOL_LOG"
if [[ "$*" == *"clippy"* ]] && [[ "$*" == *"--message-format=json"* ]]; then
    echo '{"reason":"compiler-message","message":{"spans":[{"file_name":"src/staged.rs","is_primary":true}],"message":"warning: unused variable: `x`","code":{"code":"unused_variables"},"level":"warning","rendered":"warning: unused variable: `x`\n --> src/staged.rs:1:10\n"}}'
    exit 0
fi
exit 1
CARGO_EOF
    chmod +x "$bindir/cargo"

    mkdir -p "$repo/src"
    echo 'fn main() {}' > "$repo/src/staged.rs"
    echo 'fn other() {}' > "$repo/src/unstaged.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add src/staged.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    [ $exit_code -ne 0 ] || { echo "Expected nonzero exit for staged clippy warning" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    grep -q "unused variable" "$tmpdir/out.log" || { echo "Staged diagnostic not printed" >&2; cat "$tmpdir/out.log" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 9: Clippy does NOT run when only non-Rust files are staged
test_clippy_skipped_without_rust_or_cargo() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo"
    echo '{"key":"value"}' > "$repo/config.json"
    echo '# hi' > "$repo/README.md"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add config.json README.md

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    ! grep -q "cargo clippy" "$FAKE_TOOL_LOG" 2>/dev/null || { echo "cargo clippy ran without staged Rust/Cargo files" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }
    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 10: Cargo.toml alone triggers clippy
test_clippy_runs_for_cargo_manifest() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo"
    echo '[package]' > "$repo/Cargo.toml"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add Cargo.toml

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    grep -q "cargo clippy --workspace --all-targets --locked --message-format=json" "$FAKE_TOOL_LOG" || { echo "cargo clippy not triggered by staged Cargo.toml" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }
    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 11: cargo emits no stdout and exits 1 -> runner fails due to cargo status
test_cargo_exit1_no_stdout() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    mkdir -p "$bindir"
    cat > "$bindir/rustfmt" <<'RF_EOF'
#!/usr/bin/env bash
echo "rustfmt $*" >> "$FAKE_TOOL_LOG"
exit 0
RF_EOF
    chmod +x "$bindir/rustfmt"

    # cargo clippy: no stdout at all, exit 1 (warning-level error)
    cat > "$bindir/cargo" <<'CARGO_EOF'
#!/usr/bin/env bash
echo "cargo $*" >> "$FAKE_TOOL_LOG"
if [[ "$*" == *"clippy"* ]] && [[ "$*" == *"--message-format=json"* ]]; then
    echo "error: could not compile due to 1 warning" >&2
    exit 1
fi
exit 1
CARGO_EOF
    chmod +x "$bindir/cargo"

    mkdir -p "$repo/src"
    echo 'fn main() {}' > "$repo/src/main.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add src/main.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    [ $exit_code -ne 0 ] || { echo "Expected nonzero exit for cargo exit 1 with no stdout" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    grep -q "cargo clippy failed (exit 1)" "$tmpdir/out.log" || { echo "Expected cargo clippy failure diagnostic" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    # exact fake invocation preserved
    grep -q "cargo clippy --workspace --all-targets --locked --message-format=json" "$FAKE_TOOL_LOG" || { echo "cargo clippy invocation changed" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 12: PATH prettier exists but local node_modules/.bin/prettier absent -> fail with npm ci, PATH never runs
test_local_prettier_mandatory() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    mkdir -p "$bindir"
    cat > "$bindir/prettier" <<'P_EOF'
#!/usr/bin/env bash
echo "PATH_PRETTIER_EXECUTED $*" >> "$FAKE_TOOL_LOG"
exit 0
P_EOF
    chmod +x "$bindir/prettier"

    mkdir -p "$repo"
    echo '{"key":"value"}' > "$repo/config.json"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add config.json

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    [ $exit_code -ne 0 ] || { echo "Expected nonzero exit when local prettier missing" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    grep -q "required tool 'prettier' not found" "$tmpdir/out.log" || { echo "Missing prettier setup diagnostic" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    grep -Fxq "  npm ci" "$tmpdir/out.log" || { echo "Setup hint must be exactly 'npm ci'" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    ! grep -q "PATH_PRETTIER_EXECUTED" "$FAKE_TOOL_LOG" 2>/dev/null || { echo "PATH prettier executed despite local-only requirement" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 13: PATH eslint exists but local web-ui/node_modules/.bin/eslint absent -> fail with npm --prefix web-ui ci
test_local_eslint_mandatory() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    mkdir -p "$bindir"
    cat > "$bindir/eslint" <<'E_EOF'
#!/usr/bin/env bash
echo "PATH_ESLINT_EXECUTED $*" >> "$FAKE_TOOL_LOG"
exit 0
E_EOF
    chmod +x "$bindir/eslint"
    # local prettier present so prettier check does not mask the eslint check
    mkdir -p "$repo/node_modules/.bin"
    cat > "$repo/node_modules/.bin/prettier" <<'P_EOF'
#!/usr/bin/env bash
echo "prettier $*" >> "$FAKE_TOOL_LOG"
exit 0
P_EOF
    chmod +x "$repo/node_modules/.bin/prettier"

    mkdir -p "$repo/web-ui"
    echo 'const x = 1;' > "$repo/web-ui/foo.ts"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add web-ui/foo.ts

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    [ $exit_code -ne 0 ] || { echo "Expected nonzero exit when local eslint missing" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    grep -q "required tool 'eslint' not found" "$tmpdir/out.log" || { echo "Missing eslint setup diagnostic" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    grep -Fxq "  npm --prefix web-ui ci" "$tmpdir/out.log" || { echo "Setup hint must be exactly 'npm --prefix web-ui ci'" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    ! grep -q "PATH_ESLINT_EXECUTED" "$FAKE_TOOL_LOG" 2>/dev/null || { echo "PATH eslint executed despite local-only requirement" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 14: local ESLint runs from $ROOT/web-ui with web-ui-relative args (space-safe, one arg); non-web-ui excluded
test_eslint_cwd_and_args() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"
    export ESLINT_LOG="$tmpdir/eslint.log"

    mkdir -p "$bindir"
    mkdir -p "$repo/node_modules/.bin" "$repo/web-ui/node_modules/.bin" "$repo/web-ui/src" "$repo/src"
    cat > "$repo/node_modules/.bin/prettier" <<'P_EOF'
#!/usr/bin/env bash
echo "prettier $*" >> "$FAKE_TOOL_LOG"
exit 0
P_EOF
    chmod +x "$repo/node_modules/.bin/prettier"

    cat > "$repo/web-ui/node_modules/.bin/eslint" <<'E_EOF'
#!/usr/bin/env bash
pwd >> "$ESLINT_LOG"
for a in "$@"; do echo "ARG:$a" >> "$ESLINT_LOG"; done
exit 0
E_EOF
    chmod +x "$repo/web-ui/node_modules/.bin/eslint"

    echo 'export const a = 1;' > "$repo/web-ui/src/a file.ts"
    echo 'const b = 1;' > "$repo/src/b.js"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add "web-ui/src/a file.ts" "src/b.js"

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?
    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    grep -Eq '/tree/web-ui$' "$ESLINT_LOG" || { echo "ESLint cwd was not the staged snapshot web-ui" >&2; cat "$ESLINT_LOG" >&2; return 1; }
    grep -Fxq "ARG:src/a file.ts" "$ESLINT_LOG" || { echo "ESLint did not receive web-ui-relative 'src/a file.ts' as one arg" >&2; cat "$ESLINT_LOG" >&2; return 1; }
    ! grep -q "ARG:src/b.js" "$ESLINT_LOG" || { echo "ESLint received non-web-ui src/b.js" >&2; cat "$ESLINT_LOG" >&2; return 1; }
    ! grep -q "ARG:web-ui/src/a file.ts" "$ESLINT_LOG" || { echo "ESLint received non-relative web-ui/ prefix" >&2; cat "$ESLINT_LOG" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 7: quality failure propagates nonzero and ledger unchanged on failure
# (delegation from pre-commit hook into project's check-staged-quality.sh)
test_pre_commit_hook_delegation_quality_failure_propagates_nonzero_ledger_unchanged() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    mkdir -p "$bindir"

    # A fake check-staged-quality.sh that fails with exit 1 and does NOT touch ledger
    cat > "$bindir/check-staged-quality.sh" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
    chmod +x "$bindir/check-staged-quality.sh"

    # Use a fake rustfmt to avoid real tool dependency
    cat > "$bindir/rustfmt" <<'RF_EOF'
#!/usr/bin/env bash
exit 0
RF_EOF
    chmod +x "$bindir/rustfmt"

    mkdir -p "$repo/src"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    echo 'fn main() {}' > "$repo/src/main.rs"
    git add "$repo/src/main.rs"

    # Set PATH so that our fake check-staged-quality.sh is not used by SCRIPT_PATH.
    # The pre-commit hook delegates by running "$ROOT/scripts/check-staged-quality.sh"
    # which is the repository's real script, not PATH.
    # So we will copy our fake script into the repo's scripts/ to replace it.
    mkdir -p "$repo/scripts"
    cp "$bindir/check-staged-quality.sh" "$repo/scripts/check-staged-quality.sh"
    chmod +x "$repo/scripts/check-staged-quality.sh"

    # Also install the pre-commit hook from the repository to the .git/hooks
    mkdir -p "$repo/.git/hooks"
    # Use the current working directory's scripts/hooks/pre-commit (the one we edited)
    HOOK_SRC="$HOOK_SCRIPT_PATH"
    if [ -f "$HOOK_SRC" ]; then
        cp "$HOOK_SRC" "$repo/.git/hooks/pre-commit"
        chmod +x "$repo/.git/hooks/pre-commit"
    else
        echo "warning: pre-commit hook source not found at $HOOK_SRC" >&2
    fi

    # Run the pre-commit hook
    PATH="$bindir:$PATH" bash "$repo/.git/hooks/pre-commit" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    # Expect nonzero exit (failure)
    if [ $exit_code -eq 0 ]; then
        echo "pre-commit delegation failure should exit nonzero" >&2
        cat "$tmpdir/out.log"
        rm -rf "$tmpdir"
        return 1
    fi

    # Expect ledger unchanged (if any). We expect no .NeuroStrata/session_queries.json
    # because the hook should exit before writing.
    local ledger="$repo/.NeuroStrata/session_queries.json"
    if [ -f "$ledger" ]; then
        echo "pre-commit delegation with quality failure should not write ledger" >&2
        cat "$ledger"
        rm -rf "$tmpdir"
        return 1
    fi

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 8: successful quality preserves existing graph-ledger writes/warnings
test_pre_commit_hook_delegation_quality_success_preserves_ledger() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    mkdir -p "$bindir"

    # A fake check-staged-quality.sh that succeeds and does not change ledger
    cat > "$bindir/check-staged-quality.sh" <<'EOF'
#!/usr/bin/env bash
# Successful, no ledger writes (only reads or writes that preserve existing)
exit 0
EOF
    chmod +x "$bindir/check-staged-quality.sh"

    cat > "$bindir/rustfmt" <<'RF_EOF'
#!/usr/bin/env bash
exit 0
RF_EOF
    chmod +x "$bindir/rustfmt"

    mkdir -p "$repo/src"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    echo 'fn main() {}' > "$repo/src/main.rs"
    git add "$repo/src/main.rs"

    # Copy fake quality script into repo's scripts/
    mkdir -p "$repo/scripts"
    cp "$bindir/check-staged-quality.sh" "$repo/scripts/check-staged-quality.sh"
    chmod +x "$repo/scripts/check-staged-quality.sh"

    # Create an existing ledger entry
    mkdir -p "$repo/.NeuroStrata"
    local ledger="$repo/.NeuroStrata/session_queries.json"
    cat > "$ledger" <<'LEDGER'
{
  "namespace": "testproj",
  "queries": [
    {
      "ts": 1234567890,
      "tool": "pre-commit",
      "query": "src/main.rs",
      "domains": ["src/main.rs"],
      "returned_ids": ["some-id"],
      "governed_prefixes": ["src/main.rs"]
    }
  ]
}
LEDGER

    # Install the pre-commit hook
    mkdir -p "$repo/.git/hooks"
    HOOK_SRC="$HOOK_SCRIPT_PATH"
    if [ -f "$HOOK_SRC" ]; then
        cp "$HOOK_SRC" "$repo/.git/hooks/pre-commit"
        chmod +x "$repo/.git/hooks/pre-commit"
    fi

    # Run the hook
    PATH="$bindir:$PATH" bash "$repo/.git/hooks/pre-commit" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    # Expect success exit
    if [ $exit_code -ne 0 ]; then
        echo "pre-commit delegation with successful quality should exit 0, got $exit_code" >&2
        cat "$tmpdir/out.log"
        rm -rf "$tmpdir"
        return 1
    fi

    # Expect ledger unchanged (same content)
    local ledger_content
    ledger_content=$(cat "$ledger")
    if ! echo "$ledger_content" | jq -e '.queries | length == 1' >/dev/null 2>&1; then
        echo "ledger should retain exactly one entry after successful quality" >&2
        rm -rf "$tmpdir"
        return 1
    fi

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test 9: absent quality script warns honestly and writes no fabricated ledger row
test_pre_commit_hook_delegation_absent_quality_script() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    mkdir -p "$bindir"

    cat > "$bindir/rustfmt" <<'RF_EOF'
#!/usr/bin/env bash
exit 0
RF_EOF
    chmod +x "$bindir/rustfmt"

    mkdir -p "$repo/src"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    echo 'fn main() {}' > "$repo/src/main.rs"
    git add "$repo/src/main.rs"

    # Do NOT copy any check-staged-quality.sh into repo's scripts/
    # The pre-commit hook's generic path: "scripts/check-staged-quality.sh" will be missing.

    # Install the pre-commit hook
    mkdir -p "$repo/.git/hooks"
    HOOK_SRC="$HOOK_SCRIPT_PATH"
    if [ -f "$HOOK_SRC" ]; then
        cp "$HOOK_SRC" "$repo/.git/hooks/pre-commit"
        chmod +x "$repo/.git/hooks/pre-commit"
    fi

    # Run the hook
    PATH="$bindir:$PATH" bash "$repo/.git/hooks/pre-commit" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    # Expect success exit because there is nothing to fail (no quality script)
    if [ $exit_code -ne 0 ]; then
        echo "pre-commit delegation with no quality script should exit 0, got $exit_code" >&2
        cat "$tmpdir/out.log"
        rm -rf "$tmpdir"
        return 1
    fi

    # Spec §6 R6.1: the hook must not fabricate `tool:"pre-commit"` rows for
    # staged files with no real graph lookup. It may create an empty ledger and
    # emit an honest warning — nothing more.
    local ledger="$repo/.NeuroStrata/session_queries.json"
    if [ ! -f "$ledger" ]; then
        echo "pre-commit delegation without quality script should still create ledger" >&2
        rm -rf "$tmpdir"
        return 1
    fi

    local queries_count
    queries_count=$(jq -r '.queries | length' "$ledger")
    if [ "$queries_count" -ne 0 ]; then
        echo "hook must not fabricate ledger rows (found $queries_count)" >&2
        cat "$ledger" >&2
        rm -rf "$tmpdir"
        return 1
    fi
    if jq -e '.queries[] | select(.tool == "pre-commit")' "$ledger" >/dev/null 2>&1; then
        echo "hook wrote a synthetic tool:pre-commit row" >&2
        rm -rf "$tmpdir"
        return 1
    fi
    if ! grep -q "no recorded NeuroStrata graph/memory lookup" "$tmpdir/out.log"; then
        echo "hook should emit an honest missing-lookup warning" >&2
        cat "$tmpdir/out.log" >&2
        rm -rf "$tmpdir"
        return 1
    fi

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Tests 18-19: tools consume staged blobs, never divergent worktree content
setup_partial_staging_repo() {
    local repo="$1"
    local bindir="$2"
    setup_fake_tools "$bindir"
    cat > "$bindir/rustfmt" <<'RF_EOF'
#!/usr/bin/env bash
echo "rustfmt $*" >> "$FAKE_TOOL_LOG"
for file; do
    if [ -f "$file" ] && grep -q 'let x=1' "$file"; then
        echo "unformatted staged Rust" >&2
        exit 1
    fi
done
exit 0
RF_EOF
    chmod +x "$bindir/rustfmt"
    mkdir -p "$repo/src"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
}

test_bad_staged_rust_fails_despite_formatted_worktree() {
    local tmpdir repo bindir exit_code
    tmpdir=$(mktemp -d)
    repo="$tmpdir/repo"
    bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"
    setup_partial_staging_repo "$repo" "$bindir"
    printf 'fn main(){let x=1;}\n' > src/main.rs
    git add src/main.rs
    printf 'fn main() {\n    let x = 1;\n}\n' > src/main.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    exit_code=$?
    [ $exit_code -ne 0 ] || { echo "Bad staged blob passed behind formatted worktree" >&2; cd - >/dev/null; return 1; }
    grep -q "unformatted staged Rust" "$tmpdir/out.log" || { echo "Rustfmt did not read staged blob" >&2; cd - >/dev/null; return 1; }
    cd - >/dev/null
    rm -rf "$tmpdir"
}

test_good_staged_rust_passes_despite_bad_worktree() {
    local tmpdir repo bindir exit_code
    tmpdir=$(mktemp -d)
    repo="$tmpdir/repo"
    bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"
    setup_partial_staging_repo "$repo" "$bindir"
    printf 'fn main() {\n    let x = 1;\n}\n' > src/main.rs
    git add src/main.rs
    printf 'fn main(){let x=1;}\n' > src/main.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    exit_code=$?
    [ $exit_code -eq 0 ] || { echo "Good staged blob rejected due to worktree content" >&2; cat "$tmpdir/out.log" >&2; cd - >/dev/null; return 1; }
    cd - >/dev/null
    rm -rf "$tmpdir"
}

# Test 20: extensionless staged Bash hooks are syntax-checked and shellchecked
test_extensionless_bash_hook_is_checked() {
    local tmpdir repo bindir exit_code
    tmpdir=$(mktemp -d)
    repo="$tmpdir/repo"
    bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"
    setup_fake_tools "$bindir"
    cat > "$bindir/shellcheck" <<'SC_EOF'
#!/usr/bin/env bash
echo "shellcheck $*" >> "$FAKE_TOOL_LOG"
for file; do
    if [ -f "$file" ] && grep -q 'echo $unquoted' "$file"; then
        echo "SC2086: quote staged hook expansion" >&2
        exit 1
    fi
done
exit 0
SC_EOF
    chmod +x "$bindir/shellcheck"
    mkdir -p "$repo/hooks"
    # shellcheck disable=SC2016  # fixture intentionally contains an unquoted expansion
    printf '#!/usr/bin/env bash\necho $unquoted\n' > "$repo/hooks/pre-commit"
    chmod +x "$repo/hooks/pre-commit"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add hooks/pre-commit

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    exit_code=$?
    [ $exit_code -ne 0 ] || { echo "Extensionless Bash hook skipped ShellCheck" >&2; cd - >/dev/null; return 1; }
    grep -q "hooks/pre-commit" "$FAKE_TOOL_LOG" || { echo "Hook was not passed to ShellCheck" >&2; cd - >/dev/null; return 1; }
    cd - >/dev/null
    rm -rf "$tmpdir"
}

# Direct /usr/bin interpreter paths must stay in the ShellCheck scan (#330).
test_direct_usr_bin_shebang_is_checked() {
    local shebang tmpdir repo bindir exit_code
    for shebang in '#!/usr/bin/bash' '#!/usr/bin/sh'; do
        tmpdir=$(mktemp -d)
        repo="$tmpdir/repo"
        bindir="$tmpdir/bin"
        export FAKE_TOOL_LOG="$tmpdir/tool.log"
        setup_fake_tools "$bindir"
        cat > "$bindir/shellcheck" <<'SC_EOF'
#!/usr/bin/env bash
echo "shellcheck $*" >> "$FAKE_TOOL_LOG"
for file; do
    if [ -f "$file" ] && grep -q 'echo $unquoted' "$file"; then
        echo "SC2086: quote staged hook expansion" >&2
        exit 1
    fi
done
exit 0
SC_EOF
        chmod +x "$bindir/shellcheck"
        mkdir -p "$repo/hooks"
        # shellcheck disable=SC2016  # fixture intentionally contains an unquoted expansion
        printf '%s\necho $unquoted\n' "$shebang" > "$repo/hooks/pre-commit"
        chmod +x "$repo/hooks/pre-commit"
        cd "$repo"
        git init -q
        git config user.email "test@test.com"
        git config user.name "Test"
        git add hooks/pre-commit
        PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
        exit_code=$?
        [ "$exit_code" -ne 0 ] || {
            echo "Direct /usr/bin shebang skipped ShellCheck: $shebang" >&2
            cd - >/dev/null
            rm -rf "$tmpdir"
            return 1
        }
        grep -q "hooks/pre-commit" "$FAKE_TOOL_LOG" || {
            echo "Hook was not passed to ShellCheck: $shebang" >&2
            cd - >/dev/null
            rm -rf "$tmpdir"
            return 1
        }
        cd - >/dev/null
        rm -rf "$tmpdir"
    done
}
# Test 21: staged Rust with no Cargo dep-info is not silently skipped
test_unbuilt_staged_rust_fails() {
    local tmpdir repo bindir exit_code
    tmpdir=$(mktemp -d)
    repo="$tmpdir/repo"
    bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"
    setup_fake_tools "$bindir"
    cat > "$bindir/cargo" <<'CARGO_EOF'
#!/usr/bin/env bash
echo "cargo $*" >> "$FAKE_TOOL_LOG"
exit 0
CARGO_EOF
    chmod +x "$bindir/cargo"
    mkdir -p "$repo/src" "$repo/fixtures"
    printf 'fn main() {}\n' > "$repo/src/main.rs"
    printf 'fn fixture() {}\n' > "$repo/fixtures/input.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add src/main.rs fixtures/input.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    exit_code=$?
    [ $exit_code -ne 0 ] || { echo "Unbuilt staged Rust was silently skipped" >&2; cd - >/dev/null; return 1; }
    grep -q "fixtures/input.rs" "$tmpdir/out.log" || { echo "Missing unbuilt Rust diagnostic" >&2; cat "$tmpdir/out.log" >&2; cd - >/dev/null; return 1; }
    cd - >/dev/null
    rm -rf "$tmpdir"
}

# Test 22: quality and mutation regressions execute as blocking CI steps
test_quality_suites_are_wired_into_ci() {
    local workflow="$REPO_ROOT/.github/workflows/ci.yml"
    local command
    for command in \
        "bash scripts/test-staged-quality.sh" \
        "node scripts/test-clippy-staged-diagnostics.mjs" \
        "NEUROSTRATA_REQUIRE_KANI=1 bash scripts/kani-mutation-check.sh"
    do
        grep -Fq -- "run: $command" "$workflow" || {
            echo "Missing blocking CI step: $command" >&2
            return 1
        }
        if grep -F -A3 -- "run: $command" "$workflow" | grep -q 'continue-on-error'; then
            echo "Quality step is report-only: $command" >&2
            return 1
        fi
    done
}

# Test 23: repo-local tools stay reachable after entering the staged snapshot
test_local_rustfmt_survives_snapshot_cwd() {
    local tmpdir repo bindir exit_code
    tmpdir=$(mktemp -d)
    repo="$tmpdir/repo"
    bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"
    setup_fake_tools "$bindir"
    mkdir -p "$repo/src" "$repo/node_modules/.bin"
    cat > "$repo/node_modules/.bin/rustfmt" <<'RUSTFMT_EOF'
#!/usr/bin/env bash
echo "local-rustfmt $*" >> "$FAKE_TOOL_LOG"
exit 0
RUSTFMT_EOF
    chmod +x "$repo/node_modules/.bin/rustfmt"
    printf 'fn main() {}\n' > "$repo/src/main.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add src/main.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    exit_code=$?
    [ $exit_code -eq 0 ] || { echo "Repo-local rustfmt was not resolved from the original root" >&2; cat "$tmpdir/out.log" >&2; cd - >/dev/null; return 1; }
    grep -Fq "local-rustfmt --edition 2021" "$FAKE_TOOL_LOG" || { echo "Repo-local rustfmt was not called" >&2; cd - >/dev/null; return 1; }
    cd - >/dev/null
    rm -rf "$tmpdir"
}

# Test: Type-change (symlink -> regular file) is staged and checked
test_type_change_is_checked() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo/src"
    echo 'fn main() {}' > "$repo/target.rs"
    ln -s ../target.rs "$repo/src/link.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add target.rs src/link.rs
    git commit -q -m "initial"

    rm src/link.rs
    echo 'fn main(){}' > src/link.rs
    git add src/link.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; return 1; }
    grep -q "file: src/link.rs" "$FAKE_TOOL_LOG" || { echo "type-changed file was not checked" >&2; cat "$FAKE_TOOL_LOG" >&2; return 1; }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test: rustfmt edition comes from the staged snapshot, not the working tree
test_edition_comes_from_staged_snapshot() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo/src"
    cat > "$repo/Cargo.toml" <<'CARGO_EOF'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
CARGO_EOF
    echo 'fn main() {}' > "$repo/src/main.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add Cargo.toml src/main.rs
    git commit -q -m "initial"

    # Unstaged working-tree edition bump; staged Rust is still edition 2021.
    sed 's/edition = "2021"/edition = "2024"/' Cargo.toml > Cargo.toml.new && mv Cargo.toml.new Cargo.toml
    echo '// staged change' >> src/main.rs
    git add src/main.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; cd - >/dev/null; rm -rf "$tmpdir"; return 1; }
    grep -Fq "rustfmt --edition 2021 --config skip_children=true --check src/main.rs" "$FAKE_TOOL_LOG" || {
        echo "rustfmt did not use the staged edition (2021)" >&2; cat "$FAKE_TOOL_LOG" >&2; cd - >/dev/null; rm -rf "$tmpdir"; return 1;
    }
    if grep -Fq -- "--edition 2024" "$FAKE_TOOL_LOG"; then
        echo "rustfmt used the unstaged working-tree edition (2024)" >&2; cat "$FAKE_TOOL_LOG" >&2; cd - >/dev/null; rm -rf "$tmpdir"; return 1;
    fi

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test: a staged Cargo.toml edition wins over an unstaged working-tree edit
test_edition_uses_staged_manifest() {
    local tmpdir
    tmpdir=$(mktemp -d)
    local repo="$tmpdir/repo"
    local bindir="$tmpdir/bin"
    export FAKE_TOOL_LOG="$tmpdir/tool.log"

    setup_fake_tools "$bindir"
    mkdir -p "$repo/src"
    cat > "$repo/Cargo.toml" <<'CARGO_EOF'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
CARGO_EOF
    echo 'fn main() {}' > "$repo/src/main.rs"
    cd "$repo"
    git init -q
    git config user.email "test@test.com"
    git config user.name "Test"
    git add Cargo.toml src/main.rs
    git commit -q -m "initial"

    sed 's/edition = "2021"/edition = "2024"/' Cargo.toml > Cargo.toml.new && mv Cargo.toml.new Cargo.toml
    git add Cargo.toml
    sed 's/edition = "2024"/edition = "2021"/' Cargo.toml > Cargo.toml.new && mv Cargo.toml.new Cargo.toml
    echo '// staged change' >> src/main.rs
    git add src/main.rs

    PATH="$bindir:$PATH" bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?

    [ $exit_code -eq 0 ] || { echo "Expected exit 0, got $exit_code" >&2; cat "$tmpdir/out.log" >&2; cd - >/dev/null; rm -rf "$tmpdir"; return 1; }
    grep -Fq "rustfmt --edition 2024 --config skip_children=true --check src/main.rs" "$FAKE_TOOL_LOG" || {
        echo "rustfmt did not use the staged Cargo.toml edition (2024)" >&2; cat "$FAKE_TOOL_LOG" >&2; cd - >/dev/null; rm -rf "$tmpdir"; return 1;
    }

    cd - >/dev/null
    rm -rf "$tmpdir"
    return 0
}

# Test: Outside a git repository the gate fails closed
test_outside_repo_fails_closed() {
    local tmpdir
    tmpdir=$(mktemp -d)
    cd "$tmpdir"
    bash "$SCRIPT_PATH" > "$tmpdir/out.log" 2>&1
    local exit_code=$?
    cd - >/dev/null

    if [ $exit_code -eq 0 ]; then
        echo "Expected nonzero exit outside a repo" >&2
        cat "$tmpdir/out.log" >&2
        rm -rf "$tmpdir"
        return 1
    fi
    if ! grep -q "not inside a git repository" "$tmpdir/out.log"; then
        echo "Missing not-a-repo diagnostic" >&2
        cat "$tmpdir/out.log" >&2
        rm -rf "$tmpdir"
        return 1
    fi

    rm -rf "$tmpdir"
    return 0
}

run_test "Staged routing table" test_staged_routing
run_test "Whitespace-safe arguments" test_whitespace_safe_args
run_test "Deleted and internal filtering" test_deleted_and_internal_filtering
run_test "Missing tool diagnostic" test_missing_tool_diagnostic
run_test "Tool failure propagates" test_tool_failure_propagates
run_test "Unchanged file/index hashes" test_unchanged_hashes
run_test "No generated bundle formatting" test_no_generated_bundle
run_test "Clippy staged vs unrelated" test_clippy_staged_vs_unrelated
run_test "Clippy skipped without Rust/Cargo files" test_clippy_skipped_without_rust_or_cargo
run_test "Clippy runs for staged Cargo.toml" test_clippy_runs_for_cargo_manifest
run_test "Cargo exit 1 with no stdout is fatal" test_cargo_exit1_no_stdout
run_test "Local Prettier mandatory over PATH" test_local_prettier_mandatory
run_test "Local ESLint mandatory over PATH" test_local_eslint_mandatory
run_test "ESLint cwd and web-ui-relative args" test_eslint_cwd_and_args
run_test "Pre-commit delegation quality failure nonzero, ledger unchanged" test_pre_commit_hook_delegation_quality_failure_propagates_nonzero_ledger_unchanged
run_test "Pre-commit delegation quality success preserves ledger" test_pre_commit_hook_delegation_quality_success_preserves_ledger
run_test "Pre-commit delegation absent quality script no fabrication" test_pre_commit_hook_delegation_absent_quality_script
run_test "Bad staged Rust fails despite formatted worktree" test_bad_staged_rust_fails_despite_formatted_worktree
run_test "Good staged Rust passes despite bad worktree" test_good_staged_rust_passes_despite_bad_worktree
run_test "Extensionless Bash hook is checked" test_extensionless_bash_hook_is_checked
run_test "Direct /usr/bin shebangs are checked" test_direct_usr_bin_shebang_is_checked
run_test "Unbuilt staged Rust fails" test_unbuilt_staged_rust_fails
run_test "Quality suites are wired into CI" test_quality_suites_are_wired_into_ci
run_test "Local Rustfmt survives snapshot cwd" test_local_rustfmt_survives_snapshot_cwd
run_test "Type-change is checked" test_type_change_is_checked
run_test "Edition comes from staged snapshot" test_edition_comes_from_staged_snapshot
run_test "Edition uses staged manifest" test_edition_uses_staged_manifest
run_test "Outside a repo fails closed" test_outside_repo_fails_closed

echo ""
echo "=== Summary ==="
echo "Tests run: $TESTS_RUN"
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"

[ $TESTS_FAILED -eq 0 ] || exit 1
