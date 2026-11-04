# Procedural Memory — Triggered Habits with Rehearsal Budgets

> **Why you care:** Not all knowledge is a static rule or a one-off task. Repeatable engineering habits (like post-migration checklists or pre-release audits) need to fire on specific triggers, track how many times they have been rehearsed, and retire cleanly once ingrained.

---

## 1. Tasks vs. Procedures

NeuroStrata separates work into two distinct cognitive primitives:

| Primitive | Purpose | Lifecycle | Completion / Firing Tool |
| :--- | :--- | :--- | :--- |
| **Task (`Goal`)** | A finite unit of work that finishes once. | `open` → `in_progress` → `done` (requires Lock 2 memory extraction). | `neurostrata_task_complete` |
| **Procedure** | A repeatable knowing-how routine with a trigger and rehearsal budget. | Created as `memory_type: "procedure"`; fired repeatedly until its rehearsal budget reaches zero or `valid_to` expires. | `neurostrata_procedure_perform` |

---

## 2. Creating a Procedure

Procedures are created via `neurostrata_add_memory` with `memory_type: "procedure"` and structured metadata defining the trigger condition and rehearsal budget:

```json
{
  "name": "neurostrata_add_memory",
  "arguments": {
    "namespace": "NeuroStrata",
    "memory_type": "procedure",
    "content": "Before editing any SHACL shape in src/schemas/shacl/, run cargo test --test shacl_validation_test to verify baseline shape conformance.",
    "metadata": {
      "trigger": "before-edit",
      "performance_count": 0,
      "last_performed_at": null,
      "last_episodic_pointer": null
    }
  }
}
```

### Supported Trigger Types
- `session-start` — Surfaced at the start of every new agent session during `neurostrata_get_snapshot`.
- `before-edit` — Surfaced before modifying governed files.
- `after-mutation` — Surfaced after code or schema changes to enforce verification routines.
- `every-n-sessions:N` — Periodic cadence across sessions.
- `every-n-days:N` — Time-based maintenance cadence.

---

## 3. Firing a Procedure (`neurostrata_procedure_perform`)

Each time an agent executes a procedure, it records the rehearsal using `neurostrata_procedure_perform`:

```json
{
  "name": "neurostrata_procedure_perform",
  "arguments": {
    "namespace": "NeuroStrata",
    "id": "proc-uuid-here",
    "note": "Ran shacl_validation_test before updating memory-shapes.v1.ttl; all 14 shapes green."
  }
}
```

When `neurostrata_procedure_perform` executes, NeuroStrata atomically:
1. Increments `performance_count` by `1`.
2. Stamps `last_performed_at` with the current UTC timestamp.
3. Decrements the procedure's rehearsal budget counter (if a finite budget was configured).
4. Writes an audit entry to the rolling **Episodic Buffer** and stores the resulting pointer in `last_episodic_pointer`.

If a procedure's rehearsal budget is exhausted or its `valid_to` timestamp has passed, `neurostrata_procedure_perform` returns a structured `spent` or `lapsed` status without mutating the row.
