# Argon evaluation: GLiNER2.5-multi-v1 + Cognee concept transfers

**Date:** 2026-10-27  
**Seat:** Argon (`gemini-arch`, Gemini 4 Argon via Copilot), independent architect; design-only.  
**Task:** `neurostrata-jzyp`  
**Inputs:**
- Model card: https://huggingface.co/fastino/gliner2.5-multi-v1 (GLiNER2.5 Multi, 287M params, mDeBERTa-v3-base, Apache-2.0, ~594 MB FP16, boundary architecture, entities/classification/relations/joint IE/records/span attributes, PyTorch `gliner2[local]`, no published ONNX).
- Prior report: `docs/design-cognee-concepts.md`.
- NeuroStrata source at HEAD `06ff6c0` (detached at `v1.14.3-patch`).

## 1. GLiNER2.5 verdict

**PILOT — strictly scoped to write/ingest/offline enrichment. REJECT for the read/retrieval path and hot dedup path. Prefer an English-only DeBERTa-v3 GLiNER2 checkpoint over `multi-v1` if one exists.**

Rationale: capable zero-shot boundary IE + classification model, but a 594 MB PyTorch-only multilingual encoder with no upstream ONNX export is a poor fit for a pure-Rust, deterministic, single-binary MCP server unless isolated behind an optional ONNX/sidecar enrichment seam.

### Seam-by-seam

| Seam | Value | Cost/latency | Integration | Verdict |
|---|---|---|---|---|
| Markdown/doc entity + relation extraction at ingest (`src/parser/ingest.rs:319-365`) | **High** — `.md` files are currently inert zero-vector stubs (`:520`); extraction could link prose to `GOVERNS`/`RELATES_TO` edges | ~150–600 ms/chunk CPU | Phase 0 Python sidecar; Phase 1 Rust `ort` only if ONNX export is clean | **Pilot first.** Resolve spans against `KnownIds` (`src/store/ladybug.rs:1285`) so no dangling edges. |
| Write-time auto-anchoring + domain suggestion (`handle_add_memory`, `src/server.rs:108-140`) | Medium-high — locations, `metadata.topology`, domain registry | ~80–200 ms/write | In-process `ort` behind `spawn_blocking` + semaphore | **Pilot second.** Advisory only; never silently register domains. |
| Task intake/log distillation (`server.rs:493`, `:417`) | Medium — replaces lexical `task_intent_signals` with zero-shot classification | ~80–150 ms/entry | Background worker on ThalamicBus | Phase 2; only if M2 false-negative rate is high. |
| Dedup candidate judgment (`JudgmentProvider::judge_duplicates`) | **Low / wrong architecture** — GLiNER is not a pairwise cross-encoder | ~300–800 ms per add | N/A | **Reject.** Fix Laya with a real cross-encoder ONNX model. |
| Guard pattern matching (`src/guard/validator.rs`) | Negative — safety gate needs determinism | +100–250 ms/check | N/A | **Reject.** Probabilistic extraction in a security gate invites bypass. |
| Query routing / read path | Negative — violates deterministic retrieval (`COGNITIVE_ARCHITECTURE.md` §6) | +100 ms before embedding | N/A | **Reject.** Exact-identifier routing belongs in deterministic token inspection. |

### Hard constraints

1. **Rust integration.** No official ONNX export. GLiNER2.5 packs task schemas into a structured prompt prefix and performs sparse boundary start/end pairing + constrained decoding in Python. Porting this to Rust + `tokenizers` + `ort` is a multi-week ML-systems project; do not attempt blind before verifying ONNX graph export and INT8 fidelity.
2. **Footprint.** 594 MB FP16 (≈1.1 GB FP32 / ≈290 MB INT8) doubles daemon disk/RAM next to the 523 MB fastembed cache and 2.6 MB Laya model. `mDeBERTa-v3-base` multilingual vocab wastes a large share of parameters on non-English subwords.
3. **Supply chain.** `src/model_downloader.rs` pins single-file SHA-256. Any GLiNER bundle needs a manifest over `model.onnx` + `tokenizer.json` + `config.json` and fail-closed verification.
4. **Do not use for:** `search_memory`, `get_snapshot`, `memories_for_file`, `related`; replacing tree-sitter for source files; `guard_validate`; pairwise duplicate judgment.

## 2. Cognee report adjudication

### Report corrections (Argon)

1. **§1.2 incremental ingest misstates the gap.** `src/parser/ingest.rs` already has source-scoped staged generations (`begin_generation` / `publish_generation` / `abort_generation`, `:664-748`), `file_sha256` on files/markdown (`:334-349`), and `span_sha256` on AST symbols (`:168`, `:422`). What is missing is **carry-forward of unchanged nodes + reuse of existing embeddings** across generations in `run_producer`/`run_consumer`.
2. **§1.3 / §1.8 miss the LadybugDB schema constraint.** Edge tables are property-less (`src/store/ladybug.rs:1616-1627`), and `src/traits.rs:584` asserts exactly four relations. Adding edge properties or a `CALLS` table needs a migration, Cypher updates, and a vocabulary bump to v9.

### Tier 1

| Item | Verdict | Correction |
|---|---|---|
| 1.1 Exact-lexical/phrase recall | **KEEP** | **P0**, S–M. Deterministic pre-pass matching `symbol`, `location`, exact quoted substrings; new `EvidenceKind::LexicalMatch`; never pollute vector scores with magic constants. Closes the #1 documented retrieval defect. |
| 1.2 Incremental re-ingest | **CHANGE** | **P0**, S (not M). Do not bi-temporal-supersede AST symbols (bloats `Memory` with retired code rows). Reuse previous generation's `file_sha256`/`span_sha256` + existing vectors when unchanged. |
| 1.3 Edge evidence/source citations | **CHANGE** | P2, M. Store in `payload.metadata.edge_sources` keyed by `(rel_type, target_id)` instead of migrating property-less `REL TABLE`s on `lbug 0.20.4`. |
| 1.4 Presort + dry-run | **KEEP** | P2, S. Run producer without `begin_generation`/consumer; return `{files_total, files_changed, symbols_to_embed, skipped_unchanged, secret_hits}`. Zero writes. |
| 1.5 Ontology grounding | **KILL** | OWL/RDF is heavy Python baggage. `memory-vocabulary.v3.json` + `domain_registry` already enforce a strict vocabulary. If synonyms matter, add `aliases: HashMap<String,String>` to the registry (~20 lines). |
| 1.6 Node sets/project scoping | **CHANGE** | P2, S. Do not add a parallel `node_set`. Allow `metadata.domain` to be `string | string[]`; query side already accepts both (`src/server.rs:196`). |
| 1.7 Conflict review over dated facts | **CHANGE** | P2, M. Deterministic only: flag active `rule`/`fact` sharing `governs` target + domain with near-duplicate raw distance, or same `(component,target_system)` with differing `channel`/`credential_boundary`. Reuse `is_mesh_conflicting` visibility or add a `disputed` strap to snapshot. |
| 1.8 Deterministic code graph | **CHANGE** | P1, M–L. Avoid a fifth `CALLS` table initially; extend `src/schema.json` tree-sitter queries for imports and resolve via `KnownIds` into `RELATES_TO`/`CONTAINS`, so `related` traverses module dependencies. |

### Tier 2

- **Promote to 1.5 (deterministic, small):** skill gate/procedural auto-surface (P2, S; `get_snapshot` already computes `procedures_due`); watermarks (P2, S; `last_distilled_log_offset` / `last_ingested_generation` on namespace registry).
- **Keep in Tier 2:** session distillation (P3) — do **not** add Redis/SQLite; read `.NeuroStrata/sessions/*` and write `IntakeCandidate`. Feedback weighting (P3) — derive from ThalamicBus lifecycle events (`Superseded` reason = negative; `EXTRACTED_FROM` completion = positive) instead of a new tool agents forget.
- **Kill:** global context index (duplicates snapshot + ancestor traversal); temporal reranking (coding agents rarely ask date-range questions); per-dataset enrichment lock (redundant with `IngestJobs`, `begin_generation`, task claims).

### Missing Cognee concepts worth modeling

1. **Extractor fingerprint on generations.** Hash `(source_content_hash, extractor_version, schema_version)` so a `src/schema.json` change forces re-parse even when `file_sha256` is unchanged.
2. **Dangling/unresolved target quarantine queue.** `UpsertReport` already computes `deferred_edges` (`Missing`, `Ambiguous`, `src/traits.rs:223-234`) but uses it transiently. Persist unresolved edge declarations into an inspectable `doctor`/`task_validate` report (code-rot detection).

## 3. Phased recommendation (Argon)

### Phase 0 — 2–3 day feasibility spike

- **Track A (deterministic, Rust):** benchmark exact-identifier recall@5 on 50 queries against the repo; prototype symbol/substring pre-pass + generation `span_sha256` vector reuse.
- **Track B (GLiNER2.5, standalone):** run `multi-v1` and an English DeBERTa-v3 GLiNER2 checkpoint on `docs/*.md` + `AGENTS.md`; extract `{file_path, symbol, env_var, component}` and `{GOVERNS, RELATES_TO}`; attempt ONNX export + INT8 and load in Rust `ort`.

**Gate to Phase 1:**
- Track A: recall@5 ≥95%, p95 overhead <2 ms, 1-file re-ingest skips ≥98% of symbol embeddings and completes <500 ms.
- Track B: clean ONNX export + INT8 verified in `ort`; INT8 ≤300 MB with real SHA-256; ≤150 ms per 512-token chunk on 4 threads; entity/relation precision ≥80% resolved against `KnownIds`.

**Kill GLiNER2.5 if:** boundary decoder cannot export to static ONNX ops supported by `ort`, OR resolved `GOVERNS` precision <75%, OR CPU latency >500 ms/chunk. Fallback: deterministic backtick path/symbol extraction in markdown (≈80% of value, 0 MB, <1 ms).

### Phase 1 — deterministic core hygiene (no ML)

1. Fix supply-chain bugs first: dummy empty-string SHA-256 pins (`src/embed.rs:62,71`, `src/model_downloader.rs:32`) and the hardcoded `0.75` Laya stub (`src/judgment/laya_onnx.rs:97-120`).
2. Incremental generation reuse in `src/parser/ingest.rs`.
3. Deterministic exact-identifier recall (`EvidenceKind::LexicalMatch`).
4. Ingest `--dry-run` + multi-domain tags.

### Phase 2 — markdown structure + optional local IE (conditional)

1. Deterministic heading/backtick extractor → `CONTAINS`/`RELATES_TO` against `KnownIds`.
2. Optional `--features ie-gliner` `ort` enrichment for offline markdown ingest and `add_memory` metadata pre-fill.

### Do not build

- Python sidecar in standard installs (breaks single-binary reliability).
- RDFLib/OWL ontology resolvers.
- SQLite/Redis session caches.
- Tantivy + RRF (deterministic pre-pass is enough).

## 4. Orchestrator verification of Argon's repo claims

Verified at HEAD `06ff6c0`:

| Claim | Status | Evidence |
|---|---|---|
| Laya ONNX provider returns hardcoded `0.75` whenever `model.onnx` exists | **CONFIRMED** | `src/judgment/laya_onnx.rs:97-120` |
| Default embedder/model-registry SHA-256 pins are the empty-string hash `e3b0c442...` | **CONFIRMED** | `src/embed.rs:62,71`; `src/model_downloader.rs:32` |
| Ingest already has staged generations + `file_sha256` + `span_sha256` | **CONFIRMED** | `src/parser/ingest.rs:163-203`, `:334-349`, `:422`, `:664-748` |
| Four property-less relation tables; vocabulary asserts `rels.len() == 4` | **CONFIRMED** | `src/store/ladybug.rs:1616-1627`; `src/traits.rs:584` |
| Search `domain` accepts `string | string[]` | **CONFIRMED** | `src/server.rs:196` |
| `UpsertReport` computes `deferred_edges` (`Missing`/`Ambiguous`) | **CONFIRMED** | `src/traits.rs:223-234` (as cited by Argon) |

## 5. Open questions for the owner

1. **Binary purity vs sidecar:** is strict single-binary Rust (`ort` only, zero Python runtime) a hard invariant, or is an opt-in Python sidecar acceptable for offline ingest enrichment?
2. **Deterministic markdown first?** Before spending ~594 MB on GLiNER, should we extract backticked paths/symbols deterministically and measure the leftover gap?
3. **Laya disposition:** wire the stubbed `LayaOnnxProvider` to a real cross-encoder ONNX, or formally retire it in favor of raw distance + deterministic conflict heuristics?
4. **Multilingual need:** does any target codebase need non-English docs, or restrict to English-only checkpoints?
5. **Lexical recall surface:** automatic when query tokens match `KnownIds`/quoted strings, or explicit `lexical: true` flag? Argon recommends automatic for `KnownIds`/quoted tokens with `EvidenceKind::LexicalMatch`.

---
*Argon's verdict is advisory; no implementation is authorized by this document. Verification table added by the orchestrator.*
