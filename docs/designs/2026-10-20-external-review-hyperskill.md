# External review: HyperSkill (rux001/HyperSkill) vs. the NeuroStrata graph

> **Reader's note.** This is the *working analysis* — the author's full
> reasoning, kept for provenance. The **panel-facing package** is two files,
> deliberately split so the panel's own judgment is not anchored by the
> author's conclusions:
> - `2026-10-20-hyperskill-panel-brief.md` — **Part A**: the subject and a
>   factual census of NeuroStrata, with no evaluation. Read first.
> - `2026-10-20-hyperskill-review-findings.md` — **Part B**: the author's
>   summation and the **bug register**. Read second.
>
> Where this document and the findings file differ, the findings file is
> current. This document is a superset of the working notes; the *bug
> register* and the *graph* conclusions were sharpened after it was written.
>
> - `2026-10-20-argon-k3-panel-verdicts.md` — **Part C**: the panel's verdicts
>   (Argon + K3), the **six verified corrections** to Part B, six new
>   NeuroStrata defects, five new HyperSkill paper-vs-code defects, and the
>   adjudicated fix plan. Read after Part B.

**Status:** proposal — capture only. No implementation, no adoption decision, no
behavior change. Queued for Frontier-model evaluation.

**Subject:** HyperSkill — *Self-Evolving LLM Agents via Hypergraph-Structured Skill
Memory* (Xu, Yang, Huang; arXiv:2608.16114, 17 Aug 2026; Apache-2.0)
**Read:** source at commit `a34978a` (cloned and read directly), plus the paper.
**Reviewed:** 2026-10-20

---

## 1. What HyperSkill is, from its source

Memory is described by the paper as a hypergraph `G = (V, E)` over two node
types — **subtask** steps and reusable **skills** — where each hyperedge binds
the subtasks and skills of *one trajectory* together with a distilled lesson and
a utility score.

**In the implementation, the "hyperedge" is a node.** Verified in source: it is
a `Memory`/`networkx` node carrying `role="hyperedge"`, joined to its members
by ordinary binary edges carrying `role="membership"`
(`hyperskill/hypergraph_core.py:532-536`; members are read back through
`G.neighbors(he_id)` filtered on that role, `:551-557`). `networkx` has no
hypergraph primitive, so the structure is a **bipartite two-level graph**; the
n-ary language is a presentation of that design. This matters for the
assessment — see §3, Q2.

Three mechanisms, read from `hyperskill/hypergraph_core.py` and
`hyperskill/provider.py`:

**M1 — Dual-path retrieval** (`hypergraph_core.py:568` `retrieve`). A new task is
decomposed into subtasks by an LLM call (`provider.py:598`
`_generate_initial_plan`). Two candidate sets are then formed and unioned:
- *subtask path* — the plan is embedded and matched against stored **subtask**
  nodes; each match expands to its incident episode nodes, surfacing trajectories
  that share **procedural structure** even when the task wording differs
  (`hypergraph_core.py:602-617`).
- *trajectory path* — the task description is matched directly against **episode**
  embeddings, where an episode is embedded as `task_query ‖ fragment`
  (`hypergraph_core.py:598-600`, `add_hyperedge` at `:494`).

**M2 — Co-occurrence ranking** (`hypergraph_core.py:667` `_rank_skills_mistakes`).
Candidate skills are ranked by `κ(v) = |{e ∈ E* : v ∈ e}|` — how many *retrieved
trajectories* contain that skill — with embedding similarity only as tie-break
(`_rank` is `(c, sim)`, `:689`). The paper's claim is that this is a structural
signal semantic similarity cannot provide: a skill that has been applied
repeatedly in task-relevant contexts outranks one that merely *looks* relevant.

**M3 — Structure-informed maintenance** (`hypergraph_core.py:698`
`maintain_graph`), every `⌈0.1·N⌉` episodes:
- *quality-driven pruning* (`:724`) — removes skill/mistake nodes with
  `retrieval_count ≥ 3` and utility `< 0.2`.
- *structure-informed merging* (`:828`) — builds a quality-weighted co-occurrence
  matrix `W_ij = min(γ_i, γ_j)·Σ_e 1/|V_e|` (`:776`), runs L-step symmetric
  propagation with restart (`:800`), and merges pairs whose propagated
  similarity ≥ 0.85, reassigning hyperedge memberships.
- Utility `γ = β·(σ/ν) + (1−β)·step_efficiency` (`:295`), β = 0.7, updated
  from episode outcomes (`update_utility`, `:317`).

**M4 — Outcome-conditioned extraction** (`provider.py:274` `take_in_memory`). A
trajectory is distilled by an LLM call into **skills** on success and **mistakes**
on failure (`_extract_with_llm`, `:449`). Failures are not discarded — they
become negative knowledge to avoid.

### Reported results (paper, Table 1)

GAIA +11.51, WebWalkerQA +11.18 over No Memory (GPT-4o); xBench +8.00. Beats
ten baselines. **Ablation (Fig. 4c, Qwen3-30B-A3B)** — the load-bearing numbers:

| Ablation | xBench | GAIA | WQA |
|---|---|---|---|
| w/o hypergraph (flat text-embedding retrieval) | 41.00 | 35.76 | 44.71 |
| w/o subtask retrieval | 43.00 | 32.73 | 47.06 |
| w/o trajectory retrieval | 48.00 | 35.76 | 43.53 |
| Full | 52.00 | 36.97 | 50.59 |

Two results matter more than the headline. First, on **xBench** the
hypergraph structure is the largest single lever (flat retrieval loses 11.00);
the paper's §4.2 finding ❸ is that *naive accumulation can hurt* — several
baselines fall **below** No Memory (Cheatsheet −9.00 on xBench), which the
paper attributes to maintenance, since with `ρ = 0` low-utility entries
accumulate and degrade retrieval.

**But the paper's own prose overstates the ablation, and the panel should know
this.** §4.3 claims the "w/o hypergraph" ablation "yields the lowest results
across all three benchmarks." The table does not support that: on GAIA the
lowest cell is *w/o subtask retrieval* (32.73), not *w/o hypergraph* (35.76);
on WebWalkerQA it is *w/o trajectory retrieval* (43.53), not *w/o hypergraph*
(44.71). Only on xBench is *w/o hypergraph* the lowest. Per-benchmark deltas
from the full model:

| Ablated | xBench | GAIA | WQA |
|---|---|---|---|
| w/o hypergraph | −11.00 | −1.21 | −5.88 |
| w/o subtask retrieval | −9.00 | −4.24 | −3.53 |
| w/o trajectory retrieval | −4.00 | −1.21 | −7.06 |

The honest reading: **the largest lever is benchmark-dependent** — structure on
xBench, the subtask path on GAIA, the trajectory path on WebWalkerQA — and on
GAIA both hypergraph and trajectory ablations cost only 1.21. The "hypergraph
is the win" claim rests mainly on one benchmark.

**Stated limitations** (paper, Limitations): extra LLM calls for decomposition,
extraction, and merges; evaluated only on web navigation and multi-step
reasoning; and effectiveness *depends on a sufficient number of accumulated
trajectories* — limited benefit in low-data regimes.

---

## 2. NeuroStrata's position, for contrast

| Concern | HyperSkill | NeuroStrata (source-verified) |
|---|---|---|
| Store | `networkx` GraphML + numpy index, on disk | LadybugDB vector+graph tables (`src/store/ladybug.rs`) |
| Encoder | `all-MiniLM-L6-v2` (local `sentence-transformers`) | `fastembed` local / Dendritic Bridge remote (`src/embed.rs`) |
| Unit of knowledge | trajectory (an episode *node* with membership edges) | memory row (rule / fact / procedure / task) |
| Relations | bipartite membership edges (episode node → members) | 4 declared: `GOVERNS, CONTAINS, RELATES_TO, EXTRACTED_FROM` (`src/store/ladybug.rs:814-824`) |
| Retrieval | dual-path (subtask + trajectory) + κ co-occurrence | dense vector → **graph expansion computed but discarded before the agent** (`ladybug.rs:1277` sentinel score + `server.rs:1978` truncate-to-5); **no lexical/BM25 track** |
| Lifecycle | prune + merge, automatic | supersede/archive only, **no merge** (`neurostrata_supersede_memory`, `neurostrata_archive_memory`) |
| Outcome signal | episode success/fail → `γ` | **absent** — `access_count` counts retrievals, not correctness |
| Provenance | none | `metadata.source` required on rules; `task_validate` enforces (`src/task/gate.rs:248`) |
| Bi-temporal | none | `valid_from` / `valid_to` throughout |
| Sharing | none | SynapticMesh (`src/synaptic_mesh/`) |
| Scope | agent benchmark tasks (GAIA/xBench/WQA) | software-architecture knowledge for coding agents |

**The most important difference is the outcome signal.** HyperSkill's entire
maintenance mechanism is powered by a ground-truth success/failure label per
episode. NeuroStrata has no equivalent: `access_count` (`src/store/ladybug.rs:1822`)
increments on *retrieval*, so a memory can be retrieved constantly and be wrong
every time. This is precisely the "retrieved often but consistently fails"
case HyperSkill prunes — and NeuroStrata cannot currently detect it.

---

## 3. Verdict: complementary, but at a different altitude

**Not a competitor, and not a drop-in.** HyperSkill is an *experiential* memory
for agents that execute tasks in an environment with a scorer. NeuroStrata is a
*durable architectural-knowledge* store with provenance, bi-temporal semantics,
and cross-instance sharing. Their unit of knowledge is different (trajectory vs.
row), their correctness oracle is different (scorer vs. none), and their
lifecycle differs (prune/merge vs. supersede/archive).

### The graph stays — but it does not currently reach the agent

**Nothing here argues for replacing NeuroStrata's graph, and the source check
rules out the main reason one might have thought so.** HyperSkill's
"hypergraph" is implemented as a **bipartite two-level graph** — an episode
*node* with ordinary binary membership edges (`hypergraph_core.py:532-536`).
NeuroStrata's `Memory` nodes and `RELATES_TO` / `CONTAINS` / `GOVERNS` /
`EXTRACTED_FROM` edges are already that shape, and the 1-hop/2-hop directed
traversal with evidence paths (`src/store/ladybug.rs:1085-1116`) is already
implemented. HyperSkill's structure is a *naming convention over a bipartite
graph* — and the naming is a fair thing to borrow.

**But the graph's results are currently dropped before an agent sees them.**
The expansion runs and its evidence paths are correct, yet the
scoring-and-truncation contract discards them (`src/store/ladybug.rs:1277`
gives expansions a sentinel `score: 10.0`; `src/server.rs:1978` keeps the 5
lowest-scoring rows, which are always primaries). See §5 for the full
finding and its live confirmation. **This is the single highest-value thing
the panel can act on**, and it is smaller and cheaper than any of the seven
ideas below.

What NeuroStrata has that HyperSkill does not: a real graph with four
materialized relation types, provenance on every rule, bi-temporal validity,
tombstones over deletion, and cross-instance sharing. What HyperSkill has that
NeuroStrata lacks: a **co-occurrence signal** computed over its graph, and a
**node convention** (an explicit episode record carrying a lesson). Those are
additions to a graph that already exists — not a substrate change.

**But HyperSkill answers three of the four questions in the companion gap brief
(`2026-10-20-gap-proactive-recall-and-access-topology.md` §4) with mechanisms
NeuroStrata does not have.** The alignment is not accidental — that brief's
shortfalls are exactly the ones a skill memory over a graph was built to solve.

### Q1 (Trigger) — partial answer, and the strongest idea here

HyperSkill's subtask path is a *proactive, structure-based* retrieval trigger:
the incoming task is decomposed, and the decomposition is matched against stored
subtask nodes. That is "surface knowledge when entering a topic," keyed on
*procedural structure* rather than on wording. It directly addresses the
shortfall NeuroStrata cannot fix by tuning: NeuroStrata's `get_snapshot` is
session-start shaped, and a fact that has never been retrieved has
`access_count: 0` and is cut from the top-5 (§3.3 of the brief).

NeuroStrata's analog is weaker but present: the AST ingestion already produces
**symbol** rows and the `GOVERNS` relation already ties a rule to the code it
governs. A decomposition-of-the-current-task matched against *symbols* is the
structural analog of HyperSkill's subtask path. **This is the one idea worth
piloting first** — it needs no new storage primitive, only a second retrieval
path over rows that already exist.

Note that NeuroStrata's *existing* retrieval already reaches the graph: the
GraphRAG expansion (`src/store/ladybug.rs:1085`) pulls directed neighbors of
the vector hits and returns them as first-class results with evidence paths.
So the structural idea is not "add a second path" — a structural path already
exists — but "key that path on the *current task's decomposition* rather than
on the vector hits' neighbors." HyperSkill's subtask path is exactly that
re-keying.

### Q2 (Shape) — the "hypergraph" is a bipartite graph, and NeuroStrata already has one

The brief asked what represents "component X reaches system Y by channel Z under
credential boundary W because reason R." HyperSkill's answer is *not* a record
with fields — it is an **n-ary association**: the nodes are the parts, the
episode is the relation, and it carries the distilled lesson.

**But the paper's hypergraph framing overstates what is actually implemented.**
Verified in source: HyperSkill's "hyperedge" is a **node** with
`role="hyperedge"`, connected to its members by ordinary **binary** edges with
`role="membership"` (`hyperskill/hypergraph_core.py:532-536`; members are read
back via `G.neighbors(he_id)` filtered on that role, `:551-557`). `networkx`
has no hypergraph primitive. So the implementation is a **bipartite
two-level graph**: an episode node fanning out to member nodes. The paper's
n-ary language (§1, §3.2) is a *presentation* of that two-level node design,
not a different data model.

**That is the same structure NeuroStrata already has.** `Memory` nodes with
`RELATES_TO` / `CONTAINS` / `GOVERNS` / `EXTRACTED_FROM` edges
(`src/store/ladybug.rs:814-824` — every one is `FROM Memory TO Memory`).
HyperSkill's episode node is NeuroStrata's `task` row; its membership edge is
`RELATES_TO` or `EXTRACTED_FROM`. The traversal is already implemented too:
search expands vector hits one and two hops with directed evidence paths
(`src/store/ladybug.rs:1085-1116`) — which is exactly HyperSkill's "expand
matched subtask nodes to their incident hyperedges"
(`hyperskill/hypergraph_core.py:614-617`).

**So this is not a case for replacing NeuroStrata's graph.** It is a case for
borrowing two *conventions* from a different naming of the same structure:

1. an explicit **episode-level node** that groups its members and carries the
   distilled lesson — NeuroStrata's `task` row is the existing approximation,
   but a task is a *work record*, not an *episode record*; and
2. **co-occurrence ranking over that grouping** (κ, below) — a signal the
   current ranking has no way to compute.

The panel should read the paper's §1 hypergraph argument as an argument for a
**node convention**, and should evaluate it against what NeuroStrata's graph
already expresses — not as a case for a new substrate.

### Q3 (Enforcement) — mostly not applicable, one useful piece

HyperSkill has no enforcement of claims; it has *outcome feedback*. So it does
not answer the brief's Q3 directly. What it does supply is the **shape of the
answer**: a memory carries a quality score derived from observed outcomes, and
low quality is acted on.

NeuroStrata has a promising but unused version of this: the guard already
persists `verdict` and `rule_ids_triggered` per validation
(`src/events/subscribers/guard_event_log.rs:140-158`), and behavioral rules
already carry `hit_count` (`src/guard/semantic.rs:43`). That is an outcome
signal — it is simply not connected to memory quality. HyperSkill's `γ` is a
concrete formula for *how* to connect it, including the non-obvious detail that
it blends correctness with **step efficiency** and that a node must be
retrieved at least `N_min` times before it is eligible for pruning
(untested nodes get a neutral 0.5, `:303-305`).

**This is the highest-value transfer in the whole assessment**, because it
solves a NeuroStrata weakness with a formula rather than a subsystem.

### Q4 (Linkage) — co-occurrence is the answer; maintenance is the hazard

HyperSkill's κ co-occurrence ranking is a *structural relevance signal* — "how
often does this memory co-occur with the memories you just retrieved." It is
complementary to NeuroStrata's vector-plus-GraphRAG retrieval, and cheap to
compute over rows that already carry `RELATES_TO`.

The hazard is **M3's pruning**, and it deserves the panel's scepticism.
NeuroStrata's core promises — provenance, bi-temporal history, tombstones over
deletion, mesh sharing — all argue *against* automatic removal. HyperSkill
deletes nodes outright (`self.G.remove_node(nid)`, `hypergraph_core.py:746`).
NeuroStrata's answer to "this memory is bad" is `supersede` (keep the wording
as history) or `archive` (tombstone, keep the row). Transplanting prune-as-delete
would violate the tombstone-over-delete invariant that the README and
`neurostrata-mcp` doc both state explicitly. **Merging** (M3's second half) is
the safer half to borrow, because `supersede_memory` already exists as the
consolidation primitive.

### M4 (Failure as knowledge) — a gap NeuroStrata has not named

HyperSkill extracts *mistakes* from failed trajectories and stores them as
first-class nodes to avoid. NeuroStrata has no vocabulary for negative
exemplars: a `rule` states what must hold, a `fact` states what is true. There
is no type for "this approach was tried here and it went wrong." The
companion brief did not name this gap at all — it is an addition to the brief,
not a restatement of it.

---

## 4. Candidate ideas for the panel, ranked

Each is stated as a *question the panel must answer*, not a proposal to build.
None is authorized.

**Idea 1 — Outcome-weighted memory quality (highest value, lowest cost).**
NeuroStrata has outcome data it does not use: guard verdicts with
`rule_ids_triggered`, and the `hit_count` on behavioral rules. HyperSkill's
`γ = β·success_rate + (1−β)·step_efficiency` is a ready formula. *Questions:*
what is the NeuroStrata analog of "success" for an architectural rule — a guard
verdict, a task completing without a supersede, an owner correction? Does a
quality score belong on the row, or only in a derived index? **Does a rule
whose quality is low get archived, or merely ranked lower?** The last question
is where the tombstone invariant and HyperSkill's prune-as-delete collide, and
it is the one the panel must resolve before anything is built.

**Idea 2 — Re-key the existing graph path on the current task's decomposition.**
NeuroStrata has symbol rows and `GOVERNS` edges, and search already expands
vector hits through the graph (1-hop and 2-hop `GOVERNS`, with evidence paths).
HyperSkill shows that matching a *decomposition* of the current task against
stored nodes surfaces knowledge that task-level wording misses. *Questions:*
what is the NeuroStrata decomposition primitive — the AST plan, the task's
subtasks, the `domain` tag? And is the change "expand from the *decomposition's*
neighbors" rather than "expand from the *query's* neighbors"? Note the paper's
own sensitivity result: **performance is best at `k = 2` and degrades as k
grows** — a compact surface. NeuroStrata's expansion is already capped per
family (`src/store/ladybug.rs:1239-1258`), so a smaller, better-chosen seed set
may matter more than more of them.

**Idea 3 — Co-occurrence as a ranking signal over `RELATES_TO`.**
κ(v) = number of retrieved rows that co-occur with v. Complements vector
similarity. *Questions:* is this a ranking term (like NeuroStrata's existing
`0.05·ln(1+n)` usage boost and `0.05` source gain) or a separate retrieval
path? What is the cost at NeuroStrata's scale, and does the graph expansion in
`search` already compute enough to support it? Note the ranking-constraint
rule already in NeuroStrata's memory: a boost must be admitted *inside* the
existing relevance cap (`src/store/ladybug.rs`, `boosted_distance = distance -
(usage_gain + source_gain).min(0.5 * distance)`), so any new term inherits that
cap and stays a tie-breaker rather than an override.

**Idea 4 — An explicit episode-level node, reusing the existing graph.**
HyperSkill's "hyperedge" is a *node* with membership edges — a bipartite
two-level graph, which NeuroStrata's `Memory` + `RELATES_TO`/`CONTAINS`/
`GOVERNS`/`EXTRACTED_FROM` structure already is. So this idea is **not** "add
a new edge type" (and not "adopt a hypergraph"); it is: *is there value in an
explicit episode record that groups its members and carries the distilled
lesson?* NeuroStrata's `task` row is the existing approximation, but a task
is a work record — status, assignee, history — not an episode record holding
"these members co-occurred here, and here is the lesson." *Questions:* is a
`task` row the right container, a new memory type the right container, or
neither? If a new type, does it need a new relation, or can `RELATES_TO` carry
membership? Remember `extensions.custom_relations`: custom relations are
**not materialized**, so a new relation type is a vocabulary change, not a
configuration — but a new *node* type with existing relations is not.

**Idea 5 — Negative exemplars as a first-class type.** Failed trajectories
become mistakes to avoid. NeuroStrata has no type for "tried, went wrong."
*Questions:* is this a new `memory_type`, a `fact` with a polarity field, or a
`rule` with a negation? A polarity field on an existing type would be the
cheapest fit, and would not require a vocabulary bump. Does it belong in the
`NeuroStrata` namespace at all, or only in consumer-project namespaces?

**Idea 6 — Consolidation by merging (not deletion).** HyperSkill's
quality-weighted propagation identifies redundant nodes worth merging;
NeuroStrata's `supersede_memory` is the existing consolidation primitive, and
it preserves history by design. *Questions:* can `supersede` express a merge of
two rows into a third without losing either original? What should the merged
row's `metadata.source` be?

**Idea 7 — Anti-pattern: do not transplant the substrate.** HyperSkill's
`networkx` + numpy index, its local MiniLM encoder, and its prune-as-delete
lifecycle all conflict with NeuroStrata's design. It is worth *reading* for the
mechanisms, not *adopting* as a dependency or a storage layer. The panel
should treat "vendor HyperSkill" as off the table unless it can show a reason
NeuroStrata's LadybugDB substrate cannot express the mechanism.

**This is a statement about the substrate, not about the graph.** NeuroStrata's
graph — `Memory` nodes, four materialized relation types, directed 1-hop/2-hop
traversal with evidence paths — is the right structure and stays. What must
not be transplanted is the *storage engine*, the *encoder*, and the *delete on
prune* lifecycle. Every candidate idea above is implementable on the existing
graph; none of them requires a new substrate.

---

## 5. Evidence quality and what the panel should not over-read

- **The ablation is small.** Four configurations, one backbone (Qwen3-30B-A3B)
  for the ablation table, 100–170 tasks per benchmark. The per-mechanism
  deltas are within a few points on 100-ish tasks, and on GAIA two of the three
  ablations cost only **1.21**. The headline numbers are real; the per-mechanism
  attribution is not sharp.
- **The baselines are all agent-memory systems, not code-architecture stores.**
  Nothing in the evaluation shows these mechanisms transfer to a store whose
  contents are architectural *rules* with provenance and a correctness notion
  that is not a benchmark scorer. This is the load-bearing caveat on the whole
  assessment.
- **Domain mismatch.** HyperSkill's tasks are web navigation and tool use
  (GAIA, xBench, WebWalkerQA). NeuroStrata's are software-architecture
  knowledge. The *mechanisms* may transfer; the *evidence* does not speak to
  this domain at all.
- **Cost.** Three extra LLM calls per episode (decompose, extract, merge).
  NeuroStrata's `get_snapshot` and `search_memory` are metadata-and-vector
  reads with no LLM call. A decomposition-based trigger (Idea 2) would introduce
  a per-task LLM call into a path that is currently free.
- **Low-data regime.** The paper says so itself: effectiveness depends on
  accumulated trajectories. A fresh namespace has none.
- **Reproducibility.** Verified at commit `a34978a`: the repository is **2
  commits** (`ae03fea` initial, `a34978a` "Clean up README"), with **no tests**,
  **no CI** (no `.github/`), **no releases and no tags**. `requirements.txt`
  uses `>=` floors with no lockfile, so the exact dependency set is not pinned.
  The runners need three external datasets plus API keys (`.env.example`
  requires Azure OpenAI or OpenAI, a separate judge model, and a search
  backend). The paper's numbers are therefore **not independently reproducible**
  from this repository. Read the source, not the table.

### A NeuroStrata-side discrepancy this review surfaced (not HyperSkill's fault)

While contrasting the two systems, one thing in *this* repo did not survive
checking, and the panel should know because it changes what "second retrieval
path" means here.

NeuroStrata's own documentation asserts a **hybrid semantic + BM25** retrieval
path fused by **Reciprocal Rank Fusion**, implemented with **Tantivy**:

- `README.md:17` — "an embedded LadybugDB vector store and full-text search
  (BM25 via Tantivy)";
- `docs/COGNITIVE_ARCHITECTURE.md` §6 — "utilizes `tantivy` to combine dense
  vector semantic search with exact Full-Text Search (BM25 keyword matching) …
  merged via Reciprocal Rank Fusion (RRF)";
- `CHANGELOG.md:173` — "Tantivy FTS Hybrid Search: Integrated hybrid exact-keyword
  and vector search using Reciprocal Rank Fusion (RRF)";
- `docs/architecture/MEMORY_AGENTS_VERDICT.md:36` — "`neurostrata_search_memory`
  — hybrid dense-vector + BM25 (tantivy) merged via Reciprocal Rank Fusion";
- `src/store/ladybug.rs:1085` even names the step "Hybrid GraphRAG".

**None of it is in the source.** Verified 2026-10-20: `Cargo.toml` has **no
`tantivy` dependency** (nor any FTS crate); `src/store/ladybug.rs` contains no
BM25, FTS, or RRF code — search is `array_distance` vector ranking plus directed
one-hop/two-hop graph expansion; and no memory in the `NeuroStrata` namespace
records such an implementation. The likely history is the LadybugDB/Kuzu
migration (`CHANGELOG.md` records the Tantivy FTS feature, and the store is now
`lbug 0.20.4`), with the docs never updated.

Consequence for the panel — stated carefully, because the shorthand "one
retrieval track" is easy to misread, and the investigation turned up something
sharper than a missing feature.

### The graph is built, queried — and then thrown away before the agent sees it

This is the most consequential finding in the review, so it is stated
precisely. It was **not** visible from the source alone; it took live MCP
calls to see.

**The graph is real and populated.** The `NeuroStrata` namespace export
contains **1,277 materialized edges** — 885 `CONTAINS`, 250 `EXTRACTED_FROM`,
96 `RELATES_TO`, 46 `GOVERNS` — of which **325 join two non-structural
rule/fact/procedure rows**. So the graph is not a stub.

**The expansion is computed correctly.** The store's `search` does the one-hop
neighbor and two-hop `GOVERNS` expansion and attaches evidence paths
(`src/store/ladybug.rs:1085-1116`, `:1239-1281`). The store's own tests prove
it: `one_hop_evidence_preserves_both_directed_orientations` (`:3932`) and
`governs_two_hop_evidence_contains_primary_file_and_rule` (`:3993`).

**But the expansion never reaches the agent.** Two truncations drop it:

| Stage | Line | Effect |
|---|---|---|
| expansions get a **hard-coded** score | `src/store/ladybug.rs:1277` | `score: 10.0` |
| primaries get their real distance | `src/store/ladybug.rs:1062` | `score: boosted_distance` (≈0–1) |
| store sorts by score **ASC**, truncates to `limit*2` | `src/store/ladybug.rs:1284-1287` | expansions sort *last* |
| server truncates to **5** | `src/server.rs:1978` | the 5 survivors are always 5 **primaries** |

Because the sort is ascending and every primary's distance is far below the
expansions' hard-coded `10.0`, the five rows that survive are always five
direct vector hits. The expanded rows — and their evidence paths — are
computed, then discarded.

**Confirmed empirically.** Live `neurostrata_search_memory` calls across a
semantic query, an exact-token query, a nonsense query, and a query aimed at a
rule reachable *only* by a `RELATES_TO` edge: every one returned exactly five
results and **zero** `Path [one_hop]` / `Why: This memory is one graph hop
from a direct match.` lines. The rule behind the `RELATES_TO` edge never
appeared. The evidence-rendering code (`src/server.rs:2262-2340`) and its byte
budgets (`MAX_EVIDENCE_BYTES_PER_RESULT` 512, `MAX_EVIDENCE_BYTES_PER_RESPONSE`
2048) are therefore **dead in practice** for `search_memory`.

### So what is the answer to "is the graph given to agents?"

**No.** Not for `search_memory`. The graph is built, populated, queried, and
its results are then dropped by the scoring-and-truncation contract. The
"Hybrid GraphRAG" step at `src/store/ladybug.rs:1085` — a name the code
itself uses — does not surface to the agent.

Two independent gaps, then, and they should not be conflated:

| | documented | actual |
|---|---|---|
| dense vector | yes | yes |
| graph expansion | (unstated; named in code) | **computed but discarded before the agent** |
| BM25 / exact keyword | yes | **absent entirely** |

**This is a retrieval-path defect, not a graph-model defect.** The graph model
is sound and is the right structure. What is broken is the contract between
the store and the surface: a hard-coded sentinel score of `10.0` is a
plausible way to sort expansions last, but it makes them unreachable whenever
there are ≥5 primaries — which is essentially always.

**Practical consequence:** today, `search_memory` returns pure top-5 vector
similarity. A memory whose *text* is an exact token — a function name, an env
var, a config key, an error string — is found only if its *embedding* happens
to land near the query; semantic similarity is a blunt instrument for "the
user typed this exact identifier." And a rule that matters only because of its
*relationship* to a rule you already found is invisible.

This raises the value of Idea 2 (re-keying the graph path) and makes the
BM25/RRF question live rather than hypothetical: if the documented hybrid
retrieval is genuinely wanted, it is *absent work*; the co-occurrence signal
(Idea 3) would then be a third candidate track rather than a fourth.
Resolving whether the docs or the code is authoritative is a separate,
smaller question — but it should not be answered by assuming the docs are
right.

**For the panel:** the first question is not "should we add BM25" or "should
we add co-occurrence." It is **"why does the graph expansion never reach an
agent?"** — because fixing that is a smaller, cheaper change than any of the
seven ideas, and it would make the graph NeuroStrata already pays for
actually pay off. The remaining ideas are worth evaluating *after* that is
answered, not instead of it.

---

## 6. Relationship to the companion brief

This document is an **addendum** to
`2026-10-20-gap-proactive-recall-and-access-topology.md`, not a replacement.
That brief's four questions (Trigger / Shape / Enforcement / Linkage) are
unchanged in substance; a **Q5** (negative exemplars) has been added there
because this review found a gap the brief did not name. HyperSkill contributes:

- **Q1 (Trigger)** — a concrete mechanism (structural/subtask-path retrieval)
  and a caution (an LLM call per task; small-k beats large-k).
- **Q2 (Shape)** — the episode-as-node convention (its "hyperedge" is a node
  with membership edges — the same bipartite shape NeuroStrata's graph
  already is), and the constraint that custom relations are not materialized.
- **Q3 (Enforcement)** — little direct help, but the *shape* of a quality
  signal, and the one unused outcome source NeuroStrata already owns (guard
  verdicts).
- **Q4 (Linkage)** — co-occurrence ranking as a candidate structural term, and
  the prune-vs-tombstone conflict the panel must resolve.
- **Q5 (Negative exemplars, new)** — mistakes as first-class nodes to avoid;
  NeuroStrata has no vocabulary for a failed attempt.

**The scoping rule from the companion brief still governs this document.** The
shortfall here is a *NeuroStrata mechanism* shortfall. No consumer project's
system configuration, vendor, or product appears in this assessment, and none
should enter the `NeuroStrata` namespace from it. What is recorded in memory
from this document is the *mechanism* and the *question*, not any consumer fact.

---

## 7. Source anchors

**HyperSkill** (cloned to `/tmp/opencode/HyperSkill`, commit `a34978a`):
- `hyperskill/hypergraph_core.py:568` `retrieve` (dual path);
  `:667` `_rank_skills_mistakes` (κ co-occurrence, `_rank = (c, sim)` at `:689`);
  `:698` `maintain_graph`; `:724` `_prune_low_utility` (delete at `:746`);
  `:776` `_build_co_occurrence_matrix`; `:800` `_l_step_propagation`;
  `:828` `_find_merge_candidates`; `:295` `_compute_utility_score`;
  `:317` `update_utility`; `:494` `add_hyperedge`; `:412` `_add_abstract_node`.
- `hyperskill/provider.py:135` `provide_memory`; `:274` `take_in_memory`;
  `:449` `_extract_with_llm`; `:478` `_parse_extraction`; `:598`
  `_generate_initial_plan`; `:546` `_llm_merge_nodes`.
- `hyperskill/config.py` — `k=5`, `beta=0.7`, `maintenance_ratio=0.1`,
  `prune_min_retrievals=3`, `prune_threshold=0.2`, `merge_threshold=0.85`,
  `merge_propagation_depth=2`, `skill_semantic_dedup_threshold=0.90`.
- Paper: arXiv:2608.16114 §3.1–3.5 (method), Table 1 (main results),
  Fig. 4c (ablation), §4.2 finding ❸ (accumulation can hurt), Limitations.

**NeuroStrata** (this repo, verified 2026-10-20):
- `access_count` increments on *retrieval*, not correctness —
  `src/store/ladybug.rs:1822`; usage boost `0.05·ln(1+n)` at `:1053`.
- `get_snapshot` sorts by access_count and truncates to 5 —
  `src/server.rs:1461-1476`; new memories start at `access_count: 0`
  (`src/server.rs:1122`).
- Relation set; custom relations **not** materialized; vocabulary version
  policy — `src/schemas/memory-vocabulary.v3.json` (`relations`,
  `extensions.custom_relations`, `extensions.version_policy`).
- Guard outcome data already persisted — `verdict`, `rule_ids_triggered` at
  `src/events/subscribers/guard_event_log.rs:140-158`; `hit_count` at
  `src/guard/semantic.rs:43`, `src/guard/models.rs:49`.
- Vector + directed GraphRAG retrieval, **no** BM25/Tantivy/RRF —
  `src/store/ladybug.rs:995` (vector step), `:1085` (expansion step);
  `Cargo.toml` has no FTS dependency.
- **The graph, and why it stays** — four materialized relation tables, all
  `FROM Memory TO Memory` (`src/store/ladybug.rs:814-824`); directed 1-hop
  neighbor and 2-hop `GOVERNS` expansion with evidence paths
  (`src/store/ladybug.rs:1085-1116`); the only four relations are
  materialized (`extensions.custom_relations`), and endpoint role typing is
  descriptive only (`extensions.role_enforcement`: "no endpoint type
  rejection"), so a new *node* type can participate in existing relations
  without a vocabulary change.
- Documented-but-absent hybrid retrieval — `README.md:17`,
  `docs/COGNITIVE_ARCHITECTURE.md:29`, `CHANGELOG.md:173`,
  `docs/architecture/MEMORY_AGENTS_VERDICT.md:36`.
- No merge/consolidation primitive; `neurostrata_supersede_memory` and
  `neurostrata_archive_memory` are the only consolidation tools, and
  `archive` is a tombstone (row kept) — `src/handlers/archive_memory.rs`.
- Existing dedup path (a separate, LLM-based judgment subsystem, not a
  HyperSkill analog) — `src/judgment/mod.rs:70`.
- Ranking must stay inside the relevance cap —
  `src/store/ladybug.rs:1055-1056` (`capped_gain = (gain + source_gain).min(0.5
  * distance)`; `boosted_distance = distance - capped_gain`).
- `procedure.trigger` is a closed enum; procedures have no runtime auto-fire —
  `src/schemas/memory-vocabulary.v3.json`; `src/tools/procedure.rs:91`.
