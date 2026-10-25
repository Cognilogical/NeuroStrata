#!/bin/bash
# scripts/task-coverage-gate.sh
# Pre-push guard: scan changed .md files for work-item language without task IDs.
# Exits 1 if any untracked work items found; 0 otherwise.
# Skips (exit 0) if git is not available — gate never blocks a non-git project.

command -v git >/dev/null 2>&1 || { echo "task-coverage-gate: no git — skipping"; exit 0; }

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
cd "$ROOT" || exit 0

# Determine diff range: upstream if tracking branch exists, else HEAD~1..HEAD
if git rev-parse --verify --quiet @{u} >/dev/null 2>&1; then
    RANGE="@{u}..HEAD"
else
    RANGE="HEAD~1..HEAD"
fi

CHANGED=$(git diff --name-only "$RANGE" | grep -E '\.md$' || true)
[ -n "$CHANGED" ] || { echo "task-coverage-gate: no markdown files changed — nothing to check"; exit 0; }

COUNT=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    # Only inspect added lines (+...), never context (-) or unchanged text,
    # so the gate never blocks on legacy prose already in the repo.
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        if ! grep -qE 'ns-[0-9a-zA-Z]{4,}' <<< "$line"; then
            echo "task-coverage-gate: untracked work item in $file: $line" >&2
            COUNT=$((COUNT + 1))
        fi
    done < <(git diff "$RANGE" -- "$file" | grep -E '^\+' | grep -vE '^\+\+\+' | grep -iE '(next move|todo:|todo :|open item|follow-up|remaining|next step)' || true)
done <<< "$CHANGED"

if [ "$COUNT" -gt 0 ]; then
    echo "task-coverage-gate: ERROR: $COUNT untracked work item(s) found in changed documentation." >&2
    echo "Each must reference a NeuroStrata task ID (ns-xxxx)." >&2
    exit 1
fi

echo "task-coverage-gate: no untracked work items found"
exit 0
