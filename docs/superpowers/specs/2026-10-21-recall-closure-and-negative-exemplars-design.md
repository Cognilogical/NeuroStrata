# Closing the recall gaps: negative exemplars, the domain registry, the snapshot strap

**Status:** designed. Two-seat panel (Argon / Gemini 4 Argon, K3 / Kimi K3) + orchestrator
arbitration. Unanimous on the core of A; the B strictness and the C+D shape were arbitrated.
**Date:** 2026-10-21 · **HEAD at authoring:** `c5acd4f`
**Brief:** `docs/designs/2026-10-21-panel-brief-recall-closure-and-negative-exemplars.md`
**Parent spec:** `docs/superpowers/specs/2026-10-21-proactive-recall-and-access-topology-design.md`
**Tracks:** task `neurostrata-pq5w` (this spec) · `neurostrata-426k` (the parent spec's
implementation — **prerequisite**) · `neurostrata-bzdn` (rule honesty — lands first) ·
`neurostrata-r7dj` (the distance band — named task, §3.4.4)

---

## 0. Summary

The parent spec deferred four items. Under the owner's no-deferral ruling (memory
`bca58f91`, `AGENTS.md` §9) this spec closes **all four**, plus the parent's four open
questions. Nothing here is parked.

| | Decision |
|---|---|
| **A — Negative exemplars** | A **`fact`** carrying a declared typed sub-object `metadata.attempt` (`approach`, `outcome`, `failure_mode`, `context`). The presence of a valid `attempt` **is** the negative marker — no second flag. New `memory_type` rejected (the mesh rejects unknown types at import); rule-negation rejected (a rule overclaims by type). |
| **B — Domain registry** | One **registry row per namespace** (`fact` + declared `metadata.domain_registry.domains`), seeded lazily with every legacy domain on the first domain write. Afterwards an unregistered slug is **rejected** with the registered list and a `register_domain: true` escape hatch. Zero offline migration. |
| **C + D — One strap, not two** | A single **snapshot domain strap**: ≤3 entries, computed off the pre-cut list, deduped against the top-5, two modes — `explicit` (the caller names a domain) and `recent_domain` (the caller names none). It closes the `access_count: 0` hole **and** provides the topic seat. |
| **E — Open questions** | All four **decided**: two seats with a bounded budget each, one metric family with thresholds, one migration (lazy, in-band), and one named task (`neurostrata-r7dj`). |

**The vocabulary bump is one, not two.** The parent spec's `fact.fields.topology` is
unbuilt, so `topology`, `attempt` and `domain_registry` ride **one** `vocabulary_version`
4 → 5 bump, not 4 → 5 → 6.

**The through-line, unchanged:** nothing new where something already exists. The topic
switch is already written; the domain is already written; the snapshot already computes
straps off the list it fetched; the guard already knows how to read typed fields off a
row. This spec wires them and declares three typed sub-objects.

---

## 1. Scope, non-goals, guardrails

**In scope:** A (negative exemplars), B (the per-namespace domain registry), C+D (one
snapshot strap), E (the parent's open questions, decided).

**Out of scope** (and why — none of these are deferrals; each is either already decided
elsewhere or is another spec's subject):

1. **The parent spec's own implementation.** It is written, reviewed and unbuilt. This
   spec depends on it and says in what order (§9). Task `neurostrata-426k`.
2. **The deduplication distance band.** The local-self-improvement spec's Phase 2
   subject, and its numeric cutoffs are embedder-specific by its own ruling. Named as
   task `neurostrata-r7dj` (§3.4.4).
3. **Any change to the guard, the event bus, the mesh wire format, or the relation set.**
4. **Any consumer-system fact.** A negative exemplar about a downstream system is a fact
   about that system and belongs to that system's namespace. NeuroStrata owes the
   *mechanism*.

**Guardrails (binding):**

1. **No deferrals.** Every open point is decided here or filed as a task and named here.
   §11 is the closed list.
2. **No consumer-system facts in the `NeuroStrata` namespace.**
3. **No claim is made that any consumer system is misconfigured.**
4. **No consumer vocabulary is imported.** `lane` and product names are not NeuroStrata
   concepts; `domain`, `namespace`, `memory_type` are.
5. **Mesh closure is binding.** `SHAREABLE_MEMORY_TYPES` is `rule`, `fact`, `procedure`;
   anything outside it is rejected at import.
6. **Honesty rules.** No silent substitution (a fallback states its mode); explicit zeros
   (an empty result is an explicit empty list); declared shapes only.
7. **Additive recall.** No recall surface may displace, hide, or suppress anything the
   agent would otherwise have seen.
8. **No behavioural change until implemented.**
9. **Rootless.** Nothing may require privileges, capabilities, or system state.

---

## 2. The substrate (verified at `c5acd4f`; the panel did not re-propose it)

| Constraint | Anchor |
|---|---|
| Vocabulary **v4**; `fact` carries **no** `fields` object today | `src/schemas/memory-vocabulary.v3.json:2`, `:66-69` |
| `rule.fields` = `enforcement` / `source` / `guard` — the typed-sub-field precedent | `src/schemas/memory-vocabulary.v3.json:39-64` |
| A semantic change requires a `vocabulary_version` bump; asserted in one place | `src/schemas/memory-vocabulary.v3.json:155`; `src/traits.rs:263` |
| The agent learns vocabulary **only** from `tool_summaries` and the tool description | `src/schemas/memory-vocabulary.v3.json:158`, `:161`; overlaid onto the tools at `src/server.rs:456-501` |
| The mesh is closed over `["rule","fact","procedure"]`; an unknown `memory_type` is **rejected at import** | `src/synaptic_mesh/import.rs:61`, `:649-655`; contract `src/synaptic_mesh/record.rs:26-28` |
| A mesh row whose id is not a parseable **UUID is skipped** — ids must be UUIDs | `src/synaptic_mesh/worker.rs:180-183`; `MeshRecord.id: Uuid` at `src/synaptic_mesh/record.rs:24` |
| Mesh export publishes only `eligibility: shareable`; absent or `private` reads as private | `src/synaptic_mesh/worker.rs:174-179` |
| Relations are closed; the four keys are hardcoded | `src/store/ladybug.rs:455-462` |
| `domain` is written raw on add | `src/server.rs:1279-1281` |
| `domain` is read back and echoed to export; it never filters retrieval | `src/store/ladybug.rs:1854`, `:1907` |
| A row's vector is the embedding of its **content**, unchanged; nothing negates it | `src/server.rs:1147-1150` |
| The snapshot's straps are computed off the **pre-truncation** list; then sort by `access_count` desc and truncate to 5 | `src/server.rs:1454-1460`, `:1465-1480` |
| `get_snapshot`'s input schema takes only `namespace`; its handler takes no embedder | `src/server.rs:153-161`, `:1416` |
| `handle_append_log(arguments)` — no `emb`, no `store`; response `{appended, file, size_bytes, rolled, tags}` | `src/buffer.rs:129-177`; dispatch `src/server.rs:664` |
| A malformed declared shape is a **soft** failure — `WriteShapeWarning`, the write still succeeds — and the warning goes to **stderr**, which an MCP agent never sees | `src/store/ladybug.rs:416`, `:424`, `:529-530` |
| `VectorStore::search(namespace, vector, limit, include_tasks, include_archived)` | `src/traits.rs:165-172`; impl `src/store/ladybug.rs:1004` |
| `store.get(namespace, id)` and `store.list(namespace, user_id)` exist | `src/traits.rs:195`, `:192` |
| `access_count` is incremented only by the MCP search handler and the get-memory handler | `src/server.rs:2019`, `:2652`; `src/store/ladybug.rs:2003` |
| `create_new_namespace: true` is the established **escape-hatch** precedent for a write that would otherwise be refused | `src/server.rs:117`, `:1217-1222` |
| Structural types are excluded from recall candidates | `src/store/ladybug.rs:687` |
| `format_memory` is the rendering path for a returned row | `src/server.rs:2222` |
| The bus metrics surface serializes the bus's own counters (eight integers) | `src/handlers/bus_metrics.rs:20-23` |

---

## 3. The design

### 3.1 A — Negative exemplars: a `fact` carrying a declared `metadata.attempt`

#### 3.1.1 The representation

```json
{
  "memory_type": "fact",
  "content": "Tried the embedder's raw-distance threshold for dedup on 2026-10-14; it collapsed unrelated rows. The cutoff is embedder-specific and must be measured, not assumed.",
  "metadata": {
    "domain": "dedup",
    "attempt": {
      "approach": "threshold on raw cosine distance",
      "outcome": "failed",
      "failure_mode": "collapsed unrelated rows into one candidate",
      "context": "NomicEmbedTextV15, 768 dims, on the NeuroStrata corpus"
    }
  }
}
```

**One marker, not two.** `attempt.outcome` is a closed enum with the single value
`"failed"`; a valid `attempt` **is** the negative marker. A second `polarity` scalar would
be a second source of truth that could disagree with the attempt (polarity without an
attempt is exactly the under-structured case the panel rejected).

**Declared, not conventional.** `memory_types.fact.fields.attempt` is added to the
vocabulary, alongside the parent's `fact.fields.topology`, and both are surfaced in
`tool_summaries.add` and `tool_summaries.memory_type` — otherwise the agent never learns
the shape, and an undeclared shape is invisible (§2).

#### 3.1.2 The three rejected options

| Option | Why not |
|---|---|
| A new first-class `memory_type` (`mistake`) | The mesh is closed over its shareable types and rejects an unknown type **at import** (§2). Adding a fourth breaks the wire contract across daemons, and it buys a new cognitive stratum for what is a descriptive assertion about a past episode. |
| A `fact` + bare scalar polarity | No structured fields to carry *what* was tried, *how* it failed, or *where*; and nothing to frame the row so a truncated 120-character snippet cannot read as an instruction. |
| A `rule` carrying a negation | A rule is normative; a failed attempt is evidence. Stored as a rule it overclaims by type alone ("must not do X" vs. "did not work once, here"), and under the rule-honesty contract a `rule` without a real `enforcement`/`guard` pair fails the gate. |

#### 3.1.3 Honesty, retrievability, non-poisoning

- **Honest.** `attempt.context` and `attempt.failure_mode` structurally scope the claim to
  one observed episode; the vocabulary prose says so in the agent's own words
  ("evidence of one episode, not a normative rule").
- **Retrievable.** The vector is the embedding of `content` (§2) — never negated, never
  inverted. Because the content states both the approach and the failure, the row matches
  *"how do we do X"* and *"what went wrong with X"*.
- **Non-poisoning.** Two rendering rules, both mandatory:
  1. `format_memory` (`src/server.rs:2222`) prints, immediately after `Type: fact`,
     `Attempt [FAILED in <context>]: tried '<approach>' -> failed with '<failure_mode>' (single episode, not a global rule)`.
  2. Every truncated `snippet` (§3.3, and the parent's `recalled[]`) is prefixed with
     `"[FAILED ATTEMPT] "` **before** truncation, so truncation can never strip the
     marker and leave the tail of a failure reading as an instruction.
- **Deduplication.** `metadata.attempt` joins the canonical exact-match key, so a negative
  exemplar about subject X never exact-match blocks a positive `fact` about subject X.

#### 3.1.4 Validation

`parse_edge_specs` (`src/store/ladybug.rs:424`) gains a shape check for
`metadata.attempt` on `fact` rows: an object with non-empty `approach`, `failure_mode`,
`context`, and `outcome == "failed"`. A malformed value raises a `WriteShapeWarning`
(`:416`, emitted `:529`) and **the durable write still succeeds** — a claim is worth
keeping even when its shape is imperfect; a rejected write would lose it.

---

### 3.2 B — The per-namespace domain registry

#### 3.2.1 Where it lives

One registry row **per namespace**:

| Field | Value |
|---|---|
| `memory_type` | `fact` |
| `id` | `Uuid::new_v5(&Uuid::NAMESPACE_OID, "neurostrata:domain_registry:<namespace>")` — a **deterministic UUID**, because the mesh skips any row whose id is not a parseable UUID (§2) |
| `content` | `Domain registry for <namespace>` |
| `metadata.domain_registry` | `{ "domains": ["auth", "database-access"] }` — sorted, deduplicated |
| `metadata.eligibility` | `private` — a namespace's vocabulary is namespace-local, so the row never travels the mesh (§2) |
| vector | the ordinary embedding of its content — no zero vectors anywhere in the store |

Rejected: a store table (DDL plus five trait implementations plus export plumbing — a
registry is *data about a namespace*, and the memory store already holds per-namespace
data with retrieval, export and audit); a config file (the namespace-keyed tools cannot
resolve a project-root path — it would be a second path to truth); a `rule` (it would
fail the rule-honesty gate for missing `enforcement`/`guard`); a custom `memory_type`
(rejected by mesh import if ever shared).

The registry row is **excluded from recall candidates and from the snapshot body** — the
same predicate the store already applies to structural rows (§2). It is metadata, not
knowledge.

#### 3.2.2 The bootstrap, and why it is namespace-level

The parent spec recorded the bootstrap rule so it would not be re-litigated:

> the first write of any new domain auto-registers it when **the registry row** is absent;
> subsequent writes validate against the registry.

Read per-slug ("every new slug auto-registers"), the registry has **no teeth**: nothing
can ever fail validation. Read per-namespace — which is what the parent's own words say
(*"a declared registry row **per namespace**"*, singular) — it is coherent and it is the
only reading under which the mechanism catches anything. **This spec adopts the
per-namespace reading.**

**Bootstrap (lazy, in-band, zero offline migration).** On a write carrying a `domain`:

1. Look up the registry row (`store.get`, `src/traits.rs:195`).
2. If it is **absent** — the namespace's first domain write — list the namespace's active
   rows (`store.list`, `src/traits.rs:192`), slug-normalize every `metadata.domain` they
   carry, union them with the incoming slug, and write the registry row. Every legacy
   domain is grandfathered at once, and **the write always succeeds**.
3. If it is **present** and the incoming slug is not in it, the write is **rejected**
   (§3.2.3).

#### 3.2.3 What an unregistered slug does

**Rejected, with the registered list and a one-call remedy.** The write returns:

```
ERROR [DOMAIN]: domain 'database-access' is not registered in namespace 'NeuroStrata'.
Registered domains: ["auth", "database-access-v2"]. Use one of those, or pass
register_domain: true if 'database-access' is a genuinely new domain.
```

`neurostrata_add_memory` and `neurostrata_task_complete` gain one optional boolean
`register_domain` (default `false`), mirroring the `create_new_namespace` escape hatch
already in the tool schema (§2). With `register_domain: true` the slug is inserted into
the registry (sorted, deduplicated) and the write proceeds.

**Why a rejection and not a warning.** The store's shape-warning channel writes to
stderr (§2) — an MCP agent never reads it, so a soft warning would let the wrong slug
into the store and then say nothing to the agent that could prevent it. A rejection is
the only mechanism here that the *caller* can act on. The cost is real and is accepted:
a write can fail. It cannot fail silently, and it cannot fail without telling the caller
what to do.

**The supersede path needs no check.** A supersede clones the payload wholesale and
replaces only `governs` / `related_to` / `contained_by` (parent spec §4.3.1), so it
carries the original row's already-validated domain forward.

#### 3.2.4 Grandfathering, and the static/dynamic split

- **The read path never consults the registry.** Comparison-time slug normalization
  (parent §4.3.1) is unchanged, so legacy free-text domains keep matching with no data
  migration. The registry is a **write-time** mechanism only.
- **The vocabulary declares the mechanism and the shape**, not the set: `fact.fields.
  domain_registry` plus the slug grammar `^[a-z0-9]+(-[a-z0-9]+)*$`. The **dynamic enum
  lives in the namespace's row**, and is surfaced to the agent in two places: the
  snapshot's `Registered domains (N): …` header (§3.3) and the rejection message above.
- **The registry is private.** It is never exported to the mesh. When the mesh import
  applies a shared `rule`/`fact`/`procedure` that carries a valid `metadata.domain`, the
  import step **unions that slug into the local registry row**, so an imported row's
  domain is legal locally and no shared record is ever refused for a domain the local
  registry has not seen.

---

### 3.3 C + D — One strap: the snapshot domain strap

The topic strap (C) and the domain-scoped lane (D) are **one mechanism**: both select
domain-tagged rows out of the list `get_snapshot` has already fetched, in order to
surface rows the `access_count` cut would hide. Both seats converged on that. This spec
ships **one strap with two modes**, not two straps.

#### 3.3.1 The input

`neurostrata_get_snapshot` gains one optional property:

```json
"domain": { "type": ["string", "array"], "items": { "type": "string" },
            "description": "Optional domain slug(s) to surface in the domain strap, regardless of access_count." }
```

- `domain` present → `mode: "explicit"`: only rows whose normalized domain matches.
- `domain` absent → `mode: "recent_domain"`: the newest domain-tagged rows, whatever
  their domain. This is what closes the `access_count: 0` hole on the bare
  `get_snapshot(namespace)` call that `AGENTS.md` §3 makes mandatory at task start.

#### 3.3.2 The computation

Computed off `all_memories` **before** the top-5 truncation (§2) — the same zero-extra-I/O
argument the existing `task_head` and procedures straps already make.

1. Exclude the ids that are in the top-5 (a row appears once, in the top-5).
2. Exclude structural types, `task` and `procedure` (the snapshot already has straps for
   those).
3. Require a non-empty normalized domain.
4. `mode: "explicit"` → keep only the requested slugs. `mode: "recent_domain"` → keep
   all.
5. Sort by `valid_from` desc, then `access_count` desc, then id asc.
6. Take at most **3** entries; in `recent_domain` mode, at most **2 from any one
   domain**, so a single-domain namespace cannot consume every slot.

Each entry is `{id, memory_type, domain, valid_from, access_count, snippet}`, `snippet`
≤ 120 characters, prefixed with `"[FAILED ATTEMPT] "` when the row carries a valid
`metadata.attempt` (§3.1.3).

#### 3.3.3 The output and its cost

A `Domain strap [mode: <explicit|recent_domain>]` section, rendered beside the top-5
body, with an **explicit zero** when empty (`(0): []`). Worst case 3 × 120 characters ≈
100 tokens at session start.

#### 3.3.4 Invariants

- **`access_count` is never mutated.** The strap reads the list it already has; the only
  two increments remain `src/server.rs:2019` and `:2652`. A strap hit cannot promote
  itself into the top-5.
- **Additive.** The top-5 body is byte-identical to today's for any namespace; the strap
  is a new section beside it.
- **Explicit mode is honest about its mode.** The section header names the mode, so a
  caller can never mistake a recent-domain selection for a domain match.

#### 3.3.5 What the topic seat is, then

Mid-session topic transitions are served by the parent's `append_log` recall (≤3 entries,
≤120 chars). Session/task start is served by this strap. Both are bounded, additive and
`access_count`-neutral, so an agent that triggers both in one turn sees at most six
compact entries — under ~300 characters of pointer text.

---

### 3.4 E — The parent's open questions, decided

#### 3.4.1 Is the `append_log` seat sufficient?

**Decided: no — two bounded seats, and the sufficiency of each is measured.** The
`append_log` recall covers mid-session topic transitions (`AGENTS.md` §3, Continuous
Backup Protocol); the snapshot strap covers session/task start (`AGENTS.md` §3, Mandatory
Pre-Flight Hook). Neither alone covers both moments.

**Metric: `recall_hit_rate`** — the fraction of recall responses whose `id`s the agent
later promotes with `neurostrata_get_memory` in the same session.
**Threshold:** below 10% over any 30-day window with ≥50 tagged turns, the recall budget
is tightened (the parent's §11.1 lever) before any new seat is considered.
**Where:** a counter incremented where `get_memory` increments `access_count`
(`src/server.rs:2652`), joined to the ids the recall returned.
**Who reads it:** the operator via the metrics surface in §3.4.2.

#### 3.4.2 Does a domain filter actually pay?

**Decided by instrumentation, not by deferral.** Four counters, in a small module beside
the search path (the bus's own metrics are eight integers about the bus, and know
nothing about domains — so the counters do not live inside the bus):

| Counter | Meaning |
|---|---|
| `domain_queries_total` | recall/search calls that carried a non-empty domain filter |
| `domain_narrowed_hits` | calls where the filter matched ≥1 row and narrowed the candidate set (`recalled_from: "domain"`) |
| `domain_fallback_vector` | calls where the filter matched 0 rows and fell back to the unfiltered vector search (`recalled_from: "vector"`) |
| `single_domain_bypasses` | calls in a namespace whose registry holds ≤1 domain (filtering could not narrow anything) |

Exposed as `"domain_filter": {…}` on the metrics surface (`src/handlers/bus_metrics.rs`),
with **explicit zeros** on a fresh daemon, and surfaced per call as
`recalled_from` / `registered_domain_count` on the recall response.
**Who reads it:** the operator and the agent, via `neurostrata-mcp bus-metrics`; the next
retrieval-related spec reads the recorded numbers.

#### 3.4.3 Is a domain registry worth its migration?

**Decided: yes (owner-ruled). The migration is lazy, in-band and zero-downtime** — the
bootstrap in §3.2.2. There is no offline migration, no daemon stop, and no data rewrite:
the first domain write in a namespace seeds the registry with every legacy domain that
namespace already carries. A namespace that never writes a domain never needs a registry
row, because the read path does not consult one.

#### 3.4.4 The distance band for deduplication

**Named task, not decided here:** `neurostrata-r7dj` — *"Local self-improvement Phase 2:
fix the distance band for near-duplicate detection."* Its subject is the
local-self-improvement spec's Phase 2, and its numeric cutoffs are embedder-specific by
that spec's own ruling. This spec does not touch it; naming it here is what keeps the
parent's deferral from rolling forward unmentioned.

---

## 4. Data contracts

| Contract | Where | Shape |
|---|---|---|
| `metadata.attempt` | `fact` metadata | `{approach, outcome, failure_mode, context}` — all four required by the declared shape; `outcome` ∈ `["failed"]`. A missing or malformed value **warns**, it does not reject the write. |
| `metadata.domain_registry` | `fact` metadata (the registry row) | `{domains: [...]}` — sorted, deduplicated slugs. The row is private and excluded from recall and from the snapshot body. |
| `register_domain` | `neurostrata_add_memory`, `neurostrata_task_complete` | optional boolean, default `false`. When `true`, the slug is added to the registry and the write proceeds. |
| `domain` (input) | `neurostrata_get_snapshot` | optional string or array of slugs; normalized before comparison. |
| Domain strap entry | the snapshot's `Domain strap` section | `{id, memory_type, domain, valid_from, access_count, snippet}`; ≤3 entries; `snippet` ≤120 chars; ≤2 entries per domain in `recent_domain` mode. |
| `recalled[]` snippet (parent) | the `append_log` response | prefixed with `"[FAILED ATTEMPT] "` when the row carries a valid `metadata.attempt` — prefixed **before** truncation. |
| `"domain_filter"` | the metrics surface | four integer counters, explicit zeros. |

---

## 5. Implementation plan

Ordered. Each step is independently testable. Steps marked **[P]** are the parent spec's
(`neurostrata-426k`); steps marked **[N]** are this spec's.

| # | Step | Where |
|---|---|---|
| 0 | **[P]** Prerequisite: the rule-honesty hardening + legacy backfill lands | task `neurostrata-bzdn` |
| 1 | **[P]** Domain slug normalizer, applied on write and on comparison | new helper + `src/server.rs:1279-1281`; `src/store/ladybug.rs:1070-1085` |
| 2 | **[N]** Registry row helpers: the deterministic v5 id, the lookup, the bootstrap seed (list + normalize + union), and the registry update (`store.upsert`) | new module beside the store; `src/traits.rs:192`, `:195`, `:158` |
| 3 | **[P+N]** Vocabulary **one** bump 4 → 5: `fact.fields.topology` (P), `fact.fields.attempt`, `fact.fields.domain_registry` (N); the assert at `src/traits.rs:263`; `tool_summaries.add` and `tool_summaries.memory_type` | `src/schemas/memory-vocabulary.v3.json:2`, `:66-69`, `:158`, `:161`; `src/traits.rs:263` |
| 4 | **[N]** The write-path registry check: bootstrap when absent, reject when present and unregistered, `register_domain: true` to override — on both write paths that accept a domain | `src/server.rs:1279-1325`; `src/task/mod.rs:1130-1155` |
| 5 | **[P]** Optional domain filter on search (store + MCP tool) with the soft fallback and the explicit mode | `src/traits.rs:165-172`; `src/store/ladybug.rs:1004`; `src/server.rs:1956` |
| 6 | **[N]** Registry row excluded from recall candidates and from the snapshot body | `src/store/ladybug.rs:1087-1095`; `src/server.rs:1441-1447` |
| 7 | **[P]** `handle_append_log(arguments, emb, store)`; dispatch widening; `namespace` in the input schema; the recall rides the response | `src/buffer.rs:129`, `:171-177`; `src/server.rs:373-380`, `:664`, `:2051` |
| 8 | **[N]** The `metadata.attempt` shape check → soft warning; the `[FAILED ATTEMPT]` rendering in `format_memory` and in every truncated snippet; `attempt` joins the canonical dedup key | `src/store/ladybug.rs:424`; `src/server.rs:2222`; `src/buffer.rs:171-177`; `src/server.rs:1152-1173` |
| 9 | **[N]** The snapshot domain strap: the `domain` input, the two modes, the selection, the explicit zero, the `Registered domains (N)` header | `src/server.rs:153-161`, `:1454-1493` |
| 10 | **[N]** The `domain_filter` counters and their surface | new module beside the search path; `src/handlers/bus_metrics.rs` |
| 11 | **[N]** The mesh import step unions an imported record's domain into the local registry | `src/synaptic_mesh/import.rs` |
| 12 | Tests (§6) | `src/server.rs`, `src/buffer.rs`, `src/store/ladybug.rs`, `src/schemas/` |
| 13 | Docs: the honest-recall note, the registry note; `CHANGELOG.md` | repo docs |

---

## 6. Acceptance criteria

1. A `fact` with a well-formed `metadata.attempt` round-trips, exports, and survives a
   SynapticMesh export/import round-trip (`memory_type` stays `fact`).
2. A `fact` with a malformed `metadata.attempt` is **written** and emits a
   `WriteShapeWarning`.
3. `vocabulary_version` is 5, `src/traits.rs:263` asserts 5, and the vocabulary declares
   `fact.fields.topology`, `fact.fields.attempt` and `fact.fields.domain_registry`.
4. A search for *"how do we do X"* in a namespace holding both a positive `fact` about X
   and a negative exemplar about X returns **both**, and the negative one renders the
   `Attempt [FAILED in <context>]: …` line.
5. Every truncated snippet of a negative exemplar begins with `"[FAILED ATTEMPT] "`, and
   is ≤ 120 characters **including** the prefix.
6. A negative exemplar about X never exact-match blocks a positive `fact` about X.
7. In a namespace with no registry row and an existing memory carrying
   `domain: "Database Access"`, writing a memory with `domain: "Auth"` succeeds and
   creates the registry row with `["auth", "database-access"]`.
8. A subsequent write in that namespace with `domain: "storage"` is **rejected** with the
   registered list, and no row is inserted; repeating it with `register_domain: true`
   succeeds and the registry becomes `["auth", "database-access", "storage"]`.
9. The registry row never appears in search results or in the snapshot's top-5 body, and
   `get_snapshot` renders `Registered domains (3): auth, database-access, storage`.
10. The registry row is not exported to the mesh (`eligibility: private`), and an
    imported shared record's domain is unioned into the local registry.
11. With 5 memories at `access_count: 10` and one new `fact` at `access_count: 0` with a
    domain, `get_snapshot` shows all five in the top-5 body **and** the new fact in the
    domain strap — with and without an explicit `domain` argument.
12. A domain-tagged memory already in the top-5 is not duplicated in the strap.
13. `access_count` is unchanged, row for row, across a `get_snapshot` call.
14. The strap never exceeds 3 entries, and never 2 from one domain in `recent_domain`
    mode; an empty strap renders an explicit zero and names its mode.
15. The metrics surface reports `"domain_filter"` with four explicit zeros on a fresh
    daemon, and the counters move on domain-filtered calls.
16. `neurostrata_search_memory` with no `domain` argument behaves exactly as today.
17. This spec contains no deferred item: §11 is the closed list, and every entry in it is
    either decided here or a named task.

---

## 7. Failure modes

| Failure | Behaviour | Safety |
|---|---|---|
| `metadata.attempt` is malformed | Warning; the `fact` is stored | The claim survives; the shape is correctable by supersede. |
| A search for *"how do we do X"* surfaces a negative exemplar | It is returned with the `Attempt [FAILED in …]` line and the `[FAILED ATTEMPT] ` snippet prefix | Additive; the positive corpus is never displaced and the row cannot read as an instruction. |
| A snippet is truncated mid-sentence | The prefix is already in place | Truncation cannot invert the polarity. |
| An unregistered domain slug is written | Rejected, with the registered list and the `register_domain` remedy | The wrong slug never enters the store; the caller is told exactly what to do. |
| A namespace's first domain write happens with 20 legacy free-text domains | The bootstrap seeds the registry with all of them | No legacy domain is orphaned; the write still succeeds. |
| The registry row is deleted or the namespace is wiped | The next domain write re-bootstraps | Self-healing; no manual repair step. |
| Two agents register a domain concurrently | The registry update is a read-modify-write under the store's lock | No lost update. |
| The strap's mode is wrong for the caller's intent | The mode is printed in the section header | The caller can see which selection produced the rows. |
| The strap would show only one domain | `recent_domain` mode caps at 2 per domain | A narrow namespace cannot consume every slot. |
| The domain filter would empty the candidate set | The filter is skipped; `recalled_from: "vector"` | Recall is never blind, and the substitution is reported. |
| The mesh imports a record with an unseen domain | The slug is unioned into the local registry | No shared record is refused for a local vocabulary gap. |
| The vocabulary bump breaks a consumer | One assert at `src/traits.rs:263` | Localized, compile-time, greppable. |

---

## 8. Migration and blast radius

- **No storage migration.** The registry is seeded lazily, in-band, on the first domain
  write; comparison-time normalization keeps the read path unchanged.
- **Vocabulary 4 → 5, once** — one assert plus any external consumer of the schema. The
  parent's `topology` rides the same bump because the parent is unbuilt.
- **`VectorStore`** — no signature change (the registry uses `get`/`list`/`upsert`); the
  parent's domain filter uses the default-method form (parent §4.3.2).
- **`neurostrata_add_memory` / `neurostrata_task_complete`** — one optional boolean
  (`register_domain`).
- **`neurostrata_get_snapshot`** — one optional property (`domain`) and one new section.
- **`neurostrata_append_log`** — the parent's `recalled` array; snippets gain a prefix
  for negative rows.
- **The mesh** — no wire change; the registry row is private; the import step unions
  domains.
- **No new dependencies; the build stays at its 14-warning baseline.**

---

## 9. Dependencies and sequencing

**Lands first:** `neurostrata-bzdn` — the rule-honesty hardening plus the legacy backfill
(the gate runs namespace-wide, so tightening it flags legacy rows until they are
backfilled).

**Prerequisite for this spec:** `neurostrata-426k` — the parent spec's implementation
(steps 1, 5 and 7 above). The registry, the strap and the negative-exemplar rendering
all build on the parent's slug normalizer, domain filter and `append_log` recall.

**Order within this spec:** 1 → 2 → 3 → 4 → 6 → 8 → 9 → 10 → 11, with 5 and 7 landing
from the parent. The strap (9) needs the normalizer (1); the registry check (4) needs
the registry helpers (2) and the vocabulary declaration (3).

**Already landed and orthogonal:** `c446b2c` — episode siblings via `EXTRACTED_FROM`.

---

## 10. Instrumentation

| Metric | Definition | Surface | Threshold | Reader |
|---|---|---|---|---|
| `recall_hit_rate` | recalled ids later promoted via `get_memory` ÷ recall responses | recall promotion path (`src/server.rs:2652`) | < 10% over 30 days with ≥50 tagged turns → tighten the budget | operator; the next retrieval spec |
| `domain_queries_total` | recall/search calls carrying a domain filter | metrics surface | — | operator; agent |
| `domain_narrowed_hits` | filter matched ≥1 row and narrowed the set | metrics surface | — | operator; agent |
| `domain_fallback_vector` | filter matched 0 rows; fell back to vector | metrics surface | a persistently high rate means the domains are wrong or sparse | operator; the next registry spec |
| `single_domain_bypasses` | calls in a namespace whose registry holds ≤1 domain | metrics surface | — | operator |

---

## 11. The closed list (nothing here is deferred)

| Item | Closed by |
|---|---|
| Q5 — negative exemplars | this spec, §3.1 |
| The per-namespace domain registry | this spec, §3.2 |
| The `get_snapshot` topic strap | this spec, §3.3 — shipped as one strap with two modes |
| The domain-scoped snapshot lane | this spec, §3.3 — the same strap |
| §11.1 — is the `append_log` seat sufficient? | decided, §3.4.1, with a metric and a threshold |
| §11.2 — does a domain filter pay? | instrumented, §3.4.2, §10 |
| §11.3 — is the registry worth its migration? | decided, §3.4.3 — the migration is lazy and in-band |
| §11.4 — the distance band | task `neurostrata-r7dj` |
| The parent spec's own implementation | task `neurostrata-426k` |
| The rule-honesty hardening and the legacy backfill | task `neurostrata-bzdn` |

---

## 12. Panel record

**Seats.** Argon (Gemini 4 Argon via Copilot) — `ses_ea35a588cffew7HD9kXspT5u0X` · K3
(Kimi K3 via Copilot) — `ses_ea35a4090ffegJVw7KMRSiwtvf`. Both answered the same brief
blind to each other; the orchestrator arbitrated.

**Agreement.** Both chose a `fact` carrying a declared typed sub-object for A and
rejected a new `memory_type` (mesh closure) and a rule-negation (overclaiming); both
rejected a store table, a config file, a `rule` row and a custom type for B; both
rejected replacing the top-5 cut; both folded C and D into one mechanism; both decided
E.1–E.3 and named E.4 as a task.

**Arbitrations.**

1. **B's strictness.** K3 read the parent's bootstrap rule per-slug (every new slug
   auto-registers) and paired it with a soft warning. That reading leaves the registry
   with no teeth — nothing can ever fail — and the parent's own words say *"a declared
   registry row **per namespace**"*. Adopted Argon's reading: namespace-level bootstrap,
   seed with the legacy domains, **reject** an unregistered slug, with
   `register_domain: true` as the escape hatch. Argon's reason for preferring a
   rejection over a warning is verified and load-bearing: the store's warning channel
   writes to stderr (`src/store/ladybug.rs:529-530`), which an MCP agent never reads.
2. **C + D's shape.** K3 declined C and shipped a per-domain lane of up to 6 rows;
   Argon merged C and D into one strap of ≤3 rows with two modes. Adopted Argon's merge
   (one mechanism, one budget, an explicit mode), plus K3's per-domain cap inside
   `recent_domain` mode (≤2 per domain) so one domain cannot take every slot.
3. **The negative marker.** K3 added a separate `polarity` scalar beside `attempt`;
   Argon used `attempt.outcome` alone. Adopted Argon's: one marker, closed enum
   `outcome ∈ ["failed"]` — two markers could disagree, and polarity without an attempt
   is the under-structured case both seats rejected.
4. **The metrics' home.** Argon put the domain counters on the bus-metrics surface. The
   bus's metrics are its own eight integers (`src/handlers/bus_metrics.rs:20-23`) and
   know nothing about domains, so the counters live in a module beside the search path
   and are added to that surface. Adopted Argon's four counters; corrected their home.
5. **The registry row's vector.** Argon proposed a zero vector. A real embedding of the
   row's content is used instead, so no zero vector ever enters the store.

**Facts the panel refuted or corrected.**

| Fact | Correction |
|---|---|
| The brief cited the closed relation loop at `src/store/ladybug.rs:445-452` | It is the four-key relation array at `:455-462`; `:445-452` is the `metadata` root check. (Argon) |
| The brief cited `increment_access_count` at `src/store/ladybug.rs:1993` | It is at `:2003`. (Argon) |
| `src/synaptic_mesh/record.rs:26-28` names `structural` as a mesh memory type | The binding set is three (`src/synaptic_mesh/import.rs:61`); `structural` is rejected at import. Doc corrected in commit `c5acd4f`. (K3) |
| A mesh row may carry a non-UUID id | Rows whose id fails `Uuid::parse_str` are skipped (`src/synaptic_mesh/worker.rs:180-183`) — the registry row therefore uses a deterministic **UUID v5**. (Argon) |

**Brief:** `docs/designs/2026-10-21-panel-brief-recall-closure-and-negative-exemplars.md`
· **Parent brief:** `docs/designs/2026-10-20-gap-proactive-recall-and-access-topology.md`
· **Parent spec:** `docs/superpowers/specs/2026-10-21-proactive-recall-and-access-topology-design.md`
· **Ruling:** memory `bca58f91`
