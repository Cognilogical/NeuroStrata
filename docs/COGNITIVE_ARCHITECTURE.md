# 🧠 NeuroStrata Cognitive Architecture

NeuroStrata utilizes a sophisticated, neuroscience-inspired memory architecture designed explicitly for autonomous software engineering agents. By moving beyond flat vector blobs, NeuroStrata implements a **Dual-Track Bi-Temporal Graph Memory System** that ensures agents possess stable, relational, and self-optimizing context for any codebase.

Here are the formal cognitive techniques powering the NeuroStrata engine:

### 1. Semantic Graph Edges (Relational Node Traversal)
*Inspired by human associative memory.*
Instead of treating architectural rules as isolated facts, NeuroStrata nodes form a directional knowledge graph. Memories contain a `related_to` schema array. When an agent updates a database rule, it can traverse the semantic edges to instantly identify connected API contracts or frontend component constraints, preventing cascading regressions.

### 2. Immutable Temporal Audit Trail (Bi-Temporal Memory)
*Inspired by human episodic memory.*
Agents never overwrite history. When an architectural decision changes, the system applies a `valid_to` soft-deletion Unix timestamp to the old node and creates a new node with a `valid_from` timestamp. The query engine automatically filters out expired memories whose `valid_to` timestamp is less than or equal to the current system time, keeping the active context window clean while preserving history.

### 3. Neural Gain Mechanism (Access-Based Synaptic Weighting)
*Inspired by the Ebbinghaus Forgetting Curve and synaptic plasticity.*
All vector databases rank results by mathematical distance (e.g., L2 or Cosine). NeuroStrata applies a **Neural Gain Filter** on top of this. Every **direct** hit — a row the vector query matched, not a graph-expansion neighbour — has its `access_count` metadata incremented in the background (`src/server.rs:2042-2054`), and that counter only ever rises (`src/store/ladybug.rs:2370-2386`, `:2160-2187`). During retrieval, the engine computes a final salience score from the raw distance `d`:

```
usage_gain   = 0.05 * ln(1 + access_count)
source_gain  = 0.05 if metadata.source is present, else 0
capped_gain  = min(usage_gain + source_gain, 0.5 * d)
salience     = d - capped_gain
```

(`src/store/ladybug.rs:146-151`; the provenance constant is `SOURCE_RANK_GAIN` at `:1296`, and the provenance test is `crate::traits::has_source` — `src/traits.rs:49`.) The **cap**, not the logarithm, is what keeps the boost from becoming an override: a row's final distance is never below `0.5 * d`, so a boosted row can overtake a row up to twice as far away, but never one less than half as far away. Highly-accessed core principles therefore hold the top without displacing a genuinely closer memory. Unused rules fall in *relative* rank as other rows are retrieved — the counter itself never decays — and expired rules leave recall entirely through the bi-temporal `valid_to` filter (`src/store/ladybug.rs:2267-2272`), not through a decay score.

### 4. Domain-Isolated Knowledge Shelves (Contextual Compartmentalization)
*Inspired by declarative knowledge clustering.*
NeuroStrata carries an explicit declarative `domain` tag on a memory (e.g., `frontend`, `database`, `devops`, `api`) — set from the `domain` argument at write time (`src/server.rs:1281-1296`) and to `code_ast` for ingested symbols (`src/parser/ingest.rs:465`). The tag is *descriptive* metadata: it is surfaced by graph export (`src/store/ladybug.rs:2011`, `:2064`) and it is **not** a filter the write path applies. It is now also an **optional soft filter** on the retrieval query: `neurostrata_search_memory` takes a `domain` — a slug or a list of slugs — and a filter that would empty the candidate set is skipped rather than blinding the search, with the report's first line saying which happened (`src/server.rs:2006-2078`). What shrinks the vector search space *by default* is the **namespace** — the Tri-Strata partitioning of §7 — because a search runs inside exactly one namespace (`src/store/ladybug.rs:87`). Compartmentalization is therefore still carried by the namespace; the `domain` label now narrows within it, when the caller asks for it.

### 5. Pre-computed Cognitive Snapshots (Zero-Shot Context Grounding)
*Inspired by the psychological concept of "Working Memory Priming".*
Instead of forcing agents to blindly search a new repository and waste expensive token processing, NeuroStrata features a `neurostrata_get_snapshot` tool. This instantly returns the top-5 highest-weighted, active cognitive nodes for any project. This "wake-up context" perfectly grounds an agent in a repository's most critical rules the second a session begins.

### 6. Deterministic Retrieval — Boosted Distance + Bounded Graph Evidence
*Inspired by human dual-process theory (gist vs. verbatim recall) — of which only the "gist" track is built.*

Retrieval is one deterministic pipeline. No LLM, no rank fusion, no keyword index:

1. **Row-class filter, then vector distance.** The store filters row classes *before* the candidate budget is spent — structural rows (`directory`, `file`, `markdown`, `freshness_flag`, `guard_event`) are always excluded (`:1264-1270`, `:76`), `task` rows are excluded unless the caller passes `include_tasks` (`:77-79`), tombstoned rows are excluded unless the caller passes `include_archived` (`:121`), and mesh-conflicting rows are excluded outright (`:124`; the same filters run over expansion candidates at `:282-284` and `:338-340`). What remains is scanned with `array_distance` and ordered by `dist ASC, id ASC` (`:87`). There is **no lexical/BM25 track, no full-text index, and no Reciprocal Rank Fusion** anywhere in the codebase — `tantivy` was never a dependency of this crate (`git log -S'tantivy' -- Cargo.toml` is empty, and the current `Cargo.toml` contains no FTS crate), and the store creates exactly one node table plus four relation tables (`:1408-1429`). Earlier documentation described this step as "hybrid semantic + BM25 retrieval fused by RRF"; that description never matched the code, and it is corrected by the erratum in `CHANGELOG.md` (see also `docs/designs/2026-10-20-argon-k3-panel-verdicts.md` §1, DOC-1).
2. **Capped boost.** §3's usage-and-provenance boost is applied to that distance, bounded at `0.5 * distance` (`:146-151`). A direct match carries `EvidenceKind::DirectMatch` (`:166`).
3. **Bounded directed-graph expansion.** Direct matches are expanded along the graph: one hop over `RELATES_TO`, `CONTAINS` and `GOVERNS` in both directions, and two hops over `GOVERNS` (`:219`, `:224-241`). Each expanded row carries the evidence path that reached it, capped at three paths per result (`:515`). Candidates are admitted per family — the one-hop family first, then the episode-sibling family (the other memories extracted from the same task, reached over `EXTRACTED_FROM`), then the governing-only family — at most `limit` per family, and within a family by co-occurrence count (how many distinct primaries reached the row) descending, with the id as the tie-break (`:465-493`).
4. **Ordering and rendering.** Direct matches sort first on their real boosted distance; expansion rows follow in their admission order, and the discriminator is the evidence kind, not a magic score constant (`:496-499`, `:525-537`). The store returns its primaries plus their bounded expansions without a second truncation (`:539-541`), and the server asks for five primaries and renders everything it gets back (`src/server.rs:2026-2035`, `:2056`).

**Exact-identifier recall — the known gap, and the recommendation (NOT IMPLEMENTED).** Relying solely on dense vector embeddings *is* poor at finding exact variable names, acronyms, or specific syntax: a memory whose text is an exact identifier — a function name, an env var, a config key, an error string — is found only if its *embedding* lands near the query. The adjudicated recommendation for closing that gap is a **narrow deterministic `CONTAINS` / symbol-id match** — reusing the relation and `symbol` rows the store already holds — rather than a Tantivy + RRF build: it adds no dependency, no index to operate, and no second ranking rule to keep auditable (`docs/designs/2026-10-20-argon-k3-panel-verdicts.md` §5, Phase 4). **This is a recommendation only. It is not implemented, and no part of the shipped retrieval path performs exact-token matching today.**

### 7. 3-Tier Stratification (Contextual Hierarchy)
*Inspired by the division of Working Memory vs. Long-Term Memory.*
To prevent context bleeding between vastly different projects or languages, NeuroStrata organizes memory across three hard boundaries:
- **Global:** Universal software engineering principles or developer preferences that apply universally.
- **Domain/Project:** Architectural rules tightly bound to a specific codebase's namespace.
- **Task:** Ephemeral, short-term context tied to an active problem-solving session.

### 8. Goal-State Permanence (Central Executive)
*Inspired by object permanence and executive goal maintenance.*
An AI's memory and task state must never decouple. Goals (tracked work) are Engrams in the same LadybugDB as every other memory — `memory_type: "task"` with a durable, append-only history — so one backup, one restore, and one GraphRAG traversal move knowledge and work state together. A Goal can only be completed through **Knowledge Consolidation**: `neurostrata_task_complete` fails until the work has produced an extracted Engram, and the **Supervisory Attentional System** (pre-push task gate) refuses to ship while Goals remain unfinished. Memory and momentum share one lifecycle.

### 9. Extended Cognition Workspace (Human-AI Symbiosis)
*Inspired by the extended mind thesis.*
Memory is not locked in a black-box database. The memory graph the AI uses can be exported (`/export-graph`) and inspected in the shipped Web UI, so human developers can audit the same cognitive graph the AI relies on. No Obsidian integration — and no component named NeuroVault — ships in this tree.

### 10. Asynchronous Delegation (Orchestrator-Worker Pattern)
*Inspired by the brain's division of labor across specialized regions.*
Operating highly intelligent frontier models on trivial code changes is financially and computationally wasteful. NeuroStrata enforces an Orchestrator/Worker divide. A high-intelligence Orchestrator routes context, manages the memory graph, and structures plans — while the Central Executive tracks the resulting Goals — and aggressively offloads physical code execution (file writes, linting, compiling) to a cheaper, narrowly-focused `NeuroStrata-Task` agent.

### 11. Thalamic Relay (Pub-Sub Event Bus)
*Inspired by the thalamus as the brain's central relay between cortex and sub-cortical structures.*
Memory writes are announcements, not requests. Every storage mutation emits a typed pulse on the **Thalamic Bus** — `Created`, `Superseded`, `Archived`, `GuardedActionFired` — and bookkeeping subscribers (`EpisodicPointerEcho`, `ExportFreshnessDirty`, `GuardEventLog`) react because the write happened, not because an agent remembered to do it. A bounded queue with drop-oldest backpressure, a 250ms per-subscriber timeout, and a `RecursionToken` keep the system honest under load and prevent a subscriber's own writes from re-triggering itself. The relay carries four emit sites: `add_memory`, `supersede_memory`, `task_complete` knowledge-consolidation, and `archive_memory`.

### 12. Procedural Memory (Knowing-How with Rehearsal)
*Inspired by the basal ganglia and procedural learning.*
Knowing-that is only two-thirds of memory. NeuroStrata's third leg carries `memory_type: "procedure"` Engrams with `trigger`, `remaining_fires` (the iteration budget), `valid_to` (decay with disuse), and `last_performed_at`/`performance_count` (strengthened by rehearsal). `neurostrata_procedure_perform` stamps one firing, decrements the budget, and points back to the Episodic Buffer entry that records the act; `get_snapshot`'s `procedures_due` strap surfaces what is still live. A procedure that has spent every fire or lapsed past `valid_to` answers accordingly and stops being surfaced.

### 13. Tombstone-Aware Retrieval
*Inspired by clean recall: a retired memory is one the operator has stopped trusting, and recall surfaces should respect that.*
Tombstones are not deletes: the row keeps its content, its id, and its provenance, and `metadata.archived: true` is *merged* in beside them. The read-time test is strict on the field shape (literal `true` only), so a typo never hides a row. Every read surface — `search_memory`, `get_snapshot`, `export-graph` — honors the tombstone by default, and the uniform `include_archived: true` escape hatch exists for audits and deliberate second-looks. The strict test is written in three places that must agree: the handler helper `crate::handlers::archive_memory::is_archived(payload)` (`src/handlers/archive_memory.rs:57-63`, used by `get_snapshot` at `src/server.rs:1453`), a store-level twin (`src/store/ladybug.rs:1275-1280`, used by the search path at `:121`, `:283`, `:339`, `:417`), and the inline read in the graph export (`:2039-2042`).

### 14. SynapticMesh — the inter-instance synaptic layer

*Inspired by connected brains: the same knowledge should ignite in whichever instance needs it, without one machine's regulator becoming another machine's single point of failure.*

Everything in §1–13 is one machine's mind. SynapticMesh is the *inter-instance* complement: memories (`rule`, `fact`) and knowing-how (procedure **definitions** — never runtime counters, never embeddings) travel between daemons on different machines using plain git as transport. Each node's daemon-owner keeps a git checkout of the team mesh at `~/.NeuroStrata/synaptic-mesh/<mesh_id>/`; a durable WAL outbox is drained by the mesh-worker into `synaptic-mesh-record/v1` files; publish is `git push`, receive is `git fetch` + a validated replay pass that applies records into the local store with mesh provenance, while the effective local view stays private-records + accepted-shared-records, lifecycle- and conflict-aware. Network absence never fails a query.

The locks and invariants, in the same spirit as the Prefrontal Cortex's locks:

- **Sharing boundary:** only `rule`/`fact`/`procedure` definitions cross; `structural`, tasks, embeddings, counters, and the bus's own rows are strictly local. Opt-in by default (`require_record_eligibility`); imported procedures land `untrusted` and fire only after local promotion.
- **Idempotence everywhere:** the record's `update_id` (= the outbox's `stable_op_id`) is the dedup key. A crash before the cursor advance replays and skips by update_id; the cursor never advances past an unresolved conflict.
- **No silent drops:** hostile checkout contents (traversal, symlinks, exec bits, garbage, unknown schema) are ignored and *named*; concurrent same-id conflicts are preserved as named variants in a conflicts dir with an unresolved overlay surfaced by `mesh status` — integration pauses loudly, never silently merges, never silently resolves.
- **Private rows are sovereign:** a row without mesh provenance is never overwritten, never deleted — even where a shared record carries the same id.
- **Convergence discipline:** pushes are normal git pushes; pulls are `--ff-only` (divergent history is an operator decision); sync hooks are fail-soft one-liners that never block a source push.

Open gaps, documented in `src/synaptic_mesh/acceptance.rs`: *old-epoch quarantine is not yet enforced* (`origin_mesh_epoch` is recorded but never compared on import), and *oversized JSON has no size cap*. The permanence disclosure is direct: the mesh is a permanent record; tombstones suppress from search, but erasure requires a coordinated `filter-repo` history rewrite plus an epoch bump, and an uncooperative peer's clone cannot be forced to forget.

Design provenance: **Anthropic/Kimi verdict** (`docs/design-mesh-verdict-kimi.md`) — identified the breaking C-flush merge-conflict, recommended the git-checkout-per-mesh approach (D); **OpenAI/Astra verdict** (`docs/design-mesh-verdict-openai.md`) — supplied the durable-outbox, merge table, sharing-boundary, and trust-model refinements. Full spec: `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`.

## Cognitive-name glossary

See `README.md` § Biological Nomenclature ↔ Engineering Primitives for the canonical table — one table, one set of name conventions, no drift. The 1.8.0 additions appended to that table are the `ThalamicBus`, `ThalamicPulse`, `ExportFreshnessDirty`, `GuardEventLog`, `EpisodicPointerEcho`, `RecursionToken`, and `Procedural Memory` rows.
