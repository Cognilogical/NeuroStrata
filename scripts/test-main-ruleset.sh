#!/usr/bin/env bash
# Self-test for scripts/enforce-main-ruleset.sh (#227).

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
CHECK="$ROOT/scripts/enforce-main-ruleset.sh"
SPEC="$ROOT/scripts/main-ruleset.json"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

# 1. Checked-in canonical spec must pass.
bash "$CHECK" --spec-check >/dev/null

expect_fail() {
  local name="$1"
  local jq_filter="$2"
  local expected_substr="$3"
  local fixture="$TMP_DIR/${name}.json"
  jq "$jq_filter" "$SPEC" >"$fixture"
  set +e
  out="$(bash "$CHECK" --fixture "$fixture" 2>&1)"
  rc=$?
  set -e
  if [ "$rc" -eq 0 ]; then
    echo "FAIL ($name): expected non-zero exit code, got 0" >&2
    exit 1
  fi
  if ! printf '%s\n' "$out" | grep -Fq "$expected_substr"; then
    echo "FAIL ($name): output did not contain '$expected_substr': $out" >&2
    exit 1
  fi
}

# 2. Negative checks covering every #227 invariant.
expect_fail "missing-thread-resolution" \
  '(.rules[] | select(.type == "pull_request") | .parameters.required_review_thread_resolution) = false' \
  "required_review_thread_resolution must be true"

expect_fail "missing-last-push-approval" \
  '(.rules[] | select(.type == "pull_request") | .parameters.require_last_push_approval) = false' \
  "require_last_push_approval must be true"

expect_fail "missing-gate-receipt-check" \
  '(.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks) |= map(select(.context != "gate-receipt"))' \
  "required_status_checks must require 'gate-receipt'"

expect_fail "extra-bypass-actors" \
  '.bypass_actors += [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}]' \
  "bypass_actors must contain only the single OrganizationAdmin break-glass identity"

expect_fail "missing-non-fast-forward" \
  '.rules |= map(select(.type != "non_fast_forward"))' \
  "rules must include 'non_fast_forward'"

echo "test-main-ruleset: all checks passed"
