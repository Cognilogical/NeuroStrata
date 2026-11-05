# Biological Nomenclature ↔ Systems Engineering Primitives

> **Why you care:** Every neuroscience term in NeuroStrata maps 1-to-1 to a concrete Rust module, data structure, and systems-engineering responsibility—never vague metaphor.

---

## Complete Translation Table

| Biological / Cognitive Term | Systems Engineering Primitive | Rust Module / Surface | What It Does |
| :--- | :--- | :--- | :--- |
| **SynapticGraph** | Embedded Bi-Temporal Property Graph + HNSW Store | `src/store/ladybug.rs` | Stores memory nodes, AST symbols, 768-dim vectors, and directed edges (`GOVERNS`, `RELATES_TO`, `DEPENDS_ON`, `EXTRACTED_FROM`) with `valid_from`/`valid_to` audit history. |
| **Tri-Strata** | 3-Tier Namespace & Working-Memory Partitioning | `src/server.rs`, `src/buffer.rs` | Isolates universal rules (`global`), repository rules (`<namespace>`), and active session working memory (`task` + Episodic Buffer). |
| **Engram** | Typed, Schema-Validated Memory Node | `src/traits.rs`, `src/schemas/` | A single persisted cognitive record (`rule`, `fact`, `procedure`, `task`, `attempt`, `structural`) validated against JSON Schema and SHACL shapes. |
| **Central Executive** | Compile-Time Task FSM & Pre-Push Gate | `src/task/machine.rs`, `src/task/gate.rs` | Enforces Zero-Action Start, 60-minute session claim exclusivity, Lock 1 (`pre-push` gate), Lock 2 (`EXTRACTED_FROM` done-funnel), and Lock 3 (Breath notes). |
| **Prefrontal Cortex** | Semantic Action Guard & Sandboxed Validator | `src/guard/` | Intercepts candidate commands/edits via the `neurostrata_guard_validate` MCP tool, matching the payload against learned behavioral rules (`neurostrata_guard_learn`, `neurostrata_guard_list_rules`) with vector similarity and optionally executing the action in a Podman sandbox, returning a `ValidateVerdict` (`DeterministicReject`, `SandboxReject`, `SandboxPassHighFidelity`, `SandboxPassLowFidelity`, `ApprovedFailOpen`). |
| **Dendritic Bridge** | Multi-Provider Embedder & Three-Surface Retriever | `src/embed.rs`, `src/lexical.rs`, `src/fts/mod.rs` | Combines local FastEmbed ONNX (`NomicEmbedTextV15`, 768 dims), a Tantivy full-text surface, an in-house 5-field BM25 surface, and a bounded graph expansion. The three retrieval surfaces are alternatives chosen with the `mode` argument — there is no rank fusion, no Personalized PageRank, and no neural reranker. |
| **Thalamic Bus** | Non-Blocking In-Process Pub/Sub Event Relay | `src/events/` | Broadcasts typed mutation pulses (`Created`, `Superseded`, `Archived`, `GuardedActionFired`) to background subscribers with bounded backpressure and recursion guards. |
| **Procedural Memory** | Triggered Habits with Rehearsal Budgets | `src/server.rs` (`neurostrata_procedure_perform`) | Tracks repeatable engineering routines (`memory_type: "procedure"`) with explicit triggers, `performance_count`, and rehearsal budgets. |
| **Episodic Buffer** | Rolling 500 KB Markdown Session Logger & Tag Recall | `src/buffer.rs` (`neurostrata_append_log`) | Captures conversational turns into the rolling file `.NeuroStrata/sessions/current.md` and piggybacks tag-matched memory recall onto every log response. A turn containing a credential is rejected outright, not redacted. |
| **Neural Gain** | Access-Count & Provenance Salience Weighting | `src/store/ladybug.rs` | Boosts frequently retrieved and source-anchored memories up to a `0.5 * distance` cap (`0.05 * ln(1 + access_count) + SOURCE_RANK_GAIN`). |
| **SynapticMesh** | Brokerless Git Team Memory Synchronization | `src/synaptic_mesh/` | Shares `shareable` rules, facts, and procedure definitions across machines over standard Git (`--ff-only` pull + push) with sovereign private rows and visible conflict tracking. |
| **Laya Judgment** | Calibrated Local ONNX Duplicate & Contradiction Judge | `src/judgment/`, `src/learning/` | Evaluates candidate memory writes for semantic duplication using a locally trainable ONNX model (`artifacts/laya-bundle`) or external System One provider. |
| **NeuroVault** | *Planned, not implemented* — bidirectional Obsidian Markdown & Canvas mirror | — | **Not implemented in this codebase.** There is no `src/obsidian.rs` module, no `neurostrata_sync_obsidian` MCP tool, and no `obsidian-sync` CLI subcommand. The feature is tracked by NeuroStrata task `neurostrata-dlag`. For a human-readable view of the graph today, use the web UI (`web-ui/`) and `neurostrata-mcp export-graph`. |
