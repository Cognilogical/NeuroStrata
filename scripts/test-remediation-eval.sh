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
  bash "$root/scripts/remediation-eval.sh" self-pass --json "$tmp/pass.json"
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
cp "$tmp/path-registry.json" "$tmp/protected-registry.json"
if EVAL_REGISTRY="$tmp/protected-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/protected-registry.json" >/dev/null 2>&1; then
  echo "FAIL: --json colliding with the registry was accepted" >&2
  exit 1
fi
if ! jq -e . "$tmp/protected-registry.json" >/dev/null 2>&1; then
  echo "FAIL: evaluation deleted its registry" >&2
  exit 1
fi

# Two runs on one output path must not publish another run's verdict.
EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-fail --json "$tmp/race.json" >/dev/null 2>&1 &
fail_pid=$!
EVAL_REGISTRY="$tmp/path-registry.json" EVAL_ALLOW_DIRTY=1 \
  bash "$root/scripts/remediation-eval.sh" quick-pass --json "$tmp/race.json" >/dev/null 2>&1 &
pass_pid=$!
if wait "$fail_pid"; then
  echo "FAIL: concurrent renamed expectation passed" >&2
  exit 1
fi
wait "$pass_pid"
if [ -e "$tmp/race.json" ] && ! jq -e '.verdict == "PASS" and .suite == "quick-pass"' "$tmp/race.json" >/dev/null; then
  echo "FAIL: concurrent runs published an inconsistent verdict" >&2
  exit 1
fi

echo "remediation-eval self-test: PASS"
