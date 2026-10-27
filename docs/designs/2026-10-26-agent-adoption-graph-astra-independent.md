# Astra: independent agent-adoption and graph review

Date: 2026-10-26. Task: neurostrata-fg5q.
Status: design recommendation; not implementation approval.

## 1. Assessment

Achievable for supported hosts, not universally for every agent. Guarantee
automatic context delivery at observable events; delivery does not prove
comprehension, application, or compliance. Success means useful context
arrives before decisions without user reminders and improves outcomes.

Evidence: neutral brief and targeted source reads. No peer or unified verdict
documents opened. Mandatory memory preflight returned ancillary historical
records; conclusions use the brief and source, not those records. No builds,
live Tebra lookups, or checker runs performed. Worker delegation hit the depth
limit; this design was persisted directly under the design-only exception.

Current source is ahead of portions of the brief:
- `src/server.rs:2641-2707` preserves bounded graph expansions.
- `src/server.rs:1842-1846,1870-1953` surfaces due procedures and domain
  memories outside the access-ranked top five.
- `src/buffer.rs:335-394` already recalls from tagged logs.

Extend these paths rather than creating another independent recall engine.
Exact file lookup remains a materially different missing path.

Top risks: host integration defects; wrong branch attribution; stale evidence
presented as authority; popularity feedback; partial ingestion; private-data
egress through automatic recall.

## 2. Concrete refinements

### A. Shared context service, thin host adapters

Add a bounded context-planning boundary beside `src/server.rs` handlers.
Inputs: authenticated project/source binding, session/event ID, file paths,
optional task/domain hints, caller output budget.
Outputs: schema version, event ID, index generation, applied scope, memories,
evidence paths, freshness, truncation and degradation status.
Distinguish empty, unsupported, unavailable, and partial results explicitly.

Use this service for session start, first relevant file access, task claim,
and explicit recall. Adapters supply observations, not ranking policy.
Server-only triggers cannot observe ordinary file reads: retain host hooks,
but advertise supported hosts/events and coverage gaps honestly.

Repair `plugins/opencode/index.ts:66-106,192-201,229-237,255-274` first.
Beyond endpoint/missing-tool issues in the brief, callbacks reference `ctx`
outside its declared scope. Client sends bare tool names as JSON-RPC methods;
`src/server.rs:571` dispatches `tools/call`. Verify deployed transport before
choosing the repair. Add capability negotiation, response validation, bounded
timeouts, explicit namespace binding, and observable degraded state. A tool
name appearing in prose is not proof of task creation (`index.ts:259`).

Optional recall may fail-soft; mandatory guard failures retain their
configured enforcement policy. Advisory recall must not gain system authority.

### B. Exact file recall before broad graph exploration

Expose `memories_for_file` and bounded `related` through a shared graph-read
capability beside `VectorStore` (`src/traits.rs:219`). Prefer a narrow graph
interface over enlarging a vector abstraction. Unsupported backends must
return unsupported, not empty.

File lookup resolves source identity plus normalized repo-relative path.
Select exact nodes/direct anchors, incoming GOVERNS, ancestor constraints,
then bounded supporting relations. New or unindexed files still receive
matching declarations and parent rules. File nodes have zero vectors
(`src/parser/ingest.rs:419-445`); this path must not require embeddings.

`related` accepts relation/direction allowlists, depth <=2, node/edge/output
budgets, and generation-bound continuation. Default: immediate neighbors.
Return direction, kind, declared/resolved provenance, and why each row appears.
Adjacency is neither proof nor authority. Apply privacy, namespace, retirement,
and conflict filters to intermediates as well as endpoints. No arbitrary
query execution surface.

Reuse expansion/evidence from `src/store/ladybug.rs:224-265`. Keep primary and
expansion budgets separate; graph rows must not carry fabricated semantic
distances. Add path-first candidate selection before general ranking changes.

### C. Identity/publication precede freshness

Separate repository, checkout/worktree, revision, dirty state, relative path.
`src/source_context.rs:57-98` resolves source IDs and export paths; those lines
do not establish branch-versioned storage or branch-correct retrieval.
Test simultaneous worktrees. Suffix recovery must not silently cross projects.

`src/store/ladybug.rs:2152-2240` replays declarations after ingested-node
deletion; unresolved declarations may target another namespace. Cross-scope
links require explicit authorization. Publish completed ingestion generations
atomically, or retain last-good generation with visible incomplete state.
`src/parser/ingest.rs:440-445` logs node-upsert failure and continues: consumer
completion must not imply complete graph coverage.

Freshness needs independent dimensions:
- Source currency: unchanged / changed / missing / unknown.
- Semantic validation: unreviewed / verified / contradicted, with evidence.
- Index coverage: complete / partial / unavailable, with generation.
- Decision lifecycle: proposed / accepted / superseded / withdrawn.

Record hash algorithm/scope, observed revision, observation time. Changed
hashes request review; they do not refute decisions. Unchanged files neither
validate external dependencies nor prove claims. Use lazy local fingerprints,
caches, and byte/time budgets; never hash the repo on every recall.

### D. Typed decisions, not a truthy flag

Extend `src/schemas/memory-vocabulary.v3.json` deliberately. A typed
`metadata.decision` envelope can contain version, choice, rationale,
alternatives, scope, status, authority/provenance, date, and supersession.
A new top-level type is optional; validated lifecycle semantics are not.
Legacy rows remain readable and unclassified; no automatic fact relabeling.

Normative constraints remain rules. Decisions may justify rules but cannot
bypass rule provenance/enforcement requirements. Conflicting decisions/facts
must surface conflict rather than win through recency alone. Validate new
shapes across add, supersede, task extraction, import, and store updates;
preserve extension data. Share rendering across search/snapshot/get/file
recall so status and freshness cannot disappear on another retrieval path.

### E. Embedding identity and replay-safe state

Persist embedding-space identity: model/provider revision, dimensions,
normalization, preprocessing, distance metric. Equal dimensions do not imply
compatible vectors. Refuse incompatible comparisons; retain exact file/graph
retrieval during migration or provider outage. Keep semantic distance,
ranking score, and observations as distinct typed values.

`src/buffer.rs:335-394` returns append success with empty recall on embedding
failure. Add separate append/recall states and retryable event IDs; retrying
recall must not append again.

For procedure triggers (`src/tools/procedure.rs`), distinguish due, offered,
acknowledged, performed. Display must not consume rehearsal budget. Require
idempotent receipts and recovery between counter updates and episodic writes.
Do not claim exactly-once logical processing without replay/crash evidence.

## 3. Disagreements with implied approaches

- More tools alone will not cause adoption. Delivery reliability and useful
  outcomes precede tool count.
- Host hooks are sensors, not the sole correctness boundary. Shell edits,
  external editors, restart, and compaction need coverage disclosure,
  daemon-side invalidation, and replay-aware session state.
- Hash freshness is evidence currency, not truth confidence or rule validity.
- `metadata.decision: true` is inadequate; typed metadata is acceptable.
- Access count measures exposure, not usefulness. Reserve context for new,
  applicable constraints rather than reinforcing historical popularity.
- Deterministic delivery needs no background LLM. Optional semantic
  adjudication belongs behind explicit privacy and cost policy.

## 4. Missing risks

- Local storage does not prevent embedding/snippet/telemetry egress. Apply
  caller egress policy before rendering or embedding; keep receipts private.
- Retrieved prose is untrusted evidence. Imported provenance cannot grant
  permissions or promote text into system instructions.
- Same paths across worktrees; symlink escape; rename/delete; generated files;
  permission errors; source changes between recall and edit.
- Graph hubs, duplicate evidence, query fanout, unbounded namespace state,
  repeated injections after compaction.
- Reconstruction temporarily erases applicability; declared links do not
  prove successfully materialized edges.
- Mesh/export/import must preserve new semantics without sharing local
  runtime receipts, private data, or incompatible embedding assumptions.

## 5. Priority and release criteria

1. Repair host/server contract; instrument unsupported/degraded events.
2. Establish scoped file identity, exact lookup, generation coverage,
   privacy filtering, and replay-safe delivery together.
3. Add typed decision/freshness rendering and bounded related traversal.
4. Tune graph-aware ranking only against independent outcome evidence.

Initial configurable targets: depth 2; 20 returned nodes; 40 evidence edges;
1,500-token automatic context ceiling; 200 ms warm-path target; 1 s recall
wall-time cap. These are proposed budgets, not measured performance.
Expose truncation/degradation; never imply complete constraint coverage when
budget was exhausted. Safety enforcement remains a separate gate.

Acceptance fixtures: unseen file with governing rule; zero-access new memory;
wrong namespace; dirty second worktree; missing target; cyclic/high-degree
graph; partial ingest; failed embedder; duplicate event; restart between
state/log writes; private intermediate neighbor. Require deterministic scope
correctness and no forbidden egress.

## 6. Better alternatives

Recommended: exact context + bounded graph expansion + thin host adapters.
Best balance of automatic behavior, availability, and portability.

Cheaper: repaired adapter, compact session snapshot, file-matched rule/decision
capsule. Broad graph exploration must justify itself in benchmarks; scoped
exact lookup still matters.

Heavier: background semantic agents. Reject as default: adds privacy exposure,
nondeterminism, and cost before delivery defects are resolved. Keep semantic
analysis explicit, optional, and bounded.

## 7. NeuroPlasticity loop

`plasticity.json:4-8` names desired tools and technologies: instruction
following, not reminder-free adoption. Separate contract reliability,
unprompted adoption, and decision/outcome usefulness suites. Adoption tasks
must not name NeuroStrata or desired tool calls.

`plasticity.json:67-142` relies on weak file/text proxies: database files do
not prove useful memory; commit messages do not prove completion; absence of
dangerous strings does not prove guard interception. Grade typed traces,
persisted task transitions, applied scope, pre-action delivery order, and
independent artifact/outcome assertions. Destructive scenarios use synthetic
disposable fixtures only.

Provider-unavailable runs are invalid infrastructure runs, not behavioral
score 0. Report attempted/valid/invalid counts and availability separately.
Pin model/provider, revision, dataset, embedding space, randomness. One epoch
cannot justify optimization claims. Use held-out repository/task families,
repeated seeds, cold-session and resumed-session cohorts.

`src/recall_metrics.rs:64-149` uses restart-reset counters and namespace-only
recent-ID joins. These can misattribute another session's promotion and omit
empty recalls. Add bounded durable local receipts keyed by session/task/event:
attempts, empty/error results, exposed IDs, acknowledgments, relevant outcomes,
latency, token cost, and recovery status. Cap namespace count and retention,
not just IDs per namespace. Exposure/promotion is not a Gold correctness label.

Compare no-memory, repaired-adapter, exact-file, graph-enabled arms.
Primary metric: correct task outcomes under identical conditions.
Secondary: applicable-rule recall, stale-advice rate, unwanted context rate,
automatic delivery coverage, graph evidence usefulness, cost/latency, replay
duplication. Privacy violations and missed mandatory guards are hard failures,
not weighted against convenience. Freeze held-out grader; optimize candidate
policies in shadow mode, promote with recorded evidence and rollback.

These are reconciliation recommendations, not shipped-functionality claims
or separately approved implementation scope. Parent task neurostrata-3dz2
owns reconciliation and implementation disposition.
