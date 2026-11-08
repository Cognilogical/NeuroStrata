#!/usr/bin/env bash
# Install scripts/hooks/pre-push as the hook git runs. Safe to re-run
# (idempotent: a destination already identical to the source is left
# in place and reported as such).
#
# The installed pre-push carries two guards: the upstream-push guard
# (Anthropic-Memory Parity, Task 0.2 — rejects pushes to
# Cognilogical/NeuroStrata) and the NeuroStrata task gate. Installing
# is the step that makes both mechanical rather than prose; see
# docs/plans/2026-11-07-memory-parity/sync-resolution.md.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
src="$root/scripts/hooks/pre-push"

[ -f "$src" ] || { echo "missing $src — restore it before installing" >&2; exit 1; }

# Where does git actually run hooks from for THIS clone?
# 1. core.hooksPath, if set — git honors it over everything else.
# 2. Otherwise the common git dir's hooks/ — resolved via
#    `git rev-parse --git-common-dir`, NOT "$root/.git/hooks": in a
#    linked worktree "$root/.git" is a file, and the common dir is
#    shared by the main checkout and every worktree, so one install
#    covers the whole clone.
hooks_path="$(git config --get core.hooksPath || true)"
if [ -n "$hooks_path" ]; then
    case "$hooks_path" in
        /*) dest_dir="$hooks_path" ;;
        *)  dest_dir="$root/$hooks_path" ;;
    esac
else
    common_dir="$(cd "$root" && git rev-parse --git-common-dir)"
    case "$common_dir" in
        /*) dest_dir="$common_dir/hooks" ;;
        *)  dest_dir="$root/$common_dir/hooks" ;;
    esac
fi
mkdir -p "$dest_dir"
dest="$dest_dir/pre-push"

if [ -f "$dest" ] && cmp -s "$src" "$dest"; then
    echo "Already installed (identical): $dest"
else
    install -m 0755 "$src" "$dest"
    echo "Installed $dest (from $src)"
fi
chmod +x "$dest"

# Read-back only: report presence, never execute. Running it here would
# run every project guard in the hook as a side effect of installing.
if [ -x "$dest" ]; then
    echo "Read-back: $dest present and executable"
else
    echo "Read-back: $dest MISSING or not executable" >&2
    exit 1
fi
echo "Verify: neurostrata_task_setup should report verified.hook_installed: true"
