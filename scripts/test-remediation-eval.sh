#!/usr/bin/env bash
# Self-test for scripts/remediation-eval.sh: a real passing test clears, a
# renamed expectation fails, and a zero-match command fails.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

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
  bash "$root/scripts/remediation-eval.sh" self-pass --json "$tmp/pass.json" >/dev/null
jq -e '.verdict == "PASS" and .failed == 0 and .ignored == 0' "$tmp/pass.json" >/dev/null

if EVAL_REGISTRY="$tmp/registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" self-wrong-name >/dev/null 2>&1; then
  echo "FAIL: a renamed expectation passed" >&2
  exit 1
fi

if EVAL_REGISTRY="$tmp/registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" self-zero-match >/dev/null 2>&1; then
  echo "FAIL: a zero-match filter passed" >&2
  exit 1
fi

# A failed rerun must invalidate an earlier PASS at the same output path
# instead of leaving stale evidence for a caller to read.
EVAL_REGISTRY="$tmp/registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" self-pass --json "$tmp/stale.json" >/dev/null
jq -e '.verdict == "PASS"' "$tmp/stale.json" >/dev/null
if EVAL_REGISTRY="$tmp/registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" self-wrong-name --json "$tmp/stale.json" >/dev/null 2>&1; then
  echo "FAIL: a renamed expectation passed with an output path" >&2
  exit 1
fi
if [ -e "$tmp/stale.json" ]; then
  echo "FAIL: a failed rerun left stale PASS evidence" >&2
  exit 1
fi

# Lightweight registry for output-path safety regressions (no cargo work).
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
    }
  }
}
JSON

# --json must not delete the registry used by the same run.
cp -f "$tmp/path-registry.json" "$tmp/protected-registry.json"
if EVAL_REGISTRY="$tmp/protected-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/protected-registry.json" >/dev/null 2>&1; then
  echo "FAIL: --json colliding with the registry was accepted" >&2
  exit 1
fi
if ! jq -e . "$tmp/protected-registry.json" >/dev/null 2>&1; then
  echo "FAIL: evaluation deleted its registry" >&2
  exit 1
fi

# Any existing repository input is protected, not just the named paths.
if EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$root/Cargo.toml" >/dev/null 2>&1; then
  echo "FAIL: --json naming a repository input was accepted" >&2
  exit 1
fi
if ! [ -f "$root/Cargo.toml" ]; then
  echo "FAIL: evaluation deleted a repository input" >&2
  exit 1
fi

# Two runs on one output path must not publish another run's verdict.
mkdir -p "$tmp/shared-locks"
EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  REMEDIATION_EVAL_LOCK_DIR="$tmp/shared-locks" \
  bash "$root/scripts/remediation-eval.sh" quick-fail --json "$tmp/race.json" >"$tmp/fail.out" 2>/dev/null &
fail_pid=$!
EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  REMEDIATION_EVAL_LOCK_DIR="$tmp/shared-locks" \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/race.json" >"$tmp/pass.out" 2>/dev/null &
pass_pid=$!
if wait "$fail_pid"; then
  echo "FAIL: concurrent renamed expectation passed" >&2
  exit 1
fi
wait "$pass_pid"
fail_id=$(sed -n 's/^run_id=//p' "$tmp/fail.out" | head -1)
pass_id=$(sed -n 's/^run_id=//p' "$tmp/pass.out" | head -1)
if [ -z "$fail_id" ] || [ -z "$pass_id" ]; then
  echo "FAIL: runs did not emit run ids for verdict attribution" >&2
  exit 1
fi
if [ -z "$(ls -A "$tmp/shared-locks")" ]; then
  echo "FAIL: no lock shared outside the worktree was used" >&2
  exit 1
fi
if grep -q '"verdict":"PASS"' "$tmp/fail.out"; then
  echo "FAIL: the failing run published a PASS verdict" >&2
  exit 1
fi
if [ -e "$tmp/race.json" ]; then
  # A final PASS is acceptable only when it belongs to the passing run,
  # never to the failing run or to an earlier invocation.
  if ! jq -e --arg pass "$pass_id" --arg fail "$fail_id" \
    '.verdict == "PASS" and .run_id == $pass and .run_id != $fail' "$tmp/race.json" >/dev/null; then
    echo "FAIL: a stale or foreign verdict was left at the shared output path" >&2
    exit 1
  fi
fi

echo "remediation-eval self-test: PASS"
