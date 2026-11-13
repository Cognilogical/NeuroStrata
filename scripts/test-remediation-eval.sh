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

echo "remediation-eval self-test: PASS"
