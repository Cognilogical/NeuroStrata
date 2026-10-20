# Panel brief A: HyperSkill, read independently

**Purpose:** you are asked to form **your own** view of what HyperSkill offers
NeuroStrata, *before* reading any of the author's conclusions. Those are in a
separate file (`2026-10-20-hyperskill-review-findings.md`). Read them after.

**Subject:** HyperSkill — *Self-Evolving LLM Agents via Hypergraph-Structured
Skill Memory* (Xu, Yang, Huang; arXiv:2608.16114, submitted 17 Aug 2026;
Apache-2.0). Repository: `https://github.com/rux001/HyperSkill`.

**Status:** review input. Nothing here is approved, adopted, or scheduled.

> **Package now complete.** This brief (Part A) was reviewed on 2026-10-20 by
> two independent architect seats — Argon (Gemini 4) and K3 (Kimi K3) — who
> read this file first and formed their own answers before opening Part B.
> Their verdicts, the corrections to Part B, and the adjudicated fix plan are
> in `2026-10-20-argon-k3-panel-verdicts.md` (Part C). Both confirmed all
> three findings in the bug register and both corrected six errors in Part B.

---

## How to use this brief

1. Read §1–§3 (the subject, from its own source) and §4 (what NeuroStrata is
   today, from its own source).
2. Form your own answers to the five questions in §5.
3. **Then** read the author's findings file and compare. Where you disagree,
   say so and show the source that settles it.

§4 is deliberately descriptive and contains no evaluation. If you find
yourself concluding something about NeuroStrata from §4 alone, re-read §4 —
it is a census, not a verdict.

---

## 1. The subject, from its source

Cloned and read at commit `a34978a` (repo HEAD; the repository has 2 commits
total — `ae03fea` initial, `a34978a` "Clean up README"). No tests, no CI, no
releases, no tags. `requirements.txt` uses `>=` floors with no lockfile.

### 1.1 Data model

Memory is a graph over two node types — **subtask** steps and reusable
**skills** — plus a third node kind: an **episode** node.

The episode node is how a trajectory is represented. In source
(`hyperskill/hypergraph_core.py`):

```python
# add_hyperedge(), :494
attrs = {
    "type": edge_type,
    "role": "hyperedge",
    "fragment": fragment,          # the distilled lesson
    "task_query": task_query,
    "member_count": len(node_ids),
    "retrieval_count": 0, "success_count": 0, "avg_steps": 0.0, "utility": 0.5,
}
self.G.add_node(hid, **attrs)                      # :532
for nid in node_ids:
    if self.G.has_node(nid):
        self.G.add_edge(hid, nid, role="membership")   # :536
```

So: an episode is a **node** with `role="hyperedge"`, joined to its members by
ordinary **binary** edges with `role="membership"`. Membership is read back
through `G.neighbors(he_id)` filtered on that role (`:551-557`). The backing
library is `networkx`, which has no hypergraph primitive.

The paper (§3.2) describes the same structure as a hypergraph: nodes
`V = V_u ∪ V_s` (subtasks, skills) and hyperedges `e_j = (V_j, d_j, ℓ_j, γ_j)`
where `d_j` is the task description, `ℓ_j` the distilled lesson, `γ_j` the
utility score. An episode's embedding is `φ(d_j ‖ ℓ_j)` — task description
concatenated with the lesson (`:498-500`, `:538-546`).

### 1.2 M1 — Dual-path retrieval (`hypergraph_core.py:568` `retrieve`)

A new task is decomposed into subtasks by an LLM call
(`provider.py:598` `_generate_initial_plan`). Two candidate sets are formed
and unioned:

- **subtask path** — the plan text is embedded and matched against stored
  **subtask** nodes; each match expands to its incident episode nodes,
  surfacing trajectories that share procedural structure even when the task
  wording differs (`:602-617`).
- **trajectory path** — the task description is matched directly against
  **episode** embeddings (`:598-600`).

`E* = E_sub ∪ E_traj`. The distillation lessons from `E*` are handed to the
agent as trajectory-level context.

### 1.3 M2 — Co-occurrence ranking (`hypergraph_core.py:667`)

Candidate skills are the union of members of `E*`. They are ranked by

```
κ(v) = |{ e ∈ E* : v ∈ e }|      # how many retrieved trajectories contain v
```

with embedding similarity only as a tie-break — the sort key is literally
`(c, sim)` (`:689`). The paper's claim (§3.4) is that this is a structural
signal semantic similarity cannot provide: a skill that has been applied
repeatedly in task-relevant contexts outranks one that merely looks relevant.

### 1.4 M3 — Structure-informed maintenance (`hypergraph_core.py:698`)

Runs every `⌈0.1·N⌉` episodes.

- **Quality-driven pruning** (`:724`) — removes skill/mistake nodes with
  `retrieval_count ≥ 3` **and** utility `< 0.2`. Removal is
  `self.G.remove_node(nid)` (`:746`) — the node is **deleted**, not tombstoned.
  Subtask nodes are never pruned this way.
- **Structure-informed merging** (`:828`) — builds a quality-weighted
  co-occurrence matrix `W_ij = min(γ_i, γ_j) · Σ_e 1/|V_e|` (`:776`), runs
  L-step symmetric propagation with restart (`:800`), and merges pairs whose
  propagated similarity ≥ 0.85, reassigning episode memberships. The merged
  content is proposed by an LLM (`provider.py:546` `_llm_merge_nodes`).
- **Utility** `γ = β·(σ/ν) + (1−β)·step_efficiency` (`:295`), β = 0.7.
  Untested nodes (ν = 0) get a neutral 0.5 (`:303-305`). `σ` = success count,
  `ν` = retrieval count, step efficiency normalized against the global
  min/max observed step counts. `update_utility` (`:317`) recomputes γ for
  every node and episode that participated in a retrieval.

### 1.5 M4 — Outcome-conditioned extraction (`provider.py:274` `take_in_memory`)

After each episode, an LLM call distills the trajectory into **skills** on
success or **mistakes** on failure (`_extract_with_llm`, `:449`; prompts at
`prompts.py`). Failed trajectories therefore become negative knowledge to
avoid, not discarded. The extracted lesson becomes the episode's `fragment`.
Insertion-time semantic dedup: a new skill whose embedding exceeds 0.90
similarity to an existing one is merged into it rather than added
(`config.py` `skill_semantic_dedup_threshold`).

### 1.6 Configuration (`config.py`)

`k=5` (retrieval budget), `beta=0.7`, `maintenance_ratio=0.1`,
`prune_min_retrievals=3`, `prune_threshold=0.2`, `merge_threshold=0.85`,
`merge_propagation_depth=2`, `merge_alpha=0.5`,
`skill_semantic_dedup_threshold=0.90`, encoder
`all-MiniLM-L6-v2` (local `sentence-transformers`).

### 1.7 Reported results (paper)

Table 1, success rate vs. No Memory: GAIA +11.51, WebWalkerQA +11.18, xBench
+8.00 (GPT-4o). Beats ten baselines (Voyager, DILU, ExpeL, Generative, AWM,
ReasoningBank, MemP, Cheatsheet, MemEvolve, PlugMem).

Figure 4c ablation (Qwen3-30B-A3B), success rate:

| Ablation | xBench | GAIA | WQA |
|---|---|---|---|
| w/o hypergraph (flat text-embedding retrieval) | 41.00 | 35.76 | 44.71 |
| w/o subtask retrieval | 43.00 | 32.73 | 47.06 |
| w/o trajectory retrieval | 48.00 | 35.76 | 43.53 |
| Full model | 52.00 | 36.97 | 50.59 |

Figure 4a–b sensitivity: performance is **best at k = 2** and declines as k
grows; the maintenance ratio peaks at ρ = 0.1, with ρ = 0 (no pruning)
degrading retrieval quality and ρ = 0.4 removing still-useful skills.

Paper §4.2 finding ❸: several baselines fall **below** No Memory
(Cheatsheet −9.00 on xBench, Voyager −7.00), attributed to memory accumulating
without curation.

Paper, Limitations: extra LLM calls for decomposition, extraction, and merge
decisions; evaluation covers only web navigation and multi-step reasoning;
effectiveness depends on a sufficient number of accumulated trajectories, so
benefit is limited in low-data regimes.

---

## 2. Things to weigh when judging the evidence

- The ablation is 4 configurations on 1 backbone, 100–170 tasks per benchmark.
  The per-mechanism deltas are small on GAIA.
- The baselines are all agent-memory systems on web/tool-use benchmarks.
  None is a code-architecture knowledge store.
- The repository cannot reproduce the numbers: 2 commits, no tests, no CI, no
  pinned dependency set, and the runners need three external datasets plus API
  keys (`.env.example` requires a hosted model, a separate judge model, and a
  search backend).
- The paper's own §3.2/§1 language is "hypergraph"; §1.1 above shows what the
  code actually builds. Both readings are defensible; the panel should decide
  which one it is evaluating.

---

## 3. What HyperSkill does not attempt

No provenance on its nodes, no bi-temporal validity, no tombstone-or-delete
distinction, no cross-instance sharing, no enforcement of claims (it has
outcome *feedback*, not claim checking), no support for
non-experiential/factual knowledge, and no notion of a memory's correctness
independent of a per-episode success label.

---

## 4. NeuroStrata today, from its own source

A factual census of this repo as of 2026-10-20. No evaluation is offered here
or in the census.

### 4.1 Store and vocabulary

- Vector + graph store: LadybugDB (`lbug 0.20.4`), embedded. Node table
  `Memory`; four relation tables, all `FROM Memory TO Memory`:
  `RELATES_TO`, `CONTAINS`, `GOVERNS`, `EXTRACTED_FROM`
  (`src/store/ladybug.rs:814-824`).
- Vocabulary v4 (`src/schemas/memory-vocabulary.v3.json`): types `rule`,
  `fact`, `task`, `directory`, `file`, `markdown`, `symbol`, `procedure`;
  type alias `code_ast → symbol`. `context` is the default `add_memory`
  type. Any non-empty custom type string is accepted unchanged
  (`extensions.custom_memory_types`); custom *relations* are **not**
  materialized (`extensions.custom_relations`). Endpoint role typing is
  descriptive only (`extensions.role_enforcement`).
- Rules require `metadata.source`; `task_validate` fails rules without it
  (`src/task/gate.rs:248`). Rules declare `enforcement`
  (`ENFORCED|PARTIAL|NOT_ENFORCED`) and optionally a `guard` wire id; a rule
  claiming `ENFORCED` must name a real wire (`src/task/gate.rs:259-272`).
- Bi-temporal: `valid_from` / `valid_to` on every row. Retirement is
  `neurostrata_supersede_memory` (keeps the old wording as history) or
  `neurostrata_archive_memory` (tombstone; the row is kept).
- `neurostrata_procedure_perform` stamps a procedure firing and decrements
  its budget. `procedure.trigger` is a closed enum
  (`session-start|before-edit|after-mutation|every-n-sessions:N|every-n-days:N`).
- SynapticMesh shares `rule`/`fact`/`procedure` definitions between
  instances over git.
- The Behavioral Guard (`neurostrata_guard_validate`) validates an
  **action** (`action_type` + `payload`) against behavioral rules, and
  persists per-validation `verdict` + `rule_ids_triggered`
  (`src/events/subscribers/guard_event_log.rs:140-158`).
- The Episodic Buffer writes timestamped entries to
  `.NeuroStrata/sessions/current.md`, with a `### 🔄 Topic Switch [tags]`
  marker on tagged turns (`src/buffer.rs:71`).

### 4.2 Retrieval, as implemented

`neurostrata_search_memory` → `handle_search_memory` (`src/server.rs:1927`):

- The query is embedded; the store ranks by `array_distance` on the
  embedding and returns rows ordered by that distance ascending
  (`src/store/ladybug.rs:1011-1013`).
- Primary hits carry `score = boosted_distance`, where
  `boosted_distance = distance - (usage_gain + source_gain).min(0.5 * distance)`,
  `usage_gain = 0.05·ln(1+access_count)`, and `source_gain = 0.05` when the row
  carries `metadata.source` (`src/store/ladybug.rs:1051-1056`). **Score is a
  distance: lower is better.**
- The store then performs a **graph expansion**: one-hop neighbors and
  two-hop `GOVERNS` results, each with an evidence path
  (`src/store/ladybug.rs:1085-1116`, `:1239-1281`). Expansion rows are
  assigned a fixed `score: 10.0` (`:1277`). The store sorts by score and
  truncates to `limit * 2` (`:1284-1287`).
- `handle_search_memory` filters out `task` rows unless `include_tasks` is
  set, then truncates to **5** (`src/server.rs:1965-1978`) and renders each
  result with an evidence suffix when one fits the byte budgets
  (`MAX_EVIDENCE_BYTES_PER_RESULT` 512, `MAX_EVIDENCE_BYTES_PER_RESPONSE`
  2048; `src/server.rs:2236-2340`).
- `neurostrata_get_snapshot` lists the namespace, hides retired rows, sorts by
  `access_count` descending, and truncates to 5
  (`src/server.rs:1423-1476`).

Other consumers of the same store search: the dedup candidate lookup
(`src/server.rs:1152`), the guard's rule lookup (`src/guard/semantic.rs:64`),
and a task lookup (`src/task/mod.rs:2221`).

### 4.3 A retrieval-path census, stated as observations

Each of these is a reading of the code above. The panel should confirm,
refute, or re-scope them.

**O1.** Primary rows and expansion rows are scored on different scales
(`boosted_distance`, typically well under 1, versus a fixed `10.0`). The
store sorts by score ascending, so expansions sort after all primaries. The
server then keeps the 5 lowest-scoring rows. Where at least 5 primaries
exist, the 5 kept rows are all primaries.

**O2.** A rule reachable only through a `RELATES_TO` edge is not returned by
`search_memory`; nor is the evidence path for a one-hop or two-hop
expansion. The expansion code and its tests exist
(`src/store/ladybug.rs:3932`, `:3993`), as does the evidence renderer and its
byte budgets.

**O3.** The dedup candidate lookup filters candidates with
`c.score >= 0.85` and comments it as "only high-similarity candidates"
(`src/server.rs:1153-1154`). Given that score is a distance, this filter
admits rows whose distance is ≥ 0.85 and admits every expansion row
(`score: 10.0`). The `deduplication.similarity_threshold` config field
(`src/config.rs:20-21`, default 0.85) is not read on this path. The path
runs only when a judgment API key is configured
(`src/judgment/mod.rs`, `judgment_api_key` is the enable signal).

**O4.** `git log` places the graph expansion in commit `6e5ffe8`
(2026-09-19, "feat(search): path-labeled retrieval evidence + memory
vocabulary v1"), which introduced the `score: 10.0` assignment and the
evidence paths. The `results.truncate(5)` in `handle_search_memory` arrived
later, in `61451bd` (2026-10-08, "feat: task subsystem — replace bd/beads
with in-binary LadybugDB tasks"), in the same change that raised the store
fetch from 5 to 15. Before that commit the server passed the store's
`limit * 2` rows straight to the renderer with no truncation of its own.

**O5.** The `tantivy` crate has never appeared in `Cargo.toml` in this
repository's history (`git log -S'tantivy' -- Cargo.toml` is empty). The
claim of hybrid BM25/Tantivy retrieval fused by RRF appears in
`README.md:17`, `docs/COGNITIVE_ARCHITECTURE.md` §6, and
`docs/architecture/MEMORY_AGENTS_VERDICT.md:36`; `CHANGELOG.md:173` records
"Tantivy FTS Hybrid Search" as a shipped feature, added in `09d569b`
(2026-05-25). Every `MATCH` in `src/store/ladybug.rs` is vector-distance,
id-equality, or a graph traversal.

---

## 5. Questions for you

Answer these **before** reading the author's findings.

**Q1.** Which of HyperSkill's mechanisms, if any, would you want NeuroStrata
to have? Name the mechanism and say what problem it solves for you. If none,
say so and say why.

**Q2.** HyperSkill's episode is a node with binary membership edges.
NeuroStrata's graph is `Memory` nodes with four binary relation types, and
NeuroStrata's `task` row is the existing episode-shaped container. Is there
anything in HyperSkill's *shape* that NeuroStrata's graph cannot already
express? If yes, what is it and what would it cost?

**Q3.** HyperSkill's co-occurrence signal (κ) is computed over its
trajectories. Is there a NeuroStrata analogue worth having, and would it be a
ranking term, a retrieval path, or something else?

**Q4.** HyperSkill's utility score needs a per-episode success/failure label.
What is the NeuroStrata analogue of a success label for an architectural
claim? Consider that the guard already persists `verdict` and
`rule_ids_triggered` per validation, and that `access_count` counts
*retrievals*.

**Q5.** HyperSkill prunes by deleting nodes. NeuroStrata's stated model is
provenance-preserving supersede and tombstone-over-delete. How should
NeuroStrata handle a memory that is retrieved often and turns out to be
wrong?

---

## 6. What you are being asked to produce

For each of Q1–Q5: a position, the reasoning, and the source that supports
it. Then, after reading the author's findings: an explicit statement of where
you agree, where you disagree, and where the author missed something or
overreached. **Disagreement is the useful output** — the author has already
had this material and may have anchored on it.

Everything in the findings file is the author's own work product, including
its bug register. Verify it against source rather than taking it.
