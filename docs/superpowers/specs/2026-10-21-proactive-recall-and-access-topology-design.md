# Proactive recall and access topology: a design spec

**Status:** designed. Panel-ruled (Argon / Gemini 4 + K3 / Kimi K3 + orchestrator), unanimous on every open point.
**Date:** 2026-10-21 · **HEAD at authoring:** `b3904d8`
**Brief:** `docs/designs/2026-10-20-gap-proactive-recall-and-access-topology.md`
**Panel record:** `docs/designs/2026-10-20-argon-k3-panel-verdicts.md` · **Q3 ruling:** memory `5d25375d`
**Tracks:** task `neurostrata-9enq` (this spec) · task `neurostrata-bzdn` (the honesty hardening, ships first)

---

## 0. Summary

NeuroStrata can store a fact and still never show it to the agent that needs it. This spec closes that hole for the one moment it matters — the transition to a new topic — and it gives the access-topology claim a representable, discoverable shape.

Four decisions, all unanimous:

| | Decision |
|---|---|
| **Q1 Trigger** | The recall rides the **`neurostrata_append_log` response**. That is the only call AGENTS.md §3 mandates at a topic transition, and its `tags` array already carries the topic. The response gains a bounded `recalled` array. |
| **Q2 Shape** | The access-topology claim is a **`fact`** carrying a **declared** typed sub-object `metadata.topology` — not a new memory type, not a new relation, not an undeclared convention. |
| **Q4 Linkage** | Topic linkage rides **`metadata.domain`**, which becomes a normalized (slug) and filterable key. The per-namespace domain registry is **deferred**. |
| **Q3 Enforcement** | **Referenced, not shipped here.** The obligation lives as an honestly-enforced `rule` in the consumer namespace, and the operational fix is upstream — §4.1 and §4.3 *are* that fix. The gate hardening ships separately and first under `neurostrata-bzdn`. |

**Q5 (negative exemplars) is deferred** to a later spec.

The through-line: **nothing new is introduced where something already exists.** The topic switch is already written; the domain is already written; the guard already knows how to read typed fields off a rule; the Episodic Buffer already exists as the audit trail. This spec wires them and declares one sub-object.

---

## 1. The gap

Two sub-gaps, one observed episode.

**Gap A — proactive recall.** Relevant knowledge is surfaced at session start (the mandatory pre-flight snapshot) or on an explicit query. It is **not** surfaced at a *topic transition*, which is exactly when an agent needs it. The mechanism is missing, and the most common reason it is missing is mechanical, not conceptual (§3.3): a newly written fact has `access_count: 0`, and the snapshot sorts strictly by `access_count` descending and cuts to five (`src/server.rs:1476-1491`). Even correctly-stored knowledge is cut from the one call AGENTS.md §3 makes mandatory.

**Gap B — access topology.** "Component X reaches system Y by channel Z under credential boundary W, because reason R" has no minimal representable structure. The relation vocabulary is closed and custom relations are **not materialized**; the memory vocabulary admits custom types but an undeclared custom type is invisible to the agent. So the claim either cannot be written or cannot be found.

The observed episode (the brief's §2) is an agent asserting an access-path claim without consulting remembered topology. **This spec does not claim any consumer system is misconfigured**; the access path in that episode is an unverified observation, not a finding (§2, guardrail 2).

---

## 2. Scope, non-goals, guardrails

**In scope:** Q1 (Trigger), Q2 (Shape), Q4 (Linkage), designed as one coherent change; Q3 referenced by wire id; the plumbing those three require.

**Out of scope:**

1. **Q5 — negative exemplars.** Deferred to a later spec. It is a fourth, orthogonal gap: a polarity field changes search-ranking semantics and the vocabulary contract, and it is not needed to close the recall hole.
2. **The Q3 gate hardening.** Shipped separately and first under `neurostrata-bzdn`; this spec references the `rule-honesty` wire by id (§4.4).
3. **A `get_snapshot` topic lane, and the domain-scoped snapshot lane.** Deferred with reasoning in §4.1.4.
4. **A per-namespace domain registry.** Deferred with reasoning in §4.3.3.
5. **Any new relation, any new memory type, any new trigger enum value.** See §3 and the decision tables.

**Guardrails (from the brief's §5, binding on this spec):**

1. **No consumer-system facts belong in the `NeuroStrata` namespace.** A topology claim about a downstream system is a fact about that system; it belongs to that project's namespace. NeuroStrata's obligation is the *mechanism* that surfaces such facts at the right moment.
2. **No claim is made that any consumer system is misconfigured.**
3. **This spec changes no behaviour until it is implemented**; the panel evaluated, and the design is now written down.
4. **No consumer vocabulary is imported.** `lane` — and any product name — is not a NeuroStrata concept; `domain` and `namespace` are.

---

## 3. The substrate (verified; this spec does not re-propose it)

Everything below was verified against source at `b3904d8`. The panel was instructed not to re-propose any of it. **The claims are as of that commit; the anchors are live against the working tree as of 2026-10-22 — verify each claim at its anchor.** Where this spec's own implementation changed a row, the row carries a *[changed]* marker naming the section that describes the new behaviour.

| Constraint | Anchor |
|---|---|
| Custom memory types are storable today but **invisible** to the agent — it learns vocabulary only from `tool_summaries.memory_type` and the tool description | `src/schemas/memory-vocabulary.v3.json:164-167`, `:151` (`recommended_custom_type_form`); `src/store/ladybug.rs:1462-1477` (stored unchanged), `:4083` (the test) |
| The relation set is closed and **custom relations are not materialized**; the edge tables are hardcoded to `GOVERNS, CONTAINS, RELATES_TO, EXTRACTED_FROM` | `src/schemas/memory-vocabulary.v3.json:159`; `src/store/ladybug.rs:1001-1008`; `src/traits.rs:309-330` (a test asserting exactly four) |
| The **mesh is closed** over shareable types — `SHAREABLE_MEMORY_TYPES = ["rule", "fact", "procedure"]` — and an unknown `memory_type` is **rejected at import time** with a hard `Err` | `src/synaptic_mesh/import.rs:61`, `:649-655` (the rejection); `src/synaptic_mesh/record.rs:26-28` (the contract) |
| `procedure.trigger` is a closed enum with no topic-entry value; `next_due` is honest only for `every-n-days:N`; procedures **never auto-fire** | `src/schemas/memory-vocabulary.v3.json:100-109`; `src/tools/procedure.rs:91`, `:135` |
| Procedures surface on the snapshot strap only | `src/server.rs:1471` |
| `access_count` is incremented **only** by the MCP search handler and the get-memory handler — never by `VectorStore::search` itself | `src/server.rs:2050`, `:2706`; `src/store/ladybug.rs:2160` |
| New memories are stamped `access_count: 0`; the snapshot sorts by `access_count` and cuts to 5 | `src/server.rs:1125-1132` (`stamp_new_memory`), `:1476-1491` |
| `domain` is written on add and echoed to export, and — as of `b3904d8` — **never filtered retrieval** *[changed: this spec adds the optional domain filter on search (§4.3) and the `domain` argument on `neurostrata_search_memory`]* | `src/server.rs:1281-1283`; `src/store/ladybug.rs:2011-2012` (read), `:2064` (emitted) |
| The Topic Switch marker is written on tagged turns and **never read back**; `current.md` *does* have a production reader — the episodic-pointer echo subscriber scans a buffer file's tail for pointer idempotency (`src/events/subscribers/episodic_pointer_echo.rs:214-215`) — but that reader never reads the Topic Switch marker back. *(The "no production reader" half of this row was already false at `b3904d8`.)* | `src/buffer.rs:70-76`; `src/server.rs:371` |
| The guard is **action-time only** — there is no claim-time surface | `src/server.rs:384`; `src/guard/validator.rs:43` |
| `fires_on` is a closed reminder enum; reminders ship as a bootstrap payload, not as a runtime injection | `src/task/wiring.rs:49`, `:135-168`; `src/task/mod.rs:1748-1751` |

---

## 4. The design

### 4.1 Q1 — Trigger: the recall rides the `append_log` response

#### 4.1.1 The seat

`neurostrata_append_log` is the only call that **fires at a topic transition**:

- AGENTS.md §3 (Continuous Backup Protocol) mandates it, and says to pass `tags` when a topic switch occurs (`src/server.rs:371-382`);
- the handler parses `tags` (`src/buffer.rs:151-160`);
- the Topic Switch marker is injected on non-empty tags (`src/buffer.rs:74-76`).

Every alternative seat was examined and rejected:

| Option | Why not |
|---|---|
| A new `ThalamicPulse` variant + subscriber | The bus taxonomy is storage-mutation-shaped; a topic transition is not a storage mutation. And a subscriber has **no return channel** into the agent's context — MCP is strict request/response (`src/server.rs:517-741`). A pulse could not deliver the knowledge. |
| A new `procedure.trigger` value | `next_due` is honest only for `every-n-days:N`; a "topic-entry" trigger would be owed-at-`now` forever. Procedures still do not auto-fire. |
| A new `fires_on` reminder | The enum is closed; reminder text is injected unconditionally and costs context on every session. |
| A `get_snapshot` topic strap | `get_snapshot` is a **session-start** surface. Reaching it at a topic change requires the agent to make an *extra* call carrying a topic hint, which nothing in AGENTS.md instructs mid-session — so it is advisory, not a trigger. Deferred (§4.1.4). |

#### 4.1.2 The response contract

The existing response object (`src/buffer.rs:310-317`: `appended`, `file`, `size_bytes`, `rolled`, `tags`) gains one field:

```json
{
  "appended": true,
  "file": "…/current.md",
  "size_bytes": 1841,
  "rolled": false,
  "tags": ["auth", "database"],
  "recalled": [
    { "id": "…", "memory_type": "fact", "domain": "database-access", "score": 0.41, "snippet": "…" }
  ],
  "recalled_from": "domain"
}
```

`recalled` is **always present** — an explicit empty array when `tags` is empty or nothing matched. Silence about "nothing was recalled" would be a lie the caller has to guess about (the repo's explicit-zero convention).

`recalled_from` is present **exactly when a search ran**. It names the mode that produced the rows: `"domain"` when the domain filter narrowed the candidates, `"vector"` when the filter matched nothing and was skipped. It is **absent** when no search ran (an untagged turn, or every tag normalized away) and when the search failed — in neither case did a search produce rows, so there is no mode to report. Reporting `"vector"` there would claim a vector search that never happened, which is the misreport §4.3.5 exists to prevent. *(Amended 2026-10-21 after the plan review: Argon and K3 both found the unconditional field dishonest in the no-search and error cases.)*

**Bounds:** at most **3** entries; `snippet` at most 120 characters. Worst case context cost ≈ 150 tokens, and only on tagged turns.

**Retrieval:** the recall calls `VectorStore::search_with_domain` **directly** — vector-distance ranked with the capped usage boost (`src/store/ladybug.rs:138-151`), with the domain filter of §4.3 applied as the filter's key set. The store over-fetches and narrows the candidate set in Rust after the vector query returns (`src/store/ladybug.rs:129-135`, `:172-177`), so the filter is a post-query narrowing, not a change to the query itself. Because it queries the store rather than the snapshot, it **bypasses the `access_count` top-5 cut by construction** — which is the §3.3 hole, closed.

**Only direct matches are recalled.** The store returns primaries *and* bounded graph expansions — one-hop, episode siblings, two-hop governs (`src/store/ladybug.rs:525-544`). An expansion row is context for a matched row, not the matched fact: a governed AST `file` node carries no `metadata.domain` and a synthetic score of 1.0. The recall therefore keeps only the results whose evidence carries `EvidenceKind::DirectMatch` — the same test the search handler uses to decide what counts as a direct hit (`src/server.rs:2043-2044`). Without it, a namespace with fewer than three primaries would recall governed nodes it never matched. *(Amended 2026-10-21 after the plan review: Argon's Q3.)*

**Query construction:** the query text is the normalized tags joined by a space (e.g. `"auth database"`), embedded **once** — one embedding call per tagged turn, no more. The same normalized tags are the domain filter's key set (§4.3.1), so a tagged turn is searched by both meaning and key in a single pass.

#### 4.1.3 The invariants (how it degrades when wrong)

1. **Additive only.** The recall never displaces, hides or suppresses anything the agent would otherwise have seen. A wrong recall costs ≤ 3 compact entries of noise; it never costs a wrong suppression.
2. **No `access_count` mutation.** `access_count` is incremented only by the MCP search handler (`src/server.rs:2050`) and the get-memory handler (`:2706`); the recall path touches neither. A wrong match therefore cannot self-reinforce into a future snapshot's top-5.
3. **Pointers, not payloads.** Each entry is an `id` + a short snippet. Promotion happens only when the agent explicitly calls `neurostrata_get_memory` (which does increment `access_count`). Recall points; it does not rank.
4. **Bounded and deterministic.** ≤ 3 entries, ≤ 120 characters each, no retries, no LLM call, no second embedding beyond the one already needed for the query.

#### 4.1.4 What is deferred, and why

A `get_snapshot` topic strap (the other seat's proposal) is **not** in this phase. It would duplicate the recall path, and it would require teaching the agent a new mid-session protocol. Revisit only if the `append_log` seat is shown insufficient by observation (§11).

For the same reason, the **domain-scoped snapshot lane** is deferred: in this phase the `access_count: 0` bypass comes from `VectorStore::search_with_domain` (used by the recall and by `search_memory`'s domain filter), not from a new snapshot lane.

#### 4.1.5 Plumbing

- The dispatch at `src/server.rs:665-666` passed only `arguments` before this spec; it now passes `emb.clone()` and `store.clone()` — exactly as its neighbours already do (`task_setup` at `:657-663`, `guard_validate` at `:668-674`). *(Implemented by the plan's Task 6.)*
- `handle_append_log(arguments, emb, store)` gains the two parameters.
- **Namespace:** the `neurostrata_append_log` `inputSchema` (`src/server.rs:374-381`) has `content`, `project_root`, `tags` and an **optional `namespace`**; when the `namespace` is absent — or blank — the handler falls back to `project_root.file_name()`, resolved through `resolve_namespace(&store, …)` (`src/server.rs:2105`). Without it the handler cannot know which namespace to search. *(Implemented by the plan's Task 6.)*

---

### 4.2 Q2 — Shape: a `fact` with a declared `topology` sub-object

#### 4.2.1 The representation

```json
{
  "memory_type": "fact",
  "content": "The API gateway reaches the ledger service over mTLS inside the eu-west-1 VPC boundary, because the ledger ledger is only reachable from inside the VPC.",
  "metadata": {
    "domain": "service-access",
    "topology": {
      "component": "api-gateway",
      "target_system": "ledger-service",
      "channel": "mTLS",
      "credential_boundary": "eu-west-1 VPC",
      "reason": "the ledger accepts traffic only from inside the VPC"
    }
  }
}
```

**Declared, not conventional.** `memory_types.fact.fields.topology` is added to the vocabulary, mirroring the established `rule.fields.enforcement/source/guard` precedent (`src/schemas/memory-vocabulary.v3.json:42-64`); `vocabulary_version` goes 4 → 5 (`src/schemas/memory-vocabulary.v3.json:2`), the version assert at `src/traits.rs:312` is updated, and `tool_summaries.add` + `tool_summaries.memory_type` (`:164-167`) mention it so the agent **discovers** the shape.

#### 4.2.2 The three options, decided

| Option | Decision | Reason |
|---|---|---|
| A new top-level `topology` memory type | **Rejected** | The mesh is closed over its shareable types — `SHAREABLE_MEMORY_TYPES = ["rule", "fact", "procedure"]` (`src/synaptic_mesh/import.rs:61`) — and an unknown `memory_type` is **rejected at import time** with a hard `Err` (`:649-655`; the contract is documented at `src/synaptic_mesh/record.rs:26-28`). A new top-level type breaks mesh import unless the mesh wire contract is also changed — out of scope, and expensive under the mesh's honesty rules. A `fact` crosses the mesh today as a `fact` record — the mesh's closure is over the memory **type**, so a declared field on `fact` does not break it. The published record's `metadata` is the mesh's curated envelope (`eligibility`, `source`, `tags`; `src/synaptic_mesh/worker.rs:282-296`), so a declared field such as `metadata.topology` stays **node-local**: this spec does not change the mesh's wire format. |
| A metadata convention on `fact`, **undeclared** | **Rejected** | Undeclared metadata is invisible to the agent — the exact failure `domain` already exhibits (§4.3). A convention nobody is told about is not a vocabulary. |
| **A declared typed sub-object on `fact`** | **Chosen** | Discoverable (declared + surfaced in `tool_summaries`), mesh-safe (`memory_type` stays `fact`), zero storage migration, and it reuses the typed-sub-field pattern the vocabulary already established. |

#### 4.2.3 No new relation

Custom relations are not materialized (`src/schemas/memory-vocabulary.v3.json:159`) and the edge tables are hardcoded (`src/store/ladybug.rs:1001-1008`, `src/traits.rs:309-330`) — a new relation would be a write-only artifact, the same failure mode as the unread Topic Switch marker.

Topology rows bind to code through the **existing** path: `locations` → `metadata.governs` → `GOVERNS` edges (`src/server.rs:1057-1076`). A topology fact therefore reaches the AST file and symbol nodes through the one-hop graph expansion search already performs (`src/store/ladybug.rs:223-231`), whose neighbour query traverses any `(a:Memory)-[e]->(b:Memory)` edge.

#### 4.2.4 Validation and cost

`parse_edge_specs` (`src/store/ladybug.rs:970`) gains a shape check for `metadata.topology`; a malformed value raises a `WriteShapeWarning` (the struct is at `:949-955`, the warnings are emitted by the upsert at `:1482-1485`) and the durable `fact` write still succeeds. The claim is worth keeping even when its shape is imperfect; a rejected write would lose it.

Cost: one vocabulary file edit + the version bump + the assert + two `tool_summaries` strings. **Zero storage migration** — existing facts carry no `metadata.topology` and remain valid.

---

### 4.3 Q4 — Linkage: `metadata.domain` becomes a normalized, filterable key

#### 4.3.1 Normalization

`domain` (and `append_log` `tags`) normalize to **lowercase-kebab slug tokens** matching `^[a-z0-9]+(-[a-z0-9]+)*$`:

- **on write** — `src/server.rs:1281-1283` (the add path);
- **on read and comparison** — the filter normalizes the candidate row's stored value before comparing.

The **supersede** path needs no change and does not accept a new `domain`: it clones the old payload wholesale (`src/server.rs:1701`) and replaces only an explicit subset of metadata keys — `governs`, `related_to`, `contained_by` (`:1744`) — so a stored `domain` is carried over, not rewritten. Correcting a wrong `domain` therefore means a new write, not a supersede.

The read-side normalization is what makes this **migration-free**: legacy free-text values (`"Database Access"`, `"database_access"`) are normalized at comparison time, so existing rows match without a data migration or a grandfathering pass. `"Database Access"` and `"database-access"` become the same key — which collapses the typo class that motivates a registry.

#### 4.3.2 The filter

An optional `domain` filter is added to:

- `neurostrata_search_memory` (`src/server.rs:1967`), and
- the store's candidate loop (`src/store/ladybug.rs:103-217`).

`VectorStore::search` currently takes `(namespace, vector, limit, include_tasks, include_archived)` (`src/traits.rs:193-200`); the `include_*` flags are the precedent for another parameter.

**Implementation note (orchestrator):** extending that signature touches 5 `VectorStore` implementations and 12+ call sites. The cheaper honest variant is a **new trait method with a default implementation that delegates to `search`**, overridden only by `LadybugStore`. The spec permits either; the default-method form is preferred because it keeps the blast radius at one implementation.

#### 4.3.3 The registry is deferred

The per-namespace domain registry — a declared registry row per namespace, enum-validated `domain`, legacy grandfathering — is **deferred to a later phase**, pending observation data.

Why: it needs a new write path, a bootstrap rule (the first domain must be registered before it can be used) and a migration, and slug normalization already collapses the drift that motivates it at zero cost. Its residual value is catching a *wrong but well-formed* slug, which needs observation to justify.

Recorded for that later phase, so the bootstrap is not re-litigated: **the first write of any new domain auto-registers it when the registry row is absent; subsequent writes validate against the registry.** No bootstrap paradox, no migration.

#### 4.3.4 Rejected

| Option | Why not |
|---|---|
| Ride the episodic tag | `current.md`'s one production reader — the episodic-pointer echo subscriber (§3) — scans its tail for pointer idempotency and never reads the Topic Switch marker back, so there is nothing to ride. |
| Parse `current.md` at search time | Couples the daemon's DB engine to client filesystem paths (`project_root`). |
| A namespace convention | `namespace` already exists; it does not address **within-namespace** topic separation, which is the actual case. |
| A new vocabulary term | `domain` already exists; a new term forks the write-side convention for zero gain. |

#### 4.3.5 Soft fallback

If a domain filter would empty the candidate set, the filter is **skipped** and the unfiltered candidates are returned — a wrong domain must not blind recall. The store's search is a pure-vector match (`src/store/ladybug.rs:87`); the domain filter of §4.3.2 narrows that candidate set, and the soft fallback is a property of that filter — a code path this spec adds, not one that existed at `b3904d8` (`src/store/ladybug.rs:193-202` is where the mode is chosen). Because a silent fallback would misreport what was searched, the recall response states which mode produced it: `recalled_from: "domain" | "vector"`. *(This field is an orchestrator addition, not a panel decision; it exists because the repo's honesty rules forbid a silent substitution.)* When no search ran, or the search failed, the response carries **no mode at all** (§4.1.2) — an honest silence beats a mode that names a search that never happened. The two values map exactly onto the two domain-filter counters the recall-closure spec defines (`domain_narrowed_hits` / `domain_fallback_vector`): a call with no mode ran no filter and increments neither.

---

### 4.4 Q3 — Enforcement: referenced, not shipped here

The full ruling is memory `5d25375d` (unanimous: Argon, K3, orchestrator). In brief:

- **No claim-time guard surface.** Prose assertion is not an MCP tool call, so there is no interception point between the agent deciding to assert and the user reading the assertion. Owner ruling `0713723e` stands: the guard and the snapshot remain two separate channels, and the guard's rule filter (`src/guard/semantic.rs:76`) stays byte-identical.
- **The claim is descriptive.** The access-path claim is a `fact` (§4.2) in the **consumer** namespace. A fact carries no enforcement fields, so it cannot overclaim.
- **The obligation is a `rule`** with `metadata.source`, `metadata.enforcement: "PARTIAL"`, and `metadata.guard` naming an existing generic core reminder wire. No domain-specific core wire is permitted — Core stays project-agnostic (`docs/design-wiring-panel.md:189-192`).
- **The audit artifact is the Episodic Buffer**, via the already-wired `neurostrata_append_log` / `episodic-log` reminder (`src/task/wiring.rs:152-156`).
- **The honesty contract is hardened separately** under `neurostrata-bzdn`: a rule missing `enforcement`, or carrying a value outside `{ENFORCED, PARTIAL, NOT_ENFORCED}`, fails `task_validate`; `ENFORCED` requires an `AUTOMATIC_WIRES` id with `blocks == true`; `PARTIAL` requires a non-null `guard` in `known_wire_ids()`; `NOT_ENFORCED` requires `guard == null`; a new `AUTOMATIC_WIRES` id is added for the behavioral guard with `runs_in: Core`, `blocks: true`, and a `reason` scoped to the actions the guard actually validates; and `src/schemas/memory-vocabulary.v3.json:50,62` prose is updated in tandem.

**Sequencing:** that hardening and its legacy backfill land **first and separately** (the gate runs namespace-wide in the pre-push hook, `src/task/wiring.rs:92-97`, so tightening it flags legacy rows until they are backfilled). **This spec writes its own rule rows conforming to the tightened contract from day one.**

**The operational fix is upstream.** §4.1 and §4.3 are the mechanism that makes the knowledge available at the moment of assertion. No guard can do that work, and this spec does not pretend otherwise.

---

## 5. Data contracts

| Contract | Where | Shape |
|---|---|---|
| `recalled` | `neurostrata_append_log` response | array, 0–3 entries; **always present** |
| `recalled[]` entry | same | `{id, memory_type, domain, score, snippet}`; `snippet` ≤ 120 chars |
| `recalled_from` | same | `"domain"` \| `"vector"` — how the rows were selected; **present exactly when a search ran**, absent when no search ran or the search failed |
| `metadata.topology` | `fact` metadata | `{component, target_system, channel, credential_boundary, reason}` — all five required by the declared shape; a missing or malformed value warns, it does not reject the write |
| `metadata.domain` | all memory metadata | lowercase-kebab slug; normalized on write **and** on comparison |

---

## 6. Implementation plan

Ordered. Each step is independently testable.

| # | Step | Where |
|---|---|---|
| 0 | **Prerequisite for the rule row and the rule-honesty conformance test:** the `rule-honesty` hardening + legacy backfill lands. It does **not** gate the mechanism — the recall, the filter and the vocabulary change land independently of it | task `neurostrata-bzdn` |
| 1 | Domain slug normalizer; applied on write (the add path) and on comparison | new helper + `src/server.rs:1281-1283`; `src/store/ladybug.rs:103-217` |
| 2 | Optional `domain` filter on search (store + MCP tool), with the soft fallback when the filter would empty the candidate set | `src/traits.rs:193-200`, `src/store/ladybug.rs:45-217` (the filter narrows the candidate set after the vector query returns — the store over-fetches and filters in Rust, `src/store/ladybug.rs:129-135`); `src/server.rs:1967` |
| 3 | Vocabulary: `fact.fields.topology`, version 4 → 5, assert, `tool_summaries` | `src/schemas/memory-vocabulary.v3.json:2`, `:66-75`, `:164-167`; `src/traits.rs:312` |
| 4 | Shape check for `metadata.topology` → soft warning | `src/store/ladybug.rs:970`, `:949-955` |
| 5 | `handle_append_log(arguments, emb, store)`; dispatch widening; `namespace` in the input schema; query text = normalized tags joined, embedded once, and the same tags are the domain filter's key set; `recalled` + `recalled_from` in the response | `src/buffer.rs:133`, `:151-160`, `:310-317`; `src/server.rs:374-381`, `:665-666`, `:2105` |
| 6 | Tests (§7) | `src/buffer.rs`, `src/server.rs`, `src/store/ladybug.rs` |
| 7 | Docs: the honest-recall note; `CHANGELOG.md` entry | repo docs |

---

## 7. Acceptance criteria

1. A `fact` written with `domain: "Database Access"` and a tagged turn carrying `tags: ["database-access"]` recalls that fact **in the same namespace**, from the `append_log` response.
2. A fact with `access_count: 0` **is** recalled — the recall bypasses the `access_count` top-5 cut.
3. `recalled` is present on every `append_log` response, empty when `tags` is empty, and never longer than 3.
4. The recall path changes **no** row's `access_count` (assert before/after).
5. A `fact` with a well-formed `metadata.topology` round-trips, exports, and appears in `export_graph` with its `GOVERNS` edges to the ingested files.
6. A `fact` with a malformed `metadata.topology` is written **and** emits a `WriteShapeWarning`.
7. `vocabulary_version` is 5 and `src/traits.rs:312` asserts 5.
8. A topology `fact` survives a SynapticMesh round-trip **as a `fact` record** — its `memory_type` stays `fact`, so the mesh's type closure holds, and the import path preserves a record's metadata when it is handed one. The *published* record's `metadata` is the mesh's curated envelope (`eligibility`, `source`, `tags`; `src/synaptic_mesh/worker.rs:282-296`), so `metadata.topology` itself stays node-local — the topology is not published, by design.
9. A `rule` written under this spec passes the tightened `rule-honesty` check (`enforcement` present and valid, `guard` consistent with it).
10. Search with no `domain` argument behaves exactly as today.

---

## 8. Failure modes

| Failure | Behaviour | Safety |
|---|---|---|
| Tags are wrong / off-topic | ≤ 3 irrelevant entries | Additive; nothing suppressed. Cost is noise, not a wrong absence. |
| A wrong match is recalled repeatedly | `access_count` never moves | It cannot promote itself into future snapshots. |
| The domain filter would empty the candidate set | The filter is skipped; `recalled_from: "vector"` | Recall is never blind; the mode is reported. |
| The recall query is semantically poor | Empty or weak `recalled` | The agent still has the explicit search path; this is an accelerator, not the only route. |
| `metadata.topology` is malformed | Warning; the fact is stored | The claim survives; the shape can be corrected by supersede. |
| The vocabulary version bump breaks a consumer | One assert at `src/traits.rs:312` | Localized, compile-time, greppable. |
| The daemon is down | No recall, no snapshot | Pre-existing behaviour; unrelated to this change. |

---

## 9. Migration and blast radius

- **No storage migration.** `metadata` is stored as a JSON blob; normalization happens at comparison time, so legacy free-text domains keep working.
- **Vocabulary version 4 → 5** — one assert (`src/traits.rs:312`) plus any external consumer of the schema.
- **`VectorStore::search`** — 5 implementations, 12+ call sites; the default-method form (§4.3.2) keeps the change at one implementation.
- **`neurostrata_append_log` response** — one added field; JSON, additive, safe for existing consumers.
- **The append_log input schema** — one optional property; no existing caller breaks.
- **Rule rows written by this spec** — must conform to the tightened `rule-honesty` contract from day one (the legacy backfill is `neurostrata-bzdn`'s job, not this spec's).

---

## 10. Dependencies, sequencing, deferrals

**Lands first relative to this spec's own rule rows:** `neurostrata-bzdn` — the `rule-honesty` hardening plus the legacy backfill (NeuroStrata: 86 rule rows, 84 missing `enforcement`; keywest.health: 137 rows, 137 missing `enforcement`, 102 missing `source`). The dependency is one-directional and narrow: the hardening must land before a rule row written under §4.4 is written, because the tightened contract is what the row must satisfy. It does not gate the mechanism — the recall, the domain filter and the vocabulary bump are implementable against `b3904d8` without it.

**Already landed and orthogonal:** `c446b2c` — episode siblings via `EXTRACTED_FROM` (`EvidenceKind::EpisodeSibling`, `src/store/ladybug.rs:246-249`, `:398-463`).

**Deferred, with the reason recorded:** Q5 (§2.1); the `get_snapshot` topic lane (§4.1.4); the domain-scoped snapshot lane (§4.1.4); the per-namespace domain registry (§4.3.3).

**All four are now closed** — see §13. The deferrals above are kept as the historical
record of why each was parked at the time; the resolutions live in
`docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md`.

**Hard dependencies on unlanded work:** none. Q1, Q2 and Q4 are implementable against `b3904d8` as verified.

---

## 11. Open questions that need observation data, not a review

> **Closed 2026-10-21.** Each of the four below is now **decided** — questions 1–3 in
> the recall-closure spec (§3.4), question 4 as task `neurostrata-r7dj`. The text is
> kept as the record of what was open when this spec was written; it is not a standing
> deferral.

1. **Is the `append_log` seat sufficient?** It fires on every tagged turn, which may be more often than a topic actually changes. If the recall proves noisy, the lever is the strap budget (§4.1.2) before it is a different seat.
2. **Does a domain filter actually pay?** A namespace with one domain gains nothing from it. The filter's value is measurable only across namespaces with several domains.
3. **Is a domain registry worth its migration?** Decided by observing whether well-formed but wrong slugs actually occur (§4.3.3).
4. **The correct distance band for deduplication** remains unset — the local-self-improvement spec defers it deliberately, and neither seat invented one.

---

## 12. Panel record

**Seats.** Argon (Gemini 4 Argon via Copilot) — `ses_ea3a442ffffeaKuRgu06gfmyUG`, `ses_ea3a8c4f2ffeAF0lfw1UqQx7ZA`, `ses_ea3b9a8e9ffedJI9iUzCCCK8L1` · K3 (Kimi K3 via Copilot) — `ses_ea3a442feffeZQarF7L19EtRSl`, `ses_ea3a8c4f0ffeR3peKim7iDki8U`, `ses_ea3b9a8e8ffe52pVM3yE4o9qdH`.

**Rounds.** (1) Independent answers to Q1, Q2, Q4 — the two seats disagreed on all three. (2) Orchestrator arbitration: a synthesis taking the strongest point from each side. (3) Reconciliation: **all five open points AGREES from both seats, no dissent.** The Q3 ruling was reached in its own two rounds and is unanimous (memory `5d25375d`).

**Adjudicated facts** (verified by the orchestrator before being put to the panel):

The facts below are the substrate **as of `b3904d8`** — what the panel judged. This spec's own implementation changed five of them since; each changed row says so.

| Fact | Anchor |
|---|---|
| The mesh rejects an unknown `memory_type` at import | `src/synaptic_mesh/import.rs:61`, `:649-655` (the code); `src/synaptic_mesh/record.rs:26-28` (the contract) |
| `vocabulary_version` is 4, asserted in one place — *now 5* | `src/schemas/memory-vocabulary.v3.json:2`; `src/traits.rs:312` |
| `fact` has no `fields` object today — *now carries the declared `fields.topology`* | `src/schemas/memory-vocabulary.v3.json:66-75` |
| `neurostrata_append_log` receives only `arguments`; its neighbours receive `emb` + `store` — *the append_log dispatch now receives `emb` + `store` too* | `src/server.rs:665-666` vs `:657-663`, `:668-674` |
| The append_log input schema has no `namespace` — *it now has an optional `namespace`* | `src/server.rs:374-381` |
| The snapshot's straps are computed **before** the `access_count` sort and cut | `src/server.rs:1465`, `:1471`, `:1476-1491` |
| `VectorStore::search` takes no domain filter — *the trait now has `search_with_domain`, whose default implementation ignores the key set and reports `Vector`; `LadybugStore` overrides it to apply the filter* | `src/traits.rs:193-200` |
| `access_count` is incremented only by the two MCP handlers | `src/server.rs:2050`, `:2706` |

**Brief.** `docs/designs/2026-10-20-gap-proactive-recall-and-access-topology.md` · **Panel verdicts:** `docs/designs/2026-10-20-argon-k3-panel-verdicts.md` · **External review:** `docs/designs/2026-10-20-external-review-hyperskill.md`

---

## 13. Deferral resolutions (added 2026-10-21)

The owner's no-deferral ruling (memory `bca58f91`, `AGENTS.md` §9) closes this spec's
deferral set. Every item this spec parked is now decided, and none of it rolls
forward unmentioned.

| Deferred here | Closed in | How |
|---|---|---|
| Q5 — negative exemplars (§2.1) | `2026-10-21-recall-closure-and-negative-exemplars-design.md` §3.1 | A `fact` carrying a declared typed sub-object `metadata.attempt` (`approach`, `outcome`, `failure_mode`, `context`); `outcome` closed to `["failed"]` is the only negative marker |
| The `get_snapshot` topic lane (§4.1.4) | same spec §3.3 | One snapshot **domain strap** with two modes — `explicit` (the caller names a domain) and `recent_domain` — ≤3 entries, computed off the pre-cut list |
| The domain-scoped snapshot lane (§4.1.4) | same spec §3.3 | The same strap; it is what closes the `access_count: 0` hole at session start |
| The per-namespace domain registry (§4.3.3) | same spec §3.2 | One registry row per namespace — a **cache** of the domains the namespace actually uses — seeded lazily with every legacy domain on the first domain write; a slug in neither the cache nor the namespace's own data is rejected, with a `register_domain: true` escape hatch |
| §11.1 — is the `append_log` seat sufficient? | same spec §3.4.1 | Decided: two bounded seats; sufficiency measured by `recall_hit_rate` with a threshold |
| §11.2 — does a domain filter pay? | same spec §3.4.2, §10 | Instrumented: four named counters with explicit zeros |
| §11.3 — is the registry worth its migration? | same spec §3.4.3 | Decided: yes; the migration is lazy and in-band, with no offline migration |
| §11.4 — the distance band | — | Task `neurostrata-r7dj` (the local-self-improvement spec's Phase 2) |
| This spec's own implementation | — | Task `neurostrata-426k` (prerequisite for the recall-closure implementation, task `neurostrata-r30c`) |

The bootstrap rule this spec recorded so it would not be re-litigated is adopted as
written, read **per namespace**: the first domain write creates the registry; later
writes validate against it.
