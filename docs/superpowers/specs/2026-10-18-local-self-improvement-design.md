# Local Self-Improvement — Design Spec

**Date:** 2026-10-18
**Path:** Architectural
**Status:** Approved. Two-seat review 2026-10-21 (Argon and K3, both
approve-with-changes) with the required changes applied; the owner approved on
the same date on the condition that both seats be good with the design. The
implementation plan is still the owner's call.
**Scope:** Local learning foundation, first dedup-learning loop, and staged
expansion to other NeuroStrata capabilities. No implementation authorized by
this document alone.

## Intent

NeuroStrata should improve through ordinary operation without asking users to
label examples, tune thresholds, or run training as a side task. Learning and
inference remain local by default. Laya’s model artifacts and LadybugDB memories
belong to the same local data boundary; model storage is not a separate privacy
objection.

The end state covers several learning targets—memory usefulness and retrieval,
procedures and task outcomes, and typed judgments. These targets have different
labels and metrics, so they share an observation and dataset-building substrate
but use separate learners and evaluation gates.

The first vertical slice is duplicate judgment. It provides a measurable target,
exercises the observation-to-training-to-promotion lifecycle, and surfaces a
current defect in candidate selection. Later capabilities reuse the proven
substrate rather than expanding a speculative platform up front.

Success means the system passively records useful local experience, constructs
versioned datasets, trains Laya candidates locally when data and runtime support
are sufficient, and promotes a candidate only after evaluation against a stable
baseline. If evidence is insufficient, the system continues collecting data and
keeps the current baseline; it does not claim improvement.

## Product constraints

- No user-facing labeling queue or training chore.
- No remote teacher, Jev call, or cloud service in the local training path.
- Jev remains available as an explicitly selected hosted provider; credentials
  do not trigger automatic failover from local inference.
- One shared local observation format; separate datasets, label rules, models,
  and promotion decisions per learning capability.
- No new SQLite database. Observation records stay outside LadybugDB to avoid
  turning high-frequency telemetry into vector-memory rows or DB writes.
- V1 remains scoped to memory deduplication. It does not add a general
  agent-facing System One tool.
- Use documented upstream Laya training features in V1. Do not add a custom
  loss, LoRA implementation, or custom trainer until evidence shows the stock
  trainer cannot meet the measured need.

## Current source facts and defects this design addresses

1. `JudgmentProvider` in `src/judgment/mod.rs` currently supports duplicate
   judgments only. The daemon and checker gate it on API-key presence, so local
   no-key operation currently disables the candidate search and dedup checker.
   Local learning must be enabled independently of Jev credentials.
2. `embed_dedup_and_upsert` in `src/server.rs` calls `VectorStore::search`, then
   filters `SearchResult.score >= 0.85` as though it were similarity.
3. `LadybugStore::search` in `src/store/ladybug.rs` reads
   `array_distance(...) AS dist`, orders by `dist ASC`, and assigns
   `SearchResult.score` to a distance reduced by usage and source boosts. Lower
   values mean closer results; this is not a cosine score. Graph-expanded
   neighbors are appended with `EXPANSION_BASE_SCORE = 1.0`
   (`src/store/ladybug.rs:1418`, applied at `:1437`) and are told apart from
   primaries by evidence kind, not by score. The current dedup filter
   (`score >= 0.85`) can therefore discard close matches and admit distant
   primaries and graph-expanded rows alike.
4. The same `embed_dedup_and_upsert` path serves `add_memory` and inline task
   completion. A write decision can occur in the same call that marks a task
   done. Later outcomes must join by memory IDs across completed tasks and
   subsequent connections, not only by an active task ID.
5. The duplicate error already names `neurostrata_supersede_memory`
   (`src/server.rs:1166`), which is a real MCP tool; the earlier
   `neurostrata_edit_memory` reference no longer exists in the source. The error
   does not yet name the other corrective surface,
   `neurostrata_archive_memory`, so the remaining work is to name both.
6. The current TypeSafe adapter uses `/evaluate` and parses
   `answers[*].probability`; current public System One documentation describes
   `/v1/systemone` and `noul` answers. Validate the protocol with fixtures
   before retaining Jev as an optional provider.
7. Upstream Laya offers Python fine-tuning and a Rust/ONNX inference SDK. The
   stock Python training output to Rust `laya-onnx` artifact bridge is not yet
   verified; this is a Phase 0 gate, not an assumed feature.

## Architecture

```text
MCP stdio proxy ── connection id ──► daemon / tool handlers
                                        │
            ┌───────────────────────────┼──────────────────────────┐
            ▼                           ▼                          ▼
 raw-distance candidates       local observations          outcome events
            │                     + payload blobs           + task joins
            └───────────────────────────┼──────────────────────────┘
                                        ▼
                         per-capability dataset builders
                                        ▼
                        local Laya training (dedup V1)
                                        ▼
                     candidate ONNX bundle + calibration
                                        ▼
                   replay evaluation → shadow → promotion
```

The shared observation envelope carries provenance and correlation fields.
Each dataset builder defines its own task shape and label derivation. The first
builder handles pairwise deduplication. Retrieval, procedure, and task learners
are later additions, each with an independent baseline and promotion decision.

## Observation and outcome records

### Storage

Use local append-only, newline-delimited JSON records under
`~/.config/neurostrata/learning/`; do not write each observation into the
LadybugDB vector tables.

```text
learning/
  blobs.jsonl
  observations-YYYYMM.jsonl
  outcomes-YYYYMM.jsonl
  datasets/dedup/
  models/dedup/vN/
  active.json
```

`blobs.jsonl` stores each point-in-time payload once, keyed by a content hash.
Observations reference hashes rather than repeating candidate payloads.
Observations and outcomes are append-only; derived labels and dataset splits
are rebuildable. On restart, ignore or truncate only an incomplete final JSONL
record; preserve preceding complete records. Model promotion updates
`active.json` by atomic replacement. Retain the active and previous known-good
bundles; dataset rebuilds never mutate the source observation log.

Every record carries a `schema_version`. It starts at `1` and is additive-only
within a major version: new optional fields do not bump it, a breaking change
does, and a reader that does not recognize a record's major version ignores
that record rather than mis-parsing it. Concurrent appends are serialized behind
one writer, so a partial line is only ever the final line of a file.

### Correlation

The NeuroStrata-owned stdio proxy generates a connection ID at process start
and forwards it to the local daemon in an internal HTTP header
(`X-NeuroStrata-Conn-Id`). This avoids client-specific MCP schema changes. The
header is internal: a direct HTTP caller that omits it is not rejected — the
daemon assigns a per-request fallback token instead. When a task is claimed,
associate its task ID with that connection. Outcomes join observations by
namespace, candidate memory IDs, and connection/task IDs when available. Memory
IDs remain the fallback join across task completion, process restart, and later
sessions.

The outcome join must handle inline task extraction: the task may be completed
in the same call that emitted the dedup observation. Do not require the task to
remain in progress before accepting later outcome events.

### Point-in-time decision observation

Capture every top-K candidate returned by the dedicated raw-distance query;
do not discard observations using an uncalibrated distance threshold. A
`DecisionObservation` contains:

- schema version, observation ID, timestamp, namespace, connection ID, and
  optional task ID;
- new-memory ID when a write occurred and content-hash references for the
  incoming payload and each candidate snapshot;
- raw distance, embedder identity and dimensions; never `SearchResult.score`
  or the usage/provenance-boosted distance;
- selected provider, policy version, provider confidence (if available),
  decision (`Allowed`, `AllowedWithHint`, `Blocked`, `LinkedExisting`), and
  model version;
- candidate IDs and stable order as presented to the decision policy.

Outcome events record relevant memory creation, supersession, archival, task
completion, and subsequent actions. The pulse bus cannot carry task completion
by itself: `ThalamicPulse` emits `Superseded` only on the inline-extraction
path (`src/task/mod.rs:1199-1215`), because the other two `task_complete`
paths attach an edge to a memory that already existed and announce no move.
Task completion outcomes are therefore captured explicitly on all three Lock 2
paths — inline extraction, `metadata.extracted_from` pre-attached, and
`link_memory_id` — and are not inferred from the pulse bus. Observations with
no qualifying outcome remain `Unlabeled`; silence never means a prediction was
correct.

## Candidate selection and write behavior

Add a dedicated `nearest_by_raw_distance` path for deduplication. It returns
active domain-memory candidates ordered by the unboosted distance metric. It
excludes GraphRAG-expanded neighbors, sentinel scores, usage counts, and source
boosts. The candidate set is the top `K` rows, where `K` is the configured
`deduplication.max_candidates`.

Exclusions, with their provenance. `directory`, `file`, `markdown`,
`freshness_flag`, and `guard_event` are excluded today
(`STRUCTURAL_MEMORY_TYPES`, `src/store/ladybug.rs:677-683`); `task` is excluded
today only when `include_tasks` is false (`:1036`). Excluding `symbol`,
`code_ast`, and rows owned by `auto-ingestor` is **new** behavior this design
adds to the dedup path only. `symbol`/`code_ast` are machine-ingested AST
declarations, not memories an agent can supersede or archive. Include
procedures only against active, unspent procedures with the same trigger;
exclude procedures from other memory types. Other active domain-memory types
remain eligible for model comparison but cannot trigger the exact-match path
across types. Regular `search_memory` continues to use its existing relevance
and graph ranking behavior. Retrieval-learning changes must not alter the dedup
candidate set.

Exclude rows whose metadata marks a SynapticMesh conflict
(`is_mesh_conflicting`, `src/store/ladybug.rs:698-700`, applied at
`:1083-1085`) from `nearest_by_raw_distance` **and** from canonical exact-match
reuse, so the dedup path never blocks on, hints toward, or reuses a row whose
conflict is unresolved.

Registry rows are excluded by the store seam, not by memory type: a fact
carrying `metadata.domain_registry` is dropped from every result set alongside
archived, retired, expired, and mesh-conflicting rows
(`src/store/ladybug.rs`, four filter sites), so it never reaches
`nearest_by_raw_distance` or canonical exact-match reuse. The registry row is a
cache, not a memory an agent can supersede.

The candidate set is partitioned by attempt-presence before any comparison: a
row carrying a valid `metadata.attempt` (a failed attempt) is compared only
against another such row, and a row without one only against rows without one.
A malformed `attempt` is neither a negative exemplar nor a partitioned row - it
is a plain row. This partition is applied in `nearest_by_raw_distance` and in
canonical exact-match reuse alike, so a failed attempt never reuses or is
reused by a non-attempt memory.

Apply the type, owner, conflict, and registry exclusions in the query's `WHERE`
clause rather than filtering after the fetch: the store fetches
`FETCH_MULTIPLIER * limit` rows (`src/store/ladybug.rs:1011-1012`) and filters
in Rust, so post-fetch filtering can starve a small `K` in namespaces dense
with excluded rows. The attempt partition is applied after the fetch, at the
server level, because it depends on payload metadata the store does not inspect.

Record the embedder identity and vector dimensions with every raw distance.
Distance scales are embedder-specific; do not convert distance to cosine using
`1 - distance`, and do not carry forward the current `0.85` threshold. Phase 2
measures per-embedder distance distributions before selecting hint or blocking
cutoffs.

Define a canonical exact-match key over namespace, exact UTF-8 content,
`memory_type`, `location`, `location_lines`, sorted `metadata.governs`, sorted
`metadata.contained_by`, and canonicalized `metadata.refs` (`file`, `lines`, and
`symbol`, sorted by those fields; ignore `source_context`). For `rule`, compare
`metadata.enforcement` and `metadata.guard`; for `procedure`, compare
`metadata.trigger`. For `fact` rows, compare `metadata.attempt` - its presence
and, when present, its four keys `approach`, `outcome`, `failure_mode`, and
`context` - so a failed attempt never reuses or is reused by a non-attempt
memory. `domain` is deliberately not part of the key: dedup detects duplicates
across domains. Exclude usage counters, source provenance, and other mutable
metadata. Exact content means byte-for-byte equality; do not normalize
whitespace or punctuation in ways that could merge distinct rules. Only a key
match counts as the pre-calibration exact-duplicate case; equal raw distances
alone do not.

Candidates are active only when not archived, retired, or expired by `valid_to`.
For procedures, missing or null `remaining_fires` means unbounded; zero means
spent and excludes the candidate. `remaining_fires` is a procedure-runtime
convention that the store does not enforce today, so the dedup path reads it
from metadata rather than relying on a store-level filter.

During collection and calibration:

- Allow writes in a measured uncertainty band and return a brief hint naming
  the new and similar memory IDs. This yields uncensored observations without
  turning uncertain judgments into write failures.
- Before Phase 2 establishes a distance band, only the canonical exact-match
  key may trigger exact-duplicate handling. For a bare `add_memory`, reject the
  duplicate without inserting a twin row. When the payload carries
  `metadata.extracted_from`, reuse the existing memory and idempotently attach
  those task links rather than blocking task completion or inserting another
  row. For non-canonical statistical matches, never auto-link a memory.
- After calibration, a promoted model may block a high-confidence match on a
  bare `add_memory`; inline task extraction remains allow-with-hint unless the
  canonical exact key matches and can safely reuse the existing memory.
- If there is no trained model, use the raw-distance baseline and record its
  output. Until the distance study sets a useful hint band, capture candidates
  without inventing a similarity cutoff.
- Surface `dedup_hint` consistently from both `add_memory` and inline task
  completion. A hint never blocks Lock 2 task completion.
- Observation and dataset persistence failures never fail a memory write or a
  task completion. The learning substrate is best-effort beside the write path:
  a failed append is logged and the write proceeds.
- Keep the duplicate error naming the real corrective tools. It already names
  `neurostrata_supersede_memory`; extend it to name `neurostrata_archive_memory`
  as well.

All numeric distance cutoffs remain unset until the Phase 2 baseline study.

## Label taxonomy and dataset construction

Observations and derived labels are separate. A label records its tier,
derivation rule and version, source IDs, and training weight. A new derivation
rule creates new labels; it never rewrites the original observation.

### Label tiers

| Tier | Examples | Use |
|---|---|---|
| `DeterministicInvariant` | Canonical exact-content equivalence; controlled format-only transformations; mechanically constructed AST-symbol or polarity counterfactuals | Training and invariance tests. Synthetic results are not real-world semantic-accuracy evidence. |
| `MultiSignalConsensus` | Multiple independent local behavioral/structural signals agree on a pair | Optional soft-target training and relative held-out proxy evaluation. Label explicitly as inferred agreement, not ground truth. |
| `SingleSignalWeakProxy` | One supersede/archive action, one graph edge, task link, or passive coexistence | Optional, down-weighted training only; never calibration or evaluation. |
| `ShippedAnchorGold` | A fixed, maintainer-curated, independently adjudicated dedup benchmark (to be created in Phase 2; it is not present today) | Separate component-disjoint calibration and final-evaluation shards; absolute accuracy claims use the final shard only. It does not require end users to label their memories. |

No ordinary workflow action currently proves semantic equivalence by itself.
In particular, `RELATES_TO`, shared `GOVERNS` or `EXTRACTED_FROM`, superseding,
archiving, and `task_complete(link_memory_id)` are not Gold labels. They may
provide evidence for an inferred label when combined, but retain their
provenance and uncertainty.

Dataset generation is deterministic and repeatable from source observations,
blob hashes, label-rule version, and split manifest. Split by connected
components before training: no memory ID, payload hash, supersession lineage,
or shared governance component may cross train/calibration/evaluation. Also
reserve a forward-time evaluation slice. Gold calibration rejects weak and
synthetic records. A separate inferred-consensus proxy evaluation may contain
`MultiSignalConsensus` rows, but must never be reported as Gold or absolute
accuracy.

If the local corpus has too few resolved examples, training remains dormant.
Without a suitable Gold benchmark, candidates may still train and shadow-test,
but cannot be promoted to write-blocking or support absolute accuracy/Jev-parity
claims. Do not treat proxy agreement or synthetic tests as Gold.

## Local training and runtime

### Training path

Training runs as a background local job, never in a memory-write request. The
local training runtime is provisioned and managed by normal NeuroStrata setup;
users do not install Python environments or invoke training commands manually.
If the runtime is unavailable, collection continues and the baseline remains
active; `learn status` reports why training is pending. Training starts only
after the export spike passes and the dataset has adequate component-disjoint
examples, including positive and negative cases in each required split. Minimum
counts and loss weights are set after Phase 2 measures the local distributions;
values from architect proposals are provisional, not requirements.

V1 uses stock upstream `laya-train` features only: soft cross-entropy targets
for uncertain labels, and either a frozen encoder or documented differential
encoder/head learning rates. Do not ship a custom LoRA, SCE, or KL-anchoring
trainer in V1. Stock `laya-train` fits temperatures on an internal held-out
slice of its training input; when that input contains inferred labels, mark the
bundle `calibration_tier: proxy_internal` and keep it shadow-only. Laya’s Python
package handles training; the Rust service does not require Python at inference.

For promotion, partition `ShippedAnchorGold` into component-disjoint
`gold_calib` and `gold_eval` shards, both excluded from weight training. Fit
post-hoc temperature calibration on `gold_calib`, then evaluate final claims on
the untouched `gold_eval` shard. This adds only a calibration step, not a custom
model-training loop. Without the Gold calibration/evaluation shards, a candidate
may train and shadow-test but cannot be promoted to write-blocking.

Rust owns canonical input construction for both dataset generation and runtime
inference. The Python trainer consumes those serialized examples. A Phase 0
spike must establish whether the trained checkpoint can be exported into the
artifact layout accepted by `laya-onnx`; it must verify Python/Rust prediction
parity on a fixed fixture. If export or parity fails, keep the heuristic/current
provider and continue local observation collection.

### Provider selection and failures

Decouple `DeduplicationChecker` initialization from `judgment_api_key` presence.
Local observations and the raw-distance baseline must work without an API key.
The dedup-specific provider selection is explicit:

- `auto`: local heuristic until a local Laya model is promoted; shadow-run a
  candidate when available;
- `heuristic`: use only the local raw-distance policy;
- `laya-local`: explicitly select the promoted local model;
- `jev`: explicitly select hosted Jev using a configured API-key environment
  variable and the documented `/v1/systemone` protocol;
- `none`: disable dedup decisions and learning observations.

The optional Jev key is not used by training and never triggers automatic
fallback. Do not switch between Jev and Laya on provider failure because their
confidence scales differ. On a local-model load error, timeout, or circuit
open, allow the write through the deterministic local policy; do not block
memory writes or task completion.

Run CPU-bound Rust ONNX inference through `spawn_blocking` with bounded
concurrency. Keep the concurrency permit attached to the blocking job even if
the async caller times out; a timeout must not cause unbounded queued inference.
Training/export failure preserves the active model and daemon operation.

### Optional Jev compatibility

When Jev is explicitly selected, update the existing adapter to the current
System One endpoint and response schema, and verify it with local HTTP fixtures.
This is separate from local training. A configured key alone does not select
Jev and a failed local request does not send memory data to Jev.

## Evaluation, shadowing, and promotion

Compare every candidate with three baselines: raw-distance heuristic, frozen
Laya checkpoint, and current incumbent. Keep three evaluation reports
separate:

1. **Deterministic invariance:** exact/controlled transformations and
   structural counterfactuals. These are pass/fail invariants, not semantic
   dedup accuracy.
2. **Local inferred-consensus:** component- and time-disjoint local outcomes.
   Report proxy agreement and uncertainty; do not describe it as Gold accuracy.
3. **Maintainer Gold:** a fixed independently adjudicated benchmark. This is
   the only basis for absolute precision/recall or Jev-parity claims.

Report precision/recall at the selected operating point, PR-AUC, Brier score,
calibration error, hard-negative false-positive rate, and CPU latency. Determine
numeric cutoffs after the Phase 2 baseline distribution study. No model
promotion on synthetic data alone or proxy agreement alone.

New bundles enter shadow mode: record candidate predictions beside the active
policy without changing writes. Promote only after the candidate passes the
invariance suite, improves the relevant held-out baselines, does not regress
Gold results, and meets the data-sufficiency and runtime-parity gates. Keep an
atomic active-model manifest and the previous known-good bundle for rollback.
If promotion evidence is weak, remain in shadow mode.

## User experience and controls

The normal path is passive: collect observations, build datasets, train when
data and runtime gates permit, and evaluate candidates without user prompts.
No user has to manage memories or label examples to operate the system.

Provide a small inspection/control surface:

- `learn status`: active policy/model, candidate/shadow state, latest evaluation,
  and observation/example counts by tier;
- `learn pause` / `learn resume`: stop or resume background training without
  changing the active provider;
- `learn rollback`: restore the previous known-good policy/model;
- `learn reset`: remove derived datasets and candidate models while retaining
  source observations. An explicit `--purge-observations` option is separate.

These commands are optional operator controls, not steps in the learning loop.
The learning files, checkpoints, and training remain on the local machine.

## Phased delivery and acceptance criteria

All efficacy thresholds remain unset until Phase 2 measures real score and
label distributions. Acceptance criteria below are structural and testable.

### Phase 0 — Laya training/export compatibility spike

- Run stock `laya-train` on a fixed fixture and determine the exact artifact
  conversion needed by `laya-onnx`, including whether an externally fitted
  temperature can be packaged and consumed by the Rust runtime.
- Verify Rust can load the candidate bundle and compare its predictions with
  Python on the same serialized inputs.
- If this fails, keep local data capture and baseline work moving; do not enable
  automated weight training or assume compatibility. Evaluate a bundled local
  Python inference bridge as a follow-up rather than silently switching to a
  remote provider; keep the Rust runtime path preferred if parity succeeds.

### Phase 1 — Observation substrate and raw-distance baseline

- Add the connection ID to the internal stdio-to-daemon request path; client
  MCP tool schemas remain unchanged.
- Add unboosted `nearest_by_raw_distance`; test ascending ordering, exact-match
  retrieval, invariance to access-count/source metadata, and exclusion of
  GraphRAG sentinel neighbors, task/structural rows, `symbol`/`code_ast`,
  `auto-ingestor` rows, mesh-conflicting rows, and registry rows. Procedure
  candidates must match trigger and remain unspent/unlapsed. Verify the
  attempt-partition: a failed attempt is compared only with a failed attempt.
- Remove the inverted `score >= 0.85` filter and stop interpreting the existing
  `similarity_threshold` as a cosine threshold. Record embedder identity,
  dimensions, and all top-K raw-distance candidates, where K is the configured
  `deduplication.max_candidates`.
- Bypass the existing judgment call in this phase:
  `DeduplicationChecker::check_duplicates` (`src/server.rs:1162`) is not
  invoked unless provider `jev` is explicitly selected. Under `auto`,
  `heuristic`, `laya-local`, and `none`, the path records candidates and applies
  only the canonical-key decision; no model confidence is consulted and no
  shadow comparison runs until Phase 3.
- Enable local observation without an API key. Before Phase 2 establishes a
  distance band, verify only canonical exact matches produce the exact-duplicate
  response; raw-distance candidates remain recorded without speculative hints.
- Verify a bare exact-canonical `add_memory` creates no twin row; if
  `metadata.extracted_from` is present, reuse the existing ID and idempotently
  attach the task link. A non-canonical model match must never auto-link.
- Verify inline `task_complete(memory=...)` reuses an exact-canonical existing
  memory, writes the `EXTRACTED_FROM` link, and completes Lock 2 without a twin
  row. The reused row's `metadata.extracted_from` must be updated before the
  Lock 2 gate re-reads the memory list, so the gate observes the new link. A
  non-canonical model positive must allow-with-hint on this path and must never
  block completion or auto-link the candidate.
- Verify that the candidate set excludes mesh-conflicting rows in both
  `nearest_by_raw_distance` and canonical exact-match reuse.
- Verify outcome joins work after task completion, across connections, and via
  memory IDs. Missing outcomes remain unlabeled. Task completion outcomes are
  captured on all three Lock 2 paths, not inferred from the pulse bus.
- Keep the duplicate error naming the real corrective tools. It already names
  `neurostrata_supersede_memory`; extend it to name `neurostrata_archive_memory`
  as well. Verify JSONL append/recovery, including a crash during append that
  leaves a partial final line, and that observation failures do not fail memory
  writes or task completion.

### Phase 2 — Dataset builder and baseline study

- Rebuild datasets from immutable observations, blobs, and versioned label
  rules. Verify label tiers and deterministic rebuilds.
- Verify connected-component and time-disjoint splits; weak labels are rejected
  from Gold calibration and final evaluation. Partition the maintainer benchmark
  into disjoint `gold_calib` and `gold_eval` shards, with neither in the weight
  training set. When a forward-time observation touches a component that
  already exists in the training window, drop the cross-boundary component
  members from the forward-time evaluation slice rather than leak the component
  across splits.
- Report raw-distance percentiles by embedder, provider, memory type, and
  deterministic/inferred label tier. Select hint/block boundaries only after
  reviewing these distributions.
- Once this phase establishes the distance band, verify that uncertain
  candidates produce a non-blocking hint in both `add_memory` and inline task
  completion, and that successful writes and task completions are preserved.
- Establish heuristic, frozen-Laya, and (if selected) Jev baselines on the
  same supported evaluation cases. Curate the fixed Gold benchmark before
  making absolute accuracy claims.

### Phase 3 — Local candidate training and shadow execution

- Keep training dormant until the export bridge and data/class-balance floors
  established in Phase 2 pass.
- Run stock `laya-train` as a background job using the versioned dataset and
  split manifest. Its internal calibration is marked `proxy_internal`; fit a
  separate Gold temperature only when `gold_calib` exists. Training
  absence/failure leaves the incumbent untouched.
- Load exported candidates in Rust; verify parity, bounded concurrency, and
  shadow logging. Shadow predictions never block writes.

### Phase 4 — Evidence-gated promotion

- Promote only after predeclared metrics and data floors pass on the correct
  suites, including the Gold benchmark where absolute claims are made.
- Verify atomic rollback to the previous bundle and baseline behavior on model
  load failure, timeout, circuit-breaker open, and explicit rollback.
- Verify no silent local-to-Jev failover.

### Phase 5 — Expand learning targets

After the dedup loop proves useful, add retrieval, procedure, and task datasets
one at a time. Each target must define its own observation payload, label
semantics, baseline, evaluation suite, and rollback gate. Reuse the common
observation envelope and dataset tooling only where actual reuse is proven.

## Open technical gate

The stock Python trainer to Rust ONNX artifact bridge is not verified. It is a
hard gate for automatic Laya weight promotion, not for observation capture or
raw-distance baseline improvements. No accuracy or Jev-parity claim is made
until the specified same-task evaluation exists.

## References

- Current dedup provider: `src/judgment/mod.rs`,
  `src/judgment/typesafe_jev.rs`, `src/config.rs`, `src/daemon.rs`.
- Candidate search/write path: `src/server.rs` and `src/store/ladybug.rs`.
- Task extraction path: `src/task/mod.rs`.
- Mutation events: `src/events/event.rs`.
- Laya upstream: https://github.com/NandhaKishorM/laya
- Laya Rust SDK: https://github.com/NandhaKishorM/laya/tree/main/laya-rust
- Laya training guide: https://github.com/NandhaKishorM/laya/blob/main/docs/finetune.md
- TypeSafe System One API: https://github.com/typesafe-docs/typesafeai/blob/main/docs/09-api-reference.md
- Architect panel artifacts: `docs/architecture/2026-10-18-k3-laya-judgment-provider-verdict.md`, `docs/designs/2026-10-18-laya-openai-verdict.md`
