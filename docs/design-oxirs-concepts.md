# OxiRS → NeuroStrata: Concept & Technique Transfer Analysis (REVISED)

> **Revision note (2026-10-27):** the original version of this document framed most of OxiRS as out of scope on three assumptions — (a) "JSON only, no OWL/RDF" as a permanent architectural mandate, (b) local-only as a requirement, (c) NeuroStrata as code-centric. The owner corrected all three: JSON-only was a *feature-scope* constraint for the 24-feature batch, not permanent architecture; local is the default but remote is configurable; and NeuroStrata is intended as a **general-purpose agent memory system**, not a code-centric one. The owner leans toward OWL/RDF and expects to eventually scrap the JSON implementation. This revision re-evaluates OxiRS accordingly. The prior (narrower) framing is superseded.

> Source: https://github.com/cool-japan/oxirs (v0.4.1, 2026-07-28). Rust-native Semantic Web/SPARQL/GraphQL/AI reasoning platform (27 crates, Apache-2.0).
> NeuroStrata baseline: v1.14.3, Rust MCP server, LadybugDB graph+vector, deterministic retrieval, 3-tier namespaces, bi-temporal memory, neural-gain salience, procedural memory, SynapticMesh.

## 0. Executive summary

Under the corrected framing, **OxiRS is highly relevant — arguably the closest existing Rust-native match for where NeuroStrata is heading.** OxiRS is a semantic-web platform (RDF/SPARQL/OWL/SHACL/GraphQL) with a large AI/retrieval/storage substrate. NeuroStrata's stated direction (general-purpose agent memory, OWL/RDF-leaning, local-default-but-remote-configurable) aligns with OxiRS's center of gravity rather than against it. The strategic implication: NeuroStrata's JSON vocabulary + LadybugDB ad-hoc graph is likely a transitional representation; OxiRS's RDF/SPARQL/OWL/SHACL stack is the natural long-term canonical data model and query layer.

**Highest-value, strongest fit (file tasks / specs):**
1. **RDF as the canonical memory data model** (oxirs-core, oxirs-ttl, oxirs-star) — replace JSON vocabulary rows with RDF triples/quads; memory provenance as RDF-star quoted triples.
2. **OWL/RDFS reasoning** (oxirs-rule) — infer transitive GOVERNS, subclass, equivalence; replaces hand-coded bounded traversal with a real reasoner.
3. **SHACL shape validation + inference** (oxirs-shacl, oxirs-shacl-ai) — declarative shapes for memory rows; auto-infer missing constraints from data.
4. **SPARQL 1.1/1.2 query engine** (oxirs-arq) — general-purpose query over the memory graph instead of custom tool-specific traversals.
5. **GraphQL auto-schema** (oxirs-gql) — typed query/exploration API over memory graph.
6. **Knowledge-graph embeddings** (oxirs-embed: TransE/RotatE/ComplEx, GraphSAGE) — structural link prediction + duplicate judgment, replacing the empty Laya stub.
7. **Community detection** (oxirs-graphrag: Louvain/Leiden) — hierarchical summaries for `get_snapshot`.
8. **Full-text search** (Tantivy, in oxirs-tdb/oxirs-fuseki) — lexical recall for general memory, not just code symbols.
9. **Federation** (oxirs-federate, SERVICE) — multi-instance memory queries; natural evolution of SynapticMesh.
10. **Streaming delta** (RDF Patch, oxirs-stream) — real-time incremental sync.
11. **DID / signed graphs** (oxirs-did) — tamper-evident shared memory records.
12. **ReBAC / graph-level auth** (oxirs-fuseki) — multi-user/team access control.
13. **WASM browser/edge** (oxirs-wasm) — NeuroVault in the browser.
14. **TDB2 durable storage** (oxirs-tdb) — on-disk backend option; mmap snapshots; term interning; WAL durability.
15. **RAG chat** (oxirs-chat) — optional LLM-over-memory layer (remote-configurable).
16. **GraphRAG hybrid search** (oxirs-graphrag) — vector × graph × (optional) keyword fusion.

**Lower relevance (keep optional):** industrial IoT (Modbus/CANbus), physics simulation, digital twins, NGSI-LD smart-city sensors — these are domain applications, not memory primitives; only adopt if NeuroStrata targets those domains.

## 1. Tier 1 — High value, strong fit (recommend filing tasks/specs)

### 1.1 RDF as the canonical memory data model
- **OxiRS**: `oxirs-core` (RDF 1.2, 7 syntaxes, persistent N-Quads), `oxirs-ttl`, `oxirs-star` (RDF-star quoted triples).
- **NeuroStrata gap**: memory rows are JSON payloads in LadybugDB with a hand-maintained vocabulary (`memory-vocabulary.v9.json`). Edges are 5 typed relations. No standard serialization/interchange; SynapticMesh invented its own wire format.
- **Adaptation**: adopt RDF quads as the canonical representation: a memory = a set of triples (subject = memory id, predicates = vocabulary fields), stored in TDB2 or LadybugDB. SynapticMesh records become standard RDF Patch/N-Quads. Enables interoperability, SPARQL, and eventual JSON removal. This is the big migration; phase it (dual-write JSON+RDF first).
- **Effort**: XL. **Priority**: P1 (strategic).

### 1.2 OWL/RDFS reasoning
- **OxiRS**: `oxirs-rule` forward/backward rule engine (RDFS/OWL 2 DL, SWRL), TransE training, etc.
- **NeuroStrata gap**: GOVERNS/CONTAINS/RELATES_TO traversal is hand-coded and bounded (1–2 hops). No transitive closure, no subclass/equivalence inference. Rules governing a directory do not propagate to descendants unless traversal reaches them.
- **Adaptation**: run an OWL/RDFS reasoner over the memory graph to materialize inferred edges (transitive GOVERNS, subclass closure, equivalence). Replaces hand-coded bounded expansion with a principled, configurable rule set. Aligns with the owner's OWL lean.
- **Effort**: L. **Priority**: P1.

### 1.3 SHACL shape validation + AI inference
- **OxiRS**: `oxirs-shacl` (W3C SHACL Core + SHACL-AF, subclass-aware targets), `oxirs-shacl-ai` (constraint inference from data).
- **NeuroStrata gap**: vocabulary declared in JSON; validation hand-coded in Rust; drift between schema and code.
- **Adaptation**: express memory shapes as SHACL (or JSON-LD shapes) validated at write time; run inference pass to suggest missing constraints. Removes hand-coded validation drift.
- **Effort**: M. **Priority**: P2.

### 1.4 SPARQL 1.1/1.2 query engine
- **OxiRS**: `oxirs-arq` (Jena-style algebra, aggregates, subqueries, SERVICE, CONSTRUCT/DESCRIBE, GRAPH).
- **NeuroStrata gap**: retrieval is tool-specific (search, related, memories_for_file); no general query language. Agents cannot ask ad-hoc graph questions.
- **Adaptation**: expose a SPARQL endpoint (local default, remote-configurable) over the memory graph. Replaces/augments custom traversal tools. Enables federation and CONSTRUCT-based views.
- **Effort**: L. **Priority**: P2.

### 1.5 GraphQL auto-schema
- **OxiRS**: `oxirs-gql` (Juniper) with `graphql_autoschema` from RDF vocabulary.
- **NeuroStrata gap**: graph explored via `export-graph` JSON + web-ui; no typed query API.
- **Adaptation**: generate GraphQL schema from the memory vocabulary for the web-ui/NeuroVault and external clients. Read-only exploration layer.
- **Effort**: M. **Priority**: P3.

### 1.6 Knowledge-graph embeddings (TransE/RotatE/ComplEx, GraphSAGE)
- **OxiRS**: `oxirs-embed` (TransE, ComplEx, RotatE, model save/load, link prediction), `gnn_encoder` (GraphSAGE structural embeddings).
- **NeuroStrata gap**: Laya ONNX judgment path returns empty by design; duplicate judgment and relation prediction absent locally. Retrieval uses text embeddings only; topology never feeds similarity.
- **Adaptation**: add an optional structural embedding channel (TransE over GOVERNS/CONTAINS/RELATES_TO/DEPENDS_ON/EXTRACTED_FROM) feeding duplicate-candidate ranking and a capped secondary retrieval score. Feature-gated (`kge`), optional. Upgrades the Laya stub into a real local judgment layer.
- **Effort**: L. **Priority**: P2.

### 1.7 Community detection (Louvain/Leiden)
- **OxiRS**: `oxirs-graphrag` community detection (deterministic given seed, floor above trivial partition) + hierarchical summarization.
- **NeuroStrata gap**: `get_snapshot` is flat top-5 by access; no cluster rollup.
- **Adaptation**: run community detection per namespace; store summary memory per community (CONTAINS-linked); surface root summary + hubs in snapshot.
- **Effort**: M. **Priority**: P2.

### 1.8 Entity / mention linking
- **OxiRS**: `oxirs-graphrag` mention detection, candidate ranking, type classification (`entity_linker`, `entity_classifier`); provenance chains.
- **NeuroStrata gap**: ingested symbols exist, but memory text mentioning a symbol creates no edge.
- **Adaptation**: deterministic mention detector over content against the active symbol index; create edges from memory to resolved symbols; surface in search evidence. No LLM.
- **Effort**: M. **Priority**: P2.

### 1.9 Full-text search (Tantivy)
- **OxiRS**: Tantivy full-text (feature-gated in oxirs-tdb/oxirs-fuseki).
- **NeuroStrata gap**: retrieval is vector-only by design; exact-identifier recall gap documented (docs/COGNITIVE_ARCHITECTURE.md §6). The prior recommendation was narrow CONTAINS/symbol-id match.
- **Adaptation**: add an optional Tantivy index over memory content + symbol names for lexical recall, blended deterministically (not RRF) or offered as a separate `lexical` search mode. General-purpose memory benefits more than code-only. Keep auditable (single ranking rule).
- **Effort**: M. **Priority**: P2 (closes documented correctness gap).

### 1.10 GraphRAG hybrid search (vector × graph × keyword)
- **OxiRS**: `oxirs-graphrag` (RRF fusion, N-hop SPARQL expansion, community detection, explainability, feedback reweighting, GNN encoder, neuro-symbolic fusion).
- **NeuroStrata gap**: vector + bounded graph expansion; no keyword track; no community-aware context; no feedback reweighting beyond access_count.
- **Adaptation**: adopt the *deterministic* sub-pieces (community detection, entity linking, explainability, feedback reweighting) and optionally the full hybrid pipeline as a configurable mode. RRF fusion becomes a candidate rather than a rejected design — the owner has not ruled it out. Keep the LLM completion layer optional/remote.
- **Effort**: L. **Priority**: P2–P3.

### 1.11 Federation (SERVICE) across instances
- **OxiRS**: `oxirs-federate` (SERVICE planner, 2PC, GraphQL stitching, federated auth).
- **NeuroStrata gap**: SynapticMesh is git-batch; no live cross-instance query.
- **Adaptation**: evolve SynapticMesh into a federated memory layer: SPARQL SERVICE across daemon instances for live queries, git for durable sync. Natural convergence.
- **Effort**: L. **Priority**: P3.

### 1.12 Streaming delta (RDF Patch)
- **OxiRS**: `oxirs-stream` (RDF Patch, SPARQL Update delta, NATS/Redis/MQTT).
- **NeuroStrata gap**: SynapticMesh drains whole-record files; no delta encoding.
- **Adaptation**: delta-encode `synaptic-mesh-record/v1` updates; optionally stream over NATS/MQTT for real-time sync (remote-configurable). Reduces push size/conflicts.
- **Effort**: M. **Priority**: P3.

### 1.13 DID / signed graphs
- **OxiRS**: `oxirs-did` (W3C DID, Verifiable Credentials, signed RDF graphs RDFC-1.0, Ed25519, trust chains).
- **NeuroStrata gap**: mesh records validated by schema/namespace but unsigned; trust = git authorship.
- **Adaptation**: optionally sign mesh records with Ed25519 (per-daemon/per-author) and verify on import; store signer DID. Backward-compatible with unsigned. Improves team trust.
- **Effort**: M. **Priority**: P3.

### 1.14 ReBAC / graph-level authorization
- **OxiRS**: ReBAC (relationship-based access control), graph-level authorization, OAuth2/OIDC/SAML.
- **NeuroStrata gap**: namespaces partition data but no per-row access control; mesh sharing is all-or-nothing per eligibility.
- **Adaptation**: optional relationship-based access predicates on memory rows (`visible_to`), enforced at search/export. Multi-user/team memory.
- **Effort**: M. **Priority**: P3.

### 1.15 WASM browser/edge
- **OxiRS**: `oxirs-wasm` (browser RDF/SPARQL, TypeScript defs, Cloudflare Workers/Deno).
- **NeuroStrata gap**: NeuroVault is a separate client; no in-browser memory engine.
- **Adaptation**: compile memory engine (or a read-only subset) to WASM for browser-based NeuroVault and edge deployments.
- **Effort**: L. **Priority**: P3 (blocked partly by LadybugDB C++ ABI; easier after RDF/TDB2 migration).

### 1.16 TDB2 durable storage + mmap + term interning + WAL
- **OxiRS**: `oxirs-tdb` (TDB2: superblock, fsync writes, free-page allocator, six indexes, WAL with crash recovery, checkpoint LSN, mmap snapshot, term interning ~4x RAM, O(N) buffered append).
- **NeuroStrata gap**: LadybugDB single file, checkpoint, file-copy snapshot; no mmap; verbatim metadata strings; no WAL-level crash recovery exposed.
- **Adaptation**: either adopt TDB2 as an alternative backend (remote/local configurable) or port the hygiene (mmap snapshot, term interning, WAL durability) into the LadybugDB path. Improves cold start and RSS.
- **Effort**: L. **Priority**: P2–P3.

### 1.17 RAG chat (optional LLM-over-memory)
- **OxiRS**: `oxirs-chat` (RAG chat API, conversation history, LLM providers).
- **NeuroStrata gap**: retrieval is LLM-free in core; no conversational memory layer.
- **Adaptation**: optional remote-configurable conversational layer over the memory graph. Core stays deterministic; LLM is an opt-in client.
- **Effort**: M. **Priority**: P3.

### 1.18 RDF-star for memory provenance
- **OxiRS**: `oxirs-star` (RDF-star parse/serialize, quoted triples in queries).
- **NeuroStrata gap**: provenance is `metadata.source` on rows; edges carry no source chunk ref.
- **Adaptation**: represent memory provenance as quoted triples (a statement about a statement), enabling SPARQL-star queries over "who said what, when". Natural fit for bi-temporal memory.
- **Effort**: M. **Priority**: P2.

### 1.19 Subclass/transitive closure + solution budget + zstd + anomaly detection
- **OxiRS**: SHACL subclass closure; `oxirs-wasm` solution budget; zstd shim; Welford anomaly detector.
- **NeuroStrata gap**: hand-coded traversal; no query budget; uncompressed snapshots/mesh records; no anomaly monitoring.
- **Adaptation**: materialize transitive closure at ingest/relink; add per-query expansion budget (typed abort); zstd-compress snapshots/mesh records; online anomaly detection over guard/access streams.
- **Effort**: S–M each. **Priority**: P3.

## 2. Tier 2 — Medium value / domain-specific (design-gate before adopting)

- **Time-series DB** (oxirs-tsdb) — episodic/access metrics over time; optional.
- **Multimodal validation** (oxirs-shacl-ai multimodal) — non-text memory (images/audio) validation; only if memory scope expands beyond text.
- **Physics simulation / digital twins / NGSI-LD / Modbus / CANbus** — domain applications, not memory primitives. Adopt only if NeuroStrata targets those domains. Low priority for a general agent memory system.
- **Confluent Schema Registry client** — validate mesh wire records against a schema; nice-to-have for mesh hygiene.

## 3. Tier 3 — Low fit (do not adopt as-is)

- **Industrial IoT protocols as core features** (Modbus, CANbus, NGSI-LD) — out of domain unless explicitly targeted.
- **Physics-informed digital twins** — out of domain.
- **LLM completion as a core retrieval path** — keep LLMs in optional client/remote layers; core retrieval stays deterministic and auditable. (Remote LLM is configurable, but core must not depend on it.)

## 4. Recommended next actions

1. **Strategic decision first**: confirm the RDF migration direction (owner leans OWL/RDF, expects to scrap JSON). If confirmed, file a spec for §1.1 (RDF canonical model) + §1.2 (OWL reasoning) + §1.4 (SPARQL) as the foundation; everything else layers on.
2. **In parallel (no migration needed)**: file tasks for §1.6 (KGE), §1.7 (community detection), §1.8 (entity linking), §1.9 (Tantivy lexical), §1.18 (RDF-star provenance) — these work against the current LadybugDB/JSON store and pay off immediately.
3. **Design-gate** Tier 2 items; do not implement speculatively.
4. **Cross-reference with Cognee analysis** (`docs/design-cognee-concepts.md`): §1.9 corroborates Cognee §1.1 (lexical recall); §1.8 corroborates Cognee §1.8 (code graph); §1.10 subsumes Cognee §1.3/§2.x. File one task per capability citing both sources.

## 5. Cross-reference to existing NeuroStrata decisions

- Laya stub / duplicate judgment: `src/judgment/laya_onnx.rs`, `src/judgment/typesafe_jev.rs`.
- Exact-identifier recall gap: docs/COGNITIVE_ARCHITECTURE.md §6 (NOT IMPLEMENTED); Cognee §1.1.
- Code graph / richer edges: Cognee §1.8; `src/parser/ingest.rs`.
- Edge evidence: Cognee §1.3; `SearchEvidence` in `src/traits.rs`.
- SynapticMesh privacy/sharing: docs/COGNITIVE_ARCHITECTURE.md §14; `src/synaptic_mesh/`.
- Neural gain salience formula: docs/COGNITIVE_ARCHITECTURE.md §3.
- Vocabulary version: `src/traits.rs` (v9); `src/schemas/memory-vocabulary.v*.json`.
- All-features scope ("JSON only; no OWL/RDF..."): feature-batch constraint, not permanent architecture — clarified by owner 2026-10-27.

---
*Analysis revised 2026-10-27 per owner correction. Source repo shallow-reviewed via README, crate list, and module trees. No code-level verification of individual crate APIs; verify before filing implementation tasks.*