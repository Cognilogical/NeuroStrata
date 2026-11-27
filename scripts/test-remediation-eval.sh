#!/usr/bin/env bash
# Self-test for scripts/remediation-eval.sh: passing suites clear, renamed or
# zero-match expectations fail, and output/lock paths cannot corrupt inputs.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
tmp=$(mktemp -d)
trap '[ -n "${KEEP_TMP:-}" ] || rm -rf "$tmp"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

# Build a restricted PATH that omits one command. This exercises the macOS
# fallbacks on Linux without adding test-only switches to the runner.
tool_dir() {
  local dir=$1 skip=$2 command target
  mkdir -p "$dir"
  for command in bash git jq cksum awk sed grep tail head date mktemp cp mv \
    mkdir rm rmdir stat id chmod cat sleep dirname basename pwd ls env \
    readlink true; do
    [ "$command" = "$skip" ] && continue
    target=$(command -v "$command" 2>/dev/null) || continue
    ln -sf "$target" "$dir/$command"
  done
}

cat > "$tmp/registry.json" <<'JSON'
{
  "version": 1,
  "suites": {
    "self-pass": {
      "commands": ["cargo test --locked --bin neurostrata-mcp budget_verified_ok -- --test-threads=1"],
      "tests": ["retention::gate::tests::budget_verified_ok"],
      "thresholds": {"ignored": 0}
    },
    "self-wrong-name": {
      "commands": ["cargo test --locked --bin neurostrata-mcp budget_verified_ok -- --test-threads=1"],
      "tests": ["retention::gate::tests::not_a_real_test"],
      "thresholds": {"ignored": 0}
    },
    "self-zero-match": {
      "commands": ["cargo test --locked --bin neurostrata-mcp zzz_no_such_test -- --test-threads=1"],
      "tests": ["zzz_no_such_test"],
      "thresholds": {"ignored": 0}
    }
  }
}
JSON

EVAL_REGISTRY="$tmp/registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" --suite self-pass --receipt "$tmp/pass.json" >/dev/null
jq -e '.verdict == "PASS" and .failed == 0 and .ignored == 0' "$tmp/pass.json" >/dev/null

if EVAL_REGISTRY="$tmp/registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" self-wrong-name >/dev/null 2>&1; then
  fail "a renamed expectation passed"
fi
if EVAL_REGISTRY="$tmp/registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" self-zero-match >/dev/null 2>&1; then
  fail "a zero-match filter passed"
fi

EVAL_REGISTRY="$tmp/registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" self-pass --json "$tmp/stale.json" >/dev/null
jq -e '.verdict == "PASS"' "$tmp/stale.json" >/dev/null
if EVAL_REGISTRY="$tmp/registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" self-wrong-name --json "$tmp/stale.json" >/dev/null 2>&1; then
  fail "a renamed expectation passed with an output path"
fi
[ ! -e "$tmp/stale.json" ] || fail "a failed rerun left stale PASS evidence"

# Lightweight registry for path/lock regressions (no cargo work).
cat > "$tmp/path-registry.json" <<'JSON'
{
  "version": 1,
  "suites": {
    "quick-pass": {
      "commands": ["printf 'test t1 ... ok\\ntest result: ok. 1 passed; 0 failed; 0 ignored\\n'"],
      "tests": ["t1"],
      "thresholds": {"ignored": 0}
    },
    "quick-fail": {
      "commands": ["printf 'test t1 ... ok\\ntest result: ok. 1 passed; 0 failed; 0 ignored\\n'"],
      "tests": ["no_such_test"],
      "thresholds": {"ignored": 0}
    },
    "slow-pass": {
      "commands": ["sleep 3; printf 'test t1 ... ok\\ntest result: ok. 1 passed; 0 failed; 0 ignored\\n'"],
      "tests": ["t1"],
      "thresholds": {"ignored": 0}
    }
  }
}
JSON

# --json must not delete the registry used by the same run.
cp -f "$tmp/path-registry.json" "$tmp/protected-registry.json"
if EVAL_REGISTRY="$tmp/protected-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/protected-registry.json" >/dev/null 2>&1; then
  fail "--json colliding with the registry was accepted"
fi
jq -e . "$tmp/protected-registry.json" >/dev/null || fail "evaluation deleted its registry"

# Any existing repository input is protected, not just the named paths.
if EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$root/Cargo.toml" >/dev/null 2>&1; then
  fail "--json naming a repository input was accepted"
fi
[ -f "$root/Cargo.toml" ] || fail "evaluation deleted a repository input"

# A linked worktree's metadata lives outside the checkout's .git directory.
git clone -q --no-hardlinks "$root" "$tmp/clone" || fail "cannot clone fixture"
git -C "$tmp/clone" worktree add -q --detach "$tmp/linked" || fail "cannot add linked worktree"
external_git=$(git -C "$tmp/linked" rev-parse --absolute-git-dir)
cp -f "$external_git/HEAD" "$tmp/head.before"
if (cd "$tmp/linked" && EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$external_git/HEAD" >/dev/null 2>&1); then
  fail "--json naming the external git directory was accepted"
fi
cmp -s "$external_git/HEAD" "$tmp/head.before" || fail "external git metadata was modified"

# With the GNU resolver unavailable, final symlinks must still be resolved.
tool_dir "$tmp/no-realpath" realpath
ln -s "$tmp/path-registry.json" "$tmp/registry-link.json"
ln -s "$tmp/symlink-target.json" "$tmp/output-link.json"
PATH="$tmp/no-realpath" EVAL_REGISTRY="$tmp/registry-link.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/output-link.json" >"$tmp/sym.out" 2>/dev/null \
  || fail "symlink fallback run failed"
jq -e '.verdict == "PASS"' "$tmp/symlink-target.json" >/dev/null || fail "symlink output target was not updated"
ln -s "$tmp/path-registry.json" "$tmp/output-registry-link.json"
if PATH="$tmp/no-realpath" EVAL_REGISTRY="$tmp/registry-link.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/output-registry-link.json" >/dev/null 2>&1; then
  fail "a symlink to the active registry was accepted as output"
fi
jq -e . "$tmp/path-registry.json" >/dev/null || fail "registry alias was deleted"

# A predictable lock name must not truncate a symlink target.
lock_key() { printf '%s' "$1" | cksum | awk '{print $1}'; }
evil_locks="$tmp/evil-locks"
mkdir -p "$evil_locks"
printf 'victim content\n' > "$tmp/lock-victim.txt"
ln -s "$tmp/lock-victim.txt" "$evil_locks/$(lock_key "$tmp/lockout.json").lock"
if EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  REMEDIATION_EVAL_LOCK_DIR="$evil_locks" \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/lockout.json" >/dev/null 2>&1; then
  fail "a symlink lock was accepted"
fi
[ "$(cat "$tmp/lock-victim.txt")" = "victim content" ] || fail "symlink lock truncated its target"

# flock wait must be bounded even when a holder stalls.
flock_locks="$tmp/flock-locks"
mkdir -p "$flock_locks"
flock -x "$flock_locks/$(lock_key "$tmp/flockout.json").lock" -c 'sleep 20' 2>/dev/null &
holder=$!
sleep 0.3
started=$(date +%s)
if REMEDIATION_EVAL_LOCK_TIMEOUT=2 EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  REMEDIATION_EVAL_LOCK_DIR="$flock_locks" \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/flockout.json" >/dev/null 2>&1; then
  fail "a contender acquired a held flock lock"
fi
[ "$(( $(date +%s) - started ))" -le 10 ] || fail "flock wait was not bounded"
kill "$holder" 2>/dev/null || true
wait "$holder" 2>/dev/null || true

# The mkdir fallback must reclaim a lock left by a dead owner.
tool_dir "$tmp/no-flock" flock
stale_locks="$tmp/stale-locks"
mkdir -p "$stale_locks"
stale_dir="$stale_locks/$(lock_key "$tmp/staleout.json").lock.d"
mkdir -p "$stale_dir"
printf '99999999\n' > "$stale_dir/owner"
PATH="$tmp/no-flock" EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  REMEDIATION_EVAL_LOCK_DIR="$stale_locks" \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/staleout.json" >/dev/null 2>&1 \
  || fail "stale mkdir lock was not reclaimed"
jq -e '.verdict == "PASS"' "$tmp/staleout.json" >/dev/null || fail "stale-lock run did not publish a verdict"
[ ! -d "$stale_dir" ] || fail "owned lock directory was not released"

# A timed-out contender must not remove the owner's mkdir lock.
shared_locks="$tmp/shared-locks"
mkdir -p "$shared_locks"
PATH="$tmp/no-flock" EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  REMEDIATION_EVAL_LOCK_DIR="$shared_locks" \
  bash "$root/scripts/remediation-eval.sh" slow-pass --json "$tmp/shared.json" >"$tmp/owner.out" 2>/dev/null &
owner_pid=$!
contender_dir="$shared_locks/$(lock_key "$tmp/shared.json").lock.d"
waited=0
until [ -d "$contender_dir" ]; do
  waited=$((waited + 1))
  [ "$waited" -gt 50 ] && fail "owner never acquired the mkdir lock"
  sleep 0.1
done
PATH="$tmp/no-flock" EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  REMEDIATION_EVAL_LOCK_TIMEOUT=1 REMEDIATION_EVAL_LOCK_DIR="$shared_locks" \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/shared.json" >"$tmp/contender.out" 2>/dev/null &
contender_pid=$!
sleep 0.3
kill -TERM "$contender_pid" 2>/dev/null || true
wait "$contender_pid" 2>/dev/null || true
[ -d "$contender_dir" ] || fail "a contender removed the owner's lock"
wait "$owner_pid" || fail "owner run failed"
[ ! -d "$contender_dir" ] || fail "owner did not release its lock"

# Concurrent runs on one output path must not publish another run's verdict.
EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  REMEDIATION_EVAL_LOCK_DIR="$shared_locks" \
  bash "$root/scripts/remediation-eval.sh" quick-fail --json "$tmp/race.json" >"$tmp/fail.out" 2>/dev/null &
fail_pid=$!
EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  REMEDIATION_EVAL_LOCK_DIR="$shared_locks" \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/race.json" >"$tmp/pass.out" 2>/dev/null &
pass_pid=$!
if wait "$fail_pid"; then fail "concurrent renamed expectation passed"; fi
wait "$pass_pid" || fail "concurrent passing run failed"
fail_id=$(sed -n 's/^run_id=//p' "$tmp/fail.out" | head -1)
pass_id=$(sed -n 's/^run_id=//p' "$tmp/pass.out" | head -1)
[ -n "$fail_id" ] || fail "failing run did not emit a run id"
[ -n "$pass_id" ] || fail "passing run did not emit a run id"
grep -q '"verdict":"PASS"' "$tmp/fail.out" && fail "the failing run published a PASS verdict"
if [ -e "$tmp/race.json" ]; then
  jq -e --arg pass "$pass_id" --arg fail "$fail_id" \
    '.verdict == "PASS" and .run_id == $pass and .run_id != $fail' "$tmp/race.json" >/dev/null \
    || fail "a stale or foreign verdict was left at the shared output path"
fi

# One run observes one revision: the gate reads the commit the runner pinned.
jq -n --arg cmd 'test "$TRACKED_UPGRADES_COMMIT" = "$(git rev-parse HEAD)" && printf "test revision ... ok\\ntest result: ok. 1 passed; 0 failed; 0 ignored\\n"' \
  '{version:1,suites:{revision:{commands:[$cmd],tests:["revision"],thresholds:{ignored:0}}}}' \
  > "$tmp/revision-registry.json"
EVAL_REGISTRY="$tmp/revision-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" revision >/dev/null 2>&1 \
  || fail "the gate was not pinned to the runner's revision"

# A HEAD change during the run invalidates the verdict.
jq -n --arg cmd 'git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t@t commit -q --allow-empty -m moved && printf "test revision ... ok\\ntest result: ok. 1 passed; 0 failed; 0 ignored\\n"' \
  '{version:1,suites:{moving:{commands:[$cmd],tests:["revision"],thresholds:{ignored:0}}}}' \
  > "$tmp/moving-registry.json"
if (cd "$tmp/clone" && EVAL_REGISTRY="$tmp/moving-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" moving >/dev/null 2>&1); then
  fail "a run that changed HEAD still published a verdict"
fi

echo "remediation-eval self-test: PASS"
