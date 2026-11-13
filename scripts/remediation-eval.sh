#!/usr/bin/env bash
# Run one registered remediation eval suite and emit JSON evidence.
#
# Usage: scripts/remediation-eval.sh <suite> [--json <path>]
#
# Refuses tracked dirty trees unless EVAL_ALLOW_DIRTY=1. The runner requires
# every expected test to print `test <name> ... ok` and requires zero ignored
# tests, so a renamed test or a zero-match filter cannot pass silently.
set -euo pipefail

suite=${1:?usage: remediation-eval.sh <suite> [--json <path>]}
shift || true
out=""
if [ "${1:-}" = "--json" ]; then
  out=${2:?--json needs a path}
fi

root=$(git rev-parse --show-toplevel)
registry=${EVAL_REGISTRY:-$root/eval/registry.json}
[ -f "$registry" ] || { echo "registry not found: $registry" >&2; exit 2; }

if [ "${EVAL_ALLOW_DIRTY:-0}" != "1" ]; then
  if ! git -C "$root" diff --quiet || ! git -C "$root" diff --cached --quiet; then
    echo "refusing to run on a tracked dirty tree (set EVAL_ALLOW_DIRTY=1 for local TDD)" >&2
    exit 3
  fi
fi

entry=$(jq -c --arg s "$suite" '.suites[$s] // empty' "$registry")
[ -n "$entry" ] || { echo "unknown suite: $suite" >&2; exit 2; }

commands=()
while IFS= read -r line; do commands+=("$line"); done < <(jq -r --arg s "$suite" '.suites[$s].commands[]' "$registry")
expected=()
while IFS= read -r line; do expected+=("$line"); done < <(jq -r --arg s "$suite" '.suites[$s].tests[]' "$registry")
[ "${#expected[@]}" -gt 0 ] || { echo "suite has no expected tests: $suite" >&2; exit 2; }

commit=$(git -C "$root" rev-parse HEAD)
log=$(mktemp)
trap 'rm -f "$log"' EXIT

passed=0
failed=0
ignored=0
for cmd in "${commands[@]}"; do
  set +e
  output=$(cd "$root" && bash -c "$cmd" 2>&1)
  status=$?
  set -e
  printf '%s\n' "$output" >> "$log"
  summary=$(grep -E '^test result:' <<<"$output" | tail -1 || true)
  if [ -z "$summary" ]; then
    echo "no test summary for: $cmd" >&2
    echo "$output" >&2
    exit 1
  fi
  p=$(sed -nE 's/.* ([0-9]+) passed.*/\1/p' <<<"$summary" | tail -1)
  f=$(sed -nE 's/.* ([0-9]+) failed.*/\1/p' <<<"$summary" | tail -1)
  i=$(sed -nE 's/.* ([0-9]+) ignored.*/\1/p' <<<"$summary" | tail -1)
  passed=$((passed + ${p:-0}))
  failed=$((failed + ${f:-0}))
  ignored=$((ignored + ${i:-0}))
  if [ "$status" -ne 0 ]; then
    echo "command failed: $cmd" >&2
    exit 1
  fi
done

for name in "${expected[@]}"; do
  if ! grep -Fq "test $name ... ok" "$log"; then
    echo "expected test did not pass: $name" >&2
    exit 1
  fi
done

threshold_ignored=$(jq -r --arg s "$suite" '.suites[$s].thresholds.ignored // 0' "$registry")
if [ "$ignored" -ne "$threshold_ignored" ]; then
  echo "ignored count $ignored != required $threshold_ignored" >&2
  exit 1
fi
if [ "$failed" -ne 0 ]; then
  echo "failed count $failed != 0" >&2
  exit 1
fi

result=$(jq -n \
  --arg suite "$suite" \
  --arg commit "$commit" \
  --argjson passed "$passed" \
  --argjson failed "$failed" \
  --argjson ignored "$ignored" \
  --argjson expected "$(printf '%s\n' "${expected[@]}" | jq -R . | jq -s .)" \
  --argjson thresholds "$(jq -c --arg s "$suite" '.suites[$s].thresholds' "$registry")" \
  '{suite:$suite, commit:$commit, passed:$passed, failed:$failed, ignored:$ignored, expected:$expected, thresholds:$thresholds, verdict:(if $failed==0 and $ignored==0 then "PASS" else "FAIL" end)}')

if [ -n "$out" ]; then
  mkdir -p "$(dirname "$out")"
  printf '%s\n' "$result" > "$out"
  echo "wrote $out"
else
  printf '%s\n' "$result"
fi
