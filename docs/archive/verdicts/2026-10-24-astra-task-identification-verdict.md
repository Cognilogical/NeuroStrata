# Astra: speech-to-task capture — independent verdict

Status: design material for arbitration; not an approved implementation spec.
Parent: neurostrata-39j3, owner bosun. Verdict task: neurostrata-sr3o.
Evidence: supplied brief, current source, governing rules only.

## Verdict

This is principally an observability and durable acknowledgement problem.
Prompting teaches recognition; tooling records evidence; runtime boundaries
make ignoring observed evidence consequential. A classifier alone cannot
enforce recording. An existing claimed task does not cover every new intent.

Recommend three cooperating mechanisms: compact heuristic; Core intake ledger;
capability-declared observation/review boundaries. No new memory type, task
status, procedure trigger, or claim-time guard. Never automatically execute or
claim work discovered in conversation.

Guarantee disposition of observed evidence, not capture of unseen speech.

## Corrections to brief

- Template is now `src/task/mod.rs:1383-1433`. It gives task-vs-procedure
  examples at :1413-1430, but no speech-recognition heuristic.
- append_log already performs additive domain recall after successful append
  (`src/buffer.rs:178-206`, :250-265). It searches from tags, not by rereading
  historical topic markers. Recall is not merely a future dependency.
- Snapshot accepts domain and builds an additive strap
  (`src/server.rs:1700-1726`). Base access-count ranking remains :1684-1698;
  task prefix already precedes ranking at :1670-1673.
- Reminder registry remains descriptive, not a runtime dispatcher
  (`src/task/wiring.rs:47-53`, :136-166).
- `plugins/` was absent in this checkout; adapter directory below is proposed.

## M1 — outcome-recognition heuristic

**Recommendation:** ship compact heuristic in template and tool descriptions.

### Options considered

1. Template alone: cheap, but ignored statements remain invisible; insufficient.
2. Vocabulary/new type: teaches storage category, not recognition; reject.
3. Template plus creation/intake descriptions: select; broad teaching plus
   instruction adjacent to action.
4. Reminder wire alone: reject; naming a wire does not make it fire.

### Design

Recognition test: an unresolved actionable outcome requested by the user,
committed to by an agent, or proposed for decision is task-shaped.
Requests/commitments -> create or link a real task now. Speculation -> review
candidate, not automatic obligation. “Later” changes scheduling, not capture.
An agent's concrete promise counts even when the user never said TODO.

Positive examples: “fix X”; “we also need Y”; “follow up on Z”;
“I'll add regression coverage.” Negative examples: quoted examples, negated
requests, completed work, standing facts/rules, and hypothetical discussion.
“X is broken” without a requested outcome -> investigation candidate, not an
invented repair mandate. An immediate explanatory question may belong to the
already tracked conversation task; no task per sentence or tool call.

Before continuing, inspect existing tasks beyond `ready:true`: blocked and
claimed work may match. Link only when acceptance scope covers the new outcome.
Otherwise create an open task with source, owner and concrete next action.
Claim only work being started. Similar wording never authorizes silent merging.

Data: versioned shared heuristic text and examples; M2 carries provenance.
Call sites:
- `src/task/mod.rs:1383`: template; :385: task_create implementation.
- `src/server.rs`: tool descriptions; append_log declaration at :374-384.
- `src/task/mod.rs:1317`: task snapshot prefix, independent of top-five budget.

### Failure modes

| Failure | Behaviour | Safety |
|---|---|---|
| Agent ignores prose | M2/M3 expose observed pending receipts | No guarantee |
| Quoted/negated cue | Negative examples and explicit review | No auto task |
| Similar existing task | Require scope match | Preserve distinct work |
| Task fragmentation | Track outcomes, not each step | Existing scope may fit |

### Blast radius

Template, descriptions, snapshot instruction and fixtures. No signature or DB
migration, vocabulary bump, or mesh change. Existing project AGENTS.md needs
an explicit setup update, not a silent rewrite.

### Acceptance criteria

1. Template/description fixtures contain trigger, negatives and call sequence.
2. Labeled synthetic corpus covers user and agent statements, quotation,
   negation, completed work, blocked duplicates and recurring cycles.
3. Agent evaluation reports task-level precision/recall with denominators;
   improvement is measured, not inferred from better wording.
4. No candidate is presented as authorization to claim or execute work.

**Dependencies:** none for teaching; M2/M3 provide stronger guarantees.

## M2 — durable operational intake ledger

**Recommendation:** ship local receipts/candidates and a narrow resolution API.

### Options considered

1. Every lexical match becomes task: reject; pollution, injection, compulsory
   extraction for false positives.
2. Candidate memory type or fact metadata: reject; contaminates knowledge,
   recall and export semantics. A proposal is not a fact.
3. Ephemeral suggestions only: reject; ignored tool responses evaporate.
4. Operational ledger with explicit disposition: select; durable without
   weakening task lifecycle.

### Design

New Core module `src/task/intake.rs`; Ladybug operational tables, not Memory
rows. Existing Memory schema anchor: `src/store/ladybug.rs:1436`.

Receipt schema v1:

```text
receipt_id, namespace, session_id, source_kind, source_id, source_revision,
source_digest, observed_at, observer, detector_version, observation_scope,
review_state, candidate_count, candidates, decision_history

source_kind: user_turn | agent_turn | log | memory_write
observation_scope: full_turn | excerpt | summary
review_state: unreviewed | reviewed
```

Candidate schema:

```text
candidate_id, source_span, bounded_quote, proposed_outcome, origin,
disposition, task_id, reason, reviewer, reviewed_at

origin: cue | agent_proposal
disposition: pending | created | linked | dismissed
```

Empty candidates does not mean reviewed. A summary never implies full coverage.
Final decisions are immutable; corrections create a revision. No numeric
confidence masquerades as calibrated probability.

New tools:
- `neurostrata_task_intake`: ingest attributed source and optional structured
  agent proposals; deterministic Core cues add candidates; persist receipt.
- `neurostrata_task_intake_list`: namespace/session filters and cursor.
- `neurostrata_task_intake_resolve`: expected revision plus per-candidate
  create/link/dismiss decisions; explicit review acknowledgement even at zero.

Cue detector is bounded and deterministic, initially documented English cues.
It does not adjudicate user intent. Agent reviews context and may add missed
candidates. No hosted classifier or new paid inference dependency. Input is
untrusted data, never instructions to create/claim/execute. Resolve namespace
explicitly; never guess a project for durable mutation. Scan secrets before
storing quotes; rejected/oversized input returns explicit non-observation.

Exact replay identity: namespace + source ID + revision + span. Different
wording never silently merges. Replay after timeout returns the same mapping.
Concurrent resolution uses compare-and-swap. Task creation and receipt
transition commit atomically: validate/build task via shared existing creation
logic, precompute embedding, then transactionally persist both. Link validates
namespace and task existence. Retired/done targets cannot silently hide new
work: require an explicit applicable scope decision or create new work.
Dismissal requires reviewer and reason.

Introduce `IntakeStore` capabilities beside `VectorStore`
(`src/traits.rs:211`), including transactional resolution. Unsupported backends
return unsupported, never empty queues. Extract shared task payload validation
from `handle_task_create` (:385-506), preserving its public API. No existing
transaction primitive is assumed available merely because the DB supports one.

Pending candidates persist across restart, never auto-expire. Capturing
session's operator owns review; concrete next action: review source and resolve.
They are not scheduled work and must be resolved at the next strict boundary.
Reviewed receipts remain audit records; no automatic deletion in this change.
DB backup includes ledger. Explicit administrative JSON export includes schema
version; ordinary knowledge/graph export excludes it. No mesh propagation.

Additive response envelope:

```text
task_intake: {
  status, receipt_id, observed_count, candidate_count, unresolved_count,
  candidates, next_cursor, error
}
status: observed | not_observed | unavailable
```

Known zeros and empty arrays are explicit. Unavailable counts are null, not
fake zeros. Pagination exposes total pending count; truncation cannot imply
clearance. A candidate is NOT yet a recorded task: only created/linked with
task_id establishes that fact. Zero-candidate acknowledgement records an
agent's review, not proof of perfect semantic recognition.

### Failure modes

| Failure | Behaviour | Safety |
|---|---|---|
| Ambiguous/injected prose | Candidate requires review | No automatic work |
| Replay/concurrent accept | Stable IDs, transaction, CAS | At most one task |
| Embedding/DB failure | No successful disposition | No false task claim |
| Similar but distinct intent | Suggested links only | No lossy auto merge |
| Secret or missing source | not_observed and reason | No invented coverage |
| Crash during review | Durable pending receipt | No silent expiry |

### Blast radius

New intake module/schema, Ladybug operations, IntakeStore implementations/test
doubles, server tools/dispatch, shared creation builder, backup/admin export.
Additive DB migration; operational schema v1, NOT memory-vocabulary v6.

Task enum, task_complete, EXTRACTED_FROM and existing task gate invariants stay
unchanged. Candidates neither satisfy Zero-Action Start nor create extraction
obligations. Created tasks use every existing lifecycle rule. Mesh allowlists
stay unchanged: `src/synaptic_mesh/outbox.rs:114`, `import.rs:61`.

### Acceptance criteria

1. Replay and concurrent accept produce exactly one task per candidate.
2. Fault injection around commit never yields accepted-without-task.
3. Pending receipts survive restart; namespace/session isolation is tested.
4. Zero-candidate receipt requires acknowledgement to become reviewed.
5. Candidates never appear in task_list, memory recall, graph export or mesh.
6. Intake never claims/completes tasks or grants extraction exemptions.
7. Failed writes/unsupported storage return unavailable, never success/zero.
8. Report observed turns, reviewed receipts, accept/dismiss counts, capture
   latency and task-link rate. Semantic recall uses labeled corpus; Core cannot
   measure unseen real-world speech as a denominator.

**Dependencies:** schema and transaction capability precede APIs. M1 supplies
review heuristic. No external NLP or new memory type dependency.

## M3 — instrument observable boundaries honestly

**Recommendation:** ship additive Core capture; isolate stronger host-specific
observation/blocking in new `plugins/` adapters.

### Options considered

1. task_create/claim only: reject as detector; missed utterance never reaches
   either call. Useful as acknowledgement/link points.
2. append_log/memory writes: select as universal fallback; coverage remains
   partial and agent-dependent.
3. Guard scans prose claims: reject; S1 establishes no interception point.
4. Client message/tool boundaries plus Core review check: select only where
   the host supplies real boundaries and capabilities are declared.

### Design

append_log and add_memory call shared intake observer on supplied new text.
Anchors: `src/buffer.rs:133`, secret scan :162, append :178, recall :205;
memory write handler `src/server.rs:1330`. Preserve original operation result
and ALL recall results; append an independent intake result.

Log failure, disabled log, memory failure and intake failure are reported
separately. Never label unpersisted intake observed. Intake may record text
received by a failed primary write, with provenance marking that failure.
Capture never depends on nonempty tags. An old caller with ambiguous/missing
namespace receives not_observed, not an inferred namespace mutation.

Process new source revision, not whole transcript repeatedly. Exclude generated
intake reports and task API responses to prevent recursive candidates. Shim and
append_log observations share IDs only when provenance proves the same source.

Snapshot adds pending-intake strap outside ALL recall budgets. Show source and
exact resolution call. task_create/claim responses may acknowledge explicitly
associated receipt IDs; they cannot imply other candidates disappeared.

New Core intake-check operation reports scoped unreviewed/pending receipts,
coverage mode and last observed sequence. A client adapter may hard-block its
next mutation or session-finish boundary until review succeeds. Capture user
turns and completed assistant messages where supported; flush before mutation.
This is NOT intercepting an assertion before the user reads it. Unsupported
message/tool boundaries are reported as unsupported, with advisory Core output.
Adapters and capability tests belong in `plugins/`, not Core.

Do not alter `src/guard/semantic.rs` or claim speech-time enforcement. An
action-time payload cannot recover omitted intent. An independent intake-check
can precede an observed action, but only judges captured receipts. Existing
task gate stays intact. New optional intake gate blocks only configured
client/project boundaries and declared session scope, not unrelated tasks
throughout a namespace. Session resume explicitly adopts unresolved receipts
from its predecessor; snapshot exposes namespace-wide pending count so other
sessions' receipts are visible without silently becoming its obligations.

Strict mode fails closed on intake-store outage; advisory mode explicitly
reports unavailable. Mode is declared. A host that permits bypass of the
boundary must not advertise that path as enforced.

Teaching is NOT_ENFORCED. Actual observer surfacing is PARTIAL. A blocking wire
may describe only its real boundary: unresolved OBSERVED receipts, never all
task-shaped conversation. Reminder fires_on enum stays unchanged; a registry
entry alone earns no execution or coverage claim.

### Failure modes

| Failure | Behaviour | Safety |
|---|---|---|
| Agent omits log/memory calls | Core reports partial coverage | No universal claim |
| Host lacks message hook | Unsupported; advisory mode | No fictional guard |
| Review ignored | Strict adapter blocks next boundary | No silent clearance |
| Store unavailable | Strict blocks; advisory errors | No hidden fallback |
| Claimed task is unrelated | Candidate needs scope decision | No blanket exemption |
| Summary omits statement | Coverage stays summary | No false completeness |
| Cue flood | Bounded input, explicit rejection | No silent truncation |

### Blast radius

`src/buffer.rs`, `src/server.rs`, task snapshot plumbing, intake-check/API,
new adapters and optional wiring entries. Additive response fields; no recall
displacement. No guard evaluator, reminder enum, vocabulary or mesh changes.
Client signatures stay in adapters; each must prove actual host capabilities.

### Acceptance criteria

1. Untagged log and memory writes exercise independently reported intake.
2. Original reply fields and recall remain intact on intake failure.
3. Snapshot surfaces pending receipts regardless of access_count.
4. Strict adapter blocks mutation/finish on pending observed receipts;
   resolution unblocks. Unobserved prose is never claimed covered.
5. Restart/resume exposes pending work without a reminder dispatcher.
6. Dismissal/zero acknowledgement is audited; replay never recurses.
7. Counters distinguish full turns, summaries, failures and unsupported
   observers. Host tests inject both user and agent statements.

**Dependencies:** M2 before observers/gates; actual client capability before
advertising blocking wires. Preserve existing additive recall now.

## Six direct answers

1. Primarily runtime observability plus durable tooling. Prompts teach
   semantics; data support is operational receipts, not knowledge vocabulary.
2. append_log/memory writes observe supplied text. task_create/claim acknowledge
   recording. File checks are late backstops. A capable client observes more
   speech; none of these grants universal claim-time interception.
3. Yes: local operational candidates, pending -> created/linked/dismissed.
   No mesh travel, task status expansion or extraction requirement.
4. Highest leverage: compact heuristic beside tools and template, reinforced by
   concrete pending receipt IDs in real replies. Vocabulary and reminder names
   cannot substitute for runtime behaviour.
5. Preserve gate/completion/extraction. New cost: operational schema,
   transactional creation and additive observation interfaces. Candidates
   cannot satisfy a task requirement.
6. Decisions above close the alternatives. Parent task **neurostrata-39j3**,
   owner **bosun**, owns next action: arbitrate M1-M3; record selected decisions;
   create and assign implementation tasks for teaching, transactional intake,
   and each approved adapter with the acceptance criteria above. No code or
   checker invocation belongs to this independent verdict. Parent tracks
   arbitration, not a claim that implementation is approved or complete.

## Delivery boundary

Worker persistence delegation was attempted; subagent depth limit rejected it.
Design persisted directly under the design-only exception. No implementation,
build, test, checker, commit or push was performed by this seat. Integration
and post-arbitration validation remain with the orchestrator's tracked task.
