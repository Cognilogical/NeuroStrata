#!/usr/bin/env bash
# gate-receipt: fail a PR when a gating review marker has no recorded receipt.
#
# A gating marker is CHANGES-NEEDED / CHANGES NEEDED / DO-NOT-MERGE / DO NOT MERGE,
# a P1/P2 finding badge (P1 Badge / P1-Badge / shields.io), or a bold **P1** /
# **P2**. Fenced blocks (``` and ~~~, any run, unclosed to EOF), indented code
# lines, and inline code spans of one or more backticks are stripped before any
# marker or receipt is read.
#
# Inline findings come from review threads when GraphQL answers. If it does not,
# the gate fails closed: an inline finding cannot be bound to a receipt without
# the thread it belongs to, so the script exits 1 instead of pooling receipts
# across comments.
#
# Receipts: fixed@<sha> and filed #<issue> from the PR author or a maintainer
# (OWNER / MEMBER / COLLABORATOR), never from an association-less account, and
# never from the comment that carries the finding. ruled: <reason> is the one
# receipt allowed in the finding's own body; it is maintainer-only. In --pr mode
# fixed@ is a commit on the PR history and filed # must exist as an issue (a
# pull-request number does not count); fixture and legacy modes enforce
# authorship but cannot validate targets.
#
# Accounting: each review thread (resolved or not) must carry receipts for its
# own markers, and top-level review/comment bodies form a separate pool. One
# tokenizer and one marker regex serve every mode, so the thread, top-level,
# fallback, and legacy engines count the same body the same way. Resolution
# alone never clears a finding. The legacy --reviews-file mode keeps the older
# global, same-body receipt pool.
#
# Usage: scripts/gate-receipt.sh --pr <n>
#        scripts/gate-receipt.sh --reviews-file <plain-text-or-json-fixture>
#        scripts/gate-receipt.sh --fixtures-dir <dir>
#          (reviews.json + issue-comments.json, then threads.json or
#           pull-comments.json; optional valid-targets.json for receipt targets)
#        GATE_RECEIPT_REPO=owner/name overrides the repo used by --pr (useful
#        when a fork checkout wants to check the upstream PR).
set -euo pipefail

# One marker regex is the engine, used by every mode. The accepted forms are
# CHANGES-NEEDED / CHANGES NEEDED / CHANGES_NEEDED, DO-NOT-MERGE / DO NOT MERGE
# / DO_NOT_MERGE, the badge forms P1 Badge / P1-Badge / P1Badge (and the P2
# equivalents), and the bold **P1** / **P2**. The underscore is not a badge
# separator, and P0 never gates. scripts/test-gate-receipt.sh pins this regex
# against the documented surface in docs/features-api/ops/merge-gates.md.
marker_re='CHANGES[-_ ]?NEEDED|DO[-_ ]?NOT[-_ ]?MERGE|P[12][[:space:]]*-?[[:space:]]*Badge|\*\*P[12]\*\*'

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
# The trap must not change the exit status: `cmd && cmd` as the last
# statement of a trap would exit non-zero when tmpdir is empty.
trap 'if [ -n "$tmpdir" ]; then rm -rf "$tmpdir"; fi' EXIT

json_entries() {
  jq -c '.[] | {body: (.body // ""), author_association: (.author_association // "NONE"), author: (.author // .user.login // "")}' "$1"
}

combine() {
  printf '%s\n' "$@" | grep -v '^$' || true
}

# One tokenizer, used by every mode. It drops fenced blocks (``` and ~~~, any
# run length, unclosed to EOF), 4-space/tab indented lines, and inline code
# spans delimited by one or more backticks (longest opening run wins).
# shellcheck disable=SC2016  # jq program: $vars are jq identifiers, not shell
tokenizer_def='
  def clean:
    (split("\n")) as $lines
    | (reduce $lines[] as $line ({ch: "", n: 0, out: []};
        if .ch != "" then
          if (($line | test("^ {0,3}(`{3,}|~{3,})[ ]*$"))
              and ((($line | capture("^ {0,3}(?<f>`{3,}|~{3,})[ ]*$").f) as $f
                    | (($f[0:1]) == .ch) and (($f | length) >= .n)))) then
            .ch = "" | .n = 0
          else . end
        elif ($line | test("^ {0,3}(`{3,}|~{3,})")) then
          (($line | capture("^ {0,3}(?<f>`{3,}|~{3,})").f) as $f
           | .ch = ($f[0:1]) | .n = ($f | length))
        elif ($line | test("^(    |\\t)")) then .
        else .out += [$line] end))
    | .out | join("\n")
    | gsub("(?<!`)(`+)(?!`)(.*?)(?<!`)\\1(?!`)"; ""; "m");
'
# Normalize fixed@ receipt targets to lowercase SHAs. The grep match is
# case-insensitive, so the prefix strip must be too: lowercase first, then
# strip, or FIXED@<sha> survives with its prefix and never validates.
fixed_sha_targets() {
  grep -Eoi 'fixed@[0-9a-f]{7,}' | tr '[:upper:]' '[:lower:]' | sed 's/^fixed@//' | sort -u
}

# Shared jq definitions. Markers and receipts are extracted only from cleaned
# bodies; receipts must come from an allowed author and not from the finding
# comment; fixed@/filed targets must pass the mode's validation.
# shellcheck disable=SC2016  # jq program: $vars are jq identifiers, not shell
jq_defs="${tokenizer_def}"'
  def mcount: [scan($marker_re; "i")] | length;
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
    (if ($mode == "pr" or ($pr_authors | length) > 0) then
       (.author_association == "OWNER" or .author_association == "MEMBER" or .author_association == "COLLABORATOR"
        or ((.author // "") as $a | $a != "" and (($pr_authors | index($a)) != null)))
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
  jq_args=(--arg mode "$mode" --arg marker_re "$marker_re" --argjson pr_authors "$pr_authors" --argjson validate "$validate" \
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

# shellcheck disable=SC2016  # GraphQL query: $vars bind in GraphQL, not shell
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

# A reviewThreads page is accepted only when its shape is complete. An HTTP
# 200 body can carry GraphQL errors, a partial answer, or a page that is not a
# page at all; reading such a page as "no findings" would under-count the
# findings and over-clear the gate. A page that fails this guard is refused and
# the fail-closed fallback takes over.
threads_page_guard='
  def thread_ok:
    (type == "object")
    and (has("isResolved"))
    and ((.isResolved | type) == "boolean")
    and (has("comments"))
    and ((.comments | type) == "object")
    and ((.comments.totalCount | type) == "number")
    and ((.comments.nodes | type) == "array")
    and (all(.comments.nodes[]; (type == "object") and ((.body | type) == "string")));
  def page_info_ok:
    (type == "object")
    and (has("hasNextPage"))
    and ((.hasNextPage | type) == "boolean")
    and (((.endCursor | type) == "string") or (.endCursor == null))
    and (if .hasNextPage then ((.endCursor | type) == "string") and ((.endCursor | length) > 0) else true end);
  (type == "object")
  and (((.errors // []) | type) == "array")
  and (((.errors // []) | length) == 0)
  and ((.data | type) == "object")
  and ((.data.repository | type) == "object")
  and ((.data.repository.pullRequest | type) == "object")
  and ((.data.repository.pullRequest.reviewThreads | type) == "object")
  and (.data.repository.pullRequest.reviewThreads.pageInfo | page_info_ok)
  and ((.data.repository.pullRequest.reviewThreads.nodes | type) == "array")
  and (all(.data.repository.pullRequest.reviewThreads.nodes[]; thread_ok))
'

# The projection the accounting uses, applied only to a page that passed the
# guard above.
threads_page_filter='
  .data.repository.pullRequest.reviewThreads
  | {hasNextPage: .pageInfo.hasNextPage,
     endCursor: (.pageInfo.endCursor // ""),
     nodes: [.nodes[] | {
       isResolved: .isResolved,
       commentsTotal: .comments.totalCount,
       comments: [.comments.nodes[] | {
         body: .body,
         author_association: (.authorAssociation // "NONE"),
         author: ((.author // {}) | .login // "")
       }]
     }]}
'

reviews_entries=""
comment_entries=""
top_marker_entries=""
threads_json=""
pull_entries=""
thread_mode=""
marker_entries=""
mode="legacy"
pr_author=""
pr_authors="[]"
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
    ok_issues=$(jq -c '(.issues // []) - (.pull_requests // [])' "$fixtures_dir/valid-targets.json" 2>/dev/null) || {
      echo "gate-receipt: $fixtures_dir/valid-targets.json must contain {shas: [..], issues: [..], pull_requests?: [..]}" >&2
      exit 2
    }
    pr_authors=$(jq -ce 'if has("pr_authors") then (if (.pr_authors | type) == "array" and all(.pr_authors[]; type == "string") then .pr_authors else error("bad pr_authors") end) else [] end' "$fixtures_dir/valid-targets.json" 2>/dev/null) || {
      echo "gate-receipt: $fixtures_dir/valid-targets.json pr_authors must be an array of strings" >&2
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
  mode="pr"
  [ -n "$pr" ] || { echo "usage: gate-receipt.sh --pr <n> | --reviews-file <path> | --fixtures-dir <path>" >&2; exit 2; }
  # The repository may be overridden (a fork checkout checks the upstream PR).
  # An unresolvable repository is a fetch failure, not a clearance.
  if [ -n "${GATE_RECEIPT_REPO:-}" ]; then
    repo=$GATE_RECEIPT_REPO
  else
    repo=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null) || repo=""
  fi
  if [ -z "$repo" ]; then
    echo "gate-receipt: cannot resolve the repository" >&2
    exit 1
  fi
  owner=${repo%%/*}
  name=${repo#*/}
  meta=$(gh api "repos/$repo/pulls/$pr" --jq '{author: (.user.login // ""), head: (.head.sha // "")}') || {
    echo "gate-receipt: cannot fetch PR #$pr" >&2
    exit 1
  }
  pr_author=$(printf '%s' "$meta" | jq -r '.author')
  head_sha=$(printf '%s' "$meta" | jq -r '.head')
  # GITHUB_TOKEN in Actions lacks read:org, so private org members receive
  # author_association CONTRIBUTOR instead of MEMBER. Include PR commit
  # authors/committers alongside the PR creator so co-authors who push a fix
  # commit to the PR branch can receipt it.
  # A lost commits list only narrows who may receipt a finding, so the gate
  # over-gates instead of crashing: an unreadable list is treated as empty.
  commit_authors=$(gh api "repos/$repo/pulls/$pr/commits" --paginate --jq '[.[].author.login // empty, .[].committer.login // empty]' 2>/dev/null | jq -sc 'add // []' 2>/dev/null) || commit_authors=""
  if ! printf '%s' "$commit_authors" | jq -e 'type == "array"' >/dev/null 2>&1; then
    commit_authors='[]'
  fi
  pr_authors=$(jq -cn --arg a "$pr_author" --argjson ca "$commit_authors" '([$a] + $ca) | map(select(. != "")) | unique')
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
    # The raw envelope is fetched, not a filtered projection: an HTTP 200 body
    # can still carry GraphQL errors, a partial answer, or a malformed page,
    # and a page that is not a page must never be read as "no findings".
    raw=$(gh api graphql "${gql_args[@]}" 2>/dev/null) || { threads_ok=0; break; }
    if ! printf '%s' "$raw" | jq -e "$threads_page_guard" >/dev/null 2>&1; then
      threads_ok=0
      break
    fi
    page=$(printf '%s' "$raw" | jq "$threads_page_filter")
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
  # Clean each body independently: an unclosed fence in one comment must not
  # swallow receipt targets in a later comment.
  receipt_text=$(
    printf '%s\n' "$reviews_entries" "$comment_entries" "$pull_entries" | jq -r "${tokenizer_def}"' .body | clean' 2>/dev/null || true
    printf '%s' "$threads_json" | jq -r "${tokenizer_def}"' .[].comments[].body | clean' 2>/dev/null || true
  )
  for sha in $(printf '%s\n' "$receipt_text" | fixed_sha_targets); do
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
  for n in $(printf '%s\n' "$receipt_text" | grep -Eoi 'filed[[:space:]]+#[0-9]+' | grep -Eo '[0-9]+' | sort -u); do
    # The issues endpoint also resolves pull requests; a filed receipt must
    # name an issue, so an object carrying a pull_request key is rejected.
    if issue_obj=$(gh api "repos/$repo/issues/$n" 2>/dev/null) \
       && ! printf '%s' "$issue_obj" | jq -e 'has("pull_request")' >/dev/null; then
      ok_issues=$(printf '%s' "$ok_issues" | jq -c --argjson n "$n" '. + [$n]')
    else
      echo "gate-receipt: ignoring filed #$n (no such issue in $repo, or the number is a pull request)" >&2
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
  # Fail closed. Without the threads there is no way to bind a receipt to the
  # finding it answers, so an inline finding is outstanding: pooling receipts
  # across comments let a receipt for one finding clear an unrelated one.
  set_jq_args
  inline=$(printf '%s\n' "$pull_entries" | pool_stats true)
  inline_markers=$(printf '%s' "$inline" | jq -r '.markers')
  if [ "$inline_markers" -gt 0 ]; then
    echo "gate-receipt: review-thread resolution unavailable; an inline finding cannot be bound to its own thread" >&2
    echo "gate-receipt: $inline_markers outstanding finding(s) without a receipt; $inline_markers inline finding(s) cannot be bound to a receipt without their review thread" >&2
    echo "each marker needs its own fixed@<sha>, filed #<issue>, or maintainer ruled: <reason> from the PR author or a maintainer in its own thread or body" >&2
    exit 1
  fi
  # No inline finding: only the top-level pool is accounted.
  marker_entries=$(combine "$reviews_entries" "$comment_entries")
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
