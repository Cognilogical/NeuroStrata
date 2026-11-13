#!/usr/bin/env bash
set -euo pipefail
root=$(git rev-parse --show-toplevel)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

printf 'Merge status: CHANGES-NEEDED\nNo receipt.\n' > "$tmp/fail.txt"
printf 'Merge status: DO-NOT-MERGE as filed.\nfiled #150\n' > "$tmp/pass.txt"
printf 'Codex: no findings.\n' > "$tmp/clean.txt"
printf 'CHANGES-NEEDED\nfixed@deadbeef\nDO-NOT-MERGE\nruled: accepted for this release\n' > "$tmp/two.txt"

if bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/fail.txt" >/dev/null 2>&1; then
  echo "FAIL: marker without receipt passed" >&2
  exit 1
fi
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/pass.txt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/clean.txt" >/dev/null
bash "$root/scripts/gate-receipt.sh" --reviews-file "$tmp/two.txt" >/dev/null
echo "gate-receipt self-test: PASS"
