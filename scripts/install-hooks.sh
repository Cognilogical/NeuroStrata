#!/usr/bin/env bash
# Install scripts/hooks/pre-push as the hook git runs. Safe to re-run.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
src="$root/scripts/hooks/pre-push"

[ -f "$src" ] || { echo "missing $src — restore it before installing" >&2; exit 1; }

# Non-default hooks dir? Ask git; the config wins over .git/hooks.
hooks_path="$(git config --get core.hooksPath || true)"
dest_dir="${hooks_path:-$root/.git/hooks}"
mkdir -p "$dest_dir"

install -m 0755 "$src" "$dest_dir/pre-push"
echo "Installed $dest_dir/pre-push"

# Read-back only: report presence, never execute. Running it here would run
# every project guard in PROJECT_GATES as a side effect of installing.
if [ -x "$dest_dir/pre-push" ]; then
    echo "Read-back: $dest_dir/pre-push present"
else
    echo "Read-back: $dest_dir/pre-push MISSING or not executable" >&2
    exit 1
fi
echo "Verify: neurostrata_task_setup should report verified.hook_installed: true"
