# Semantic & Retrieval Evaluation Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an offline v1.19.0 release gate that executes versioned SPARQL competency contracts and a 60-query, four-arm retrieval ablation against real temporary LadybugDB stores, then emits strict JSON evidence.

**Architecture:** Add one `src/semantic_eval/` module with three narrow components: crate-free ranking metrics, SPARQL fixture execution over an in-memory Oxigraph dataset with optional certified reasoning, and retrieval ablation over identical temporary LadybugDB stores. Expose one CLI command that runs both suites and returns nonzero on gate failure.

**Evidence boundary:** The repository has no owner-labeled production corpus. The checked-in 60-query corpus is therefore explicitly `synthetic_deterministic`, not owner-labeled production evidence. The gate proves semantic/retrieval deltas on a deterministic, reproducible, versioned corpus; it must not be presented as owner-labeled, real-namespace, or production-lift validation.

**Fail-closed, honest-gate contract (critical):** Any fixture-load, validation, execution, timeout, or comparison failure is a failed gate and must exit nonzero with strict JSON evidence naming the failure (never silently pass or claim lift). No hardcoded rankings, no synthetic pass paths, and no production-ROI claims.

**Tech Stack:** Rust, `LadybugStore::for_testing`, `BenchEmbedder`, `SearchMode`, the existing domain-registry/`OntologyIndex` production path, `oxrdf`/`oxttl`/`spargebra`/`spareval`/`sparesults`, certified `ReasoningAdapter`, production SPARQL custom functions (empty metadata, fixed `now`) with `SERVICE` forbidden, strict `serde_json`, and existing CLI/remediation-registry wiring.

**Key design constraints (approved structure + mandatory corrections, reconciled):**
- **Canonical competency reasoning profiles:** `none`, `rdfs`, `owl-rl` only. `rdfs` is metadata-only in the current certified-reasoning/profile model and is not used for bundled reasoning questions. Bundled reasoning questions execute with `owl-rl`; non-reasoning competency questions use `none`. The plan makes **no RDFS-only rule-selection claim**.
- **Competency execution:** Run ASK/SELECT contracts over `eval/semantic/competency.json` (single strict JSON fixture containing `dataset_nquads`) via the production custom function registry with empty metadata and a fixed `now`, with `SERVICE` forbidden/disabled. Use bounded, fail-closed budgets.
- **Retrieval execution:** All four arms use real `LadybugStore::for_testing()` stores seeded with identical fixture memories/embeddings via `BenchEmbedder` and queried with `SearchMode::Default`. No hand-authored rankings. All arms use the original query text.
- **Arm ontology/index isolation (reconciled, preserved):**
  - Arm 1 (`baseline`): original query, `exact=false`, `SearchMode::Default`, **no fixture domain-registry row applied**. Shipped vocabulary remains available in all arms.
  - Arm 2 (`exact`): original query, `exact=true`, `SearchMode::Default`, same no-fixture-registry state as arm 1.
  - Arm 3 (`flat_aliases`): original query text, `exact=true`, `SearchMode::Default`, fixture domain applied via the **production alias-only `OntologyIndex`/domain registry path** using the **same alias set as arm 4** but with **all `parent = null`** (no hierarchy expansion, no hand query rewrite).
  - Arm 4 (`ontology`): original query text, `exact=true`, `SearchMode::Default`, fixture domain applied via the **production alias+hierarchy `OntologyIndex`/domain registry path** (same aliases as arm 3 plus valid parent links) using existing production expansion behavior.
- **Derived storage (exact serialization):** numerator = **compact canonical JSON of arm 4 domain entries sorted by slug with aliases sorted** (canonical serialized arm 4 `OntologyIndex`/registry-row bytes); denominator = **raw `retrieval.json` bytes**. Emit both byte values and the derived ratio in evidence.
- **Latency / p95 gate:** use `>= 3` measured iterations with warmup excluded. Compute per-query latency as the median across measured iterations, then compute arm p95 over those per-query medians. **P95 gate = arm4-minus-arm3 overhead `<= 25ms` AND `<= 10%`** (not arm4 absolute p95 only). Emit iteration counts, warmup exclusion, medians/p95 values, delta-ms, and delta-% in evidence.
- **Gate math:** `relative_gain(0, x) = 0.0` (and therefore cannot satisfy/pass any nDCG-gain clause on a zero baseline). Core lift requires arm 4 to beat arm 3 by the specified thresholds.
- **Fixture integrity:** retrieval fixtures must reject `task` memory type and **every actual production `STRUCTURAL_MEMORY_TYPES` member** (not merely a literal `structural` string). Bundled fixtures must include `curation_notes` and negative controls where flat aliases would otherwise suffice.
- **Review evidence boundary:** independent label/forbidden-id review is **release-process evidence**; **DO NOT add `independent_review` DTO fields/timestamps to fixtures**. Keep `curation_notes` and negative controls in the fixture.
- **Acceptance caveat:** bundled evidence is `synthetic_deterministic` only and is **not** owner-labeled/real-namespace/production evidence.

---

### Task 1: Strict ranking metrics and gate math

**Files:**
- Create: `src/semantic_eval/mod.rs`, `src/semantic_eval/metrics.rs`, `tests/semantic_eval_metrics_test.rs`
- Modify: `Cargo.toml` (named integration target), `src/main.rs` (`mod semantic_eval;` only)

- [ ] **Step 1: Write failing crate-free metric tests**

Use `#[path = "../src/semantic_eval/metrics.rs"] mod metrics;`. Cover graded nDCG, ideal/empty rankings, rank discounts, Recall@5/Precision@5 with short rankings and duplicate returned ids, finite serialization, malformed inputs, `relative_gain(0,x)=0.0`, and every acceptance rejection: insufficient overall/ontology-targeted lift, Precision@5 drop, forbidden hit, p95 overhead, derived storage, nondeterminism, and failed query.

Run `cargo test --locked --test semantic_eval_metrics_test -- --test-threads=1`; confirm RED before implementation.

- [ ] **Step 2: Implement strict DTOs and pure helpers**

Define `RelevanceLabel`, `QueryMetrics`, `ArmMetrics`, `RetrievalArm { Baseline, Exact, FlatAliases, Ontology }`, and `QueryCategory { Alias, ExactIdentifier, Hierarchy, Adversarial }` with strict serde and snake_case enums. Implement `ndcg_at`, `recall_at`, `precision_at`, nearest-rank `percentile`, `summarize_arm`, `relative_gain`, and `evaluate_thresholds`. Use zero-based-rank DCG `(2^gain - 1)/log2(rank + 2)`, mean per-query aggregates, and fixed Precision@5 denominator 5. Duplicate returned ids count once.

Gate arm 4 vs arm 3 on >=5% relative nDCG@10 OR >=5pp Recall@5; alias+hierarchy subset >=10pp Recall@5; Precision@5 drop <=2pp; zero arm-4 forbidden hits; p95 overhead <=25ms AND <=10%; derived storage <5%; deterministic repeated rankings; and zero failed queries. Zero nDCG baseline yields relative gain 0, so only Recall@5 can satisfy the lift alternative. Zero latency baseline fails only when absolute overhead exceeds 25ms. All outputs must be finite.

Keep `metrics.rs` free of `crate::` imports so the integration target tests shipped source directly.

- [ ] **Step 3: Run GREEN and scope verification**

Run `cargo test --locked --test semantic_eval_metrics_test -- --test-threads=1`. Expect all named tests to pass with zero ignored. Do not commit.

### Task 2: Add SPARQL competency fixture execution

**Files:**
- Create: `src/semantic_eval/competency.rs`, `eval/semantic/competency.json` (single strict JSON containing `dataset_nquads`)
- Modify: `src/semantic_eval/mod.rs`, `src/rdf/mod.rs` only if needed
- **No** separate competency manifest, Turtle, or questions fixture files (use single `eval/semantic/competency.json` only)

- [ ] **Step 1: Write the failing competency tests**

Add `#[cfg(test)]` tests in `src/semantic_eval/competency.rs` covering:
- loading/validation of `eval/semantic/competency.json` as a single strict fixture containing `dataset_nquads` (and the competency question/expectation fields required by the suite),
- canonical `reasoning_profile` parsing/validation accepts only `none`, `rdfs`, `owl-rl`,
- `rdfs` is metadata-only (not a bundled reasoning execution path) and bundled reasoning questions use `owl-rl` only (no RDFS-only rule-selection claim),
- ASK true/false execution and wrong-result rejection,
- SELECT complete sorted-multiset comparison preserving duplicate solutions and RDF term type/value/datatype/language,
- RDFS subclass and OWL inverse/transitive entailments through certified materialization; uncertified output fails closed,
- `SERVICE`, `CONSTRUCT`, `DESCRIBE`, `DELETE`, and `INSERT` rejection before evaluation,
- production custom functions with empty metadata and fixed `now`, never the federated service evaluator,
- dataset/algebra/solution/inferred-triple/timeout budgets, malformed fixtures, wrong forms, truncation, and nondeterminism all fail closed.

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval::competency:: -- --test-threads=1`
Expected: FAIL because the competency runner/fixture do not exist.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval::competency:: -- --test-threads=1`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

- Create `src/semantic_eval/competency.rs` with strict DTOs, including **`CompetencyFixture`** containing `dataset_nquads` (and strict question/expectation definitions required to execute the suite) and canonical `reasoning_profile: "none" | "rdfs" | "owl-rl"`.
- Implement fixture loading from a single strict JSON file: `eval/semantic/competency.json`.
- Parse bounded default-graph N-Quads. For `owl-rl`, call `ReasoningAdapter::materialize_owl2_rl_with_proofs` with a 50,000 inferred-triple cap and add only certified inferred triples; record profile/counts/certificate SHA-256. `rdfs` is metadata-only and is not used for bundled reasoning questions.
- Parse through the existing prefix parser; permit only ASK/SELECT and reject every `SERVICE`. Evaluate through production custom functions with empty metadata and fixed `now`, default-graph-as-union, cancellation, `max_algebra_nodes=100`, `max_dataset_quads=50_000`, `max_solutions=1_000`, and `timeout_ms=250`.
- Compare ASK booleans and complete SELECT RDF-term multisets; never compare partial results. Return deterministic strict per-question expected/actual/pass evidence without wall-clock timestamps or latency fields.
- Export/wire in `src/semantic_eval/mod.rs`; modify `src/rdf/mod.rs` only if a small public seam is needed.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval::competency:: -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Verify Task 2 scope**

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval::competency:: -- --test-threads=1`
Expected: PASS with only Task 2 files modified/created and no separate competency manifest, Turtle, or questions fixture files added.

### Task 3: Add four-arm retrieval ablation over real temporary stores

**Files:**
- Create: `src/semantic_eval/ablation.rs`, `eval/semantic/retrieval.json`
- Modify: `src/semantic_eval/mod.rs`, `src/main.rs`

- [ ] **Step 1: Write the failing ablation tests**

Add `#[cfg(test)]` tests in `src/semantic_eval/ablation.rs` covering:
- strict loading/validation of `eval/semantic/retrieval.json` with required DTO fields: `documents`, `queries`, `flat_aliases`, `domains`, `curation_notes`,
- fixture/document validation rejects `task` and **every actual production `STRUCTURAL_MEMORY_TYPES` member** (not merely a literal `structural` string),
- negative controls present where flat aliases would otherwise suffice; `curation_notes` present (no `independent_review` DTO/timestamps in fixture),
- four real temporary `LadybugStore::for_testing()` stores seeded identically via `BenchEmbedder`,
- all arms execute with original query text and `SearchMode::Default`,
- arm 1 and arm 2 run with **no fixture domain-registry row applied** while shipped vocabulary remains available,
- arm 3 applies a **production alias-only `OntologyIndex`/domain registry** with the **same aliases as arm 4** but **all `parent = null`** (no hierarchy, no hand query rewrite),
- arm 4 applies the **production alias+hierarchy `OntologyIndex`/domain registry** (same aliases plus valid parent links) via existing production expansion/ontology path,
- no hardcoded rankings; results come from actual retrieval and are deterministic,
- per-arm metric summaries and pairwise deltas computed from real rankings,
- latency uses `>= 3` measured iterations with warmup excluded; per-query latency is the median across measured iterations and arm p95 is computed over those per-query medians; iteration/warmup/timing details emitted,
- derived storage numerator = **compact canonical JSON of arm 4 domain entries sorted by slug with aliases sorted** and denominator = **raw `retrieval.json` bytes**; emit both byte values and ratio,
- gate evaluation is fail-closed: arm4-minus-arm3 **p95 overhead `<= 25ms` AND `<= 10%`** (not arm4 absolute p95 only), `relative_gain(0,x)=0` cannot satisfy nDCG-gain on zero baseline, and required lift/non-regression criteria produce nonzero/failing evidence when unmet.

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval::ablation:: -- --test-threads=1`
Expected: FAIL because the ablation runner/fixture do not exist.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval::ablation:: -- --test-threads=1`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

- Create `src/semantic_eval/ablation.rs` with strict DTOs, including **`RetrievalFixture { documents, queries, flat_aliases, domains, curation_notes }`** (plus any strictly required internal fields) and strict query/document/category/relevance shapes needed by the 60-query corpus; **do not** add `independent_review` fields/timestamps to the fixture DTO.
- Validate fixture integrity: reject `task` and every actual production `STRUCTURAL_MEMORY_TYPES` member; require `curation_notes` and negative controls where flat aliases would otherwise suffice.
- Implement four-arm execution over identical seeded temp stores (`SearchMode::Default`, original query text for all arms): arm1/arm2 no fixture domain-registry row (shipped vocabulary retained); arm3 production alias-only `OntologyIndex`/domain registry with same aliases as arm4 and all `parent=null`; arm4 production alias+hierarchy registry/path.
- Compute metrics via `metrics` (including exact nDCG `(2^gain - 1)/log2(rank + 2)` zero-based rank) and deterministic deltas/gate decisions: `relative_gain(0,x)=0`, p95 gate = arm4-arm3 overhead `<=25ms` AND `<=10%` over per-query medians from `>=3` measured iterations (warmup excluded).
- Emit derived storage evidence as compact canonical JSON of arm4 domain entries sorted by slug with aliases sorted (numerator bytes) and raw `retrieval.json` bytes (denominator), plus both values and ratio.
- Create `eval/semantic/retrieval.json` as `synthetic_deterministic` corpus with required DTO fields and reconciled arm3/arm4 domain/alias semantics.
- Export/wire in `src/semantic_eval/mod.rs` and modify `src/main.rs` to expose the exact CLI: `neurostrata-mcp semantic-eval --competency eval/semantic/competency.json --retrieval eval/semantic/retrieval.json --iterations 3 --output /tmp/opencode/semantic-eval.json --check` (iterations/measured-iteration semantics as specified). CLI must write strict JSON and exit nonzero on any gate failure or execution/validation error when `--check` (fail-closed).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval::ablation:: -- --test-threads=1`
Expected: PASS.

- [ ] **Step 5: Verify Task 3 scope**

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval::ablation:: -- --test-threads=1`
Expected: PASS with only Task 3 files modified/created and no forbidden files introduced.

### Task 4: Wire strict evidence output and remediation coverage

**Files:**
- Modify: `eval/registry.json`, `docs/superpowers/specs/2026-11-17-v1-19-0-semantic-graph-and-agentic-reasoning-design.md`, `docs/superpowers/plans/2026-11-17-v1-19-0-semantic-graph-and-agentic-reasoning-plan.md`, `docs/features-api/cli-reference.md`, `docs/releases/changelog/1.19.0.md`, `CHANGELOG.md` (only if needed)
- **No** library crate root, **no** remediation test-registry source file, **no** monolithic semantic-eval integration test file, and **no** version-control commit steps anywhere

- [ ] **Step 1: Write/update the failing wiring/report expectations**

Add focused report/CLI expectations in `src/semantic_eval/mod.rs` for strict JSON evidence/output fields:
- top-level suite pass/fail and `evidence_class: "synthetic_deterministic"` with explicit non-owner-labeled/non-production boundary,
- competency results (canonical profiles `none|rdfs|owl-rl`, `rdfs` metadata-only, bundled reasoning on `owl-rl`, production custom function registry with empty metadata/fixed `now`, no `SERVICE`, expected vs actual),
- per-arm metrics/deltas for all four arms (original query text, `SearchMode::Default`; arm1/2 no fixture domain-registry row with shipped vocabulary retained; arm3 alias-only production `OntologyIndex`/registry same aliases as arm4 all `parent=null`; arm4 alias+hierarchy),
- derived storage numerator bytes = compact canonical JSON of arm4 domain entries sorted by slug with aliases sorted, denominator bytes = raw `retrieval.json` bytes, and ratio (both emitted),
- latency evidence: `measured_iterations >= 3`, warmup excluded, per-query medians basis, arm p95 over per-query medians, and **p95 gate = arm4-minus-arm3 overhead `<=25ms` AND `<=10%`** (not arm4 absolute p95 only),
- `--check` exits nonzero on any gate failure or validation/execution error (fail-closed).

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval:: -- --test-threads=1`
Expected: FAIL until wiring/report fields and docs/registry updates are complete.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval:: -- --test-threads=1`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation and doc/registry updates**

- Implement strict report serialization and CLI wiring in the existing module/main entry points (per Task 3 CLI): `neurostrata-mcp semantic-eval --competency eval/semantic/competency.json --retrieval eval/semantic/retrieval.json --iterations 3 --output /tmp/opencode/semantic-eval.json --check` writes strict JSON and exits nonzero on `--check` failures/errors (fail-closed).
- Update `eval/registry.json` to register/remediate the semantic evaluation gate using the approved test/command surface.
- Update docs: `docs/superpowers/specs/2026-11-17-v1-19-0-semantic-graph-and-agentic-reasoning-design.md`, `docs/superpowers/plans/2026-11-17-v1-19-0-semantic-graph-and-agentic-reasoning-plan.md`, `docs/features-api/cli-reference.md`, and `docs/releases/changelog/1.19.0.md` / `CHANGELOG.md` only if needed to describe fixtures (`eval/semantic/competency.json`, `eval/semantic/retrieval.json`), CLI, gate criteria/corrections (arm isolation, `SearchMode::Default`, derived storage serialization, `relative_gain(0,x)=0`, p95 overhead gate), and the honest `synthetic_deterministic` non-production evidence boundary.
- Do **not** add a library crate root, a remediation test-registry source file, a monolithic semantic-eval integration test file, or any version-control commit steps.

- [ ] **Step 4: Run targeted verification**

Run:
- `cargo test --locked --test semantic_eval_metrics_test -- --test-threads=1`
- `cargo test --locked --bin neurostrata-mcp semantic_eval:: -- --test-threads=1`
- `EVAL_ALLOW_DIRTY=1 scripts/remediation-eval.sh semantic-evaluation --json /tmp/opencode/semantic-evaluation-remediation.json`
- `cargo run --locked --bin neurostrata-mcp -- semantic-eval --competency eval/semantic/competency.json --retrieval eval/semantic/retrieval.json --iterations 3 --output /tmp/opencode/semantic-eval.json --check`
- `jq -e '.verdict == "PASS" and .production_roi_claimed == false and .evidence_class == "synthetic_deterministic"' /tmp/opencode/semantic-eval.json`

Expected: actual receipts pass before any completion claim. `EVAL_ALLOW_DIRTY=1` only permits the intentionally dirty consolidated release tree; it never ignores test failures.

- [ ] **Step 5: Verify Task 4 scope and evidence boundary**

Run: `cargo test --locked --bin neurostrata-mcp semantic_eval:: -- --test-threads=1`
Expected: PASS; docs/registry updated only as listed, CLI and fail-closed synthetic evidence boundary preserved.

## Acceptance Criteria
- [ ] **Task 1 files/scope (exact):** create `src/semantic_eval/mod.rs`, `src/semantic_eval/metrics.rs`, `tests/semantic_eval_metrics_test.rs`; modify `Cargo.toml` (named integration target) and `src/main.rs` (`mod semantic_eval;` only). Metrics include strict DTOs/types and exact nDCG `(2^gain - 1)/log2(rank + 2)` with zero-based rank; `relative_gain(0, x) = 0.0`.
- [ ] **Task 2 files/scope (exact):** create `src/semantic_eval/competency.rs` and `eval/semantic/competency.json` (single strict JSON containing `dataset_nquads`); modify `src/semantic_eval/mod.rs` and `src/rdf/mod.rs` only if needed. No separate competency manifest, Turtle, or questions fixture files. Competency uses canonical profiles `none|rdfs|owl-rl` (`rdfs` metadata-only, bundled reasoning on `owl-rl`, no RDFS-only rule-selection claim), production custom function registry with empty metadata and fixed `now`, and no `SERVICE`; bounded fail-closed execution.
- [ ] **Task 3 files/scope (exact):** create `src/semantic_eval/ablation.rs` and `eval/semantic/retrieval.json`; modify `src/semantic_eval/mod.rs` and `src/main.rs`. `RetrievalFixture` DTO is strict with `documents`, `queries`, `flat_aliases`, `domains`, `curation_notes` (no `independent_review` DTO/timestamps). Fixtures reject `task` and every actual production `STRUCTURAL_MEMORY_TYPES` member (not merely a literal `structural` string) and include negative controls where flat aliases would otherwise suffice.
- [ ] **Four-arm ablation (reconciled):** 60 deterministic queries over four real temporary stores, all arms `SearchMode::Default` with original query text. Arm1/arm2: no fixture domain-registry row (shipped vocabulary retained). Arm3: production alias-only `OntologyIndex`/domain registry with same aliases as arm4 and all `parent=null` (no hierarchy, no hand query rewrite). Arm4: production alias+hierarchy `OntologyIndex`/domain registry via production path. No hardcoded rankings.
- [ ] **Latency/p95 gate:** `>= 3` measured iterations, warmup excluded; per-query latency = median across measured iterations; arm p95 computed over per-query medians. Gate requires arm4-minus-arm3 overhead `<= 25ms` AND `<= 10%` (not arm4 absolute p95 only). Emit iteration counts, warmup exclusion, medians/p95, delta-ms, and delta-%.
- [ ] **Derived storage:** numerator = compact canonical JSON of arm4 domain entries sorted by slug with aliases sorted; denominator = raw `retrieval.json` bytes. Emit both byte values and ratio.
- [ ] **CLI/report (exact) and fail-closed evidence:** `neurostrata-mcp semantic-eval --competency eval/semantic/competency.json --retrieval eval/semantic/retrieval.json --iterations 3 --output /tmp/opencode/semantic-eval.json --check` writes strict JSON and exits nonzero on any gate failure or validation/execution error when `--check`. Evidence is `synthetic_deterministic` only and states non-owner-labeled/non-production boundary (independent review is release-process evidence, not a fixture DTO).
- [ ] **Task 4 files/scope (exact):** modify `eval/registry.json`, `docs/superpowers/specs/2026-11-17-v1-19-0-semantic-graph-and-agentic-reasoning-design.md`, `docs/superpowers/plans/2026-11-17-v1-19-0-semantic-graph-and-agentic-reasoning-plan.md`, `docs/features-api/cli-reference.md`, `docs/releases/changelog/1.19.0.md`, `CHANGELOG.md` only if needed. No library crate root, no remediation test-registry source file, no monolithic semantic-eval integration test file, and no version-control commit steps.

## Remediation-runner notes
- Primary command: `neurostrata-mcp semantic-eval --competency eval/semantic/competency.json --retrieval eval/semantic/retrieval.json --iterations 3 --output /tmp/opencode/semantic-eval.json --check`
- Focused tests: `cargo test --locked --test semantic_eval_metrics_test -- --test-threads=1` and `cargo test --locked --bin neurostrata-mcp semantic_eval:: -- --test-threads=1`; register every expected test name in `eval/registry.json` with `ignored: 0`.
- Evidence remains fail-closed, deterministic, and explicitly `synthetic_deterministic` (not owner-labeled/production). All plan checkboxes remain unchecked.
