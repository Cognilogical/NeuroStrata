#!/usr/bin/env bash
set -euo pipefail
root=$(git rev-parse --show-toplevel)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

printf 'Merge status: CHANGES-NEEDED\nNo receipt.\n' > "$tmp/fail.txt"
printf 'Merge status: DO-NOT-MERGE as filed.\nfiled #150\n' > "$tmp/pass.txt"
printf 'Codex: no findings.\n' > "$tmp/clean.txt"
printf 'CHANGES-NEEDED\nfixed@deadbeef\nDO-NOT-MERGE\nruled: accepted for this release\n' > "$tmp/two.txt"

cat > "$tmp/outsider-ruling.json" <<'JSON'
[{"author_association":"CONTRIBUTOR","body":"CHANGES-NEEDED\nruled: I accept my own residual"}]
JSON
cat > "$tmp/maintainer-ruling.json" <<'JSON'
[{"author_association":"OWNER","body":"CHANGES-NEEDED\nruled: accepted for this release"}]
JSON
cat > "$tmp/contributor-fixed.json" <<'JSON'
[{"author_association":"CONTRIBUTOR","body":"DO-NOT-MERGE\nfixed@deadbeef"}]
JSON
cat > "$tmp/multi-one-line.json" <<'JSON'
[{"author_association":"CONTRIBUTOR","body":"CHANGES-NEEDED DO-NOT-MERGE\nfixed@deadbeef"}]
JSON
cat > "$tmp/multi-both.json" <<'JSON'
[{"author_association":"CONTRIBUTOR","body":"CHANGES-NEEDED DO-NOT-MERGE\nfixed@deadbeef fixed@cafebabe"}]
JSON

# Bold P1/P2 without the word "Badge", and code/fence stripping.
printf '**P1** error swallowed on retry\nNo receipt.\n' > "$tmp/bold-p1.txt"
printf '**P1** error swallowed on retry\nfiled #207\n' > "$tmp/bold-p1-filed.txt"
printf '**P2** slow query path\nNo receipt.\n' > "$tmp/bold-p2.txt"
printf 'Nothing to see.\n```\n**P1 Badge** hidden finding\nfixed@deadbeef\n```\n' > "$tmp/fenced-only.txt"
printf 'Nothing to see.\n~~~\n**P1 Badge** hidden finding\nfixed@deadbeef\n~~~\n' > "$tmp/tilde-fenced.txt"
printf 'Nothing to see.\n~~~\n**P1 Badge** hidden finding\nfixed@deadbeef\n' > "$tmp/unclosed-fence.txt"
printf '**P1 Badge** real finding\n    fixed@deadbeef\n' > "$tmp/indented-receipt.txt"
printf '    **P1 Badge** indented finding\nfiled #207\n' > "$tmp/indented-marker.txt"
printf '**P1 Badge** real finding\n``fixed@deadbeef``\n' > "$tmp/double-backtick-receipt.txt"
printf '**P1 Badge** real finding\n`fixed@deadbeef`\n' > "$tmp/inline-code-receipt.txt"
printf '**P1 Badge** real finding\n```\nfixed@deadbeef\n```\n' > "$tmp/fenced-receipt.txt"

# Problem badges are gating markers too (#201 merged with P1/P2 threads open).
printf '**P1 Badge** unbounded retry loop in ingest\nNo receipt.\n' > "$tmp/p1-no-receipt.txt"
printf '**P1 Badge** unbounded retry loop in ingest\nfiled #207\n' > "$tmp/p1-filed.txt"
cat > "$tmp/p2-fixed.json" <<'JSON'
[{"author_association":"CONTRIBUTOR","body":"**P2 Badge** racy read of the cursor\nfixed@abcdef1"}]
JSON
cat > "$tmp/p1-p2-both.json" <<'JSON'
[{"author_association":"CONTRIBUTOR","body":"**P1 Badge** first finding\n**P2 Badge** second finding\nfixed@abcdef1 filed #207"}]
JSON
cat > "$tmp/p1-p2-one-receipt.json" <<'JSON'
[{"author_association":"CONTRIBUTOR","body":"**P1 Badge** first finding\n**P2 Badge** second finding\nfiled #207"}]
JSON
cat > "$tmp/bare-p1-badge.txt" <<'EOF'
P1 Badge: error swallowed on retry
filed #207
EOF
cat > "$tmp/codex-finding-marker.json" <<'JSON'
[{"author_association":"CONTRIBUTOR","body":"<!-- codex-code-review-finding:v1 -->\n**P1 Badge** unbounded retry loop\nNo receipt."}]
JSON
cat > "$tmp/codex-finding-filed.json" <<'JSON'
[{"author_association":"CONTRIBUTOR","body":"<!-- codex-code-review-finding:v1 -->\n**P1 Badge** unbounded retry loop\nfiled #207"}]
JSON

if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/fail.txt" >/dev/null 2>&1; then
  echo "FAIL: marker without receipt passed" >&2
  exit 1
fi
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/pass.txt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/clean.txt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/two.txt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/maintainer-ruling.json" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/contributor-fixed.json" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/multi-both.json" >/dev/null
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/multi-one-line.json" >/dev/null 2>&1; then
  echo "FAIL: two markers on one line were cleared by one receipt" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/outsider-ruling.json" >/dev/null 2>&1; then
  echo "FAIL: a non-maintainer ruled: cleared a marker" >&2
  exit 1
fi

# P1/P2 problem badges are gating markers (issue #217).
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/p1-no-receipt.txt" >/dev/null 2>&1; then
  echo "FAIL: P1 Badge without receipt passed" >&2
  exit 1
fi
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/p1-filed.txt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/p2-fixed.json" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/p1-p2-both.json" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/bare-p1-badge.txt" >/dev/null
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/p1-p2-one-receipt.json" >/dev/null 2>&1; then
  echo "FAIL: two P1/P2 badges were cleared by one receipt" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/codex-finding-marker.json" >/dev/null 2>&1; then
  echo "FAIL: codex finding marker with no receipt passed" >&2
  exit 1
fi
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/codex-finding-filed.json" >/dev/null

if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/bold-p1.txt" >/dev/null 2>&1; then
  echo "FAIL: **P1** without receipt passed" >&2
  exit 1
fi
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/bold-p1-filed.txt" >/dev/null
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/bold-p2.txt" >/dev/null 2>&1; then
  echo "FAIL: **P2** without receipt passed" >&2
  exit 1
fi
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/fenced-only.txt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/tilde-fenced.txt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/unclosed-fence.txt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/indented-marker.txt" >/dev/null
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/indented-receipt.txt" >/dev/null 2>&1; then
  echo "FAIL: an indented code line supplied a receipt" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/double-backtick-receipt.txt" >/dev/null 2>&1; then
  echo "FAIL: a double-backtick span supplied a receipt" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/inline-code-receipt.txt" >/dev/null 2>&1; then
  echo "FAIL: a receipt inside inline code cleared a finding" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/fenced-receipt.txt" >/dev/null 2>&1; then
  echo "FAIL: a fenced receipt cleared a finding" >&2
  exit 1
fi

# Fetch-layer fixtures: the shape GitHub actually returns for Codex findings.
fix="$root/scripts/fixtures/gate-receipt"
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/p1-unresolved" >/dev/null 2>&1; then
  echo "FAIL: unresolved P1 review thread without receipt passed" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/p1-resolved" >/dev/null 2>&1; then
  echo "FAIL: resolution alone cleared a P1 finding with no receipt" >&2
  exit 1
fi
bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/p1-resolved-filed" >/dev/null
bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/p1-filed" >/dev/null
bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/envelope-thread-receipt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/quoted-prose" >/dev/null

out=$(bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/pr-201" 2>&1 >/dev/null || true)
case "$out" in
  *"4 outstanding finding(s)"*) : ;;
  *) echo "FAIL: #201-shaped fixture should gate on 4 badges: $out" >&2; exit 1 ;;
esac
out=$(bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/pr-204" 2>&1 >/dev/null || true)
case "$out" in
  *"2 outstanding finding(s)"*) : ;;
  *) echo "FAIL: #204-shaped fixture should gate on 2 badges: $out" >&2; exit 1 ;;
esac
out=$(bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/pr-204-fallback" 2>&1 >/dev/null || true)
case "$out" in
  *"2 outstanding finding(s)"*) : ;;
  *) echo "FAIL: fallback should still count 2 inline badges: $out" >&2; exit 1 ;;
esac
case "$out" in
  *"review-thread resolution unavailable"*) : ;;
  *) echo "FAIL: fallback must announce that threads were not resolved: $out" >&2; exit 1 ;;
esac

# Receipts are bound to their finding, not pooled across threads (#218).
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/resolved-receipt-does-not-clear-an-unresolved-finding" >/dev/null 2>&1; then
  echo "FAIL: a resolved thread's receipt cleared an unresolved finding" >&2
  exit 1
fi
out=$(bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/unresolved-receipt-is-thread-local" 2>&1 >/dev/null || true)
case "$out" in
  *"1 outstanding finding(s)"*) : ;;
  *) echo "FAIL: a receipt in thread A cleared a marker in thread B: $out" >&2; exit 1 ;;
esac
bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/each-unresolved-thread-has-its-own-receipt" >/dev/null
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/top-level-marker-not-cleared-by-thread-receipt" >/dev/null 2>&1; then
  echo "FAIL: a thread receipt cleared a top-level marker" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/thread-marker-not-cleared-by-top-level-receipt" >/dev/null 2>&1; then
  echo "FAIL: a top-level receipt cleared a thread marker" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/quoted-receipt-is-not-a-receipt" >/dev/null 2>&1; then
  echo "FAIL: a receipt quoted inside inline code cleared a finding" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/self-receipt-in-finding-body" >/dev/null 2>&1; then
  echo "FAIL: a receipt token inside the finding comment itself cleared a finding" >&2
  exit 1
fi

# Receipt authorship and target validation (#218 P1.2).
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/none-author-receipt" >/dev/null 2>&1; then
  echo "FAIL: an association-less (NONE) receipt cleared a finding" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/unvalidated-sha-receipt" >/dev/null 2>&1; then
  echo "FAIL: fixed@0000000 cleared a finding without a validated commit" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/unknown-issue-receipt" >/dev/null 2>&1; then
  echo "FAIL: filed #999999 cleared a finding without an existing issue" >&2
  exit 1
fi
bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/validated-target-receipt" >/dev/null
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/double-backtick-receipt" >/dev/null 2>&1; then
  echo "FAIL: a double-backtick span in a reply cleared a finding" >&2
  exit 1
fi
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/pr-number-receipt" >/dev/null 2>&1; then
  echo "FAIL: filed #218 cleared a finding although #218 is a pull request, not an issue" >&2
  exit 1
fi

# Truncated review data must fail closed instead of under-counting (#218 P2).
out=$(bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/truncated-threads" 2>&1 >/dev/null || true)
case "$out" in
  *"truncated"*) : ;;
  *) echo "FAIL: a threads fixture with pageInfo.hasNextPage passed: $out" >&2; exit 1 ;;
esac
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/truncated-threads" >/dev/null 2>&1; then
  echo "FAIL: truncated thread pages exited 0" >&2
  exit 1
fi
out=$(bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/truncated-comments" 2>&1 >/dev/null || true)
case "$out" in
  *"truncated"*) : ;;
  *) echo "FAIL: a thread with comments.totalCount > nodes passed: $out" >&2; exit 1 ;;
esac
if bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$fix/truncated-comments" >/dev/null 2>&1; then
  echo "FAIL: a truncated comment list exited 0" >&2
  exit 1
fi

# Malformed fixture shapes must fail closed, not clear the gate silently.
expect_shape_error() {
  local label=$1 dir=$2 needle=$3 rc=0 err
  err=$(bash "$root/scripts/gate-receipt.sh" --fixtures-dir "$dir" 2>&1 >/dev/null) || rc=$?
  if [ "$rc" -ne 2 ]; then
    echo "FAIL: $label exited $rc, expected 2: $err" >&2
    exit 1
  fi
  case "$err" in
    *"$needle"*) : ;;
    *) echo "FAIL: $label did not name the shape error: $err" >&2; exit 1 ;;
  esac
}

mkdir -p "$tmp/malformed-threads" "$tmp/malformed-fallback" \
  "$tmp/malformed-thread-element" "$tmp/malformed-reviews" "$tmp/malformed-thread-comments"
for d in malformed-threads malformed-fallback malformed-thread-element \
  malformed-reviews malformed-thread-comments; do
  printf '[]\n' > "$tmp/$d/reviews.json"
  printf '[]\n' > "$tmp/$d/issue-comments.json"
done
printf '{"oops":1}\n' > "$tmp/malformed-threads/threads.json"
printf '{"oops":1}\n' > "$tmp/malformed-fallback/pull-comments.json"
printf '[{"oops":1}]\n' > "$tmp/malformed-thread-element/threads.json"
printf '{"oops":1}\n' > "$tmp/malformed-reviews/reviews.json"
printf '[{"isResolved":false,"comments":{"body":"not an array"}}]\n' > "$tmp/malformed-thread-comments/threads.json"

expect_shape_error "object threads.json" "$tmp/malformed-threads" "must be a JSON array of threads"
expect_shape_error "object pull-comments.json" "$tmp/malformed-fallback" "must be a JSON array of objects with a string body"
expect_shape_error "thread element without isResolved/comments" "$tmp/malformed-thread-element" "must be a JSON array of threads"
expect_shape_error "object reviews.json" "$tmp/malformed-reviews" "must be a JSON array of objects with a string body"
expect_shape_error "thread comments not an array" "$tmp/malformed-thread-comments" "must be a JSON array of threads"

echo "gate-receipt self-test: PASS"
