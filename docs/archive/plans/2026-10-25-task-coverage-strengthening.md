# Task Coverage Strengthening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Strengthen NeuroStrata to automatically identify and enforce task creation for work items mentioned in documentation, eliminating untracked work.

**Architecture:** Add git-diff scanning pre-push gate that requires work-item language in docs to reference existing NeuroStrata task IDs, and enhance in-flight task creation tools to reject payloads containing task intent markers unless explicit task references are provided.

**Tech Stack:** Rust (NeuroStrata core), shell scripting (pre-push hook), markdown parsing (intent detection), JSON-RPC (NeuroStrata tools)

**Spec:** /home/kenton/Documents/NeuroStrata/docs/design-task-subsystem.md

---

### Global Constraints

- Zero-Action Start: no file edits without a claimed task
- Task lifecycle: open → in_progress → done (with extraction)
- Pre-push gate must run before git push to block untracked work
- Git is optional - gate must gracefully skip if not present
- All changes must be testable and follow existing NeuroStrata patterns
- No new database schema changes - use existing task memory model

### Review Focus

- Work items in documentation without task references (untracked work)
- Task intent markers in tool payloads without follow_up_tasks or task IDs
- Git diff analysis for work-item language in documentation files
- Pre-push gate failure when work items lack task IDs
- Session completion rejections when work items are described but not tracked

---

## Task 1: Git-Diff Pre-Push Gate Implementation

**Files:**
- Create: `scripts/task-coverage-gate.sh` (new executable)
- Modify: `scripts/install_hooks.sh` to include new gate check

**Interfaces:**
- Consumes: git diff output, NeuroStrata task list API
- Produces: exit code 1 with error message if work items lack task IDs

- [ ] **Step 1: Write the failing test**

```bash
#!/bin/bash
# scripts/task-coverage-gate.sh
# Check for work-item language in changed markdown files that lacks task IDs

echo "Checking for untracked work items..."
# Find changed .md files
changed_files=$(git diff --name-only @{u}..HEAD | grep -E '\.md$' || true)
if [ -z "$changed_files" ]; then
    echo "No markdown files changed - nothing to check"
    exit 0
fi

untracked_items=0
for file in $changed_files; do
    # Look for work-item patterns that should reference task IDs
    # Patterns: "Next Move", "remaining", "TODO", "open item", "follow-up", "TODO:"
    if grep -E "(Next Move|remaining|TODO|open item|follow-up|TODO:)" "$file" | grep -v "ns-" | while read line; do
        if ! grep -E "ns-[a-z0-9]{4}" <<< "$line" >/dev/null; then
            echo "Found untracked work item in $file: $line"
            untracked_items=$((untracked_items + 1))
        fi
    done
done

if [ $untracked_items -gt 0 ]; then
    echo "ERROR: $untracked_items untracked work items found in documentation. Each must reference a NeuroStrata task ID (ns-xxxx)." >&2
    exit 1
fi

echo "No untracked work items found"
exit 0
```

- [ ] **Step 2: Run test to verify it fails**

Run: `bash scripts/task-coverage-gate.sh` on a repo with untracked work items
Expected: FAIL with error message about untracked work items
Expected: PASS when no untracked items

- [ ] **Step 3: Implement `scripts/task-coverage-gate.sh`**

Create the script with the logic above, ensuring it:
  - Only runs if git is available (exit 0 if not)
  - Checks changed .md files for work-item language without task IDs
  - Exits with code 1 if any found, 0 otherwise
  - Maintains existing hook structure

- [ ] **Step 4: Run test to verify it passes**

Run: `bash scripts/task-coverage-gate.sh` on a repo with no untracked items
Expected: PASS with "No untracked work items found"
Expected: PASS with exit code 0

- [ ] **Step 5: Commit**

```bash
git add scripts/task-coverage-gate.sh
git commit -m "feat: add task coverage pre-push gate"
```

---

## Task 2: In-Flight Task Creation Enhancement

**Files:**
- Modify: `src/task/mod.rs` (task_complete and add_memory handlers)
- Modify: `src/task/intent.rs` (if needed for better intent detection)

**Interfaces:**
- Consumes: task_complete and add_memory payloads
- Produces: JSON-RPC error -32603 when intent markers found without task references

- [ ] **Step 1: Write the failing test**

Create test that shows task_complete rejects payload with "TODO:" marker but no follow_up_tasks or task ID

- [ ] **Step 2: Run test to verify it fails**

Run test - should fail with expected error

- [ ] **Step 3: Implement enhancement in task_complete handler**

Add logic to detect task-intent markers in reason/memory.content and reject if follow_up_tasks empty and no task ID cited

- [ ] **Step 4: Run test to verify it passes**

Run test - should now pass

- [ ] **Step 5: Commit**

```bash
git add src/task/mod.rs src/task/intent.rs
git commit -m "feat: enhance task_complete to reject untracked work items"
```

---

## Task 3: Integration and Testing

**Files:**
- Modify: `scripts/install_hooks.sh` to include new gate check
- Add tests in `tests/` for both new features

**Interfaces:**
- Consumes: all components from Tasks 1 and 2
- Produces: working system that blocks untracked work

- [ ] **Step 1: Write the failing test**

Create integration test that simulates git push with untracked work items

- [ ] **Step 2: Run test to verify it fails**

Run test - should fail

- [ ] **Step 3: Implement integration**

Ensure pre-push hook calls task-coverage-gate.sh and task_complete rejects untracked intent markers

- [ ] **Step 4: Run test to verify it passes**

Run test - should pass

- [ ] **Step 5: Commit**

```bash
git add scripts/ tests/
git commit -m "feat: integrate task coverage enforcement"
```

---

### Self-Review

After writing the complete plan, look at the spec with fresh eyes and check the plan against it.

**1. Spec coverage:** Skim each section/requirement in the spec. Can you point to a task that implements it? List any gaps.

**2. Step scan:** Every step must let the implementer write exactly one reasonable thing, and no step may carry more than that: a line that decides nothing is a gap, a function body the signature and tests already determine is a transcript. Fix both.

**4. Review Focus:** For each input class or failure mode the spec implies, is there a task whose tests exercise it? The five uncovered ones most likely to bite a person using this software go in the Review Focus section, and each line there gets its test added to the owning task.

**5. Proportion:** Compare the plan's length to the spec's. A plan several times longer than the spec it implements is a transcript of the program, not a plan. If code blocks are most of the document, replace bodies with signatures, test names and assertions, and check that each step is still unambiguous.

If you find issues, fix them inline.

---

### Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-10-25-task-coverage-strengthening.md`. Please review the plan. Which execution approach would you prefer?**

- **Subagent-driven** - A fresh subagent implements each task and a fresh reviewer checks it before the next one starts, then a whole-branch review at the end.
- **Native** - I implement every task myself in this session, the way this harness runs work, then one fresh reviewer on the most capable model checks the whole branch.

**For this plan I recommend Subagent-driven, because the tasks have clear dependencies (pre-push gate must exist before in-flight enhancements).**
