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

# Bash 3.2 compatibility leg (#320). Stock macOS /bin/bash rejects `declare -A`
# and, without errexit, used to continue with an empty rename map and exit 0 —
# silently skipping the Markdown scan. Shadow `declare` in a child bash so an
# -A declaration aborts the gate; a regression then fails this test loudly.
bash32_gate() {
    (
        cd "$1"
        bash -c '
            declare() {
                case " $* " in
                    *" -A "*) echo "declare: -A: invalid option" >&2; exit 3 ;;
                esac
                builtin declare "$@"
            }
            source "$1"
        ' _ "$GATE"
    )
}

compat_repo="$TMP/bash32-compat"
new_repo "$compat_repo"
printf 'TODO: untracked under bash 3.2\n' >> "$compat_repo/old.md"
git -C "$compat_repo" add old.md
git -C "$compat_repo" commit -q -m 'bash 3.2 work item'
set +e
compat_output=$(bash32_gate "$compat_repo" 2>&1)
compat_status=$?
set -e
if [ "$compat_status" -ne 1 ] || ! grep -q 'old.md.*TODO: untracked under bash 3.2' <<< "$compat_output"; then
    printf 'FAIL: bash 3.2 emulation must still run the Markdown scan\n%s\n' "$compat_output" >&2
    exit 1
fi

compat_clean="$TMP/bash32-clean"
new_repo "$compat_clean"
if ! bash32_gate "$compat_clean"; then
    echo 'FAIL: bash 3.2 emulation must pass a clean tree' >&2
    exit 1
fi

echo 'task-coverage-gate tests passed'
