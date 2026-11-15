# Panel verdicts: Argon (Gemini 4) and K3 (Kimi K3) on the NeuroStrata / HyperSkill package

**Part C of the review package.** Read order: Part A
(`2026-10-20-hyperskill-panel-brief.md`, conclusion-free) → Part B
(`2026-10-20-hyperskill-review-findings.md`, the author's summation and bug
register) → **this file** (the panel's verdicts, the corrections to Part B, and
the adjudicated fix plan).

**Status:** review output. No fix is authorized by this document. The rulings
below are the panel's; implementation decisions remain the owner's.

---

## 0. Method and provenance

Two independent architect seats ran against the same package, concurrently,
with the same brief and **without sight of each other's verdict**:

| Seat | Model | Session |
|---|---|---|
| Argon | Gemini 4 Argon (Copilot) | `ses_ea449a3acffeIOzp0ErjL5eug9` |
| K3 | Kimi K3 (Copilot) | `ses_ea449a3aaffeg5aVrpHhjitqL3` |

Both were instructed to read Part A first, **write their own answers to Q1–Q5
before opening Part B**, verify every claim in Part B against source and git
history, and rule on the proposed fixes. Both were told, explicitly, that
disagreement with the author was the useful output.

Both seats then worked independently for the duration of the review. **Neither
saw the other's verdict**, and neither was given the author's conclusions
before forming a position on Q1–Q5.

### 0.1 Independent verification performed by the orchestrator

Neither verdict was relayed on trust. Every claim that changes a conclusion was
re-checked against this repo's source at HEAD (`230c5ff`) and against git
history. Results:

| # | Claim | Source | Verdict |
|---|---|---|---|
| 1 | `hit_count` is never incremented anywhere | `git grep hit_count src/` → only `guard/models.rs:49` (field), `guard/semantic.rs:43` (serialize), `:76` (deserialize), `server.rs:412` (learn schema), `:2527` (error text) | **Argon correct** |
| 2 | The guard's semantic evaluator only matches `guard_rule` rows, never architectural `rule`/`fact`/`procedure` rows | `guard/semantic.rs:68-89` — `filter_map` requires `version`, `rule_class`, `trigger_pattern`, `hit_count`, `status` metadata, fields that exist only on `memory_type: "guard_rule"` rows | **Argon correct** |
| 3 | `rule_ids_triggered` is computed before the `deprecated` check and before `matches_trigger` | `guard/validator.rs:70` (collect) precedes `:74` (`status == "deprecated"`) and `:79` (`matches_trigger`) | **Argon correct** |
| 4 | The dedup error message names `neurostrata_edit_memory`, a tool that does not exist | `server.rs:1162` is the only occurrence; `tools/list` contains `neurostrata_supersede_memory` / `neurostrata_archive_memory`, no `edit` tool | **Argon correct** (new defect) |
| 5 | The dedup path calls `store.search` directly and therefore already receives expansion rows | `server.rs:1152` calls `store.search(namespace, vec, 5)`; `store/ladybug.rs:1287` truncates to `limit * 2` = 10, which includes the expansion rows | **Argon correct; the author's ordering rationale is mechanically wrong** |
| 6 | `EXTRACTED_FROM` is absent from `supported_labels` | `store/ladybug.rs:1094` — `["RELATES_TO", "CONTAINS", "GOVERNS"]` | **Argon correct** (new finding) |
| 7 | Expansion candidates are ordered alphabetically by UUID | `store/ladybug.rs:1243-1249` — `candidates.sort()` over `(family: usize, id: String)` | **Argon correct** (new finding) |
| 8 | `score: 10.0` predates `6e5ffe8` | `git show ce3004e^:src/store/kuzu.rs:206` — `score: 10.0, // High distance (low relevance score)…`; the `6e5ffe8` diff *moves* the existing push rather than originating the sentinel | **Argon correct; Part B contradicts itself** |
| 9 | Removing `results.truncate(5)` alone would return up to 30 rows | `server.rs:1950` `fetch = 15` when `!include_tasks`; `store/ladybug.rs:1287` truncates to `limit * 2` = 30 | **Argon correct; Part B's Option (a) is broken as written** |
| 10 | `access_count` is incremented on expansion rows too | `server.rs:1982-1991` — the spawn loop iterates `&results` with no evidence-kind filter | **Argon correct** (new finding) |
| 11 | `store.search` admits `task` rows into `primary_ids` and expands from them | `store/ladybug.rs:1036-1062` — the primary loop skips only retired rows, then pushes every id at `:1058`; no `memory_type` filter | **Argon correct** |
| 12 | `get_snapshot` sorts by `access_count` | `server.rs:1461-1470` | **K3 correct** |
| 13 | HyperSkill's insertion-time "merge" silently drops the new node | `hyperskill/hypergraph_core.py:437-441` — `find_similar_by_vec` → `if existing is not None: return existing` | **Argon correct** |
| 14 | HyperSkill's merge prompt omits the shared trajectory fragments the paper claims it reasons over | `hyperskill/prompts.py:247-253` — `get_merge_prompt(node_type, name_a, content_a, name_b, content_b)` | **Argon correct** |
| 15 | HyperSkill credits `MISTAKE` nodes on episode success | `hyperskill/hypergraph_core.py:325-346` — `update_utility` increments `success_count` for every node in `retrieved_node_ids` | **Argon correct** |
| 16 | HyperSkill's `fragment` docstring says "Not shown to agent" but the fragments are injected into the prompt | `hyperskill/hypergraph_core.py:498-501` vs `hyperskill/provider.py:416-428` | **Argon correct** (the docstring is wrong) |

**16 of 16 Argon claims verified.** No K3 claim was contradicted by source; the
two seats' *rulings* differ (see §2) but not their factual base.

### 0.2 The finding that reframes everything else

Argon surfaced a document the author never read:
`docs/superpowers/specs/2026-10-18-local-self-improvement-design.md` (2026-10-18
— two days *before* the HyperSkill review began). Its Phase 1 already
prescribes BUG-2's fix in more detail than the author's Part B does, and
rejects the very interpretation the author proposed:

> `docs/superpowers/specs/2026-10-18-local-self-improvement-design.md:394-401`
> "Add unboosted `nearest_by_raw_distance`; test ascending ordering, exact-match
> retrieval, **invariance to access-count/source metadata**, and **exclusion of
> GraphRAG sentinel neighbors**, task/structural rows, `symbol`/`code_ast`, and
> `auto-ingestor` rows. … **Remove the inverted `score >= 0.85` filter and stop
> interpreting the existing `similarity_threshold` as a cosine threshold.** …
> Before Phase 2 establishes a distance band, verify only canonical exact
> matches produce the exact-duplicate response; raw-distance candidates remain
> recorded without speculative hints."

**BUG-2 was diagnosed and prescribed two days earlier, and that Phase 1 was
never implemented.** The author's Part B re-derived part of it independently
and proposed a fix the spec had already ruled out (`score <= threshold` on a
*boosted* distance is still wrong, because the boost makes frequently accessed
rows look closer than they are). This is the panel's most actionable output:
**the fix for BUG-2 already exists in this repo as an unimplemented spec
phase.** It should be implemented, not re-specified.

---

## 1. Consensus verdicts on the bug register

Both seats independently confirmed all three findings, and both agreed on all
three severity ratings. No finding was refuted by either seat.

### BUG-1 — graph expansion discarded before the agent sees it — **CONFIRMED, HIGH**

Both seats verified the mechanism (`store/ladybug.rs:1277` sentinel `10.0`;
`:1284-1287` ascending sort and `limit * 2` truncation; `server.rs:1978`
`results.truncate(5)`; primaries carry `boosted_distance` at `:1051-1062`) and
the regression (`git show 61451bd` adds the truncate in the same hunk that adds
the `include_tasks` filter; the pre-image renders `store.search(ns, vec, 5)`
with no server-side truncation).

Both seats independently concluded that the pre-`61451bd` behavior was
**intent, not accident**, citing the same three artifacts: the `6e5ffe8` commit
message ("responses stay bounded by the pre-existing `limit*2` contract plus at
most 2048 evidence bytes"), the deliberate per-family admission caps at
`:1239-1258`, and the renderer's evidence byte budgets at `server.rs:2236-2340`.

**K3's addition:** the feature was dead in production for ~6 weeks, and
`docs/architecture/MEMORY_AGENTS_VERDICT.md`'s downstream reasoning implicitly
relied on the graph path being live.

### BUG-2 — dedup candidate filter inverted — **CONFIRMED, MEDIUM (dormant by default)**

Both seats verified that `c.score >= 0.85` on a distance admits the *far* rows
and every expansion row, that score was never a similarity in any store
incarnation (`ce3004e^:src/store/kuzu.rs:162, 206`), and that
`deduplication.similarity_threshold` is never read on this path.

**Argon's under-scoping (both claims verified):** `deduplication.max_candidates`
(`config.rs:29`) is ignored as well, and the error returned to the agent
(`server.rs:1162`) tells it to call `neurostrata_edit_memory` — **a tool that
does not exist in `tools/list`**.

**K3's addition:** once the filter is corrected, near-duplicates will actually
reach the judgment provider for the first time. Expect a **spike in write
rejections** on namespaces that have been silently accepting duplicates since
2026-10-08. Both seats independently flagged that this path **blocks
`neurostrata_task_complete`** (Lock 2) — Argon via the laya verdict's
write-blocking hazard, K3 via the rejection spike. A fix that turns the filter
around without addressing the blocking behaviour converts a silent bug into a
loud one that can wedge task completion.

### DOC-1 — documented BM25/Tantivy/RRF never existed — **CONFIRMED, MEDIUM**

Both seats ran `git log -S'tantivy' -- Cargo.toml` (empty) and found no FTS
index or query in `src/store/ladybug.rs`. Both agreed the four documents claim
a shipped feature that never existed, and both agreed the knock-on requires
re-deriving the reasoning in `MEMORY_AGENTS_VERDICT.md` even though the
recommendation survives.

---

## 2. Where the two seats disagree — adjudicated

### 2.1 BUG-1's fix shape

| | Argon | K3 |
|---|---|---|
| Choice | **(c)** split the over-fetch from the expansion budget at the store seam, preserving the `5 + 5` (`limit * 2`) contract | **(b)** per-family slot allocation at the server layer: 4 primaries + 2 expansions, partitioned on the `10.0` sentinel |
| Key reason | Removing `truncate(5)` without moving the `!include_tasks` filter into `store.search` yields up to **30** rows (`fetch = 15` × `limit * 2`); the filter must move into the store so `limit` again means "recall rows" | Allocating slots inside 5 total slots forces primaries down to 2–3, regressing direct recall |

**Adjudication: Argon's plan, with one amendment.** Argon's is right on the
mechanics (the `fetch = 15` interaction is real and is a trap the author's
Option (a) walked straight into), and it additionally fixes two things K3's
plan does not: expansion rows currently seed from *excluded* `task` rows
(`store/ladybug.rs:1058`), and `access_count` is currently incremented on
expansion rows (`server.rs:1982-1991`).

The amendment, agreed by both: **do not partition on the magic `10.0` sentinel.**
K3 correctly notes it is fragile. Replace it with a real discriminator —
`EvidenceKind::DirectMatch` is already on the evidence objects
(`store/ladybug.rs:1243`) and `SearchResult` already carries `evidence`.

K3's budget objection is answered by the design itself: `6e5ffe8` sized the
evidence renderer (`MAX_EVIDENCE_BYTES_PER_RESPONSE = 2048`) for exactly the
`5 + 5` contract, and the commit message says so in as many words. Restoring
`5 + 5` restores the *designed* response shape; it does not exceed it.

### 2.2 BUG-2's fix semantics

| | Argon | K3 |
|---|---|---|
| Comparison | Use **unboosted raw distance** via a new `nearest_by_raw_distance`; exclude expansion rows and structural/task/symbol rows | Invert to `c.score <= (1.0 - cfg.similarity_threshold)`, i.e. treat the config as a *cosine similarity* in [0,1] |
| Threshold | Do not reinterpret `similarity_threshold` at all | Reinterpret it, with a doc note |

**Adjudication: Argon.** K3's `1.0 - threshold` reintroduces precisely the
cosine-threshold reading that `2026-10-18-local-self-improvement-design.md:399-400`
already ordered removed ("stop interpreting the existing
`similarity_threshold` as a cosine threshold"), and it still filters on a
*boosted* distance, so a frequently-retrieved row still looks closer than it
is. The spec's Phase 1 is the correct shape.

### 2.3 The author's #1 idea — outcome-weighted memory quality

| | Argon | K3 |
|---|---|---|
| Verdict on the premise | **Reject.** Three code claims in Part B §1.3 / §1.2 are false (see §0.1 rows 1–3), and the guard's polarity is inverted: a `REJECT` verdict means the rule *caught* a bad action, not that the memory *failed* | Accept. The guard's `verdict` + `rule_ids_triggered` is the per-episode label; `access_count` is ν; success rate is derivable by a join |

**Adjudication: Argon.** This is the most consequential disagreement in the
review, and it is settled by source, not by preference:

1. `hit_count` is **never incremented** anywhere in the repository.
2. `guard/semantic.rs:68-89` silently drops any row lacking the `guard_rule`
   metadata fields (`version`, `rule_class`, `trigger_pattern`, `hit_count`,
   `status`). Architectural `rule`/`fact`/`procedure` rows written by
   `neurostrata_add_memory` do not carry them, so **the guard's semantic
   evaluator never sees an architectural memory at all.**
3. `guard/validator.rs:70` collects `rule_ids` *before* the `deprecated` check
   (`:74`) and *before* `matches_trigger` (`:79`), and returns them on every
   verdict. The persisted "which rules fired" signal records **the top-3
   vector matches, regardless of whether any of them triggered.**
4. Polarity: under HyperSkill's semantics `is_correct = False` means the
   retrieved skill *failed*. A NeuroStrata guard rule that produces a
   `DeterministicReject` **succeeded** — it caught the bad action.

So the signal the author's highest-ranked idea depends on does not exist in the
form described. **The idea may still be worth having, but it must be
re-derived from signals that are actually recorded.** Argon's replacement —
lifecycle and provenance events (was a retrieved memory later superseded or
archived; did its `metadata.refs` anchors go stale on AST ingest), plus
task-completion vs. block/revert — is the honest candidate. K3's Q4 answer
(which agrees with the author) is refuted by source.

K3 contributed an orthogonal point worth keeping: a quality term must
**replace** `access_count` in the snapshot sort (`server.rs:1461-1470`), not
stack with it, or it compounds the rich-get-richer dynamic.

### 2.4 The remaining ideas

Both seats independently demoted the author's #2 (LLM task decomposition on
search):

- **Argon** rejects it: it adds an LLM call to a path whose whole argument
  (`MEMORY_AGENTS_VERDICT.md:25, 51-59`) is zero-LLM-cost deterministic
  retrieval.
- **K3** demotes it to last-but-one: the failure mode is that a plausible-but-
  wrong decomposition seeds the expansion with irrelevant nodes which then
  **displace relevant primaries** in a fixed 5-slot window — and that cannot be
  measured until BUG-1 lands and the post-fix baseline exists.

Both seats independently demoted or rejected the author's #4 (a separate
episode-node type): Argon calls it redundant with `task` + `EXTRACTED_FROM`
(Lock 2 already forces every completed task to carry a `reason` and an
extracted memory); K3 keeps it but at #5 and calls it "real value, medium
cost."

They differ on #5 (negative exemplars): Argon keeps it at #3 as a cheap
polarity field; K3 demotes it, on the grounds that a polarity field changes the
vocabulary contract in `src/schemas/memory-vocabulary.v3.json` and existing
namespaces have no migration path — **cost is medium, not low, as Part B
claims.**

### 2.5 The one idea both seats raised independently

**Build the co-occurrence signal first.** Argon upgraded it to #1; K3 put it at
#3. Both note it is nearly free: `store/ladybug.rs:1243-1249` currently orders
expansion candidates by `(family, id)` — **alphabetically by UUID** — while
`by_id` already holds every path reaching each candidate, so the in-query
co-occurrence count is already in memory and unused. Argon estimates ~15 lines
of Rust and zero schema change. K3 adds that it must stay a ranking term
inside the existing `0.5 * distance` cap (`store/ladybug.rs:1055-1056`), not an
override.

---

## 3. Corrections to Part B (the author's summation and bug register)

These are **verified errors in Part B**, not opinions. Each is restated here so
the panel record is self-contained.

**C1 — `score: 10.0` did not originate in `6e5ffe8`.** Part B §2 (BUG-1,
"Regression evidence") states that `6e5ffe8` "introduced both the `score: 10.0`
assignment and the evidence rendering." The sentinel predates it:
`ce3004e^:src/store/kuzu.rs:206` already carries
`score: 10.0, // High distance (low relevance score) ensures they rank below
direct matches`. `6e5ffe8` restructured the expansion push to carry evidence
paths and per-family admission; it did not invent the sentinel. Part B
contradicts itself on this point (its own BUG-2 evidence quotes the Kuzu-era
line). **Fix:** say the sentinel predates the Ladybug store and that `6e5ffe8`
added *evidence-carrying* expansion rows and per-family admission.

**C2 — the BUG-1 → BUG-2 ordering rationale is mechanically wrong.** Part B
§3 says "fixing BUG-1 alone would widen that window" because more expansion
rows would flow into the dedup filter. In fact `embed_dedup_and_upsert` calls
`store.search(namespace, vec, 5)` **directly** (`server.rs:1152`), bypassing
`handle_search_memory` entirely — and `store.search` already returns `limit * 2`
rows including the expansions (`store/ladybug.rs:1287`). **The dedup filter
already receives all five expansion rows today.** The correct statement: BUG-1's
fix will change `store.search`'s admission and scoring, and the dedup path
consumes that same function, so the coupling is real — but it runs through the
store, not through `truncate(5)`. The recommended *order* (BUG-2 first or
atomically) is unchanged, and both seats independently endorsed it.

**C3 — the author's Option (a) for BUG-1 is broken as written.** Part B offers
"(a) restore the pre-`61451bd` behavior" as a candidate. Removing
`results.truncate(5)` while `fetch = 15` (`server.rs:1950`) yields up to **30**
rendered rows and 30 background `increment_access_count` writes, because the
store truncates to `limit * 2` (`store/ladybug.rs:1287`). The pre-`61451bd`
behavior is only restorable if the `!include_tasks` filter moves **into**
`store.search` so that `limit` again counts recall-eligible rows only.

**C4 — the author's #1 idea rests on three false code claims.** Part B §1.3
states the guard already persists `verdict` and `rule_ids_triggered` per
validation and that behavioral rules carry `hit_count`, presenting these as
the ingredients for the highest-value idea. Verified: `hit_count` is never
incremented; the guard's semantic evaluator cannot see architectural memories
at all; `rule_ids_triggered` records top-3 vector matches rather than fired
rules; and the polarity is inverted (§2.3). **The idea must be re-derived.**

**C5 — the idea list's ordering contradicts the bug register.** Part B §1.2
ranks "outcome-weighted memory quality" first while Part B §3 says the bug
register is "upstream of" the idea list. Both seats flagged this: the bug
register is the actual priority list, and the seven-ideas ranking is a
secondary sort on top of a *working* system. K3's phrasing is the sharper one:
"the seven-ideas ranking is a secondary sort on top of a working system."

**C6 — two framing slips.** (i) Part B §1.1's "HyperSkill's episode node ≡
NeuroStrata's `task` row" is too strong; they are analogous in shape, not
equivalent in semantics (K3 — and Part B concedes this two sentences later).
(ii) Part B §1.2 prices negative exemplars as "low (a polarity field)"; K3
counters that it changes the vocabulary contract in
`src/schemas/memory-vocabulary.v3.json` with no migration path for existing
namespaces — **medium**, not low.

---

## 4. New findings not in the original package

Verified by the orchestrator (§0.1 rows 4, 6, 7, 10, 11, 13–16).

### 4.1 New NeuroStrata defects

| ID | Finding | Location |
|---|---|---|
| **BUG-3** (new, medium) | The dedup rejection message instructs the agent to call `neurostrata_edit_memory`, **which is not a tool**. An agent that follows the instruction gets an error instead of a remedy. | `server.rs:1162` |
| **BUG-4** (new, medium) | The guard's semantic evaluator is structurally blind to architectural memories: `filter_map` requires `guard_rule`-only metadata (`version`, `rule_class`, `trigger_pattern`, `hit_count`, `status`), so architectural `rule`/`fact`/`procedure` rows are silently dropped. Separately, `hit_count` is never incremented anywhere, so it is always 0. | `guard/semantic.rs:68-89`; `:43`, `:76` |
| **BUG-5** (new, medium) | `rule_ids_triggered` is not a "triggered" signal: it is the top-3 vector-matched rule IDs, collected before the `deprecated` check and before `matches_trigger`, and returned on every verdict including passes. The persisted outcome signal is unusable as-is. | `guard/validator.rs:70` vs `:74`, `:79` |
| **BUG-6** (new, medium) | `EXTRACTED_FROM` is not in `supported_labels`, so the one relation type linking a memory to its episode/task is not traversable by the graph expansion. This is exactly the relation the "episode ≡ task" argument leans on. | `store/ladybug.rs:1094` |
| **BUG-7** (new, low-medium) | `access_count` is incremented on expansion rows as well as direct hits, so graph neighbours accumulate retrieval credit they never earned — which pollutes `get_snapshot`'s `access_count` sort and inflates `usage_gain` on hub nodes. | `server.rs:1982-1991` |
| **BUG-8** (new, low) | `store.search` admits `task` rows into `primary_ids` and expands from them; the `!include_tasks` filter is applied only at the server, after the expansion has already run. | `store/ladybug.rs:1036-1062`, `:1058` |

### 4.2 New HyperSkill findings (paper vs. code at `a34978a`)

Argon found four places where the implementation does not do what the paper
says. All verified.

1. **Insertion-time "merge" is a silent drop.** The paper claims a new skill
   exceeding the dedup threshold "is merged into its nearest existing
   neighbour." The code returns the existing node id and discards the new
   skill's `name`, `content`, and `metadata` (`hypergraph_core.py:437-441`).
2. **The merge prompt never sees the trajectory fragments** the paper says it
   reasons over (`prompts.py:247-253`).
3. **`MISTAKE` nodes earn success credit.** `update_utility` increments
   `success_count` for every node in `retrieved_node_ids` when the episode
   succeeded — including the mistake nodes the agent was supposed to avoid
   (`hypergraph_core.py:325-346`).
4. **The co-occurrence signal is near-degenerate at the paper's own optimum.**
   At `k = 2` (the best setting in Fig. 4a), the retrieved set is typically
   1–2 episodes, so κ is almost always 1 and the `(c, sim)` sort key
   (`hypergraph_core.py:689`) collapses to plain similarity. The paper's
   headline structural signal is weakest exactly where the paper says the
   system performs best.
5. **The `fragment` docstring is wrong.** `hypergraph_core.py:498-501` says the
   episode fragment is "Not shown to agent"; `provider.py:416-428` injects
   every retrieved fragment into the prompt.

**Net effect on the HyperSkill verdict: it strengthens it.** The author's
conclusion (complementary, not a competitor; do not transplant the substrate)
is endorsed by both seats. Argon's and K3's additional reasoning converges:
the empirical case is weaker than the paper presents, because the repository
cannot reproduce it (two commits, no tests, no CI, no lockfile, `>=` floors,
three external datasets and three API keys) and the mechanism the paper
markets — the co-occurrence signal — is weakest at the paper's optimal
operating point.

---

## 5. Adjudicated fix plan

Sequenced. Nothing here is authorized; the owner rules.

**Phase 0 — implement the spec that already exists (no new design needed).**
`docs/superpowers/specs/2026-10-18-local-self-improvement-design.md` Phase 1:
add unboosted `nearest_by_raw_distance`; exclude GraphRAG sentinel neighbours,
task/structural/symbol/`auto-ingestor` rows; remove the inverted
`score >= 0.85` filter; stop reading `similarity_threshold` as a cosine
threshold; read `max_candidates`; and until a distance band is established,
let only canonical exact matches produce a duplicate response. This is BUG-2's
fix, and it is already written down. Fix **BUG-3** in the same change (correct
the tool name to `neurostrata_supersede_memory`).

**Phase 1 — restore the graph path to the agent (BUG-1, plus BUG-7, BUG-8).**
Move the `!include_tasks` / archived / conflicting-row filters into
`store.search` so `limit` counts recall-eligible rows; restore the `5 + 5`
(`limit * 2`) contract; partition primaries from expansions using a real
discriminator (`EvidenceKind::DirectMatch`) rather than the `10.0` sentinel;
increment `access_count` only on direct hits; and stop expanding from excluded
`task` rows.

**Phase 2 — make the outcome signal real (BUG-4, BUG-5).** Make
`rule_ids_triggered` record rules that actually triggered; make `hit_count`
increment on trigger; decide explicitly whether the guard's semantic evaluator
should see architectural memories or stay a `guard_rule`-only subsystem. Do not
build the quality formula until this is settled.

**Phase 3 — the co-occurrence ranking term.** Replace the UUID ordering at
`store/ladybug.rs:1243-1249` with a count of distinct originating primaries
per candidate, keeping it strictly inside the `0.5 * distance` cap. Add
`EXTRACTED_FROM` to `supported_labels` (BUG-6) so episode co-extraction
reinforces its siblings. ~15 lines, no schema change, no LLM call.

**Phase 4 — correct the documentation (DOC-1).** Four documents, plus an
erratum in `CHANGELOG.md` rather than a rewrite of history. Re-derive the
no-LLM-reranker conclusion in `MEMORY_AGENTS_VERDICT.md` on the true premise
(deterministic boosted distance + bounded graph evidence), which both seats
agree survives. If exact-identifier recall is wanted later, both seats prefer a
narrow deterministic `CONTAINS`/symbol-id match over a Tantivy+RRF build.

**Phase 5 — the feature ideas, re-ranked by both seats.**
1. Co-occurrence ranking (above).
2. Deterministic wiring the author missed: `domain`/`file_path` seeds and
   snapshot slots for recently created rules (Argon), plus reading the Episodic
   Buffer's `### 🔄 Topic Switch [tags]` marker, which is written and never
   read (K3) — this is the gap brief's §D, which the author's HyperSkill frame
   pushed off the list.
3. Negative exemplars — as a polarity field, but budgeted as **medium**, not low.
4. Outcome-weighted quality — only after Phase 2 makes the signal real.
5. Episode-level node — keep as an open option, not a commitment.
6. LLM task decomposition on the search path — **rejected by both seats**
   (zero-LLM-cost architecture; and its failure mode displaces primaries).
7. Do not transplant HyperSkill's substrate or its prune-as-delete — a standing
   constraint, not a list item.

---

## 6. What the panel did not settle

- **Whether the guard should evaluate architectural memories at all.** Both
  seats exposed the gap; neither ruled on the intended design.
- **Whether the outcome-weighted quality idea survives** once the signal is
  re-derived from real events. Argon rejected the premise, not the idea.
- **The correct distance band for dedup.** The spec deliberately defers this
  to a later phase with observation data; neither seat invented a band, and
  neither should.
- **The `0.85` config field's intended semantics.** K3 proposed a cosine
  reading; the existing spec forbids that reading. The field's intent needs an
  owner's decision, not a code review's.
- **Any HyperSkill adoption decision.** Both seats endorse the author's
  verdict — complementary, borrow conventions and one signal, do not
  transplant — and both stop there.

---

## 7. Source anchors

**Argon.** `ses_ea449a3acffeIOzp0ErjL5eug9`.
**K3.** `ses_ea449a3aaffeg5aVrpHhjitqL3`.

**NeuroStrata, verified at HEAD `230c5ff`:**
`server.rs:1152-1169` (dedup path, inverted filter, dead tool name),
`server.rs:1423-1476` / `:1461-1470` (snapshot, `access_count` sort),
`server.rs:1950` (`fetch`), `:1978` (`truncate(5)`), `:1982-1991` (`access_count`
increment), `:2236-2340` (evidence renderer and byte budgets);
`store/ladybug.rs:1011-1013` (vector query, `array_distance` ASC),
`:1036-1062` (primary loop, `primary_ids`), `:1051-1062` (`boosted_distance`,
the `0.5 * distance` cap), `:1085-1116` (expansion), `:1094`
(`supported_labels`), `:1239-1258` (family admission, UUID ordering),
`:1277` (`score: 10.0`), `:1284-1287` (sort, `limit * 2`);
`guard/semantic.rs:43`, `:68-89` (`hit_count`, `filter_map`);
`guard/validator.rs:70`, `:74`, `:79` (`rule_ids`, `deprecated`, `matches_trigger`);
`guard/models.rs:49`; `config.rs:20-21`, `:29`;
`docs/superpowers/specs/2026-10-18-local-self-improvement-design.md:390-404`
(Phase 1 — the unimplemented spec);
`README.md:17`, `:44`; `docs/COGNITIVE_ARCHITECTURE.md:27-29`;
`docs/architecture/MEMORY_AGENTS_VERDICT.md:25`, `:36`, `:40`, `:71`;
`CHANGELOG.md:173`;
`src/schemas/memory-vocabulary.v3.json`.

**Commits.** `6e5ffe8` (2026-09-19, evidence-carrying expansion, per-family
admission), `61451bd` (2026-10-08, `truncate(5)` + `fetch` 5→15),
`af134ca` (2026-10-08, dedup filter), `09d569b` (2026-05-25, CHANGELOG FTS
claim), `ce3004e` (2026-04-27, Ladybug migration; pre-migration Kuzu store
confirms distance-based score and the pre-existing `10.0` sentinel).

**HyperSkill, at `a34978a`:** `hyperskill/hypergraph_core.py:325-346`
(utility credit), `:437-441` (silent drop), `:494`, `:498-501` (docstring),
`:532-536` (episode node + membership edges), `:568`, `:602-617`, `:667`, `:689`
(κ sort key), `:698`, `:724`, `:746` (prune = delete), `:776`, `:800`, `:828`,
`:295`, `:317`; `hyperskill/prompts.py:247-253`; `hyperskill/provider.py:416-428`,
`:449`, `:546`, `:557`, `:598`; `hyperskill/config.py`. Paper: arXiv:2608.16114
§3.1–3.5, Table 1, Fig. 4a–c, §4.2 ❸, Limitations.

**Package.** Part A `2026-10-20-hyperskill-panel-brief.md`; Part B
`2026-10-20-hyperskill-review-findings.md`; working analysis
`2026-10-20-external-review-hyperskill.md`; gap brief
`2026-10-20-gap-proactive-recall-and-access-topology.md`; prior verdict
`2026-10-18-laya-openai-verdict.md`.
