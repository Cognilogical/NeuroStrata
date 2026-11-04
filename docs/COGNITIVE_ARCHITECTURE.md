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

### 6. 4-Stage Hybrid Retrieval — Lexical + Vector + Graph PPR + Cross-Encoder Reranking
*Inspired by human dual-process theory (verbatim token recall + conceptual gist + associative spreading activation).*

Retrieval executes a deterministic 4-stage hybrid pipeline that combines exact-identifier precision, dense semantic similarity, multi-hop graph topology, and neural cross-encoder reranking:

1. **Stage 1 — Dual-Track Candidate Generation (Tantivy BM25 + LadybugDB HNSW Vector):**
   - **Lexical Track (`src/lexical.rs`):** A full-text Tantivy BM25 index scores exact identifiers, function names, config keys, file paths, and error codes (`recall_exact_identifier`).
   - **Dense Vector Track (`src/store/ladybug.rs`):** Active, non-archived, non-conflicting Engrams are scanned via 384-dimensional HNSW cosine distance (`BGESmallENV15`) with §3's capped Neural Gain boost (`min(usage_gain + source_gain, 0.5 * d)`).
2. **Stage 2 — Reciprocal Rank Fusion (RRF) & Temporal Proximity Boost:**
   - Lexical and vector rankings are fused via Reciprocal Rank Fusion (`k = 60`), while queries carrying temporal expressions (`src/graph_read/temporal.rs`) apply `temporal_proximity_boost` to surface chronologically aligned memories.
3. **Stage 3 — Bounded Directed-Graph Expansion & Personalized PageRank (PPR):**
   - Primary hits expand across `RELATES_TO`, `CONTAINS`, `DEPENDS_ON`, `EXTRACTED_FROM`, and 2-hop `GOVERNS` + Tree-sitter AST (`calls`, `imports`) edges. Each expanded node carries its explicit provenance path (`EvidenceKind`) and Personalized PageRank weight.
4. **Stage 4 — FlashRank ONNX Cross-Encoder Reranking (`src/reranker.rs`):**
   - Top candidate pairs `(query, memory)` are jointly scored through a local ONNX cross-encoder (`ms-marco-MiniLM-L-12-v2`) to ensure only the highest-precision context reaches the agent's prompt window.

### 7. 3-Tier Stratification (Contextual Hierarchy)
*Inspired by the division of Working Memory vs. Long-Term Memory.*
To prevent context bleeding between vastly different projects or languages, NeuroStrata organizes memory across three hard boundaries:
- **Global:** Universal software engineering principles or developer preferences that apply universally.
- **Domain/Project:** Architectural rules tightly bound to a specific codebase's namespace.
- **Task:** Ephemeral, short-term context tied to an active problem-solving session.

### 8. Goal-State Permanence (Central Executive)
*Inspired by object permanence and executive goal maintenance.*
An AI's memory and task state must never decouple. Goals (tracked work) are Engrams in the same LadybugDB as every other memory — `memory_type: "task"` with a durable, append-only history — so one backup, one restore, and one GraphRAG traversal move knowledge and work state together. A Goal can only be completed through **Knowledge Consolidation**: `neurostrata_task_complete` fails until the work has produced an extracted Engram, and the **Supervisory Attentional System** (pre-push task gate) refuses to ship while Goals remain unfinished. Memory and momentum share one lifecycle.

### 9. Extended Cognition Workspace (Human-AI Symbiosis via NeuroVault)
*Inspired by the extended mind thesis.*
Memory is not locked in a black-box database. Through the NeuroVault and Obsidian integration, the exact same memory nodes the AI uses to structure code are exposed as a visual, navigable markdown graph for human developers. Humans can curate, audit, and organically learn from the same cognitive graph the AI relies on, creating true symbiosis.

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

### 15. Symbolic Reasoning, Community Topology & World-Model Simulation
*Inspired by neocortical symbolic reasoning, topological modularity, and prefrontal mental simulation.*

Beyond associative recall, NeuroStrata embeds four higher-order cognitive faculties directly in the binary:
- **W3C Semantic Web Reasoning (`src/rdf/`):** Projects graph nodes into RDF triples, executes federated SPARQL 1.1 queries (`neurostrata_sparql_query`), materializes OWL 2 RL transitive and inverse inferences (`neurostrata_owl_reason`), and validates every write against versioned Turtle SHACL shapes (`src/schemas/shacl/`).
- **Leiden Community Detection (`src/community.rs`):** Continuously partitions densely connected memory and AST nodes into modular communities with thematic summaries.
- **TransE Knowledge Graph Embeddings (`src/kge.rs`):** Learns relational translation vectors (`h + r ≈ t`) over graph triples (`neurostrata_kge_train`) to predict missing links and structural analogies.
- **Digital Twin State Simulation (`src/twin.rs`):** Maintains versioned state models (`neurostrata_twin_define`, `neurostrata_twin_observe`, `neurostrata_twin_simulate`) so agents can test hypothetical state transitions and compare branched outcomes before mutating production systems.

## Cognitive-name glossary

See [`docs/science/nomenclature.md`](science/nomenclature.md) for the canonical translation table mapping every biological term to its Rust systems-engineering primitive.
