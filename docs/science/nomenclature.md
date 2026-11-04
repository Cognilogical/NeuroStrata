# Biological Nomenclature ↔ Systems Engineering Primitives

> **Why you care:** Every neuroscience term in NeuroStrata maps 1-to-1 to a concrete Rust module, data structure, and systems-engineering responsibility—never vague metaphor.

---

## Complete Translation Table

| Biological / Cognitive Term | Systems Engineering Primitive | Rust Module / Surface | What It Does |
| :--- | :--- | :--- | :--- |
| **SynapticGraph** | Embedded Bi-Temporal Property Graph + HNSW Store | `src/store/ladybug.rs` | Stores memory nodes, AST symbols, 384-dim vectors, and directed edges (`GOVERNS`, `RELATES_TO`, `DEPENDS_ON`, `EXTRACTED_FROM`) with `valid_from`/`valid_to` audit history. |
| **Tri-Strata** | 3-Tier Namespace & Working-Memory Partitioning | `src/server.rs`, `src/buffer.rs` | Isolates universal rules (`global`), repository rules (`<namespace>`), and active session working memory (`task` + Episodic Buffer). |
| **Engram** | Typed, Schema-Validated Memory Node | `src/traits.rs`, `src/schemas/` | A single persisted cognitive record (`rule`, `fact`, `procedure`, `task`, `attempt`, `structural`) validated against JSON Schema and SHACL shapes. |
| **Central Executive** | Compile-Time Task FSM & Pre-Push Gate | `src/task/machine.rs`, `src/task/gate.rs` | Enforces Zero-Action Start, 60-minute session claim exclusivity, Lock 1 (`pre-push` gate), Lock 2 (`EXTRACTED_FROM` done-funnel), and Lock 3 (Breath notes). |
| **Prefrontal Cortex** | Semantic Action Guard & Sandboxed Validator | `src/guard/mod.rs` | Intercepts candidate commands/edits via `neurostrata_validate_action`, matching against `guard_rule` patterns and vector similarity to return `Allow`, `Warn`, or `Block`. |
| **Dendritic Bridge** | Multi-Provider Embedder & 4-Stage Hybrid Retriever | `src/embedder.rs`, `src/lexical.rs`, `src/reranker.rs` | Combines local FastEmbed ONNX (`BGESmallENV15`), Tantivy BM25, HNSW vector search, Reciprocal Rank Fusion, Personalized PageRank, and FlashRank cross-encoder reranking. |
| **Thalamic Bus** | Non-Blocking In-Process Pub/Sub Event Relay | `src/events/` | Broadcasts typed mutation pulses (`Created`, `Superseded`, `Archived`, `GuardedActionFired`) to background subscribers with bounded backpressure and recursion guards. |
| **Procedural Memory** | Triggered Habits with Rehearsal Budgets | `src/server.rs` (`neurostrata_procedure_perform`) | Tracks repeatable engineering routines (`memory_type: "procedure"`) with explicit triggers, `performance_count`, and rehearsal budgets. |
| **Episodic Buffer** | Rolling 500 KB JSONL Session Logger & Tag Recall | `src/buffer.rs` (`neurostrata_append_log`) | Captures scrubbed conversational turns into `.NeuroStrata/sessions/` and piggybacks tag-matched memory recall onto every log response. |
| **Neural Gain** | Access-Count & Provenance Salience Weighting | `src/store/ladybug.rs` | Boosts frequently retrieved and source-anchored memories up to a `0.5 * distance` cap (`0.05 * ln(1 + access_count) + SOURCE_RANK_GAIN`). |
| **SynapticMesh** | Brokerless Git Team Memory Synchronization | `src/synaptic_mesh/` | Shares `shareable` rules, facts, and procedure definitions across machines over standard Git (`--ff-only` pull + push) with sovereign private rows and visible conflict tracking. |
| **Laya Judgment** | Calibrated Local ONNX Duplicate & Contradiction Judge | `src/judgment/`, `src/learning/` | Evaluates candidate memory writes for semantic duplication using a locally trainable ONNX model (`artifacts/laya-bundle`) or external System One provider. |
| **NeuroVault** | Bidirectional Obsidian Markdown & Canvas Mirror | `src/obsidian.rs` | Synchronizes graph nodes with Markdown notes (YAML frontmatter + WikiLinks) and exports spatial JSON Canvas topologies. |
