# Two-seat adjudication: Argon and K3 on GLiNER2.5 and the Cognee transfers

**Date:** 2026-10-27
**Seats:**
- Argon — Gemini 4 Argon (Copilot), `gemini-arch`, session `ses_e82a43a1effeEXCx6QZbNAI2kB`
- K3 — Kimi K3.1 Pro (Copilot), `k3-arch`, session `ses_e829eb5cfffeFnoiXrbzA8oJa3`
**Authority:** owner directive, 2026-10-27 — “get the K3.1 architects opinion on both of your findings and evaluate them as well.”
**Inputs:**
- `docs/design-cognee-concepts.md` (the Cognee transfer report under review)
- `docs/designs/2026-10-27-argon-gliner-cognee-evaluation.md` (Argon’s verdict)
- Model card and files for `fastino/gliner2.5-multi-v1`
- NeuroStrata source at HEAD `06ff6c0` (detached at `v1.14.3-patch`)

## 0. Method and provenance

This was **not** a blind two-seat review. The owner asked K3 to evaluate *both*
findings, so K3 received Argon’s verdict. K3 was instructed to form its own
verdict from primary sources first, then adjudicate the report and Argon. Argon
did not see K3. The orchestrator re-verified every conclusion-changing claim
against source and against the model/export repositories.

**Status of the verdict:** advisory design input. No implementation is
authorized by this document. No Phase 0 tasks were filed; the owner decides
whether to fund the spikes.

## 1. Executive ruling

Both seats converge on the architectural boundary, and the convergence is
strong enough to treat as settled:

- **GLiNER2.5: PILOT, offline/write-side enrichment only. REJECT for the read
  path, hot dedup, guard validation, query routing, and source-code parsing.**
- **Cognee transfers: keep lexical recall and incremental ingest as P0; use
  metadata for edge evidence instead of an lbug migration; kill OWL/RDF
  ontology, generated global summaries, and temporal reranking; treat
  multi-domain tags, dry-run, conflict candidates, and code-graph edges as
  scoped follow-ons.**

Three facts changed the cost calculus, and **both seats missed them**:

1. **An ONNX export of this exact checkpoint already exists** —
   `jugaadsrl/gliner2.5-multi-v1-onnx`, Apache-2.0, opset 18, with FP16 and
   FP32 bundles. It is third-party, not official fastino, but it is real.
2. **A Rust `ort` crate already loads it** — `gliner25-rs` 0.5.6, Apache-2.0,
   floor `ort ^2.0.0-rc.13`; NeuroStrata already locks `ort 2.0.0-rc.13`. The
   crate implements the boundary engine plus entities, relations, and
   classification; it does **not** expose joint IE, records, or span attributes.
3. **The published export is capped at 512 tokens**, not the model card’s
   4096-token window: `boundary_manifest.json` has
   `max_position_embeddings: 512`, `length_buckets: [64,128,256,512]`, and the
   crate’s `extract_long` chunks at 384 words / 64 overlap. Cross-window
   relations are lost.

Consequences:

- Argon’s “multi-week Rust port, no ONNX export” premise is **too pessimistic
  for a Phase 0 spike**, though still right about production risk.
- K3’s “ONNX feasible in principle, not established” is **outdated**: it is
  established by a third party. What is not established is quality, parity,
  footprint, and supply-chain trust.
- Argon’s ~594 MB footprint and K3’s 1.15 GB correction are **both right about
  different artifacts**: the HF checkpoint is 1,149,461,028 bytes FP32; the
  ONNX FP16 bundle is ~590 MB; no INT8 export exists.
- The binding constraint is no longer “can we run it?” It is “does it beat a
  deterministic markdown extractor by enough to justify ~590 MB, a beta
  dependency, and a 512-token window?”

**Orchestrator ruling:** run the deterministic work first; run GLiNER as a
gated, isolated enrichment spike; do not commit to it as a production
dependency until it beats the deterministic baseline and the export can be
pinned and audited.

## 2. GLiNER2.5 — seam-by-seam ruling

| Seam | Argon | K3 | Orchestrator ruling |
|---|---|---|---|
| Markdown/doc entity + relation extraction at ingest | Pilot first; highest value; `.md` rows are zero-vector stubs | Pilot first, but only after a deterministic markdown baseline proves a gap | **Pilot first, review-only edges.** Baseline first; model must beat it. |
| Write-time auto-anchoring + domain suggestion | Pilot second; advisory only | Pilot second; asynchronous prefill, registry authoritative | **Pilot second, shadow mode.** Never silently register domains or write edges. |
| Task intake/log classification | Phase 2 if lexical false-negative rate is high | Shadow only; existing detector is 100% deterministic | **Shadow only.** Emit candidates for comparison; never auto-create tasks. |
| Dedup candidate judgment | Reject: GLiNER is not a pairwise cross-encoder | Reject: wrong model type | **Reject.** Fix or retire Laya instead. |
| Guard pattern matching | Reject: probabilistic gate invites bypass | Reject: safety decisions need determinism | **Reject.** |
| Query routing / read path | Reject: violates deterministic retrieval | Reject: hot latency, unstable behavior | **Reject.** Exact-token inspection belongs in deterministic code. |
| Ontology labeling | Kill OWL; use registry aliases | Annotation only; no strict dropping | **Annotation only, if aliases are needed at all.** |
| Prose → durable memory extraction | Not explicitly ruled | Reviewed pilot; candidates with citations, never automatic truth | **Review-only candidates.** No automatic durable rules. |
| Source-code extraction / secrets / PII | Use tree-sitter and fail-closed scanners | Reject: inferior to deterministic tools | **Reject.** |

### Integration reality

| Item | Finding |
|---|---|
| Official ONNX | None. The official repo has torch export modes and `torch.compile` tests (`tests/models/boundary/test_export_and_compile.py`), not ONNX. |
| Third-party export | `jugaadsrl/gliner2.5-multi-v1-onnx`, Apache-2.0, opset 18. FP16 encoder 556,324,341 B + heads/tokenizer ≈ 590 MB total. FP32 encoder 1,111,055,955 B. No INT8 variant. |
| Rust crate | `gliner25-rs` 0.5.6, Apache-2.0, one author, 288 downloads, first published 2026-08-25. Uses `ort ^2.0.0-rc.13`; NeuroStrata locks `2.0.0-rc.13`. Supports `SchemaTask::{Entities, Relations, Classifications}`; no joint IE/records/span attributes. |
| Window | Export manifest: `max_position_embeddings: 512`; buckets 64–512 tokens. The card’s 4096-token window requires a new export with larger buckets. |
| Checkpoints | English `small` (74M) and `base` (194M) exist, but **no ONNX export for either**. Only `multi-v1` has a published export. |
| Offsets | Rust uses UTF-8 byte offsets; Python uses character indices. Any parity test must compare byte ranges and final spans, not just text. |
| Determinism | ONNX CPU is deterministic only under fixed weights, thread count, and execution provider. Pin all three and compare golden outputs. |
| Supply chain | Current `src/model_downloader.rs:16-23` models one `url`/`sha256`/`path`; this bundle needs a per-file manifest (encoder, heads, classifier, tokenizer, boundary manifest, export revision) with fail-closed verification. |
| Maturity | The export and crate are a reference implementation, not a trusted production dependency. Vendor or reimplement after the spike; do not add a 288-download beta crate to the main binary on faith. |

### Checkpoint ruling

K3’s preference for English `small`/`base` is directionally correct on
footprint and latency, but it conflicts with the only ready-made ONNX path.
For Phase 0, test `multi-v1` first because it is the cheapest route to a
signal. Only if it wins on quality but fails on cost should the team export
`small` or `base` and re-run the gates. Do not choose a checkpoint on paper
before the baseline exists.

## 3. Cognee report adjudication

### Tier 1

| # | Report position | Argon | K3 | Orchestrator ruling |
|---|---|---|---|---|
| 1.1 | Lexical recall, P1/M | Keep, P0/S–M; deterministic pre-pass, `EvidenceKind::LexicalMatch`, no score fusion | Keep, P0; split symbol/path lookup from general phrase search | **Keep, P0.** Split: exact symbol/path lookup (S) first; general phrase scan (M) second. No keyword index, no RRF. |
| 1.2 | Incremental re-ingest, P1/M | Change, P0/S; reuse hashes + vectors, no bi-temporal supersede of AST rows | Change, P0/M–L; cross-generation carry-forward with atomic generations and descriptor compatibility | **Change, P0 but split.** Skip unchanged files + reuse vectors (M); edge carry-forward and generation-safe node reuse (L). Do not supersede AST symbols bi-temporally. |
| 1.3 | Edge evidence, P2/S–M | P2/M; `metadata.edge_sources` keyed by `(rel_type,target_id)` | P1 for generated edges, P2 otherwise; key by stable declaration index + logical target, not generation-scoped IDs | **P2/M.** Store `declaration_index`, `logical_target`, `source_sha`, `span`, `extractor_fingerprint` on the declaring memory. No lbug migration. |
| 1.4 | Presort + dry-run, P2/S | Keep, P2/S; zero writes | Keep, P2/M; no reliable PII claim; no model downloads | **P2/M.** Zero writes, zero downloads. Report files changed, embeddings avoided, secret hits, estimated work. Do not promise PII detection. |
| 1.5 | Ontology grounding, P2/M–L | Kill OWL/RDF; registry aliases | Change: versioned JSON aliases, annotation only, no strict dropping | **Kill OWL/RDF.** Aliases are a small optional registry feature, not an ontology subsystem. |
| 1.6 | Node sets, P2/S | Change to multi-domain array; query side already accepts both | Change, S–M; add path is string-only and registry rejects non-strings | **Change, S–M.** Use `domain: string | string[]`; update add schema (`src/server.rs:133`), registry (`src/registry.rs:81-86`), storage, and query. No parallel `node_set`. |
| 1.7 | Conflict review, P2–P3/M | Change, P2/M; deterministic heuristics | Change, P3; candidates only; near-duplicates are not contradictions | **P3.** Candidate generation + review. Never auto-mark a dispute. Do not reuse SynapticMesh same-id conflict semantics. |
| 1.8 | Code graph, P2/M–L | Change, P1/M–L; extend `RELATES_TO`/`CONTAINS`, no fifth table | **Disagree**: `CONTAINS` is structural, `RELATES_TO` is undirected; neither represents directed calls | **P1 for imports/dependency edges, not calls.** Undirected dependency edges are honest. Directed `CALLS` is a separate vocabulary/schema migration; do not disguise it. |

### Tier 2

| Item | Report | Argon | K3 | Ruling |
|---|---|---|---|---|
| Session distillation | P3/L | P3; no new store; read session logs → `IntakeCandidate` | Keep; offline distillation from logs | **P3; no new SQLite/Redis store.** |
| Feedback weighting | P3/S–M | Derive from lifecycle events, no new tool | Defer until retrieval metrics show need | **Defer.** If built, use existing events only. |
| Global context index | P3/M | Kill | Kill; duplicates snapshot + ancestor traversal | **Kill.** |
| Temporal reranking | P3/M | Kill | Kill; an explicit bi-temporal “as-of” query is more aligned | **Kill the reranker.** Optional `as-of` query is a separate, later idea. |
| Skill gate / procedural surfacing | P3/S | Promote to P2/S | Keep deterministic trigger surfacing | **P2/S, deterministic.** `get_snapshot` already computes `procedures_due`. |
| Watermarks | P3/S | Promote to P2/S; namespace registry | Key by input digest + extractor/schema/embedder fingerprints | **P2/S.** Store `last_ingested_generation` and `last_distilled_log_offset`. |
| Enrichment locks | P3/S | Kill; `IngestJobs` exists | Only when a durable enrichment subsystem exists; `IngestJobs` is in-process only | **Defer.** Do not treat `IngestJobs` as a durable lease. |

### Missing ideas worth modeling

- **Extractor fingerprint on generations** (Argon): hash
  `(source_content_hash, extractor_version, schema_version, embedder_id)` so a
  `src/schema.json` change forces re-parse even when `file_sha256` is unchanged.
  K3 refines this correctly: separate per-file input fingerprints from
  pipeline/extractor fingerprints.
- **Persisted dangling-edge report** (Argon): `UpsertReport.deferred_edges`
  already classifies `Missing`/`Ambiguous` (`src/traits.rs:223-234`) but is
  transient. Persist it for `doctor`/`task_validate` so code-rot is visible.
  K3’s refinement: status and resolution history, not an undifferentiated
  permanent queue.
- **Enrichment state machine + idempotency** (K3): `pending/running/done/failed`,
  with input digest and extractor fingerprint. This is the missing operational
  layer if enrichment is added; it is also the honest answer to “locks.”
- **Graph construction/read separation** (K3): ingestion may use models; reads
  must not. This is already the architecture, but it should be an explicit
  invariant in any enrichment design.
- **Pipeline observability** (K3): counts per stage, skipped/failed items,
  latency, and last-run fingerprints. The current ingest reports generation
  status but not extraction-level telemetry.
- **Deterministic regex routing** (K3): Cognee’s router is three regex rules,
  first match wins, no LLM. NeuroStrata should keep any routing deterministic;
  this is a confirmation, not a new subsystem.
- **Bi-temporal “as-of” query** (K3): a better fit than date-based reranking;
  low priority, but worth recording as the aligned alternative.

### Where the original report was wrong

1. **§1.2 misstated the incremental-ingest gap.** Generations and hashes
   already exist: `file_sha256` (`src/parser/ingest.rs:334-346`), `span_sha256`
   (`:415-423`), staged publish/abort (`:663-747`). The real gap is
   cross-generation carry-forward and embedding reuse. Argon and K3 both
   caught this; the orchestrator accepts the correction.
2. **§1.3 understated the schema constraint.** The four relation tables are
   property-less (`src/store/ladybug.rs:1616-1627`) and the vocabulary asserts
   `rels.len() == 4` (`src/traits.rs:584`). Edge properties require a
   migration; metadata is the right first step.
3. **§1.5 overestimated ontology value.** OWL/RDF is heavy Python baggage;
   the existing `memory-vocabulary.v3.json` plus `domain_registry` already
   enforce a vocabulary. Aliases cover the synonym case.
4. **§1.7 overstated deterministic contradiction detection.** Near-duplicate
   text, shared entities, or differing channels do not prove contradiction.
   Only candidate generation is honest.
5. **§1.8 overstated what existing relations can represent.** `CONTAINS` is
   structural and `RELATES_TO` is undirected; directed calls need a real
   migration. Do not fake them.

## 4. Argon vs K3 — direct rulings

| Argon claim | K3 verdict | Orchestrator ruling | Reason |
|---|---|---|---|
| Reject GLiNER for read path, dedup, guard, routing | Agree | **Agree** | Correct architectural boundary; no dissent. |
| Kill OWL/RDF ontology | Agree | **Agree** | Registry aliases cover synonym canonicalization; strict dropping is dangerous. |
| Kill global context index and temporal reranking | Agree | **Agree** | Generated summaries duplicate grounding; filtering is preferable to reranking. |
| Edge evidence via `metadata.edge_sources` | Partial | **K3 wins on keying** | Use stable declaration index + logical target + source hash; generation-scoped IDs change. |
| Multi-domain arrays instead of `node_set` | Partial | **Direction agreed; effort corrected** | Add path is string-only (`src/server.rs:133`) and registry rejects non-strings (`src/registry.rs:81-86`); S–M, not S. |
| Deterministic conflict heuristics | Partial | **K3 wins on caution** | Candidates only; near-duplicates are not contradictions; never auto-dispute. |
| Code graph via `RELATES_TO`/`CONTAINS`, no `CALLS` | Disagree | **K3 wins on semantics** | Undirected relations cannot faithfully represent directed calls. Use existing relations for imports/deps only; a `CALLS` table is a separate v9 migration. |
| Fix SHA pins and Laya stub before any ML work | Partial | **K3 wins on sequencing** | They are P0 production blockers, not Phase 0 spike blockers. Disable/retire Laya before any dedup claim. |
| Extractor fingerprint + dangling-edge quarantine | Agree with changes | **Adopt with K3’s refinements** | Separate input vs pipeline fingerprints; persist resolution history, not a permanent queue. |
| Phase 0 gate thresholds | Partial | **K3’s structure wins; latency gate re-based** | Add deterministic-baseline lift, parity, INT8 degradation, fixed hardware/corpus, resolved-edge precision. Drop the 300 MB INT8 gate unless an INT8 export is produced. |

Argon’s Phase 0 underestimated incremental-ingest effort as **S**. K3 is right:
generation-safe carry-forward with edge preservation is **M–L**. K3’s critique
of “kill enrichment locks because `IngestJobs` exists” is also right:
`IngestJobs` is in-process and namespace-keyed
(`src/ingest_jobs.rs:130-135`), not a durable lease.

## 5. Revised Phase 0

### Track A — deterministic retrieval/ingest (2–3 days)

- **A1 exact-identifier recall:** prototype symbol/path and quoted-phrase
  matching. Held-out suite of 50 identifier queries from this repo.
  Gate: recall@5 ≥ 95%, no dense-recall regression, p95 overhead < 2 ms on
  named hardware.
- **A2 generation reuse:** unchanged file → zero embedding calls; reuse
  `file_sha256`/`span_sha256` and existing vectors. Gate: ≥ 95% symbol reuse
  across representative commits, zero stale edges, atomic publish parity.

### Track B — deterministic markdown baseline (2 days)

- Heading/backtick/path/symbol extractor over `docs/*.md` and `AGENTS.md`.
- Label a held-out corpus for entities, relations, and resolvable targets.
- This is the baseline GLiNER must beat. If it captures ~80% of the value,
  the model case collapses.

### Track C — GLiNER2.5 spike (3–5 days, isolated)

- **C1:** load `jugaadsrl/gliner2.5-multi-v1-onnx` with `gliner25-rs` in a
  throwaway crate (or Python `gliner2[local]` if crate friction dominates).
  Measure per-document latency, RSS, entity F1, relation precision, and
  resolution rate against `KnownIds`. Do not add the crate to the main binary.
- **C2:** only if C1 shows signal, export `small`/`base` and compare.
- **C3:** optional INT8 quantization of the encoder; measure final-span F1
  delta, not embedding cosine.

### Gates

| Gate | Threshold |
|---|---|
| Entity/relation lift over deterministic baseline | ≥ 10 F1 points |
| Resolved-edge precision, review queue | ≥ 90% |
| Resolved-edge precision, automatic materialization | ≥ 95% |
| UTF-8 offsets | Exact; no dropped/duplicated spans |
| INT8/FP16 degradation | ≤ 2 F1 vs FP32 |
| Footprint | Explicit owner decision; FP16 bundle ≈ 590 MB, no INT8 published |
| Latency | p95 measured on owner hardware; 150 ms/512-token chunk is a target, not a fact |

### Kill criteria

- No meaningful lift over the deterministic baseline.
- ORT parity failure or offset drift.
- Resolved-edge precision < 90%.
- Footprint/RSS unacceptable to the owner.
- Export/crate cannot be pinned, vendored, or audited.
- Cross-window relation loss makes the extraction misleading.

### Phase 1 — deterministic hygiene (no ML)

1. Fix supply-chain bugs: empty-string SHA-256 pins (`src/embed.rs:62,71`,
   `src/model_downloader.rs:32`) and the hardcoded `0.75` Laya stub
   (`src/judgment/laya_onnx.rs:97-120`).
2. Incremental generation reuse in `src/parser/ingest.rs`.
3. Deterministic exact-identifier recall.
4. Ingest `--dry-run` and multi-domain tags.

### Phase 2 — optional local enrichment (conditional)

1. `--features ie-gliner` offline markdown enrichment.
2. Candidate edges with citations, extractor fingerprints, watermarks, and
   resolution history.
3. Shadow write-time suggestions only after measured accepted-suggestion value.

### Do not build

- Python sidecar in standard installs.
- OWL/RDF ontology resolvers or strict dropping.
- SQLite/Redis session stores.
- Tantivy/RRF or any keyword index.
- Generated global summaries or temporal rerankers.
- Automatic dispute verdicts.
- Directed calls disguised as undirected relations.
- GLiNER for read, dedup, guard, routing, source parsing, or secrets.

## 6. Risks and owner decisions

1. **Single-binary invariant:** is strict Rust (`ort` only, no Python at
   runtime) hard, or is an opt-in enrichment feature acceptable? Both seats
   assume single-binary; neither authorizes a sidecar in standard installs.
2. **Graph pollution:** are extracted edges review-only initially, and what
   precision permits automatic materialization? Recommendation: 95% auto,
   90% review.
3. **Footprint/RSS:** is ~590 MB FP16 plus tokenizer acceptable next to the
   existing fastembed cache? If not, the deterministic baseline may be the
   whole answer.
4. **Third-party dependency:** `gliner25-rs` is a two-month-old, 288-download,
   single-author crate. Vendor it, reimplement the narrow path, or reject it?
   Do not add it to the main binary on faith.
5. **Checkpoint:** `multi-v1` has the only ONNX export; English `small`/`base`
   are cheaper but need a new export. Which constraint wins?
6. **Laya:** repair with a real cross-encoder ONNX, or formally retire it?
   The current 0.75 stub is a Prime Directive violation.
7. **Directed `CALLS`:** is a vocabulary-v9 schema migration acceptable when
   code traversal becomes a priority?
8. **Window:** accept the 512-token export, or produce a 4096-token export
   with larger buckets and pay the memory/latency cost?

## 7. Verification table

### K3 claims

| Claim | Status | Evidence |
|---|---|---|
| Task detector is “100% deterministic” | **CONFIRMED** | `src/task/intent.rs:1-5` |
| Retrieval has no LLM/rank fusion/keyword index | **CONFIRMED** | `docs/COGNITIVE_ARCHITECTURE.md:39` |
| Markdown rows are zero-vector `Path: …` stubs | **CONFIRMED** | `src/parser/ingest.rs:505-520` |
| Every symbol reaches `embedder.embed(&summary)` | **CONFIRMED** | `src/parser/ingest.rs:594` |
| Downloader models one `url`/`sha256`/`path` | **CONFIRMED** | `src/model_downloader.rs:16-23` |
| Add-memory `domain` is string-only | **CONFIRMED** | `src/server.rs:133` |
| Registry rejects non-string domains | **CONFIRMED** | `src/registry.rs:81-86` |
| `IngestJobs` is in-process, namespace-keyed | **CONFIRMED** | `src/ingest_jobs.rs:130-135` |
| HF checkpoint has no ONNX files | **CONFIRMED** | HF API file list: safetensors/config/tokenizer only |
| `model.safetensors` is 1,149,461,028 bytes | **CONFIRMED** | HF API `?blobs=true` |
| Upstream has export-oriented tensor code / compile tests | **CONFIRMED, but torch not ONNX** | `tests/models/boundary/test_export_and_compile.py`; official README has no ONNX mention |

### New orchestrator findings (neither seat had them)

| Finding | Status | Evidence |
|---|---|---|
| Exact-model ONNX export exists | **CONFIRMED** | `jugaadsrl/gliner2.5-multi-v1-onnx`, Apache-2.0, opset 18, FP16 encoder 556,324,341 B |
| Rust `ort` crate loads it | **CONFIRMED** | `gliner25-rs` 0.5.6, Apache-2.0, `ort ^2.0.0-rc.13` |
| NeuroStrata already uses the same `ort` floor | **CONFIRMED** | `Cargo.lock`: `ort 2.0.0-rc.13` |
| Crate supports relations/classification, not joint IE/records | **CONFIRMED** | `SchemaTask::{Entities, Relations, Classifications}` in `processor.rs`; no joint/records symbols |
| No INT8 export exists | **CONFIRMED** | Export repo contains only `fp16_25/` and `fp32_25/` |
| No `small`/`base` ONNX export exists | **CONFIRMED** | HF API resolves only `multi-v1`; the other ids return an auth error, not a model |
| Published export is capped at 512 tokens | **CONFIRMED** | `boundary_manifest.json`: `max_position_embeddings: 512`, buckets 64–512 |
| Crate maturity is low | **CONFIRMED** | 288 downloads, first published 2026-08-25, one author |

### Argon claims (re-verified at HEAD)

| Claim | Status | Evidence |
|---|---|---|
| Laya returns hardcoded `0.75` | **CONFIRMED** | `src/judgment/laya_onnx.rs:97-120` |
| Default SHA-256 pins are the empty-string hash | **CONFIRMED** | `src/embed.rs:62,71`; `src/model_downloader.rs:32` |
| Ingest has generations + `file_sha256` + `span_sha256` | **CONFIRMED** | `src/parser/ingest.rs:163-203`, `:334-349`, `:415-423`, `:663-747` |
| Four property-less relation tables; `rels.len() == 4` | **CONFIRMED** | `src/store/ladybug.rs:1616-1627`; `src/traits.rs:584` |
| Search `domain` accepts string or array | **CONFIRMED** | `src/server.rs:196` |
| `UpsertReport` computes `deferred_edges` | **CONFIRMED** | `src/traits.rs:223-234` |

## 8. Bottom line

The two seats agree on the important boundary: **models may enrich the write
side; reads stay deterministic.** The disagreement is about cost and sequence,
and the orchestrator finds K3 closer on both: deterministic baseline first,
review-only edges, stable edge-evidence keys, no fake directed calls, and
stronger Phase 0 gates. The one sequencing divergence is checkpoint order:
K3 prefers English small/base before multi, but only `multi-v1` has a published
ONNX export, so the spike tests `multi-v1` first and revisits that choice only
if it wins on quality but fails on cost.

The one place both seats were wrong is the integration premise. An ONNX export
and a Rust loader already exist, which makes the spike cheaper than either
assumed. That is a reason to run the spike, not a reason to adopt the model.
The 512-token window, the missing INT8 export, the 288-download beta crate, and
the absent small/base exports are the real constraints. Measure against the
deterministic baseline before spending any of them.

---

*Adjudication performed 2026-10-27 by the orchestrator. Argon and K3 verdicts are
advisory; no implementation is authorized by this document.*
