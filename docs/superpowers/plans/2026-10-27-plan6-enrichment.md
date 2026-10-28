# Plan 6: Enrichment Pipeline, Distillation & Signals Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans.

**Goal:** Implement F23 (feedback weighting), F10 (conflict candidates), F12/F14/F24 (enrichment state machine: watermarks, telemetry, locking), F22 (session distillation), F5 (GLiNER feature-gated scaffolding). All warn-first, deterministic, no LLM in core paths.

**Spec:** docs/superpowers/specs/2026-10-27-neurostrata-feature-specs-design.md (§2.4, §3 F5, F10, F12, F14, F22, F23, F24, §5 Plan 6)

**Vetting:** Argon REVISE (2026-10-28), K3 REVISE (2026-10-28). This plan is the full rewrite per both blocking contracts.

## Verified Baseline (already landed — with bugs to fix)

- `src/domain.rs:detect_conflicts` (F10): substring bug (`"no"`/`"not"` false positives), skips `valid_to`, skips recall badge, no disputed metadata.
- `src/store/ladybug.rs`: ignores `feedback_score` in salience (F23); `write_registry` clobbers `enrichment_state`/`doctor_state` (F12/F14/F24).
- `src/task/intake.rs`: IntakeCandidate schema (F22/F5) — migration required (K3). Add candidate_kind, payload, confidence, source_span, extractor_fingerprint columns (see K3 contract). Backward compat: existing rows default candidate_kind=task, payload=null.
- `src/server.rs`: handle_distill bypasses IntakeStore + path containment (J22) (F22).
- `Cargo.toml`: `gliner` feature empty (F5).
- `src/model_downloader.rs`: multi-file bundles exist (F5).
- Enrichment state record: not yet on registry row (F12/F14/F24).

## Global Constraints

- JSON only; no LLM in core paths (GLiNER is optional feature, not core).
- Zero new dependencies; no banned crates (oxirs-core, scirs2, horned-owl).
- Deterministic (sorted outputs, BTreeMap/BTreeSet).
- Warn-first, never reject (enrichment failures warn, never reject writes).
- Backward compatible: registry rows without enrichment_state read cleanly.
- SynapticMesh privacy: enrichment telemetry/feedback stay excluded from projected_metadata.
- Prime Directive: every feature fully implemented, no stubs/deferred.

---

## Task 1: F23 feedback weighting (revised per Argon B4)

**Files:** src/server.rs (tool + handler), src/store/ladybug.rs (salience), tests.

**Existing handler reconciliation (`src/server.rs:501-512, 2501-2529`):**
- `neurostrata_feedback` already registered with required `["id", "namespace", "rating"]`. Plan 6 must keep `namespace` in schema (was omitted in v1 stub).
- Fix `handle_feedback_memory`:
  - Validate rating: finite and in [-1.0, 1.0]; out-of-range or non-finite → error, no write.
  - Reject rating the namespace registry row (`is_registry_row` → error "cannot rate registry row").
  - If `payload.metadata` is null, initialize `{}` before setting feedback_score.
  - Merge feedback_score into existing metadata (not clobber).
  - Store `metadata.feedback_score` (number) on Memory row.

**Salience formula (`src/store/ladybug.rs:240-245`):**
- `const FEEDBACK_RANK_GAIN: f32 = 0.05;` (alongside existing `SOURCE_RANK_GAIN = 0.05` at line 1650).
- `let feedback_gain = FEEDBACK_RANK_GAIN * (feedback_score.clamp(-1.0, 1.0) as f32);`
- When `distance == 0.0` (exact match): `boosted_distance = 0.0` (exact matches stay at 0.0).
- When `distance > 0.0`: `let capped_gain = (gain + source_gain + feedback_gain).clamp(-FEEDBACK_RANK_GAIN.min(0.5 * distance), 0.5 * distance); let boosted_distance = distance - capped_gain;`.

**Tests:** rating stored; salience reflects rating within cap; out-of-range rejected (no write); non-finite rejected; null metadata initialized; registry row rejected; exact match stays 0.0; negative feedback cannot push boosted_distance above 0.5*distance downside guard; deterministic repeated; mesh excludes feedback_score.

## Task 2: F10 conflict candidates

**Files:** src/domain.rs (detect_conflicts), src/store/ladybug.rs (search/snapshot surfacing), src/server.rs (snapshot), tests.

- [ ] Fix `detect_conflicts`:
  - Opposing enforcement: rule A enforcement=ENFORCED vs rule B enforcement=NOT_ENFORCED on same location/topology.
  - Opposing decision: fact A decision.status=accepted vs fact B decision.status ∈ {withdrawn, superseded} OR attempt.outcome=failed on same anchor/topology.
  - High cosine (>= 0.85) + opposite negation/modal tokens ("must" vs "must not", "always" vs "never").
  - Cosine path: write/enrichment path uses LadybugStore array_distance (cosine distance <= 0.15); metadata/text path uses anchor/topology matching + persisted disputed flags.
  - Fix substring bug: use word-boundary tokenization, not substring contains.
  - Include `valid_to`: skip expired memories.
  - Canonical slugs (from Plan 5 F17) for topology matching.
- [ ] Snapshot ordering fix (`src/server.rs:2133-2146`): move detect_conflicts AFTER active-row filtering (valid_to, is_archived, is_mesh_conflicting, !is_registry_row). Pass now: i64.
- [ ] Set `metadata.disputed = true` and `metadata.disputed_with = [other_id, ...]` on both rows (write path, idempotent).
- [ ] Surface in recall (search_memory) and snapshot as warning badge `conflict_candidates: [{id, disputed_with}]`. Never auto-resolve.
- [ ] `disputed: true` does NOT hide the row (unlike is_mesh_conflicting).
- [ ] Tests: synthetic contradictory pair flagged; near-duplicate pair not flagged; expired skipped; disputed rows still returned; deterministic.

## Task 3: F12/F14/F24 enrichment state machine

**Files:** src/registry.rs (state record + read-merge-write), src/store/ladybug.rs (read/write), src/server.rs (tool/doctor), src/main.rs (run_doctor), src/task/gate.rs (task_validate), tests.

### State record schema (on registry row metadata key `enrichment_state`):
```json
{
  "state": "pending" | "running" | "done" | "failed",
  "watermark": { "last_enriched_at": string | null, "enriched_count": number, "extractor_fingerprint": string, "source_digest": string | null },
  "lock_holder": string | null,
  "lock_acquired_at": string | null,
  "rerun_passes": [string],
  "telemetry": {
    "stages": [ { "name": string, "count": number, "failures": number, "latency_ms": number, "fingerprint": string, "last_error": string | null } ],
    "totals": { "count": number, "failures": number, "latency_ms": number }
  }
}
```

**Replace dead stubs:** src/registry.rs:560-577 (`enrichment_state` returning `{"state":"pending","watermark":0,...}` and `enrichment_state_record(..., watermark: u64, ...)`) must be replaced with the structured schema above. Backward compat: legacy rows without enrichment_state → default pending with null watermark.

**Files:** src/registry.rs (state record + read-merge-write), src/store/ladybug.rs (read/write), src/server.rs (tool/doctor), src/main.rs (run_doctor), src/task/gate.rs (task_validate), tests.

- [ ] **Fix write_registry clobber:** read-merge-write preserving enrichment_state, doctor_state, project_root, domain entries. Never overwrite enrichment_state on unrelated writes.
- [ ] **Locking (F24):** `enrichment_start(namespace, holder_id)`:
  - If state != running or lock stale (> timeout, e.g., 30min) → acquire: set state=running, lock_holder=holder_id, lock_acquired_at=now. Return `{ acquired: true }`.
  - If state=running and lock fresh → return `{ acquired: false, lock_held: true, holder: lock_holder, rerun_passes: [...] }` (rerun passes = stages to re-run).
  - Follow task-claim exclusivity pattern (stale takeover after timeout).
- [ ] **Watermark (F12):** `enrichment_commit(namespace, watermark, telemetry)`:
  - Set state=done, watermark, telemetry. Clear lock.
  - Skip work already covered by watermark (extractor_fingerprint + enriched_count).
  - Second run reports zero new work if watermark matches.
- [ ] **Failure (F14):** `enrichment_fail(namespace, stage, error)` → state=failed, telemetry stage failure++, clear lock (or keep lock for retry? Decision: clear lock, record failure). Telemetry includes error string per stage.
- [ ] **Doctor exposure:** doctor + task_validate surface `enrichment_state` summary (state, watermark, telemetry totals). Warn-first (failed state → warning, not rejection).
- [ ] **Determinism:** telemetry stages sorted by name; totals reconcile (sum of stage counts = totals.count).
- [ ] Tests: lock acquire/contention/stale takeover; watermark skip; telemetry reconcile; write_registry preserves state; doctor surfaces; concurrent two callers → one holder + one lock_held.

## Task 4: F22 session distillation (revised per Argon B1)

**Files:** src/server.rs (handle_distill), src/task/intake.rs (IntakeStore), src/store/ladybug.rs (IntakeCandidate write via store), tests.

**IntakeStore contract:**
- `handle_distill` signature: `async fn handle_distill(arguments: Value, store: Arc<dyn VectorStore>) -> String` (currently missing store param).
- Route candidate creation through IntakeStore (not direct bypass).
- When `request.title.is_some()`, `create_candidates_from_intents` MUST honor explicit `request.title` and `request.description` directly (1 receipt -> 1 deterministic candidate) instead of re-splitting via M2 `process_intake_text`.
- Standardize title prefixes:
  - F22 rule preference: source_kind = Conversation, title = "[rule_candidate] <summary>"
  - F22 task action item: source_kind = Conversation, title = "[<marker>] <summary>"
  - F5 GLiNER relation/entity: source_kind = Tool, title = "[gliner:<rel_or_entity>] <source> -> <target>"
- IntakeCandidate table migration required (K3): add candidate_kind, payload, confidence, source_span, extractor_fingerprint columns. Backward compat: existing rows default candidate_kind=task, payload=null.

**Path containment (J22):** validate input path is within allowed roots (`.NeuroStrata/sessions/*` or configured roots); reject traversal (`..`, absolute outside roots).

**Behavior:**
- `distill` command scans `neurostrata_append_log` output (session logs) and proposes candidate rules/tasks.
- Candidates are review-only: write to IntakeCandidate with review_state=pending, disposition=pending. Never auto-create Memory or Task.
- No separate session store; uses existing logs.
- Heuristic extraction: stated preferences, constraints, decisions -> candidate rules; action items -> candidate tasks. Deterministic (sorted, dedup by digest).
- Dry-run option: report proposed candidates without writing.

**Tests:** distill on session log produces >=1 candidate; candidates pending review; no Memory/Task auto-created; path containment rejects traversal; deterministic repeated.

## Task 5: F5 GLiNER (feature-gated) (revised per Argon B5)

**Files:** Cargo.toml (feature), src/enrichment (new module, gated), src/model_downloader.rs (bundle verify), src/task/intake.rs (candidate write), tests.

**Cargo feature:** `gliner = ["dep:ort", "dep:tokenizers", "dep:ndarray"]`. Cargo.lock already resolves ort=2.0.0-rc.13, tokenizers=0.23.2, ndarray=0.17.2 transitively via fastembed. Zero new external crates. Remove "check Cargo.toml" open question.

**Model bundle verification:** `src/model_downloader.rs:133-151` currently ignores `entry.files` and only fetches/verifies a single file (`entry.path`). Task 5 must update `fetch_from_registry` (or add `verify_bundle_files`) so each required file in a GLiNER bundle (`model.onnx`, `tokenizer.json`) has its SHA-256 verified fail-closed before session initialization. No partial bundle accepted.

**Behavior:**
- Opt-in enrichment stage using GLiNER2.5 via Rust ONNX path (feature-gated: `--features gliner`).
- Chunk prose at <= 384 tokens (measured by tokenizer, not words) with 64-token overlap to respect 512-token ONNX ceiling. Tokenization is authoritative; word-count is a fallback estimate only.
- Run inference in `spawn_blocking` gated by bounded `Semaphore` (max concurrency configurable, default 2).
- Output review-only: write candidates to IntakeCandidate (not Memory), or hard `is_pending_review` exclusion across all read queries. Decision: write to IntakeCandidate (consistent with F22). Title prefix `[gliner:<rel_or_entity>] <source> -> <target>`, source_kind=Tool.
- Auto-materialize only when measured precision bar met (configurable, default 0.9). Default: no auto-materialize (review-only). Auto-materialize behind explicit config flag + precision gate.
- Never runs in search, dedup, guard, or routing (architectural boundary: enrichment module only called by explicit enrich command).
- Dry-run reports proposed edges without writing.

**Tests (feature-gated, ignored by default):** chunking boundaries, semaphore bounds, candidate output, dry-run no writes, precision gate, bundle verification fail-closed.

## Task 6: Cross-cutting

- [ ] SynapticMesh: enrichment_state, feedback_score, disputed excluded from projected_metadata (privacy).
- [ ] doctor + task_validate surface all new diagnostics (conflict_candidates, enrichment_state, feedback summary) warn-first.
- [ ] Gate never blocks on these (warn-only).

## Test Fixtures (deterministic expected outputs)

### F23 feedback
- Fixture F23-1: upsert memory m1; call feedback {id:m1, namespace:ns, rating:0.5} → metadata.feedback_score=0.5; salience boosted_distance = distance - 0.025 (gain 0.025) for distance>0.
- Fixture F23-2: rating 1.5 → error, no write, metadata unchanged.
- Fixture F23-3: rating NaN → error, no write.
- Fixture F23-4: rating registry row → error "cannot rate registry row".
- Fixture F23-5: null metadata row → feedback initializes metadata={"feedback_score":0.3}.
- Fixture F23-6: exact match distance=0.0 with rating -1.0 → boosted_distance stays 0.0.
- Fixture F23-7: mesh projected_metadata excludes feedback_score.

### F10 conflict
- Fixture F10-1: two rules same location, enforcement ENFORCED vs NOT_ENFORCED → both metadata.disputed=true, disputed_with=[other]. Badge in recall+snapshot.
- Fixture F10-2: near-duplicate pair (cosine 0.9, no opposing enforcement/decision) → not flagged.
- Fixture F10-3: expired memory (valid_to past) → skipped in detect_conflicts.
- Fixture F10-4: disputed row still returned in search (not hidden).
- Fixture F10-5: word-boundary: "must" vs "mustard" → not flagged (no false positive).
- Fixture F10-6: snapshot conflict detection runs after valid_to filtering.

### F12/F14/F24 enrichment
- Fixture E1: first enrichment_start → acquired=true, state=running, lock_holder=caller.
- Fixture E2: second concurrent enrichment_start → acquired=false, lock_held=true, holder=caller1, rerun_passes=[...].
- Fixture E3: stale lock (>30min) → takeover, acquired=true.
- Fixture E4: commit with watermark W → state=done, watermark=W, lock cleared.
- Fixture E5: second run with same watermark → zero new work.
- Fixture E6: fail stage "gliner" with error "timeout" → state=failed, stage last_error="timeout", failures=1.
- Fixture E7: telemetry totals reconcile (sum stage counts = totals.count).
- Fixture E8: write_registry preserves enrichment_state (read-merge-write).
- Fixture E9: legacy row without enrichment_state → default pending, null watermark.

### F22 distillation
- Fixture D1: distill on session log with "I prefer X" → >=1 IntakeCandidate with title "[rule_candidate] ...", review_state=pending, disposition=pending.
- Fixture D2: no Memory or Task auto-created.
- Fixture D3: path traversal "../../etc/passwd" → rejected (path containment).
- Fixture D4: dry_run=true → candidates reported, none written.
- Fixture D5: deterministic repeated → identical candidate set.

### F5 GLiNER
- Fixture G1: feature disabled → enrich command reports "gliner feature not enabled", no panic.
- Fixture G2: chunking 400-token input → 2 chunks (384 + 80 with 64 overlap).
- Fixture G3: dry_run → proposed edges reported, no IntakeCandidate written.
- Fixture G4: bundle missing tokenizer.json → fail-closed, no session init.

## Library Fit & Constraints

- Zero new dependencies (gliner uses existing ONNX deps behind feature gate; if missing, add only behind feature).
- No banned crates.
- Deterministic.
- Warn-first.

## Checkable Acceptance Criteria

- [ ] AC1 (F23): feedback stored; salience reflects rating within cap (FEEDBACK_RANK_GAIN=0.05, clamped formula); out-of-range/non-finite rejected without write; null metadata initialized; registry row rejected; exact match stays 0.0; mesh excludes.
- [ ] AC2 (F10): contradictory pair flagged; near-duplicate not; expired skipped; disputed rows returned; badge in recall/snapshot; word-boundary no false positives; snapshot ordering after valid_to filter; cosine path documented.
- [ ] AC3 (F12): second run reports zero new work.
- [ ] AC4 (F14): failed stage reports error+count (last_error); totals reconcile; sorted.
- [ ] AC5 (F24): two concurrent calls → one holder + one lock_held; stale takeover.
- [ ] AC6 (F22): distill produces >=1 candidate; review-only; path containment; deterministic; IntakeStore honors explicit title.
- [ ] AC7 (F5): feature-gated; chunking; semaphore; candidate output; dry-run; never in search/dedup/guard/routing; bundle verified fail-closed.
- [ ] AC8 (cross-cutting): write_registry preserves enrichment_state; doctor/task_validate surface diagnostics warn-first; gate never blocks.
- [ ] AC9 (no open questions): IntakeCandidate migration note resolved; Cargo.toml feature specified against existing Cargo.lock; dead stubs replaced.

## Cross-Plan Boundaries

- Plan 6 consumes Plan 5 canonical slugs (F17/C4) and concept_iri for F10 conflict matching. Does not re-canonicalize.
- Plan 6 consumes Plan 4 edge_sources for provenance in conflict detection (optional).
- Plan 5 F10 interface: conflict matching uses metadata.topology.component/target_system (canonical).

## Execution Handoff

Execute natively after Argon+K3 vetting.

## Contract Specifications (K3 v2 required additions)

### K1 — Enrichment orchestrator (F12/F14/F24)
- **Entry point:** MCP tool `neurostrata_enrich` + CLI `neurostrata-mcp enrich <namespace> [--stages ...] [--dry-run]`. Signature: `{ namespace, holder_id, stages?: string[], dry_run?: bool }`.
- **Stage order (deterministic):** (1) conflict_pass (F10), (2) distillation (F22), (3) gliner (F5, feature-gated), (4) watermark_commit.
- **Flow:** acquire lock → snapshot cursor → execute stages in order (each stage: run → persist outputs → record telemetry) → commit watermark → release lock. On stage failure: record last_error, leave watermark unchanged, release/fence lock, warn.
- **Cursor:** `{ last_enriched_at: ISO8601|null, enriched_count: number, extractor_fingerprint: string, source_digest: string }`. Resumable: re-run skips work covered by cursor (fingerprint + count + digest match).
- **Fingerprint change:** if extractor_fingerprint differs from stored, reset enriched_count=0 and reprocess (full re-enrichment).
- **Durable-success-before-commit:** stage outputs persisted before watermark commit; commit is atomic (single registry write).
- **Fencing:** commit/fail require `lock_holder == current holder` and `lock_acquired_at` matches; stale holder commit rejected (error "lock lost").

### K2 — Registry concurrency (F12/F14/F24)
- **Per-namespace serialization:** all registry writers (enrichment, cache_project_root, domain writes, write_registry) share a per-namespace async mutex (tokio::sync::Mutex keyed by namespace). No independent read-merge-upserts.
- **CAS:** registry row has `cas_version` (u64). Write: read current cas_version, merge, upsert with `WHERE cas_version = expected`; on mismatch → retry (max 3) or error "concurrent modification".
- **Lease renewal:** lock holder renews lease every 5min (heartbeat) during long runs; if renewal fails, abort and release.
- **Stale takeover:** lock older than 30min → new holder takes over (fencing invalidates old holder).
- **rerun_passes production/consumption:** on lock contention, return `rerun_passes` = list of stages not yet completed (state=running → all remaining; state=failed → failed stage + downstream). Consumer re-runs those stages on next acquire. (ns-kqpl)
- **Idempotent partial retry:** re-running a completed stage produces identical outputs (no duplicate candidates, no double-counted telemetry).

### K3 — Candidate schema extension (F22/F5)
- **Version/extend IntakeCandidate:** add fields `candidate_kind: enum { rule, task, entity, relation }`, `payload: Value` (typed per kind), `confidence: f64 | null`, `source_span: { file?, start_line?, end_line?, excerpt }`, `extractor_fingerprint: string | null`.
- **Resolution contract:** accepting a candidate creates a Memory (rule/fact) or Task per candidate_kind; links candidate.task_id; sets disposition=created. Rejecting sets disposition=dismissed. No auto-creation without explicit accept.
- **Store migration:** add columns to IntakeCandidate table (candidate_kind, payload, confidence, source_span, extractor_fingerprint). Backward compat: existing rows default candidate_kind=task, payload=null.

### K4 — GLiNER bundle schema (F5)
- **Chunking:** use tokenizer lengths (not word counts) for <=384 tokens with 64-token overlap. Deduplicate entities/relations across overlap.
- **Bundle schema:** `ModelEntry.files: Option<Vec<{ path, sha256, size }>>` (per-file hashes). Update `fetch_from_registry` to verify each file's SHA-256 fail-closed. No partial bundle accepted (atomic: all files verified before session init).
- **Inference:** inputs = chunk text; outputs = entities/relations with spans. Provenance: candidate.source_span = { file, start_line, end_line, excerpt }; candidate.extractor_fingerprint = model hash + tokenizer hash.
- **Precision gate:** 50-doc corpus evaluation; auto-materialize only if precision >= 0.9 (configurable). Default review-only.
- **Feature-off:** `#[cfg(feature = "gliner")]` gates module; default build excludes it; tests gated with `#[cfg(feature = "gliner")]`.

### K5 — F10 lifecycle (F10)
- **Cosine path:** `detect_conflicts` takes `&[SearchResult]` (no embedding). Pairwise cosine candidates come from a store query `find_similar_pairs(threshold=0.85)` returning `Vec<(id_a, id_b, cosine)>` sorted by (cosine desc, id_a asc, id_b asc), capped at 1000 pairs. detect_conflicts merges with metadata/anchor candidates.
- **Clearing disputed:** on re-enrichment or on row update, recompute conflicts; if a pair no longer conflicts, clear `disputed`/`disputed_with` on both rows (remove from arrays). Expired counterpart → clear.
- **Active filters (every path):** exclude archived, retired, mesh_conflicting, registry rows; require valid_to not expired.
- **Read-only:** recall/snapshot detect_conflicts does not mutate rows; only enrichment write path stamps disputed.
- **Badge shape (stable):** `conflict_candidates: [{ id: string, disputed_with: string[] }]` sorted by id asc.
- **Symmetric write failure:** if stamping disputed fails on one row, rollback (neither stamped), warn.

### K6 — F22 data flow (F22)
- **Arguments:** `neurostrata_distill { namespace, path?, roots?: string[], dry_run?: bool }`. Default path = latest session log (current.md then most recent session-*.md by mtime).
- **Roots:** configured roots (default `.NeuroStrata/sessions/`). Canonicalize (resolve symlinks); reject paths outside roots or with `..`.
- **Missing/unreadable:** warn with stable code, skip file, continue. Empty result → no candidates, no error.
- **Extraction rules (deterministic):** regex-based lexical markers (preference, constraint, decision, action item) → candidate. Sorted by (source_file, line_number, marker).
- **Digest/stable IDs:** candidate ID = sha256(namespace + source_file + line_number + marker + excerpt) truncated to 16 hex. Receipt ID = sha256(aggregate log digest). Re-running on unchanged logs → identical candidate IDs (idempotent, no duplicates).
- **Idempotent receipt:** MERGE by digest; if exists, read back (do not re-timestamp). `create_receipt` must return existing receipt on duplicate digest.
- **No recall/mesh leakage:** candidates stay in IntakeCandidate table; never in search_memory or mesh.

### K7 — F23 write race (F23)
- **Write race fix:** per-ID serialization via per-memory async mutex OR store CAS (cas_version on Memory row). Use per-memory mutex (simpler, matches enrichment per-namespace pattern). Contention test: concurrent feedback + metadata update → both applied, no clobber.
- **Exact assertions:** with other gains controlled (gain=0, source_gain=0), distance=1.0:
  - rating -1.0 → feedback_gain=-0.05 → capped_gain=-0.05 (clamped to [-0.05, 0.5]) → boosted_distance = 1.0 - (-0.05) = 1.05.
  - rating 0 → feedback_gain=0 → capped_gain=0 → boosted_distance = 1.00.
  - rating +1.0 → feedback_gain=+0.05 → capped_gain=+0.05 → boosted_distance = 1.0 - 0.05 = 0.95.
  - Small-distance cap case: distance=0.05, rating +1.0 → feedback_gain=+0.05 clamped to 0.5*distance=0.025 → boosted_distance = 0.05 - 0.025 = 0.025.

### K8 — Warn-first/telemetry (F14)
- **Structured warnings:** doctor + task_validate + logs surface `enrichment_warnings: [{ namespace, stage, reason, code }]` sorted by (namespace, stage, reason). reason enum: stage_failed, lock_contention, watermark_stale, fingerprint_changed.
- **Sanitized errors:** last_error stored as sanitized string (no paths/PII beyond configured roots); full error to logs only.
- **Explicit zeroes:** all counts default 0 (not null/absent).
- **Cumulative vs last-run:** telemetry.totals = last-run totals; cumulative stored separately as `telemetry_cumulative` (optional, default absent). Default: last-run only.
- **Fingerprint contents:** sha256 of (extractor name + version + config hash).
- **Reconciliation:** totals.count = sum stage.count; totals.failures = sum stage.failures; totals.latency_ms = sum stage.latency_ms.
- **Failed stage:** preserves prior watermark (unchanged), releases/fences lock, warn-only at gate (never blocks push).
- **Tests:** task_validate surfaces enrichment warnings; gate remains green; doctor + logs agree.

## Updated Acceptance Criteria (K3 additions)
- [ ] AC10 (K1): orchestrator entry point + stage order + flow + cursor + fingerprint change + fencing.
- [ ] AC11 (K2): per-namespace serialization + CAS + lease renewal + stale takeover + rerun_passes + idempotent retry.
- [ ] AC12 (K3): candidate schema extension + resolution contract + migration.
- [ ] AC13 (K4): tokenizer chunking + bundle per-file hashes + inference provenance + precision gate + feature-off.
- [ ] AC14 (K5): cosine path + clearing disputed + active filters + read-only + badge shape + symmetric failure.
- [ ] AC15 (K6): distill args/roots/symlink/missing files/extraction/digest/stable IDs/idempotent receipt.
- [ ] AC16 (K7): write race fix + exact distance deltas.
- [ ] AC17 (K8): structured warnings + sanitized errors + zeroes + cumulative/last-run + fingerprint + reconciliation + failed-stage preservation + gate tests.
