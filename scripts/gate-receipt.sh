#!/usr/bin/env bash
# gate-receipt: fail a PR when a gating review marker has no recorded receipt.
#
# A gating marker is CHANGES-NEEDED / CHANGES NEEDED / DO-NOT-MERGE / DO NOT MERGE,
# or a review finding badge: P1 Badge / P2 Badge (optionally wrapped in ** or
# preceded by the codex-code-review-finding marker comment). Inline findings are
# counted from unresolved review threads when GraphQL answers; if it does not,
# every inline finding counts (over-gating beats a silent pass).
# Receipts: fixed@<sha> and filed #<issue> from anyone; ruled: <reason> only
# from a maintainer (OWNER / MEMBER / COLLABORATOR). One receipt per marker.
#
# Usage: scripts/gate-receipt.sh --pr <n>
#        scripts/gate-receipt.sh --reviews-file <plain-text-or-json-fixture>
#        scripts/gate-receipt.sh --fixtures-dir <dir>
#          (reviews.json + issue-comments.json, then threads.json or
#           pull-comments.json; exercises the fetch layer without the network)
#        GATE_RECEIPT_REPO=owner/name overrides the repo used by --pr (useful
#        when a fork checkout wants to check the upstream PR).
set -euo pipefail

pr=""
fixture=""
fixtures_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pr) pr=${2:?--pr needs a number}; shift 2 ;;
    --reviews-file) fixture=${2:?--reviews-file needs a path}; shift 2 ;;
    --fixtures-dir) fixtures_dir=${2:?--fixtures-dir needs a path}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

json_entries() {
  jq -c '.[] | {body: (.body // ""), author_association: (.author_association // "NONE")}' "$1"
}

combine() {
  printf '%s\n' "$@" | grep -v '^$' || true
}

# Thread payload shape: [{isResolved, comments:[{body, author_association}]}].
thread_entries() {
  if [ "$2" = "unresolved" ]; then
    printf '%s' "$1" | jq -c '.[] | select(.isResolved != true) | (.comments // [])[] | {body: (.body // ""), author_association: (.author_association // "NONE")}'
  else
    printf '%s' "$1" | jq -c '.[] | (.comments // [])[] | {body: (.body // ""), author_association: (.author_association // "NONE")}'
  fi
}

reviews_entries=""
comment_entries=""
threads_json=""
pull_entries=""
thread_mode=""
marker_entries=""
receipt_entries=""

if [ -n "$fixture" ]; then
  [ -f "$fixture" ] || { echo "no such fixture: $fixture" >&2; exit 2; }
  if jq -e . "$fixture" >/dev/null 2>&1; then
    marker_entries=$(json_entries "$fixture")
  else
    marker_entries=$(jq -cn --rawfile body "$fixture" '{body:$body, author_association:"OWNER"}')
  fi
  receipt_entries=$marker_entries
elif [ -n "$fixtures_dir" ]; then
  [ -d "$fixtures_dir" ] || { echo "no such fixture directory: $fixtures_dir" >&2; exit 2; }
  for f in reviews.json issue-comments.json; do
    [ -f "$fixtures_dir/$f" ] || { echo "missing $fixtures_dir/$f" >&2; exit 2; }
  done
  reviews_entries=$(json_entries "$fixtures_dir/reviews.json")
  comment_entries=$(json_entries "$fixtures_dir/issue-comments.json")
  if [ -f "$fixtures_dir/threads.json" ]; then
    threads_json=$(jq -c . "$fixtures_dir/threads.json" 2>/dev/null) || {
      echo "gate-receipt: $fixtures_dir/threads.json is not valid JSON" >&2
      exit 2
    }
    if ! printf '%s' "$threads_json" | jq -e 'type == "array"' >/dev/null 2>&1; then
      echo "gate-receipt: $fixtures_dir/threads.json must be a JSON array of threads" >&2
      exit 2
    fi
    thread_mode=threads
  elif [ -f "$fixtures_dir/pull-comments.json" ]; then
    if ! jq -e 'type == "array"' "$fixtures_dir/pull-comments.json" >/dev/null 2>&1; then
      echo "gate-receipt: $fixtures_dir/pull-comments.json must be a JSON array of comments" >&2
      exit 2
    fi
    pull_entries=$(json_entries "$fixtures_dir/pull-comments.json")
    thread_mode=fallback
  else
    echo "missing $fixtures_dir/threads.json or $fixtures_dir/pull-comments.json" >&2
    exit 2
  fi
else
  [ -n "$pr" ] || { echo "usage: gate-receipt.sh --pr <n> | --reviews-file <path> | --fixtures-dir <path>" >&2; exit 2; }
  repo=${GATE_RECEIPT_REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner)}
  owner=${repo%%/*}
  name=${repo#*/}
  # Reviews and comments only. The PR body often describes the gate itself
  # ("one receipt per CHANGES-NEEDED marker") and must not count as a marker.
  # Fail closed: unavailable evidence is not clearance.
  reviews_entries=$(gh api "repos/$repo/pulls/$pr/reviews" --paginate --jq '.[] | {body:(.body // ""), author_association:(.author_association // "NONE")}') || {
    echo "gate-receipt: cannot fetch reviews for #$pr" >&2
    exit 1
  }
  comment_entries=$(gh api "repos/$repo/issues/$pr/comments" --paginate --jq '.[] | {body:(.body // ""), author_association:(.author_association // "NONE")}') || {
    echo "gate-receipt: cannot fetch comments for #$pr" >&2
    exit 1
  }
  # Codex P1/P2 findings live in inline review comments, and only unresolved
  # threads gate. When GraphQL is unavailable, fall back to every inline
  # comment and warn: an unresolvable thread must never look like clearance.
  threads_json=$(gh api graphql -f query='
    query($owner: String!, $name: String!, $pr: Int!) {
      repository(owner: $owner, name: $name) {
        pullRequest(number: $pr) {
          reviewThreads(first: 100) {
            nodes {
              isResolved
              comments(first: 10) { nodes { body authorAssociation } }
            }
          }
        }
      }
    }' -F owner="$owner" -F name="$name" -F pr="$pr" \
    --jq '[.data.repository.pullRequest.reviewThreads.nodes[] | {isResolved: .isResolved, comments: [.comments.nodes[] | {body: (.body // ""), author_association: (.authorAssociation // "NONE")}]}]' 2>/dev/null) || threads_json=""
  if printf '%s' "$threads_json" | jq -e 'type == "array"' >/dev/null 2>&1; then
    thread_mode=threads
  else
    pull_entries=$(gh api "repos/$repo/pulls/$pr/comments" --paginate --jq '.[] | {body:(.body // ""), author_association:(.author_association // "NONE")}') || {
      echo "gate-receipt: cannot fetch inline review comments for #$pr" >&2
      exit 1
    }
    thread_mode=fallback
  fi
fi

if [ "$thread_mode" = "threads" ]; then
  marker_entries=$(combine "$reviews_entries" "$comment_entries" "$(thread_entries "$threads_json" unresolved)")
  # Receipts stay global: a fixed@/filed/ruled: reply clears a marker even if
  # it sits on a thread that has since been resolved.
  receipt_entries=$(combine "$reviews_entries" "$comment_entries" "$(thread_entries "$threads_json" all)")
elif [ "$thread_mode" = "fallback" ]; then
  marker_entries=$(combine "$reviews_entries" "$comment_entries" "$pull_entries")
  receipt_entries=$marker_entries
  echo "gate-receipt: review-thread resolution unavailable; counting every inline finding (may over-gate)" >&2
fi

all_text=$(printf '%s\n' "$marker_entries" | jq -r '.body' 2>/dev/null || true)
receipt_text=$(printf '%s\n' "$receipt_entries" | jq -r '.body' 2>/dev/null || true)
markers=$(printf '%s\n' "$all_text" | grep -Eoi 'CHANGES[-_ ]?NEEDED|DO[-_ ]?NOT[-_ ]?MERGE|P[12][[:space:]]*-?[[:space:]]*Badge' | wc -l | tr -d ' ' || true)
fixed_filed=$(printf '%s\n' "$receipt_text" | grep -Eoi 'fixed@[0-9a-f]{7,}|filed[[:space:]]+#[0-9]+' | wc -l | tr -d ' ' || true)
ruled=$(printf '%s\n' "$receipt_entries" | jq -r 'select(.author_association == "OWNER" or .author_association == "MEMBER" or .author_association == "COLLABORATOR") | .body' 2>/dev/null | grep -Eoi 'ruled:' | wc -l | tr -d ' ' || true)
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
