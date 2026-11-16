#!/usr/bin/env bash
# gate-receipt: fail a PR when a gating review marker has no recorded receipt.
#
# A gating marker is CHANGES-NEEDED / CHANGES NEEDED / DO-NOT-MERGE / DO NOT MERGE,
# a P1/P2 finding badge (P1 Badge / P1-Badge / shields.io), or a bold **P1** /
# **P2**. Inline findings come from review threads when GraphQL answers; if it
# does not, every inline finding counts (over-gating beats a silent pass).
#
# Receipts: fixed@<sha> and filed #<issue> from the PR author or a maintainer
# (OWNER / MEMBER / COLLABORATOR), never from an association-less account, and
# never from the comment that carries the finding. ruled: <reason> only from a
# maintainer. In --pr mode fixed@ is a commit on the PR history and filed # must
# exist; fixture and legacy modes enforce authorship but cannot validate targets.
#
# Accounting: each review thread (resolved or not) must carry receipts for its
# own markers, and top-level review/comment bodies form a separate pool.
# Resolution alone never clears a finding. The legacy --reviews-file mode keeps
# the older global, same-body receipt pool.
#
# Usage: scripts/gate-receipt.sh --pr <n>
#        scripts/gate-receipt.sh --reviews-file <plain-text-or-json-fixture>
#        scripts/gate-receipt.sh --fixtures-dir <dir>
#          (reviews.json + issue-comments.json, then threads.json or
#           pull-comments.json; optional valid-targets.json for receipt targets)
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
  jq -c '.[] | {body: (.body // ""), author_association: (.author_association // "NONE"), author: (.author // .user.login // "")}' "$1"
}

combine() {
  printf '%s\n' "$@" | grep -v '^$' || true
}

# Fenced code blocks and inline code are not evidence. Applied to every mode.
clean_stream() {
  awk 'BEGIN { fence = 0 } /^[[:space:]]*```/ { fence = !fence; next } fence == 0' \
    | sed -e 's/`[^`]*`//g'
}

count_markers() {
  clean_stream | grep -Eoi 'CHANGES[-_ ]?NEEDED|DO[-_ ]?NOT[-_ ]?MERGE|P[12][[:space:]]*-?[[:space:]]*Badge|\*\*P[12]\*\*' | wc -l | tr -d ' ' || true
}

count_receipts() {
  clean_stream | grep -Eoi 'fixed@[0-9a-f]{7,}|filed[[:space:]]+#[0-9]+' | wc -l | tr -d ' ' || true
}

# Shared jq definitions. Markers and receipts are extracted only from cleaned
# bodies; receipts must come from an allowed author and not from the finding
# comment; fixed@/filed targets must pass the mode's validation.
jq_defs='
  def clean: gsub("(?s)```.*?```"; "") | gsub("`[^`]*`"; "");
  def mcount: [scan("CHANGES[-_ ]?NEEDED|DO[-_ ]?NOT[-_ ]?MERGE|P[12][[:space:]]*-?[[:space:]]*Badge|\\*\\*P[12]\\*\\*"; "i")] | length;
  def ucount: [scan("ruled:"; "i")] | length;
  def maint: (. == "OWNER") or (. == "MEMBER") or (. == "COLLABORATOR");
  def receipt_ok:
    if test("^fixed@"; "i") then
      (ascii_downcase | .[6:]) as $s | ($ok_shas | index($s)) != null
    else
      (capture("filed[[:space:]]+#(?<n>[0-9]+)"; "i").n | tonumber) as $n | ($ok_issues | index($n)) != null
    end;
  def rcount:
    [scan("fixed@[0-9a-f]{7,}|filed[[:space:]]+#[0-9]+"; "i")
     | select(if $validate == 1 then receipt_ok else true end)] | length;
  def allowed:
    (if $mode == "pr" then
       (.author_association == "OWNER" or .author_association == "MEMBER" or .author_association == "COLLABORATOR"
        or ((.author // "") != "" and (.author // "") == $pr_author))
     else (.author_association != "NONE") end)
    and (.author_association != "NONE");
  def cstats($same_comment):
    . as $c
    | ($c.body | clean | mcount) as $m
    | (if ((($same_comment | not) or ($m == 0)) and ($c | allowed))
       then ($c.body | clean | rcount) else 0 end) as $f
    | (if (($c | allowed) and ($c.author_association | maint))
       then ($c.body | clean | ucount) else 0 end) as $u
    | {markers: $m, receipts: ($f + $u)};
'

jq_args=()

set_jq_args() {
  jq_args=(--arg mode "$mode" --arg pr_author "$pr_author" --argjson validate "$validate" \
           --argjson ok_shas "$ok_shas" --argjson ok_issues "$ok_issues")
}

# Per-thread accounting: every thread, resolved or not, must receipt its own
# markers. A resolved thread without a receipt stays outstanding.
thread_stats() {
  jq -c "${jq_args[@]}" "$jq_defs"'
    [ .[] | (.comments | map(cstats(true))) as $cs
      | ($cs | map(.markers) | add // 0) as $m
      | ($cs | map(.receipts) | add // 0) as $r
      | {markers: $m, receipts: $r, outstanding: ([$m - $r, 0] | max)} ]
    | {markers: (map(.markers) | add // 0), receipts: (map(.receipts) | add // 0), outstanding: (map(.outstanding) | add // 0)}
  '
}

pool_stats() {
  jq -cs "${jq_args[@]}" --argjson same_comment "${1:-true}" "$jq_defs"'
    [ .[] | cstats($same_comment) ]
    | {markers: (map(.markers) | add // 0), receipts: (map(.receipts) | add // 0)}
    | . + {outstanding: ([.markers - .receipts, 0] | max)}
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
        comments: ((if (.comments | type) == "object" then (.comments.nodes // []) else (.comments // []) end)
                   | map({body: (.body // ""), author_association: (.author_association // "NONE"), author: (.author // "")}))
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
            comments(first: 100) { totalCount nodes { body authorAssociation author { login } } }
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
mode="legacy"
pr_author=""
validate=0
ok_shas="[]"
ok_issues="[]"
head_sha=""
repo=""

if [ -n "$fixture" ]; then
  [ -f "$fixture" ] || { echo "no such fixture: $fixture" >&2; exit 2; }
  if jq -e . "$fixture" >/dev/null 2>&1; then
    marker_entries=$(json_entries "$fixture")
  else
    marker_entries=$(jq -cn --rawfile body "$fixture" '{body:$body, author_association:"OWNER", author:""}')
  fi
  receipt_entries=$marker_entries
  echo "gate-receipt: legacy --reviews-file mode: receipt authorship enforced; commit/issue targets not validated" >&2
elif [ -n "$fixtures_dir" ]; then
  mode=fixture
  [ -d "$fixtures_dir" ] || { echo "no such fixture directory: $fixtures_dir" >&2; exit 2; }
  for f in reviews.json issue-comments.json; do
    [ -f "$fixtures_dir/$f" ] || { echo "missing $fixtures_dir/$f" >&2; exit 2; }
    require_comment_array "$fixtures_dir/$f" || exit 2
  done
  reviews_entries=$(json_entries "$fixtures_dir/reviews.json")
  comment_entries=$(json_entries "$fixtures_dir/issue-comments.json")
  if [ -f "$fixtures_dir/valid-targets.json" ]; then
    validate=1
    ok_shas=$(jq -c '(.shas // []) | map(ascii_downcase)' "$fixtures_dir/valid-targets.json" 2>/dev/null) || {
      echo "gate-receipt: $fixtures_dir/valid-targets.json must contain {shas: [..], issues: [..]}" >&2
      exit 2
    }
    ok_issues=$(jq -c '.issues // []' "$fixtures_dir/valid-targets.json" 2>/dev/null) || {
      echo "gate-receipt: $fixtures_dir/valid-targets.json must contain {shas: [..], issues: [..]}" >&2
      exit 2
    }
    echo "gate-receipt: fixture mode: receipt targets validated against valid-targets.json" >&2
  else
    echo "gate-receipt: fixture mode: receipt authorship enforced; commit/issue targets not validated" >&2
  fi
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
  mode=pr
  [ -n "$pr" ] || { echo "usage: gate-receipt.sh --pr <n> | --reviews-file <path> | --fixtures-dir <path>" >&2; exit 2; }
  repo=${GATE_RECEIPT_REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner)}
  owner=${repo%%/*}
  name=${repo#*/}
  meta=$(gh api "repos/$repo/pulls/$pr" --jq '{author: (.user.login // ""), head: (.head.sha // "")}') || {
    echo "gate-receipt: cannot fetch PR #$pr" >&2
    exit 1
  }
  pr_author=$(printf '%s' "$meta" | jq -r '.author')
  head_sha=$(printf '%s' "$meta" | jq -r '.head')
  # Reviews and comments only. The PR body often describes the gate itself
  # ("one receipt per CHANGES-NEEDED marker") and must not count as a marker.
  # Fail closed: unavailable evidence is not clearance.
  reviews_entries=$(gh api "repos/$repo/pulls/$pr/reviews" --paginate --jq '.[] | {body:(.body // ""), author_association:(.author_association // "NONE"), author:(.user.login // "")}') || {
    echo "gate-receipt: cannot fetch reviews for #$pr" >&2
    exit 1
  }
  comment_entries=$(gh api "repos/$repo/issues/$pr/comments" --paginate --jq '.[] | {body:(.body // ""), author_association:(.author_association // "NONE"), author:(.user.login // "")}') || {
    echo "gate-receipt: cannot fetch comments for #$pr" >&2
    exit 1
  }
  # Codex P1/P2 findings live in inline review comments. Page both threads and
  # their comments until exhausted; a truncated list is refused rather than
  # silently under-counted.
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
           comments: [.comments.nodes[] | {
             body: (.body // ""),
             author_association: (.authorAssociation // "NONE"),
             author: ((.author // {}) | .login // "")
           }]
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
    pull_entries=$(gh api "repos/$repo/pulls/$pr/comments" --paginate --jq '.[] | {body:(.body // ""), author_association:(.author_association // "NONE"), author:(.user.login // "")}') || {
      echo "gate-receipt: cannot fetch inline review comments for #$pr" >&2
      exit 1
    }
    thread_mode=fallback
  fi

  # Validate every fixed@/filed target once: the commit must exist and be the
  # head or an ancestor; the issue number must exist in this repository.
  validate=1
  receipt_text=$(
    printf '%s\n' "$reviews_entries" "$comment_entries" "$pull_entries" | jq -r '.body' 2>/dev/null || true
    printf '%s' "$threads_json" | jq -r '.[].comments[].body' 2>/dev/null || true
  )
  for sha in $(printf '%s\n' "$receipt_text" | clean_stream | grep -Eoi 'fixed@[0-9a-f]{7,}' | sed 's/^fixed@//' | tr '[:upper:]' '[:lower:]' | sort -u); do
    if ! gh api "repos/$repo/commits/$sha" --jq .sha >/dev/null 2>&1; then
      echo "gate-receipt: ignoring fixed@$sha (no such commit in $repo)" >&2
      continue
    fi
    status=$(gh api "repos/$repo/compare/$sha...$head_sha" --jq .status 2>/dev/null || true)
    case "$status" in
      ahead|identical) ok_shas=$(printf '%s' "$ok_shas" | jq -c --arg s "$sha" '. + [$s]') ;;
      *) echo "gate-receipt: ignoring fixed@$sha (not the PR head or an ancestor of it)" >&2 ;;
    esac
  done
  for n in $(printf '%s\n' "$receipt_text" | clean_stream | grep -Eoi 'filed[[:space:]]+#[0-9]+' | grep -Eo '[0-9]+' | sort -u); do
    if gh api "repos/$repo/issues/$n" >/dev/null 2>&1; then
      ok_issues=$(printf '%s' "$ok_issues" | jq -c --argjson n "$n" '. + [$n]')
    else
      echo "gate-receipt: ignoring filed #$n (no such issue in $repo)" >&2
    fi
  done
fi

if [ "$thread_mode" = "threads" ]; then
  top_marker_entries=$(combine "$reviews_entries" "$comment_entries")
  set_jq_args
  stats=$(printf '%s' "$threads_json" | thread_stats)
  thread_markers=$(printf '%s' "$stats" | jq -r '.markers')
  thread_receipts=$(printf '%s' "$stats" | jq -r '.receipts')
  thread_outstanding=$(printf '%s' "$stats" | jq -r '.outstanding')
  top=$(printf '%s\n' "$top_marker_entries" | pool_stats true)
  top_markers=$(printf '%s' "$top" | jq -r '.markers')
  top_receipts=$(printf '%s' "$top" | jq -r '.receipts')
  top_outstanding=$(printf '%s' "$top" | jq -r '.outstanding')
  markers=$((top_markers + thread_markers))
  receipts=$((top_receipts + thread_receipts))
  outstanding=$((top_outstanding + thread_outstanding))
elif [ "$thread_mode" = "fallback" ]; then
  marker_entries=$(combine "$reviews_entries" "$comment_entries" "$pull_entries")
  echo "gate-receipt: review-thread resolution unavailable; counting every inline finding (global receipt pool, may over-gate)" >&2
  set_jq_args
  pool=$(printf '%s\n' "$marker_entries" | pool_stats true)
  markers=$(printf '%s' "$pool" | jq -r '.markers')
  receipts=$(printf '%s' "$pool" | jq -r '.receipts')
  outstanding=$(printf '%s' "$pool" | jq -r '.outstanding')
else
  set_jq_args
  pool=$(printf '%s\n' "$marker_entries" | pool_stats false)
  markers=$(printf '%s' "$pool" | jq -r '.markers')
  receipts=$(printf '%s' "$pool" | jq -r '.receipts')
  outstanding=$(printf '%s' "$pool" | jq -r '.outstanding')
fi

if [ "$outstanding" -eq 0 ] && [ "$markers" -eq 0 ]; then
  echo "gate-receipt: no gating marker"
  exit 0
fi
if [ "$outstanding" -gt 0 ]; then
  echo "gate-receipt: $outstanding outstanding finding(s) without a receipt; $markers marker(s) seen, $receipts receipt(s)" >&2
  echo "each marker needs its own fixed@<sha>, filed #<issue>, or maintainer ruled: <reason> from the PR author or a maintainer in its own thread or body" >&2
  exit 1
fi
echo "gate-receipt: $markers marker(s), $receipts receipt(s) — cleared"
