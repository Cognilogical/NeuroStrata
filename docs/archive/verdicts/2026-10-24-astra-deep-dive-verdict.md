# Astra independent NeuroStrata architecture verdict

Date: 2026-10-24  
Evaluator: Astra / openai-arch  
Evaluation task: `neurostrata-0cjh`  
Source baseline: `d83ee2a`; package version `1.11.0`

## Evidence and limits

This verdict derives from repository source and the supplied evidence brief.
It is an independent assessment, not a reconciled architecture-panel result.
No peer verdict supplies a finding or recommendation here. No external service,
live project data, model endpoint, build, test suite, or checker was invoked.
Findings below distinguish visible implementation defects from design risks.
Test names establish coverage intent, not evidence that tests currently pass.

Inspection covered `src/`, both vocabulary files under `src/schemas/`, parser
and tool contracts, `tests/`, CI definitions, `README.md`, `CLI-readme.md`,
`AGENTS.md`, and operational/architecture documentation. This is a broad
architectural source review, not an exhaustive audit of every line or platform.

Recommendations R01–R15 are concrete design decisions with implementation
acceptance criteria. They do not claim that implementation has been authorized
or completed. No additional task was created, as instructed. This artifact is
the complete decision set handed to the orchestrator for reconciliation.

## 1. Executive summary

**Verdict: retain the core architecture; harden its promises before expanding
the cognitive feature surface.**

NeuroStrata has a useful, distinctive center: source-anchored graph memory,
deterministic retrieval, and a task completion funnel that requires recorded
knowledge. Keeping tasks and lessons in one embedded store is a sound product
decision. Git-based sharing, local embeddings, explicit provider selection,
and operator-visible provenance are coherent complements.

The implementation is substantially more than a prototype. Lifecycle logic,
graph evidence, import validation, namespace handling, operator commands,
and regression fixtures represent meaningful engineering. However, maturity
is uneven. The strongest user claims—universal behavioral enforcement,
exclusive claims, lossless sharing, and self-improvement—exceed the guarantees
visible at several critical boundaries.

Four release-blocking concerns for those claims:

1. Loopback HTTP exposes administrative mutation and sandbox execution without
   application authentication or demonstrated caller authorization.
2. Guard matching is top-three retrieval plus substring triggers, not complete
   policy enforcement; a semantic-only pass receives a high-fidelity label.
3. Ingestion treats queue saturation as successful completion after clearing
   the previous structural graph.
4. Task claim exclusivity is implemented as a non-atomic read/check/write.

Additional high-priority concerns include fabricated intake creation success,
mesh crash/overflow recovery gaps, successful no-op learning operations,
embedding-space identity, post-limit recall filtering, and weak contract tests.

**Deployment judgment:** useful for a trusted, single-owner local workflow
with informed operators. Not yet supportable as a hostile-client security
boundary, a multi-tenant service, or a reliable autonomous learning platform.
Those distinctions should be product capabilities, not caveats users discover
after a failure.

### Functionality and technology map

| Area | Implemented shape | Architectural assessment |
| --- | --- | --- |
| Memory core | Rust binary; embedded LadybugDB; vectors and four graph relation types | Appropriate local-first foundation |
| Three tiers | Global/project namespace conventions; task rows; domain metadata | Organizational layers, not three independent security boundaries |
| Retrieval | Vector distance, capped usage/source gain, bounded graph evidence | Explainable; exact identifiers and candidate completeness need work |
| Task subsystem | Four-state machine, history, blockers, claims, extraction gate | Strong model; concurrency and bypass surfaces weaken enforcement |
| Guard | Embedded rule retrieval, trigger matching, optional Podman execution | Advisory screening presented too strongly as enforcement |
| Buffer | Local timestamped Markdown, rollover, retention, tag recall | Simple and useful; shared-file concurrency and path ownership need work |
| Procedures | Explicit acknowledgment, finite budget, trust/lapse checks | Real bookkeeping; not an autonomous scheduler or verified execution |
| Attempts | Structured failed-episode metadata and visible negative exemplars | Good separation of evidence from normative rules |
| Ingest | Tree-sitter, ignore walker, bounded producer/consumer pipeline | Good topology; lossy producer and clear-first rebuild are unsafe |
| Registry | Per-namespace domain cache with normalization/reconciliation | Sensible compatibility design; not an authorization boundary |
| Source anchors | Source identity and registered marker resolution | Strong worktree-aware boundary; preserves stored provenance |
| Mesh | JSON records, local outbox, git transport, validation and conflicts | Thoughtful import safety; publication durability remains incomplete |
| Learning | Observation/split/label scaffolding, provider interfaces | Mostly scaffolding, not an operational improvement loop |
| Laya/Jev | Empty local inference stub; optional remote request implementation | Capability reporting and actual selection disagree |
| MCP/CLI | Handwritten JSON-RPC/tool schema; broad operator command surface | Useful reach; duplicated contracts and transport parity risks |
| Operations | Daemon, backups, diagnostics, checkpoints, cross-platform CI | Good operational investment; resource/durability contracts need evidence |

## 2. Strengths

### 2.1 Knowledge consolidation has a mechanical home

`src/task/machine.rs::apply` expresses a small, understandable transition
table and guards completion with extraction evidence. `src/task/gate.rs`
adds a separate shipping-time policy. This is materially stronger than
asking an agent to remember to write a summary. Retired work and retired
extractions receive explicit treatment rather than being confused with
live obligations.

The lesson is still only as useful as its content: an extraction relationship
proves that an artifact exists, not that it teaches a correct or novel fact.
Nevertheless, artifact existence is a legitimate enforceable minimum.

### 2.2 Retrieval is inspectable rather than opaque

`src/store/ladybug.rs::search_inner` exposes the ranking formula, lifecycle
filters, deterministic tie-breaking, and graph expansion. `src/traits.rs`
defines result/evidence contracts. Source pointers and bounded evidence paths
are better suited to coding agents than unbounded document dumping.

Separating raw-distance duplicate candidates from boosted recall is also
correct: popularity should not become evidence of semantic duplication.

### 2.3 Privacy boundaries are intentional, even where incomplete

Local fastembed is the default. Declared remote credentials are resolved
explicitly in `src/embed.rs::build_embedder`. `src/secrets.rs` rejects
recognized secrets without echoing their values in rejection messages.
Mesh publication filters types and eligibility; imported procedures require
local promotion before acknowledgment can fire.

### 2.4 Mesh import is unusually careful

`src/synaptic_mesh/import.rs` separates path classification, file validation,
record validation, epoch checks, lifecycle decisions, and store application.
Private rows are protected against imported ID collisions. Conflicts have a
visible representation instead of silently selecting the last writer.
`acceptance.rs` contains multi-node and lifecycle scenarios, including private
row preservation and procedure-state preservation.

### 2.5 Failure isolation is a real design concern

`LadybugStore::with_conn` moves synchronous FFI off async executor threads.
`write_with_deadline` preserves writer ownership while an abandoned blocking
operation remains active. The daemon separates periodic checkpoints from
request latency and waits for ingest shutdown. These are good responses to
actual embedded-engine constraints, not unnecessary abstractions.

### 2.6 Compatibility and provenance receive attention

`src/registry.rs` treats its domain row as a repairable cache, not truth.
`src/source_context.rs` scopes path overrides to source identity and refuses
ambiguous anchor resolution. `src/attempt.rs` treats a failed attempt as
episodic evidence rather than automatically converting it into a rule.

### 2.7 CI and regression culture exist

`.github/workflows/ci.yml` builds three platform families and runs a Linux
test job with a serial schedule to control embedded-database resource use.
Source modules include substantial lifecycle, routing, import and recall
regression tests. The problem is uneven assertion quality and adversarial
coverage, not absence of engineering effort or absence of CI tests.

## 3. Weaknesses, gaps, and risks

### F01 — Loopback is mistaken for an authorization boundary

**Confirmed source exposure.** `src/daemon.rs:525–542` registers `/delete`,
`/edit`, `/set-metadata`, `/backup`, `/shutdown`, `/ingest`, `/validate`, and
`/mcp` with shared state but no authentication middleware. The default bind
at line 131 is loopback, which prevents direct ordinary remote connections;
it does not distinguish the owner from another local process or user.

Removing destructive operations from MCP discovery does not stop a
shell-capable agent from calling the HTTP administration surface. Arbitrary
local clients can also request sandbox execution. Browser-mediated access
requires a separate Host/Origin analysis; this review does not claim that a
cross-origin exploit was demonstrated.

### F02 — Guard coverage and verdict semantics are overstated

**Confirmed implementation limitation.**
`src/guard/semantic.rs:104–106` asks for three candidate rules.
`src/guard/validator.rs:155–163` matches lowercase substrings, including
`pattern.contains(action_type)`. A hard rule outside that candidate set is
not evaluated. This is neither complete deterministic policy coverage nor
semantic entailment of an action's compliance.

`validate` returns `SandboxPassHighFidelity` at lines 142–150 even when
`use_sandbox` is false. A successful container exit is likewise not proof of
safe host behavior. The README's universal interception and safety wording
is not established by an optional validation API or by hook installation.

### F03 — Sandbox timeout and output handling are fragile

**Confirmed missing controls; runtime consequences require testing.**
`src/guard/sandbox.rs:89–107` spawns Podman and times out its output future.
There is no explicit container-ID cleanup or child kill/wait sequence on
timeout. No CPU, memory, PID or output-byte limit is supplied. Output is
collected before truncation. A five-second caller timeout is therefore not a
demonstrated execution or resource limit.

The image is `alpine:latest`, the mount policy is a short denylist, and a
user-home directory is not excluded. Read-only mounts prevent writes, not
reads or disclosure through logs. Lines 117–128 slice UTF-8 strings at byte
offsets and can panic on a non-character boundary.

### F04 — Ingestion can succeed with a partially erased graph

**Confirmed control-flow defect.** In `src/parser/ingest.rs:289–300`, any
`try_send` error ends the producer with `Ok(())`. A full channel is not a
closed channel. The channel capacity is 256 at line 560; slow embedding or
storage makes saturation plausible during an ordinary large-tree scan.

`ingest_directory` clears old ingested rows first at lines 544–552.
`run_consumer` logs individual embedding/upsert failures and returns success
at line 521. Consequently the system can replace a good graph with a partial
one and still report a successful job. Root-readability preflight and
shutdown waiting are valuable but do not close this gap.

### F05 — Claims and metadata updates are not atomic workflows

**Confirmed race structure.** `src/task/mod.rs::handle_task_claim` loads
state at line 539, checks ownership in memory, then saves at line 642.
Two callers can both read `open`, both pass, and both receive successful
claims. The store's writer mutex serializes writes, not the encompassing
read/check/write operation. Empty session IDs are also accepted and can be
treated as the same holder.

The same lost-update class applies to whole-payload metadata writes, guard
hit counting, and procedure budgets. `handle_task_complete` emits its
consolidation pulse at lines 1319–1327 before saving the completed task at
line 1338. A subsequent save failure leaves a misleading event. The
state-machine's local correctness does not make the workflow transactional.

### F06 — Intake reports task creation without creating a task

**Confirmed false-success contract.**
`src/task/intake.rs::handle_intake_resolve` accepts a caller-supplied task ID
for `created` or `linked`. `src/store/ladybug.rs::resolve_candidate`
updates the intake candidate but neither creates nor verifies the referenced
task. At line 2867 it derives `task_created` solely from the disposition.
Candidate CAS protects the candidate version; it does not prove a task
exists. This is particularly damaging in a product built to prevent work
from disappearing into notes.

### F07 — Mesh publication has two distinct durability holes

**Confirmed ordering defects.**

First, `src/events/bus.rs::Queue::push` drops oldest pulses under pressure.
The mesh subscriber appends its WAL after the source mutation. The comments
in `src/synaptic_mesh/outbox.rs:31–81` explicitly describe this loss window.
They then describe pull/replay as store-to-mesh recovery, but
`import.rs::apply_changed` applies mesh files to the local store. That
opposite-direction replay cannot reconstruct a local row never published.
Another mutation may recover it, but untouched rows have no guaranteed
convergence bound in the inspected path.

Second, `worker.rs::drain_once` commits only when `report.applied > 0`
(line 122). `apply_one` returns `AlreadyApplied` from working-tree content
when the update ID matches (lines 200–204). After a crash between file rename
and git commit, a retry containing only those records skips the commit yet
calls `mark_committed`. The WAL can be acknowledged while the record is
still absent from git history. Idempotence of a file write is not durability
of a git publication.

### F08 — Learning capabilities claim success without doing the work

**Confirmed scaffolding.** `src/judgment/mod.rs:135–141` injects
`MockJudgmentProvider` as the Laya candidate. Selecting `LayaLocal` therefore
does not instantiate `LayaOnnxProvider`. `check_duplicates` does not execute
the configured shadow provider.

`laya_onnx.rs::judge_duplicates` returns an empty vector; `health_check`
returns true. `learning/trainer.rs::train_candidate` returns `Ok(())`
without training. `learning/dataset.rs::build_dedup_dataset` returns zero
counts without reading inputs. `learning/control.rs::apply_control` reports
success with a default status rather than applying the requested state.

The README discloses empty Laya output, which is good. Describing that as
proof of offline independence does not establish implemented inference,
training, calibration or promotion. The small provider/training state
abstractions currently exceed the amount of functioning behavior they govern.

### F09 — Embedding compatibility is underspecified

**Confirmed parser weakness; compatibility risk from inspected store design.**
`src/embed.rs::RemoteEmbedder::try_embed` substitutes `0.0` for nonnumeric
elements and does not validate response width or finite f32 values before
returning. Its retry path attempts to downcast an error to HTTP status, while
the failed response is converted to a formatted error string.

The store is configured with vector dimensions, not a persisted embedding
space identity in the inspected open/write path. The default local choices
include two different 768-dimensional models. Equal width does not mean
compatible geometry. A same-width model switch can silently invalidate
ranking even if the database accepts every vector.

### F10 — Recall filtering can conceal relevant live memories

**Confirmed bounded-candidate defect.**
`src/store/ladybug.rs::search_inner` selects at most `10 * limit` rows with
`array_distance` before Rust filters retired, archived, mesh-conflicting and
registry rows at lines 113–128. Domain matching also happens after this limit.
The SQL filters memory types, not those metadata predicates.

Enough retired nearest neighbors can exhaust the candidate budget and yield
empty recall despite live matches beyond it. A domain can falsely look
unrepresented for the same reason. Comments claiming those rows never spend
the candidate budget are stronger than the query implements. Dense recall
also lacks an independent exact-identifier route for symbol/config/error
lookups. A large semantic-search stack is not necessary to fix that omission.

### F11 — Local storage is not a complete privacy policy

`src/judgment/typesafe_jev.rs::build_request` sends both new and candidate
memory content and metadata to the configured remote judgment endpoint.
Local database ownership does not mean those contents remain local. Regex
secret recognition is useful but is not sensitive-data classification.

`run_consumer` embeds source summaries directly, without the add-memory
entry-point scanner. Remote embeddings therefore create an additional code
egress path. Provider error bodies are incorporated into errors in both
remote adapters, creating potential reflection into logs. Buffer, backup,
learning blobs and git history each need explicit retention/access semantics.

### F12 — Procedure and buffer semantics are weaker than the names suggest

`src/tools/procedure.rs` explicitly documents thin due logic: non-lapsed
procedures are surfaced, not autonomously scheduled from all declared
triggers. `handle_procedure_perform` acknowledges an agent's claim of a
firing; it does not execute or verify the procedure.

At lines 317–349 it appends the episodic line before saving counters. A store
failure leaves a performed-looking log without the corresponding budget
transition. Concurrent callers can spend the same last fire twice.
`src/buffer.rs::append_entry` performs size-check/rename/append without a
per-root lock; simultaneous sessions can race during rollover. The handler
also accepts a caller-provided root without enforcing the promised absolute,
registered-project boundary.

### F13 — The persistence boundary is too permissive

`src/store/ladybug.rs::upsert` merges solely on `Memory.id` at lines
1511–1516. On a match it updates payload but neither checks nor changes the
namespace. A caller using an existing global ID under another namespace can
mutate the original row if an upper layer does not reject it. Namespace
partitioning is a query convention, not a robust write invariant here.

Metadata is an open JSON object; relation endpoint roles are explicitly
descriptive in `memory-vocabulary.v3.json:168`. Lenience is appropriate for
custom descriptive facts. It is inappropriate for ownership, task status,
trust, identity and other enforcement state. General upserts and metadata
replacement provide a weaker boundary than the specialized task API.

### F14 — Documentation and tests sometimes certify intentions

`tests/raw_distance_contract.rs` uses unconditional `assert!(true)` for its
named contracts. `src/embed.rs::remote_embedder_retries_on_500` is empty.
`src/guard/semantic.rs::test_guard_rule_storage` is empty. These tests can
stay green when the claimed behavior is absent.

`docs/ops/security.md` is three lines, not a threat model.
`CLI-readme.md:6` states a blanket daemon/CLI write incompatibility even
though daemon routes support operator commands.
`server.rs:440` advertises archival as not filtering retrieval, while the
store does filter archived rows. Vocabulary filename v3 currently contains
version 7. Handwritten descriptions, duplicate lifecycle predicates and
line-number citations drift independently.

### F15 — Operational complexity is concentrated in large modules

`server.rs`, `store/ladybug.rs`, `task/mod.rs`, and `main.rs` combine many
contracts, transports, storage details and tests. `IntakeStore` is followed
by a concrete `LadybugStore` downcast in its handler. This undermines the
benefit of the interface and makes alternate test implementations difficult.

Read queries run synchronous FFI on blocking threads; general recall computes
distance over matching rows. A returned-row limit is not a scan-work limit.
The serial CI suite controls resource exhaustion but does not prove daemon
behavior under concurrent claims, large ingests, checkpoint contention,
backpressure or process interruption. Write deadlines can expire while the
write later lands; clients need a first-class ambiguous-outcome contract.

## 4. Prioritized improvement recommendations

Effort estimates are engineer-days after reconciliation, excluding native
toolchain installation and unrelated cleanup. P0 blocks the corresponding
safety/correctness claim; P1 closes serious reliability/product gaps; P2
improves maintainability and scale. Estimates overlap and should not be summed
as a precise schedule.

### R01 [P0] Authenticate and separate daemon capabilities

- **Rationale:** F01 makes all local callers equivalent to the operator.
- **Decision:** separate read, ordinary write, guard execution, and admin
  capabilities. Prefer OS-owned local transport where supported; otherwise
  use a protected per-user capability credential, strict loopback binding,
  Host/Origin validation, and an explicit protected administration path.
- **Next action:** inventory every route and caller, then enforce the same
  capability decision before HTTP/MCP/CLI mutations reach application code.
- **Acceptance:** an unauthenticated local client cannot read private graph
  data, delete, edit trust, trigger execution, back up, or stop the daemon.
  Record denied requests without recording credentials or memory content.
- **Effort:** 3–6 days. **Dependencies:** none; informs R02, R10 and R14.

### R02 [P0] Make guard semantics honest and sandbox execution bounded

- **Rationale:** F02–F03 conflate heuristic screening with enforcement.
- **Decision:** evaluate hard deterministic policies over a complete rule
  set; keep semantic candidates advisory. Use explicit verdicts for
  `not_evaluated`, `semantic_screen_pass`, `sandbox_pass`, and rejection.
  Require verified client interception before advertising mandatory coverage.
- **Next action:** replace substring policy dispatch with typed action/rule
  matching; bound container lifetime, CPU, memory, PIDs and captured output;
  pin the image and allow only registered workspace mounts.
- **Acceptance:** a matching fourth rule still rejects; obfuscated/action-type
  fixtures exercise policy parsing; timeout leaves no container/process;
  UTF-8 output cannot panic; no sandbox run yields no sandbox-pass label.
  Metrics: unvalidated actions, rule coverage, timeout cleanup failures.
- **Effort:** 5–10 days. **Dependencies:** R01 for caller authority; R12 tests.

### R03 [P0] Make ingest complete, resumable, and non-destructive on failure

- **Rationale:** F04 loses existing structural knowledge during normal load.
- **Decision:** use blocking backpressure in the blocking producer, distinguish
  closed consumers from saturation, and publish an ingest generation only
  after successful validation. Preserve the prior generation on failure.
- **Next action:** correct producer sends and return structured attempted,
  stored, skipped and failed counts; introduce staged generation activation
  and recovery markers rather than clear-first replacement.
- **Acceptance:** a slow consumer with substantially more than 256 items
  receives every item; failed embedding/store/parse policy is visible;
  interrupted rebuild leaves the last good graph queryable.
- **Effort:** 3–7 days. **Dependencies:** R14 transaction contract for staging.

### R04 [P0] Enforce atomic ownership and lifecycle mutations

- **Rationale:** F05 breaks the project's exclusive-work guarantee.
- **Decision:** introduce store-level conditional task transitions with
  expected version, current holder, nonempty session identity and idempotency
  key. Emit durable consequences only after a committed transition.
- **Next action:** move claim load/check/save under one atomic storage
  operation; apply the same version discipline to update and completion.
- **Acceptance:** synchronized competing claims yield exactly one winner;
  repeated requests by that holder return the same result; stale writes fail;
  save failure emits no completed-task event. Measure CAS conflicts and lease
  takeovers separately from infrastructure errors.
- **Effort:** 4–8 days. **Dependencies:** R14 persistence invariants; R12.

### R05 [P1] Make intake resolution create or verify real tasks

- **Rationale:** F06 produces receipts for work that may not exist.
- **Decision:** `created` performs atomic task creation plus candidate
  resolution; `linked` verifies a compatible existing task. Never derive
  creation success from the requested enum alone.
- **Next action:** define one transaction/result contract and reject missing,
  blank, wrong-namespace or non-task IDs on the link path.
- **Acceptance:** reported `task_created: true` implies a persisted task;
  concurrent retries create one task; failure leaves a resolvable candidate.
- **Effort:** 2–4 days. **Dependencies:** R04/R14.

### R06 [P1] Close both mesh publication loss windows

- **Rationale:** F07 invalidates bounded eventual-convergence expectations.
- **Decision:** durability-critical publication must not depend solely on a
  lossy notification bus. Persist publication intent with the source mutation
  or a durable revision ledger, and reconcile local eligible rows to mesh.
  Acknowledge outbox records only after their update IDs exist in git history.
- **Next action:** fix the AlreadyApplied commit path; add source-to-mesh
  revision reconciliation distinct from mesh-to-store import.
- **Acceptance:** crash at every write/rename/commit/ack boundary, queue
  overflow, and subscriber timeout converge without another user mutation.
  Preserve private rows, quarantine and conflicts. Metrics: unpublished local
  revisions, oldest pending age, committed-but-unpushed records, replay lag.
- **Effort:** 5–9 days. **Dependencies:** R14 mutation revisions; R12.

### R07 [P1] Expose truthful learning capabilities

- **Rationale:** F08 allows successful no-op APIs to masquerade as learning.
- **Decision:** mark incomplete inference/training/control operations
  `unsupported` or `unavailable`, not successful. Keep core recall independent.
  Wire Laya only to an actual loaded provider; identify heuristic behavior
  separately from mocks. Require evaluated evidence before promotion.
- **Next action:** add capability/status tests and remove production mock
  substitution; gate experimental learning APIs with explicit capability
  checks. A model path is not readiness proof.
- **Acceptance:** no weights/runtime means unavailable; a training success
  references an actual artifact; rollback changes a persisted manifest;
  shadow execution has measured observations or is explicitly disabled.
- **Effort:** 2–4 days for honest gating; 10–20 additional days for a complete
  local inference/training/evaluation loop. **Dependencies:** R08 and R12 for
  activating that loop; immediate gating has no dependency.

### R08 [P1] Version embedding spaces and validate remote outputs

- **Rationale:** F09 can silently degrade all semantic behavior.
- **Decision:** persist a space identity including provider/model revision,
  dimensions, normalization and relevant preprocessing. Reject incompatible
  opens; migrate through a separately built and verified vector generation.
- **Next action:** make response parsing reject malformed, wrong-width and
  nonfinite vectors; retain typed HTTP failure status for bounded retries.
- **Acceptance:** switching between different 768-dimensional models is
  rejected without migration; malformed numbers never become zeros; retry
  tests distinguish transient status from invalid input and parse failures.
- **Effort:** 3–6 days. **Dependencies:** R14 schema/version migration.

### R09 [P1] Preserve recall completeness and add exact identifiers

- **Rationale:** F10 can suppress the very constraints agents must recover.
- **Decision:** apply active/domain predicates before the candidate budget,
  or paginate until enough eligible candidates are found with an explicit
  incomplete-result marker at a real work limit. Add a narrow deterministic
  identifier/symbol lane with provenance; do not add a full search platform.
- **Next action:** plant more than `10 * limit` retired neighbors and a live
  exact-symbol match; implement queryable lifecycle fields and exact lookup.
- **Acceptance:** active results survive retired-row saturation; a real
  domain is not reported absent due only to prefilter truncation. Publish
  Recall@k on pinned fixtures, exact-hit rate, latency and incomplete counts.
- **Effort:** 4–7 days. **Dependencies:** R08 for stable evaluation; R14 fields.

### R10 [P1] Enforce privacy at persistence and egress boundaries

- **Rationale:** F11 crosses more trust boundaries than the README explains.
- **Decision:** authorize embedding and judgment egress separately per
  namespace/data class; minimize metadata sent remotely. Keep regex scanning
  as one control, not the definition of private or sensitive data.
- **Next action:** trace add, complete, ingest, intake, buffer, mesh, learning,
  backup and provider-error paths; enforce common scanning/redaction and
  protected file modes at the actual boundary.
- **Acceptance:** private/no-egress namespaces produce no remote requests;
  source ingestion cannot bypass policy; provider errors cannot reflect raw
  sensitive payloads. Document backup, buffer and immutable-git retention.
- **Effort:** 4–7 days. **Dependencies:** R01 and R08; informs R07 activation.

### R11 [P1] Replace capability claims with executable documentation

- **Rationale:** F14 creates operator risk and expensive repeated discovery.
- **Decision:** keep one authoritative capability registry with implemented,
  experimental and unavailable states. Generate tool descriptions and an
  operational capability table from it. Label namespaces as organization,
  procedures as acknowledgment, and guard coverage as client-dependent.
- **Next action:** correct README, CLI daemon guidance, archival tool text,
  learning status and security documentation against current source behavior.
- **Acceptance:** each claimed enforcement mechanism names its interception
  point and failure behavior; public examples match registered tools and CLI;
  vocabulary version is unambiguous. Documentation contract checks detect
  missing/renamed tools and contradictory capability flags.
- **Effort:** 2–4 days. **Dependencies:** immediate disclosure now; R01/R02/R07
  decisions supply final capability wording.

### R12 [P1] Replace ceremonial tests with executable failure contracts

- **Rationale:** F14 and F15 allow false confidence despite an existing suite.
- **Decision:** every behavioral test must execute the named behavior and
  assert observable state. Separate pure tests, native-store tests,
  concurrent workflow tests, and crash/resource tests in CI.
- **Next action:** replace unconditional raw-distance assertions and empty
  embed/guard tests; add the exact regressions described in R01–R10 and R13.
- **Acceptance:** planted regressions fail the corresponding test; suite
  reports nonzero real cases; native tests use bounded databases and controlled
  parallelism. Pin provider fixtures and perform no live inference in tests.
- **Effort:** 4–8 days for the initial risk suite. **Dependencies:** none to
  reproduce failures; implementations above determine passing contracts.

### R13 [P1] Make procedure acknowledgment and buffer writes recoverable

- **Rationale:** F12 splits finite-budget truth from its audit record.
- **Decision:** CAS-protect procedure firing and persist an acknowledgment
  event atomically with the counter change. Project that event idempotently
  into a locked per-project buffer. Do not claim scheduled execution.
- **Next action:** require valid registered roots, serialize rollover, enforce
  entry-size limits and preserve a durable pending-log state when logging fails.
- **Acceptance:** two requests cannot spend one remaining fire; retries write
  one acknowledgment; store failure produces no successful firing log;
  simultaneous rollover loses no entry. Metrics: pending log projections and
  exhausted/late acknowledgments, distinct from actual procedure execution.
- **Effort:** 3–6 days. **Dependencies:** R04/R14; R10 for private file policy.

### R14 [P1] Put invariants behind one typed persistence boundary

- **Rationale:** F13 and F15 leave correctness scattered across callers.
- **Decision:** preserve globally unique IDs but reject namespace mismatch
  on update. Use typed/versioned task, lifecycle, trust and provider state;
  leave descriptive custom metadata extensible. Restrict invariant-breaking
  administration explicitly. Share application commands across transports.
- **Next action:** add conditional mutation APIs, typed errors and migration
  metadata; split task/intake/recall/provider handlers along domain seams.
  Remove concrete intake downcasting. Generate transport schemas from typed
  commands without introducing separate services or another database.
- **Acceptance:** wrong-namespace upsert changes nothing; all transports
  exercise identical state guards; stale full-payload updates fail; schema
  migrations have backup and rollback instructions.
- **Effort:** 6–12 days, delivered by invariant rather than a wholesale
  rewrite. **Dependencies:** none; foundational to R03–R09 and R13.

### R15 [P2] Specify durability, load budgets and recovery evidence

- **Rationale:** F15 leaves operational guarantees dependent on anecdotes.
- **Decision:** define acknowledged-write durability and ambiguous timeout
  semantics. Bound blocking-engine work and request concurrency. Export
  readiness/capability separately from process liveness; retain useful
  metrics across restarts without retaining raw private payloads.
- **Next action:** build a pinned local load/recovery harness covering large
  namespaces, concurrent searches, ingestion, checkpoints, restart and backup
  restore. Record source/vector/schema identity in backup manifests.
- **Acceptance:** publish measured p50/p95/p99 latency and resource envelopes;
  clients can reconcile a timed-out write by operation ID; restore drills
  preserve task/lesson edges and mesh publication state. Choose numerical
  service budgets from measured workload fixtures before claiming capacity.
- **Effort:** 4–8 days. **Dependencies:** R08/R12/R14; no external benchmark
  service or new distributed infrastructure required.

### Execution ordering

Immediate containment: R01, R02, R03, R04; disclose limitations through R11
and make unavailable learning paths honest through R07. R12 begins with
failing regressions. R14 supplies the shared invariants for task/intake,
ingest generations, procedure events and mesh revision recovery. Then close
R05/R06/R08/R09/R10/R13, with R15 measuring the resulting system.

This ordering does not require implementing the optional learning engine to
ship a trustworthy memory/task product. The decided shipped behavior for an
absent engine is explicit unavailability, not successful empty work.

## 5. Over-engineered, under-specified, and fragile areas

### Over-engineered relative to functioning behavior

- Learning has runtime managers, bundle objects, calibration/split concepts,
  active manifests, job states and control requests while training, dataset
  generation and local judgments do no substantive work. Keep these isolated
  as experimental contracts instead of presenting a complete subsystem.
- Repeated transport schemas, rule text and documentation overlays consume
  maintenance effort without strengthening invariants. Consolidate them.
- A cognitive metaphor for every component makes engineering status harder
  to see. Keep the naming, but pair it with ordinary technical terminology
  and a capability state. Metaphor is not a correctness argument.

### Under-specified where guarantees matter

- Three-tier precedence: global/project/task scope is not a policy-resolution
  algorithm or a tenant isolation scheme. Define scope and override rules.
- Enforcement ownership: specify exactly which client interception points
  are installed and what happens when they fail or a client bypasses them.
- Lesson quality: extraction existence is enforced; correctness, novelty
  and usefulness are not. Measure correction/reuse rather than asserting
  that every completed task has improved the system.
- Durable completion: database commits, outbox append, git commit, response
  success and checkpoint are different milestones. Name each guarantee.
- Procedures: due display, acknowledgment and execution are different
  concepts. Trigger strings alone do not make a scheduler.

### Fragile implementation seams

- JSON metadata read/modify/write across asynchronous calls.
- Fallible FFI writes that can finish after the caller times out.
- A lossy bus carrying bookkeeping with durability implications.
- SQL-string construction and duplicated lifecycle filtering predicates.
- Path-sensitive behavior split across registry, buffer, ingest and sandbox.
- A small number of very large modules with domain-specific downcasts.

The remedy is fewer, stronger boundaries—not a service decomposition, a
second search engine, another broker, or a replacement embedded database.

## 6. Security and safety concerns

### Threat model to adopt

Distinguish the owner, another local user/process, an untrusted agent with
shell access, hostile repository content, an untrusted mesh peer, and an
optional remote inference provider. These actors have different authority.
Today several surfaces effectively collapse them into "local caller."

1. **Local API abuse:** protect graph reads and administrative writes, not
   just destructive MCP tool discovery. Loopback reduces exposure, not trust.
2. **Policy bypass:** direct HTTP, raw metadata edits and uninstrumented
   client tools can bypass intended workflow or trust gates. Advertise only
   interception points actually installed and verified.
3. **Sandbox abuse:** restrict roots, resource use, image provenance, output
   and lifetime. Success in an Alpine container is not host-safety proof.
4. **Data egress:** judge requests include candidate metadata as well as
   content. Embedding requests include source summaries. Apply policy before
   either request; sanitize error/log paths independently.
5. **Prompt injection and poisoning:** stored facts and imported rules are
   evidence, not authority merely because they were retrieved. Preserve
   provenance/trust labels in recall and require local promotion for effects.
6. **Integrity:** CAS ownership, namespace consistency and idempotent
   transitions protect against accidental and adversarial stale writes.
7. **Retention:** buffer pruning does not remove backups or git history.
   State those limits explicitly; imported peers cannot be made to forget by
   archiving a local record.
8. **Availability:** native FFI, unbounded captured output and write/checkpoint
   contention need resource budgets. Rust alone does not remove native-engine
   fault or process-exhaustion risk.

No exploit, fresh benchmark, or test pass is claimed here. The source is
sufficient to justify the containment decisions; validation belongs to the
post-reconciliation implementation and review workflow.

## RECOMMENDATIONS LIST

- [P0] R01 — Authenticate and separate daemon capabilities.
- [P0] R02 — Make guard semantics honest and sandbox execution bounded.
- [P0] R03 — Make ingest complete and non-destructive on failure.
- [P0] R04 — Enforce atomic ownership and lifecycle mutations.
- [P1] R05 — Make intake resolution create or verify real tasks.
- [P1] R06 — Close both mesh publication loss windows.
- [P1] R07 — Expose truthful learning capabilities.
- [P1] R08 — Version embedding spaces and validate remote outputs.
- [P1] R09 — Preserve recall completeness and add exact identifiers.
- [P1] R10 — Enforce privacy at persistence and egress boundaries.
- [P1] R11 — Replace capability claims with executable documentation.
- [P1] R12 — Replace ceremonial tests with failure contracts.
- [P1] R13 — Make procedure acknowledgment and buffer writes recoverable.
- [P1] R14 — Put invariants behind one typed persistence boundary.
- [P2] R15 — Specify durability, load budgets and recovery evidence.
