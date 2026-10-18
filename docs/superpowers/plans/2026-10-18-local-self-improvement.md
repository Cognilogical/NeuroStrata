# Local Self-Improvement Learning Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` or `superpowers:executing-plans` to
> implement this plan task-by-task. Each task uses checkbox steps.

**Goal:** Add a local experience and training pipeline, then ship Laya as the
first evidence-gated learner for memory deduplication.

**Architecture:** Record ordinary operations across memory, retrieval,
procedures, and tasks in a local append-only observation stream. Build separate,
provenance-tagged datasets per capability. The next release activates this
shared capture foundation and Laya as the first dedup learner; other learners
reuse the foundation in follow-up plans after their label and evaluation
contracts are established.

**Tech Stack:** Rust 2021, Tokio, Axum, LadybugDB, JSONL, Laya Python
`laya-train`, Laya Rust `laya-onnx`/ONNX Runtime.

**Spec:** `docs/superpowers/specs/2026-10-18-local-self-improvement-design.md`

## Global Constraints

- **Release gate:** Do not begin implementation until the owner confirms the
  current release feature set is complete and opens this next-release plan.
- The release captures experience across domains; only the dedup learner is
  promoted in this plan. Retrieval/procedure/task learners need follow-up plans.
- Keep the full learning path local; no Jev teacher or remote fallback.
- Select Jev only explicitly; a configured API key alone does not select it.
- Do not add SQLite. Persist observations outside LadybugDB as append-only JSONL.
- Dedup distances use unboosted raw distance, lower-is-closer, scoped by
  embedder; never reuse the current `0.85` threshold as cosine.
- Exact canonical duplicates may reuse an existing extracted memory. Statistical
  model scores never auto-link memories; inline task completion must not be
  blocked by a non-canonical match.
- Silence, retrieval exposure, task completion, and single graph links are not
  Gold labels. Keep source tier and derivation version on every derived label.
- Train with documented upstream Laya features only in V1. The Python-to-Rust
  ONNX export/parity spike gates automatic model training and promotion.
- Run ONNX inference via `spawn_blocking` with bounded concurrency. Model,
  trainer, or observation failures must not break memory writes or task
  completion.
- Never promote a model from synthetic tests or inferred-consensus agreement
  alone. Absolute accuracy claims require the fixed, independently adjudicated
  Gold benchmark.
- Preserve client-agnostic core behavior. Correlation travels only through
  NeuroStrata's internal stdio-to-daemon proxy; do not add client tool fields.

## Review Focus

1. **Distance semantics:** `SearchResult.score` is boosted distance, lower is
   closer; assert raw query ordering and invariance to usage/provenance boosts.
2. **Lock 2:** Exact extraction reuse must create `EXTRACTED_FROM`; every
   non-canonical model match on inline `task_complete` must allow completion.
3. **Temporal/correlation joins:** A task can finish in the same call as inline
   extraction; link later events by namespace and memory IDs after task/session
   completion.
4. **Label leakage:** No connected component or Gold calibration/evaluation pair
   may cross splits; inferred labels never enter Gold calibration/evaluation.
5. **Runtime/export failure:** Missing Python, invalid ONNX, timeout, or parity
   mismatch must preserve the incumbent and continue baseline writes.

---

## File Map

| File | Responsibility |
|---|---|
| `src/traits.rs` | `RawDistanceQuery`, `RawDistanceCandidate`, and the raw-distance store contract. |
| `src/store/ladybug.rs` | Unboosted nearest-candidate query and type/lifecycle filtering. |
| `src/judgment/canonical.rs` | Stable exact-canonical identity and memory-type identity rules. |
| `src/judgment/policy.rs` | Exact, distance-band, and provider-confidence decision policy. |
| `src/judgment/mod.rs` | Candidate judgment/provider boundary and policy decision types. |
| `src/judgment/laya_onnx.rs` | Local Laya inference provider behind the learning feature. |
| `src/judgment/typesafe_jev.rs` | Optional explicit Jev `/v1/systemone` adapter. |
| `src/learning/` | Observation records, recorder, blob store, outcome joins, datasets, trainer, model registry, and coordinator. |
| `src/server.rs` | Internal connection ID, `add_memory` observation/decision flow, and hint response. |
| `src/daemon.rs` | Local recorder/coordinator wiring; remove API-key-only enable gate. |
| `src/task/mod.rs` | Inline extraction reuse/link behavior and task outcome recording. |
| `src/events/` | Existing mutation bus subscriber for local learning outcomes. |
| `src/config.rs` | Provider mode, key environment reference, learning runtime/config defaults. |
| `src/main.rs` | Optional `learn` status/control and dataset/evaluation subcommands. |
| `src/setup.rs` | Install/manage local training runtime without blocking daemon startup. |
| `Cargo.toml`, `Cargo.lock` | Direct stable-hash dependency and optional Laya inference feature. |
| `tests/fixtures/learning/` | Fixed bridge fixture and component-disjoint Gold calibration/evaluation cases. |

The spec's future retrieval/procedure/task learner policies are not implemented
by this first plan. This release records their ordinary-use events in the common
observation envelope; each active learner still needs its own follow-up plan.

## Task 1: Verify the Laya Training-to-Rust Inference Bridge

**Files:**
- Create: `tests/fixtures/learning/laya_bridge_train.jsonl`
- Create: `tests/fixtures/learning/laya_bridge_eval.jsonl`
- Create: `scripts/learning/laya_bridge_probe.py`
- Create: `examples/laya_onnx_bridge_probe.rs`
- Modify: `Cargo.toml` and `Cargo.lock` (dev-only `laya-onnx` dependency for the
  probe; do not add the production feature until the spike passes)
- Probe: the normal installer must be able to provision the local Python
  training runtime without manual user setup; no product runtime code until
  this packaging gate passes.

**Interfaces:**
- Consumes: approved design spec, upstream `laya-train`, upstream ONNX exporter,
  Rust `laya-onnx` crate.
- Produces: a checked report showing whether a stock fine-tuned checkpoint,
  tokenizer, and calibration configuration load in Rust and preserve predictions.

- [ ] **Step 1: Add tiny train and evaluation fixtures** with the same `state`,
  `questions`, and soft `expected` schema used by the Rust dataset builder.
  Include exact pairs, controlled paraphrases, and hard negatives; no production
  memory data and no train/eval pair overlap.
- [ ] **Step 2: Add a failing bridge contract test** that expects a trained
  checkpoint directory, ONNX bundle, tokenizer, calibration metadata, and
  fixed-input predictions. It must fail with a specific missing-artifact
  message before a bridge is supplied.
- [ ] **Step 3: Prototype that normal setup can provision the local Python
  runtime and Laya training package without a manual user step.** Use a temporary
  bootstrap harness; do not add product runtime code until the bridge gate passes.
  Provisioning must not block daemon startup. Stop for design review if any
  supported release platform cannot do this locally.
- [ ] **Step 4: Write the Python probe** to invoke the documented
  `laya-train --loss soft-ce --freeze-encoder` flow on the fixture, then use the
  upstream-supported export path. Keep generated artifacts in a temporary
  directory and leave no model files in the repository.
- [ ] **Step 5: Load the exported bundle in the Rust example** and record the
  Python and Rust `noul` probabilities for every fixture case.
- [ ] **Step 6: Run the bridge probe.**

Run: `python3 scripts/learning/laya_bridge_probe.py`

Expected: exit 0 only when Rust loads the exported bundle and maximum absolute
probability difference is `<= 1e-4` (provisional bridge tolerance from Argon's
review). Otherwise exit nonzero with the exact unsupported format/operator or
mismatch. **If this task fails, stop Tasks 7–9
and return an architecture blocker; Tasks 2–6 may still ship. Do not silently
switch to remote Jev or assume a Python sidecar.**

- [ ] **Step 7: Commit the probe and result report.**

## Task 2: Add an Unboosted Raw-Distance Candidate API

**Files:**
- Modify: `src/traits.rs`
- Modify: `src/store/ladybug.rs`
- Modify: `src/events/bus.rs` (`StoreNotAttached`, `RecordingStore` test stores)
- Modify: `src/task/mod.rs` (`ListHidingStore` test store)

**Interfaces:**
- Consumes: `MemoryPayload`, current `VectorStore`, approved candidate eligibility
  rules.
- Produces:

```rust
pub struct RawDistanceCandidate {
    pub id: String,
    pub raw_distance: f32,
    pub payload: MemoryPayload,
}

pub enum DedupCandidateScope {
    Domain,
    Procedure { trigger: String },
}

pub struct RawDistanceQuery {
    pub limit: usize,
    pub scope: DedupCandidateScope,
}

async fn nearest_by_raw_distance(
    &self,
    namespace: &str,
    vector: Vec<f32>,
    query: RawDistanceQuery,
) -> Result<Vec<RawDistanceCandidate>>;
```

`Domain` excludes `directory`, `file`, `markdown`, `freshness_flag`,
`guard_event`, `task`, `procedure`, `symbol`, `code_ast`, and
`user_id == "auto-ingestor"`. `Procedure { trigger }` returns only active,
unspent procedures with that exact trigger.

- [ ] **Step 1: Add failing `VectorStore` contract tests** for ascending raw
  distance and excluded row types.
- [ ] **Step 2: Add the method to `VectorStore` and every implementation.** Test
  fakes return fixtures and do no hidden ranking.
- [ ] **Step 3: Implement the LadybugDB query** using `array_distance` ordered
  ascending before any usage/source boost or GraphRAG expansion. Filter archived,
  retired, expired, `auto-ingestor`, `directory`, `file`, `markdown`,
  `freshness_flag`, `guard_event`, `task`, `symbol`, and `code_ast` rows.
- [ ] **Step 4: Implement `Procedure` scope**: require `memory_type == "procedure"`,
  matching `metadata.trigger`, not lapsed, and not spent (`remaining_fires != 0`).
  Domain scope excludes procedures and admits other active non-structural domain
  memory types.
- [ ] **Step 5: Add regression tests**:
  `raw_distance_is_ascending_and_unboosted`,
  `raw_distance_is_invariant_to_access_count_and_source`,
  `raw_distance_excludes_graph_expansion_sentinel`,
  `raw_distance_excludes_task_structural_symbol_and_ingester_rows`,
  `procedure_scope_matches_trigger_and_budget`, and
  `exact_duplicate_survives_candidate_filter`.
- [ ] **Step 6: Run the focused tests.**

Run: `cargo test raw_distance -- --test-threads=1`

Expected: all raw-distance tests pass; existing `search()` ranking tests remain
unchanged.

- [ ] **Step 7: Run all compile-time trait implementations** with
  `cargo test --no-run -- --test-threads=1` and commit.

## Task 3: Define Canonical Identity and Safe Write Decisions

**Files:**
- Create: `src/judgment/canonical.rs`
- Create: `src/judgment/policy.rs`
- Modify: `src/judgment/mod.rs`
- Modify: `src/server.rs`
- Modify: `src/task/mod.rs`

**Interfaces:**
- Consumes: `RawDistanceCandidate`, `MemoryPayload`, existing `UpsertReport`.
- Produces:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DedupDecision {
    Allowed,
    AllowedWithHint,
    BlockedExact,
    BlockedModel,
    LinkedExisting,
}

pub struct DedupHint {
    pub candidate_id: String,
    pub raw_distance: f32,
    pub confidence: Option<f32>,
}

pub struct MemoryWriteReport {
    pub upsert: UpsertReport,
    pub decision: DedupDecision,
    pub dedup_hint: Option<DedupHint>,
    pub linked_existing_id: Option<String>,
}

pub struct EmbedderDistancePolicy {
    pub embedder_id: String,
    pub hint_max_raw_distance: f32,
    pub source_report_hash: String,
}

pub struct DedupDecisionPolicy {
    pub distance_by_embedder: BTreeMap<String, EmbedderDistancePolicy>,
}
```

`canonical_duplicate_key(namespace: &str, payload: &MemoryPayload) -> Result<String>`
must use the spec's exact field set and byte-preserving content rule. It must
not treat raw-distance equality or a model score as proof of canonical identity.

- [ ] **Step 1: Add failing canonical-key tests** for content, type, namespace,
  `location`, `location_lines`, sorted `metadata.governs`, `contained_by`,
  canonicalized `metadata.refs` (`file`, `lines`, `symbol`; ignore
  `source_context`), rule `enforcement`/`guard`, and procedure `trigger`.
- [ ] **Step 2: Implement deterministic canonical serialization** in
  `src/judgment/canonical.rs`; compare canonical strings, not vector distances.
- [ ] **Step 3: Add failing policy tests** for allow, allow-with-hint, exact
  duplicate reject, exact existing-memory reuse with `metadata.extracted_from`,
  and promoted-model blocking on a bare add only.
- [ ] **Step 4: Implement `DedupDecisionPolicy`** in `src/judgment/policy.rs`;
  raw-distance hint bands remain disabled until the Phase-2 distribution study
  writes per-embedder thresholds to `datasets/dedup/distance_policy.json`.
- [ ] **Step 5: Change `embed_dedup_and_upsert`** to consume
  `nearest_by_raw_distance` and return `MemoryWriteReport`. Do not threshold on
  `SearchResult.score` or the old `0.85` value.
- [ ] **Step 6: Implement exact reuse**: for an exact key with
  `metadata.extracted_from`, update the existing candidate's metadata
  idempotently, reuse its vector, return its ID, and create no twin row. For a
  bare exact `add_memory`, return a duplicate response without inserting.
- [ ] **Step 7: Preserve Lock 2:** on inline `task_complete(memory=...)`, a
  canonical exact pair reuses/links the existing ID; a non-canonical model
  positive returns allow-with-hint and never blocks completion or auto-links.
  A promoted model may block only a bare standalone `add_memory`.
- [ ] **Step 8: Fix the duplicate guidance** to name actual MCP tools
  `neurostrata_supersede_memory` and `neurostrata_archive_memory`.
- [ ] **Step 9: Run tests**:
  `cargo test canonical_duplicate -- --test-threads=1` and
  `cargo test task_complete -- --test-threads=1`.
- [ ] **Step 10: Commit.**

## Task 4: Add the Local Observation and Outcome Ledger

**Files:**
- Create: `src/learning/mod.rs`
- Create: `src/learning/observation.rs`
- Create: `src/learning/recorder.rs`
- Create: `src/learning/blob_store.rs`
- Modify: `src/server.rs`, `src/daemon.rs`, `src/task/mod.rs`, `src/events/`
- Modify: `Cargo.toml`, `Cargo.lock` for a direct SHA-256 dependency

**Interfaces:**
- Consumes: raw-distance candidate observations, write decisions, `ThalamicPulse`,
  `task_complete`, `search_memory`, `get_memory`, and `procedure_perform`.
- Produces: append-only `observations-YYYYMM.jsonl`, `outcomes-YYYYMM.jsonl`,
  `blobs.jsonl`, plus:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningContext {
    pub connection_id: String,
    pub active_task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PayloadHash(pub String); // lowercase 64-character SHA-256 hex

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedCandidate {
    pub candidate_id: String,
    pub payload_hash: PayloadHash,
    pub raw_distance: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionObservation {
    pub schema_version: u32,
    pub observation_id: String,
    pub occurred_at: i64,
    pub namespace: String,
    pub context: LearningContext,
    pub input_payload_hash: PayloadHash,
    pub candidates: Vec<ObservedCandidate>,
    pub embedder_id: String,
    pub policy_version: String,
    pub model_version: Option<String>,
    pub decision: DedupDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningOutcome {
    pub outcome_id: String,
    pub occurred_at: i64,
    pub namespace: String,
    pub connection_id: Option<String>,
    pub task_id: Option<String>,
    pub target_ids: Vec<String>,
    pub kind: LearningOutcomeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningOutcomeKind {
    MemoryCreated,
    MemorySuperseded,
    MemoryArchived,
    TaskCompleted,
    MemoryInspected,
    ProcedurePerformed,
}

pub struct LearningRecorder { root: PathBuf }
impl LearningRecorder {
    pub async fn open(root: PathBuf) -> Result<Self>;
    pub async fn record_observation(&self, value: &DecisionObservation) -> Result<()>;
    pub async fn record_outcome(&self, value: &LearningOutcome) -> Result<()>;
    pub async fn put_payload(&self, value: &MemoryPayload) -> Result<PayloadHash>;
}
```

- [ ] **Step 1: Add failing recorder tests** for content-addressed blob reuse,
  schema-versioned records, JSONL append/reopen, an incomplete final line, and
  concurrent writes remaining line-atomic.
- [ ] **Step 2: Implement `LearningRecorder`** under
  `~/.config/neurostrata/learning/`; append observations/outcomes without
  writing telemetry rows into LadybugDB.
- [ ] **Step 3: Implement `BlobStore::put(payload) -> PayloadHash`** with a
  direct SHA-256 dependency; same payload produces the same hash and only one
  blob record.
- [ ] **Step 4: Add a per-process UUID connection ID** in
  `server::start_mcp_proxy`, send `X-NeuroStrata-Conn-Id` on each local POST,
  extract it in `daemon::handle_mcp`, and pass `LearningContext` to
  `process_mcp_request`.
- [ ] **Step 5: Add a connection-to-active-task map** in the daemon learning
  coordinator. Bind on task claim; retain memory-ID joins after task completion
  so inline extraction outcomes remain attributable.
- [ ] **Step 6: Bind task claim/completion and tool outcomes** to the connection
  where present; join later events by `(namespace, memory_id)` across completion
  and reconnect. No follow-up remains `Unlabeled`.
- [ ] **Step 7: Record dedup observations** for every raw top-K candidate,
  including current provider/policy/model version, embedder identity/dimensions,
  raw distance, decision, and point-in-time payload hashes.
- [ ] **Step 8: Record other normal-use observations** with distinct `kind` values
  for search exposure, explicit `get_memory`, task extraction/linkage, and
  procedure performance. Do not assign their future learner labels in this task.
- [ ] **Step 9: Register mutation/task outcome recording** through a separate
  `MemorySubscriber` plus the `task_complete`/`procedure_perform` call sites.
  Recorder failure must not fail the memory/task operation.
- [ ] **Step 10: Add tests** for `observation_logging_failure_does_not_fail_write`,
  `task_completion_outcome_joins_after_task_done`,
  `connection_id_crosses_stdio_proxy_to_daemon`, and
  `no_follow_up_is_unlabeled`.
- [ ] **Step 11: Run focused tests.**

Run: `cargo test learning:: -- --test-threads=1`

Expected: all recorder, blob, correlation, and fail-soft tests pass.

- [ ] **Step 12: Commit.**

## Task 5: Add Provider Selection and Remove the API-Key Gate

**Files:**
- Modify: `src/config.rs`
- Modify: `src/judgment/mod.rs`
- Modify: `src/judgment/typesafe_jev.rs`
- Modify: `src/daemon.rs`
- Modify: `src/server.rs`

**Interfaces:**
- Consumes: `DeduplicationConfig`, `RawDistanceCandidate`, `LearningRecorder`.
- Produces:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DedupProviderMode { Auto, Heuristic, LayaLocal, Jev, None }
pub struct ProviderSet {
    pub active: Option<Arc<dyn JudgmentProvider>>,
    pub shadow: Option<Arc<dyn JudgmentProvider>>,
}
pub fn select_dedup_providers(
    config: &DeduplicationConfig,
    laya: Option<Arc<dyn JudgmentProvider>>,
    jev: Option<Arc<dyn JudgmentProvider>>,
) -> Result<ProviderSet>;
```

`JudgmentProvider::judge_duplicates(&MemoryPayload, &[RawDistanceCandidate])`
consumes raw-distance candidates. `Auto` uses the local policy and shadows a
candidate Laya model; `Heuristic` locks to local policy; `LayaLocal` selects a
promoted local model; `Jev` requires explicit selection plus a key; `None`
disables dedup learning. Legacy key-only configurations deserialize safely for
one compatibility release without auto-selecting Jev.

- [ ] **Step 1: Add config tests** for old JSON, default `auto` without key,
  explicit `jev` with env key, `jev` without key, and `none`.
- [ ] **Step 2: Add `provider: DedupProviderMode` and
  `judgment_api_key_env: Option<String>`** to `DeduplicationConfig`. Deprecate
  `similarity_threshold`; do not apply it to raw distance. Keep the old
  literal-key field as a one-release deserialization shim, but never let its
  presence auto-select Jev. When `provider == Jev`, prefer the configured
  environment variable (default `TYPESAFE_API_KEY`), then use the legacy literal
  only as a compatibility fallback.
- [ ] **Step 3: Change daemon wiring** so local observation/raw-distance
  collection does not depend on `judgment_api_key.is_some()`; `none` alone
  disables it.
- [ ] **Step 4: Refactor `DeduplicationChecker::new`** to receive an explicit
  `ProviderSet`; implement the `Auto`, `Heuristic`, `LayaLocal`, `Jev`, and `None`
  selection behavior without selecting Jev from key presence.
- [ ] **Step 5: Update the Jev adapter** to POST `/v1/systemone`, send a single
  `noul` question per candidate, parse `answers[*].noul`, and use a local Axum
  fixture for success, malformed, unauthorized, and timeout responses.
- [ ] **Step 6: Assert no automatic Jev failover** from `auto`, `laya-local`, or
  provider errors; provider failures allow the baseline write path.
- [ ] **Step 7: Run provider/config tests.**

Run: `cargo test dedup_provider_config -- --test-threads=1`

Expected: legacy config, explicit Jev selection, local auto mode, no-key startup,
and protocol fixture tests pass.

- [ ] **Step 8: Commit.**

## Task 6: Build Versioned Datasets and Baselines

**Files:**
- Create: `src/learning/labels.rs`
- Create: `src/learning/dataset.rs`
- Create: `src/learning/split.rs`
- Create: `tests/fixtures/learning/dedup_gold_calib.jsonl`
- Create: `tests/fixtures/learning/dedup_gold_eval.jsonl`
- Modify: `src/learning/mod.rs`

**Interfaces:**
- Consumes: observations, outcomes, content blobs, embedder identities, and
  versioned label rules.
- Produces:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LabelTier { DeterministicInvariant, MultiSignalConsensus,
                     SingleSignalWeakProxy, ShippedAnchorGold }
pub struct DerivedLabel { observation_id: String,
                          expected_duplicate_probability: Option<f32>,
                          tier: LabelTier, rule_id: String,
                          rule_version: u32, weight: f32 }
pub struct DatasetManifest { dataset_version: u32, split_seed: u64,
                             observation_ids: Vec<String>,
                             label_rule_versions: Vec<(String, u32)> }
pub struct DatasetBuildReport { train_rows: usize, proxy_calib_rows: usize,
                                proxy_eval_rows: usize, gold_calib_rows: usize,
                                gold_eval_rows: usize }
pub async fn build_dedup_dataset(
    store: &dyn VectorStore,
    learning_root: &Path,
    out_dir: &Path,
) -> Result<DatasetBuildReport>;
```

- [ ] **Step 1: Add failing tests** proving observations remain immutable when
  label rules change and absence of follow-up yields no `DerivedLabel`.
- [ ] **Step 2: Implement label derivation** with explicit source tier and
  deterministic rule IDs; `SingleSignalWeakProxy` cannot enter calibration or
  evaluation files.
- [ ] **Step 3: Implement component-disjoint splitting** across memory IDs,
  payload hashes, supersession lineages, shared governance targets, and time.
  Assert no connected component crosses train/calibration/evaluation.
- [ ] **Step 4: Add separately adjudicated Gold fixtures** with disjoint
  `gold_calib` and `gold_eval` partitions; tests reject overlap or weak labels.
- [ ] **Step 5: Build reproducible Rust-serialized Laya input JSONL** with
  `state`, `questions`, and soft `expected` targets. Label files with
  `proxy_internal` or `gold` calibration provenance.
- [ ] **Step 6: Add raw-distance distribution and baseline reports** for the
  heuristic, frozen Laya (when bridge passes), and explicit Jev only when
  selected for a same-dataset comparison. Do not claim Gold metrics from proxy
  suites. Write the measured per-embedder hint bands to a versioned
  `datasets/dedup/distance_policy.json`; leave model blocking thresholds unset
  until a Gold-calibrated Laya candidate exists.
- [ ] **Step 7: Run dataset/split tests.**

Run: `cargo test learning::dataset -- --test-threads=1`

Expected: tier isolation, split disjointness, rebuild determinism, and raw
distance report tests pass.

- [ ] **Step 8: Commit.**

## Task 7: Provision Local Laya Runtime and Inference Provider

**Files:**
- Create: `src/judgment/laya_onnx.rs`
- Create: `src/learning/model_bundle.rs`
- Create: `src/learning/runtime.rs`
- Modify: `Cargo.toml`, `Cargo.lock`, `src/judgment/mod.rs`, `src/setup.rs`,
  `src/daemon.rs`

**Interfaces:**
- Consumes: a Phase-0-approved ONNX bundle with manifest, tokenizer, checkpoint,
  and calibration tier.
- Produces: `LayaOnnxProvider: JudgmentProvider` and a local runtime state
  (`Ready | Provisioning | Unavailable`) consumed by `learn status`.

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState { Ready, Provisioning, Unavailable }
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationTier { ProxyInternal, Gold }
pub type ModelVersion = String;

pub struct ModelBundle {
    pub version: ModelVersion,
    pub root: PathBuf,
    pub calibration_tier: CalibrationTier,
    pub embedder_id: String,
}

impl LearningRuntimeManager {
    pub async fn ensure_ready(&self) -> RuntimeState;
    pub async fn runtime(&self) -> Result<LocalTrainingRuntime>;
}
pub struct LearningPaths { pub root: PathBuf }
impl LearningPaths {
    pub fn train_jsonl(&self) -> PathBuf;
    pub fn proxy_eval_jsonl(&self) -> PathBuf;
    pub fn base_model_dir(&self) -> PathBuf;
    pub fn candidate_dir(&self, version: &ModelVersion) -> PathBuf;
}
pub struct LocalTrainingRuntime {
    pub python: PathBuf,
    pub laya_train: PathBuf,
    pub model_cache: PathBuf,
}
pub struct LearningRuntimeManager { paths: LearningPaths }

impl LayaOnnxProvider {
    pub fn load(bundle_dir: &Path, semaphore: Arc<Semaphore>) -> Result<Self>;
}
```

- [ ] **Step 1: Add `laya-onnx` at the exact version that passed Task 1** as an
  optional dependency and add Cargo feature `learning-laya`. Enable
  `learning-laya` in the standard release feature set; verify
  `--no-default-features` still builds the heuristic-only binary.
- [ ] **Step 2: Add bundle validation tests** for missing manifest, bad
  checkpoint path, unknown schema version, bad checksum, and wrong embedder or
  model identity.
- [ ] **Step 3: Implement `LearningRuntimeManager`** in
  `src/learning/runtime.rs` and connect it to normal setup. Provision a pinned,
  app-managed Python runtime, `laya-train`, the selected base checkpoint, and
  the validated export utility locally on supported release platforms. Do not
  require a system-global Python installation or manual venv/training command.
  Provision in the background; progress/failure is queryable without blocking
  daemon startup. If any supported release platform cannot meet this contract,
  stop for owner review rather than shipping a user setup sidequest.
- [ ] **Step 4: Implement `LayaOnnxProvider`** using canonical Rust `noul`
  questions and a provisioned local bundle path (`allow_download = false`). CPU
  inference runs inside `spawn_blocking` under a semaphore; hold the permit until
  the blocking job ends even if the async caller times out. Add an offline test
  proving inference does not attempt network access.
- [ ] **Step 5: Test absent runtime, interrupted provisioning, valid bundle,
  circuit-breaker open, and fail-open write behavior with fake runners/models.**
- [ ] **Step 6: Build and test with `learning-laya` enabled and disabled.**

Run: `cargo test laya_onnx --features learning-laya -- --test-threads=1`
Run: `cargo test --no-default-features --no-run`

Expected: local-provider tests pass with the feature; the minimal build has no
Laya dependency and still compiles.

- [ ] **Step 7: Commit.**

## Task 8: Add Automatic Local Training, Calibration, and Candidate Promotion

**Files:**
- Create: `src/learning/trainer.rs`
- Create: `src/learning/calibration.rs`
- Create: `src/learning/evaluator.rs`
- Create: `src/learning/registry.rs`
- Modify: `src/learning/mod.rs`, `src/daemon.rs`, `src/judgment/mod.rs`

**Interfaces:**
- Consumes: versioned datasets, local trainer runtime, model bundle loader, and
  the fixed Gold/proxy evaluation suites.
- Produces: `TrainingJobState`, `EvaluationReport`, and an atomic
  `active_model.json` manifest naming incumbent and shadow bundle versions.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingJobState {
    WaitingForData,
    Training,
    ProxyShadow,
    GoldShadow,
    Promoted,
    Failed(String),
}

pub struct EvaluationReport {
    pub candidate: ModelVersion,
    pub suites: Vec<SuiteReport>,
}
pub struct SuiteReport { pub name: String, pub metrics: serde_json::Value }
pub struct LearningCoordinator { recorder: Arc<LearningRecorder> }

impl LearningCoordinator {
    pub async fn maybe_schedule_training(&self, now: DateTime<Utc>) -> Result<TrainingJobState>;
    pub async fn evaluate_candidate(&self, bundle: &ModelBundle) -> Result<EvaluationReport>;
    pub async fn promote_candidate(&self, version: &ModelVersion) -> Result<()>;
    pub async fn rollback(&self) -> Result<ModelVersion>;
}
```

- [ ] **Step 1: Add coordinator state-machine tests** for `WaitingForData`,
  `Training`, `ProxyShadow`, `GoldShadow`, `Promoted`, and `Failed`; training is
  never started in an MCP write call.
- [ ] **Step 2: Implement data-sufficiency checks** using Phase-2 measured class
  floors. All counts are config/manifest values determined from the baseline
  study, not magic constants copied from architect proposals.
- [ ] **Step 3: Invoke `LocalTrainingRuntime.laya_train`** using direct process
  arguments from `LearningPaths`:

```rust
Command::new(&runtime.laya_train)
    .arg("--data").arg(paths.train_jsonl())
    .arg("--base").arg(paths.base_model_dir())
    .arg("--out").arg(paths.candidate_dir(&version))
    .arg("--eval").arg(paths.proxy_eval_jsonl())
    .arg("--loss").arg("soft-ce")
    .arg("--freeze-encoder")
```

The base model path is local; the trainer cannot fetch weights during a job.
Capture stdout/stderr and exit status; failure leaves the incumbent untouched.
- [ ] **Step 4: Mark the stock trainer's internal calibration** as
  `proxy_internal`. Such bundles may run in shadow mode only.
- [ ] **Step 5: Implement post-hoc Gold temperature fitting** on `gold_calib`
  only, write calibration metadata into the exported bundle, and prove
  `gold_eval` data never enters fitting or weight updates.
- [ ] **Step 6: Evaluate against raw-distance heuristic, frozen Laya, incumbent,
  deterministic invariants, proxy consensus, and Gold shards.** Store separate
  reports; only Gold metrics support absolute accuracy claims.
- [ ] **Step 7: Promote atomically only after all spec gates pass. Keep the
  previous known-good bundle and test rollback on load/runtime errors.**
- [ ] **Step 8: Test candidate metrics and split manifests using fixtures.**

Run: `cargo test learning::trainer -- --test-threads=1`

Expected: fake-runtime tests prove no training below data floors, proxy
calibration stays shadow-only, Gold calib/eval never overlap, and failed jobs
preserve the incumbent.

- [ ] **Step 9: Commit.**

## Task 9: Add Low-Friction Learning Controls and Next-Release Integration

**Files:**
- Modify: `src/main.rs`, `src/daemon.rs`, `src/server.rs`
- Create or modify: `src/learning/status.rs`, `src/learning/control.rs`
- Modify: `README.md`, release workflow only after the current release branch
  is complete

**Interfaces:**
- CLI enum: `neurostrata-mcp learn status|pause|resume|rollback|reset`.
- Internal daemon routes: `GET /learning/status -> LearnStatus` and
  `POST /learning/control` with strict `LearningControlRequest` JSON;
  no new MCP labeling tools.
- Persist `status.json` and `control.json` under the learning directory. When the
  daemon is offline, `learn status` reads the last persisted status and labels
  it stale; pause/resume/reset control updates `control.json` for the next start.
- `learn reset` removes derived datasets/candidates; observation purge requires
  explicit `--purge-observations`.

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningControlAction { Pause, Resume, Rollback, Reset }
pub struct LearningControlRequest {
    pub action: LearningControlAction,
    pub purge_observations: bool,
}
pub struct LearnStatus {
    pub active_version: Option<ModelVersion>,
    pub shadow_version: Option<ModelVersion>,
    pub runtime_state: RuntimeState,
    pub counts_by_label_tier: BTreeMap<LabelTier, usize>,
}
```

- [ ] **Step 1: Add CLI parse tests** for every `learn` action and invalid
  combinations.
- [ ] **Step 2: Add local daemon status/control handlers** that report explicit
  zero counts and do not mutate memory rows.
- [ ] **Step 2a: Add tests** for offline status staleness and persisted pause,
  resume, rollback, and reset controls being applied at the next daemon start.
- [ ] **Step 3: Wire background schedule** to start only when training runtime,
  data floors, and the Phase-0 bridge gate are satisfied; pause/resume persists
  without stopping ordinary MCP service.
- [ ] **Step 4: Add end-to-end tests** through the live MCP proxy for observation,
  hint, exact reuse, task completion, `learn status`, pause, resume, and rollback.
- [ ] **Step 5: Capture ordinary retrieval, task, and procedure observations** in
  the common envelope, but leave their learners disabled until separate label,
  baseline, and evaluation specs are approved. Verify they do not change
  retrieval ranking or procedure behavior in this release.
- [ ] **Step 6: Document install/runtime state and release notes.** State clearly
  that only the dedup learner is active in this release; other domains are
  collecting observations pending their own evidence gates.
- [ ] **Step 7: Run the full quality gate** listed below and commit.

## Release Gate and Verification

Do not start any implementation task until the owner confirms the current
release feature set is complete. At release integration, run:

```bash
cargo fmt --check
cargo test -- --test-threads=1
cargo clippy --all-targets -- -D warnings
cargo build --release
cargo build --release --no-default-features
```

Expected: all tests pass, both Laya-enabled and minimal builds compile, and
`learn` remains fail-soft if the local training runtime/model is absent. The
runtime bridge/export parity job is required for promotion, not for the
baseline-only build.

## Follow-up Plans (Not Part of This Build Plan)

- **Retrieval learner:** only after observations distinguish impressions from
  verified uses and a target-domain evaluation set exists.
- **Procedure learner:** only after fired procedures can be paired with reliable
  outcomes rather than treating firing or later task completion as success.
- **Task/memory usefulness learner:** only after independent label rules and
  evaluation criteria exist; Lock-2 completion is not itself a quality label.

Each follow-up gets its own design review and implementation plan. They reuse
the local observation substrate but do not share Laya weights or promotion
criteria by default.
