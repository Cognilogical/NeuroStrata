#!/usr/bin/env bash
# gate-receipt: fail a PR when a gating review marker has no recorded receipt.
#
# A gating marker is CHANGES-NEEDED / CHANGES NEEDED / DO-NOT-MERGE / DO NOT MERGE,
# or a review finding badge: P1 Badge / P2 Badge (optionally wrapped in ** or
# preceded by the codex-code-review-finding marker comment). Inline findings are
# counted from unresolved review threads when GraphQL answers; if it does not,
# every inline finding counts (over-gating beats a silent pass).
# Receipts: fixed@<sha> and filed #<issue> from anyone; ruled: <reason> only
# from a maintainer (OWNER / MEMBER / COLLABORATOR).
#
# Receipts are bound to their finding: in thread mode each unresolved thread,
# and the top-level review/comment pool, must carry receipts for its own
# markers. Resolved threads contribute neither markers nor receipts. The legacy
# --reviews-file mode keeps the older global receipt pool.
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

tmpdir=""
trap '[ -n "$tmpdir" ] && rm -rf "$tmpdir"' EXIT

json_entries() {
  jq -c '.[] | {body: (.body // ""), author_association: (.author_association // "NONE")}' "$1"
}

combine() {
  printf '%s\n' "$@" | grep -v '^$' || true
}

count_markers() {
  grep -Eoi 'CHANGES[-_ ]?NEEDED|DO[-_ ]?NOT[-_ ]?MERGE|P[12][[:space:]]*-?[[:space:]]*Badge' | wc -l | tr -d ' ' || true
}

count_receipts() {
  # A receipt quoted as an example (inside inline code) is not a receipt.
  sed -e 's/`[^`]*`//g' | grep -Eoi 'fixed@[0-9a-f]{7,}|filed[[:space:]]+#[0-9]+' | wc -l | tr -d ' ' || true
}

count_ruled() {
  jq -r 'select(.author_association == "OWNER" or .author_association == "MEMBER" or .author_association == "COLLABORATOR") | .body' 2>/dev/null \
    | sed -e 's/`[^`]*`//g' | grep -Eoi 'ruled:' | wc -l | tr -d ' ' || true
}

# Per-thread accounting: unresolved threads only, each thread needs its own
# receipts, and top-level markers/receipts form a separate pool.
thread_stats() {
  jq -c '
    def mcount: [scan("CHANGES[-_ ]?NEEDED|DO[-_ ]?NOT[-_ ]?MERGE|P[12][[:space:]]*-?[[:space:]]*Badge"; "i")] | length;
    def rcount: [scan("fixed@[0-9a-f]{7,}|filed[[:space:]]+#[0-9]+"; "i")] | length;
    def ucount: [scan("ruled:"; "i")] | length;
    def maint: (. == "OWNER") or (. == "MEMBER") or (. == "COLLABORATOR");
    def stats:
      .comments as $c
      | ([$c[] | (.body | mcount)] | add // 0) as $m
      | ([$c[] | ((.body | gsub("`[^`]*`"; "") | rcount) + (if (.author_association | maint) then (.body | gsub("`[^`]*`"; "") | ucount) else 0 end))] | add // 0) as $r
      | {markers: $m, receipts: $r, outstanding: ([$m - $r, 0] | max)};
    [ .[] | select(.isResolved != true) | stats ]
    | {markers: (map(.markers) | add // 0), receipts: (map(.receipts) | add // 0), outstanding: (map(.outstanding) | add // 0)}
  '
}

# Fixture shapes are validated strictly: a typo in a fixture must fail the
# gate, never silently produce "no gating marker".
require_comment_array() {
  if ! jq -e 'type == "array" and all(.[]; (type == "object") and ((.body | type) == "string"))' "$1" >/dev/null 2>&1; then
    echo "gate-receipt: $1 must be a JSON array of objects with a string body" >&2
    return 1
  fi
}

# Accepts either a bare array of threads or {pageInfo, nodes}. Each thread may
# carry comments as an array, or as {totalCount, nodes} like the GraphQL API.
# Prints {truncated, nodes}, where nodes is the canonical internal shape.
load_threads_fixture() {
  jq -c '
    def valid_comment: (type == "object") and ((.body | type) == "string");
    def valid_comments:
      if type == "array" then all(.[]; valid_comment)
      elif type == "object" then
        ((.nodes | type) == "array")
        and all(.nodes[]; valid_comment)
        and ((has("totalCount") | not) or ((.totalCount | type) == "number"))
      else false end;
    def valid_thread:
      (type == "object")
      and (has("isResolved") or has("comments"))
      and ((has("isResolved") | not) or ((.isResolved | type) == "boolean"))
      and ((has("comments") | not) or (.comments | valid_comments));
    def valid_doc:
      if type == "array" then all(.[]; valid_thread)
      elif (type == "object") and ((.nodes | type) == "array") then
        all(.nodes[]; valid_thread)
        and ((has("pageInfo") | not) or ((.pageInfo | type) == "object"
             and ((.pageInfo | has("hasNextPage") | not) or ((.pageInfo.hasNextPage | type) == "boolean"))))
      else false end;
    if (valid_doc | not) then error("invalid threads fixture") else . end
    | (if type == "array" then {pageInfo: {}, nodes: .} else . end)
    | . as $doc
    | ($doc.nodes | map({
        isResolved: (.isResolved // false),
        commentsTotal: (if (.comments | type) == "object"
                        then (.comments.totalCount // ((.comments.nodes // []) | length))
                        else ((.comments // []) | length) end),
        comments: (if (.comments | type) == "object" then (.comments.nodes // []) else (.comments // []) end)
      })) as $nodes
    | {truncated: (($doc.pageInfo.hasNextPage // false)
                   or ([$nodes[] | select(.commentsTotal > (.comments | length))] | length > 0)),
       nodes: $nodes}
  ' "$1"
}

threads_query='
  query($owner: String!, $name: String!, $pr: Int!, $endCursor: String) {
    repository(owner: $owner, name: $name) {
      pullRequest(number: $pr) {
        reviewThreads(first: 100, after: $endCursor) {
          pageInfo { hasNextPage endCursor }
          nodes {
            isResolved
            comments(first: 100) { totalCount nodes { body authorAssociation } }
          }
        }
      }
    }
  }'

reviews_entries=""
comment_entries=""
top_marker_entries=""
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
    require_comment_array "$fixtures_dir/$f" || exit 2
  done
  reviews_entries=$(json_entries "$fixtures_dir/reviews.json")
  comment_entries=$(json_entries "$fixtures_dir/issue-comments.json")
  if [ -f "$fixtures_dir/threads.json" ]; then
    if ! normalized=$(load_threads_fixture "$fixtures_dir/threads.json" 2>/dev/null); then
      echo "gate-receipt: $fixtures_dir/threads.json must be a JSON array of threads: {isResolved?: boolean, comments?: [{body: string}]}" >&2
      exit 2
    fi
    if [ "$(printf '%s' "$normalized" | jq -r '.truncated')" = "true" ]; then
      echo "gate-receipt: $fixtures_dir/threads.json is truncated (pageInfo.hasNextPage or comments.totalCount exceeds the comments present); refusing to under-count" >&2
      exit 1
    fi
    threads_json=$(printf '%s' "$normalized" | jq -c '.nodes')
    thread_mode=threads
  elif [ -f "$fixtures_dir/pull-comments.json" ]; then
    require_comment_array "$fixtures_dir/pull-comments.json" || exit 2
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
  # threads gate. Page both threads and their comments until exhausted; a
  # truncated list is refused rather than silently under-counted.
  tmpdir=$(mktemp -d)
  threads_pages="$tmpdir/threads"
  : > "$threads_pages"
  cursor=""
  page_no=0
  threads_ok=1
  while :; do
    page_no=$((page_no + 1))
    gql_args=(-f query="$threads_query" -F owner="$owner" -F name="$name" -F pr="$pr")
    [ -n "$cursor" ] && gql_args+=(-F endCursor="$cursor")
    page=$(gh api graphql "${gql_args[@]}" --jq '
      .data.repository.pullRequest.reviewThreads
      | {hasNextPage: (.pageInfo.hasNextPage // false),
         endCursor: (.pageInfo.endCursor // ""),
         nodes: [.nodes[] | {
           isResolved: (.isResolved // false),
           commentsTotal: (.comments.totalCount // 0),
           comments: [.comments.nodes[] | {body: (.body // ""), author_association: (.authorAssociation // "NONE")}]
         }]}' 2>/dev/null) || { threads_ok=0; break; }
    printf '%s\n' "$page" >> "$threads_pages"
    if [ "$(printf '%s' "$page" | jq -r '.hasNextPage')" != "true" ]; then
      break
    fi
    if [ "$page_no" -ge 50 ]; then
      echo "gate-receipt: review thread list truncated after 50 pages for #$pr" >&2
      exit 1
    fi
    cursor=$(printf '%s' "$page" | jq -r '.endCursor')
    if [ -z "$cursor" ]; then
      echo "gate-receipt: review thread pagination returned no cursor for #$pr" >&2
      exit 1
    fi
  done
  if [ "$threads_ok" -eq 1 ]; then
    threads_json=$(jq -s 'map(.nodes) | add // []' "$threads_pages")
    truncated=$(printf '%s' "$threads_json" | jq '[.[] | select(.commentsTotal > (.comments | length))] | length')
    if [ "$truncated" -gt 0 ]; then
      echo "gate-receipt: review comment list truncated on #$pr; refusing to under-count receipts or findings" >&2
      exit 1
    fi
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
  top_marker_entries=$(combine "$reviews_entries" "$comment_entries")
  stats=$(printf '%s' "$threads_json" | thread_stats)
  thread_markers=$(printf '%s' "$stats" | jq -r '.markers')
  thread_receipts=$(printf '%s' "$stats" | jq -r '.receipts')
  thread_outstanding=$(printf '%s' "$stats" | jq -r '.outstanding')
  top_markers=$(printf '%s\n' "$top_marker_entries" | jq -r '.body' 2>/dev/null | count_markers)
  top_fixed_filed=$(printf '%s\n' "$top_marker_entries" | jq -r '.body' 2>/dev/null | count_receipts)
  top_ruled=$(printf '%s\n' "$top_marker_entries" | count_ruled)
  top_markers=${top_markers:-0}
  top_fixed_filed=${top_fixed_filed:-0}
  top_ruled=${top_ruled:-0}
  top_receipts=$((top_fixed_filed + top_ruled))
  top_outstanding=$((top_markers > top_receipts ? top_markers - top_receipts : 0))
  markers=$((top_markers + thread_markers))
  receipts=$((top_receipts + thread_receipts))
  outstanding=$((top_outstanding + thread_outstanding))
elif [ "$thread_mode" = "fallback" ]; then
  marker_entries=$(combine "$reviews_entries" "$comment_entries" "$pull_entries")
  receipt_entries=$marker_entries
  echo "gate-receipt: review-thread resolution unavailable; counting every inline finding (may over-gate)" >&2
  markers=$(printf '%s\n' "$marker_entries" | jq -r '.body' 2>/dev/null | count_markers)
  fixed_filed=$(printf '%s\n' "$receipt_entries" | jq -r '.body' 2>/dev/null | count_receipts)
  ruled=$(printf '%s\n' "$receipt_entries" | count_ruled)
  markers=${markers:-0}
  fixed_filed=${fixed_filed:-0}
  ruled=${ruled:-0}
  receipts=$((fixed_filed + ruled))
  outstanding=$((markers > receipts ? markers - receipts : 0))
else
  markers=$(printf '%s\n' "$marker_entries" | jq -r '.body' 2>/dev/null | count_markers)
  fixed_filed=$(printf '%s\n' "$receipt_entries" | jq -r '.body' 2>/dev/null | count_receipts)
  ruled=$(printf '%s\n' "$receipt_entries" | count_ruled)
  markers=${markers:-0}
  fixed_filed=${fixed_filed:-0}
  ruled=${ruled:-0}
  receipts=$((fixed_filed + ruled))
  outstanding=$((markers > receipts ? markers - receipts : 0))
fi

if [ "$outstanding" -eq 0 ] && [ "$markers" -eq 0 ]; then
  echo "gate-receipt: no gating marker"
  exit 0
fi
if [ "$outstanding" -gt 0 ]; then
  echo "gate-receipt: $outstanding outstanding finding(s) without a receipt; $markers marker(s) seen, $receipts receipt(s)" >&2
  echo "each marker needs its own fixed@<sha>, filed #<issue>, or maintainer ruled: <reason> in the same thread or body" >&2
  exit 1
fi
echo "gate-receipt: $markers marker(s), $receipts receipt(s) — cleared"
