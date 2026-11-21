# 2026-11-21 — Task Transition Revert Must Resolve Staged State (#349)

> **Baseline**: `cognilogical/main` @ `a75b243a`
> **Issue**: #349 (P2)
> **Branch**: `fix/task-revert-and-shellcheck`
> **Workflow**: spec → red eval → minimal fix → green eval → regression suites

---

## 1. Scope

One branch of `revert_action`'s delta loop in `src/rdf/action.rs`: the
`transition_task` arm (~line 3470). The sibling arms (`upsert_memory` at
~3293, `supersede_memory`, `set_twin_state` at ~2943) already consult
`staged_rows` before the store; this one does not.

Non-goals: `force` semantics, ReBAC/authorization, the task state machine,
receipt bitemporality, RevertCompat normalization (#299). The other delta
arms are read only for contrast and are not changed.

---

## 2. Problem

`apply_action` chains multiple `TransitionTask` effects on one task through
`current_targets`: each effect stages the after-payload and re-inserts it as
the next effect's input. One action that moves a task `open → in_progress →
blocked` therefore records two `RowDelta { kind: "transition_task" }` pairs,
`(open, in_progress)` then `(in_progress, blocked)`.

`revert_action` walks the deltas in reverse, staging each restored
before-image in `staged_rows` (an in-memory map) and committing once at the
end. Reverting the last delta first is fine: the persisted row is `blocked`,
matching that delta's after-image. The first delta then expects the
after-image `in_progress`, but the `transition_task` arm reads only
`store.get(&delta.after_id)`, which still returns the persisted `blocked`
row. The non-force guard at ~3517 compares `actual_status` to
`expected_status` and returns `ConflictWithNewerVersion { superseded_by:
"task:<id>:blocked" }` — for the action's own final state, with no
intervening write.

This is the same defect class as #248 (multi-effect `set_twin_state`),
fixed in #251 by resolving revert heads against staged revert state. The
`transition_task` arm was not covered.

## 3. Invariant

An ordinary revert (`force: false`) reverses every recorded
`transition_task` delta in reverse order and restores the task's original
status, provided nothing external wrote to any affected row since the
receipt became active. `ConflictWithNewerVersion` is reserved for stale
active heads, missing rows, and shacl/authorization-free state drift.

A denied revert writes nothing: no row mutation, no revert receipt, no
superseding of the apply receipt.

## 4. Behavior Specification

1. **Staged-first comparison head.** In the `transition_task` arm, when
   `staged_rows` already holds `delta.after_id` (staged by a later delta in
   the reverse walk), that payload is the comparison head; otherwise the
   persisted row from `store.get(&invocation.namespace, &delta.after_id)`
   is. The persisted row is still read in both cases so a genuinely missing
   row is reported and the commit CAS keeps a persisted anchor.
2. **Guard semantics unchanged.** The guard still requires
   `is_active_payload(current, now_ts)`, `actual_status == expected_status`,
   and `actual_upd == expected_upd` on the resolved head. A genuine external
   write between apply and revert still raises `ConflictWithNewerVersion`.
3. **Expected-head CAS anchored to persisted state.** The commit CAS
   (`ExpectedHead::ActiveRow`) verifies pre-transaction persisted values, so
   `record_expected_active_row` is invoked with the persisted row, never the
   staged comparison head. Feeding an uncommitted staged status into the CAS
   would make the revert commit fail against its own staging.
4. **Revert receipt deltas stay chained.** `revert_task_deltas` records
   `before_status = actual_status` of the resolved head and
   `after_status = before_status` of the original delta, so the revert
   receipt reads as the exact reverse chain (`blocked → in_progress →
   open`).
5. **Single-transition behavior is byte-identical.** With one
   `transition_task` delta, `staged_rows` has no entry for the row and the
   arm behaves exactly as before.

## 5. Evals

`src/rdf/action.rs` test module:

1. `revert_multi_effect_transition_task_on_same_task_chains_through_staged_state`
   — seed task `open`; define an action with two `transition_task` effects
   (`in_progress`, then `blocked`); apply; assert persisted `blocked` and
   two `transition_task` deltas; ordinary `revert_action` must succeed,
   restore `open`, and record two revert deltas in reverse order.
2. Denied path in the same eval: against the unfixed code the revert fails;
   asserted to leave the task at `blocked` with no revert receipt row. The
   test asserts on the fixed path that a *stale* revert (external write
   after apply) is still refused and writes nothing.

Existing `transition_task` tests (`link_edge_records_existed_before_and_task_transition_captures_history`,
`all_five` round-trip, stale-head tests) must stay green.

## 6. Documentation

`docs/features-api/subsystems/kinetic-actions.md` line 18 lists the revert
state-resolution mechanisms. It gains the `transition_task` staged-head
resolution alongside the existing `SetTwinState` and `UpsertMemory`
mechanisms.

## 7. Risks

- **CAS regression (highest).** If the CAS expected head were taken from
  staged state, the revert commit would fail with
  `ConflictWithNewerVersion` after passing the guard. Mitigated by keeping
  `record_expected_active_row` on the persisted row and by the
  two-transition eval.
- **Masked external writes.** Resolving from staged state must not weaken
  the guard for the first delta of a chain: the staged payload is compared
  strictly (status + `updated_at`) and a mismatch still conflicts. Eval 2's
  stale case pins it.
- **Force path.** Under `force: true` the guard is skipped; the staged head
  is still what gets retired/restored. Unchanged from the sibling arms.
