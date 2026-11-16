# Argon + K3 Adjudication: OxiRS Transfer Analysis for NeuroStrata

Date: 2026-10-27. Design only. No implementation authorized.

Independent seats: Argon (Gemini 4 Argon) and K3 (Kimi K3.1 Pro), run in parallel
without reading each other's output. This document adjudicates them against the
repository at HEAD and against verified crate metadata, following the pattern of
`docs/designs/2026-10-27-owl-re-evaluation.md`.

Source under review: `docs/design-oxirs-concepts.md` (revised 2026-10-27).
Task: `neurostrata-83no`.

## 1. Verdict

**APPROVE-WITH-CHANGES (both seats agree).** RDF is sound as the canonical
*logical* model for NeuroStrata's general-purpose agent-memory direction.
OxiRS/TDB2 as an *immediate physical replacement* for LadybugDB is not sound.
Keep LadybugDB operational during a reversible, per-namespace migration; a
permanent second-class RDF projection would preserve schema drift.

The analysis's Tier 1 is overstuffed (19 items) and mixes foundation,
retrieval fixes, research, remote services, and UX experiments. Re-tier into
P0/P1/P2/P3 per §3. The "do now requires no migration" claim is false for 3 of
the 5 listed items (see §6).

## 2. Agreements (both seats)

1. **RDF-as-canonical logical model: sound.** NeuroStrata's domain (rule, fact,
   procedure, task, bi-temporal `valid_from`/`valid_to`, `metadata.source`,
   `synaptic-mesh-record/v1`) is textbook Named-Graph / RDF-star territory.
   Quoted triples eliminate the current split where edges in
   `src/store/ladybug.rs:1790-1804` cannot carry provenance/timestamps/generation.
2. **OWL profile: RL only, never DL.** Argon: "OWL 2 RL + Datalog". K3: "reject
   unqualified OWL 2 DL; start RDFS + explicit OWL RL". Adjudicate to K3's
   framing — and note `oxirs-rule` advertises RDFS/OWL RL, not OWL 2 DL, so
   the analysis's "OWL 2 DL" wording is inaccurate.
3. **SHACL Core with foundation (P1), not after OWL.** `oxirs-shacl-ai`
   (AI constraint inference) demoted to P3 — auto-inferred constraints
   institutionalize drift.
4. **SPARQL as augmentation (P2), not a replacement.** Read-only, local-first,
   namespace-constrained, budgeted. Cannot replace vector distance, neural-gain
   ranking, evidence paths, or lifecycle filtering in `src/store/ladybug.rs`.
5. **Kills:** big-bang LadybugDB→TDB2 replacement; dual-write without
   atomicity/reconciliation; auto-generated GraphQL as stable contract; core RAG
   chat inside the daemon; WASM browser engine as initial objective; IoT/physics.
6. **Licensing/crate hygiene:** avoid `horned-owl` (LGPL-3.0), avoid `rustdl`
   (soundness), avoid `oxirs-core` direct dependency (ctor pre-main rustls
   provider hook + scirs2 bloat). Prefer `oxrdf`/`oxttl`/`oxjsonld` + selective
   vendoring of Apache-2.0 modules. Strict Apache-2.0/MIT allowlist in
   `cargo-deny`.

## 3. Priority reorder (P0 → P3)

**P0 (immediate)**
- Resolve or formally retire the Laya stub (`src/judgment/laya_onnx.rs:55,109`)
  per `AGENTS.md` §0 Prime Directive (it is listed as a prior compliance failure).
  Wiring real ONNX **or** formally retiring with extracted memory both satisfy;
  KGE is not the only path.
- SynapticMesh v1 hygiene: oversized-record size cap + `origin_mesh_epoch`
  quarantine (both documented open in `COGNITIVE_ARCHITECTURE.md` §14).

**P1 (foundation)**
- RDF-star/Named-Graph canonical data + wire model spec (§1.1 + §1.18 merged).
- SHACL Core shape validation at write/import boundary (§1.3).
- OWL 2 RL / Datalog forward-chaining at upsert/relink time (§1.2), with
  bi-temporal truth maintenance (retraction on supersede/archive).
- Authorization/threat model before any remote SPARQL/federation/GraphQL (K3).

**P2 (retrieval & query)**
- Deterministic entity/symbol mention linking (§1.8) — additive, but decide
  `MENTIONS` vs. reusing `RELATES_TO`.
- SPARQL 1.2 read tool (§1.4) with query/time/solution budgets; register
  `ns:salience` custom function so SPARQL honors neural gain + tombstones.
- Tantivy lexical (§1.9) **only after** benchmarking the existing exact-name
  pre-pass (`src/store/ladybug.rs:71-143`, F1) — docs falsely claim it is
  unimplemented (`COGNITIVE_ARCHITECTURE.md` §6).
- KGE structural channel (§1.6) as offline experiment, not a duplicate-judge
  replacement (K3).

**P3 (defer / demand-gated)**
- Community detection (§1.7) — UI-first, not default snapshot.
- Federation (§1.11), streaming delta (§1.12), DID (§1.13), ReBAC (§1.14),
  TDB2 (§1.16), RAG chat (§1.17), WASM (§1.15), GraphQL (§1.5),
  GraphRAG full (§1.10), zstd/anomaly (§1.19).

## 4. Adjudicated disagreements

| Disagreement | Argon | K3 | Adjudication |
|---|---|---|---|
| KGE priority | P0 blocker for Laya stub | P3; not a calibrated duplicate judge | **Split.** Resolving the Laya stub is P0 (Prime Directive). KGE as the replacement is P2/P3; a simpler resolution (wire real ONNX or formally retire with extracted memory) may satisfy P0 without KGE. K3 is right that TransE/RotatE link prediction is not a calibrated pairwise textual-equivalence decision. |
| RDF-star provenance (§1.18) | Requires schema migration (not "no migration") | RDF-star alone inadequate; use stable assertion resources/identifiers | **Agree with K3.** RDF-star is syntax, not an assertion model. Merge §1.18 into §1.1 and design assertion identity + provenance (PROV-O-aligned) + valid-time vs transaction-time. |
| Crate strategy | Ban direct `oxirs-core` dep; vendor/extract | Spike first, then decide | **Both.** Spike first (K3), but Argon's caution stands: expect to extract/vendor rather than depend directly, due to ctor/rustls/scirs2. |
| Community detection (§1.7) | P2, gate on summarization | P3, UI experiment | **K3.** P3, UI-first, not default snapshot. |
| Tantivy (§1.9) | P1 | Benchmark existing exact-name prepass first | **K3.** P2, after benchmarking F1. |
| TDB2 (§1.16) | P2, gate on unified query | LadybugDB already has WAL/checkpoint/recovery | **P3**, and reconsider whether TDB2 is needed at all vs. porting hygiene into lbug. Note oxirs-tdb is OxiRS-native TDB2-equivalent, not Jena-file-compatible. |
| Entity linking (§1.8) | P1 | P2 | **P2.** Additive but needs vocabulary decision (MENTIONS vs. RELATES_TO). |
| RRF (§1.10) | Keep out of default search (HIGH) | (not addressed; SPARQL can't replace vector ranking) | **Keep RRF out of default `search_memory`.** If hybrid, use explicit `EvidenceKind` tiers (like `OneHop`/`Governs2hop` in `src/traits.rs:79-88`), not opaque score fusion. |
| GraphQL (§1.5) | Kill | Not worth now | **Kill from core.** If needed, curated versioned read API with pagination (K3). |
| WASM (§1.15) | Blocked by C++ FFI | Defer; browser needs OPFS/IndexedDB/workers/sync | **Defer.** Note oxirs-wasm is in-memory-only, no OWL, no SERVICE, partial SHACL. |
| RAG chat (§1.17) | Kill from daemon | Separate optional client | **Agree.** Agents are the LLM orchestrators (`AGENTS.md` §7). |
| Laya stub (§1.6) | P0 (Prime Directive) | Resolve/retire separately; KGE doesn't fix calibrated dedup | **P0 to resolve/retire the stub; KGE is not the fix.** |

## 5. Additions (missing from the analysis)

1. **Named-graph mapping for 3-tier stratification + ingest generations (Argon).**
   Map `(namespace, tier, active_generation, mesh_origin)` to RDF 1.2 Named
   Graphs (`urn:neurostrata:graph:<namespace>:gen:<N>`). Today
   `src/store/ladybug.rs:1480-1500` filters stale AST generations via
   string-splitting node IDs. Named graphs make generation switching an O(1)
   pointer flip and isolate private vs. mesh records.
2. **Incremental truth maintenance / retraction (Argon + K3).** Materializing
   inferred edges at write time without DRed (Delete-and-Rederive) or
   `valid_to`-scoped inference leaves phantom truths on supersede/archive.
3. **Salience custom function in SPARQL (Argon).** `ns:salience` honoring
   capped logarithmic gain + tombstones.
4. **Assertion identity, not just RDF-star syntax (K3).** Distinct assertion
   events; PROV-O-aligned provenance; valid-time vs transaction-time.
5. **Migration parity: `DEPENDS_ON` missing from export (K3).** Graph export
   (`src/store/ladybug.rs:3029-3093`) emits only 4 of 5 relations.
6. **`update_id` is a random UUID, not monotonic (K3).** `src/synaptic_mesh/record.rs:40-43`
   calls it "monotonic" but it is `Uuid::new_v4()`. Streaming needs real versions.
7. **Derived artifacts excluded from mesh sharing (K3).** KGE vectors, community
   labels, inferred edges must stay local/versioned derived data.
8. **Versioned ontology/SHACL governance (K3).** Lossless custom-metadata
   mapping; single-source-of-truth cutover with shadow reads, parity reports,
   rollback.
9. **Frontend ROI (K3).** Server-side pagination, search, temporal/provenance
   filters, accessible list view in `web-ui` (which currently loads and
   client-filters the whole graph, `web-ui/src/App.tsx:34-76,148-163`) beat
   GraphQL/WASM for value.

## 6. "Do now requires no migration" — corrected

| Item | Claim true? | Why |
|---|---|---|
| §1.18 RDF-star provenance | **False** | LadybugDB relation tables (`src/store/ladybug.rs:1790-1804`) have zero property columns; edges cannot store provenance/timestamps/generation without schema migration or reified nodes. |
| §1.6 KGE | Partial | Needs persisted structural vector + retrain trigger on upsert/relink; sidecar file possible initially. |
| §1.7 Community detection | Partial | Summary rows need derived-row identity, invalidation, retrieval exclusion, lifecycle rules. |
| §1.8 Entity/mention linking | **True** | Additive; scans content against `KnownIds` at upsert/relink; standard edges. |
| §1.9 Tantivy lexical | **True** (sidecar) | Rebuildable sidecar index; but requires backfill, update/tombstone consistency, namespace filtering, deterministic ranking. |

## 7. Factual corrections to `docs/design-oxirs-concepts.md`

1. **`memory-vocabulary.v9.json` does not exist.** `src/traits.rs:22` embeds
   `src/schemas/memory-vocabulary.v3.json`, whose internal `"vocabulary_version"`
   is `9`. Fix references in §1.1 and §5.
2. **Exact-identifier recall is partially implemented.** `src/store/ladybug.rs:71-143`
   contains an exact-name pre-pass (F1). `COGNITIVE_ARCHITECTURE.md` §6 says
   "NOT IMPLEMENTED" — stale. Benchmark before funding Tantivy.
3. **LadybugDB already has WAL/checkpoint/recovery** (`src/store/ladybug.rs:785-1088`),
   weakening §1.16's storage-gap rationale.
4. **`DEPENDS_ON` is absent from graph export** (`src/store/ladybug.rs:3029-3093`)
   — only 4 of 5 relations emitted. Migration-parity gap.
5. **`update_id` is random UUID, not monotonic** (`src/synaptic_mesh/record.rs:40-43`).
6. **`oxirs-rule` is RDFS/OWL RL**, not OWL 2 DL — correct the analysis wording.

## 8. Open questions for owner

1. Does "scrap JSON" mean bespoke metadata only, or also JSON-LD/wire formats?
2. Is LadybugDB intended to remain the vector/operational store permanently?
3. Which reasoning profile and inference-retraction guarantees are required?
4. What remote/multi-user threat model precedes SPARQL and federation?
5. What snapshot latency/token budget and offline-browser requirement justify
   communities or WASM?
6. Crate strategy: spike `oxirs-core`/`oxirs-arq`/`oxirs-shacl`/`oxirs-tdb`
   independently, then extract/vendor rather than depend directly?
7. Laya stub disposition: wire real ONNX, or formally retire with extracted
   memory?

---
*Adjudication performed 2026-10-27. No implementation authorized by this document.*