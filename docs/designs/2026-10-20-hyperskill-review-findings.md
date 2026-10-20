# Panel findings B: author's summation and NeuroStrata bug register

**Read after** `2026-10-20-hyperskill-panel-brief.md`, and only after
recording your own answers to Q1-Q5 there. The brief is deliberately
conclusion-free; this file is where the author's conclusions and claims live.

**Status:** capture only. No fix is authorized by this document. Every claim
below is stated with its evidence so the panel can refute it.

> ### ⚠️ CORRECTED — read `2026-10-20-argon-k3-panel-verdicts.md` (Part C)
>
> The Argon (Gemini 4) and K3 (Kimi K3) panel ran on this package and
> **confirmed all three findings** (BUG-1, BUG-2, DOC-1) with the same
> severities. But it also found **six errors in the material below**, all
> verified against source at HEAD `230c5ff`. Do not read this file without
> Part C alongside it.
>
> | # | Correction |
> |---|---|
> | **C1** | BUG-1's "Regression evidence" says `6e5ffe8` "introduced both the `score: 10.0` assignment and the evidence rendering." **The sentinel predates it** — `ce3004e^:src/store/kuzu.rs:206` already carries `score: 10.0`. `6e5ffe8` added evidence-carrying expansion rows and per-family admission. This file contradicts itself on the point. |
> | **C2** | §3's claim that "fixing BUG-1 alone would widen BUG-2's window" is **mechanically wrong**: `embed_dedup_and_upsert` calls `store.search` **directly** (`server.rs:1152`), bypassing `handle_search_memory`, and `store.search` already returns the expansion rows. The dedup filter receives all five expansions **today**. The recommended order (BUG-2 first or together) stands; the stated mechanism does not. |
> | **C3** | BUG-1's Option (a) ("restore the pre-`61451bd` behavior") is **broken as written**: with `fetch = 15` (`server.rs:1950`), removing `results.truncate(5)` yields up to **30** rendered rows. The old behavior is only restorable if the `!include_tasks` filter moves **into** `store.search`. |
> | **C4** | §1.3's highest-rated idea rests on **three false code claims**: `hit_count` is never incremented anywhere; the guard's semantic evaluator (`guard/semantic.rs:68-89`) cannot see architectural `rule`/`fact`/`procedure` rows at all; and `rule_ids_triggered` (`guard/validator.rs:70`) records the top-3 vector matches, computed **before** the `deprecated` check and **before** `matches_trigger`. The guard's polarity is also inverted — a `REJECT` verdict means the rule *caught* a bad action. **The idea must be re-derived.** |
> | **C5** | §1.2's idea ranking contradicts §3's "the bug register is upstream of the idea list." The bug register is the priority list; the seven ideas are a secondary sort on top of a *working* system. |
> | **C6** | Two framing slips: §1.1's "episode node ≡ `task` row" is too strong (analogous in shape, not equivalent in semantics); §1.2 prices negative exemplars as "low" when a vocabulary-contract change with no migration path is **medium**. |
>
> **Six new NeuroStrata defects** (BUG-3…BUG-8) and **five new HyperSkill
> paper-vs-code defects** were also found, and **BUG-2's fix was already
> prescribed** on 2026-10-18 in
> `docs/superpowers/specs/2026-10-18-local-self-improvement-design.md` Phase 1 —
> never implemented. All of it is in Part C.

---

## 1. The author's summation

### 1.1 Verdict

**Complementary, but at a different altitude. Not a competitor, not a
drop-in, and no reason to replace NeuroStrata's graph.**

HyperSkill is an *experiential* memory for agents that execute tasks in an
environment with a scorer. NeuroStrata is a *durable architectural-knowledge*
store with provenance, bi-temporal semantics, and cross-instance sharing.
Their unit of knowledge differs (trajectory vs. row), their correctness oracle
differs (scorer vs. none), and their lifecycle differs (prune/merge vs.
supersede/archive).

**On the graph specifically:** the author's first pass overstated HyperSkill's
contribution as "the n-ary hyperedge argument," which read as a case against
NeuroStrata's graph. Checking the source corrected that. HyperSkill's
"hyperedge" is a *node* with `role="hyperedge"` joined to members by ordinary
binary `role="membership"` edges (`hypergraph_core.py:532-536`); `networkx`
has no hypergraph primitive. That is a **bipartite two-level graph** — the
same shape as NeuroStrata's `Memory` nodes with four binary relation types
(`src/store/ladybug.rs:814-824`), whose 1-hop/2-hop directed traversal with
evidence paths (`src/store/ladybug.rs:1085-1116`) is the same operation as
HyperSkill's "expand matched subtask nodes to their incident hyperedges"
(`hypergraph_core.py:614-617`). HyperSkill's episode node ≡ NeuroStrata's
`task` row; its membership edge ≡ `RELATES_TO`/`EXTRACTED_FROM`.

So the transferable parts are **two conventions and one signal**, not a new
substrate:

1. **A node convention** — an explicit episode-level record that groups its
   members and carries the distilled lesson. NeuroStrata's `task` row is a
   *work* record (status, assignee, history), not an *episode* record.
   Implementable with existing relations and no vocabulary change, because
   endpoint role typing is descriptive only (`extensions.role_enforcement`)
   and edge creation checks string shape, not endpoint types
   (`src/store/ladybug.rs:444-469`). Only *new relation types* are blocked
   (`extensions.custom_relations`).
2. **A co-occurrence signal** over the existing graph.
3. **An outcome-weighted quality formula** — see §1.3, the highest-value item.

### 1.2 Answers to the panel's questions, as the author sees them

**Q1 (what to borrow).** Ranked, with the author's reasoning:

| # | Idea | Why | Cost |
|---|---|---|---|
| 1 | Outcome-weighted memory quality | Solves a NeuroStrata weakness with a *formula*, and the input data already exists (§1.3) | low |
| 2 | Re-key the graph path on the current task's decomposition | Surfaces knowledge wording misses; the graph path already exists (once O1/O2 are fixed) | medium (an LLM call per task) |
| 3 | Co-occurrence as a ranking term over `RELATES_TO` | Complements vector similarity with a structural signal; cheap over existing edges | low–medium |
| 4 | An explicit episode-level node | The existing `task` row is a work record, not an episode record | medium |
| 5 | Negative exemplars as a first-class type | Closes a gap neither the brief nor NeuroStrata's vocabulary names | low (a polarity field) |
| 6 | Consolidation by merging, not deletion | `supersede_memory` already exists as the consolidation primitive and preserves history | medium |
| 7 | *Anti-pattern:* do not transplant the substrate | `networkx` + numpy index, the local MiniLM encoder, and prune-as-delete all conflict with NeuroStrata's design | — |

**Q2 (shape).** No, the shape is not missing. NeuroStrata's graph already
expresses the bipartite episode/member structure. The genuine open question is
narrower: *is there value in an explicit episode record that carries the
distilled lesson, as distinct from a task record?* The author's view: yes,
probably — but this is the idea the author is **least** confident in, because
the episode's value in HyperSkill comes from its utility counters and its
lesson, and NeuroStrata's `task` row has neither.

**Q3 (co-occurrence).** As a **ranking term**, not a separate retrieval
path — and it must obey the existing relevance cap. The rule already in
NeuroStrata's memory is that a boost is admitted *inside* the cap
(`boosted_distance = distance - (usage_gain + source_gain).min(0.5*distance)`,
`src/store/ladybug.rs:1055-1056`), so any new term stays a tie-breaker rather
than an override. κ is computable over `RELATES_TO` neighbors the expansion
already materializes.

**Q4 (the success label).** This is the author's **highest-value** item, and
the reason is that NeuroStrata already owns the input and does not use it.
HyperSkill's utility is `γ = β·success_rate + (1−β)·step_efficiency` (β=0.7),
updated from episode outcomes. NeuroStrata's guard already persists
`verdict` and `rule_ids_triggered` per validation
(`src/events/subscribers/guard_event_log.rs:140-158`), and behavioral rules
already carry `hit_count` (`src/guard/semantic.rs:43`). Meanwhile
`access_count` (`src/store/ladybug.rs:1822`) increments on **retrieval**, so a
memory can be retrieved constantly and be wrong every time — precisely the
"retrieved often but consistently fails" case HyperSkill prunes and
NeuroStrata cannot currently detect.

The honest caveat: the "step efficiency" term has no NeuroStrata analog for
architectural claims, and "success" for an architectural claim is not
self-evident. A guard verdict is a *proxy*, not a correctness oracle, and a
rule that is never triggered is not thereby wrong.

**Q5 (pruning).** **Do not delete.** NeuroStrata's answer to "this memory is
bad" should be `supersede` (keep the wording as history) or `archive`
(tombstone, row kept). Transplanting prune-as-delete would violate the
tombstone-over-delete invariant that the README and the `neurostrata-mcp`
documentation both state. Merging — the safer half of HyperSkill's M3 — is
worth borrowing, because `supersede_memory` already is the consolidation
primitive.

### 1.3 The idea the author rates highest, stated precisely

> **Give every memory an outcome-weighted quality score, computed from
> evidence NeuroStrata already collects, and use it to rank — not to delete.**

It is a formula, not a subsystem. The ingredients exist; only the connection
is missing. The two decisions the panel must make are (a) what counts as
"success" for an architectural claim, and (b) whether a low score causes
*demotion* (ranked lower, kept) or *retirement* (superseded/archived, kept as
history). The author recommends demotion, because deletion is off the table
and because demotion is reversible.

### 1.4 A gap the companion brief did not name

**Negative exemplars.** HyperSkill extracts *mistakes* from failed
trajectories and stores them as first-class nodes to avoid. NeuroStrata has no
vocabulary for a failed attempt: a `rule` states what must hold, a `fact`
states what is true, and there is no type for "this approach was tried here and
it went wrong." The companion brief
(`2026-10-20-gap-proactive-recall-and-access-topology.md`) did not name this;
it was found during this review and is now added there as Q5.

Cheapest fit: a polarity field on an existing type, which needs no vocabulary
bump. Genuine open question: does this belong in the `NeuroStrata` namespace
at all, or only in consumer-project namespaces?

---

## 2. Bug register — NeuroStrata-side defects found during this review

**The author's claim: three of these are real defects, and the third is a
documentation problem rather than a code defect.** Each is stated with
evidence and a falsification test. The panel should confirm, refute, or
re-scope each.

### BUG-1 — The graph expansion never reaches the agent (regression)

**Severity:** high. **Type:** behavioral regression with a traceable commit.

**Claim.** `search_memory` computes the graph expansion, and the expansion's
results — including their evidence paths — are discarded before the agent
sees them. A rule reachable only through a `RELATES_TO` edge is not returned.

**Mechanism.** Expansion rows are assigned a fixed `score: 10.0`
(`src/store/ladybug.rs:1277`); primary rows carry `score = boosted_distance`,
a distance typically well under 1 (`:1062`). The store sorts by score
ascending and truncates to `limit * 2` (`:1284-1287`), so expansions sort
after every primary. `handle_search_memory` then truncates to **5**
(`src/server.rs:1978`), keeping the 5 lowest-scoring rows — which, whenever
at least 5 primaries exist, are 5 primaries. The store's own admission logic
caps each family at `limit` and admits expansions in one-hop-before-governing
order (`:1239-1258`), which is machinery built to *interleave* the two
families; the server's flat truncate defeats it.

**Regression evidence.** The expansion and its evidence paths landed in
`6e5ffe8` (2026-09-19, "feat(search): path-labeled retrieval evidence + memory
vocabulary v1"), which introduced both the `score: 10.0` assignment and the
evidence rendering. The `results.truncate(5)` arrived later, in `61451bd`
(2026-10-08, "feat: task subsystem — replace bd/beads with in-binary
LadybugDB tasks"), in the same change that raised the store fetch from 5 to
15 to support the new `include_tasks` filter. Before `61451bd`,
`handle_search_memory` passed the store's `limit * 2` rows straight to
`render_search_results` with no truncation of its own — so with `limit = 5`
the store returned 5 primaries plus up to 5 expansions, **all rendered, with
their evidence paths**. The truncation was introduced to filter *task rows*,
and in doing so it severed the expansion path.

**Live confirmation.** Repeated `neurostrata_search_memory` calls against the
`NeuroStrata` namespace — a semantic query, an exact-token query, a nonsense
query, and a query aimed at a rule reachable *only* by a `RELATES_TO` edge —
each returned exactly 5 results and **zero** `Path [one_hop]` or
`Why: This memory is one graph hop from a direct match.` lines. The
edge-targeted rule never appeared. The evidence renderer
(`src/server.rs:2262-2340`) and its byte budgets are therefore dead in
practice for `search_memory`.

**Falsification test.** Reconstruct the pre-`61451bd` server behavior — remove
the `truncate(5)` and render the store's `limit * 2` rows — and confirm that
expansion rows with `OneHop`/`Governs2hop` evidence appear in the response.
The store-level tests already assert the expansion works
(`src/store/ladybug.rs:3932`, `:3993`).

**Panel questions.** Should the fix be (a) restore the pre-`61451bd` behavior,
(b) keep 5 but allocate slots per family the way the store's admission logic
already does, or (c) something else? What should the promotion rule be for
expansions, given the relevance-cap rule? Note that a fix here makes the
graph pay off and is smaller and cheaper than any of the seven ideas in §1.2.

### BUG-2 — The dedup candidate filter is inverted, and admits expansions

**Severity:** medium (dormant by default). **Type:** logic inversion, present
since introduction.

**Claim.** The dedup path filters candidates with `c.score >= 0.85`, commented
"Filter to only high-similarity candidates" (`src/server.rs:1153-1154`).
Score is a **distance** — lower is better (`array_distance`, ordered ascending
at `src/store/ladybug.rs:1011-1013`; `boosted_distance` at `:1056`). So the
filter keeps rows whose distance is **≥ 0.85** — the *far* ones — and discards
the near duplicates it is meant to catch. It also admits every expansion row,
whose fixed `score: 10.0` passes any such threshold.

**Evidence.** `score` has been a distance in every incarnation of the store in
this repository's history: the Kuzu-era store already had
`score: boosted_distance` and `score: 10.0, // High distance (low relevance
score) ensures they rank below direct matches` (`git show ce3004e^:src/store/kuzu.rs`
lines 162, 206). The filter was introduced in `af134ca` (2026-10-08, "feat: add
judgment model integration for memory deduplication"), against that
distance-based score — so the inversion was present from birth, not introduced
by a later change.

**Dormancy.** The path runs only when a judgment API key is configured
(`judgment_api_key` is the enable signal, `src/judgment/mod.rs`). The
`deduplication.similarity_threshold` config field
(`src/config.rs:20-21`, default 0.85) is **never read** on this path — the
threshold is hardcoded. So the operator's configured threshold is silently
ignored even where the path is live.

**Falsification test.** With a judgment provider configured, submit a memory
nearly identical to an existing one. If the inverted filter is the cause, the
near duplicate will *not* be among the candidates sent to the judge.

**Panel questions.** Is the intended semantics `score <= threshold`? If so,
should it also exclude expansion rows, and should it read the configured
`similarity_threshold` rather than a hardcoded 0.85? Note the interaction with
BUG-1: fixing the truncation would admit *more* expansion rows into this
filter, and every one of them passes a `>= 0.85` threshold.

### DOC-1 — The documented hybrid BM25/Tantivy/RFF retrieval never existed

**Severity:** medium (misleads readers and reviewers). **Type:**
documentation defect, not a code defect.

**Claim.** The documentation asserts a hybrid semantic + BM25 retrieval path
fused by Reciprocal Rank Fusion, implemented with Tantivy, in four places:

- `README.md:17` — "an embedded LadybugDB vector store and full-text search
  (BM25 via Tantivy)";
- `docs/COGNITIVE_ARCHITECTURE.md` §6 — "utilizes `tantivy` to combine dense
  vector semantic search with exact Full-Text Search (BM25 keyword matching) …
  merged via Reciprocal Rank Fusion (RRF)";
- `docs/architecture/MEMORY_AGENTS_VERDICT.md:36` — "`neurostrata_search_memory`
  — hybrid dense-vector + BM25 (tantivy) merged via Reciprocal Rank Fusion";
- `CHANGELOG.md:173` — "Tantivy FTS Hybrid Search: Integrated hybrid
  exact-keyword and vector search using Reciprocal Rank Fusion (RRF)".

**Evidence.** `git log -S'tantivy' -- Cargo.toml` is **empty**: the crate has
never been a dependency in this repository's history. There is no
`CREATE_INDEX`/FTS statement in `src/store/ladybug.rs`, and every `MATCH` in
the store is vector-distance, id-equality, or a graph traversal. The
`CHANGELOG.md` entry was added in `09d569b` (2026-05-25) as a shipped feature.
The source comment at `src/store/ladybug.rs:1085` names its step "Hybrid
GraphRAG" — which is a *graph* expansion, not a lexical one, and may be the
origin of the confusion.

**Why this is a documentation defect, not a missing feature.** There is no
code to repair: the feature was described but never built. The question for
the panel is which is authoritative — and note the answer is not "add BM25,"
because that is a build decision with a real cost (a new dependency, a new
index, a new fusion rule) that the owner may not want.

**Practical consequence (independent of the above).** Exact-token recall
depends on the embedder rather than keyword matching. A memory whose text is
an exact identifier — a function name, an env var, a config key, an error
string — is found only if its *embedding* lands near the query. Semantic
similarity is a blunt instrument for "the user typed this exact identifier."

**Falsification test.** `cargo tree | grep -i tantivy` returns nothing;
`rg 'CREATE_INDEX|FTS' src/store/ladybug.rs` returns nothing.

**Panel questions.** Should the docs be corrected to describe what the code
does? If hybrid retrieval is wanted, is it a build item? And is the
`MEMORY_AGENTS_VERDICT.md` reasoning affected — that document uses
"RRF + neural gain" as the justification for *not* adding an LLM reranking
layer, and the argument's premise (that RRF exists) is false, so the
conclusion may need re-deriving even though the recommendation (no LLM in the
retrieval path) may still be right.

### Not bugs — things the review looked at and cleared

- **The four relation tables and their directions** are correct and
  intentional; `extensions.custom_relations` is a deliberate honesty
  statement, not an oversight.
- **`access_count` counting retrievals rather than correctness** is by
  design (README, "Synaptic Pruning": unused rules decay out of the context
  window). The *gap* is that no correctness signal is joined to it — BUG-2's
  territory and §1.3's opportunity, not a defect in the counter itself.
- **`get_snapshot` truncating to 5 by `access_count`** is the documented
  "top-5 highest-weighted active Engrams" behavior, and the brief's §3.3
  already flags the *interaction* (a never-retrieved fact cannot surface) as
  a design question rather than a bug.
- **The `include_tasks` filter itself** works as intended; BUG-1 is the
  truncation that came with it, not the filter.

---

## 3. How these findings relate to the seven ideas

The bug register is **upstream of** the idea list, not parallel to it:

- BUG-1 is the reason Idea 2 is cheap. The graph path already exists and
  already computes the right thing; it is simply not delivered. Fix the
  delivery and the "re-key the expansion on the task decomposition" idea
  becomes a change to *which* nodes seed the expansion, not a new subsystem.
- BUG-1 is also why Idea 3 (co-occurrence) is cheap: κ is computable over
  the `RELATES_TO` neighbors the expansion already materializes — but only
  once those neighbors actually reach the agent.
- BUG-2 means the dedup path currently protects the store from *far* memories
  rather than near ones, and would silently change behavior if BUG-1 is fixed
  without also fixing BUG-2. **Fixing BUG-1 alone would widen that window.**
  The author flags this ordering explicitly.
- DOC-1 changes what "a second retrieval track" means. If the documented
  hybrid retrieval is wanted, it is absent work; if the docs are wrong, the
  work is to correct them. Either way, the co-occurrence signal (Idea 3) is
  a candidate for that track rather than an addition to a track that already
  has two.

**The author's recommended order for the panel:** settle DOC-1 (what is
authoritative) and BUG-1 (why the graph does not reach agents) first; they
are small and they change the meaning of everything else. Then evaluate the
idea list against a system that actually delivers its graph.

---

## 4. What the author may have gotten wrong

Offered explicitly, because the panel's disagreement is the point.

- **The verdict that the graph is "already the right shape"** could be wrong
  if the panel thinks the episode/lesson distinction deserves a first-class
  representation that the current relation set genuinely cannot express. The
  author argues `role_enforcement` is descriptive only, so a new *node* type
  can use existing relations — but the author has not designed that, only
  argued it is possible.
- **The claim that the seven ideas are ordered by value** is a judgment about
  an unbuilt system, and the author may be over-weighting ideas that are
  cheap to describe rather than cheap to build. Idea 2 in particular carries
  a per-task LLM call that the author describes but has not costed against
  NeuroStrata's latency budget.
- **The author may be anchoring on HyperSkill's framing** even after
  correcting the hypergraph over-claim. Having read the source, the author
  may notice HyperSkill's *problems* less readily than they would on a
  cold read — which is exactly why Part A exists.
- **The claim that BUG-1 is a regression rather than a long-standing
  limitation** rests on reading `git log -L` output. The panel should
  confirm the pre-`61451bd` behavior directly, and should consider whether
  the pre-`61451bd` behavior (5 primaries + 5 expansions rendered) was
  itself intended or merely what the code happened to do.
- **The severity ratings** (high / medium / dormant) are the author's, and
  the author has no telemetry on how often agents were affected.

---

## 5. Source anchors

**HyperSkill** (cloned to `/tmp/opencode/HyperSkill`, commit `a34978a`):
`hyperskill/hypergraph_core.py:494` `add_hyperedge`; `:532-536` episode node
and membership edges; `:551-557` membership read-back; `:568` `retrieve`;
`:602-617` subtask path; `:667` `_rank_skills_mistakes` (κ, sort key `(c, sim)`
at `:689`); `:698` `maintain_graph`; `:724` `_prune_low_utility` (delete at
`:746`); `:776` `_build_co_occurrence_matrix`; `:800` `_l_step_propagation`;
`:828` `_find_merge_candidates`; `:295` `_compute_utility_score`; `:317`
`update_utility`; `hyperskill/provider.py:135` `provide_memory`; `:274`
`take_in_memory`; `:449` `_extract_with_llm`; `:598` `_generate_initial_plan`;
`:546` `_llm_merge_nodes`; `hyperskill/config.py` hyperparameters.
Paper: arXiv:2608.16114 §3.1–3.5, Table 1, Fig. 4a–c, §4.2 finding ❸,
Limitations.

**NeuroStrata** (this repo, verified 2026-10-20):
relation tables `src/store/ladybug.rs:814-824`; expansion
`src/store/ladybug.rs:1085-1116`; family admission `:1239-1258`; expansion
score `:1277`; primary score `:1051-1062`; store truncate `:1284-1287`;
server truncate `src/server.rs:1978`; evidence rendering `src/server.rs:2236-2340`;
dedup filter `src/server.rs:1152-1154`; dedup enable signal
`src/judgment/mod.rs`; unused `similarity_threshold` `src/config.rs:20-21`;
vocabulary `src/schemas/memory-vocabulary.v3.json`; guard outcome data
`src/events/subscribers/guard_event_log.rs:140-158`; `hit_count`
`src/guard/semantic.rs:43`; `access_count` `src/store/ladybug.rs:1822`;
snapshot `src/server.rs:1423-1476`; store expansion tests
`src/store/ladybug.rs:3932`, `:3993`.
Commits: `6e5ffe8` (2026-09-19, expansion + evidence paths), `61451bd`
(2026-10-08, `truncate(5)` + fetch 15), `af134ca` (2026-10-08, dedup
filter), `09d569b` (2026-05-25, CHANGELOG FTS claim), `ce3004e` (2026-04-27,
Ladybug migration; pre-migration Kuzu store confirms distance-based score).

**Companion documents:** `2026-10-20-gap-proactive-recall-and-access-topology.md`
(the four-question brief; now also Q5), `2026-10-20-external-review-hyperskill.md`
(the working analysis this file condenses).
