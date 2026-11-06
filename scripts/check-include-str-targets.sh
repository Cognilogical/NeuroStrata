#!/usr/bin/env bash
# Guard: every include_str! target in src/task/harness_plugins.rs must exist
# on disk at this point in the pipeline. Run BEFORE `cargo build` in CI
# (see .github/workflows/ci.yml).
#
# Why: src/task/harness_plugins.rs embeds plugin bundles with include_str!.
# The bundles are tracked in git, so they are present on a fresh checkout.
# Without them, cargo fails with include_str! errors naming the
# missing files — but only once compilation reaches this crate, after
# its dependencies build. This guard fails first, in seconds, before
# compilation begins, naming every missing target it extracts.
#
# Diagnostic only: passing here does NOT prove CI is green. A persisting
# dist/ makes this pass regardless of whether CI ever built from a fresh
# checkout; only a fresh-checkout CI run proves the bundle is tracked.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HARNESS_FILE="${1:-$ROOT/src/task/harness_plugins.rs}"
HARNESS_DIR="$(dirname "$HARNESS_FILE")"

missing=0
checked=0
while IFS= read -r target; do
  checked=$((checked + 1))
  resolved="$HARNESS_DIR/$target"
  if [ -f "$resolved" ]; then
    echo "ok: $target"
  else
    echo "::error::Missing include_str! target: $target (resolved: $resolved)"
    missing=$((missing + 1))
  fi
done < <(grep -o 'include_str![[:space:]]*([[:space:]]*"[^"]*"' "$HARNESS_FILE" | sed -E 's/^[^"]*"//; s/"$//')

if [ "$checked" -eq 0 ]; then
  echo "::error::No include_str! targets found in $HARNESS_FILE; guard is vacuous, refusing to pass."
  exit 1
fi

if [ "$missing" -ne 0 ]; then
  echo "::error::Guard FAILED: $missing of $checked include_str! target(s) missing (see above)."
  exit 1
fi

echo "Guard PASSED: all $checked include_str! target(s) present."
