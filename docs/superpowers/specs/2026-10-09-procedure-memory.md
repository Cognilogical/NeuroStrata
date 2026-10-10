# Procedural Memory (procedure) — Spec

**Date:** 2026-10-09
**Author:** BOSUN session + K3 architect verdict
**Vocabulary bump:** v3 → v4
**Task:** neurostrata-9nzd (in_progress)
**Path:** architectural (per superpowers:brainstorming HARD-GATE ratchet; K3 architect produced the shape via independent noodle; conversational design approval was the answer to "on the procedure lets capture the details and get it implemented if we have everything we need")

> **Shipped (1.8.0):** `memory_type: "procedure"`, `neurostrata_procedure_perform`, the `procedures_due` strap in `get_snapshot`, and the v1 simplification (no spawned task per fire, simple "what's not lapsed" surface). The "v2 candidate" markers below are stale — owner directive 2026-10-09 dropped the v2 backlog and the procedure ride shipped in 1.8.0. See [`CHANGELOG.md`](../../../CHANGELOG.md) for the resolved-state list.

---

## Intent

Add the third leg of the memory triad. NeuroStrata has semantic (rule/fact/context) and episodic (Episodic Buffer) but no procedural memory. `procedure` closes the third leg: knowing-how, never closes, decays with disuse, strengthened by rehearsal. Maps to the user's words: never-closes (procedures persist), TTL (disuse decay), iteration (rehearsal count).

Success looks like: an operator can `neurostrata_add_memory(memory_type="procedure", metadata={trigger:..., remaining_fires:..., valid_to:..., last_performed_at:...})`, see it in `neurostrata_procedure_perform`-d returns, and have `get_snapshot`'s `procedures_due` strap surface it on the right session/event.

## Non-goals (v1 scope)

- No new thalamic-bus pulse variant (firing is observation, not state change)
- No new wiring-panel wire (procedure is a memory, not a rule)
- No generative variant (spawn a task per fire) — captured as v2 candidate
- No CL1 reasoner integration
- No 1.9.0 split — procedure rides in 1.8.0 (owner decision 2026-10-09: drop the v2 candidate backlog; everything ships in one release)

## Shape

### Memory type `procedure` (vocab v4)

```json
{
  "vocabulary_version": 4,
  "memory_types": {
    "rule": { ... },
    "fact": { ... },
    "task": { ... },
    "directory": { ... },
    "file": { ... },
    "markdown": { ... },
    "symbol": { ... },
    "procedure": {
      "definition": "A recurring attentional prompt: a procedure the agent performs on a schedule, after a trigger, or for a bounded number of repetitions. The cognitive sibling of Episodic Buffer; if a runner daemon ever materializes, its anatomical seat is Basal Ganglia.",
      "structural": false,
      "fields": {
        "trigger": {
          "type": "string",
          "enum": [
            "session-start", "before-edit", "after-mutation",
            "every-n-sessions:N", "every-n-days:N"
          ],
          "definition": "When this procedure should fire. `every-n-sessions:N` and `every-n-days:N` carry a numeric parameter (parsed from the string). Distinct from wiring.rs `fires_on` (which is the code-level reminder enum) — `procedure.trigger` is data-level, written by users; `fires_on` is code, in a closed registry."
        },
        "remaining_fires": {
          "type": ["integer", "null"],
          "definition": "Iteration budget. null = unbounded. Decremented on each `procedure_perform`; when 0 the procedure is spent (still surfacable, but `procedure_perform` returns `performed: false, reason: 'spent'`)."
        },
        "valid_to": {
          "type": ["integer", "null"],
          "definition": "Bi-temporal TTL. Reuses the existing `valid_to` semantics from server.rs:1375-1380,1418-1423 (supersede transfers future expiry to replacement). null = no expiry."
        },
        "last_performed_at": {
          "type": ["integer", "null"],
          "definition": "Epoch of the last `procedure_perform`. null = never performed."
        },
        "performance_count": {
          "type": "integer",
          "definition": "Total times this procedure has been performed (lifetime)."
        },
        "last_episodic_pointer": {
          "type": ["string", "null"],
          "definition": "Id of the most recent `append_log` entry produced by a `procedure_perform`. null = never performed."
        }
      }
    }
  }
}
```

### Cognitive name
`procedure` (Procedural Memory). Sibling of Episodic Buffer.

### Interaction with 1.8.0 surface

- **Thalamic bus:** no new pulse variant. Firing is observation, not state change. Lapse reuses existing `ThalamicPulse::Archived { id, namespace }` with `metadata.lapse_reason` set.
- **Pre-push gate (SAS):** warn-only, never block. Precedent: `export-freshness` wire `blocks: false` (`src/task/wiring.rs:117-121`).
- **Episodic Buffer:** consumed by agent, not a subscriber. `procedure_perform` writes an `append_log` entry; `last_episodic_pointer` back-references it.
- **Strap arc:** slots into `plan`. `get_snapshot` renders `procedures_due` beside ready tasks.
- **Wiring panel:** no new wire (procedure is a memory, not a rule). Existing `fires_on` enum is closed; procedure's `trigger` lives in metadata.

## New tool: `neurostrata_procedure_perform`

**Input schema:**
```json
{
  "type": "object",
  "properties": {
    "id": { "type": "string", "description": "Memory id of the procedure." },
    "note": { "type": "string", "description": "Optional free-text note for the Episodic Buffer entry." }
  },
  "required": ["id"]
}
```

**Output schema:**
```json
{
  "type": "object",
  "properties": {
    "performed": { "type": "boolean" },
    "lapsed": { "type": "boolean" },
    "spent": { "type": "boolean" },
    "remaining_fires": { "type": ["integer", "null"] },
    "performance_count": { "type": "integer" },
    "episodic_pointer": { "type": ["string", "null"] },
    "reason": { "type": "string" }
  }
}
```

**Behavior (in order):**
1. Load memory by `id`. If not found → `performed: false, reason: 'not found'`.
2. If `memory_type != "procedure"` → `performed: false, reason: 'wrong memory_type: <actual>'`.
3. Check `valid_to`: if `now > valid_to` → `lapsed: true, performed: false, reason: 'lapsed'` (no mutation; do nothing).
4. Check `remaining_fires`: if `0` → `spent: true, performed: false, reason: 'spent'` (no mutation).
5. Mutate atomically: set `last_performed_at` = now; increment `performance_count`; if `remaining_fires` is not null, decrement it (`remaining_fires.saturating_sub(1)`); append `append_log` entry; set `last_episodic_pointer` to the new entry id.
6. Return `performed: true, remaining_fires, performance_count, episodic_pointer`.

## New strap: `procedures_due` in `get_snapshot`

**Returns:** list of `[]` (or) `{id, content, trigger, next_due, remaining_fires, last_performed_at, performance_count}` for procedures that are due.

**Due logic (per `trigger` value):**
- `session-start` → always due when this strap is computed in the `get_snapshot` call
- `before-edit` → always due (the act of getting a snapshot is the "edit" gate; refine later)
- `after-mutation` → always due (the act of getting a snapshot is the "after-mutation" gate; refine later)
- `every-n-sessions:N` → due when `(session_count % N) == 0` (session_count is exposed by the snapshot, or 0 for v1)
- `every-n-days:N` → due when `now - last_performed_at >= N * 86400` (or when `last_perisnull: true` and the procedure is freshly created)

Sort by `next_due` ascending. v1 simplification: skip due-logic nuance for the periodic triggers; emit a row for every procedure whose `valid_to` is in the future (i.e., not lapsed) regardless of trigger type. The trigger enum still exists; the strap is a simple "what's not lapsed yet" surface. Refinement is v2.

## Reuses (no new infrastructure)

- Bi-temporal TTL: `valid_to` reuses `src/server.rs:1375-1380,1418-1423` (supersede transfers future expiry to replacement)
- Archived pulse: no new bus variant; lapse uses existing `ThalamicPulse::Archived { id, namespace }`
- Pre-push gate warn-only: precedent is `export-freshness` (`src/task/wiring.rs:117-121`)
- Append_log: `src/buffer.rs::append_entry` (existing)
- Strap arc: `get_snapshot` (existing) gets a new strap entry

## Files to touch (estimated)

- `src/schemas/memory-vocabulary.v3.json` → rename to `v4` (or keep filename, bump `vocabulary_version` to 4); add `procedure` block; extend `tool_summaries.memory_type` description.
- `src/tools/procedure.rs` (new file) — `neurostrata_procedure_perform` tool + the `procedures_due` strap compute.
- `src/server.rs` (tools/list) — register the new tool; update `tools_list_carries_all_nineteen_tools` test to `tools_list_carries_all_twenty_tools` (rename and bump the count).
- `src/server.rs` (get_snapshot) — add the `procedures_due` strap entry.
- `src/task/wiring.rs` — **no change** to the closed `fires_on` enum; `procedure.trigger` lives in memory metadata, not in the code-level wiring registry.
- `src/buffer.rs` — **no change**; `append_entry` is consumed as-is.
- `src/main.rs` (CLI) — **no change**; the tool is MCP-only per design.
- `src/handlers/` — **no change**; tool is served via `process_mcp_request`.
- `tests/` — new tests for `procedure` memory_type CRUD, `procedures_due` strap, `neurostrata_procedure_perform` stamping + decrement + lapse.
- `CHANGELOG.md` — add `### Added` entry: "**Procedural memory (v4 vocabulary):** `procedure` memory type with `trigger` / `remaining_fires` / `valid_to` / `last_performed_at` / `performance_count` / `last_episodic_pointer` metadata; `neurostrata_procedure_perform` tool stamps + decrements + writes an Episodic Buffer pointer; `get_snapshot`'s new `procedures_due` strap surfaces what's not lapsed yet."
- `docs/COGNITIVE_ARCHITECTURE.md` — append the `procedure` row to the cognitive-name glossary.

## Verifies (TDD)

- Unit: `add_memory(memory_type="procedure", metadata={...})` succeeds and the row round-trips with all 6 fields preserved.
- Unit: `neurostrata_procedure_perform(id)` happy path: stamps `last_performed_at`, increments `performance_count`, decrements `remaining_fires`, writes the append_log entry, sets `last_episodic_pointer`, returns `performed: true`.
- Unit: `neurostrata_procedure_perform` on a lapsed procedure (now > `valid_to`): `performed: false, lapsed: true, reason: 'lapsed'`, no mutation.
- Unit: `neurostrata_procedure_perform` on a spent procedure (`remaining_fires == 0`): `performed: false, spent: true, reason: 'spent'`, no mutation.
- Unit: `neurostrata_procedure_perform` with wrong memory_type: `performed: false, reason: 'wrong memory_type: <actual>'`, no mutation.
- Unit: `procedures_due` strap: 3 procedures, 1 due (not lapsed), ordered correctly.
- Drill: removing `remaining_fires.saturating_sub(1)` in `procedure_perform` fails the iteration-budget test (mutation-proof).
- Drill: removing the `now > valid_to` check in the lapse branch fails the lapse test (mutation-proof).
- Drill: removing the `performance_count += 1` fails the count test.
- Integration: end-to-end via the live daemon (when 1.8.0 lands and the procedure ride is added to a feature branch): `add_memory` a procedure, `neurostrata_procedure_perform`, observe `get_snapshot`'s `procedures_due` returns the same row with decremented counter.

## Open question (resolved by v1 simplification)

**Q:** When a procedure fires, must a closeable artifact exist (a spawned task per occurrence) or is snapshot-surfacing + acknowledgment enough?
**A (v1):** Snapshot-surfacing + acknowledgment. No spawned task. The `procedure_perform` tool stamps + acknowledges; the strap surfaces. Generative (task-per-fire) is a v2 candidate (logged).

## v2 candidates (logged, not in scope)

- Generative variant: each fire spawns a `task` that goes through claim → complete → extraction. Useful for "release procedure" where each step is auditable.
- Cross-namespace procedures: the trigger fires per-namespace, not just the procedure's home.
- Performance-correlation: derive `next_due` from past `last_performed_at` variance (rehearsal effect; earlier-firing becomes the natural cadence).
- Cohort effects: a procedure fires if a related procedure's trigger fires (procedural chaining).
- More precise due-logic for periodic triggers (currently v1 just emits non-lapsed procedures; refine to true periodic next-due computation).
