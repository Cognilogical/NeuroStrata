# NeuroStrata gap brief: proactive recall, and what "remembered" can even mean

**Status:** proposal — capture only. No implementation, no approval, no behavioral
change. Queued for Frontier-model evaluation.

**Observed:** 2026-10-19 (owner-reported episode); brief written 2026-10-20
**Reporter:** owner, from a downstream consumer session
**Scope:** one observed agent behavior; read-only inspection of this repo's
source. No database, source, or memory contents modified.

---

## 1. The gap, stated in NeuroStrata's own terms

NeuroStrata's memory surfaces are **pull-only and topic-blind**. An agent
recalls a fact when *the agent* decides to pull it, using a query the agent
*already knows to ask*. Nothing in the system says: "you are now working on
topic X; here is what we know about X."

Concretely, four separate shortfalls compound:

**A. No proactive/associative retrieval trigger.** The only surfaces are
`neurostrata_get_snapshot` (session-start oriented, top-5 by access count) and
`neurostrata_search_memory` (explicit query). Neither is invoked *because* the
conversation entered a topic. `AGENTS.md` §3 does mandate a pre-flight
`get_snapshot`/`search_memory` on every new task — but that is a session-start
discipline, not a topic-transition one, and the mandated search is still
query-shaped by the agent's own prior knowledge.

**B. No representable shape for "how is system X actually reached."**
NeuroStrata captures *what code exists* (AST symbols) and *what the rules say*
(rule/fact/procedure). It has no vocabulary for the connective tissue between
them: which component talks to which external system, through which client or
server channel, under which credential boundary, and why that boundary exists.
This is a *shape* gap, not a *storage* gap — see §3.1, because the storage
substrate already accommodates it.

**C. No seat for assumption-validation.** The Behavioral Guard
(`neurostrata_guard_validate`) validates **actions** — a `bash` command or a
file write — before execution. It has no surface for intercepting a
*conversational claim* ("security rules don't apply here because…") and
checking it against memory. The agent can therefore assert an unverified
architectural claim with no machine check standing behind it.

**D. No topic→knowledge linkage.** The Episodic Buffer already writes a
`### 🔄 Topic Switch [tags]` marker when a turn is tagged — and **nothing
reads it back**. It is a write-only artifact. So the *topic signal already
exists inside NeuroStrata*; it simply terminates at a log file and never
reaches recall. The same shape appears at the namespace level: `metadata.domain`
is accepted on `add_memory` and echoed into the graph export, but **no search
or snapshot path filters on it** — it is write-only metadata. The ingredients
for a linkage exist; the wiring does not.

---

## 2. The reproduction case (evidence, scoped — NOT a requirement)

**This section is an observed instance of the general failure above. Nothing in
it is a NeuroStrata requirement, a NeuroStrata fact, or a claim about any
consumer system's configuration.**

Observed: during a session in a downstream consumer project, an agent answered
a question about whether a managed database's client-side security rules
applied, **without first checking how that database is actually accessed in
that codebase.** The agent had not been challenged, and did not self-check. A
prior exchange had established the access path; the agent did not recall or
consult it.

Two readings are possible, and the owner explicitly raised both:

- The fact was never *stored* → shortfall **B** (nothing to recall).
- The fact *was* stored but never *surfaced* → shortfall **A** (recall is
  pull-only).

The owner judged the failure to be a *NeuroStrata* problem rather than a
consumer-project problem, on the grounds that "even if the fact had been in
NeuroStrata, the agent still would not have thought to check it." That is
shortfall **A**, and it is the shortfall this brief is primarily about.

**Guarding against a false conclusion:** the *missing knowledge* in this case
is a fact about a specific consumer system. It is **not** something NeuroStrata
should encode as a NeuroStrata-level claim, and it does not belong in the
`NeuroStrata` namespace. It is a consumer-project fact, and it belongs to that
project's namespace. What NeuroStrata owes is the *mechanism* that surfaces
such facts at the right moment — not the fact itself.

---

## 3. What already exists (verified, so the panel does not re-propose it)

Verified by reading this repo's source on 2026-10-20.

### 3.1 A new memory type is *not* the blocker

`src/schemas/memory-vocabulary.v3.json` declares vocabulary v4 with types
`rule, fact, task, directory, file, markdown, symbol, procedure`. `context`
is **not** a first-class type — it appears under `extensions.existing_custom_types`
alongside `preference, bootstrap, persona`. And `extensions.custom_memory_types`
states *"Any nonempty string is accepted unchanged"*, confirmed by
`src/store/ladybug.rs:3755` (`vocabulary_validation_preserves_custom_memory_types`).

So a `topology`-shaped memory can be written **today** with no core change.
The real cost is **discoverability, not storage**: the agent only learns
vocabulary from `tool_summaries.memory_type` and the tool description, so an
undeclared custom type is invisible to it. A topology type would need to be
*declared* in the vocabulary (a `vocabulary_version` bump per
`extensions.version_policy`) to be used reliably — but the storage
prerequisite is already met. A panel that proposes "add a topology memory
type" as if it were a schema migration has misread the substrate.

### 3.2 The procedure mechanism is weaker than it looks

The original report proposed "a procedure that fires on topic entry." Three
verified constraints:

- The `procedure.trigger` enum is **closed**: `session-start`, `before-edit`,
  `after-mutation`, `every-n-sessions:N`, `every-n-days:N`. There is no
  topic-entry trigger, and adding one is a vocabulary change, not a data
  write.
- `next_due` (`src/tools/procedure.rs:91`) gives a real answer **only** for
  `every-n-days:N`; every other trigger is owed at `now`. Due-logic is
  deliberately thin.
- Procedures surface via the `procedures_due` strap on `get_snapshot`
  (`src/server.rs:1456`) and are stamped by `neurostrata_procedure_perform`.
  **There is no runtime auto-fire.** The agent must still choose to perform
  one. A procedure is a *prompt the agent may heed*, not a hook that fires.

### 3.3 A newly written fact cannot surface via the snapshot

This is the mechanical explanation for "it did not remember up front," and it
is the sharpest finding in this brief.

`stamp_new_memory` (`src/server.rs:1122`) sets `access_count: 0` on write.
`handle_get_snapshot` (`src/server.rs:1461-1476`) sorts strictly by
`access_count` descending and then `truncate(5)`. A memory that has never been
retrieved has `access_count: 0`, ranks last, and is **cut from the snapshot**.

So even if the missing fact had been correctly stored, the snapshot — the one
call `AGENTS.md` §3 makes mandatory — would not have shown it. The
access-count ranking and the top-5 cut are a *deliberate* design (prune
unused rules; README "Synaptic Pruning"), but their interaction with
newly-written knowledge is a real recall hole. Note the search path does not
share this bias: it over-fetches (`limit * 10`) and ranks by vector distance
with a usage *boost* of `0.05 * ln(1+n)` (`src/store/ladybug.rs:1069`), so a
new fact is findable — but only if the agent searches for it.

### 3.4 The event bus has no topic pulse

`ThalamicPulse` carries four variants — `Created`, `Superseded`, `Archived`,
`GuardedActionFired` — to bookkeeping subscribers (`EpisodicPointerEcho`,
`ExportFreshnessDirty`, `GuardEventLog`) and the mesh outbox. No pulse
represents "the conversation changed topic," and no subscriber surfaces
knowledge to an agent. Any proactive-retrieval design lands here or invents
its own channel.

### 3.5 The guard is action-time only

`neurostrata_guard_validate` takes `action_type` + `payload` and returns a
verdict before execution. There is no claim-time equivalent. Shortfall **C**
needs either a new guard surface or a different enforcement point.

### 3.6 The closed reminder enum, for completeness

`src/task/wiring.rs:49` documents `fires_on` as a closed enum:
`mcp-call-failure | memory-added-or-superseded | memory-corrected |
session-start | before-edit | during-work`. It contains a `lane-declaration`
reminder ("declare the lane this session works in") — but **`lane` has no data
model in NeuroStrata**. It is a consumer-project convention that leaked into
this repo's reminder text. Any proposal phrased in terms of "lanes" must be
re-expressed in NeuroStrata's own vocabulary (`domain`, `namespace`, or a
declared vocabulary term) before it can be evaluated.

---

## 4. The four sub-gaps, restated as questions for the panel

Each is stated as the question the owner actually wants answered, with the
verified constraint that shapes it.

**Q1 — Trigger.** What mechanism causes relevant knowledge to be surfaced at
*topic transition*, rather than only at session start or on explicit query?
Given §3.2 (no runtime auto-fire), §3.3 (access-count bias against new facts),
and §3.4 (no topic pulse), candidate seats are: a new `ThalamicPulse` variant
plus a subscriber; a declared procedure trigger plus an agent-side
acknowledgement step; a `get_snapshot` variant keyed on topic; or a
recall-strategy prompt that rides the existing `before-edit` reminder. Note the
tradeoff the panel must price: proactive recall costs context window and can
inject the wrong facts at the wrong time. Which seat degrades most safely when
it is wrong?

**Q2 — Shape.** What is the minimal representable structure for "component X
reaches system Y by channel Z under credential boundary W, because reason R"?
Per §3.1 this needs a *declared* vocabulary term (or a documented
`fact` + metadata convention) to be discoverable, and it needs a *relation*
vocabulary — the current relation set is `GOVERNS, CONTAINS, RELATES_TO,
EXTRACTED_FROM` and custom relations are **not materialized**
(`extensions.custom_relations`). Is topology a new memory type, a metadata
shape on `fact`, or a new relation? Note the vocabulary already has a
precedent for typed sub-fields on a type (`rule.fields.enforcement/source/guard`),
so a `topology` type with typed fields is consistent with existing design.

**Q3 — Enforcement.** What enforces "verify an access-path claim against
memory before asserting it"? Per §3.5 the guard is action-time. Options: a
guard surface for claims; a `rule` whose `enforcement` field is set honestly
to `PARTIAL`/`NOT_ENFORCED` (the vocabulary already supports this honesty
distinction, and `task_validate` checks it); or a procedural obligation.
The panel should be explicit that an un-enforced rule is a *note*, not a rule
— the `enforcement`/`guard` field pair exists precisely so a rule cannot
overclaim its own enforcement.

**Q4 — Linkage.** What binds a topic to its knowledge? Per §1D the Topic
Switch marker and `metadata.domain` are both already written and never read
back — so this may be a *wiring* problem, not a *design* problem. Does topic
linkage ride the episodic tag, a `domain` filter on search, a namespace
convention, or a declared vocabulary term? If it rides `domain`, note that
`domain` is currently free-text with no validation and no filter — it would
need both. *[changed by the proactive-recall plan: `domain` is normalized to a slug on write, and it is now an optional soft filter on search]*

**Q5 — Negative exemplars (added by the external review).** What represents
"we tried this here, and it went wrong"? NeuroStrata's vocabulary has no type
for a failed attempt: a `rule` states what must hold, a `fact` states what is
true, and there is nothing for a *negative* example. The companion external
review (`2026-10-20-external-review-hyperskill.md`) found this gap in the
course of assessing HyperSkill, which extracts *mistakes* from failed
trajectories as first-class nodes to avoid. See that document, Idea 5, for
whether a polarity field on an existing type would suffice.

**External review.** `2026-10-20-external-review-hyperskill.md` assesses
HyperSkill (arXiv:2608.16114) against these questions. It contributes a
mechanism to Q1, Q2, and Q4, the *shape* of an answer to Q3, and Q5 above. It
does not change any question here.

---

## 5. Non-goals and guardrails for this brief

1. **No consumer-system facts belong in the `NeuroStrata` namespace.** The
   observed episode's missing knowledge is a fact about a downstream consumer
   system. It belongs to that project's namespace. NeuroStrata's obligation is
   the *mechanism* that surfaces such facts at the right moment.
2. **No claim is made that any consumer system is misconfigured.** This brief
   says nothing about how any specific datastore, vendor, or service is
   actually accessed, in any project. The access path in the observed case is
   an unverified observation, not a finding.
3. **This is not a proposal to change behavior.** No new trigger, type, or
   enforcement point is being introduced here. The panel evaluates; a later
   decision authorizes any change.
4. **Do not import consumer vocabulary.** `lane`, and any product name
   appearing in §2, are not NeuroStrata concepts (§3.6). Propositions must be
   stated in NeuroStrata's own terms to be evaluable.

### Memory-extraction boundary (if this file is read by an extracting agent)

Extracting **from §1, §3, §4** is appropriate: those are claims about
NeuroStrata's own machinery, verified against source, and they carry
`metadata.source` anchors.

Extracting **from §2** is **not** appropriate as a NeuroStrata-level fact. It
is an anecdote about a consumer session. If a durable memory is wanted from
§2, it should record *the failure mode* ("an agent asserted an access-path
claim without consulting remembered topology") in the abstract — with no
consumer project, vendor, or system named — and it should carry
`metadata.source` naming this document.

---

## 6. Source anchors

Verified in this repo on 2026-10-20. *(the anchors were re-derived on 2026-10-22)*

| Claim | Anchor |
|---|---|
| Vocabulary v4 types; `context` is a custom type; custom types accepted unchanged | `src/schemas/memory-vocabulary.v3.json` (`memory_types`, `extensions.custom_memory_types`, `extensions.existing_custom_types`) |
| Custom memory types accepted silently | `src/store/ladybug.rs:3755` |
| Relation set; custom relations not materialized | `src/schemas/memory-vocabulary.v3.json` (`relations`, `extensions.custom_relations`) |
| Vocabulary version bump required for semantic change | `src/schemas/memory-vocabulary.v3.json` (`extensions.version_policy`) |
| Closed `procedure.trigger` enum | `src/schemas/memory-vocabulary.v3.json` (`memory_types.procedure.fields.trigger`); asserted `src/traits.rs:301` |
| `next_due` real only for `every-n-days:N` | `src/tools/procedure.rs:91` |
| Procedures surface on snapshot; no runtime auto-fire | `src/server.rs:1456`; `src/tools/procedure.rs:135` |
| New memory gets `access_count: 0` | `src/server.rs:1122` (`stamp_new_memory`) |
| Snapshot sorts by access_count, truncates to 5 | `src/server.rs:1461-1476` |
| Search ranks by distance with usage boost; over-fetches | `src/store/ladybug.rs:1008`, `src/store/ladybug.rs:1069` |
| `domain` written on add_memory; echoed to export; never filters retrieval *[changed by the proactive-recall plan: `domain` is normalized to a slug on write, and it is now an optional soft filter on search]* | `src/server.rs:1275`; `src/store/ladybug.rs:1689`, `:1742`; absent from search/snapshot queries |
| Topic Switch marker written; never read back for recall | `src/buffer.rs:71`; `src/server.rs:370`; the only two `current.md` readers are both test bodies (`src/daemon.rs:1542`, `src/tools/procedure.rs:496`) — no production path reads it |
| Guard validates actions, not claims | `src/server.rs:382`; `src/guard/validator.rs:43` |
| Rule honesty: `enforcement` / `guard` fields, checked by the gate | `src/schemas/memory-vocabulary.v3.json` (`memory_types.rule.fields`); `src/task/gate.rs:235-272` |
| ThalamicPulse variants; no topic pulse | `src/events/event.rs`; `src/events/subscribers/` |
| Closed `fires_on` reminder enum; `lane` is not a NeuroStrata data model | `src/task/wiring.rs:49`, `:158` |
| Mandatory pre-flight recall is session-start shaped | `AGENTS.md` §3 (MANDATORY PRE-FLIGHT HOOK) |

## 7. Scope

Read-only source inspection plus one owner-reported episode. No NeuroStrata
source, database, or memory contents were modified by this brief. No
implementation is proposed or authorized.
