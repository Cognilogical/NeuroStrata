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

# Exclude commits already on origin/main when pushing a feature branch that merged main
EXCLUDE_MAIN=()
CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
if [ "$CURRENT_BRANCH" != "main" ] && git rev-parse --verify --quiet origin/main >/dev/null 2>&1; then
    EXCLUDE_MAIN=("^origin/main")
fi

CHANGED=$(git log --no-merges --name-only --pretty=format: "$RANGE" "${EXCLUDE_MAIN[@]}" | sort -u | grep -E '\.md$' || true)

COUNT=0
if [ -n "$CHANGED" ]; then
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
    done < <(git log --no-merges -p "$RANGE" "${EXCLUDE_MAIN[@]}" -- "$file" | grep -E '^\+' | grep -vE '^\+\+\+' | grep -iE '(next move|todo:|todo :|fixme:|action item:|deferred:|open item|follow-up|remaining|next step|we should also|later we need to|still need to|must also fix|need to follow up)' || true)
done <<< "$CHANGED"
else
    echo "task-coverage-gate: no markdown files changed — markdown check skipped"
fi

if [ "$COUNT" -gt 0 ]; then
    echo "task-coverage-gate: ERROR: $COUNT untracked work item(s) found in changed documentation." >&2
    echo "Each must reference a NeuroStrata task ID (ns-xxxx)." >&2
    exit 1
fi

# Lean 4 Formal Verification Sync & Integrity Check: governed state-machine /
# gate / CRDT modules must keep proofs/NeuroStrata/*.lean and the parity test
# in step with src/formal_kernel.rs.
ALL_CHANGED=$(git diff --name-only "$RANGE" || true)
GOVERNED_ALT='src/task/machine\.rs|src/task/gate\.rs|src/task/mod\.rs|src/synaptic_mesh/record\.rs|src/synaptic_mesh/frame\.rs|src/synaptic_mesh/import\.rs|src/synaptic_mesh/reactor\.rs|src/tools/procedure\.rs|src/retention/gate\.rs'
GOVERNED_RE="^(${GOVERNED_ALT})$"
if echo "$ALL_CHANGED" | grep -qE "^(src/formal_kernel\.rs|proofs/|${GOVERNED_ALT})"; then
    # 1. Standalone Aeneas/rustc check for the extracted kernel, when the extractor is present.
    if [ -f "$ROOT/proofs/scripts/extract-aeneas.sh" ]; then
        bash "$ROOT/proofs/scripts/extract-aeneas.sh" >/dev/null \
            || { echo "lean-formal-gate: ERROR: src/formal_kernel.rs failed standalone Aeneas/rustc check" >&2; exit 1; }
    fi
    # 2. Zero forbidden sorry/axiom anywhere in proofs/ (excluding .lake build cache) and no deleted proof modules.
    if [ -d "$ROOT/proofs" ] && grep -rnE --exclude-dir=.lake --include='*.lean' '\b(sorry|axiom)\b' "$ROOT/proofs/" >/dev/null 2>&1; then
        echo "lean-formal-gate: ERROR: forbidden sorry/axiom found in proofs/" >&2
        exit 1
    fi
    if git diff --diff-filter=D --name-only "$RANGE" | grep -qE '^(proofs/NeuroStrata/.*\.lean|tests/lean_parity_test\.rs|src/formal_kernel\.rs)$'; then
        echo "lean-formal-gate: ERROR: deleting formal kernel, Lean proof modules, or parity test suite is forbidden" >&2
        exit 1
    fi
    # 3. formal_kernel.rs changes must be accompanied by proofs/ and parity-test changes.
    if echo "$ALL_CHANGED" | grep -q '^src/formal_kernel\.rs$'; then
        if ! echo "$ALL_CHANGED" | grep -q '^proofs/' || ! echo "$ALL_CHANGED" | grep -q '^tests/lean_parity_test\.rs$'; then
            echo "lean-formal-gate: ERROR: src/formal_kernel.rs was modified without updating proofs/ and tests/lean_parity_test.rs" >&2
            exit 1
        fi
    fi
    # 4. Governed production modules changed without the kernel changing: advisory notice.
    if echo "$ALL_CHANGED" | grep -qE "$GOVERNED_RE" && ! echo "$ALL_CHANGED" | grep -q '^src/formal_kernel\.rs$'; then
        echo "lean-formal-gate: NOTICE: formally governed module(s) changed — verify whether new state-machine/gate/CRDT/invariant logic needs a Lean 4 proof in proofs/NeuroStrata/ and parity row in tests/lean_parity_test.rs."
    fi
fi

echo "task-coverage-gate: no untracked work items found"
exit 0
