#!/usr/bin/env bash
# Self-test for scripts/disk-hygiene.sh.
#
# Every destructive path in the script is exercised against COPIES of the
# fixtures in scripts/fixtures/disk-hygiene/, inside a scratch directory. The
# script is pointed at the copy through its environment overrides, so the real
# /tmp, the real worktree root and the real repository are never touched.
#
# The cases pin the behaviour issue #215 asks for:
#   1. the repo root resolves from the script's own checkout
#   2. an unresolvable root exits 2 instead of silently doing nothing
#   3. --dry-run prints the plan and deletes nothing
#   4. idle targets and stale temp dirs are reclaimed, fresh ones are kept
#   5. cargo's dependency artifacts are never deleted
#   6. an active build stops every destructive step
#   7. the script never mentions a dependency-artifact directory
set -euo pipefail

root=$(git rev-parse --show-toplevel)
script="$root/scripts/disk-hygiene.sh"
fixture="$root/scripts/fixtures/disk-hygiene"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

failures=0
checks=0
EXTRA_ARGS=()

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

assert_exists() {
  checks=$((checks + 1))
  if [ ! -e "$1" ]; then
    fail "expected to exist: $1"
  fi
}

assert_absent() {
  checks=$((checks + 1))
  if [ -e "$1" ]; then
    fail "expected to be gone: $1"
  fi
}

assert_contains() {
  checks=$((checks + 1))
  if ! grep -qF -- "$2" "$1"; then
    fail "expected '$2' in $1"
  fi
}

assert_not_contains() {
  checks=$((checks + 1))
  if grep -qF -- "$2" "$1"; then
    fail "did not expect '$2' in $1"
  fi
}

# Age a whole subtree: every file and directory gets the same old mtime, so
# "idle" is decided by the content and not by the directory entry alone.
age_tree() {
  find "$1" -exec touch -d '3 days ago' {} +
}

# A fresh copy of the fixtures, with a git repository at the fixture repo root
# so the script can resolve a root, and with the idle parts aged.
new_case() {
  case_dir="$tmp/$1"
  rm -rf "$case_dir"
  mkdir -p "$case_dir"
  cp -R "$fixture/repo" "$fixture/worktrees" "$fixture/tmp-base" "$case_dir/"
  git -C "$case_dir/repo" init --quiet
  age_tree "$case_dir/repo/target/debug/incremental/stale-cache"
  age_tree "$case_dir/repo/target/debug/deps"
  age_tree "$case_dir/repo/.worktrees/wt-stale"
  age_tree "$case_dir/worktrees/1ebe42/wt-stale"
  age_tree "$case_dir/tmp-base/ns-stale"
  age_tree "$case_dir/tmp-base/rustc-stale"
  age_tree "$case_dir/tmp-base/keep-me"
  mkdir -p "$case_dir/bin"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$case_dir/bin/pgrep-active"
  printf '#!/usr/bin/env bash\nexit 1\n' > "$case_dir/bin/pgrep-inactive"
  chmod +x "$case_dir/bin/pgrep-active" "$case_dir/bin/pgrep-inactive"
}

# The environment that keeps the run inside the fixture. The pgrep lookup is
# stubbed so the result does not depend on what the host happens to be
# building at the time of the test.
run_hygiene() {
  case_dir="$1"
  shift
  env \
    NEUROSTRATA_REPO_ROOT="$case_dir/repo" \
    NEUROSTRATA_OPENCODE_WORKTREES="$case_dir/worktrees" \
    NEUROSTRATA_DISK_HYGIENE_TMP_BASES="$case_dir/tmp-base" \
    NEUROSTRATA_DISK_HYGIENE_PODMAN="" \
    NEUROSTRATA_DISK_HYGIENE_PGREP="$case_dir/bin/pgrep-inactive" \
    NEUROSTRATA_TMP_MIN_AGE_MINUTES=60 \
    NEUROSTRATA_WORKTREE_TARGET_IDLE_DAYS=1 \
    NEUROSTRATA_OLD_TEST_BIN_DAYS=2 \
    "$@" \
    bash "$script" "${EXTRA_ARGS[@]}" > "$case_dir/out.log" 2> "$case_dir/err.log"
}

# --- 1. the root resolves from the script's own checkout -------------------
new_case root-from-checkout
if env \
     NEUROSTRATA_REPO_ROOT="" \
     NEUROSTRATA_OPENCODE_WORKTREES="$case_dir/worktrees" \
     NEUROSTRATA_DISK_HYGIENE_TMP_BASES="$case_dir/tmp-base" \
     NEUROSTRATA_DISK_HYGIENE_PODMAN="" \
     NEUROSTRATA_DISK_HYGIENE_PGREP="$case_dir/bin/pgrep-inactive" \
     bash "$script" --dry-run > "$case_dir/out.log" 2> "$case_dir/err.log"; then
  # The script lives in the real repository, so its own checkout is the real
  # root; the fixture root is only used when the override is absent. Either
  # way the run must succeed and name the root it resolved.
  assert_contains "$case_dir/out.log" "repo root:"
else
  fail "the script failed to resolve a root from its own checkout"
fi

# --- 2. an unresolvable root exits 2 ---------------------------------------
new_case missing-root
set +e
env \
  NEUROSTRATA_REPO_ROOT="$case_dir/does-not-exist" \
  NEUROSTRATA_DISK_HYGIENE_TMP_BASES="$case_dir/tmp-base" \
  NEUROSTRATA_DISK_HYGIENE_PODMAN="" \
  bash "$script" --dry-run > "$case_dir/out.log" 2> "$case_dir/err.log"
status=$?
set -e
checks=$((checks + 1))
if [ "$status" -ne 2 ]; then
  fail "a missing repo root exited $status instead of 2"
fi
assert_contains "$case_dir/err.log" "repo root"

# --- 3. --dry-run deletes nothing -------------------------------------------
new_case dry-run
EXTRA_ARGS=(--dry-run)
run_hygiene "$case_dir"
EXTRA_ARGS=()
assert_contains "$case_dir/out.log" "would remove"
assert_exists "$case_dir/worktrees/1ebe42/wt-stale/target"
assert_exists "$case_dir/repo/.worktrees/wt-stale/target"
assert_exists "$case_dir/repo/target/debug/incremental/stale-cache"
assert_exists "$case_dir/tmp-base/ns-stale"
assert_exists "$case_dir/tmp-base/rustc-stale"

# --- 4. a real run reclaims idle state and keeps fresh state ----------------
new_case reclaim
run_hygiene "$case_dir"
assert_absent "$case_dir/worktrees/1ebe42/wt-stale/target"
assert_absent "$case_dir/repo/.worktrees/wt-stale/target"
assert_absent "$case_dir/repo/target/debug/incremental/stale-cache"
assert_absent "$case_dir/tmp-base/ns-stale"
assert_absent "$case_dir/tmp-base/rustc-stale"
assert_exists "$case_dir/worktrees/1ebe42/wt-fresh/target"
assert_exists "$case_dir/repo/.worktrees/wt-fresh/target"
assert_exists "$case_dir/repo/target/debug/incremental/fresh-cache"
assert_exists "$case_dir/tmp-base/ns-fresh"
assert_exists "$case_dir/tmp-base/keep-me"

# --- 5. cargo's dependency artifacts are never deleted ----------------------
assert_exists "$case_dir/repo/target/debug/deps/stale-test-binary"

# --- 6. an active build stops every destructive step ------------------------
new_case active-build
run_hygiene "$case_dir" NEUROSTRATA_DISK_HYGIENE_PGREP="$case_dir/bin/pgrep-active"
assert_exists "$case_dir/worktrees/1ebe42/wt-stale/target"
assert_exists "$case_dir/repo/.worktrees/wt-stale/target"
assert_exists "$case_dir/repo/target/debug/incremental/stale-cache"
assert_exists "$case_dir/tmp-base/ns-stale"
assert_exists "$case_dir/tmp-base/rustc-stale"
assert_contains "$case_dir/out.log" "build active"

# With no build running the same run does reclaim them.
new_case no-active-build
run_hygiene "$case_dir" NEUROSTRATA_DISK_HYGIENE_PGREP="$case_dir/bin/pgrep-inactive"
assert_absent "$case_dir/worktrees/1ebe42/wt-stale/target"
assert_absent "$case_dir/repo/.worktrees/wt-stale/target"
assert_absent "$case_dir/tmp-base/ns-stale"

# --- 7. the script never touches cargo's dependency artifacts --------------
checks=$((checks + 1))
if grep -q "debug/deps" "$script"; then
  fail "the script still references a dependency-artifact directory"
fi

echo "disk-hygiene self-test: $checks checks, $failures failures"
[ "$failures" -eq 0 ]
