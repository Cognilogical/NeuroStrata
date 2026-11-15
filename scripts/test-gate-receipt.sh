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
echo "gate-receipt self-test: PASS"
