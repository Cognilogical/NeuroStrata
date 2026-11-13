#!/usr/bin/env bash
# gate-receipt: fail a PR when a gating review marker has no recorded receipt.
#
# A gating marker is CHANGES-NEEDED / CHANGES NEEDED / DO-NOT-MERGE / DO NOT MERGE.
# A receipt is one of: fixed@<sha>, filed #<issue>, ruled: <reason>.
# One receipt per marker is required.
#
# Usage: scripts/gate-receipt.sh --pr <n>
#        scripts/gate-receipt.sh --reviews-file <fixture>
set -euo pipefail

pr=""
fixture=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pr) pr=${2:?--pr needs a number}; shift 2 ;;
    --reviews-file) fixture=${2:?--reviews-file needs a path}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -n "$fixture" ]; then
  [ -f "$fixture" ] || { echo "no such fixture: $fixture" >&2; exit 2; }
  text=$(cat "$fixture")
else
  [ -n "$pr" ] || { echo "usage: gate-receipt.sh --pr <n> | --reviews-file <path>" >&2; exit 2; }
  repo=$(gh repo view --json nameWithOwner -q .nameWithOwner)
  text=$(
    gh api "repos/$repo/pulls/$pr/reviews" --jq '.[].body' 2>/dev/null || true
    gh api "repos/$repo/issues/$pr/comments" --jq '.[].body' 2>/dev/null || true
    gh api "repos/$repo/pulls/$pr" --jq '.body' 2>/dev/null || true
  )
fi

markers=$(printf '%s\n' "$text" | grep -Eic 'CHANGES[-_ ]?NEEDED|DO[-_ ]?NOT[-_ ]?MERGE' || true)
receipts=$(printf '%s\n' "$text" | grep -Eic 'fixed@[0-9a-f]{7,}|filed[[:space:]]+#[0-9]+|ruled:' || true)
markers=${markers:-0}
receipts=${receipts:-0}

if [ "$markers" -eq 0 ]; then
  echo "gate-receipt: no gating marker"
  exit 0
fi
if [ "$receipts" -lt "$markers" ]; then
  echo "gate-receipt: $markers gating marker(s), $receipts receipt(s)" >&2
  echo "every marker needs fixed@<sha>, filed #<issue>, or ruled: <reason>" >&2
  exit 1
fi
echo "gate-receipt: $markers marker(s), $receipts receipt(s) — cleared"
