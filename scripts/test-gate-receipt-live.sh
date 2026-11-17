#!/usr/bin/env bash
# gate-receipt live-path harness: drives --pr mode against a stubbed `gh`.
#
# The stub answers every endpoint from files in a scenario directory, records
# each invocation's argv, and never touches the network: the stub is the only
# `gh` on PATH and it never calls out. The accounting tail is covered by
# scripts/test-gate-receipt.sh; this harness covers the fetch layer, which
# fixtures cannot reach.
#
# Covered: the exact argv of every fetch, GraphQL reviewThreads pagination,
# truncation refusal, the 50-page cap, per-endpoint fetch failures, the
# receipt-target validation path, the repository override, and the fail-closed
# fallback taken when GraphQL is unavailable.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

stubdir="$tmp/bin"
mkdir -p "$stubdir"

cat > "$stubdir/gh" <<'STUB'
#!/usr/bin/env bash
# Stubbed gh: answers from GH_STUB_DIR, records argv to GH_STUB_LOG, and
# never touches the network.
set -uo pipefail
log=${GH_STUB_LOG:?GH_STUB_LOG is required}
dir=${GH_STUB_DIR:?GH_STUB_DIR is required}

# One record per call: the args joined by a unit separator, newlines escaped.
{
  printf 'CALL'
  for a in "$@"; do printf '\037%s' "${a//$'\n'/\\n}"; done
  printf '\n'
} >> "$log"

jq_filter=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--jq" ]; then jq_filter=$a; fi
  prev=$a
done

fails() {
  [ -f "$dir/fail" ] && grep -Fxq "$1" "$dir/fail"
}

emit() {
  local file=$1
  if [ ! -f "$file" ]; then
    echo "gh: not found: $file" >&2
    exit 1
  fi
  if [ -n "$jq_filter" ]; then
    jq -r "$jq_filter" "$file"
  else
    cat "$file"
  fi
}

cmd=${1:-}
if [ $# -gt 0 ]; then shift; fi

case "$cmd" in
  repo)
    if fails repo; then
      echo "gh: repo view failed" >&2
      exit 1
    fi
    emit "$dir/repo-view.json"
    ;;
  api)
    endpoint=${1:-}
    if [ $# -gt 0 ]; then shift; fi
    case "$endpoint" in
      graphql)
        if fails graphql; then
          echo "gh: graphql failed" >&2
          exit 1
        fi
        cursor=""
        prev=""
        for a in "$@"; do
          if [ "$prev" = "-F" ]; then
            case "$a" in endCursor=*) cursor=${a#endCursor=} ;; esac
          fi
          prev=$a
        done
        page=$(jq -r --arg c "$cursor" '.[$c] // empty' "$dir/graphql-map.json" 2>/dev/null || true)
        if [ -z "$page" ]; then
          echo "gh: no graphql page for cursor '$cursor'" >&2
          exit 1
        fi
        emit "$dir/$page"
        ;;
      repos/*)
        rest=${endpoint#repos/*}
        rest=${rest#*/}   # owner
        rest=${rest#*/}   # name
        key=${rest//\//-}
        if fails "$key"; then
          echo "gh: request failed: $endpoint" >&2
          exit 1
        fi
        emit "$dir/api/$key.json"
        ;;
      *)
        echo "gh: unsupported endpoint: $endpoint" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "gh: unsupported command: $cmd" >&2
    exit 1
    ;;
esac
STUB
chmod +x "$stubdir/gh"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

rc=0
run_gate() {
  # run_gate <scenario-dir> [gate-receipt args...]
  local dir=$1
  shift
  : > "$dir/gh.log"
  set +e
  PATH="$stubdir:$PATH" GH_STUB_DIR="$dir" GH_STUB_LOG="$dir/gh.log" \
    bash "$root/scripts/gate-receipt.sh" "$@" > "$dir/stdout.txt" 2> "$dir/stderr.txt"
  rc=$?
  set -e
}

expect_rc() {
  # expect_rc <expected> <scenario-dir>
  if [ "$rc" -ne "$1" ]; then
    fail "expected exit $1, got $rc; stderr: $(cat "$2/stderr.txt")"
  fi
}

expect_stderr() {
  # expect_stderr <needle> <scenario-dir>
  case "$(cat "$2/stderr.txt")" in
    *"$1"*) : ;;
    *) fail "stderr does not name '$1': $(cat "$2/stderr.txt")" ;;
  esac
}

expect_stdout() {
  # expect_stdout <needle> <scenario-dir>
  case "$(cat "$2/stdout.txt")" in
    *"$1"*) : ;;
    *) fail "stdout does not name '$1': $(cat "$2/stdout.txt")" ;;
  esac
}

expect_no_stderr() {
  # expect_no_stderr <needle> <scenario-dir>
  case "$(cat "$2/stderr.txt")" in
    *"$1"*) fail "stderr must not name '$1': $(cat "$2/stderr.txt")" ;;
    *) : ;;
  esac
}

call_count() {
  grep -c '^CALL' "$1" || true
}

expect_call() {
  # expect_call <log> <line-number> <expected-arg>...
  local log=$1 line=$2
  shift 2
  local expected="" a
  for a in "$@"; do expected+=$'\037'"$a"; done
  local actual
  actual=$(sed -n "${line}p" "$log")
  actual=${actual#CALL}
  if [ "$actual" != "$expected" ]; then
    fail "gh call $line argv mismatch
  expected: $expected
  actual:   $actual"
  fi
}

expect_graphql_call() {
  # expect_graphql_call <log> <line-number> <cursor|none>
  #
  # The GraphQL call fetches the raw envelope: the script validates the page
  # shape itself, so no --jq filter travels with the request.
  local log=$1 line=$2 cursor=$3
  local rec args
  rec=$(sed -n "${line}p" "$log")
  rec=${rec#CALL}
  rec=${rec#$'\037'}
  IFS=$'\037' read -r -a args <<< "$rec"
  local want=10
  [ "$cursor" = "none" ] || want=12
  if [ "${#args[@]}" -ne "$want" ]; then
    fail "graphql call $line has ${#args[@]} args, expected $want"
  fi
  [ "${args[0]}" = "api" ] || fail "graphql call $line arg 0: ${args[0]}"
  [ "${args[1]}" = "graphql" ] || fail "graphql call $line arg 1: ${args[1]}"
  [ "${args[2]}" = "-f" ] || fail "graphql call $line arg 2: ${args[2]}"
  case "${args[3]}" in
    query=*reviewThreads*) : ;;
    *) fail "graphql call $line does not query reviewThreads: ${args[3]}" ;;
  esac
  [ "${args[4]}" = "-F" ] || fail "graphql call $line arg 4: ${args[4]}"
  [ "${args[5]}" = "owner=Cognilogical" ] || fail "graphql call $line arg 5: ${args[5]}"
  [ "${args[6]}" = "-F" ] || fail "graphql call $line arg 6: ${args[6]}"
  [ "${args[7]}" = "name=NeuroStrata" ] || fail "graphql call $line arg 7: ${args[7]}"
  [ "${args[8]}" = "-F" ] || fail "graphql call $line arg 8: ${args[8]}"
  [ "${args[9]}" = "pr=123" ] || fail "graphql call $line arg 9: ${args[9]}"
  if [ "$cursor" = "none" ]; then
    return 0
  fi
  [ "${args[10]}" = "-F" ] || fail "graphql call $line arg 10: ${args[10]}"
  [ "${args[11]}" = "endCursor=$cursor" ] || fail "graphql call $line arg 11: ${args[11]}"
}

# ---------------------------------------------------------------------------
# Scenario helpers
# ---------------------------------------------------------------------------

new_scenario() {
  local d="$tmp/$1"
  mkdir -p "$d/api"
  printf '%s\n' "$d"
}

standard_api() {
  # standard_api <scenario-dir>
  local d=$1
  cat > "$d/api/pulls-123.json" <<'JSON'
{"user":{"login":"pr-author"},"head":{"sha":"headsha1"}}
JSON
  cat > "$d/api/pulls-123-commits.json" <<'JSON'
[{"author":{"login":"pr-author"},"committer":{"login":"pr-author"}}]
JSON
  cat > "$d/api/pulls-123-reviews.json" <<'JSON'
[{"body":"CHANGES-NEEDED","author_association":"MEMBER","user":{"login":"maintainer"}}]
JSON
  cat > "$d/api/issues-123-comments.json" <<'JSON'
[{"body":"filed #207","author_association":"MEMBER","user":{"login":"maintainer"}}]
JSON
  cat > "$d/api/pulls-123-comments.json" <<'JSON'
[]
JSON
  cat > "$d/api/issues-207.json" <<'JSON'
{"number":207,"title":"tracked finding"}
JSON
  cat > "$d/api/commits-abc1234.json" <<'JSON'
{"sha":"abc1234abc1234abc1234abc1234abc1234"}
JSON
  cat > "$d/api/compare-abc1234...headsha1.json" <<'JSON'
{"status":"ahead"}
JSON
}

# One review thread: a P1 badge finding and a maintainer reply carrying both
# receipt forms.
threads_page() {
  cat <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{
  "pageInfo":{"hasNextPage":false,"endCursor":null},
  "nodes":[{"isResolved":false,"comments":{"totalCount":2,"nodes":[
    {"body":"**P1 Badge** a finding","authorAssociation":"NONE","author":{"login":"bot"}},
    {"body":"fixed@abc1234\nfiled #207","authorAssociation":"MEMBER","author":{"login":"maintainer"}}
  ]}}]}}}}}
JSON
}

# ---------------------------------------------------------------------------
# 1. Happy path: one GraphQL page, exact argv for every fetch.
# ---------------------------------------------------------------------------
d=$(new_scenario happy)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
threads_page > "$d/graphql-1.json"
run_gate "$d" --pr 123
expect_rc 0 "$d"
expect_stdout "cleared" "$d"
log="$d/gh.log"
expect_call "$log" 1 "repo" "view" "--json" "nameWithOwner" "-q" ".nameWithOwner"
expect_call "$log" 2 "api" "repos/Cognilogical/NeuroStrata/pulls/123" "--jq" \
  '{author: (.user.login // ""), head: (.head.sha // "")}'
expect_call "$log" 3 "api" "repos/Cognilogical/NeuroStrata/pulls/123/commits" "--paginate" "--jq" \
  '[.[].author.login // empty, .[].committer.login // empty]'
expect_call "$log" 4 "api" "repos/Cognilogical/NeuroStrata/pulls/123/reviews" "--paginate" "--jq" \
  '.[] | {body:(.body // ""), author_association:(.author_association // "NONE"), author:(.user.login // "")}'
expect_call "$log" 5 "api" "repos/Cognilogical/NeuroStrata/issues/123/comments" "--paginate" "--jq" \
  '.[] | {body:(.body // ""), author_association:(.author_association // "NONE"), author:(.user.login // "")}'
expect_graphql_call "$log" 6 none
expect_call "$log" 7 "api" "repos/Cognilogical/NeuroStrata/commits/abc1234" "--jq" ".sha"
expect_call "$log" 8 "api" "repos/Cognilogical/NeuroStrata/compare/abc1234...headsha1" "--jq" ".status"
expect_call "$log" 9 "api" "repos/Cognilogical/NeuroStrata/issues/207"
if [ "$(call_count "$log")" -ne 9 ]; then
  fail "the happy path made $(call_count "$log") gh calls, expected 9"
fi

# ---------------------------------------------------------------------------
# 2. Pagination: the second page is fetched with the cursor of the first.
# ---------------------------------------------------------------------------
d=$(new_scenario pagination)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
cat > "$d/graphql-map.json" <<'JSON'
{"": "graphql-1.json", "CUR1": "graphql-2.json"}
JSON
cat > "$d/graphql-1.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{
  "pageInfo":{"hasNextPage":true,"endCursor":"CUR1"},
  "nodes":[{"isResolved":false,"comments":{"totalCount":2,"nodes":[
    {"body":"**P1 Badge** first page finding","authorAssociation":"NONE","author":{"login":"bot"}},
    {"body":"fixed@abc1234","authorAssociation":"MEMBER","author":{"login":"maintainer"}}
  ]}}]}}}}}
JSON
cat > "$d/graphql-2.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{
  "pageInfo":{"hasNextPage":false,"endCursor":null},
  "nodes":[{"isResolved":false,"comments":{"totalCount":2,"nodes":[
    {"body":"**P2 Badge** second page finding","authorAssociation":"NONE","author":{"login":"bot"}},
    {"body":"filed #207","authorAssociation":"MEMBER","author":{"login":"maintainer"}}
  ]}}]}}}}}
JSON
run_gate "$d" --pr 123
expect_rc 0 "$d"
expect_stdout "cleared" "$d"
log="$d/gh.log"
expect_graphql_call "$log" 6 none
expect_graphql_call "$log" 7 "CUR1"
if [ "$(call_count "$log")" -ne 10 ]; then
  fail "the paginated path made $(call_count "$log") gh calls, expected 10"
fi

# ---------------------------------------------------------------------------
# 3. Truncation refusal: comments.totalCount exceeds the comments returned.
# ---------------------------------------------------------------------------
d=$(new_scenario truncated-comments)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
cat > "$d/graphql-1.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{
  "pageInfo":{"hasNextPage":false,"endCursor":null},
  "nodes":[{"isResolved":false,"comments":{"totalCount":5,"nodes":[
    {"body":"**P1 Badge** a finding","authorAssociation":"NONE","author":{"login":"bot"}}
  ]}}]}}}}}
JSON
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "truncated" "$d"

# ---------------------------------------------------------------------------
# 4. The 50-page cap: a page that always claims another page is refused.
# ---------------------------------------------------------------------------
d=$(new_scenario page-cap)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
jq -n '{ "": "graphql-1.json" } + (reduce range(0;60) as $i ({}; .["C" + ($i|tostring)] = "graphql-1.json"))' \
  > "$d/graphql-map.json"
cat > "$d/graphql-1.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{
  "pageInfo":{"hasNextPage":true,"endCursor":"C0"},
  "nodes":[{"isResolved":false,"comments":{"totalCount":1,"nodes":[
    {"body":"**P1 Badge** a finding","authorAssociation":"NONE","author":{"login":"bot"}}
  ]}}]}}}}}
JSON
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "truncated after 50 pages" "$d"
if [ "$(call_count "$d/gh.log")" -ne 55 ]; then
  fail "the page cap made $(call_count "$d/gh.log") gh calls, expected 55 (5 REST + 50 GraphQL)"
fi

# ---------------------------------------------------------------------------
# 5. Fetch failures: every evidence endpoint fails closed.
# ---------------------------------------------------------------------------
for endpoint in pulls-123 pulls-123-reviews issues-123-comments; do
  d=$(new_scenario "fail-$endpoint")
  standard_api "$d"
  printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
  printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
  threads_page > "$d/graphql-1.json"
  printf '%s\n' "$endpoint" > "$d/fail"
  run_gate "$d" --pr 123
  expect_rc 1 "$d"
done
d=$(new_scenario fail-pull-meta)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
threads_page > "$d/graphql-1.json"
printf 'pulls-123\n' > "$d/fail"
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "cannot fetch PR #123" "$d"

# The commits list only widens who may receipt a finding. Losing it narrows
# the accepted receipts (the gate over-gates), so it must degrade, not crash.
d=$(new_scenario fail-commits)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
threads_page > "$d/graphql-1.json"
printf 'pulls-123-commits\n' > "$d/fail"
run_gate "$d" --pr 123
expect_rc 0 "$d"

# ---------------------------------------------------------------------------
# 6. GraphQL unavailable: the fallback is fail-closed on inline findings.
# ---------------------------------------------------------------------------
d=$(new_scenario fallback-inline)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
threads_page > "$d/graphql-1.json"
cat > "$d/api/pulls-123-comments.json" <<'JSON'
[{"body":"**P1 Badge** an inline finding with no bindable receipt","author_association":"NONE","user":{"login":"bot"}}]
JSON
printf 'graphql\n' > "$d/fail"
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "review-thread resolution unavailable" "$d"
expect_stderr "inline finding" "$d"

# The same fallback with no inline finding still accounts the top-level pool,
# and says nothing about a thread it never resolved.
d=$(new_scenario fallback-top-level)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
threads_page > "$d/graphql-1.json"
printf 'graphql\n' > "$d/fail"
run_gate "$d" --pr 123
expect_rc 0 "$d"
expect_stdout "cleared" "$d"
expect_no_stderr "review-thread resolution unavailable" "$d"

# ---------------------------------------------------------------------------
# 6b. A GraphQL page that is not a page.
#
# An HTTP 200 body can still carry GraphQL errors, a partial answer, or a
# malformed page. Such a page must never be accepted as "no findings": the
# script falls back, and the fail-closed fallback then gates the inline
# finding instead of silently under-counting it.
# ---------------------------------------------------------------------------
invalid_page() {
  # invalid_page <scenario-name> <page-body-file-content>
  local name=$1
  d=$(new_scenario "$name")
  standard_api "$d"
  printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
  printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
  cat > "$d/graphql-1.json"
  # An inline finding is present: the fallback must gate it.
  cat > "$d/api/pulls-123-comments.json" <<'JSON'
[{"body":"**P1 Badge** an inline finding with no bindable receipt","author_association":"NONE","user":{"login":"bot"}}]
JSON
  run_gate "$d" --pr 123
  expect_rc 1 "$d"
  expect_stderr "review-thread resolution unavailable" "$d"
  expect_stderr "inline finding" "$d"
}

invalid_page graphql-http200-errors <<'JSON'
{"errors":[{"type":"NOT_FOUND","message":"Could not resolve to a Repository"}],"data":null}
JSON

invalid_page graphql-http200-partial <<'JSON'
{"data":{"repository":null}}
JSON

invalid_page graphql-malformed <<'JSON'
{ this is not json
JSON

invalid_page graphql-empty-body <<'JSON'
{}
JSON

invalid_page graphql-empty-data <<'JSON'
{"data":{}}
JSON

invalid_page graphql-hasnext-not-boolean <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{"pageInfo":{"hasNextPage":"false","endCursor":null},"nodes":[]}}}}}
JSON

invalid_page graphql-hasnext-true-without-cursor <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{"pageInfo":{"hasNextPage":true,"endCursor":null},"nodes":[]}}}}}
JSON

invalid_page graphql-node-without-comments <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"isResolved":false}]}}}}}
JSON

invalid_page graphql-node-without-resolution <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"comments":{"totalCount":0,"nodes":[]}}]}}}}}
JSON

invalid_page graphql-comment-without-body <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"isResolved":false,"comments":{"totalCount":1,"nodes":[{"authorAssociation":"NONE"}]}}]}}}}}
JSON

# The inline fetch failing inside the fallback is a fetch failure, not a
# clearance.
d=$(new_scenario fallback-inline-fetch-failure)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
threads_page > "$d/graphql-1.json"
printf 'graphql\npulls-123-comments\n' > "$d/fail"
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "cannot fetch inline review comments for #123" "$d"

# ---------------------------------------------------------------------------
# 7. Receipt-target validation: only real targets validate.
# ---------------------------------------------------------------------------
# A commit that does not exist is ignored, so the finding stays outstanding.
d=$(new_scenario unknown-commit)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
cat > "$d/graphql-1.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{
  "pageInfo":{"hasNextPage":false,"endCursor":null},
  "nodes":[{"isResolved":false,"comments":{"totalCount":2,"nodes":[
    {"body":"**P1 Badge** a finding","authorAssociation":"NONE","author":{"login":"bot"}},
    {"body":"fixed@abc1234","authorAssociation":"MEMBER","author":{"login":"maintainer"}}
  ]}}]}}}}}
JSON
rm -f "$d/api/commits-abc1234.json"
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "ignoring fixed@abc1234" "$d"

# A commit that exists but is not on the PR history is ignored too.
d=$(new_scenario off-history-commit)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
cat > "$d/graphql-1.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{
  "pageInfo":{"hasNextPage":false,"endCursor":null},
  "nodes":[{"isResolved":false,"comments":{"totalCount":2,"nodes":[
    {"body":"**P1 Badge** a finding","authorAssociation":"NONE","author":{"login":"bot"}},
    {"body":"fixed@abc1234","authorAssociation":"MEMBER","author":{"login":"maintainer"}}
  ]}}]}}}}}
JSON
printf '{"status":"behind"}\n' > "$d/api/compare-abc1234...headsha1.json"
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "not the PR head or an ancestor" "$d"

# A pull request is not an issue: the filed receipt is rejected.
d=$(new_scenario pull-request-receipt)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
cat > "$d/graphql-1.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{
  "pageInfo":{"hasNextPage":false,"endCursor":null},
  "nodes":[{"isResolved":false,"comments":{"totalCount":2,"nodes":[
    {"body":"**P1 Badge** a finding","authorAssociation":"NONE","author":{"login":"bot"}},
    {"body":"filed #207","authorAssociation":"MEMBER","author":{"login":"maintainer"}}
  ]}}]}}}}}
JSON
cat > "$d/api/issues-207.json" <<'JSON'
{"number":207,"title":"a pull request","pull_request":{"url":"https://example.invalid/pr/207"}}
JSON
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "ignoring filed #207" "$d"

# An issue that does not exist is ignored.
d=$(new_scenario unknown-issue)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
cat > "$d/graphql-1.json" <<'JSON'
{"data":{"repository":{"pullRequest":{"reviewThreads":{
  "pageInfo":{"hasNextPage":false,"endCursor":null},
  "nodes":[{"isResolved":false,"comments":{"totalCount":2,"nodes":[
    {"body":"**P1 Badge** a finding","authorAssociation":"NONE","author":{"login":"bot"}},
    {"body":"filed #999","authorAssociation":"MEMBER","author":{"login":"maintainer"}}
  ]}}]}}}}}
JSON
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "ignoring filed #999" "$d"

# ---------------------------------------------------------------------------
# 8. GATE_RECEIPT_REPO: the repository override still decides every call.
# ---------------------------------------------------------------------------
d=$(new_scenario repo-override)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf '{"": "graphql-1.json"}\n' > "$d/graphql-map.json"
threads_page > "$d/graphql-1.json"
set +e
PATH="$stubdir:$PATH" GH_STUB_DIR="$d" GH_STUB_LOG="$d/gh.log" GATE_RECEIPT_REPO="other/repo" \
  bash "$root/scripts/gate-receipt.sh" --pr 123 > "$d/stdout.txt" 2> "$d/stderr.txt"
rc=$?
set -e
expect_rc 0 "$d"
log="$d/gh.log"
if grep -q '^CALL.\037repo' "$log"; then
  fail "the repository override must not ask gh for the repository"
fi
expect_call "$log" 1 "api" "repos/other/repo/pulls/123" "--jq" \
  '{author: (.user.login // ""), head: (.head.sha // "")}'

# The repository lookup failing is a fetch failure, not a clearance.
d=$(new_scenario repo-view-failure)
standard_api "$d"
printf 'Cognilogical/NeuroStrata\n' > "$d/repo-view.json"
printf 'repo\n' > "$d/fail"
run_gate "$d" --pr 123
expect_rc 1 "$d"
expect_stderr "cannot resolve the repository" "$d"

echo "gate-receipt live harness: PASS"
