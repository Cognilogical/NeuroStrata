# OxiRS → NeuroStrata: Concept & Technique Transfer Analysis

> Source: https://github.com/cool-japan/oxirs (v0.4.1, 2026-07-28). Rust-native Semantic Web/SPARQL/GraphQL/AI reasoning platform (27 crates, Apache-2.0).
> NeuroStrata baseline: v1.14.3, Rust MCP server, LadybugDB graph+vector, deterministic retrieval (no LLM in the read path), 3-tier namespace stratification, bi-temporal memory, neural-gain salience, procedural memory, SynapticMesh.
> Goal: identify OxiRS concepts/techniques that could enhance NeuroStrata, ranked by fit with NeuroStrata's philosophy (deterministic, local, code-aware, LLM-free read path, JSON-first, no external services by default).

## 0. Executive summary

OxiRS is a much larger surface than NeuroStrata (semantic web platform: RDF/SPARQL/GraphQL, SHACL, OWL/RDFS reasoning, federation, streaming, DID, WASM, industrial IoT). Most of it is **out of scope** for NeuroStrata — NeuroStrata deliberately killed OWL/RDF (Argon adjudication, 2026-10-27) and keeps retrieval LLM-free. The transferable value is concentrated in **deterministic sub-pieces** that OxiRS ships as pure-Rust, dependency-light modules: knowledge-graph embeddings (TransE/RotatE/ComplEx) for structural link prediction, community detection (Louvain/Leiden) for hierarchical summarization, entity linking/mention detection, explainability/provenance chains, signed-graph/DID primitives for tamper-evident mesh records, streaming delta (RDF Patch) for incremental sync, and storage hygiene (mmap snapshots, term interning, WAL durability, zstd). Several of these corroborate and extend findings already captured from the Cognee analysis (`docs/design-cognee-concepts.md`): exact-lexical recall, incremental re-ingest, edge evidence, presort/dry-run, ontology grounding, node sets, conflict review, code graph, session distillation, feedback weighting, global context summaries, temporal reranking, skill-gate surfacing, watermarks, enrichment locks.

**High-value, strong fit (file tasks):** (1) structural/knowledge-graph embeddings to replace the empty Laya duplicate-judgment stub and augment vector retrieval with topology; (2) community detection for hierarchical `get_snapshot` summaries; (3) entity/mention linking from memory text to ingested symbol nodes; (4) subclass/transitive closure for `GOVERNS` (rule inheritance over directory trees); (5) mmap snapshot + term interning for cold-start/RAM; (6) edge evidence / source-chunk citations on graph edges.

**Do not adopt as-is:** RRF/BM25 fusion ranking, LLM completion search types, GraphQL auto-schema as a core dependency, RDF/OWL/SHACL-as-mandatory, federation over HTTP, DID/crypto as a hard requirement, WASM browser engine, industrial IoT protocols. These conflict with NeuroStrata's deterministic/local/code-centric/JSON-first stance or add heavy dependencies.

## 1. Tier 1 — High value, strong fit (recommend filing tasks)

### 1.1 Knowledge-graph embeddings (TransE/RotatE/ComplEx) for structural judgment
- **OxiRS**: `oxirs-embed` ships TransE, ComplEx, RotatE, etc. with model save/load, link prediction, and structural (GraphSAGE-style) embeddings (`gnn_encoder`). `oxirs-shacl-ai` uses them for constraint inference.
- **NeuroStrata gap**: the Laya ONNX judgment path returns empty results by design (`src/judgment/laya_onnx.rs`); duplicate judgment and relation prediction are effectively absent locally. Retrieval uses text embeddings only — graph topology is used for expansion/evidence but never for similarity. `RawDistanceCandidate` dedup is pure vector distance.
- **Adaptation**: add an optional `structural` embedding channel computed from the memory graph (TransE over GOVERNS/CONTAINS/RELATES_TO/DEPENDS_ON/EXTRACTED_FROM) that feeds (a) duplicate-candidate ranking and (b) a secondary retrieval score blended *deterministically* (capped, like neural gain) — never replacing vector distance. Train incrementally on ingest/mutation; persist model alongside the store. Keep strictly optional (feature-gated `kge`) so the default stays LLM-free and dependency-light. This directly upgrades the "Laya stub" into a real local judgment layer without cloud.
- **Effort**: L. **Priority**: P2 (high payoff; needs training-data + eval harness).

### 1.2 Community detection (Louvain/Leiden) for hierarchical summaries
- **OxiRS**: `oxirs-graphrag` ships deterministic Louvain/Leiden community detection (`graph::community`) with a guaranteed floor above the trivial single-community partition, plus hierarchical summarization.
- **NeuroStrata gap**: `get_snapshot` returns a flat top-5 by access count; no hierarchical summary or hub report (Cognee Tier 2 §2.3). Directory/file nodes exist but no cluster-level rollup.
- **Adaptation**: run community detection over the memory graph per namespace; store a summary memory per community (linked via CONTAINS to its members) at ingest/mutation time (Thalamic Bus subscriber). Surface a "root summary" + community hubs in `get_snapshot`. Deterministic given a fixed seed (reproducible snapshots). Fits the existing summary-node concept.
- **Effort**: M. **Priority**: P2.

### 1.3 Entity / mention linking from memory text to symbol nodes
- **OxiRS**: `oxirs-graphrag` has mention detection, candidate ranking, and type classification (`entity_linker`, `entity_classifier`) plus explainability provenance chains.
- **NeuroStrata gap**: ingest captures AST symbols, but memory text that *mentions* a symbol (e.g. a rule saying "handle `foo_bar`") does not create an edge to that symbol node. Retrieval relies on vector similarity, not mention resolution. This is a documented gap in the code-graph item (Cognee §1.8).
- **Adaptation**: at `add_memory`/ingest, run a deterministic mention detector over content against the active generation's symbol index; create `RELATES_TO` (or a new `MENTIONS`) edge from the memory to each resolved symbol node. Surface resolved mentions in `search_memory` evidence. No LLM, no new dependency (reuse tree-sitter symbol table + exact token match).
- **Effort**: M. **Priority**: P2.

### 1.4 Transitive closure for `GOVERNS` (rule inheritance)
- **OxiRS**: SHACL subclass-aware targets compute reflexive+transitive `rdfs:subClassOf` closure (`advanced_features::subclass_closure`); `sh:class` and implicit-class targets honor subclassing.
- **NeuroStrata gap**: `GOVERNS` edges are materialized per declared target; a rule governing a directory does not automatically govern its descendants unless traversal reaches them. Bounded expansion is only 2 hops and is query-time, not materialized. Ingestion deletes/recreates code nodes and loses edges until relink.
- **Adaptation**: compute and materialize the transitive closure of `GOVERNS` over the CONTAINS/file tree at ingest/relink time, so a directory-level rule governs all descendant symbols. Reuse the existing `relink_edges` primitive and staged generations. Cap depth to avoid blowup. This is the code-centric analogue of subclass closure and directly improves recall of applicable rules.
- **Effort**: M. **Priority**: P2.

### 1.5 mmap snapshot + term interning for cold start and RAM
- **OxiRS**: mmap-able RDF store snapshot format gives sub-second cold start (vs ~13.6s re-parsing 1.35M quads); `MemoryStorage` interns terms behind ID-based index permutations (~4x lower RAM); `RdfStore` Persistent backend uses O(N) buffered append.
- **NeuroStrata gap**: daemon cold start loads LadybugDB file; `export_database` is a file copy; no mmap; metadata strings (domain, memory_type, agent_name) are stored verbatim per row.
- **Adaptation**: (a) add an application-level mmap'd read-only snapshot of the hot node/edge tables for sub-second daemon warm-up, rebuilt on checkpoint; (b) intern repeated metadata strings (domain slug, memory_type, user_id) into an id table at the storage layer. Both are storage-engine-adjacent and require care with LadybugDB's C++ core, but the gain (startup latency, RSS) is concrete. Feature-gate behind `mmap-snapshot`.
- **Effort**: L. **Priority**: P3 (measure first; may be blocked by lbug ABI).

### 1.6 Edge evidence / source-chunk citations on graph edges
- **OxiRS**: `include_references=True` attaches the document chunks supporting each graph edge; provenance stamped on every chunk and surfaced in hybrid retrieval; explainability module provides attention-weight contributions and provenance chains.
- **NeuroStrata gap**: graph expansion carries evidence *paths* (relation traversal) but not the originating source chunk/text that justified the edge (Cognee §1.3). `SearchEvidence` has `derivation` but no source ref.
- **Adaptation**: store `source_ref`/`source_chunk_id` on edges (requires lbug relation property migration, as noted in Cognee adjudication §1.3) and surface in `search_memory` output and `export-graph`. Complements existing `metadata.source` on rows.
- **Effort**: S–M (schema migration). **Priority**: P2.

## 2. Tier 2 — Medium value, needs design

### 2.1 Signed memories / DID for tamper-evident SynapticMesh records
- **OxiRS**: `oxirs-did` provides W3C DID, Verifiable Credentials, signed RDF graphs (RDFC-1.0), Ed25519 proofs, trust-chain validation.
- **NeuroStrata gap**: SynapticMesh records are validated by schema/namespace/lifecycle but carry no cryptographic signature; trust is git-authorship + validation. A compromised peer could forge records.
- **Adaptation**: optionally sign each `synaptic-mesh-record/v1` with an Ed25519 key (per-daemon or per-author) and verify on import; store signer DID in record. Keep optional (feature `mesh-signing`) and backward-compatible with unsigned records (trust-but-verify). Improves team trust without changing transport.
- **Effort**: M. **Priority**: P3 (only if team deployments demand it).

### 2.2 Streaming delta (RDF Patch) for incremental mesh sync
- **OxiRS**: `oxirs-stream` supports RDF Patch and SPARQL Update delta; NATS/Redis/MQTT I/O.
- **NeuroStrata gap**: SynapticMesh drains a durable WAL into whole-record files and pulls via git; no delta-level incremental sync. Large namespaces produce large record files.
- **Adaptation**: adopt delta encoding (only changed fields) for `synaptic-mesh-record/v1` on update, keeping full records for create. Reduces push size and merge conflicts. No broker needed — still git transport. Aligns with existing outbox.
- **Effort**: S–M. **Priority**: P3.

### 2.3 ReBAC / graph-level authorization for shared memory
- **OxiRS**: ReBAC (relationship-based access control), graph-level authorization, OAuth2/OIDC/SAML.
- **NeuroStrata gap**: namespaces partition data but there is no per-row access control; SynapticMesh sharing is all-or-nothing per namespace/eligibility.
- **Adaptation**: optional relationship-based access predicates on memory rows (e.g. `visible_to: [team:foo]`) enforced at search/export. Useful for multi-team shared namespaces. Keep optional; default remains open within a namespace.
- **Effort**: M. **Priority**: P3.

### 2.4 SHACL-style shape validation for memory rows
- **OxiRS**: `oxirs-shacl` W3C-compliant SHACL Core + SHACL-AF; subclass-aware targets; `oxirs-shacl-ai` infers constraints from data.
- **NeuroStrata gap**: `memory-vocabulary.v9.json` declares fields but validation is hand-coded in Rust; no declarative shape layer or auto-inference of missing constraints.
- **Adaptation**: express the vocabulary as SHACL-like shapes (JSON, not RDF) validated at write time; optionally run an inference pass to suggest new required fields from observed data. Reuses existing vocabulary tests. Lowers drift between schema and code.
- **Effort**: M. **Priority**: P3.

### 2.5 Solution budget / query guards
- **OxiRS**: `oxirs-wasm` per-store solution budget (`setSolutionBudget`/`clearSolutionBudget`) fails unselective joins fast instead of running to completion.
- **NeuroStrata gap**: bounded traversal already caps hops/limit, but a pathological query (huge fan-out) can still expand many rows before the cap.
- **Adaptation**: add a per-query node-expansion budget (max visited nodes) to `related`/`memories_for_file` that aborts with a typed error instead of silently truncating. Surfaces in the tool result.
- **Effort**: S. **Priority**: P3.

### 2.6 zstd compression for snapshots/backups
- **OxiRS**: pure-Rust zstd shim (`crates/zstd-shim` via `oxiarc-zstd`) applied workspace-wide.
- **NeuroStrata gap**: backups/snapshots are uncompressed; mesh record files are uncompressed JSON.
- **Adaptation**: compress snapshot files and mesh record files with zstd (pure-Rust crate, no C dep). Reduces disk and push size. Transparent to readers.
- **Effort**: S. **Priority**: P3.

### 2.7 Anomaly detection (Welford) for guard/event monitoring
- **OxiRS**: real z-score anomaly detector using Welford's online algorithm; alert-handler dispatch.
- **NeuroStrata gap**: guard validates proposed actions but does not detect anomalous patterns over time (e.g. repeated near-miss actions, unusual access spikes).
- **Adaptation**: add an online anomaly detector over guard-event and access-count streams; surface alerts in `get_snapshot`/bus metrics. Deterministic, no LLM.
- **Effort**: S–M. **Priority**: P3.

### 2.8 GraphQL auto-schema for memory graph exploration
- **OxiRS**: `graphql_autoschema` generates a GraphQL schema from an RDF vocabulary.
- **NeuroStrata gap**: graph is explored via `export-graph` JSON + web-ui; no typed query API.
- **Adaptation**: generate a GraphQL schema from `memory-vocabulary.v9.json` for the web-ui/NeuroVault, enabling typed exploration. Optional, read-only, no core dependency on a GraphQL server.
- **Effort**: M. **Priority**: P3.

### 2.9 ODRL-style data-sovereignty policies for SynapticMesh
- **OxiRS**: ODRL policy engine with spatial/temporal/purpose constraints.
- **NeuroStrata gap**: SynapticMesh privacy is binary (private vs shareable eligibility); no per-record purpose/region/time constraints.
- **Adaptation**: extend `eligibility` metadata with optional constraint fields (purpose, region, expiry) evaluated at publish/import. Formalizes existing privacy boundary. Low urgency.
- **Effort**: M. **Priority**: P4.

## 3. Tier 3 — Low fit / misaligned (do not adopt as-is)

- **RRF/BM25 fusion ranking** (OxiRS GraphRAG): NeuroStrata explicitly rejects BM25/RRF by design (docs/COGNITIVE_ARCHITECTURE.md §6; erratum in CHANGELOG). Adopting would violate the deterministic, auditable single-track retrieval. Borrow only the *deterministic* sub-pieces (community detection, entity linking, explainability).
- **LLM completion search types** (GRAPH_COMPLETION, COT, RAG_COMPLETION, FEELING_LUCKY, AGENTIC_COMPLETION): NeuroStrata keeps retrieval LLM-free. Could be offered as an optional client-side layer, never in core.
- **SPARQL/GraphQL as a core dependency**: NeuroStrata is JSON-first and graph-native via LadybugDB; adding a SPARQL engine is a heavy, misaligned dependency. If a query language is wanted, a small JSON-graph query DSL is more aligned.
- **RDF/OWL/SHACL-as-mandatory**: explicitly killed (Cognee adjudication §1.5). JSON vocabulary + hand validation is the chosen path; SHACL only as an optional shape layer (§2.4).
- **Federation over HTTP (SERVICE)**: NeuroStrata's mesh is git-based and offline-first; HTTP federation breaks the no-external-services default.
- **DID/crypto as a hard requirement**: keep optional; default trust model stays git + validation.
- **WASM browser engine**: LadybugDB is C++ and not WASM-friendly; web-ui stays a separate client.
- **Industrial IoT protocols** (Modbus/CANbus/NGSI-LD/physics simulation/digital twins): out of domain for a coding-agent memory system.
- **Multi-tenancy / per-user DB isolation**: NeuroStrata uses namespaces + SynapticMesh; different trust model, not needed.

## 4. Recommended next actions

1. File tasks for Tier 1 items 1.1 (KGE structural judgment) and 1.3 (entity/mention linking) first — both upgrade existing gaps (empty Laya stub, code-graph edges) with concrete payoff.
2. Then 1.2 (community detection summaries), 1.4 (GOVERNS transitive closure), 1.6 (edge evidence) as medium enhancements.
3. Tier 2 items are design-gated: write a spec before implementing; do not implement speculatively. Prioritize 2.5 (solution budget) and 2.6 (zstd) as low-risk, small wins.
4. Cross-reference with Cognee analysis: 1.1 corroborates Cognee §1.8 (code graph) and §2.2 (feedback) at the structural level; 1.6 corroborates Cognee §1.3 (edge evidence). Avoid duplicating effort — file one task per capability, citing both sources.

## 5. Cross-reference to existing NeuroStrata decisions

- Laya stub / duplicate judgment: `src/judgment/laya_onnx.rs`, `src/judgment/typesafe_jev.rs`; README §"How Laya and Jev make this structurally different".
- Exact-identifier recall gap: docs/COGNITIVE_ARCHITECTURE.md §6 (NOT IMPLEMENTED); Cognee §1.1.
- Code graph / richer edges: Cognee §1.8; `src/parser/ingest.rs`.
- Edge evidence: Cognee §1.3; `SearchEvidence` in `src/traits.rs`.
- Community/hierarchical summaries: Cognee §2.3.
- SynapticMesh privacy/sharing: docs/COGNITIVE_ARCHITECTURE.md §14; `src/synaptic_mesh/`.
- Neural gain salience formula: docs/COGNITIVE_ARCHITECTURE.md §3.
- Vocabulary version: `src/traits.rs` (v9); `src/schemas/memory-vocabulary.v*.json`.

---
*Analysis performed 2026-10-27. Source repo shallow-reviewed via README, crate list, and module trees (graphrag, embed, shacl-ai). No code-level verification of individual crate APIs; verify before filing implementation tasks.*