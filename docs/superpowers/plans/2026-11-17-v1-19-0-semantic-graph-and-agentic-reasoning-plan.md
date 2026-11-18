# NeuroStrata v1.19.0 Semantic Graph & Agentic Reasoning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** Implemented and verified (consolidated v1.19.0, capabilities A–G). All seven capabilities have focused-test evidence and independent reviews at zero Critical/Important findings. Capability G is complete with strict receipts and an owner-approved honest FAIL (retrieval lift below threshold); no full-workspace-green or semantic-PASS claim. Checkboxes record implemented work; not claimed pushed or published.

**Goal:** Implement the six `v1.19.0` capabilities A–F and the Capability G evaluation gate: (A) DataFrog OWL 2 RL reasoning (`gtfierro/reasonable`) + Lean 4 derivation certificates (`neurostrata-s0l0`) with fail-closed/no-fabricated proof steps, (B) pre-commit ontology blast-radius & semantic closure diff `ontology_plan` (`neurostrata-miio`) zero-mutation and production-wired via `OntologyPort` + `neurostrata_scenario(plan_ontology_diff)` with implemented `diff_ontologies`/`generate_cypher_ddl`, (C) epistemic trust tiers (`generated_by` vs `verified_by`, exact tiers `unverified_agent,empirical_verified,formal_verified,human_governed`), freshness SLAs (`stale_after` RFC3339/epoch advisory), governed deprecation annotations and `not_definition` on production surfaces with `memory-shapes.v1.ttl` modified only (`neurostrata-50sm`), (D) W3C `YAML-LD 1.0` / `Vault-LD` bidirectional Markdown vault sync + JSON Canvas 1.0 as shared `neurostrata_vault_sync`/`vault-sync` MCP/CLI/HTTP, no remote fetch, strict secret+SHACL before writes (`neurostrata-dlag`), (E) trajectory-to-procedure mining & outcome-verified belief reflection with pure core, actual `task_complete`/successful-revert hooks, define-gated non-auto-applied action candidates (`neurostrata-mpau`), (F) ontology-scoped bounded classification ingestion, always strict, empty domain/vocabulary fail-closed, whole-batch validation before writes, production handler exposed (`neurostrata-v6dw`), and (G) offline synthetic deterministic semantic/retrieval evaluation gate (`neurostrata-dyrq`) with strict JSON receipt/nonzero `--check`, exact thresholds, determinism/fail-closed behavior, and explicit `not_evaluated` production-pilot exclusions (no owner-labeled production ROI, blind-agent accuracy, rollback, curation-cost, or real-namespace claims).

**Architecture:** Builds directly on `crates/ontobug` (`LadybugDB/ontobug` + `rudof` `0.3.21`), `oxigraph` `0.5.11` / `oxrdf` `0.3.4`, `src/formal_kernel.rs`, `src/rdf/`, `src/task/mod.rs`, and `src/learning/`. See `docs/superpowers/specs/2026-11-17-v1-19-0-semantic-graph-and-agentic-reasoning-design.md` for normative contracts.

**Tech Stack:** Rust 2021, `crates/ontobug`, `rudof` `0.3.21`, `gtfierro/reasonable` (`BSD-3-Clause`, `datafrog` + `oxrdf`), `oxigraph` `0.5.11`, `lbug` `0.22.3`.

---

### Task 1: DataFrog OWL 2 RL Reasoning & Lean 4 Derivation Certificates (`neurostrata-s0l0`) — implemented, verified

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/rdf/reasoning.rs`
- Modify: `src/formal_kernel.rs`
- Modify: `docs/vision/product-vision.md` (required co-change whenever `src/formal_kernel.rs` is touched per `docs/manifest.json`)
- Modify (as needed): `src/rdf/ontology.rs`, `src/rdf/scenario.rs`, snapshot/summary surfaces reporting `ReasoningProfile` (integration wiring only; keep zero-mutation semantics)

- [x] **Step 1: Graph-first + claim discipline (implementation session)**
  `neurostrata_task_claim(id: "neurostrata-s0l0")` and `neurostrata_memories_for_file` on `src/rdf/reasoning.rs`, `src/formal_kernel.rs`, and touched integration files before edits.
- [x] **Step 2: Write failing TDD tests (certified OWL-RL, no uncertified output)**
  In `src/rdf/reasoning.rs` and `src/formal_kernel.rs`: cover full supported RDFS/OWL-RL materialization and `ReasoningProfile` reporting (`rdfs`/`owl-rl`), `DerivationCertificate.profile` + digest over canonicalized `profile + steps`, and deterministic reconstruction/auditing that yields a sound verified derivation for **every returned inferred triple** (including `prp-trp`, `prp-inv`, `prp-spo2`/property chains, `prp-dom`/`prp-rng`, `cax-sco`, `prp-spo1`, `prp-eqp`/`cax-eqc`, relevant restriction rules such as `cls-hv`/`cls-svf`/`cls-avf`, and applicable violations such as `cax-dw`). Test fail-closed rejection when an individual triple/rule derivation cannot actually be established (no uncertified inferred output returned). `ProofUnavailable` must not be the normal OWL-RL path / blanket substitute.
- [x] **Step 3: Implement required contracts (all ship together)**
  Keep `ReasoningAdapter` store-free; accept explicit base triples + `namespace/profile/max_inferred_triples`. Integrate live callers via narrow store/view parameter (e.g. load namespace base graph in ontology/scenario helpers), not a store-backed no-arg adapter method. Use `reasonable` for DataFrog RDFS/OWL-RL materialization; deterministically reconstruct and independently verify derivations over `base + materialized` triples for every returned inferred triple. Isolate inferred triples to `https://neurostrata.io/ns/graph/inferred#<namespace>`, emit RDF-star/provenance annotations only from verified/certified steps, and implement/extend `src/formal_kernel.rs::verify_derivation_certificate` to independently verify certificates. Full supported OWL-RL support is not optional.
- [x] **Step 4: Focused verification (evidence required)**
  Run `cargo test --locked --bin neurostrata-mcp rdf::reasoning::` and `cargo test --locked --bin neurostrata-mcp formal_kernel::`; report results with evidence before marking complete. No pass claims without evidence.
  Evidence (verified): reasoning 41 passed; formal kernel 11 passed; Lean parity 16 passed; competency 17 passed; Lean build 46 jobs. Independent review: zero Critical/Important. Focused only; no full-workspace-green claim.

---

### Task 2: Pre-Commit Ontology Blast-Radius & Semantic Closure Diff (`neurostrata-miio`) — implemented, verified

**Files:**
- Modify: `src/rdf/ontology.rs`
- Modify: `src/rdf/scenario.rs`
- Modify: `src/kernel/ports.rs`
- Modify: `src/server.rs`
- `src/daemon.rs` (verified: generic `/scenario` → `handle_scenario` already forwards the full request; no edit required for parity)
- Modify: `src/main.rs` (CLI parity)
- Modify (as needed): `crates/ontobug/*` for review/integration fixes to implemented helpers `diff_ontologies`, `generate_cypher_ddl`, and `OntologyDiff` (present in `crates/ontobug/src/diff.rs` and `src/lib.rs`, integrated and verified)
- Modify: `docs/features-api/mcp-and-http-api.md` and CLI docs as needed for surfaced operation

- [x] **Step 1: Graph-first + claim discipline (implementation session)**
  Claim `neurostrata-miio`; run `neurostrata_memories_for_file` on `src/rdf/ontology.rs`, `src/rdf/scenario.rs`, `src/kernel/ports.rs`, `src/server.rs`, `src/daemon.rs`, `src/main.rs`, and touched `crates/ontobug` files.
- [x] **Step 2: Write failing TDD tests (zero-mutation + wiring-visible)**
  Test `OntologyIndex::plan_mutation_diff` returns `added_entailments`, `removed_entailments`, `new_shacl_violations`, impacted sets, and `schema_ddl_diff` without mutating live store/scenario/task state. Cover `schema_ddl_diff` produced by the implemented `diff_ontologies`/`generate_cypher_ddl` helpers (no "DDL diff pending implementation" behavior) as consumed by the integrated path. Add/extend scenario/port/server-path tests for `neurostrata_scenario` operation `plan_ontology_diff` dispatch.
- [x] **Step 3: Implement zero-mutation plan + production wiring**
  Clone active namespace (or merge scenario overlay) into ephemeral sandbox, apply staged additions/retractions, compute pre/post closure deltas via Capability A fail-closed contracts, validate via `ontobug::validate::shacl_turtle`, and produce DDL diff via `crates/ontobug` `diff_ontologies` + `generate_cypher_ddl` (implemented and verified). Wire read-only `OntologyPort::plan_ontology_diff` (Ladybug impl) and shared `neurostrata_scenario(plan_ontology_diff)` through `src/server.rs`, `src/daemon.rs`, `src/main.rs` with identical semantics.
- [x] **Step 4: Focused verification (evidence required)**
  Run `cargo test --locked --bin neurostrata-mcp rdf::ontology::`, `cargo test --locked --bin neurostrata-mcp rdf::scenario::`, and focused ontobug/port/handler tests added; report evidence (no pass claims without evidence).
  Evidence (verified): `cargo check --locked --bin neurostrata-mcp` clean; fresh ontobug unit/doc tests green; `rdf::ontology::` 27 passed (incl. `plan_tests` 5); `rdf::scenario::` 3 passed; MCP `plan_ontology_diff` dispatch 1 passed. Independent review: zero Critical/Important. Focused only; no full-workspace-green claim.

---

### Task 3: Epistemic Trust Tiers, Freshness SLAs & Governed Deprecation Annotations (`neurostrata-50sm`) — implemented, verified

**Files:**
- Modify: `src/schemas/shacl/memory-shapes.v1.ttl` (**only this embedded SHACL file; keep total inventory at 8 files**)
- Modify: `src/rdf/provenance.rs` (tier parsing/validation/projection, `stale_after` RFC3339/epoch normalization helpers, deprecation/not_definition formatting helpers)
- Modify: `src/rdf/taxonomy.rs` (concept metadata: `not_definition`, deprecation; taxonomy briefing/compare/gaps/advise surfaces)
- Modify: `src/task/gate.rs` and/or task validation paths + snapshot/search/recall presentation paths (advisory `stale_memories`, banner/not_definition on production surfaces)
- Modify: `src/server.rs` / other production presentation modules as needed for `neurostrata_search_memory`, `neurostrata_memories_for_file`, `neurostrata_related`/recall, `neurostrata_get_snapshot`, `neurostrata_task_validate`
- Modify: `docs/features-api/subsystems/taxonomy-advisor.md` (co-change for taxonomy surfaces)

- [x] **Step 1: Graph-first + claim discipline (implementation session)**
  Claim `neurostrata-50sm`; run `neurostrata_memories_for_file` on `src/rdf/provenance.rs`, `src/rdf/taxonomy.rs`, `src/task/gate.rs`, `src/schemas/shacl/memory-shapes.v1.ttl`, and touched presentation handlers.
- [x] **Step 2: Write failing TDD tests (exact tiers, formats, surfaces)**
  Verify exact `epistemic_tier` values `unverified_agent, empirical_verified, formal_verified, human_governed`; reject elevated tiers without non-empty distinct `verified_by` (`generated_by != verified_by` for autonomous claims) in SHACL/provenance validation. Verify `stale_after` accepts RFC3339/ISO-8601 string and Unix epoch seconds and surfaces **advisory** `stale_memories` in `neurostrata_get_snapshot` and `neurostrata_task_validate`. Verify deterministic `[DEPRECATED — SUPERSEDED BY <id>: <reason>]` banners and `not_definition` on production surfaces (search, memories_for_file, related/recall, snapshot, task validate/views, taxonomy briefing/compare/gaps/advise).
- [x] **Step 3: Implement production logic (no ninth SHACL file)**
  Update `memory-shapes.v1.ttl` + `src/rdf/provenance.rs` invariants/projection, `src/rdf/taxonomy.rs` concept metadata and outputs, and shared presentation formatting wired into all required surfaces. Keep stale list advisory (no implicit push-blocking).
- [x] **Step 4: Focused verification (evidence required)**
  Run `cargo test --locked --bin neurostrata-mcp rdf::provenance::`, `cargo test --locked --bin neurostrata-mcp rdf::shacl::`, `cargo test --locked --bin neurostrata-mcp rdf::taxonomy::`, plus focused snapshot/task/search surface tests added; report evidence.
  Evidence (final review, child `neurostrata-815z`): RED-first confirmed; `cargo check --locked --bin neurostrata-mcp` clean, `rdf::provenance::` 46 passed, `rdf::shacl::` 15 passed, `rdf::taxonomy::` 15 passed, `task::gate::` 40 passed, production/AST/truncation tests green, server and task surface tests passed. Independent review: zero Critical/Important. Focused only; no full-workspace-green claim.

---

### Task 4: W3C `YAML-LD 1.0` / `Vault-LD` Bidirectional Markdown Knowledge Vault Sync (`neurostrata-dlag`) — implemented, verified

**Files:**
- Create/modify: `src/vault_ld/mod.rs` (shared sync engine: export/import, YAML-LD parse/render, diff, canvas, `stub:true`, supersession)
- Modify: `src/lib.rs` (module export as needed)
- Modify: `src/server.rs` (MCP tool `neurostrata_vault_sync` schema/list/dispatch/handler)
- Modify: `src/daemon.rs` (HTTP route/handler delegating to shared implementation)
- Modify: `src/main.rs` (CLI `vault-sync` command delegating to shared implementation)
- Modify: `docs/features-api/ops/obsidian.md`
- Modify: `docs/features-api/mcp-and-http-api.md`, `docs/features-api/cli-reference.md` (co-change for surfaced tool/route/command)

- [x] **Step 1: Graph-first + claim discipline (implementation session)**
  Claim `neurostrata-dlag`; run `neurostrata_memories_for_file` on `docs/features-api/ops/obsidian.md`, `src/vault_ld/mod.rs`, `src/server.rs`, `src/daemon.rs`, and `src/main.rs`.
- [x] **Step 2: Write failing TDD tests (strict, shared path)**
  Cover `Database -> YAML-LD 1.0 Markdown + topology.canvas -> Database`, diff-and-supersede import, unresolved `[[WikiLinks]]` → `stub:true`, **secret scan + SHACL (`ontobug::validate::shacl_turtle`) fail-closed before any store write**, `J6` `check_caller_path` rejection, ReBAC/privacy filtering, and SynapticMesh/private-row non-clobber. No remote fetch behavior.
- [x] **Step 3: Implement shared core and wire all surfaces**
  Implement `src/vault_ld` shared API used by MCP `neurostrata_vault_sync`, CLI `vault-sync`, and daemon HTTP handler. Enforce canonicalized vault path inside project root or `allowed_external_dirs`, strict validation order, and bitemporal supersession/write-lock discipline.
- [x] **Step 4: Focused verification (evidence required)**
  Run `cargo test --locked --bin neurostrata-mcp vault_ld::` and focused handler/wiring tests added; run `neurostrata-mcp doc-gate` if docs changed; report evidence.
  Evidence (verified): `vault_ld::` 31 passed; independent closure complete. Independent review: zero Critical/Important. Focused only; no full-workspace-green claim.

---

### Task 5: Trajectory-to-Procedure Mining & Outcome-Verified Belief Reflection (`neurostrata-mpau`) — implemented, verified

**Files:**
- Modify/create: `src/learning/trajectory.rs` (pure deterministic miner core, side-effect free candidate synthesis)
- Modify/create: `src/learning/reflection.rs` (pure outcome/reliability core + hook-facing APIs)
- Modify: `src/learning/mod.rs`
- Modify: `src/task/mod.rs` (actual `task_complete` / `handle_task_complete` completion-finalization hook, post-Lock-2)
- Modify: `src/rdf/action.rs` (actual successful `revert_action` commit-point hook; define-gate remains authoritative)
- Modify: `docs/features-api/subsystems/kinetic-actions.md` (co-change for `src/rdf/action.rs` if surfaced)

- [x] **Step 1: Graph-first + claim discipline (implementation session)**
  Claim `neurostrata-mpau`; run `neurostrata_memories_for_file` on `src/learning/mod.rs`, `src/learning/trajectory.rs`, `src/learning/reflection.rs`, `src/task/mod.rs`, and `src/rdf/action.rs`.
- [x] **Step 2: Write failing TDD tests (pure core + hook contracts)**
  Test that $\ge 3$ completed task trajectories with identical `(trigger_context, tool_or_action_sequence, verification_gate)` n-grams and 100% non-reverted completion synthesize SHACL-valid `procedure` candidates (and optional candidate `ActionTypeDefinition` payloads) deduplicated fail-closed via Laya. Test bounded reliability updates/review flags on negative outcomes, and that **actual hooks** trigger only at `task_complete` post-Lock-2 finalization and at successful `revert_action` commits (not rejected/dry paths). Candidate action types must pass the existing define gate (`ActionTypeDefinition::from_payload` / `validate_definition` / `define_action`) and are **never auto-applied**.
- [x] **Step 3: Implement pure cores and production hook integration**
  Keep miner/reflection computation deterministic and side-effect free where specified; perform persistence/supersession/review/candidate registration only through production hooks with ReBAC/privacy respect and no bypass of Lock 2 or `neurostrata_action(apply)`.
- [x] **Step 4: Focused verification (evidence required)**
  Run `cargo test --locked --bin neurostrata-mcp learning::`, `cargo test --locked --bin neurostrata-mcp rdf::action::`, and focused task-complete hook tests added; report evidence.
  Evidence (verified): hooks 13 passed; queue 5 passed; `learning::` 97 passed; `rdf::action::` 27 passed. Independent review: zero Critical/Important. Focused only; no full-workspace-green claim.

---

### Task 6: Ontology-Scoped Bounded Classification Ingestion (`neurostrata-v6dw`) — implemented, verified

**Files:**
- Modify/create: `src/rdf/ingest.rs` (strict bounded ingestion core: domain/vocabulary resolution, Kerner bounded KG construction + Adamchic O-RAG, batch validation, atomic commit)
- Modify: `src/rdf/ontology.rs` (governed domain subgraph/vocabulary helpers as needed)
- Modify: `src/server.rs` (production handler: MCP tool/operation schema + dispatch)
- Modify: `src/daemon.rs`, `src/main.rs` if HTTP/CLI parity is in release scope (shared semantics)
- Modify: relevant `docs/features-api/*` for surfaced handler/operation

- [x] **Step 1: Graph-first + claim discipline (implementation session)**
  Claim `neurostrata-v6dw`; run `neurostrata_memories_for_file` on `src/rdf/ingest.rs`, `src/rdf/ontology.rs`, `src/server.rs`, and any wired daemon/CLI/docs surfaces.
- [x] **Step 2: Write failing TDD tests (always strict, fail-closed)**
  Verify acceptance only for governed SKOS/OWL classes/predicates from the relevant domain subgraph. Verify rejection of invented classes/predicates, **empty domain/vocabulary fail-closed with zero writes**, and SHACL/post-image validation fail-closed **before any writes**. Verify **whole-batch validation before any writes** (no partial commits on batch failure). Include handler tests proving production exposure.
- [x] **Step 3: Implement core + production handler exposure**
  New bounded ingestion path is always strict (no opt-out/permissive bypass). Resolve governed vocabulary/domain subgraph, construct bounded candidates, validate entire batch, then commit atomically; emit `task_intent_signals`/`task_intent` metadata on resulting memories.
- [x] **Step 4: Focused verification (evidence required)**
  Run `cargo test --locked --bin neurostrata-mcp rdf::ingest::` and handler/wiring tests added; report evidence. No full-workspace suite; no pass claims without evidence.
  Evidence (verified): `rdf::ingest::` 24 passed; `rdf::ontology::` 16 passed at closure; MCP handler 1 passed; pinned tools list 1 passed. Independent review: zero Critical/Important. Focused only; no full-workspace-green claim.

---

### Task 7: Semantic & Retrieval Evaluation Gate (`neurostrata-dyrq`, Capability G) — implemented, verified (strict receipts; owner-approved honest FAIL)

**Evidence boundary (normative, synthetic regression gate only):** `evidence_class = "synthetic_deterministic"` and `production_roi_claimed = false`. MUST NOT claim owner-labeled production ROI, owner-labeled corpus, blind agent-answer accuracy, rollback, curation-cost, or real-namespace accuracy. These labels MUST appear only in `not_evaluated` (or explicit exclusion fields), never as pass claims. No production data, no external files/downloads, no live global-database assertions.

**Files:**
- Create: `src/semantic_eval/mod.rs`, `src/semantic_eval/metrics.rs`, `src/semantic_eval/competency.rs`, `src/semantic_eval/ablation.rs`
- Create: `eval/semantic/competency.json`, `eval/semantic/retrieval.json`
- Create: `tests/semantic_eval_metrics_test.rs`
- Modify: `src/main.rs` (add `semantic-eval` CLI variant + dispatch), `Cargo.toml` (integration-test target), `src/rdf/mod.rs` only if existing public helper visibility is required, `eval/registry.json`, release docs/changelog as applicable

- [x] **Step 1: Strict competency fixtures and offline ASK/SELECT runner (normative contract)**
  - Fixture: strict JSON `schema_version=1`, bounded `dataset_nquads`, `reasoning_profile`, `queries[]`; reject unknown fields/duplicates/malformed inputs.
  - Query forms: only `ASK`/`SELECT`; reject `SERVICE`, `CONSTRUCT`, `DESCRIBE`, `DELETE`, `INSERT` before evaluation (no service handler/network). Bounded budgets and fail-closed on truncation/timeout/malformed RDF-SPARQL/wrong result/mismatch.
  - Reasoning: canonical `owl-rl` profile for RDFS/OWL-RL materialized cases per Capability A certified path; include/report materialization/certificate evidence. **Do not claim Rdfs profile restricts rules.** Use production custom functions with empty metadata and fixed `now` (deterministic). No wall-clock timestamps/latency in competency receipt.
  - Expected comparison: `ASK` boolean; `SELECT` canonical sorted multiset preserving RDF term type/value/datatype/language and duplicate solutions.
  - Minimum coverage: >= 8 named synthetic questions with source-grounded rationale (direct type present/absent, RDFS subclass entailment, multi-level subclass SELECT exact bindings, OWL inverse, OWL transitive, SKOS pref/alt label SELECT preserving literals, negative cross-identity non-merge). Competency keeps its own expected ASK/SELECT fixtures and negative assertions (no unspecified `curation_notes` DTO field).

- [x] **Step 2: 60-query synthetic deterministic retrieval corpus and four real-store arms (normative contract)**
  - Corpus: strict fixture `schema_version=1`, `fixture_id`, `evidence_class="synthetic_deterministic"`, `dimensions=64`, documents, queries, `flat_aliases`, domains; reject unknown/malformed/duplicate/invalid-grade/empty/non-finite cases.
  - Distribution: exactly **60 queries** = **20 alias/canonicalization, 15 exact identifier, 15 hierarchy/type, 10 ambiguous/adversarial**; grades 1–3, >=1 relevant id per query, explicit `forbidden_ids`/negative controls; corpus must exercise ontology hierarchy (not alias-only). Include independent label review, negative controls, `curation_notes`.
  - Real stores: four independent temporary LadybugDB stores (`tempfile::tempdir()`), byte-identical documents, same `BenchEmbedder::new(64)`, identical candidate limits/include flags/graph-expansion behavior; real `LadybugStore::search_hybrid_with_domain` path only; no checked-in ranked lists/metric overrides; no ontology/state leakage.
  - Arm definitions (corrected): all `SearchMode::Default`.
    - **Arm 1 (baseline):** original query, `exact = false`, no ontology/domain-registry row, shipped vocabulary retained.
    - **Arm 2 (exact):** original query, `exact = true`, no ontology/domain-registry row, shipped vocabulary retained.
    - **Arm 3 (flat aliases):** `exact = true`, production alias-only `OntologyIndex`/domain registry installed with **all parents `null`** (same aliases as arm 4, no hierarchy); both arm 3 and arm 4 pass the **original query text** through the same production path (no arm-3-only alias rewrite or re-embed). Hierarchy is the only arm3→arm4 mechanism change.
    - **Arm 4 (ontology):** `exact = true`, same aliases as arm 3 plus hierarchy via compiled registry/`OntologyIndex`; same production path and original query text as arm 3, using the existing production expansion path (no second hand-coded ontology ranker).

- [x] **Step 3: Metrics, thresholds, determinism, and fail-closed receipt (normative contract)**
  - Iterations/timing: warm up once, then **>= 3 identical iterations**; record per-query rankings for every arm and compute p95 from **per-query medians across iterations**. Exclude latency/timing samples from canonical comparison/determinism digests.
  - Metrics: `ndcg_at_10` (`(2^gain - 1) / log2(rank + 2)`), `recall_at_5`, `precision_at_5` (duplicates count once), ontology-targeted `recall_at_5` (alias + hierarchy subset), forbidden hits, p95 overhead, derived storage. All serialized metrics finite.
  - Derived storage (corrected): **canonical serialized arm4 index/registry bytes / retrieval fixture JSON bytes**.
  - Accepted thresholds: arm4 vs arm3 >= 5% relative nDCG@10 gain OR >= 5 pp Recall@5 gain; alias+hierarchy subset >= 10 pp Recall@5 gain; Precision@5 drop <= 2 pp; zero arm4 forbidden identity hits; p95 overhead <= 25 ms and <= 10% (zero-baseline fails only when absolute overhead > 25 ms); derived storage < 5%; two executions produce identical ranked ids, forbidden hits, finite metrics, and matching canonical comparison SHA-256.
  - CLI/receipt: `neurostrata-mcp semantic-eval --competency eval/semantic/competency.json --retrieval eval/semantic/retrieval.json --iterations 3 --output /tmp/opencode/semantic-eval.json --check` writes/prints strict JSON receipt even on failure and exits nonzero with `--check` on any competency/ablation/threshold/determinism violation. Receipt MUST include `schema_version`, package version, fixture ids+SHA-256 digests, `evidence_class="synthetic_deterministic"`, `production_roi_claimed=false`, competency per-query results + materialization/certificate digest evidence, four-arm aggregate/per-query metrics/rankings/forbidden hits, ontology-targeted Recall@5, p95 overhead, derived-storage %, determinism comparison SHA-256, violations, overall `PASS`/`FAIL`, and `not_evaluated` including `owner_labeled_production_roi`, `blind_agent_answer_accuracy`, `owner_labeled_corpus`, `rollback_validation`, `curation_cost`, `real_namespace_accuracy`.

- [x] **Step 4: Registration, docs, and independent verification (evidence retained)**
  - Register `semantic-evaluation` focused suite in `eval/registry.json` with explicit expected test names and `ignored: 0`; numerical acceptance remains owned by semantic CLI receipt.
  - CLI docs/changelog updated; Capability G contract aligned in this plan and spec.
  - Evidence (verified, strict receipts): remediation 65/65 (36 metrics + 17 competency + 12 ablation), zero ignored/failed; two `semantic-eval --check` runs exit nonzero honestly; competency 8/8 PASS; retrieval FAIL solely on mixed alias+hierarchy Recall@5 lift 0.042857 < 0.10; identical determinism SHA-256 `5f378d7b6d2666dbaef44a06fa80de1b75f5dec2c99951559593f52db4f45583` across both runs. `evidence_class="synthetic_deterministic"`, `production_roi_claimed=false`, six `not_evaluated` exclusions. Owner-approved honest FAIL; no full-workspace-green or semantic-PASS claim. Independent review: zero Critical/Important.
