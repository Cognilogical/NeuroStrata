# Two-seat spec review: Argon (Gemini 4 Argon) and K3 (Kimi K3) on the local self-improvement design spec

**Date:** 2026-10-21
**Document under review:** `docs/superpowers/specs/2026-10-18-local-self-improvement-design.md`
as landed on `main` in `5759d36`.
**Authority:** owner directive, 2026-10-21 — "run it by Argon and K3; if they are
good, then I am good."

---

## 0. Method and provenance

Two independent architect seats ran concurrently against the same brief, with no
sight of each other's verdict.

| Seat | Model | Subagent seat | Session |
|---|---|---|---|
| Argon | Gemini 4 Argon (Copilot) | `gemini-arch` | `ses_ea3b759f3ffe0PD7zzznd2JRX2` |
| K3 | Kimi K3 (Copilot) | `k3-arch` | `ses_ea3b759f2ffeXKmu06nD9Qncey` |

Both seats received: the spec as landed on `main`; the implementation plan as
context (not under review); the 2026-10-20 panel record as prior art; the
owner's settled `max_candidates` ruling, held settled and not to be re-litigated;
a read-only rule; and the Q1–Q6 frame ending in APPROVE / APPROVE-WITH-CHANGES
/ REJECT with required-versus-optional changes and an explicit "may the spec
leave Draft" answer.

The read-only and citation rules, verbatim from the brief: *"Every claim that
changes a conclusion must be verified against source in this repo and cited as
file:line with the exact quoted line. If you cannot verify something, say so
explicitly rather than asserting it. Disagreement is the useful output. Do not
flatter."*

Neither verdict was relayed to the other seat. Every claim that changes a
conclusion was re-verified by the orchestrator against source at HEAD before it
was recorded here.

### 0.1 Orchestrator verification of K3's claims (HEAD `5759d36`)

| # | K3 claim | Result | Source |
|---|---|---|---|
| 1 | dedup filters `score >= 0.85` | **verified** | `src/server.rs:1158` `.filter(\|c\| c.score >= 0.85)` inside `embed_dedup_and_upsert` (`:1136`) |
| 2 | hardcoded candidate count `5`, no `max_candidates` read | **verified** | `src/server.rs:1154` `.search(namespace, vector.clone(), 5, false, false)`; `max_candidates` has zero readers outside `src/config.rs` |
| 3 | duplicate error already names `neurostrata_supersede_memory` | **verified** | `src/server.rs:1166`; zero hits for `neurostrata_edit_memory` in `src/` |
| 4 | `embed_dedup_and_upsert` shared by `add_memory` and inline task completion | **verified** | `src/server.rs:1136`, shared `stamp_new_memory` at `:1123` |
| 5 | `score` is a boosted **distance** (lower = closer) | **verified** | `src/store/ladybug.rs:1046` `array_distance(...) AS dist ... ORDER BY dist ASC`; `:1099-1100` `capped_gain = (gain + source_gain).min(0.5 * distance); boosted_distance = distance - capped_gain`; `:1106` `score: boosted_distance` |
| 6 | expansion sentinel is `1.0`, not `10.0` | **verified** | `src/store/ladybug.rs:1418` `const EXPANSION_BASE_SCORE: f32 = 1.0;` (comment `:1416` "before the old 10.0 sentinel"), assigned `:1437` |
| 7 | primaries/expansions discriminated by `EvidenceKind`, not score | **verified** | `src/store/ladybug.rs:1447-1456` final sort keys on `EvidenceKind::DirectMatch` |
| 8 | `STRUCTURAL_MEMORY_TYPES` = the five structural types only | **verified** | `src/store/ladybug.rs:677-683` — `directory`, `file`, `markdown`, `freshness_flag`, `guard_event` |
| 9 | `task` excluded only when `include_tasks` is false | **verified** | `src/store/ladybug.rs:1036` `if !include_tasks {`; also `:1201`, `:1257`, `:1335` |
| 10 | `symbol`/`code_ast` are not excluded today; `code_ast` aliases `symbol` | **verified** | not in `STRUCTURAL_MEMORY_TYPES`; `src/traits.rs:356-358` asserts `v["type_aliases"]["code_ast"] == "symbol"` |
| 11 | retired / archived / mesh-conflicting filtered in the store; `valid_to` null means unbounded | **verified** | `src/store/ladybug.rs:1073`, `:1080`, `:1083`; `is_retired_at` at `:2089-2102` |
| 12 | `max_candidates` exists, default 5 | **verified** | `src/config.rs:27-29` field; `:53-55` `default_max_candidates() -> 5` |
| 13 | dedup checker enabled by API-key presence | **verified** | `src/daemon.rs:462` `if config.judgment_api_key.is_some()`; `src/judgment/mod.rs:79` |
| 14 | `check_duplicates` short-circuits with no key | **verified** | `src/judgment/mod.rs:104-112` returns `None` when `judgment_api_key.is_none()` |
| 15 | Jev adapter posts `{base}/evaluate`, parses `answers`, uses `noul` | **verified** | `src/judgment/typesafe_jev.rs:126`, `:76-85`, `:41` |
| 16 | stdio proxy exists; no `connection_id` plumbing today | **verified** | `src/main.rs:1725-1726`, `:1752-1753`, `:1802` `server::start_mcp_proxy`; zero hits for `connection_id` in `src/` |
| 17 | `remaining_fires` is not a store-layer concept | **verified** | zero hits in `src/store/ladybug.rs` (consistent with the spec's §3a: procedure counters are local) |
| 18 | plan Task 2/3 wire `limit = config.deduplication.max_candidates` | **verified** | plan `:212`, `:232`, `:362-364`; header `X-NeuroStrata-Conn-Id` at `:495` |
| 19 | spec's external TypeSafe claim (`/v1/systemone`, `noul`) | **verified externally** | public API reference: `POST https://api.typesafe.ai/v1/systemone`; wire names `state` and `noul` are documented request fields. The repo adapter's `{base}/evaluate` + `answers` (`typesafe_jev.rs:126`, `:76-79`) is therefore the thing to validate against fixtures, exactly as the spec says |
| 20 | spec defects #5 and the `10.0` sentinel are stale at HEAD | **verified** | spec `:62` still says sentinel `10.0`; spec `:69-71` and `:220-221` still describe `neurostrata_edit_memory`; source says otherwise (§0.1 rows 3, 6) |

**20 of 20 verified. No K3 claim was contradicted by source.**

---

## 1. K3's verdict — **APPROVE-WITH-CHANGES**

### 1.1 What K3 confirmed

- Design coherence with the stated Intent: passive local learning, no user
  labeling, dedup as the first vertical slice, one local data boundary, Jev
  opt-in only. Phase sequencing (0 export spike → 1 observation + raw distance
  → 2 dataset study → 3 training → 4 promotion → 5 expansion) is respected by
  the body; thresholds are consistently deferred to Phase 2.
- The technical core of Phase 1: `score` is a boosted distance, so `>= 0.85`
  keeps only *far* primaries plus every expansion row; removing the filter and
  querying raw distance is the correct fix shape.
- The owner ruling is carried coherently: spec `:401-402` (K = configured
  `deduplication.max_candidates`) matches plan Task 2 (`:212`, `:232`) and Task 3
  (`:362-364`), which replace the hardcoded `5`.
- The invariants that matter are stated: Lock 2 is never blocked by a
  non-canonical match (`:213-214`, `:218-219`, `:408-412`); exact canonical
  duplicates reuse the existing extracted memory (`:208-211`, `:406-408`);
  statistical scores never auto-link (`:211`, `:408`, `:412`); learning stays
  local and Jev is explicit-only (`:37-39`, `:309-311`, `:326`, `:453`);
  model/trainer failure fails soft (`:311-314`, `:319`, `:443`); a hint never
  blocks completion (`:219`).

### 1.2 K3's required changes (six)

| # | Change | Spec site |
|---|---|---|
| R1 | Fix the stale sentinel fact: expansions now carry `EXPANSION_BASE_SCORE = 1.0` (`src/store/ladybug.rs:1418`, `:1437`), discriminated by `EvidenceKind`, not score | `:61-62` |
| R2 | Fix the stale defect #5: the duplicate error already names `neurostrata_supersede_memory` (`src/server.rs:1166`); Phase 1 must keep it aligned, not "fix" it | `:69-71`, `:220-221`, `:418` |
| R3 | State Phase 1's disposition of the existing judgment call: `DeduplicationChecker::check_duplicates` (`src/server.rs:1162`) is bypassed unless provider `jev` is explicitly selected; `auto`/`heuristic` record candidates and apply only the canonical-key decision until Phase 3 shadowing | new clause near `:399` |
| R4 | Mark new-versus-existing exclusions: the five structural types are excluded today (`src/store/ladybug.rs:677-683`), `task` only under `include_tasks=false` (`:1036`); `symbol`, `code_ast`, and `auto-ingestor` exclusion is **new** behavior added to the dedup path only | `:170-174` |
| R5 | Move the post-Phase-2 hint-verification criterion out of Phase 1 (it cannot execute before Phase 2); the negative assertion already at `:404-405` covers Phase 1 | `:413-415` |
| R6 | Add the invariant: observation/dataset persistence failures never fail a memory write or task completion — the learning substrate is best-effort beside the write path | near `:219` |

### 1.3 K3's risks for starting implementation

- **R1** An implementer reads the defect list literally, "fixes" an
  already-fixed error string, or hunts a `10.0` sentinel that no longer exists.
- **R2** The Phase 1 disposition of the judgment path is ambiguous today; two
  implementers could ship different behavior for `auto` + key present.
- **R3** Excluding `symbol`/`code_ast` narrows what can exact-match; that is a
  product decision wearing a filter's clothes.
- **R4** Phase 0 is a hard external dependency with unknown outcome — schedule
  risk only, the spec already fails soft.
- **R5** JSONL append/recovery is specified at record granularity but untested
  against a real filesystem; Phase 1 acceptance should name a crash-during-append
  case.

### 1.4 K3's optional suggestions

Name the connection-ID header in the spec; drop the
`learn reset --purge-observations` flag shape to the plan; note that
`remaining_fires` liveness is a procedure-runtime convention the store does not
enforce yet; record a starting `schema_version` value and its evolution rule.

---

## 2. Argon's verdict — **APPROVE-WITH-CHANGES**

### 2.1 What Argon confirmed

- The spec's technical spine is correct at HEAD: `score` is a boosted
  **distance** (`src/store/ladybug.rs:1046`, `:1100`, `:1106`), the dedup path
  filters it as if it were similarity (`src/server.rs:1158`), the candidate
  count is hardcoded to `5` (`:1154`), and `similarity_threshold` (default
  `0.85`, `src/config.rs:21`, `:47-48`) is being read as a cosine.
- `embed_dedup_and_upsert` (`src/server.rs:1136-1180`) really is shared by
  `add_memory` and inline task completion (`src/task/mod.rs:1181-1188`), and it
  sits immediately before the Lock 2 gate (`src/task/mod.rs:1195-1197`).
- The owner's `max_candidates` ruling is carried coherently across spec
  `:401-402` and the plan's Task 2/Task 3 wiring.
- The safety fallbacks hold: the local path needs no Jev credential, a missing
  training runtime cannot block collection, and a local-model failure allows the
  write through.

### 2.2 Argon's required changes (three)

| # | Change | Spec site |
|---|---|---|
| A1 | Fix the two stale source facts — the expansion sentinel is `EXPANSION_BASE_SCORE = 1.0` (`src/store/ladybug.rs:1418`, `:1437`), not the legacy `10.0`; and the duplicate error already names `neurostrata_supersede_memory` (`src/server.rs:1166`), so the remaining change is only to also name `neurostrata_archive_memory` | `:62`, `:69-71`, `:220-221`, `:418` |
| A2 | Exclude `is_mesh_conflicting` rows (`src/store/ladybug.rs:698-700`, applied `:1083-1085`) from `nearest_by_raw_distance` **and** from canonical exact-match reuse, so the dedup path never blocks on, hints toward, or reuses an unresolved mesh conflict | `:167-199` |
| A3 | Specify outcome capture across all three `task_complete` Lock 2 paths: `ThalamicPulse` emits `Superseded` only on inline extraction (`src/task/mod.rs:1199-1215`), so task completion outcomes must be captured explicitly on Path 1, Path 2, and Path 3 rather than inferred from the bus | `:161-163`, `:416` |

### 2.3 Argon's optional refinements

1. Name `K = deduplication.max_candidates` in the candidate-selection section
   so the normative body is self-contained.
2. State the precedence rule when a connected component spans the training
   window and the forward-time evaluation window.
3. Push `symbol`, `code_ast`, and `auto-ingestor` exclusions into the Cypher
   `WHERE` clause so `FETCH_MULTIPLIER` (`src/store/ladybug.rs:1011`) cannot
   starve a small `max_candidates`.

### 2.4 Argon's risks to starting implementation

1. **Lock 2 ordering on reuse** — when canonical exact-match reuses an existing
   memory ID, the `metadata.extracted_from` update must be visible to the
   gate's own re-read (`memories = list_now().await?` at
   `src/task/mod.rs:1189`, `gate::extraction_exists` at `:1190`).
2. **Phase 0 bridge** — the `laya-train` → `laya-onnx` artifact boundary is
   unverified upstream; correctly isolated as a Phase 0 gate.
3. **No hash crate** — `Cargo.toml` carries no SHA-256/BLAKE3 dependency today
   (and `GuardedActionFired.payload_hash` is only a `u32`,
   `src/events/event.rs:53`), so Phase 1 needs a deterministic 256-bit hash
   dependency added for `blobs.jsonl` and canonical keys.

### 2.5 Orchestrator verification of Argon's claims (HEAD `5759d36`)

| # | Argon claim | Result | Source |
|---|---|---|---|
| 1 | Fact #1 — API-key gating of the checker | **verified** | `src/judgment/mod.rs:79`, `:110-112`; `src/daemon.rs:462-463` |
| 2 | Fact #2 — inverted `>= 0.85` filter over a hardcoded `5` | **verified** | `src/server.rs:1154`, `:1158`; `src/config.rs:21`, `:47-48` (`0.85`), `:53-55` (`5`) |
| 3 | Fact #3 — distance ordering and boost; sentinel claim | **verified, one stale** | `src/store/ladybug.rs:1046`, `:1100`, `:1106` verified; `10.0` is stale — `:1418` is `1.0` (see K3 row 6) |
| 4 | Fact #4 — shared write path and Lock 2 gate | **verified** | `src/server.rs:1136-1180`; `src/task/mod.rs:1181-1188`, `:1195-1197` |
| 5 | Fact #5 — duplicate error tool name | **verified, stale** | `src/server.rs:1166` names `neurostrata_supersede_memory`; zero hits for `neurostrata_edit_memory` in `src/` |
| 6 | Fact #6 — TypeSafe adapter protocol | **verified** | `src/judgment/typesafe_jev.rs:25` base URL, `:126` `/evaluate`, `:87` `probability`, `:76-85` `answers` |
| 7 | Fact #7 — `laya-onnx` is a dev-dependency at `Cargo.toml:58-59` | **not true on `main`** | `main`'s `Cargo.toml` is 56 lines and has no `laya` entry. The citation holds on the **probe branch** `4e41775` (`Cargo.toml:59`). The spec's own Fact #7 does not make this claim, so no spec edit follows. Recorded as a provenance discrepancy, not a conclusion change |
| 8 | Spec `:401-402` carries `K = deduplication.max_candidates` | **verified** | spec `:401-402`; plan `:212`, `:232`, `:362-364` |
| 9 | `K` is not named in the candidate-selection body | **verified** | spec `:167-179` named only "top-K"; now named there (optional 1 applied) |
| 10 | `FETCH_MULTIPLIER` can starve a small `K` when exclusions filter in Rust | **verified** | `src/store/ladybug.rs:1011-1012`, post-fetch filters at `:1073-1085` |
| 11 | Canonical key must be computed after location stamping and before timestamp stamping | **verified as covered** | plan Task 3 Step 7 already requires one shared canonical normalization helper called from both `add_memory` and inline `task_complete` |
| 12 | Mesh-conflicting rows are excluded in `search` but absent from the spec's exclusion list | **verified** | `src/store/ladybug.rs:698-700`, applied `:1083-1085` |
| 13 | Lock 2 re-read ordering (`list_now()` → `extraction_exists`) | **verified** | `src/task/mod.rs:1189-1190` |
| 14 | `ThalamicPulse` has four variants and emits `Superseded` only on inline extraction | **verified** | `src/events/event.rs:14`, `:19`, `:25`, `:31`, `:50`; `src/task/mod.rs:1199-1203` comment |
| 15 | Headerless `/mcp` callers need a fallback token, not a rejection | **verified** | `src/server.rs:904-907`; `src/daemon.rs:1009-1012` |
| 16 | Concurrent JSONL appends must be serialized | **verified as covered** | plan Task 1 Step 1 already requires concurrent writes to remain line-atomic (`plan:499`) |
| 17 | Split precedence is unspecified | **verified** | spec `:247-250` had no precedence rule; now added |
| 18 | `Embedder` exposes only `embed` and `dimensions` | **verified** | `src/traits.rs:113-119`; plan Task 2 Step 6 adds `embedder_id` (`plan:267`) |
| 19 | `setup` provisions Linux only | **verified** | `src/setup.rs:4-7` |
| 20 | No SHA-256/BLAKE3 dependency in `Cargo.toml` | **verified** | no `sha2`/`blake3`/`sha1`/`md5` in `Cargo.toml`; `src/events/event.rs:53` `payload_hash: u32` |

**20 of 20 checked: 19 verified, 1 provenance discrepancy (#7) that changes no
conclusion.**

---

## 3. Adjudication

**Both seats: APPROVE-WITH-CHANGES. Neither rejected; neither asked for a
design change.** Their findings converge almost completely.

- **Shared ground.** Both seats independently found the same two stale source
  facts (the `10.0` sentinel, the `neurostrata_edit_memory` reference), and
  both verified the technical spine: `score` is a boosted distance, the
  inverted filter is real, the hardcoded `5` is real, and the owner's
  `max_candidates` ruling is carried coherently. K3 counted 20 claims, Argon
  23; the orchestrator verified 40 of them against source, and **no claim that
  changes a conclusion was contradicted.**
- **Complementary, not conflicting.** K3's required edits are about making the
  spec say what it means — mark new-versus-existing exclusion provenance, pin
  Phase 1's disposition of the judgment call, add the observation-failure
  invariant, and move a Phase-2-dependent criterion out of Phase 1. Argon's
  are about two behaviors the spec never pinned at all — mesh-conflicting rows
  must be excluded from both the candidate query and canonical reuse, and task
  completion outcomes must be captured on all three Lock 2 paths. The two sets
  are disjoint except for the two stale facts, which they agree on.
- **Where the seats disagree.** Only on emphasis: K3 treats the exclusion-list
  provenance as a required edit, Argon treats it as optional and asks instead
  for the exclusions to be pushed into the Cypher `WHERE`. The union is
  strictly better than either — the spec now labels which exclusions exist
  today and which are new, *and* requires them pushed down so `K` is not
  starved.
- **One seat's finding the other missed, and vice versa.** K3's Phase 1
  judgment-call gap and the misplaced acceptance criterion are invisible in
  Argon's list; Argon's mesh-conflict exclusion and three-path outcome capture
  are invisible in K3's. Neither seat was complete alone. The union is the
  reason both seats were run.
- **Not re-litigated.** The owner's `max_candidates` ruling was held settled in
  both briefs; neither seat argued against it. K3 confirmed it is carried
  coherently; Argon confirmed it and added a related implementation risk
  (over-fetch starvation) that the ruling does not cover.

**Adjudicated verdict: the spec is sound.** It leaves Draft once the eight
required edits land; two of them are corrections of facts that went stale when
the owner's other feature (`c446b2c`) rewrote `src/store/ladybug.rs`.

---

## 4. Disposition of required changes

All eight required edits are applied. Both seats' required sets are covered;
the two stale-fact corrections are one edit each in the spec.

| Edit | Seat(s) | Where it landed | Substance |
|---|---|---|---|
| **R1 / A1a** — sentinel | K3 R1, Argon A1 | spec "Current source facts" #3; the "Graph-expanded neighbors are appended with `EXPANSION_BASE_SCORE = 1.0` (`src/store/ladybug.rs:1418`, applied at `:1437`) and are told apart from primaries by evidence kind, not by score" | fact now matches HEAD; the defect conclusion is unchanged (`1.0 >= 0.85`, so expansions still pass the filter) |
| **R2 / A1b** — corrective tool name | K3 R2, Argon A1 | spec fact #5, the "During collection and calibration" bullet, and Phase 1 | the error already names `neurostrata_supersede_memory`; the remaining change is to also name `neurostrata_archive_memory`. The dead tool name is no longer presented as a defect to "fix" |
| **R3** — Phase 1 judgment-call disposition | K3 R3 | spec Phase 1 | `DeduplicationChecker::check_duplicates` (`src/server.rs:1162`) is bypassed unless provider `jev` is explicitly selected; `auto`/`heuristic`/`laya-local`/`none` record candidates and apply only the canonical-key decision. Plan Task 3 Step 5 now says the same |
| **R4** — exclusion provenance | K3 R4 | spec "Candidate selection and write behavior" | the five structural types are excluded today (`src/store/ladybug.rs:677-683`), `task` only under `include_tasks=false` (`:1036`); `symbol`, `code_ast`, and `auto-ingestor` exclusion is marked **new**, dedup-path-only behavior |
| **R5** — Phase-2 criterion in Phase 1 | K3 R5 | spec Phase 1 → Phase 2 | the hint-band verification bullet moved to Phase 2, where the band is established; Phase 1 keeps the negative assertion |
| **R6** — observation-failure invariant | K3 R6 | spec "During collection and calibration" | observation and dataset persistence failures never fail a memory write or a task completion |
| **A2** — mesh-conflict exclusion | Argon A2 | spec "Candidate selection and write behavior"; plan Task 2 (scope contract, Step 3, Step 5) | `is_mesh_conflicting` rows are excluded from `nearest_by_raw_distance` **and** canonical exact-match reuse |
| **A3** — outcome capture on three Lock 2 paths | Argon A3 | spec "Observation and outcome records" and Phase 1 | `ThalamicPulse` emits `Superseded` only on inline extraction (`src/task/mod.rs:1199-1215`); task completion outcomes are captured explicitly on all three paths, not inferred from the bus |

### Optional changes — disposition

| Optional | Seat | Disposition |
|---|---|---|
| Name `K = deduplication.max_candidates` in the candidate-selection body | Argon | **applied** (spec) |
| Name the connection-ID header (`X-NeuroStrata-Conn-Id`) | K3 | **applied** (spec) — the plan already used it |
| Headerless `/mcp` callers get a fallback token, not a rejection | Argon | **applied** (spec) |
| `remaining_fires` is a procedure-runtime convention the store does not enforce | K3 | **applied** (spec) |
| Record a starting `schema_version` and its evolution rule | K3 | **applied** (spec) — `schema_version` starts at `1`, additive within a major version, unknown major versions are ignored |
| Component-vs-forward-time split precedence | Argon | **applied** (spec Phase 2) |
| Push exclusions into the Cypher `WHERE` so `FETCH_MULTIPLIER` cannot starve `K` | Argon | **applied** (spec + plan Task 2 Step 3) |
| Drop the `learn reset --purge-observations` flag shape to the plan | K3 | **already satisfied** — plan `:866` already requires an explicit purge flag for observations |
| Name a hash crate for `blobs.jsonl` and canonical keys | Argon (risk 3) | **applied** (plan Task 1 Step 3 now names `sha2` as a direct dependency) |
| Lock 2 re-read ordering on reuse | Argon (risk 1) | **applied** (spec Phase 1 requires the `extracted_from` update to be visible to the gate's re-read) |
| Crash-during-append recovery case | K3 (risk 5) | **applied** (spec Phase 1) |

---

## 5. Residual risks (unchanged by this review)

1. **Phase 0 provisioning.** The host Python 3.14.4 has no torch wheels, so
   `laya-train` and the ONNX export are unavailable; plan Tasks 7-8 stay
   blocked (`neurostrata-nvuu` tracks the interpreter pin).
2. **Implementation is still owner-gated.** This review cleared the spec, not
   the plan's Tasks 1-9. The probe branch `origin/feature/local-self-improvement`
   (`4e41775`) stays off `main` until the owner re-reviews it.
3. **`Cargo.toml` still has no hash crate** — the spec and plan now require it,
   but adding it is implementation work, not a doc change.

---

## 6. What this review did not cover

- No code review: the probe branch's implementation was not re-reviewed.
- No live Tebra lookups and no patient/PHI data were involved.
- The owner's `max_candidates` ruling was treated as settled, by instruction.

---

## 7. Post-review addendum (2026-10-22)

After this review landed, the recall-closure feature (`docs/superpowers/specs/
2026-10-21-recall-closure-and-negative-exemplars-design.md`) shipped on `main`
and introduced two interactions with the dedup path that this review did not
see. They are recorded here rather than in a new review, because neither changes
the adjudicated verdict; both are clarifications to the implementation contract.

1. **Attempt partition (new).** A `fact` may carry `metadata.attempt` (a failed
   attempt). The candidate set is partitioned by attempt-presence before any
   comparison: a failed attempt is compared only against a failed attempt, and a
   non-attempt only against non-attempts (`src/server.rs` now filters on
   `crate::attempt::is_failed_attempt` equality inside `embed_dedup_and_upsert`).
   The canonical exact-match key must include `metadata.attempt` (presence and
   its four keys) so exact reuse cannot cross the partition. This is now
   normative in the spec (`Candidate selection and write behavior`,
   `canonical exact-match key`) and in plan Task 3 Steps 1, 2, 5, 6.
2. **Registry row (new, store seam).** A fact carrying `metadata.domain_registry`
   is dropped from every result set at the store seam (`src/store/ladybug.rs`,
   four filter sites), so it never reaches `nearest_by_raw_distance` or
   canonical exact-match reuse. It is **not** a new `STRUCTURAL_MEMORY_TYPES`
   entry (it is a `fact`, not a structural type), so the by-memory-type
   exclusion list is unchanged; the seam exclusion is what matters. Plan Task 2
   Step 3 and Step 5 now say so.
3. **Domain is not part of the canonical key.** The recall-closure feature adds
   `domain` as a soft filter on search; dedup deliberately detects duplicates
   across domains, so `domain` is excluded from the canonical key. Stated
   explicitly in the spec to prevent an implementer from adding it.

No re-review of the spec is required: both seats' findings stand, and these
three points are additive to the implementation contract they approved.

