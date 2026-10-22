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

**The vocabulary bump is two, and the order is fixed.** The parent spec's
`fact.fields.topology` is built first — the parent's implementation task
`neurostrata-426k` lands ahead of this spec's implementation — so the parent owns the
`vocabulary_version` **4 → 5** bump and this spec owns **5 → 6** for `attempt` and
`domain_registry`. This spec's premise when it was written ("`topology` is unbuilt, so
all three ride one bump") is now stale: `topology` ships with version 5, and this
spec's two fields ship with version 6. The single assert at `src/traits.rs:312` moves
with each bump, and there is no third bump.

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

## 2. The substrate (the claims were verified at `c5acd4f`; **this table's** anchors are live against the working tree as of 2026-10-22 — verify each claim at its anchor; the panel did not re-propose it)

> **Anchor scope.** Every anchor in this spec — this table's and the prose anchors in §0,
> §3, §5, §6, §7, §10 and §12 — is live against the working tree as of `dc01697`
> (2026-10-22); the prose anchors were re-derived by the `neurostrata-r30c` plan's first
> task. Verify each claim at its anchor. The dated spans marked "(at `c5acd4f`)" are the
> exception: they record what an earlier commit cited and are not live anchors.

| Constraint | Anchor |
|---|---|
| Vocabulary **v4**; `fact` carries **no** `fields` object today *[changed by neurostrata-r30c: the vocabulary is v6 and declares fact.fields.attempt and fact.fields.domain_registry; `fact` also carries `fields.topology`]* | `src/schemas/memory-vocabulary.v3.json:2`, `:66-83` |
| `rule.fields` = `enforcement` / `source` / `guard` — the typed-sub-field precedent | `src/schemas/memory-vocabulary.v3.json:39-64` |
| A semantic change requires a `vocabulary_version` bump; asserted in one place | `src/schemas/memory-vocabulary.v3.json:169`; `src/traits.rs:312` |
| The agent learns vocabulary **only** from `tool_summaries` and the tool description | `src/schemas/memory-vocabulary.v3.json:171` (the `tool_summaries` block), `:175` (the `memory_type` key inside it); overlaid onto the tools at `src/server.rs:456-501` |
| The mesh is closed over `["rule","fact","procedure"]`; an unknown `memory_type` is **rejected at import** | `src/synaptic_mesh/import.rs:61`, `:649-655`; contract `src/synaptic_mesh/record.rs:26-28` |
| The mesh's own import doc **contradicts its code**: the comment says an unknown type is "ignored and reported, never applied, never crash"; the code returns a hard `Err` | doc `src/synaptic_mesh/import.rs:57-60`; code `:649-655`. Task `neurostrata-wl4i` owns the reconciliation (§11). This spec is safe under **either** reading — a `fact` is shareable under both. |
| A mesh row whose id is not a parseable **UUID is skipped** — ids must be UUIDs | `src/synaptic_mesh/worker.rs:180-183`; `MeshRecord.id: Uuid` at `src/synaptic_mesh/record.rs:24` |
| Mesh export publishes only `eligibility: shareable`; absent or `private` reads as private | `src/synaptic_mesh/worker.rs:174-179` |
| Relations are closed; the four keys are hardcoded | `src/store/ladybug.rs:1001-1007` |
| `domain` is written raw on add *[changed by neurostrata-r30c: the domain is normalized to a slug on write, and the registry check reads the raw value captured before that normalization]* | `src/server.rs:1283-1284` (raw insert), `:1303-1305` (slug normalization; the comment at `:1299-1302` says the write never guesses a domain), `:1290-1298` (the raw-domain capture the check reads), `:1331-1345` (the check) |
| `domain` is read back and echoed to export; it never filters retrieval *[changed: `domain` is now an optional soft filter on search]* | `src/store/ladybug.rs:2011-2012` (read), `:2064` (emitted) |
| A row's vector is the embedding of its **content**, unchanged; nothing negates it | `src/server.rs:1149-1152` |
| The snapshot's straps are computed off the **pre-truncation** list; then sort by `access_count` desc and truncate to 5 | `src/server.rs:1520` (`task_head`), `:1526` (`procedures_due_strap`), `:1531-1547` (the `access_count`-desc sort), `:1699` (`truncate(5)`) |
| `get_snapshot`'s input schema gains an optional `domain`; its handler takes no embedder | `src/server.rs:153-162`, `:1474-1477` |
| `handle_append_log(arguments)` — no `emb`, no `store`; response `{appended, file, size_bytes, rolled, tags}` *[changed: the handler takes `emb` and `store`; the response carries `recalled` and, when a search ran, `recalled_from`]* | `src/buffer.rs:133-136` (signature), `:304-321` (response); dispatch `src/server.rs:668` |
| A malformed declared shape is a **soft** failure — `WriteShapeWarning`, the write still succeeds — and the warning goes to **stderr**, which an MCP agent never sees | `src/store/ladybug.rs:951` (the `WriteShapeWarning` struct), `:1106-1107` (the stderr emission) |
| `VectorStore::search(namespace, vector, limit, include_tasks, include_archived)` | `src/traits.rs:175` (`pub trait VectorStore`), `:193-199` (`search`); impl `src/store/ladybug.rs:1581` |
| `store.get(namespace, id)` and `store.list(namespace, user_id)` exist | `src/traits.rs:223` (`get`), `:220` (`list`) |
| `access_count` is incremented only by the MCP search handler and the get-memory handler | `src/server.rs:2261`, `:2925`; `src/store/ladybug.rs:2160` |
| `create_new_namespace: true` is the established **escape-hatch** precedent for a write that would otherwise be refused *[changed by neurostrata-r30c: `register_domain` is the same shape, and the registry check sits after the namespace-existence check]* | `src/server.rs:117`, `:1221-1226`, `:1240-1241`, `:1324-1326`, `:1331-1345` |
| Structural types are excluded from recall candidates **by type, at query-build time** — the set is `directory`, `file`, `markdown`, `freshness_flag`, `guard_event` (not `symbol`), and it is a fixed `const [&str; 5]` with **no per-row hook** | definition `src/store/ladybug.rs:1270-1276`; the excluded-type list is built at `:76-81` and applied in the query at `:87` |
| The store's **per-row metadata filter seam** — `is_retired_at`, `is_archived`, `is_mesh_conflicting` — is where a metadata-keyed row class is dropped, so it never consumes a *result* slot (it may consume one over-fetch slot: `fetch_limit = limit * 10`) | `is_archived` `src/store/ladybug.rs:1281`, `is_mesh_conflicting` `:1291`, `is_retired_at` `:2273`; applied at `:114`, `:121`, `:124` |
| The duplicate check is **semantic**: candidates scoring `>= 0.85` are handed to the judgment provider. There is **no canonical exact-match key** anywhere in `src/` | `src/server.rs:1162` (the `>= 0.85` filter), `:1166` (`checker.check_duplicates(payload, &similar)`); `SearchResult` carries each candidate's payload at `src/traits.rs:58-66` |
| `format_memory` is the rendering path for a returned row | `src/server.rs:2324` |
| The bus metrics surface serializes the bus's own counters (eight integers) *[changed by neurostrata-r30c: composite body now also carries `domain_filter` and `recall`]* | `src/handlers/bus_metrics.rs:20-29` |

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
  1. `format_memory` (`src/server.rs:2324`) prints, immediately after `Type: fact`,
     `Attempt [FAILED in <context>]: tried '<approach>' -> failed with '<failure_mode>' (single episode, not a global rule)`.
  2. Every truncated `snippet` (§3.3, and the parent's `recalled[]`) is prefixed with
     `"[FAILED ATTEMPT] "` **before** truncation, so truncation can never strip the
     marker and leave the tail of a failure reading as an instruction.
- **Deduplication.** The store's duplicate check is **semantic** — the candidates scoring
  `>= 0.85` are handed to the judgment provider, which compares them against the
  incoming payload (§2; there is no canonical exact-match key to extend, and none is
  invented here). The candidate set is therefore **partitioned by attempt-presence**
  before the check: a row carrying a valid `metadata.attempt` is compared only against
  rows that carry one, and a row without one only against rows without. A negative
  exemplar about subject X can consequently never block a positive `fact` about subject
  X, while two attempts about the same approach are still compared with each other.
  The partition uses the **same** predicate as the `[FAILED ATTEMPT]` prefix: a row whose
  `metadata.attempt` is malformed is neither rendered as a negative nor partitioned as
  one — it is a plain row (the store accepts a malformed attempt with a soft warning:
  `src/store/ladybug.rs:979` — the `parse_edge_specs` seam where that check lands; the
  `WriteShapeWarning` at `:951-955`, emitted to stderr at `:1106-1107`). Rendering and
  partitioning must not disagree, or a row would be shown to the agent as a failure while
  being compared against successes.

#### 3.1.4 Validation

`parse_edge_specs` (`src/store/ladybug.rs:979`) gains a shape check for
`metadata.attempt` on `fact` rows: an object with non-empty `approach`, `failure_mode`,
`context`, and `outcome == "failed"`. A malformed value raises a `WriteShapeWarning`
(`:951-955`, emitted `:1106-1107`) and **the durable write still succeeds** — a claim is worth
keeping even when its shape is imperfect; a rejected write would lose it.

---

### 3.2 B — The per-namespace domain registry

**The registry row is a cache, not the truth.** The truth is the namespace's own rows —
the domains it actually carries. The registry row is a denormalized index of those
domains, kept current on the write path so that a write can be checked with one read
instead of a scan of the namespace. Everything below follows from that framing: a cache
may be stale; a stale cache must never cause a **wrong acceptance**; and it repairs
itself (§3.2.3).

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

The registry row is **excluded from recall results and from the snapshot body**. It is
metadata, not knowledge. The mechanism is explicit, not borrowed:

- **In the store**, the exclusion is a metadata predicate at the store's per-row filter
  seam — beside `is_retired_at`, `is_archived` and `is_mesh_conflicting`
  (`src/store/ladybug.rs:114-127`) — applied at **all four** filter sites: the base
  search (`:114-127`), the one-hop expansion (`:286-288`) and the two-hop govern
  expansion (`:343-345`), plus the episode-sibling pass (`:423-424`). A row whose
  metadata carries `domain_registry` is dropped at each of them, so it never consumes a
  *result* slot. It may consume one **over-fetch** slot (`fetch_limit = limit * 10`,
  `:52-53`), the same cost class as any other filtered row. (The sibling pass
  could not reach the registry row anyway — it carries no `EXTRACTED_FROM` edge — but the
  exclusion is applied there too, so the rule is uniform rather than incidental.)
- **In the snapshot**, the exclusion is a `retain` on the list the snapshot has already
  fetched — beside the archived and mesh-conflicting retains (`src/server.rs:1500`,
  `:1504`, `:1512`) — so it costs nothing beyond the scan.

The structural-type exclusion is **not** the mechanism: that one is by `memory_type` at
query-build time (`src/store/ladybug.rs:76-84`, `:86-89`) over a fixed five-member
const, with no per-row hook. The registry row is a `fact`, so it is not in that set and
needs a predicate of its own.

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

1. Look up the registry row (`store.get`, `src/traits.rs:223`).
2. If it is **absent** — the namespace's first domain write — list the namespace's rows
   (`store.list(namespace, None)`, `src/traits.rs:220`; it returns every row in the
   namespace), slug-normalize every `metadata.domain` they carry, union them with the
   incoming slug, and write the registry row. Every legacy domain is grandfathered at
   once, and **the write always succeeds**. The bootstrap deliberately errs toward
   registering **more**: a stale domain left behind by a retired or archived row costs
   nothing, whereas a missing legacy domain would reject a legitimate write.
3. If it is **present** and the incoming slug is not in it, the **reconcile** runs
   (§3.2.3): the slug is accepted if the namespace's own data carries it, and rejected
   only when neither the cache nor the data carries it.

#### 3.2.3 What an unregistered slug does

**Three ways through.** A write carrying slug S is accepted when:

1. **S is in the registry row's list** — the common case, one read.
2. **S is in the namespace's own data — the reconcile.** When the row's list does not
   hold S, the handler re-derives the list from the namespace's rows (the same scan the
   bootstrap uses: `store.list`, `src/traits.rs:220`, then slug-normalize every
   `metadata.domain` they carry) and merges the result into the cache. If S is among
   them, S is a domain this namespace has used, the write is accepted, and the cache is
   repaired. The scan runs only on a path that would otherwise have been a rejection, so
   its cost is paid on the rare path, never the hot one. This is what makes a lost
   cache entry a non-event rather than a false rejection (§3.2.5).
3. **`register_domain: true`** — the caller declares the domain genuinely new. The write
   proceeds; the cache is updated afterwards (§3.2.5), so a cache failure cannot undo
   the declaration.

**The order is fixed.** The acceptance decision — cache hit, reconcile hit, or
`register_domain` — is made **before** any registry write. The registry write is a
post-acceptance side effect: its failure (an exhausted retry, a store error) is reported
as a warning and never converts an acceptance into a rejection, nor a rejection into an
acceptance (§3.2.5).

**The reconcile normalizes exactly as the write path does.** "S is in the namespace's
own data" is decided by slug-normalizing every `metadata.domain` the namespace's rows
carry, using the **same** normalizer the write path applies (the parent's slug
normalizer, §5 step 1). Two different normalizers would let a never-used slug slip
through as a match — the exact wrong acceptance the registry exists to prevent — so the
two paths must call the same function, not two implementations that happen to agree
today.

Anything else is a **rejection**, with the registered list and a one-call remedy. The
write returns:

```
ERROR [DOMAIN]: domain 'database-access' is not registered in namespace 'NeuroStrata'.
Registered domains: ["auth", "database-access-v2"]. Use one of those, or pass
register_domain: true if 'database-access' is a genuinely new domain.
```

`neurostrata_add_memory` and `neurostrata_task_complete` gain one optional boolean
`register_domain` (default `false`), mirroring the `create_new_namespace` escape hatch
already in the tool schema (§2).

**Where each path takes the slug from.** The two write paths differ, and the flag
follows the slug, not the other way round:

- **`neurostrata_add_memory`** — the slug is the top-level `domain` argument. It is
  written into the metadata **raw**, as a JSON value rather than a string
  (`src/server.rs:1283-1285`), so the registry check reads it as a string and
  **rejects a non-string `domain`** rather than guessing at it. The check sits between
  the metadata assembly (`:1303-1307`) and the write (`:1357`).
  *[changed by neurostrata-r30c: the write normalizes the domain to a slug
  (`src/server.rs:1303-1307`), so what lands in the store is the slug and not the
  raw value; a non-string `domain` is now rejected by the check (`:1331-1345`),
  while a string with no usable slug is still stored verbatim]*
- **`neurostrata_task_complete`** — there is **no** top-level `domain` argument on this
  path. The completion's metadata is the caller's `memory.metadata` object
  (`src/task/mod.rs:1132-1157`), so the only slug that can exist there is
  `metadata.domain`. `register_domain: true` therefore applies to
  `metadata.domain`; if the completion carries no domain at all, the flag is a no-op
  and no registry row is touched. The same check runs on this path
  (`src/task/mod.rs:1214-1220` is its write), so a completion cannot smuggle in an
  unregistered slug through the back door.

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
  registry has not seen. (`src/synaptic_mesh/import.rs:1257-1279`)

#### 3.2.5 The registry update: serialized, retried, and never load-bearing

Registering a domain is a **read-merge-write**: read the registry row, add the slug to
the sorted list, write it back. That sequence is not atomic by itself, and the store
does not make it atomic for us: each `VectorStore` call opens its own connection
(`with_conn`, `src/store/ladybug.rs:923-946`), and the write lock the store does hold
(`write_with_deadline`, `:816-873`) covers the write alone, not the preceding read.

**Two processes really can interleave.** The engine admits one writer *at a time*, but
the loser does not fail — it retries (`WRITE_COLLISION_RETRIES`,
`src/store/ladybug.rs:2244`, `:844`) — and the CLI's `add` command writes to the same store
directly (`src/main.rs:2846`, `:2864`). The registry must not assume it is alone in the
store.

**There is no compare-and-swap to fall back on.** The engine's writes are auto-commit
`conn.query` calls; there is no transaction wrapper and no `BEGIN`/`COMMIT` anywhere in
the store (`src/store/ladybug.rs:816-909`, `:1445-1579`). A cross-process update cannot
be made atomic. The best available is a **bounded compare-and-retry**:

1. **In-process serialization.** The registry update runs under a **process-wide
   registry mutex** — registrations are rare and off the hot path — so two registrations
   inside one process cannot interleave.
2. **Compare-and-retry.** After the write, the row is re-read and checked against the
   whole set that was there before the write, not only against the new slug: every slug
   in the pre-write set must still be present. If one is missing, another writer
   changed the row under us; the update is re-merged and written again. The bound is
   **at most three attempts in total** — enough for a rare event, and no more.

**What that does not cover, stated plainly.** The retry cannot see a write that landed
*before* our own write: we read an old list, another process added a slug, and our
write overwrites it. The row we wrote still contains our whole pre-write set, so the
check passes and **that other process's slug is dropped from the cache**. Only from the
cache — the namespace's own rows, which are the truth, are never touched (§3.2). And
because the cache is not the truth, the loss is neither a wrong acceptance nor a
corrupted row: it is a **miss**, repaired by the reconcile of §3.2.3 the next time a
write needs that slug.

**So a cache update never fails the write.** If the bounded retries are exhausted, the
write still proceeds — the acceptance decision was already made — and the failure is
reported in the response's warnings rather than swallowed. What the caller is told is
what happened: with `register_domain: true` the domain is accepted as the caller
declared it, and if the cache update did not stick, the response says so in a warning.
The cache never claims a registration it does not hold, and the caller is never told
the cache holds one it does not.

No `VectorStore` trait method is added. `store.get`, `store.list` and `store.upsert` are
enough (`src/traits.rs:220`, `:223`, `:180-186`); the mutex, the retry and the
verification live in the handler layer, beside the write path.

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

Each entry is `{id, memory_type, domain, valid_from, access_count, snippet}`. The
**snippet's total length is ≤ 120 characters, prefix included** — when the row carries a
valid `metadata.attempt` the `"[FAILED ATTEMPT] "` prefix is part of those 120
characters, not added on top of them (§3.1.3).

#### 3.3.3 The output and its cost

Two sections are rendered beside the top-5 body, both with an **explicit zero** when
there is nothing to show:

- **`Registered domains (N): <slug1>, <slug2>`** — the namespace's registry, as the
  agent sees it (the registry row of §3.2.1). The snapshot fetches the namespace's rows
  with `store.list` (`src/server.rs:1484-1487`), not `store.search`, so the store's
  per-row filter seam never touches this list — the snapshot's own exclusion is the
  `retain` of §3.2.1. The registry row is therefore either read directly by its
  deterministic id (`store.get`, `src/traits.rs:223`) or captured from the listed rows
  **before** that retain drops it. `N` is the number of registered domains; when there
  is no registry row yet, the line reads `Registered domains (0): none` — an explicit
  zero, never a silent omission. This is one of the two places the dynamic
  per-namespace enum is surfaced (§3.2.4); the other is the rejection message of
  §3.2.3. The count is the **cache's**, and the cache can be stale (§3.2) — it is the
  set of domains the agent should expect to be accepted, and the reconcile keeps it
  close to the truth. It is never a promise that the list is exhaustive.
- **`Domain strap [mode: <explicit|recent_domain>]`** — the strap itself, at most 3
  entries, or `(0): []` when the selection is empty. The mode is printed so a caller
  can never mistake a `recent_domain` selection for a domain match.

Worst case 3 × 120 characters ≈ 90–100 tokens at session start.

#### 3.3.4 Invariants

- **`access_count` is never mutated.** The strap reads the list it already has; the only
  two increments remain `src/server.rs:2261` and `:2925`. A strap hit cannot promote
  itself into the top-5.
- **Additive.** The top-5 body is byte-identical to today's for any namespace; the strap
  is a new section beside it.
- **Explicit mode is honest about its mode.** The section header names the mode, so a
  caller can never mistake a recent-domain selection for a domain match.

#### 3.3.5 What the topic seat is, then

Mid-session topic transitions are served by the parent's `append_log` recall (≤3 entries,
≤120 chars total each, prefix included — parent §4.1.2). Session/task start is served by
this strap. Both are
bounded, additive and `access_count`-neutral, so an agent that triggers both in one turn
sees at most six compact entries — **at most** 720 characters of pointer text (≈180
tokens) — and the two seats answer different moments, so in practice they rarely fire
together.

---

### 3.4 E — The parent's open questions, decided

#### 3.4.1 Is the `append_log` seat sufficient?

**Decided: no — two bounded seats, and the sufficiency of each is measured.** The
`append_log` recall covers mid-session topic transitions (`AGENTS.md` §3, Continuous
Backup Protocol); the snapshot strap covers session/task start (`AGENTS.md` §3, Mandatory
Pre-Flight Hook). Neither alone covers both moments.

**Metric: `recall_hit_rate`** — the fraction of recall responses whose `id`s the agent
later promotes with `neurostrata_get_memory`.
**Threshold:** below 10% over the daemon's current lifetime with ≥50 tagged turns since
it started, the recall budget is tightened (the parent's §11.1 lever) before any new
seat is considered.
**Where:** two counters in the same module as the domain counters (§3.4.2) —
`recall_responses`, incremented where the recall response is built (the parent's
`append_log` path, `src/buffer.rs:133-298`), and `recall_promotions`, incremented where
`get_memory` increments `access_count` (`src/server.rs:2925`). The join is a **bounded
in-memory set of recently-recalled ids** in that module: the recall path records the
ids it returned, the promotion path tests membership and removes the id. The set is
bounded (the most recent recall responses) and evicted on promotion or on age, so it
never grows without limit and never outlives the daemon.
**Scope:** the join is **per namespace, not per session** — the MCP tool surface carries
no session id, so a session-scoped join could not be built from what the tools receive.
A response whose ids age out of the bounded set before promotion counts as unpromoted,
so the measured rate is a lower bound.
**Window:** the counters live in memory and reset when the daemon restarts, exactly as
the bus's own counters do ("what the bus has done since the daemon started",
`src/main.rs:112-119`). The operator reads them from a running daemon; a threshold read
after a restart starts from zero.
**Who reads it:** the operator, via the metrics surface in §3.4.2 — the body reports
`"recall": {recall_responses, recall_promotions}`, and the rate is their quotient.

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

**A call with no `recalled_from` increments none of the four.** The parent spec
(`docs/superpowers/specs/2026-10-21-proactive-recall-and-access-topology-design.md`
§4.1.2) makes the mode present *exactly* when a search ran, and absent when no search
ran (an untagged turn, or every tag normalized away) or when the search failed. Such a
call carried no filter and narrowed nothing, so it is not a domain-filter call at all —
and a failed search is not a fallback, because no unfiltered search was run in its place.
Counting it as `domain_fallback_vector` would overstate the filter's fallback rate with
the errors it produced.

Exposed as `"domain_filter": {…}` on the metrics surface, with **explicit zeros** on a
fresh daemon, and surfaced per call as `recalled_from` / `registered_domain_count` on
the recall response.

**The surface is a composite, and the bus is untouched.** `handle_bus_metrics` today
serializes `bus.metrics()` — a bare `BusMetrics` of eight integers, with no extension
point (`src/handlers/bus_metrics.rs:20-23`; the existing test asserts that those eight
fields sit at the top level of the body, by containment rather than exact shape). The
counters therefore live in their own module, and the handler reads them **directly**:
they are process-global atomics **in the daemon process** (see the next paragraph),
beside the search path, so the handler's signature does not
change, no `AppState` field is added, and the route needs no plumbing
(`src/daemon.rs:539`, `:1106-1110`). The handler serializes the bus metrics to a
`serde_json::Value`, inserts `"domain_filter"` as an **extra top-level key**, and
serializes the result. The bus's eight integers stay exactly where they are, so every
existing reader of that body keeps working, and the bus itself never learns what a
domain is.

`neurostrata-mcp bus-metrics` is an HTTP client of the daemon (`src/main.rs:118-119` — the
command declaration; the POST is `src/main.rs:1688-1692`), so it inherits this body without
knowing anything about it.

**The counters belong to the daemon process.** The topology has two processes in stdio
mode: the MCP proxy the client talks to, and the detached daemon that owns the store
(`src/main.rs:1775-1779`, `:1817-1825`). The proxy forwards every tool call over HTTP
to the daemon's `/mcp` endpoint (`src/server.rs:909`), so the search path — and
therefore every counter increment — executes **in the daemon**, which is the same
process that serves `/bus/metrics` (`src/daemon.rs:534`, `:539`). The counters are
process-global atomics in that process, read directly by the handler in it. The proxy
holds no counter state and reads none; the CLI reads the daemon's body over HTTP, and
its own doc comment says as much ("Reads the running daemon's bus", `src/main.rs:112-119`).

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
| `register_domain` | `neurostrata_add_memory`, `neurostrata_task_complete` | optional boolean, default `false`. When `true`, the slug is added to the registry cache and the write proceeds. The slug comes from `domain` (add_memory) or `metadata.domain` (task_complete); a non-string `domain` is rejected. |
| `domain` (input) | `neurostrata_get_snapshot` | optional string or array of slugs; normalized before comparison. |
| Domain strap entry | the snapshot's `Domain strap` section | `{id, memory_type, domain, valid_from, access_count, snippet}`; ≤3 entries; `snippet` ≤120 chars **total, prefix included**; ≤2 entries per domain in `recent_domain` mode. |
| `recalled[]` snippet (parent) | the `append_log` response | prefixed with `"[FAILED ATTEMPT] "` when the row carries a valid `metadata.attempt` — prefixed **before** truncation, and the prefix counts toward the 120 characters. |
| `"domain_filter"` | the metrics surface | four integer counters, explicit zeros — an extra top-level key beside the bus's own eight integers (§3.4.2) |

---

## 5. Implementation plan

Ordered. Each step is independently testable. Steps marked **[P]** are the parent spec's
(`neurostrata-426k`); steps marked **[N]** are this spec's.

| # | Step | Where |
|---|---|---|
| 0 | **[P]** Prerequisite: the rule-honesty hardening + legacy backfill lands | task `neurostrata-bzdn` |
| 1 | **[P]** Domain slug normalizer, applied on write and on comparison | new helper `src/domain.rs:3-33`; `src/server.rs:1303-1307` (on write); `src/store/ladybug.rs:128-136` (on comparison) |
| 2 | **[N]** Registry row helpers: the deterministic v5 id, the lookup, the bootstrap seed (list + normalize + union), the reconcile scan, and the registry update (`store.upsert`) under the process-wide registry mutex, with a bounded compare-and-retry that never fails the write | new module beside the store; `src/traits.rs:220`, `:223`, `:180-186` |
| 3 | **[P+N]** Vocabulary, **two** bumps in a fixed order: **4 → 5** (P) carries `fact.fields.topology` and lands with the parent's implementation; **5 → 6** (N) carries `fact.fields.attempt` and `fact.fields.domain_registry` and lands with this spec's; the assert at `src/traits.rs:312`; `tool_summaries.add` and `tool_summaries.memory_type` | `src/schemas/memory-vocabulary.v3.json:2`, `:66-83`, `:172`, `:175`; `src/traits.rs:312` |
| 4 | **[N]** The write-path registry check: bootstrap when absent, reconcile when the slug is missing from the cache, reject when it is in neither the cache nor the namespace's data, `register_domain: true` to override — on both write paths, each taking its slug from its own source (`domain` on add_memory, `metadata.domain` on task_complete) | `src/server.rs:1283-1285`, `:1331-1345`, `:1357`; `src/task/mod.rs:1132-1157`, `:1178-1194`, `:1204-1211` |
| 5 | **[P]** Optional domain filter on search (store + MCP tool) with the soft fallback and the explicit mode | `src/traits.rs:255-271`; `src/store/ladybug.rs:1581`, `:1602-1630`; `src/server.rs:2045-2061` |
| 6 | **[N]** Registry row excluded from recall results and from the snapshot body — a metadata predicate at the store's per-row filter seam, and a `retain` on the snapshot's fetched list | `src/store/ladybug.rs:114-127`; `src/server.rs:1500`, `:1504`, `:1512` |
| 7 | **[P]** `handle_append_log(arguments, emb, store)`; dispatch widening; `namespace` in the input schema; the recall rides the response | `src/buffer.rs:133`, `:304-321`; `src/server.rs:372-384`, `:667-669`, `:2144` |
| 8 | **[N]** The `metadata.attempt` shape check → soft warning; the `[FAILED ATTEMPT]` rendering in `format_memory` and in every truncated snippet; the duplicate-check candidate set partitioned by attempt-presence | `src/store/ladybug.rs:979`; `src/server.rs:2324`; the parent's `recalled[]` response, built beside `src/buffer.rs:277-287`; `src/server.rs:1163-1175` |
| 9 | **[N]** The snapshot domain strap: the `domain` input, the two modes, the selection, the explicit zero, the `Registered domains (N)` header | `src/server.rs:153-162`, `:1520-1707` |
| 10 | **[N]** The `domain_filter` counters and their surface — process-global atomics in a module beside the search path, incremented in the **daemon** process (which serves both the MCP dispatch and `/bus/metrics`) and read directly by the metrics handler: no signature change, no `AppState` field, no route plumbing | new module beside the search path; `src/handlers/bus_metrics.rs:20-23`; `src/daemon.rs:534`, `:539`; `src/server.rs:909`; `src/main.rs:1775-1779` |
| 11 | **[N]** The mesh import step unions an imported record's domain into the local registry | `src/synaptic_mesh/import.rs:1257-1279` (`apply_effective`) |
| 12 | Tests (§6) | `src/server.rs`, `src/buffer.rs`, `src/store/ladybug.rs`, `src/schemas/` |
| 13 | Docs: the honest-recall note, the registry note; `CHANGELOG.md` | repo docs |

---

## 6. Acceptance criteria

1. A `fact` with a well-formed `metadata.attempt` round-trips, exports, and survives a
   SynapticMesh export/import round-trip (`memory_type` stays `fact`).
2. A `fact` with a malformed `metadata.attempt` is **written** and emits a
   `WriteShapeWarning`.
3. `vocabulary_version` is 6, `src/traits.rs:312` asserts 6, and the vocabulary declares
   `fact.fields.topology`, `fact.fields.attempt` and `fact.fields.domain_registry`
   (`topology` shipped with the parent's 4 → 5 bump; this spec's two fields ship with 6).
4. A search for *"how do we do X"* in a namespace holding both a positive `fact` about X
   and a negative exemplar about X returns **both**, and the negative one renders the
   `Attempt [FAILED in <context>]: …` line.
5. Every truncated snippet of a negative exemplar begins with `"[FAILED ATTEMPT] "`, and
   is ≤ 120 characters **including** the prefix.
6. A negative exemplar about X never blocks a positive `fact` about X: the duplicate
   check's candidate set is partitioned by attempt-presence, so the two are never
   compared against each other.
7. In a namespace with no registry row and an existing memory carrying
   `domain: "Database Access"`, writing a memory with `domain: "Auth"` succeeds and
   creates the registry row with `["auth", "database-access"]`.
8. A subsequent write in that namespace with `domain: "storage"` is **rejected** with the
   registered list, and no row is inserted; repeating it with `register_domain: true`
   succeeds and the registry becomes `["auth", "database-access", "storage"]`. A slug
   that is missing from the cache but **present in the namespace's own data** is
   accepted instead of rejected — the reconcile (§3.2.3) — and the cache is repaired to
   include it.
9. The registry row never appears in search results or in the snapshot's top-5 body, and
   `get_snapshot` renders `Registered domains (3): auth, database-access, storage`.
10. The registry row is not exported to the mesh (`eligibility: private`), and an
    imported shared record's domain is unioned into the local registry
    (`src/synaptic_mesh/import.rs:1257-1279`).
11. **The acceptance decision precedes any registry write.** A cache update that fails —
    an exhausted retry, a store error — never converts an accepted write into a
    rejection, nor a rejection into an acceptance; it is reported as a warning.
12. **The reconcile normalizes as the write path does.** A slug that no row of the
    namespace carries is rejected, even if the reconcile's own normalization could have
    matched it; a slug the namespace has used under a different spelling is accepted
    through the reconcile.
13. **A non-string `domain` is rejected**, not guessed at, on both write paths.
14. With 5 memories at `access_count: 10` and one new `fact` at `access_count: 0` with a
    domain, `get_snapshot` shows all five in the top-5 body **and** the new fact in the
    domain strap — with and without an explicit `domain` argument.
15. A domain-tagged memory already in the top-5 is not duplicated in the strap.
16. `access_count` is unchanged, row for row, across a `get_snapshot` call.
17. The strap never exceeds 3 entries, and never 2 from one domain in `recent_domain`
    mode; an empty strap renders an explicit zero and names its mode.
18. The metrics surface reports `"domain_filter"` with four explicit zeros on a fresh
    daemon, and the counters move on domain-filtered calls **served by the daemon**; the
    stdio proxy process holds no counter state. The same body reports `"recall"` with
    two explicit zeros — `recall_responses` and `recall_promotions` (§3.4.1).
19. `neurostrata_search_memory` with no `domain` argument behaves exactly as today.
20. This spec contains no deferred item: §11 is the closed list, and every entry in it is
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
| Two agents register a domain concurrently | In-process, the registry mutex serializes them. Cross-process, the store has no compare-and-swap, so a registration can still lose another process's slug **from the cache** — the namespace's data is never touched, and the next write that needs that slug triggers a reconcile that restores it (§3.2.3) | The loss is a cache miss — never a wrong acceptance, never a corrupted row |
| The reconcile scan is expensive | It runs only on the path that would otherwise have been a rejection, and only over the namespace's own rows | A rejection is rare; the hot path is one read |
| The bootstrap registers a stale domain from a retired row | Stale entries cost nothing; a missing legacy domain would | The bootstrap errs toward registering more, never less |
| The strap's mode is wrong for the caller's intent | The mode is printed in the section header | The caller can see which selection produced the rows. |
| The strap would show only one domain | `recent_domain` mode caps at 2 per domain | A narrow namespace cannot consume every slot. |
| The domain filter would empty the candidate set | The filter is skipped; `recalled_from: "vector"` | Recall is never blind, and the substitution is reported. |
| The mesh imports a record with an unseen domain | The slug is unioned into the local registry | No shared record is refused for a local vocabulary gap. |
| The registry row matches a vector query | It is dropped at the store's per-row filter seam, so it never occupies a result slot; at worst it uses one over-fetch slot | No registry content is ever returned to an agent. |
| The mesh's unknown-type policy is reconciled the other way (`neurostrata-wl4i`) | The negative exemplar is a `fact`, which is shareable under either reading | The design does not depend on the unresolved contradiction. |
| The vocabulary bump breaks a consumer | One assert at `src/traits.rs:312` | Localized, compile-time, greppable. |

---

## 8. Migration and blast radius

- **No storage migration.** The registry is seeded lazily, in-band, on the first domain
  write; comparison-time normalization keeps the read path unchanged.
- **Vocabulary 4 → 5 → 6, in that order** — one assert plus any external consumer of the
  schema. The parent's `topology` ships with the parent's 4 → 5 bump (the parent's
  implementation lands first); this spec's `attempt` and `domain_registry` ship with
  5 → 6.
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
| `recall_hit_rate` | recalled ids later promoted via `get_memory` ÷ recall responses | `"recall": {recall_responses, recall_promotions}` on the metrics surface (§3.4.2); incremented at the recall path (`src/buffer.rs:133-298`) and the promotion path (`src/server.rs:2925`) | < 10% over the daemon's current lifetime with ≥50 tagged turns → tighten the budget | operator; the next retrieval spec |
| `domain_queries_total` | recall/search calls carrying a domain filter | `"domain_filter"` on the metrics surface — an extra top-level key beside the bus's eight integers (§3.4.2) | — | operator; agent |
| `domain_narrowed_hits` | filter matched ≥1 row and narrowed the set | as above | — | operator; agent |
| `domain_fallback_vector` | filter matched 0 rows; fell back to vector | as above | a persistently high rate means the domains are wrong or sparse | operator; the next registry spec |
| `single_domain_bypasses` | calls in a namespace whose registry holds ≤1 domain | as above | — | operator |

Every counter above is a process-global atomic in the **daemon** process, and the
metrics surface is served by that same process (§3.4.2). The counters reset when the
daemon restarts, exactly as the bus's own do.

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
| The mesh's import doc contradicts its code (an unknown `memory_type`: ignored, or a hard error) | task `neurostrata-wl4i` |

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
   writes to stderr (`src/store/ladybug.rs:1106-1107`), which an MCP agent never reads.
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
   and are **added to that surface as an extra top-level key**, leaving the bus's eight
   integers where they are. Adopted Argon's four counters; corrected their home.
5. **The registry row's vector.** Argon proposed a zero vector. A real embedding of the
   row's content is used instead, so no zero vector ever enters the store.

**Facts the panel refuted or corrected.**

| Fact | Correction |
|---|---|
| The brief cited the closed relation loop at `src/store/ladybug.rs:445-452` (at `c5acd4f`) | It is the four-key relation array at `:1000-1008`; `:986-998` is the `metadata` root check. (Argon) |
| The brief cited `increment_access_count` at `src/store/ladybug.rs:1993` (at `c5acd4f`) | It is at `:2160`. (Argon) |
| `src/synaptic_mesh/record.rs:26-28` (at `c5acd4f`) names `structural` as a mesh memory type | The binding set is three (`src/synaptic_mesh/import.rs:61`); `structural` is rejected at import. Doc corrected in commit `c5acd4f`. (K3) |
| A mesh row may carry a non-UUID id | Rows whose id fails `Uuid::parse_str` are skipped (`src/synaptic_mesh/worker.rs:180-183`) — the registry row therefore uses a deterministic **UUID v5**. (Argon) |

**The checker's findings** (session `ses_ea3515449fferHzlPrObOKwd9E`). The checker
confirmed 21 of 22 spot-checked substrate anchors, confirmed all four corrections above,
and refuted two claims this spec had made:

| Claim as first written | The checker's finding | The fix |
|---|---|---|
| "`metadata.attempt` joins the canonical exact-match key" | No canonical exact-match key exists in `src/`. `src/server.rs:1163-1175` is the **semantic** duplicate check — candidates scoring `>= 0.85`, then `checker.check_duplicates(payload, &similar)`. | §3.1.3 now **partitions the duplicate check's candidate set by attempt-presence** — a mechanism that exists — instead of extending a key that does not. |
| "the registry row is excluded by the same predicate the store already applies to structural rows" | `STRUCTURAL_MEMORY_TYPES` is a fixed `const [&str; 5]` consulted only at query-build time; it has no per-row hook, and its members are `directory`, `file`, `markdown`, `freshness_flag`, `guard_event` — **not** `symbol`. | §3.2.1 now names the real seam: a metadata predicate in the store's per-row filter (`src/store/ladybug.rs:114-127`) and a `retain` on the snapshot's fetched list (`src/server.rs:1492`, `:1496`, `:1498`). |
| "the mesh rejects an unknown `memory_type` at import" | The **code** does reject it (`src/synaptic_mesh/import.rs:649-655`); the **comment** directly above it (`src/synaptic_mesh/import.rs:57-60`) says unknown types are "ignored and reported, never applied, never crash". The mesh's own doc contradicts its own code. | Recorded in §2 as a known contradiction; filed as task `neurostrata-wl4i`. The design is safe under either reading. |

The checker's run ended at its step limit with the feasibility pass and the numeric
consistency pass unfinished. The orchestrator completed both against source — the
registry-row exclusion, the registry's read-merge-write and the atomicity hole the
store's own locking does **not** cover (§3.2.5), the bootstrap scan through
`store.list`, the `[FAILED ATTEMPT]` prefix applied before truncation, the polarity
partition, and the arithmetic of every bound in §3 and §6 — and a narrow re-check was
dispatched to confirm the amended spec.

**The second checker's findings** (session `ses_ea349f685ffetn6QkBPshvk6s4`, verdict:
"not yet approvable, but close"). It confirmed the two rewritten mechanisms at their
cited lines, and surfaced eight more:

| The finding | The fix |
|---|---|
| The store has **three** row-class filter sites, not two — the episode-sibling pass at `src/store/ladybug.rs:412-418` also filters rows | §3.2.1 now names all four sites (base search, one-hop, two-hop, sibling). The registry row could not reach the sibling pass anyway — it carries no `EXTRACTED_FROM` edge — but the exclusion is applied there too, so the rule is uniform rather than incidental. |
| `Registered domains (N)` was referenced by §3.2.4, §5 step 9 and acceptance criterion 9, but **§3.3 never defined it** | §3.3.3 now defines both rendered sections: `Registered domains (N): …` (with an explicit `(0): none`) and `Domain strap [mode: …]`. |
| "under 720 characters" contradicted 6 × 120 = 720 exactly | §3.3.5 now says **at most** 720 characters (≈180 tokens), and anchors the parent's bound to parent §4.1.2. |
| The ≤120-character bound was stated two ways — "`snippet` ≤ 120 characters" and "≤ 120 characters **including** the prefix" | §3.3.2 now says it once: the snippet's **total** length is ≤ 120 characters, prefix included. The prefix is part of the budget, not added on top of it. **Superseded by the third round** — the bound now reads the same way in §3.3.2, §3.3.5 and §4. |
| The `domain_filter` counters do not fit a surface that is a bare eight-integer `BusMetrics` with no extension point (`src/handlers/bus_metrics.rs:20-23`) — while the spec also said they do not live inside the bus | §3.4.2 now specifies the composite: the handler serializes the bus metrics to a `serde_json::Value`, inserts `"domain_filter"` as an **extra top-level key**, and serializes that. The bus's eight integers stay exactly where they are, so no existing reader breaks, and the bus never learns what a domain is. |
| **"The daemon is the only process that can write to the store" was false.** The engine admits one writer at a time, but the loser *retries* (`WRITE_COLLISION_RETRIES`, `src/store/ladybug.rs:2244`, `:844`), and the CLI's `add` command writes to the same store directly (`src/main.rs:2846`, `:2864`) | §3.2.5 was rewritten: an in-process **registry mutex**, plus a **cross-process compare-and-retry** that checks the *whole* pre-write set (not just the new slug) and fails loudly if the bounded retries are exhausted. **Superseded by the third round** — the retries no longer fail the write; the cache framing and the reconcile replaced that rule (§3.2.5, §3.2.3). |
| `neurostrata_task_complete` has **no top-level `domain` argument** — the completion's metadata is the caller's `memory.metadata` object (`src/task/mod.rs:1132-1157`), so `register_domain` there had no stated source of the slug | §3.2.3 now gives each write path its slug source: `domain` on add_memory, `metadata.domain` on task_complete; the flag follows the slug, and is a no-op when the completion carries no domain. |
| The `domain` argument is inserted into the metadata **raw**, as a JSON value rather than a string (`src/server.rs:1283-1285`), so the check must not compare a non-string against the registry *[changed: the write now normalizes the domain to a slug (`src/server.rs:1303-1307`), so the raw insert is no longer what is stored]* | §3.2.3 now says the check reads it as a string and **rejects** a non-string `domain` rather than guessing. |
| A row with a **malformed** `metadata.attempt` (accepted with a soft warning, `src/store/ladybug.rs:979`) would be rendered as a negative but partitioned as a positive | §3.1.3 now requires the partition and the `[FAILED ATTEMPT]` prefix to use the **same** predicate: a malformed attempt is neither rendered nor partitioned as a negative. |

The anchors that round never reached were read and verified by the orchestrator against
HEAD: the vocabulary version and its single assert (`src/schemas/memory-vocabulary.v3.json:2`,
`src/traits.rs:312`), the `rule.fields` precedent
(`src/schemas/memory-vocabulary.v3.json:39-64`), `fact` having no `fields`
object (`src/schemas/memory-vocabulary.v3.json:66-83`) *[changed by neurostrata-r30c: the vocabulary is v6 and declares fact.fields.attempt and fact.fields.domain_registry; `fact` also carries `fields.topology`]*, the bump policy (`src/schemas/memory-vocabulary.v3.json:169`), and the two `tool_summaries` keys (`src/schemas/memory-vocabulary.v3.json:172`,
`src/schemas/memory-vocabulary.v3.json:175`).

**The third checker's findings** (session `ses_ea34460a4ffeIo2Z9ucZgtTT27`, verdict:
"not yet approvable"). It confirmed the mechanisms it could reach — the registry's
trait-API feasibility, the snapshot's `store.list` path and the separation of the
snapshot's retain from the store's search filter, the composite metrics body's
compatibility with the existing test (which asserts containment, not exact shape), and
the numeric bounds — and found two substantive defects, both in the amended material:

| The finding | The fix |
|---|---|
| **The compare-and-retry has a real lost-update hole.** When the other writer's write lands *before* ours, our write clobbers its slug; the row we wrote still contains our whole pre-write set, so the check passes and that other process's slug is silently lost. §7's claim that "no slug can vanish from the registry without the caller being told" was **false**. | §3.2 is reframed: **the registry row is a cache of the domains in use; the namespace's own rows are the truth.** A lost cache entry is a miss, not a wrong acceptance and not a corrupted row, and it is repaired by a new **reconcile** path (§3.2.3): when the cache does not hold the slug, the handler re-derives the list from the namespace's rows (the bootstrap's own scan) and, if the slug is there, accepts the write and repairs the cache. The scan runs only on the path that would otherwise have been a rejection. §7's row and §5 step 2 were rewritten to match, and §6's criterion 8 gained the reconcile clause. |
| There is **no compare-and-swap**: the engine's writes are auto-commit `conn.query` calls, with no transaction wrapper and no `BEGIN`/`COMMIT` anywhere in the store (`src/store/ladybug.rs:816-909`, `:1445-1579`), so a cross-process update cannot be made atomic | §3.2.5 now says so outright, and states that a cache update **never fails the write**: if the bounded retries are exhausted the write still proceeds — the acceptance decision was already made — and the failure is reported in the response's warnings rather than swallowed. |
| The metrics plumbing was **unstated**: `AppState` (`src/daemon.rs:51-70`) has no counters field, so "the handler takes them as a second argument" would have required the route, the HTTP handler and the handler's own unit test to change | The counters are now **process-global atomics** in their own module, read directly by the handler: no signature change, no `AppState` field, no route plumbing, and the existing unit test keeps compiling. `neurostrata-mcp bus-metrics` is an HTTP client of the daemon (`src/main.rs:112-119`), so it inherits the body without knowing anything about it. **Refined by the fourth round** — the owning process is pinned to the daemon (see below). |
| The ≤120-character bound was still stated two ways (§4's row and §3.3.5's parent anchor) | Both now say **total, prefix included**. |
| Cross-connection read visibility after another process's commit could not be verified at the engine level | It is **no longer load-bearing**: the cache framing means a stale cache is repaired by the reconcile, and a failed cache update never fails the write. |

The round also verified what it could and left three items unread: the `BusMetrics`
derive line above `src/events/subscriber.rs:178`, `src/server.rs:2925` (the
`recall_hit_rate` promotion path) and `src/server.rs:2261` — all read and confirmed by
the orchestrator, along with the store's four filter sites
(`src/store/ladybug.rs:114-127`, `:286-288`, `:343-345`, `:423-424`).

**The fourth checker's findings** (session `ses_ea33e9db5ffeaMQuQQ8FCjYh4s`, verdict:
"not yet approvable"). It confirmed that the cache framing restores the registry's
teeth — a never-used slug still needs `register_domain: true`, because the reconcile
accepts only slugs the namespace's own rows actually carry — and that the composite
metrics body is mechanically valid and test-compatible. One finding was load-bearing:

| The finding | The fix |
|---|---|
| **"Process-global" was ambiguous across a two-process topology.** In stdio mode the client talks to an MCP **proxy** process while the daemon is a detached process that owns the store (`src/main.rs:1775-1779`, `:1817-1825`). The proxy forwards every tool call over HTTP to the daemon's `/mcp` (`src/server.rs:909`), so the search path runs in the daemon — but a reader could not tell from the spec which process owned the counters, and the wrong answer reports **zeros silently**. | §3.4.2 and §5 step 10 now **pin the counters to the daemon**: it serves both the MCP dispatch (`src/daemon.rs:534`) and `/bus/metrics` (`:539`), so the atomics are incremented and read in the same process. The proxy holds no counter state; the CLI reads the daemon's body over HTTP, and its own doc comment says so ("Reads the running daemon's bus", `src/main.rs:112-119`). |
| §12 contradicted itself: the second-checker row said the retries "fail loudly", the third-checker row said they never fail the write | The second-checker row is marked **superseded**, with a pointer to the rule that replaced it. |
| The **acceptance ordering** was only implied — §3.2.3's rejection list and §3.2.5's "the write still proceeds" were reconcilable but never stated as one order | §3.2.3 now fixes the order: the acceptance decision (cache hit ∥ reconcile hit ∥ `register_domain`) is made **before** any registry write, and the registry write's failure never flips an acceptance into a rejection or the reverse. |
| The **reconcile's normalizer** was unstated — the one place a wrong acceptance could enter, since a never-used slug would match if the reconcile normalized differently from the write path | §3.2.3 now requires the reconcile to call the **same** normalizer function as the write path (§5 step 1), not two implementations that agree today. |
| "Two direct-call test sites" was wrong — there is one test module (`src/handlers/bus_metrics.rs:25-78`) | Corrected. |

The anchors that round could not reach were read by the orchestrator: `store.list` is a
bare `MATCH` with no row-class filters, so the store's search-time seam never covers
the snapshot (`src/store/ladybug.rs:1849-1859`); the engine has no transaction wrapper
and no compare-and-swap — its own error text is "Cannot start a new write transaction in
the system", one write at a time (`:2251`, `:2400-2405`), and the store issues no `BEGIN` or
`COMMIT`; the CLI's `bus-metrics` is an HTTP client of the daemon (`src/main.rs:112-119`);
and the CLI's `add` command does write to the same store directly
(`src/main.rs:2846-2864`).

**The fifth checker's findings** (session `ses_ea33ba38affe4f7c5AGfN1rZ2B`, a consistency
sweep with no source re-verification). It mapped every acceptance criterion to a
mechanism and found three blockers — two sentences that would have been implemented
literally, and one coverage gap — plus a set of editorial nits:

| The finding | The fix |
|---|---|
| **Blocker.** §3.2.2's bootstrap step 3 still said a cache miss ⇒ "the write is **rejected**", contradicting the reconcile's acceptance in §3.2.3 — the last place the cache was still treated as authoritative | §3.2.2 step 3 now routes a cache miss to the reconcile: the slug is accepted if the namespace's own data carries it, and rejected only when neither the cache nor the data carries it. |
| **Blocker.** §3.2.5 still claimed "there is no case where the caller is told a domain was registered when the cache does not hold it" — false under the cache framing, because `register_domain: true` plus an exhausted retry accepts the write while the cache misses it | §3.2.5 now says what is true: the cache never claims a registration it does not hold, the caller is never told the cache holds one it does not, and a cache update that did not stick is reported as a warning. |
| **Blocker (coverage).** The two behaviours this revision introduced — acceptance before any registry write, and normalizer parity — had **no acceptance criterion**, so the spec's own headline claims were untested; the daemon pin and the non-string `domain` rejection were likewise untested | §6 grew three criteria (11, 12, 13) covering the ordering, the normalizer parity and the non-string rejection, and criterion 18 now asserts that the counters move **in the daemon** while the proxy holds none. The criteria after the insertion were renumbered (11–17 → 14–20); the only cross-reference to a criterion number (criterion 9, in this log) is unaffected. |
| `recall_hit_rate` had **no surface** — §3.4.1 read it "via the metrics surface in §3.4.2", but §3.4.2 hosts only the four `domain_filter` counters, and its 30-day window was unmeasurable from in-memory counters | §3.4.1 now names the two counters (`recall_responses`, `recall_promotions`), the bounded in-memory set of recently-recalled ids that joins them, the surface (`"recall": {…}` beside `"domain_filter"`), and the window (**the daemon's current lifetime** — the counters reset on restart, exactly as the bus's own do). The join is **per namespace, not per session**: the MCP tool surface carries no session id, so a session-scoped join could not be built from what the tools receive. §10's row was rewritten to match. |
| The retry bound was phrased as justification ("three attempts is enough for a rare event") rather than as a bound | §3.2.5 now states **at most three attempts in total**. |
| §3.2.1 said "applied at **all three** filter sites" and then listed four | Now "all four". |
| §3.2.3's path 3 read as insert-then-write, against the fixed decide-then-write order | Reworded: the write proceeds on the caller's declaration, and the cache is updated afterwards. |
| "Process-global" appeared unqualified before the daemon pin | The first mention now says "process-global atomics **in the daemon process**". |
| `Registered domains (N)` is rendered from the cache, and nothing said it could be stale | §3.3.3 now says the count is the cache's, that the reconcile keeps it close to the truth, and that it is never a promise of exhaustiveness. |
| §12's second-checker row said "§3.3.2 now says it **once**" while the bound also stood in §3.3.5 and §4 | That row is marked **superseded**, and the third round's row is the one that matches the current text. |
| §12's third-checker row did not record the §6 criterion-8 edit | It now does. |

**Brief:** `docs/designs/2026-10-21-panel-brief-recall-closure-and-negative-exemplars.md`
· **Parent brief:** `docs/designs/2026-10-20-gap-proactive-recall-and-access-topology.md`
· **Parent spec:** `docs/superpowers/specs/2026-10-21-proactive-recall-and-access-topology-design.md`
· **Ruling:** memory `bca58f91`
