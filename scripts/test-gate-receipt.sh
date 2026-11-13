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
echo "gate-receipt self-test: PASS"
