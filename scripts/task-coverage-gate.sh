#!/bin/bash
# scripts/task-coverage-gate.sh
# Pre-push guard: scan changed .md files for work-item language without task IDs.
# Exits 1 if any untracked work items found; 0 otherwise.
# Skips (exit 0) if git is not available — gate never blocks a non-git project.
# Must run on stock macOS Bash 3.2: no associative arrays (see #320).

if [ "${BASH_VERSINFO[0]}" -lt 3 ] ||
   { [ "${BASH_VERSINFO[0]}" -eq 3 ] && [ "${BASH_VERSINFO[1]}" -lt 2 ]; }; then
    echo "task-coverage-gate: ERROR: Bash 3.2+ required" >&2
    exit 1
fi

command -v git >/dev/null 2>&1 || { echo "task-coverage-gate: no git — skipping"; exit 0; }

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
cd "$ROOT" || exit 0

# Determine diff range: upstream if tracking branch exists, else origin/main..HEAD or HEAD~1..HEAD
if git rev-parse --verify --quiet '@{u}' >/dev/null 2>&1; then
    RANGE="@{u}..HEAD"
elif git rev-parse --verify --quiet origin/main >/dev/null 2>&1; then
    RANGE="origin/main..HEAD"
else
    RANGE="HEAD~1..HEAD"
fi

# Include renames so modified renames expose additions; R100 diffs have no added lines.
CHANGED=$(git diff -M --name-only --diff-filter=AMR "$RANGE" | grep -E '\.md$' || true)
# Parallel indexed arrays instead of `declare -A`: stock macOS Bash 3.2 has no
# associative arrays, and a rejected declaration used to skip the Markdown scan
# while still exiting 0 (#320).
RENAME_NEW=()
RENAME_OLD=()
while IFS=$'\t' read -r _status old new; do
    RENAME_NEW[${#RENAME_NEW[@]}]="$new"
    RENAME_OLD[${#RENAME_OLD[@]}]="$old"
done < <(git diff -M --name-status --diff-filter=R "$RANGE")

COUNT=0
if [ -n "$CHANGED" ]; then
while IFS= read -r file; do
    [ -n "$file" ] || continue
    # Only inspect added lines (+...), never context (-) or unchanged text,
    # so the gate never blocks on legacy prose already in the repo.
    # Skip the archive directory — it contains historical records whose prose
    # is intentionally un-cited and should never trigger the gate.
    [[ "$file" =~ ^docs/archive/ ]] && { continue; }
    diff_paths=("$file")
    for i in "${!RENAME_NEW[@]}"; do
        if [ "${RENAME_NEW[$i]}" = "$file" ]; then
            # Destination-only pathspecs hide the old side and turn R100 into an add.
            diff_paths=("${RENAME_OLD[$i]}" "$file")
            break
        fi
    done
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        if ! grep -qE '(ns|neurostrata)-[0-9a-zA-Z]{4,}' <<< "$line"; then
            echo "task-coverage-gate: untracked work item in $file: $line" >&2
            COUNT=$((COUNT + 1))
        fi
    done < <(git diff -M "$RANGE" -- "${diff_paths[@]}" | grep -E '^\+' | grep -vE '^\+\+\+' | grep -iE '(next move|todo:|todo :|fixme:|action item:|deferred:|open item|follow-up|remaining|next step|we should also|later we need to|still need to|must also fix|need to follow up)' || true)
done <<< "$CHANGED"
else
    echo "task-coverage-gate: no markdown files changed — markdown check skipped"
fi

if [ "$COUNT" -gt 0 ]; then
    echo "task-coverage-gate: ERROR: $COUNT untracked work item(s) found in changed documentation." >&2
    echo "Each must reference a NeuroStrata task ID (ns-xxxx or neurostrata-xxxx)." >&2
    exit 1
fi

# Lean 4 Formal Verification Sync & Integrity Check: governed state-machine /
# gate / CRDT modules must keep proofs/NeuroStrata/*.lean and the parity test
# in step with src/formal_kernel.rs.
ALL_CHANGED=$(git diff --name-only "$RANGE" || true)
GOVERNED_ALT='src/task/machine\.rs|src/task/gate\.rs|src/task/mod\.rs|src/workflow/machine\.rs|src/synaptic_mesh/record\.rs|src/synaptic_mesh/frame\.rs|src/synaptic_mesh/import\.rs|src/synaptic_mesh/reactor\.rs|src/tools/procedure\.rs|src/retention/gate\.rs|src/kernel/mod\.rs'
GOVERNED_RE="^(${GOVERNED_ALT})$"
if echo "$ALL_CHANGED" | grep -qE "^(src/formal_kernel\.rs|proofs/|${GOVERNED_ALT})"; then
    # 1. Standalone Aeneas/rustc check for the extracted kernel, when the extractor is present.
    if [ -f "$ROOT/proofs/scripts/extract-aeneas.sh" ]; then
        bash "$ROOT/proofs/scripts/extract-aeneas.sh" >/dev/null \
            || { echo "lean-formal-gate: ERROR: src/formal_kernel.rs failed standalone Aeneas/Kani/rustc check" >&2; exit 1; }
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

# Documentation Manifest & Surface-Diff Gate
if [ -f "$ROOT/docs/manifest.json" ]; then
    if [ -x "$ROOT/target/release/neurostrata-mcp" ] && "$ROOT/target/release/neurostrata-mcp" doc-gate --help >/dev/null 2>&1; then
        "$ROOT/target/release/neurostrata-mcp" doc-gate --root "$ROOT" || exit 1
    elif [ -x "$ROOT/target/debug/neurostrata-mcp" ] && "$ROOT/target/debug/neurostrata-mcp" doc-gate --help >/dev/null 2>&1; then
        "$ROOT/target/debug/neurostrata-mcp" doc-gate --root "$ROOT" || exit 1
    elif command -v neurostrata-mcp >/dev/null 2>&1 && neurostrata-mcp doc-gate --help >/dev/null 2>&1; then
        neurostrata-mcp doc-gate --root "$ROOT" || exit 1
    else
        cargo test --quiet --bin neurostrata-mcp doc_gate::tests::doc_gate_passes_on_live_repository -- --exact || exit 1
    fi
fi

echo "task-coverage-gate: no untracked work items found"
exit 0
# neurostrata-gate-sha256: e46c4eeb26f05f064b9bd5abb7e7db3ff18fbb8a3f2a753c32566b37e72431d6
