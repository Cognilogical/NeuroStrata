#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="$ROOT/scripts/task-coverage-gate.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/task-coverage-gate-test.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

new_repo() {
    local repo="$1"
    mkdir -p "$repo"
    git -C "$repo" init -q
    git -C "$repo" config user.email test@example.com
    git -C "$repo" config user.name test
    for i in $(seq 1 100); do
        printf 'legacy line %s\n' "$i"
    done > "$repo/old.md"
    git -C "$repo" add old.md
    git -C "$repo" commit -q -m baseline
    git -C "$repo" update-ref refs/remotes/origin/main HEAD
}

modified_rename="$TMP/modified-rename"
new_repo "$modified_rename"
git -C "$modified_rename" mv old.md new.md
printf 'TODO: added during rename without a task id\n' >> "$modified_rename/new.md"
git -C "$modified_rename" add new.md
git -C "$modified_rename" commit -q -m 'modified rename'
set +e
modified_output=$(cd "$modified_rename" && bash "$GATE" 2>&1)
modified_status=$?
set -e
if [ "$modified_status" -eq 0 ] || ! grep -q 'new.md.*TODO: added during rename' <<< "$modified_output"; then
    printf 'FAIL: modified rename must expose its added work item\n%s\n' "$modified_output" >&2
    exit 1
fi

pure_rename="$TMP/pure-rename"
new_repo "$pure_rename"
printf 'TODO: pre-existing legacy prose\n' >> "$pure_rename/old.md"
git -C "$pure_rename" add old.md
git -C "$pure_rename" commit -q -m 'legacy prose'
git -C "$pure_rename" update-ref refs/remotes/origin/main HEAD
git -C "$pure_rename" mv old.md new.md
git -C "$pure_rename" commit -q -m 'pure rename'
if ! (cd "$pure_rename" && bash "$GATE"); then
    echo 'FAIL: pure rename must not re-trigger on unchanged legacy prose' >&2
    exit 1
fi

echo 'task-coverage-gate tests passed'
