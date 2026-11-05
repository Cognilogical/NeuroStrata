#!/usr/bin/env bash
# Guard: every include_str! target in src/task/harness_plugins.rs must exist
# on disk at this point in the pipeline. Run AFTER the plugin-build step and
# BEFORE `cargo build` in CI (see .github/workflows/ci.yml).
#
# Why: src/task/harness_plugins.rs embeds plugin bundles with include_str!.
# One target, plugins/opencode/dist/index.js, is a build artifact that is
# gitignored (see .gitignore) and therefore absent from a fresh checkout.
# Without a prior plugin build, cargo fails with an include_str! error that
# does not name the CI step at fault. This guard fails first, naming the
# missing target(s) explicitly.
#
# Diagnostic only: passing here does NOT prove CI is green. A persisting
# local dist/ makes this pass regardless of whether the CI plugin-build
# step works; only the PR's fresh-checkout CI run proves that (see AC1
# for Issue #42).
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
done < <(grep -o 'include_str!\s*(\s*"[^"]*"' "$HARNESS_FILE" | sed -E 's/^[^"]*"//; s/"$//')

if [ "$checked" -eq 0 ]; then
  echo "::error::No include_str! targets found in $HARNESS_FILE; guard is vacuous, refusing to pass."
  exit 1
fi

if [ "$missing" -ne 0 ]; then
  echo "::error::Guard FAILED: $missing of $checked include_str! target(s) missing (see above)."
  exit 1
fi

echo "Guard PASSED: all $checked include_str! target(s) present."
