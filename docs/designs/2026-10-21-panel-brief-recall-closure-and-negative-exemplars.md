# Panel brief: closing the recall gaps — negative exemplars, the domain registry, the topic strap

**Status:** proposal — capture only. No implementation, no approval, no behavioural change.
**Date:** 2026-10-21 · **Parent spec:** `docs/superpowers/specs/2026-10-21-proactive-recall-and-access-topology-design.md`
**Parent brief:** `docs/designs/2026-10-20-gap-proactive-recall-and-access-topology.md`
**Task:** `neurostrata-pq5w` · **Ruling recorded as:** memory `bca58f91` (no-deferral rule)

---

## 0. The mandate

The owner has ruled: **we never defer.** No spec, brief, plan, or task may park an open
item for "observation data" or "a later spec". Every open point is either decided inside
the artifact that is being written, or filed as a tracked task with an owner and a concrete
next action, and named in the artifact.

The parent spec deferred four items. This brief closes all four, plus the parent's open
question list, in **one** spec.

**This brief asks the panel one thing:** for each item below, what is the design? Options,
trade-offs, a recommendation, the failure modes, the blast radius, the acceptance criteria,
and the source anchors.

---

## 1. What this spec must close

### A. Q5 — negative exemplars

**The requirement.** NeuroStrata has no vocabulary for *"we tried this here, and it went
wrong."* A `rule` states what must hold; a `fact` states what is true; nothing states that
an attempted approach failed and must be avoided. (Parent brief §4 Q5; external review Idea 5.)

The panel must answer: **what is the minimal representable structure for a negative
exemplar?** Live candidates, all of which the panel may refine or replace:

1. a new first-class `memory_type` (e.g. `mistake`, `negative_exemplar`);
2. a `fact` carrying a **polarity** field (e.g. `metadata.polarity: "negative"`);
3. a `fact` carrying a declared typed sub-object (e.g. `metadata.attempt`);
4. a `rule` carrying a negation (e.g. `metadata.guard` semantics inverted).

Each candidate must be judged against **all** of these, explicitly:

- **Representable** — storable today, and storable after the change.
- **Discoverable** — the agent must *learn* the shape. The agent only learns vocabulary from
  `tool_summaries.memory_type` and the tool description; an undeclared shape is invisible.
- **Mesh-safe** — the mesh is closed over `SHAREABLE_MEMORY_TYPES`. An unknown `memory_type`
  is **rejected at import time** with a hard error.
- **Retrievable** — a query about "what did not work here" must *find* it. If the polarity
  lives inside the content text, a vector query over positive phrasing may never match it.
  Say how retrieval works, and say what happens to the vector of a negative row.
- **Honest** — it must not be able to overclaim. A negative exemplar is evidence that an
  approach failed *once*, in *one* context; it is not a law. The shape must be able to carry
  that nuance or the spec must say what carries it.
- **Non-poisoning** — a negative row must not invert the meaning of the positive corpus, and
  must not be silently retrieved as if it were an instruction. State what a search for
  "how do we do X" returns when a negative exemplar about X exists.
- **Cheapest sufficient** — prefer the shape that adds the least vocabulary surface. Say
  explicitly whether a vocabulary bump is required and what it costs.

### B. The per-namespace domain registry

**The requirement.** `metadata.domain` becomes a normalized, filterable key in the parent
spec (slug-normalized on write **and** on comparison, with an optional filter on search).
That collapses *shape* drift (`"Database Access"` ≡ `"database-access"`) but cannot catch a
**wrong-but-well-formed** slug — `"database-access"` where the namespace means
`"auth"`. The registry is the mechanism that catches it. The parent spec recorded the
bootstrap rule so it is not re-litigated:

> **the first write of any new domain auto-registers it when the registry row is absent;
> subsequent writes validate against the registry.**

The panel must answer, concretely:

1. **Where does the registry live?** Options: a memory row (of which type? a `rule`? a
   custom type? a `fact`?), a file under the namespace's config directory, or a table in the
   store. Each has a different blast radius; say which and why.
2. **What happens when a write presents a well-formed slug that is not registered?** Options:
   hard reject, soft warn + write, auto-correct to the nearest registered slug, or
   auto-register (which defeats the purpose — say so if you reject it). Say which, and say
   what an agent experiences when it trips.
3. **How are legacy free-text domains grandfathered?** The parent spec made the filter
   migration-free by normalizing at comparison time. The registry must not break that.
4. **What does "enum-validated" mean when the set is dynamic per namespace?** The vocabulary
   file is static; a per-namespace registry is not. Say how the two coexist.
5. **How does the registry interact with the mesh?** A registry row is a memory row; which
   `memory_type` is shareable, and does a shared registry make sense when the namespace's
   domain vocabulary is namespace-local?

### C. The `get_snapshot` topic strap

**The requirement.** The parent spec put the proactive recall on the
`neurostrata_append_log` response — the one call `AGENTS.md` §3 mandates at a topic
transition — and deferred a second seat: a `get_snapshot` lane keyed on topic. The owner has
now ruled that the deferral ends. The panel must answer:

1. **What is the input?** `get_snapshot` today takes only `namespace`. A topic strap needs a
   topic: an explicit `topic`/`tags` argument, or the tags of the current turn, or the last
   tagged turn in the Episodic Buffer.
2. **Does it coexist with the `append_log` recall, or replace it?** If both fire on the same
   topic change, the agent sees two recall surfaces. Say how they compose, and what the
   total context cost per topic transition is.
3. **What is the budget?** (The parent spec bounded the `append_log` recall at ≤ 3 entries,
   ≤ 120 chars each.) State this strap's bound.
4. **Does it mutate `access_count`?** (The parent spec's invariant: recall must not.)
5. **How does it differ from B and D below?** If two of these turn out to be the same
   mechanism, the spec must say so and ship one.

### D. The domain-scoped snapshot lane

**The requirement.** The parent spec deferred "the domain-scoped snapshot lane" alongside C:
the lane that surfaces domain-matched rows in the snapshot **regardless of `access_count`** —
the mechanical fix for the parent brief's sharpest finding (a newly written fact has
`access_count: 0`, and the snapshot sorts strictly by `access_count` and cuts to five, so
correctly-stored new knowledge is cut from the one call `AGENTS.md` §3 makes mandatory).

The panel must answer: is this lane the *same mechanism* as C (a snapshot query keyed on a
topic/domain), or a distinct one? What is its bound, and does it mutate `access_count`?
How does it interact with the top-5 cut — does it sit beside it, above it, or replace it?

### E. The parent spec's open questions

The parent spec §11 listed four open questions. Under the no-deferral rule each becomes
either a decision in this spec or a **named task**. The panel must classify each and say
which:

1. *Is the `append_log` seat sufficient?* (C answers it structurally — say how.)
2. *Does a domain filter actually pay?* (A namespace with one domain gains nothing. This
   becomes instrumentation: name the metric, where it is recorded, who reads it.)
3. *Is a domain registry worth its migration?* (The owner has ruled: yes. Say what the
   migration is.)
4. *The correct distance band for deduplication* — belongs to the local-self-improvement
   spec's Phase 2, not to this one. Name it as a task; do not decide it here.

---

## 2. The substrate (verified; do not re-propose it)

Verified against source on 2026-10-21 at `940736f`. *(the anchors were re-derived on 2026-10-22)*

| Constraint | Anchor |
|---|---|
| Vocabulary **v4**; types `rule, fact, task, directory, file, markdown, symbol, procedure`; `context` is *not* first-class *[changed by the proactive-recall plan: the vocabulary is v5]* | `src/schemas/memory-vocabulary.v3.json:2`; `memory_types` |
| `fact` has **no** `fields` object today (a `definition` and `structural: false` only) *[changed by the proactive-recall plan: `fact` carries a `fields.topology` object]* | `src/schemas/memory-vocabulary.v3.json:66-69` |
| `rule.fields` = `enforcement` / `source` / `guard` — the typed-sub-field precedent | `src/schemas/memory-vocabulary.v3.json:39-64` |
| Custom memory types are accepted unchanged ("Any nonempty string is accepted unchanged") | `src/schemas/memory-vocabulary.v3.json`, `extensions.custom_memory_types` |
| Existing **custom** types in use: `preference`, `bootstrap`, `persona`, `context` | `src/schemas/memory-vocabulary.v3.json`, `extensions.existing_custom_types` |
| A semantic vocabulary change requires a `vocabulary_version` bump | `src/schemas/memory-vocabulary.v3.json:155`; asserted at `src/traits.rs:263` |
| The agent learns the vocabulary only from `tool_summaries.memory_type` and the tool description | `src/schemas/memory-vocabulary.v3.json:161` |
| The mesh is closed over shareable types — `["rule", "fact", "procedure"]`; an unknown `memory_type` is **rejected at import time** with a hard `Err` | `src/synaptic_mesh/import.rs:61`, `:649-655`; contract at `src/synaptic_mesh/record.rs:26-28` |
| Relations are closed (`GOVERNS, CONTAINS, RELATES_TO, EXTRACTED_FROM`) and custom relations are **not materialized** | `src/schemas/memory-vocabulary.v3.json`, `extensions.custom_relations`; `src/store/ladybug.rs:461-468` |
| `domain` is written on add (raw, unvalidated) *[changed by the proactive-recall plan: the domain is normalized to a slug on write]* | `src/server.rs:1279-1281` |
| `domain` is read back and echoed to export; it never filters retrieval *[changed by the proactive-recall plan: it is now an optional soft filter on search]* | `src/store/ladybug.rs:1870` (read), `:1923` (emitted) |
| The snapshot's straps are computed **before** the cut; then it sorts strictly by `access_count` desc and truncates to 5 | `src/server.rs:1454-1460`, `:1465-1480` |
| `get_snapshot`'s input schema takes only `namespace` | `src/server.rs:153-161` |
| `handle_append_log(arguments)` — no `emb`, no `store`; the response is `{appended, file, size_bytes, rolled, tags}` *[changed by the proactive-recall plan: the handler takes the embedder and the store; the response carries `recalled`]* | `src/buffer.rs:129-177`; dispatch `src/server.rs:664` |
| `append_log`'s input schema has `content`, `project_root`, `tags` — **no `namespace`** | `src/server.rs:373-380` |
| A malformed declared shape is a **soft** failure: `parse_edge_specs` emits a `WriteShapeWarning` and the durable write still succeeds | `src/store/ladybug.rs:432`, `:440`, `:545` |
| `VectorStore::search(namespace, vector, limit, include_tasks, include_archived)` — the `include_*` flags are the precedent for another parameter | `src/traits.rs:165-172`; impl `src/store/ladybug.rs:1020` |
| A task's extracted memory defaults to `memory_type: "fact"` when the caller names none | `src/task/mod.rs:1110-1116` |
| `access_count` is incremented only by the MCP search handler and the get-memory handler — never by `VectorStore::search` itself | `src/server.rs:2019`, `:2652`; `src/store/ladybug.rs:2009` |

---

## 3. Binding constraints

1. **No deferrals.** Every open point is decided here or filed as a task, named in the spec.
   "We will know later" is not an answer.
2. **Nothing new where something already exists.** The parent spec's through-line: the topic
   switch is already written; the domain is already written; the guard already reads typed
   fields off a rule; the Episodic Buffer already exists as the audit trail.
3. **No consumer-system facts in the `NeuroStrata` namespace.** A topology claim, a lesson,
   or a negative exemplar about a downstream system is a fact about that system and belongs
   to that project's namespace. NeuroStrata owes the *mechanism*.
4. **No consumer vocabulary imported.** `lane` — and any product name — is not a
   NeuroStrata concept. `domain`, `namespace`, `memory_type`, `polarity` are.
5. **Mesh closure is binding.** Any design that introduces a `memory_type` the mesh refuses
   is out unless it also changes the mesh wire contract — and that must be priced.
6. **Honesty rules.** No silent fallbacks (a substituted path must be reported); explicit
   zeros (an empty result is `[]`, not silence); declared shapes only (an undeclared
   convention is invisible to the agent).
7. **Additive recall.** A recall surface never displaces, hides, or suppresses anything the
   agent would otherwise have seen; a wrong recall costs noise, not a wrong absence.
8. **No behavioural change until implemented.** The panel evaluates; the spec is written down.
9. **Rootless.** No design element may require privileges, capabilities, or system state
   changes from a NeuroStrata user.
10. **Vendor-neutral, and the build stays at its warning baseline** (14 warnings today).

---

## 4. Output format

For **each** of A, B, C, D, E, answer in this shape:

```
### <item>
**Recommendation:** <one line>
**Options considered:** <2-4 options, each with the reason it is or is not chosen>
**Design:** <the mechanism, concretely — the data shape, the call sites, the file:line anchors>
**Failure modes:** <table: failure | behaviour | safety>
**Blast radius:** <files, signatures, migration, vocabulary bump, mesh impact>
**Acceptance criteria:** <numbered, testable>
**Dependencies:** <what must land first>
```

Anchors must be `file:line` and verified against the current tree. If an anchor in §2 is
wrong, say so and give the right one — refuting a fact is a better contribution than
agreeing with it.

**Do not** write the spec. Produce the design material; the orchestrator arbitrates and
writes.

---

## 5. Out of scope

- The parent spec's own implementation (it is written and unimplemented). This spec states
  its dependencies on that implementation explicitly and says what order they land in.
- The local-self-improvement spec's Phase 2 distance band (named as a task, not decided).
- Any consumer-system claim, and any live lookup in a consumer system.
