#!/usr/bin/env bash
# gate-receipt: fail a PR when a gating review marker has no recorded receipt.
#
# A gating marker is CHANGES-NEEDED / CHANGES NEEDED / DO-NOT-MERGE / DO NOT MERGE.
# Receipts: fixed@<sha> and filed #<issue> from anyone; ruled: <reason> only
# from a maintainer (OWNER / MEMBER / COLLABORATOR). One receipt per marker.
#
# Usage: scripts/gate-receipt.sh --pr <n>
#        scripts/gate-receipt.sh --reviews-file <plain-text-or-json-fixture>
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

entries=""
if [ -n "$fixture" ]; then
  [ -f "$fixture" ] || { echo "no such fixture: $fixture" >&2; exit 2; }
  if jq -e . "$fixture" >/dev/null 2>&1; then
    entries=$(jq -c '.[] | {body:(.body // ""), author_association:(.author_association // "NONE")}' "$fixture")
  else
    entries=$(jq -cn --rawfile body "$fixture" '{body:$body, author_association:"OWNER"}')
  fi
else
  [ -n "$pr" ] || { echo "usage: gate-receipt.sh --pr <n> | --reviews-file <path>" >&2; exit 2; }
  repo=$(gh repo view --json nameWithOwner -q .nameWithOwner)
  # Reviews and comments only. The PR body often describes the gate itself
  # ("one receipt per CHANGES-NEEDED marker") and must not count as a marker.
  # Fail closed: unavailable evidence is not clearance.
  reviews=$(gh api "repos/$repo/pulls/$pr/reviews" --paginate --jq '.[] | {body:(.body // ""), author_association:(.author_association // "NONE")}') || {
    echo "gate-receipt: cannot fetch reviews for #$pr" >&2
    exit 1
  }
  comments=$(gh api "repos/$repo/issues/$pr/comments" --paginate --jq '.[] | {body:(.body // ""), author_association:(.author_association // "NONE")}') || {
    echo "gate-receipt: cannot fetch comments for #$pr" >&2
    exit 1
  }
  entries=$(printf '%s\n%s\n' "$reviews" "$comments")
fi

all_text=$(printf '%s\n' "$entries" | jq -r '.body' 2>/dev/null || true)
markers=$(printf '%s\n' "$all_text" | grep -Eoi 'CHANGES[-_ ]?NEEDED|DO[-_ ]?NOT[-_ ]?MERGE' | wc -l | tr -d ' ' || true)
fixed_filed=$(printf '%s\n' "$all_text" | grep -Eoi 'fixed@[0-9a-f]{7,}|filed[[:space:]]+#[0-9]+' | wc -l | tr -d ' ' || true)
ruled=$(printf '%s\n' "$entries" | jq -r 'select(.author_association == "OWNER" or .author_association == "MEMBER" or .author_association == "COLLABORATOR") | .body' 2>/dev/null | grep -Eoi 'ruled:' | wc -l | tr -d ' ' || true)
markers=${markers:-0}
fixed_filed=${fixed_filed:-0}
ruled=${ruled:-0}
receipts=$((fixed_filed + ruled))

if [ "$markers" -eq 0 ]; then
  echo "gate-receipt: no gating marker"
  exit 0
fi
if [ "$receipts" -lt "$markers" ]; then
  echo "gate-receipt: $markers gating marker(s), $fixed_filed fixed/filed receipt(s), $ruled maintainer ruling(s)" >&2
  echo "every marker needs fixed@<sha>, filed #<issue>, or a maintainer ruled: <reason>" >&2
  exit 1
fi
echo "gate-receipt: $markers marker(s), $receipts receipt(s) — cleared"
