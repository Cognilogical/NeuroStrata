# Independent OpenAI verdict: Laya for dedup

Status: proposal, not reconciled or approved. Evidence: supplied brief and
`src/judgment/{mod,typesafe_jev}.rs`, `config.rs`, `daemon.rs`, `server.rs`.
No other architect verdicts consulted. No experiments or hosted calls run.

## Decision

Pursue a bounded local-first dedup experiment, not production adoption or
a generic System One platform. Laya is a plausible optional first-line
provider, not yet a credible default decision-maker. Local means no API
bill, not zero disk/RAM/CPU/support cost.

Base benchmark ~0.36 versus majority ~0.461 argues against trusting stock
weights. Specialized ~0.766 supports testing specialization, not dedup
readiness. Shared `/v1/systemone` format establishes wire compatibility,
not equivalent semantics, calibration, or quality.

Jev should initially be an explicitly selected alternate provider.
User-authorized second opinions are useful for evaluation, but Jev is not
ground truth or an automatic tie-breaker. Reject automatic hosted fallback:
uncertainty, missing weights and local outages must not export memory.
Later escalation requires separate namespace-level consent, payload policy,
spending limits and independent evaluation. Credentials are not consent.

## Smallest credible experiment

Bound scope to existing duplicate judgments. No public generic judgment
API, model router, training service or always-on sidecar.

Stage A: offline harness, pinned Laya Rust SDK/runtime/checkpoint, CPU only,
one fixed typed pairwise duplicate question, existing candidate selection.
Compare stock and available specialized checkpoints against similarity-only
and majority baselines. Optional Jev comparison uses the same synthetic or
public corpus, explicit credentials and a spending cap. First establish
protocol fixtures: existing `/evaluate`, `jev-1.13`, and
`probability`/`reasoning` parsing do not prove `/v1/systemone` support.

Initial dataset: 600 human-labeled pairs, split by source fact/document
family into 300 training, 100 calibration, 200 untouched test. Keep
paraphrase families out of multiple splits. Include equivalent facts,
partial overlap, negation, changed numbers/dates, supersession, entity and
scope differences, metadata authority, long inputs and embedded malicious
instructions. Two independent human labels; adjudicate disagreement;
ambiguous pairs remain non-actionable.

Add a small synthetic namespace corpus to measure candidate retrieval
misses, not merely pairwise classification. Report natural-prevalence and
hard-negative strata separately. Synthetic success qualifies a local
pilot, not general deployment.

Run frozen checkpoints before training. If justified, permit one bounded
dedup fine-tuning iteration using training split only. Calibrate separately
and leave final test untouched. Optional teacher distributions use approved
synthetic/public inputs and never serve as evaluation ground truth.
Retuning after test inspection requires a fresh final test.

Stage B, only after Stage A passes: owner-opted local shadow mode. No model
output rejects writes or mutates records. Keep contents local; export only
permissioned aggregate metrics. Validate actual workload relevance through
owner-reviewed local labels. Do not build customer-data collection.

## Minimal architecture and correctness

Keep existing dedup-specific provider seam. Explicit `off/laya/jev`
selection, initially default off. Document compatibility for legacy
key-only configurations; do not silently enable a different route.

Both `daemon.rs:452` and `judgment/mod.rs:79,108` gate on API-key presence:
swapping one adapter is insufficient. `server.rs:1045-1049` hard-codes top-5
and 0.85 despite configurable fields. Freeze these for fair comparison;
make effective settings observable before rollout.

Critical consequence: `server.rs:1054-1060` rejects memory writes on
positive judgments. The shared pipeline also serves task-completion
extraction. False positives suppress knowledge or block task completion,
not merely create bad suggestions. Never automatically merge/delete or
overwrite. Unavailable, malformed and ambiguous results must abstain and
preserve ordinary writes. Distinguish abstention from confident negatives
in metrics; current `Option` collapses several outcomes.

Map deterministic question IDs only to supplied candidates. Validate answer
variant, completeness, candidate membership, finite bounded values and
input limits. Do not treat arbitrary choice/score/noul fields as equivalent
probabilities. Bind documented semantics; calibrate each checkpoint.
Existing 0.8 threshold is not proven portable. No silent truncation:
overlength input abstains. Enforce namespace/privacy/authority boundaries
deterministically. Treat memory text as untrusted data, not instructions.

## Security, credentials and operations

Current adapter sends content and all metadata. Minimize hosted payload to
necessary, policy-approved fields; omit IDs and unrelated metadata. Enforce
privacy labels and authorization before dispatch. Redaction does not
guarantee de-identification.

No raw prompts, answers, reasons, keys or hosted error bodies in logs.
`typesafe_jev.rs:134-137` puts response bodies into errors later logged by
the checker. Config derives `Debug` and `Serialize` with the raw key.
Do not persist new credentials in ordinary JSON or config dumps. Use OS
secret storage, with explicit environment or permission-restricted secret
file references for headless systems. Support rotation/deletion, redact
debug/error paths, never commit keys, and document old-JSON migration.
Use approved HTTPS destinations; prevent redirects or endpoint changes
from forwarding credentials. Verify hosted retention/training terms
before sensitive-data eligibility. BYOK provides no privacy guarantee.

Pin artifact revisions/hashes. Review SDK/runtime, weights and training-data
licenses and redistribution rights separately. Explicit model installation
with size disclosure; no inference-time auto-download or startup-blocking
fetch. Bound CPU threads, concurrency, queue and input sizes. CPU inference
must not occupy async executor threads. Async timeout alone may not cancel
ONNX work: use bounded workers, with process isolation if cancellation
requires it. Track loading/ready/unavailable, cold/warm latency, peak RAM,
disk footprint, failures and versions. Defer GPU backends. Missing/corrupt
weights must preserve daemon startup and writes without hosted fallback.

## Training and evaluation privacy

Start with invented or appropriately licensed public facts only. No central
export of customer memories, telemetry prompts or production teacher calls.
Optional customer adaptation runs inside customer-controlled environments
with explicit permission and encrypted/access-controlled datasets.
Weights and calibration artifacts may memorize sensitive content: do not
upload/share them by default. Mesh-shareable is not training consent.
Split by source/project/entity family, audit provenance and leakage,
document artifact retention/deletion, and recalibrate after quantization
or export. Local pilot labels stay local; inspect only authorized aggregate
results outside the customer environment.

## Go/no-go gates

Proposed acceptance thresholds, not measured results:

1. Pilot: no critical false duplicate in adversarial slices. At chosen
   threshold, >=99% observed duplicate precision and >=50% duplicate recall
   on untouched test, with confidence intervals and prevalence stated.
   Beat similarity-only false positives at matched recall. Too few positive
   predictions means inconclusive, not pass. Stock failure can justify the
   single training iteration, never shipping.
2. Write-blocking rollout requires a separate decision and larger evidence:
   one-sided 95% upper bound <=0.5% false-positive rate on representative
   nonduplicates; one-sided 95% lower bound >=99% precision on representative
   flagged positives. Zero errors needs roughly 600 independent negatives
   and 300 flagged positives respectively. Family correlation reduces
   effective sample size. No critical negation/supersession/scope failures.
   Retain useful recall >=50%; near-total abstention does not pass. The
   initial pilot cannot certify this gate.
3. Predeclare supported CPU hardware. Proposed pilot envelope: <=2s warm
   p95 for up to five candidates, <=5s request deadline, <=4GiB incremental
   peak RAM and <=5GiB model disk. Measure cold readiness separately;
   daemon remains responsive during loading. If unsuitable, explicitly
   renegotiate minimum hardware before rollout. No runaway inference after
   timeout. These budgets are proposed product limits, not upstream claims.
4. Local mode has zero inference-related network traffic after explicit
   installation. No secret/raw-content logging. Error, overload and missing
   weights neither block writes nor invoke Jev. Reproducible artifact,
   license and protocol checks required.
5. Stop after one training iteration if quality fails, privacy/license
   obligations remain unresolved, or resource budgets miss. Retain opt-in
   Jev only if independently qualified; otherwise leave judgment off.
   This experiment does not authorize broader System One expansion.

Orchestrator owns reconciliation, implementation authorization and later
validation. Worker delegation hit depth limit; design persisted directly.
