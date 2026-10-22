# Recall Closure and Negative Exemplars Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the recall-closure spec: a declared `metadata.attempt` shape that makes a failed approach retrievable as a negative exemplar, a per-namespace domain registry that rejects an unregistered slug with a remedy, a snapshot domain strap that surfaces rows the `access_count` cut would hide, and the counters that measure both.

**Architecture:** Three new flat modules beside the existing ones — `src/registry.rs` (the registry cache and the write-path check), `src/attempt.rs` (one predicate for "this row is a failed attempt", shared by the store's shape check, the rendering, the snippet prefix and the duplicate partition), `src/recall_metrics.rs` (the process-global counters). Everything else wires into seams that already exist: the store's per-row metadata filter, the snapshot's retained list, the write path's metadata assembly, the metrics handler's body. No trait changes, no new dependencies, no storage migration.

**Tech Stack:** Rust (edition 2021), tokio (full features), serde_json, uuid v5, LadybugDB store, the MCP JSON-RPC tool surface.

**Spec:** `docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md` (the plan argues from the spec; the spec travels with it — executors read both)

## Global Constraints

- **The vocabulary bump is 5 → 6.** The task description's "one bump 4 → 5" is stale: the parent spec's 4 → 5 bump (`fact.fields.topology`) already landed with `neurostrata-426k`, and the vocabulary is at `vocabulary_version: 5` today (`src/schemas/memory-vocabulary.v3.json:2`).
- **There are 20 acceptance criteria**, not 17. The spec grew criteria 11–13 (the acceptance-before-write ordering, the normalizer parity, the non-string rejection) and renumbered the rest (§6, lines 645–694).
- **No deferrals.** Every open point is decided in the spec (§11 is the closed list) or filed as a named task. No task in this plan may add a "later" bucket.
- **Two items are named tasks, not this plan's work, and this plan does not touch them:** the deduplication distance band (`neurostrata-r7dj`, the local-self-improvement spec's Phase 2) and the mesh's import doc contradicting its code (`neurostrata-wl4i`). Both are already filed and owned; the spec's design is safe under either reading.
- **No new build warnings.** The baseline is 14 production warnings and 17 test-unit warnings. A new module whose functions are never called is a dead-code warning — every new module lands in the same task as its first caller.
- **No new dependencies.** `uuid` and `tokio` are already in `Cargo.toml`.
- **No consumer-system facts and no consumer vocabulary in the `NeuroStrata` namespace.** `domain`, `namespace`, `memory_type` are NeuroStrata concepts; anything else is not.
- **The spec's anchors are live.** Every task re-derives the spec citations its diff invalidates and marks each changed row `*[changed by neurostrata-r30c: <clause>]*`. A citation that no longer points at the code it names is a defect.
- **TDD.** Every behavioural change lands as a failing test first, then the minimal implementation, then the test passing.
- **Work in place on `main`.** No worktree. One commit per task. `git pull --rebase` then `git push` only after `neurostrata_task_complete` succeeds (Lock 2).
- **Every rule this work extracts carries `metadata.source`** — the pre-push gate rejects a rule without it.

## Review Focus

Five input classes the spec implies but no acceptance criterion names. Each line gets its test in the task that owns the code.

1. **A write whose `domain` is an object, an array, a number or `null`** — the write is refused with a message that names the registered list; the store never sees a non-string domain.
2. **A namespace whose registry row was deleted or never written, carrying a legacy free-text domain (`"Database Access"`)** — the next domain write re-bootstraps the registry with the slug form and succeeds; no manual repair step exists.
3. **A row whose content is shorter than the `[FAILED ATTEMPT] ` prefix budget** — the snippet still carries the prefix and is still ≤ 120 characters total; the prefix is never silently dropped to fit the content.
4. **Two writers registering domains at once (the daemon's tool call and the CLI's `add`)** — the cache may miss one slug, but no write is refused for a slug the namespace's own rows carry, and the next reconcile repairs the cache.
5. **A domain-filtered recall in a namespace whose registry holds exactly one domain** — the filter cannot narrow anything; the call is counted as a bypass, not as a narrowing hit.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `src/registry.rs` (create) | The per-namespace domain registry: the deterministic row id, the row's content and metadata, the read/merge/write under the registry mutex with the bounded compare-and-retry, the bootstrap scan, the reconcile scan, the write-path acceptance decision. | 3 |
| `src/attempt.rs` (create) | One predicate for "this row carries a valid failed attempt" plus its shape defects. The single definition the store's shape check, `format_memory`, the snippet prefix and the duplicate partition all call. | 5 |
| `src/recall_metrics.rs` (create) | The process-global counters (four `domain_filter`, two `recall`) and the bounded set of recently-recalled ids that joins them. | 7 |
| `src/main.rs` (modify) | Three `mod` lines, in the file's existing alphabetical order. | 3, 5, 7 |
| `src/schemas/memory-vocabulary.v3.json` (modify) | `vocabulary_version` 6; `fact.fields.attempt`; `fact.fields.domain_registry`; the two `tool_summaries` texts. | 2 |
| `src/traits.rs` (modify) | The single vocabulary assert (now 6), extended with the two new declared fields. | 2 |
| `src/server.rs` (modify) | The write path's registry check and cache update; `register_domain` on the `add_memory` schema; the `Attempt [FAILED in …]` line in `format_memory`; the duplicate partition; the snapshot's registry exclusion, domain strap and `Registered domains (N)` header; the `domain` property on the `get_snapshot` schema; the counters at the two search sites; the promotion counter in `handle_get_memory`. | 3, 4, 5, 6, 7 |
| `src/store/ladybug.rs` (modify) | The registry-row exclusion at the four per-row filter sites; the `metadata.attempt` shape check beside the `topology` check. | 4, 5 |
| `src/task/mod.rs` (modify) | The write-path registry check on the completion path. | 3 |
| `src/buffer.rs` (modify) | The `[FAILED ATTEMPT] ` snippet prefix; the recall counters and the recently-recalled id set. | 5, 7 |
| `src/handlers/bus_metrics.rs` (modify) | The composite body: the bus's eight integers unchanged, plus `domain_filter` and `recall` as extra top-level keys. | 7 |
| `src/synaptic_mesh/import.rs` (modify) | The import step unions an imported record's domain into the local registry. | 8 |
| `docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md` (modify) | The anchor re-derivation (Task 1) and the per-task `[changed by neurostrata-r30c: …]` markers. | 1, 3–9 |
| `CHANGELOG.md` + the repo's recall docs (modify) | The honest-recall note and the registry note. | 9 |

**Task order.** The spec's own order is `1 → 2 → 3 → 4 → 6 → 8 → 9 → 10 → 11` (steps 1, 5 and 7 landed with the parent). This plan merges the spec's steps 2 and 4 into one task and moves the vocabulary bump (step 3) ahead of it: the registry helpers are dead code until the write path calls them, and an uncalled module adds a dead-code warning above the baseline. The dependency the spec names — "the registry check (4) needs the registry helpers (2) and the vocabulary declaration (3)" — is satisfied.

---

### Task 1: Re-derive the spec's stale anchors

The spec's §2 table anchors are live against the working tree (as of 2026-10-22). Its prose anchors in §3, §5 and §12 are as of `c5acd4f` — 30 of them — and the spec's own anchor-scope note says they are re-derived by this plan's first task, before anything is implemented. Do not treat a stale §3/§5/§12 anchor as a live one.

**Files:**
- Modify: `docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md` (§3, §5, §12 — the 30 prose anchors)

**Interfaces:**
- Consumes: nothing (a documentation task).
- Produces: a spec whose §3/§5/§12 citations point at the code they name in the working tree at `06fb26f`.

- [ ] **Step 1: List the 30 stale citations**

  Read the spec's §3, §5 and §12 and extract every `src/…:NNN` citation. §2's table is excluded (its anchors are already live). Expect roughly 7 in §3, 4 in §5 and 17 in §12 (the panel record cites its own findings).

- [ ] **Step 2: Re-derive each citation against the working tree**

  For each citation, find the line the same code now occupies and re-point the citation. The rule that decides it: the citation names a *piece of code*, not a line number — re-point it to where that code is now, and if the code no longer exists under that name, say so in the citation rather than pointing at a neighbour.

  The known drifts at this HEAD, verified while writing this plan:

  | The spec says | The code is at |
  |---|---|
  | `src/traits.rs:263` (the vocabulary assert) | `src/traits.rs:309-321` (the assert is the test `memory_vocabulary_v5_parses_and_has_exact_relations`) |
  | `src/store/ladybug.rs:1083-1094` (the base search filter) | `src/store/ladybug.rs:114-124` |
  | `src/store/ladybug.rs:1206-1213` (one-hop) | `src/store/ladybug.rs:277-284` |
  | `src/store/ladybug.rs:1262-1269` (two-hop) | `src/store/ladybug.rs:333-340` |
  | `src/store/ladybug.rs:1341-1347` (episode sibling) | `src/store/ladybug.rs:412-427` |
  | `src/store/ladybug.rs:1083-1094` (the shape-warning seam) | `src/store/ladybug.rs:951-955` (the struct), `:970` (`parse_edge_specs`), `:1101` (`write_shape_warnings`), `:1106` (the stderr emission) |
  | `src/store/ladybug.rs:424` (the `topology` shape check) | `src/store/ladybug.rs:1066-1095` |
  | `src/store/ladybug.rs:529-530` (the stderr emission) | `src/store/ladybug.rs:1106-1107` |
  | `src/store/ladybug.rs:388-410` / `:281-299` / `:289-320` (the store's locking) | verify each against the current `with_conn` / `write_with_deadline` |
  | `src/store/ladybug.rs:1275` / `:1285` / `:2267` (the filter predicates) | already correct |
  | `src/store/ladybug.rs:1692-1702` (`store.list`) | `src/store/ladybug.rs:1849` |
  | `src/store/ladybug.rs:2160` (`increment_access_count`) | already correct |
  | `src/server.rs:1278-1281` / `:1319-1325` (the write path) | `src/server.rs:1280-1296` (the metadata assembly) and `:1330` (the write) |
  | `src/server.rs:1427-1430` / `:1442` / `:1446` / `:1454-1493` (the snapshot) | `src/server.rs:1427-1430`, `:1452-1457`, `:1476-1499` |
  | `src/server.rs:2222` (`format_memory`) | `src/server.rs:2276` |
  | `src/server.rs:1152-1173` (the duplicate check) | `src/server.rs:1138-1175` |
  | `src/server.rs:2019` / `:2652` (the access-count increments) | `src/server.rs:2050` and `:2706` |
  | `src/server.rs:153-161` (the `get_snapshot` schema) | `src/server.rs:153` |
  | `src/server.rs:1956` (the search tool's domain argument) | `src/server.rs:2007-2024` |
  | `src/buffer.rs:129-177` / `:169-176` (the append_log recall) | `src/buffer.rs:133-297`, the recall search at `:252`, the snippet at `:275-285` |
  | `src/task/mod.rs:1131-1141` / `:1181-1188` (the completion) | `src/task/mod.rs:1130-1155` and `:1181-1188` |
  | `src/schemas/memory-vocabulary.v3.json:66-72` / `:161` / `:163` / `:167` | `:66-75` (the fact), `:163-168` (`tool_summaries`) |
  | `src/main.rs:110-117` / `:1773-1777` / `:1815-1823` / `:2844` / `:2862` | verify each against the current `main.rs` |
  | `src/handlers/bus_metrics.rs:20-23` | already correct |

  Every remaining citation in §3/§5/§12 not in this table is verified the same way: find the code, cite where it is.

- [ ] **Step 3: Mark the rows whose claim the parent implementation changed**

  A citation that moved is not the same as a claim that moved. Where the parent's implementation changed what the sentence says, mark the row `*[changed: …]*` and say what changed — the spec's §2 table already carries four such markers; §3/§5/§12 may need more (for example §3.2.3's "written into the metadata **raw**" is stale: the write now normalizes the domain to a slug, `src/server.rs:1292-1296`).

- [ ] **Step 4: Verify every citation resolves**

  For each re-pointed citation, read the cited line and confirm it is the code the sentence names. A citation that resolves to a blank line, a comment or a neighbouring function is still stale.

- [ ] **Step 5: Commit**

  ```bash
  git add docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md
  git commit -m "docs: re-derive the closure spec's prose anchors against the working tree"
  ```

---

### Task 2: The vocabulary bump 5 → 6

The vocabulary is the one place the agent learns what a shape is. Two new declared fields ship with version 6: `fact.fields.attempt` and `fact.fields.domain_registry`.

**Files:**
- Modify: `src/schemas/memory-vocabulary.v3.json:2` (the version), `:66-75` (the `fact` block), `:163-168` (`tool_summaries`)
- Modify: `src/traits.rs:309-321` (the assert)
- Test: `src/traits.rs` (the vocabulary assert's own test module)

**Interfaces:**
- Consumes: nothing.
- Produces: `vocabulary_version: 6`; `memory_types.fact.fields.attempt` = `{approach, outcome, failure_mode, context}` (all four required strings; `outcome` ∈ `["failed"]`); `memory_types.fact.fields.domain_registry` = `{domains: [...]}` (sorted slugs); the `tool_summaries.add` and `tool_summaries.memory_type` texts mention both.

- [ ] **Step 1: Write the failing test**

  In `src/traits.rs`'s test module, rename `memory_vocabulary_v5_parses_and_has_exact_relations` to `memory_vocabulary_v6_parses_and_has_exact_relations`, change its version assertion to 6, and add:

  ```rust
  let fact = &v["memory_types"]["fact"]["fields"];
  assert!(fact.get("attempt").is_some(), "the fact declares the attempt shape: {v}");
  assert!(fact.get("domain_registry").is_some(), "the fact declares the registry shape: {v}");
  assert_eq!(fact["attempt"]["type"], "object");
  assert_eq!(fact["domain_registry"]["type"], "object");
  ```

  Run: `cargo test --bin neurostrata memory_vocabulary_v6` → Expected: FAIL — the version is still 5 and the two fields are absent.

- [ ] **Step 2: Bump the version and declare the two fields**

  In `src/schemas/memory-vocabulary.v3.json`:

  - `"vocabulary_version": 5` → `"vocabulary_version": 6`.
  - Add to `memory_types.fact.fields` (beside `topology`) an `attempt` object whose `definition` says, in the agent's own words: a `fact` may carry `metadata.attempt` with the four keys `approach`, `outcome`, `failure_mode`, `context`, all strings, `outcome` being exactly `"failed"` — **evidence of one episode, not a normative rule**.
  - Add a `domain_registry` object whose `definition` says: the namespace's registry row carries `metadata.domain_registry.domains`, the sorted list of the domain slugs the namespace uses; the row is private, excluded from recall and from the snapshot body.

- [ ] **Step 3: Teach the agent the two shapes**

  In `tool_summaries.add` and `tool_summaries.memory_type`, name both new fields and their keys. The `topology` sentence in each stays as it is.

- [ ] **Step 4: Run the test to verify it passes**

  Run: `cargo test --bin neurostrata memory_vocabulary_v6` → Expected: PASS.

- [ ] **Step 5: Check the build's warning count has not moved**

  Run: `cargo build 2>&1 | grep -c '^warning'` → Expected: 14 (the baseline). Run: `cargo test --no-run 2>&1 | grep -c '^warning'` → Expected: 17.

- [ ] **Step 6: Re-derive the spec citations this task moved**

  The vocabulary schema and `src/traits.rs` are cited in §2's table (`src/schemas/memory-vocabulary.v3.json:2`, `:39-64`, `:161`, `:163`, `:167`; `src/traits.rs:312`) and in §3.1.1, §3.2.4, §4, §5 step 3, §6 criterion 3, §7 and §12. Re-point each to its new line, and mark the rows whose claim changed with `*[changed by neurostrata-r30c: the vocabulary is v6 and declares fact.fields.attempt and fact.fields.domain_registry]*`.

- [ ] **Step 7: Commit**

  ```bash
  git add src/schemas/memory-vocabulary.v3.json src/traits.rs docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md
  git commit -m "feat(vocabulary): declare the attempt and domain_registry shapes at version 6"
  ```

---

### Task 3: The registry helpers and the write-path registry check

The registry row is a **cache**, not the truth: the namespace's own rows are the truth. A stale cache is a miss, repaired by the reconcile; it is never a wrong acceptance.

**Files:**
- Create: `src/registry.rs`
- Modify: `src/main.rs:1-21` (one `mod` line, alphabetically)
- Modify: `src/server.rs:108-140` (the `add_memory` schema), `:1280-1296` (the metadata assembly), `:1313-1330` (the write path)
- Modify: `src/task/mod.rs:1130-1188` (the completion's write path), and the `task_complete` tool schema in `src/server.rs:310-333`
- Test: `src/registry.rs` (its own test module), `src/server.rs` (the write-path tests), `src/task/mod.rs` (the completion's tests)

**Interfaces:**
- Consumes: `crate::domain::normalize_domain_slug(&str) -> Option<String>` and `crate::domain::domain_key(&serde_json::Value) -> Option<String>` (`src/domain.rs:3`, `:29`) — **the same normalizer on every path**.
- Produces (all in `src/registry.rs`, all `pub`):

  ```rust
  pub fn registry_row_id(namespace: &str) -> String;      // Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("neurostrata:domain_registry:{namespace}"))
  pub fn registry_content(namespace: &str) -> String;    // format!("Domain registry for {namespace}")
  pub fn is_registry_row(metadata: &serde_json::Value) -> bool;   // metadata.domain_registry is present
  pub fn registry_domains(metadata: &serde_json::Value) -> Vec<String>;  // sorted, deduplicated
  pub fn registry_metadata(domains: &[String]) -> serde_json::Value;    // {domain_registry:{domains}, eligibility:"private"}

  pub struct DomainCheck {
      /// The domain list the cache must hold after this write, or None when the cache is already current.
      pub cache: Option<Vec<String>>,
      /// A warning for the caller. Never a rejection.
      pub warning: Option<String>,
  }

  pub async fn check_domain(
      store: &std::sync::Arc<dyn crate::traits::VectorStore>,
      emb: &std::sync::Arc<dyn crate::traits::Embedder>,
      namespace: &str,
      raw_domain: &serde_json::Value,
      register_domain: bool,
  ) -> Result<DomainCheck, String>;

  pub async fn write_registry(
      store: &std::sync::Arc<dyn crate::traits::VectorStore>,
      emb: &std::sync::Arc<dyn crate::traits::Embedder>,
      namespace: &str,
      domains: &[String],
  ) -> Option<String>;   // None on success; Some(warning) when the cache did not stick

  pub async fn registry_domain_count(
      store: &std::sync::Arc<dyn crate::traits::VectorStore>,
      namespace: &str,
  ) -> usize;
  ```

- [ ] **Step 1: Write the failing tests**

  In `src/registry.rs`'s own test module, cover the pure parts first:

  - `registry_row_id` is stable for a namespace and differs between namespaces, and parses as a UUID (`uuid::Uuid::parse_str`).
  - `registry_domains` sorts and deduplicates, and `registry_metadata` round-trips it.
  - `is_registry_row` is true only for a metadata carrying `domain_registry`.

  In `src/server.rs`'s test module, cover the write path (the store helper used by these tests is the same mock store the existing `add_memory` tests use):

  - `a_write_in_a_namespace_with_no_registry_row_bootstraps_the_registry` — a namespace holding one memory with `metadata.domain == "Database Access"`; a second write with `domain: "Auth"` succeeds; the registry row now lists `["auth", "database-access"]`.
  - `an_unregistered_slug_is_rejected_and_nothing_is_written` — a third write with `domain: "storage"` returns the rejection message; the store's row count is unchanged.
  - `register_domain_true_accepts_the_slug_and_registers_it` — the same write with `register_domain: true` succeeds; the registry lists `["auth", "database-access", "storage"]`.
  - `a_slug_the_namespace_already_carries_is_accepted_by_the_reconcile` — delete the registry row, write `domain: "database-access"`, and the write succeeds (the reconcile finds the slug in the namespace's own rows) and the registry row is repaired.
  - `a_non_string_domain_is_rejected` — `domain: 42` is refused with a message naming the registered list; nothing is written.
  - `a_domain_with_no_usable_slug_is_stored_as_it_was_given` — `domain: "!!!"` does not normalize to a slug; the write succeeds, the value is stored raw, and no registry row is touched.
  - `a_write_with_no_domain_touches_no_registry_row` — the write succeeds and the namespace still has no registry row.

  In `src/task/mod.rs`'s test module:

  - `a_completion_with_an_unregistered_domain_is_rejected` — the completion returns the rejection and the task does not reach `done`.
  - `a_completion_with_register_domain_true_registers_the_domain`.

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo test --bin neurostrata registry` → Expected: FAIL — `src/registry.rs` does not exist yet.

- [ ] **Step 3: Create `src/registry.rs` with the pure helpers**

  `registry_row_id` is a UUID v5 over the OID namespace and the string `neurostrata:domain_registry:<namespace>` — the mesh skips any row whose id is not a parseable UUID. `registry_metadata` sets `eligibility: "private"`, so the row is never exported. `registry_domains` reads `metadata.domain_registry.domains`, keeps the strings, and returns them sorted and deduplicated.

- [ ] **Step 4: Implement `check_domain`**

  The decision is made **before** any registry write, and in this order:

  1. `raw_domain` that is not a string → `Err` with the rejection text (the non-string rejection; criterion 13).
  2. Normalize with `crate::domain::normalize_domain_slug`. A slug that does not normalize (a string with no usable slug) is **not governed by the registry**: the check returns `DomainCheck { cache: None, warning: None }` and the write stores it exactly as the parent stored it. It has no slug to register, and refusing it would leave the caller with no remedy.
  3. Read the registry row: `store.get(namespace, &registry_row_id(namespace))`.
  4. **Absent** → the bootstrap: `store.list(namespace, None)`, normalize every row's `metadata.domain` with the same normalizer, union with the incoming slug, and return `DomainCheck { cache: Some(unioned), warning: None }`. The write always succeeds — the bootstrap errs toward registering more.
  5. **Present and the slug is in the cache** → `DomainCheck { cache: None, warning: None }` (the hot path: one read, no write).
  6. **Present and the slug is not in the cache** → the reconcile: the same `store.list` scan. If the slug is among the derived slugs → `DomainCheck { cache: Some(derived ∪ slug), warning: None }` (accepted, cache repaired). If it is not:
     - `register_domain == true` → `DomainCheck { cache: Some(cached ∪ slug), warning: None }`.
     - otherwise → `Err` with the rejection text.

  The rejection text, exactly:

  ```
  ERROR [DOMAIN]: domain '{slug}' is not registered in namespace '{namespace}'.
  Registered domains: {json array}. Use one of those, or pass
  register_domain: true if '{slug}' is a genuinely new domain.
  ```

  The registered array is `serde_json::to_string(&domains)` — a JSON array of the sorted slugs, `[]` when the cache is empty.

- [ ] **Step 5: Implement `write_registry`**

  A read-merge-write under a **process-wide registry mutex** (`tokio::sync::Mutex` — the guard is held across `await`s, so a `std::sync::Mutex` will not do):

  1. Take the mutex.
  2. Read the registry row; merge the given domains into its list (sorted, deduplicated).
  3. Write it: `store.upsert(namespace, &registry_row_id(namespace), vector, payload)` with `vector` = `emb.embed(&registry_content(namespace))` — a real embedding, never a zero vector — and `payload.metadata` = `registry_metadata(&merged)`.
  4. Re-read the row and check that **every slug in the pre-write set** is still present (not only the new slug). If one is missing, re-merge and write again — **at most three attempts in total**.
  5. If the attempts are exhausted, or the store errored → `Some(warning)`; the write to the namespace still proceeds (the acceptance decision was already made).

  The retry cannot see a write that landed *before* ours — that other process's slug is dropped from the cache. It is a miss, not a wrong acceptance and not a corrupted row; the reconcile of step 4.6 repairs it.

- [ ] **Step 6: Implement `registry_domain_count`**

  Read the registry row and return the number of domains it holds (0 when the row is absent). This is the `single_domain_bypasses` counter's source (Task 7).

- [ ] **Step 7: Wire the check into `neurostrata_add_memory`**

  In `src/server.rs`:

  - The tool schema (`:108-140`) gains one optional boolean property `register_domain`, described as "Set to true ONLY if the domain is genuinely new for this namespace; the registry rejects any other unregistered domain."
  - In the metadata assembly (`:1280-1296`), capture the **raw** domain before the normalization rewrites it: `let raw_domain = metadata.get("domain").cloned();` immediately after the `domain` argument is inserted into the metadata object, and before the slug normalization at `:1292-1296`.
  - After the namespace-existence check (`:1313-1315`) and before the write (`:1330`), call `check_domain`. On `Err`, return the rejection string immediately — nothing is written. On `Ok`, carry `check.cache` forward.
  - After the memory write succeeds, if `check.cache` is `Some(domains)`, call `write_registry`. On `Some(warning)`, append a line to the response message: `Registry warning: {warning}`.

  The completion path (`src/task/mod.rs:1130-1188`) does the same: the slug comes from the caller's `memory.metadata.domain` (there is no top-level `domain` argument on this path); the completion's `register_domain` argument is a no-op when the completion carries no domain; the check runs before the write at `:1181-1188`; a rejection returns the message and the task does not complete. The `neurostrata_task_complete` tool schema (`src/server.rs:310-333`) gains the same optional boolean `register_domain`, described as applying to `memory.metadata.domain`.

- [ ] **Step 8: Add `mod registry;` to `src/main.rs`**

  Between `mod parser;` and `mod secrets;` — alphabetical at this point in the sequence. Task 7 later inserts `mod recall_metrics;` between `mod parser;` and `mod registry;`, so the final order is alphabetical.

- [ ] **Step 9: Run the tests to verify they pass**

  Run: `cargo test --bin neurostrata registry` → Expected: PASS.
  Run: `cargo test` → Expected: the whole suite passes, with the new tests added to the count.

- [ ] **Step 10: Check the build's warning count has not moved**

  Run: `cargo build 2>&1 | grep -c '^warning'` → Expected: 14. Run: `cargo test --no-run 2>&1 | grep -c '^warning'` → Expected: 17.

- [ ] **Step 11: Re-derive the spec citations this task moved**

  `src/server.rs`, `src/task/mod.rs` and `src/main.rs` are cited in §2's table and in §3.2.2, §3.2.3, §3.2.5, §5 steps 2 and 4, §6 criteria 7, 8, 11, 12, 13, §8, §10 and §12. Re-point each citation this task's diff invalidated, and mark the changed rows with `*[changed by neurostrata-r30c: <clause>]*` — the registry check now sits after the namespace-existence check and reads the raw domain captured before the slug normalization.

- [ ] **Step 12: Commit**

  ```bash
  git add src/registry.rs src/main.rs src/server.rs src/task/mod.rs docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md
  git commit -m "feat(registry): reject an unregistered domain with the registered list and a remedy"
  ```

---

### Task 4: The registry row excluded from recall and from the snapshot

The registry row is metadata, not knowledge. It must never reach an agent as a result.

**Files:**
- Modify: `src/store/ladybug.rs:114-124`, `:277-284`, `:333-340`, `:412-427` (the four per-row filter sites)
- Modify: `src/server.rs:1452-1457` (the snapshot's retains)
- Test: `src/store/ladybug.rs`, `src/server.rs`

**Interfaces:**
- Consumes: `crate::registry::is_registry_row(&serde_json::Value) -> bool` (Task 3).
- Produces: nothing new — the store and the snapshot both drop a row whose metadata carries `domain_registry`.

- [ ] **Step 1: Write the failing tests**

  In `src/store/ladybug.rs`'s test module:

  - `the_registry_row_never_appears_in_search_results` — write a namespace holding a normal `fact` and a registry row (the row Task 3's `write_registry` writes), search for the normal fact's vector, and assert the registry row's id is absent from the results.

  In `src/server.rs`'s test module:

  - `the_snapshot_body_holds_no_registry_row` — the same namespace, take a snapshot, and assert the registry row's id is absent from the body **and** that the body still names the registered domains (Task 6 renders that header; until then assert only the absence).

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo test --bin neurostrata registry_row` → Expected: FAIL — the row is still returned.

- [ ] **Step 3: Apply the exclusion at the four filter sites**

  In `src/store/ladybug.rs`, at each of the four per-row filter sites, add the registry predicate beside `is_archived` and `is_mesh_conflicting`:

  ```rust
  if crate::registry::is_registry_row(&metadata_val) { continue; }
  ```

  The four sites are the base search (`:114-124`), the one-hop expansion (`:277-284`), the two-hop govern expansion (`:333-340`) and the episode-sibling pass (`:412-427`). The sibling pass could not reach the registry row anyway — it carries no `EXTRACTED_FROM` edge — but the rule is applied there too so it is uniform rather than incidental.

  This is a metadata predicate, **not** the structural-type exclusion: that one is a fixed `const [&str; 5]` consulted at query-build time with no per-row hook, and the registry row is a `fact`.

- [ ] **Step 4: Apply the exclusion in the snapshot**

  In `src/server.rs`, beside the archived and mesh-conflicting retains (`:1452-1457`):

  ```rust
  all_memories.retain(|r| !crate::registry::is_registry_row(&r.payload.metadata));
  ```

  The snapshot's list comes from `store.list`, a bare `MATCH` with no row-class filters, so this retain is the snapshot's own exclusion — it costs nothing beyond the scan.

- [ ] **Step 5: Run the tests to verify they pass**

  Run: `cargo test --bin neurostrata registry_row` → Expected: PASS. Run: `cargo test` → Expected: the whole suite passes.

- [ ] **Step 6: Check the build's warning count has not moved**

  Expected: 14 production warnings, 17 test-unit warnings.

- [ ] **Step 7: Re-derive the spec citations this task moved**

  `src/store/ladybug.rs`'s four filter sites and `src/server.rs`'s snapshot retains are cited in §2's table and in §3.2.1, §3.3.3, §5 step 6, §6 criterion 9 and §12. Re-point them and mark the changed rows.

- [ ] **Step 8: Commit**

  ```bash
  git add src/store/ladybug.rs src/server.rs docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md
  git commit -m "feat(store): drop the registry row from every result set and from the snapshot"
  ```

---

### Task 5: The attempt shape check, the `[FAILED ATTEMPT]` rendering, the duplicate partition

A negative exemplar is a `fact` carrying a declared `metadata.attempt`. It is retrievable (the vector is the embedding of the content, never negated) and non-poisoning (the rendering and the snippet prefix mark it as a failure). The malformed shape warns; it never rejects the write.

**Files:**
- Create: `src/attempt.rs`
- Modify: `src/main.rs:1-21` (one `mod` line)
- Modify: `src/store/ladybug.rs:1066-1095` (the shape check beside the `topology` check), `:957-966` (the declared keys)
- Modify: `src/server.rs:2276-2345` (`format_memory`), `:1138-1175` (the duplicate check)
- Modify: `src/buffer.rs:275-285` (the recall snippet)
- Test: `src/attempt.rs`, `src/store/ladybug.rs`, `src/server.rs`, `src/buffer.rs`

**Interfaces:**
- Consumes: nothing (this task defines the predicate the later tasks rely on).
- Produces (in `src/attempt.rs`):

  ```rust
  pub const ATTEMPT_KEYS: [&str; 4];       // ["approach", "outcome", "failure_mode", "context"]
  pub const FAILED_ATTEMPT_PREFIX: &str;   // "[FAILED ATTEMPT] " (17 characters)
  pub fn attempt_defects(metadata: &serde_json::Value) -> Vec<String>;  // empty when absent or valid
  pub fn is_failed_attempt(metadata: &serde_json::Value) -> bool;       // true only for a valid failed attempt
  ```

- [ ] **Step 1: Write the failing tests**

  In `src/attempt.rs`'s own test module:

  - `a_well_formed_failed_attempt_is_a_failed_attempt` — the four keys present and non-empty, `outcome == "failed"` → `is_failed_attempt` is true and `attempt_defects` is empty.
  - `a_malformed_attempt_is_neither_a_failed_attempt_nor_defect_free` — a missing key, an empty string, a non-string and an `outcome` other than `"failed"` each produce a defect and `is_failed_attempt` is false.
  - `an_absent_attempt_is_not_a_failed_attempt` — no `attempt` key → false, no defects.

  In `src/store/ladybug.rs`'s test module:

  - `a_malformed_attempt_warns_and_the_write_still_succeeds` — write a `fact` with `metadata.attempt` missing `context`; the write succeeds and a `WriteShapeWarning` is produced.

  In `src/server.rs`'s test module:

  - `a_failed_attempt_renders_the_attempt_line` — `format_memory` on a row with a valid attempt contains `Attempt [FAILED in {context}]: tried '{approach}' -> failed with '{failure_mode}' (single episode, not a global rule)`.
  - `a_malformed_attempt_renders_no_attempt_line` — the same row with a malformed attempt has no `Attempt [FAILED in` line.
  - `a_negative_exemplar_never_blocks_a_positive_fact` — the duplicate check's candidate set is partitioned: a candidate carrying a valid attempt is not handed to the judgment provider when the incoming row carries none, and vice versa.

  In `src/buffer.rs`'s test module:

  - `a_recalled_negative_exemplar_is_prefixed_and_bounded` — the recalled entry's `snippet` starts with `[FAILED ATTEMPT] ` and its total length is ≤ 120 characters.
  - `a_recalled_negative_exemplar_shorter_than_the_prefix_budget_keeps_the_prefix` — a row whose content is shorter than the prefix's own length still yields a snippet that starts with `[FAILED ATTEMPT] ` and is ≤ 120 characters; the prefix is never dropped to make room for the content.
  - `a_recalled_positive_fact_is_not_prefixed`.

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo test --bin neurostrata attempt` → Expected: FAIL — `src/attempt.rs` does not exist yet.

- [ ] **Step 3: Create `src/attempt.rs`**

  `attempt_defects` returns an empty list when `metadata.attempt` is absent (an absent attempt is not a defect) and a list of human-readable defects when it is present but malformed: not an object; a missing key; a key that is not a string; a key whose string is empty or whitespace; an `outcome` that is not exactly `"failed"`.

  `is_failed_attempt` is true only when `attempt` is present **and** `attempt_defects` is empty. One predicate, four callers — a row rendered as a failure must be partitioned as one too.

- [ ] **Step 4: Add the store's shape check**

  In `src/store/ladybug.rs`, beside the `topology` check (`:1066-1095`), add the `attempt` check for `fact` rows: when `payload.memory_type == "fact"` and the metadata carries an `attempt` with defects, push a `WriteShapeWarning` naming the defects. The write still succeeds — a claim is worth keeping even when its shape is imperfect.

  The declared keys constant sits beside `TOPOLOGY_KEYS` (`:957-966`): `pub const ATTEMPT_KEYS: [&str; 4] = ["approach", "outcome", "failure_mode", "context"];` — or import it from `crate::attempt` so the vocabulary and the code cannot drift. Prefer the import.

- [ ] **Step 5: Render the attempt line**

  In `src/server.rs`'s `format_memory` (`:2276`), immediately after the `Type:` line and before `Content:`, print the attempt line when `crate::attempt::is_failed_attempt(&payload.metadata)`:

  ```
  Attempt [FAILED in {context}]: tried '{approach}' -> failed with '{failure_mode}' (single episode, not a global rule)
  ```

  A row whose attempt is malformed renders no line — it is a plain row.

- [ ] **Step 6: Prefix the truncated snippets**

  In `src/buffer.rs` (`:275-285`), the snippet is built from `content.chars().take(120)`. Build it as: the prefix `"[FAILED ATTEMPT] "` when the row is a failed attempt, then the content truncated to `120 - prefix.chars().count()` characters. The prefix is part of the 120-character budget, not added on top of it, and it is applied **before** truncation so truncation can never strip it.

  The same rule governs the snapshot strap's snippets (Task 6): one helper, one budget.

- [ ] **Step 7: Partition the duplicate check's candidates**

  In `src/server.rs`'s `embed_dedup_and_upsert` (`:1138-1175`), the candidates scoring `>= 0.85` (`:1160`) are handed to `checker.check_duplicates(payload, &similar)`. Partition them by attempt-presence before the check: keep a candidate only when `crate::attempt::is_failed_attempt(&candidate.payload.metadata)` equals `crate::attempt::is_failed_attempt(&payload.metadata)`.

  A negative exemplar about subject X can then never block a positive `fact` about X, and two attempts about the same approach are still compared with each other. The partition uses the **same** predicate as the prefix.

- [ ] **Step 8: Add `mod attempt;` to `src/main.rs`**

  Alphabetically, first (before `mod buffer;`).

- [ ] **Step 9: Run the tests to verify they pass**

  Run: `cargo test --bin neurostrata attempt` → Expected: PASS. Run: `cargo test` → Expected: the whole suite passes.

- [ ] **Step 10: Check the build's warning count has not moved**

  Expected: 14 production warnings, 17 test-unit warnings.

- [ ] **Step 11: Re-derive the spec citations this task moved**

  `src/store/ladybug.rs`'s shape seam, `src/server.rs`'s `format_memory` and duplicate check, `src/buffer.rs`'s snippet and `src/main.rs` are cited in §2's table and in §3.1.3, §3.1.4, §4, §5 step 8, §6 criteria 1, 2, 4, 5, 6 and §12. Re-point them and mark the changed rows.

- [ ] **Step 12: Commit**

  ```bash
  git add src/attempt.rs src/main.rs src/server.rs src/store/ladybug.rs src/buffer.rs docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md
  git commit -m "feat(attempt): mark a failed attempt in the rendering, the snippet and the dedup partition"
  ```

---

### Task 6: The snapshot domain strap

One strap with two modes, computed off the list the snapshot already fetched, so it costs no extra I/O and never displaces the top-5.

**Files:**
- Modify: `src/server.rs:153-161` (the `get_snapshot` schema), `:1427-1499` (the handler)
- Test: `src/server.rs`

**Interfaces:**
- Consumes: `crate::registry::registry_domains(&serde_json::Value) -> Vec<String>` and `crate::registry::registry_row_id(&str) -> String` (Task 3); `crate::attempt::is_failed_attempt` and `crate::attempt::FAILED_ATTEMPT_PREFIX` (Task 5).
- Produces: nothing other tasks call — the strap is rendered into the snapshot's body.

- [ ] **Step 1: Write the failing tests**

  In `src/server.rs`'s test module:

  - `the_domain_strap_shows_a_row_the_access_count_cut_would_hide` — five memories at `access_count: 10` and one new `fact` at `access_count: 0` with a domain; the snapshot's top-5 holds the five, and the strap holds the new fact — both with and without an explicit `domain` argument.
  - `a_domain_tagged_row_already_in_the_top_five_is_not_duplicated` — the strap holds no id the top-5 holds.
  - `the_snapshot_does_not_mutate_any_access_count` — capture every row's `access_count` before and after the call; the two lists are equal.
  - `the_strap_is_bounded_and_names_its_mode` — four domain-tagged rows in one domain in `recent_domain` mode: the strap holds at most 3 entries and at most 2 from that one domain, and the body contains `Domain strap [mode: recent_domain]`.
  - `an_empty_strap_renders_an_explicit_zero` — the body contains `Domain strap [mode: explicit] (0): []`.
  - `the_snapshot_names_the_registered_domains` — the body contains `Registered domains (3): auth, database-access, storage`.
  - `a_namespace_with_no_registry_row_names_zero_domains` — the body contains `Registered domains (0): none`.

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo test --bin neurostrata strap` → Expected: FAIL — no strap is rendered.

- [ ] **Step 3: Add the `domain` property to the `get_snapshot` schema**

  One optional property, a string or an array of strings, describing the slugs to surface. Absent → `recent_domain` mode; present → `explicit` mode.

- [ ] **Step 4: Compute the top-5 ids, then the strap**

  The current handler sorts by `access_count` descending and then truncates to 5 (`:1476-1491`). Change it to take the top five ids first, then compute the strap off the **pre-truncation** list, then truncate:

  1. Collect the ids of the first five rows after the sort.
  2. Build the strap candidates from the whole list: exclude the top-5 ids; exclude structural types, `task` and `procedure` (the snapshot already has straps for those); require a non-empty normalized domain (`crate::domain::domain_key`).
  3. `explicit` mode → keep only the requested slugs (normalized with `crate::domain::normalize_domain_slug`); `recent_domain` mode → keep all.
  4. Sort by `valid_from` descending, then `access_count` descending, then id ascending.
  5. Take at most **3** entries; in `recent_domain` mode at most **2** from any one domain.

  `access_count` is never mutated: the strap reads the list the handler already has. The only two increments remain `src/server.rs:2050` and `:2706`.

- [ ] **Step 5: Render the two sections**

  Beside the top-5 body, in the same `format!("{}\n{}\n{}", …)` assembly (`:1493-1499`), render:

  - `Registered domains ({N}): {slug1}, {slug2}` — the registry row's domain list; `Registered domains (0): none` when there is no registry row. The count is the cache's, and the cache can be stale: it is the set of domains the agent should expect to be accepted, never a promise that the list is exhaustive.
  - `Domain strap [mode: {explicit|recent_domain}]` followed by the entries as a JSON array, each `{id, memory_type, domain, valid_from, access_count, snippet}`; when the selection is empty, `Domain strap [mode: {mode}] (0): []`.

  The snippet follows Task 5's rule: the `[FAILED ATTEMPT] ` prefix when the row is a failed attempt, and a total length ≤ 120 characters including the prefix.

- [ ] **Step 6: Run the tests to verify they pass**

  Run: `cargo test --bin neurostrata strap` → Expected: PASS. Run: `cargo test` → Expected: the whole suite passes.

- [ ] **Step 7: Check the build's warning count has not moved**

  Expected: 14 production warnings, 17 test-unit warnings.

- [ ] **Step 8: Re-derive the spec citations this task moved**

  `src/server.rs`'s snapshot handler and schema are cited in §2's table and in §3.3.1, §3.3.2, §3.3.3, §3.3.4, §4, §5 step 9, §6 criteria 9 and 14–17 and §12. Re-point them and mark the changed rows.

- [ ] **Step 9: Commit**

  ```bash
  git add src/server.rs docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md
  git commit -m "feat(snapshot): add the domain strap and the registered-domain header"
  ```

---

### Task 7: The counters and the metrics surface

The counters answer two questions the spec's open questions asked: does a domain filter pay, and is the `append_log` seat sufficient? Both are answered by measurement, not by deferral.

**Files:**
- Create: `src/recall_metrics.rs`
- Modify: `src/main.rs:1-21` (one `mod` line)
- Modify: `src/handlers/bus_metrics.rs:20-23` (the composite body)
- Modify: `src/server.rs:2027-2040` (the search tool's counters), `:2682-2710` (`handle_get_memory`'s promotion counter)
- Modify: `src/buffer.rs:252-297` (the recall counters and the recently-recalled id set)
- Test: `src/recall_metrics.rs`, `src/handlers/bus_metrics.rs`, `src/buffer.rs`, `src/server.rs`

**Interfaces:**
- Consumes: `crate::registry::registry_domain_count(&Arc<dyn VectorStore>, &str) -> usize` (Task 3).
- Produces (in `src/recall_metrics.rs`):

  ```rust
  pub fn domain_filter_snapshot() -> serde_json::Value;  // {domain_queries_total, domain_narrowed_hits, domain_fallback_vector, single_domain_bypasses}
  pub fn recall_snapshot() -> serde_json::Value;         // {recall_responses, recall_promotions}
  pub fn record_domain_query(narrowed: bool, fell_back: bool, single_domain: bool);
  pub fn record_recall(namespace: &str, ids: &[String]);
  pub fn record_promotion(namespace: &str, id: &str) -> bool;  // true when the id was a recently-recalled one
  ```

- [ ] **Step 1: Write the failing tests**

  In `src/recall_metrics.rs`'s own test module:

  - `the_counters_start_at_zero` — a fresh process reports four zeros and two zeros.
  - `a_recalled_id_is_counted_once_when_it_is_promoted` — record a recall of one id, promote it → `recall_promotions` is 1; promoting it again → still 1 (the id is removed on promotion).
  - `the_recalled_id_set_is_bounded` — record more than the bound and assert the set never exceeds it.

  In `src/handlers/bus_metrics.rs`'s test module:

  - `the_metrics_body_carries_the_bus_counters_and_the_new_sections` — the body still holds the bus's eight integers at the top level (the existing assertion) **and** now holds `domain_filter` with four integer keys and `recall` with two integer keys, each present even when the value is zero.

  In `src/server.rs`'s test module:

  - `a_domain_filtered_search_counts_the_filter` — a search with a `domain` argument increments `domain_queries_total`; a search without one increments nothing.
  - `a_search_in_a_single_domain_namespace_counts_a_bypass_not_a_narrowing_hit` — a namespace whose registry holds one domain: the call increments `domain_queries_total` and `single_domain_bypasses`, and increments `domain_narrowed_hits` only when the filter really matched.
  - `a_promotion_of_a_recalled_id_is_counted` — the promotion path records the promotion.

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo test --bin neurostrata metrics` → Expected: FAIL — `src/recall_metrics.rs` does not exist yet.

- [ ] **Step 3: Create `src/recall_metrics.rs`**

  Six process-global atomics: `domain_queries_total`, `domain_narrowed_hits`, `domain_fallback_vector`, `single_domain_bypasses`, `recall_responses`, `recall_promotions`.

  The join between a recall and its later promotion is a **bounded in-memory set of recently-recalled ids, per namespace** — the MCP tool surface carries no session id, so the join is per namespace, not per session. The set is evicted on promotion and when the bound is reached, so it never grows without limit and never outlives the daemon. A response whose ids age out before promotion counts as unpromoted, so the measured rate is a lower bound.

- [ ] **Step 4: Increment the domain counters at the two search sites**

  In `src/server.rs`'s search handler (`:2027`) and `src/buffer.rs`'s recall (`:252`), after the search returns:

  - A call with **no** `recalled_from` increments none of the four: it carried no filter and narrowed nothing, and a failed search is not a fallback because no unfiltered search ran in its place.
  - Otherwise increment `domain_queries_total`; increment `domain_narrowed_hits` when the mode is `Domain`; increment `domain_fallback_vector` when the mode is `Vector`; and increment `single_domain_bypasses` when the namespace's registry holds ≤ 1 domain (read it with `crate::registry::registry_domain_count` — one indexed read, and only on the path where a filter was actually requested).

- [ ] **Step 5: Record the recall and the promotion**

  In `src/buffer.rs`, after the recall response's ids are known, call `record_recall(namespace, &ids)` and increment `recall_responses`.

  In `src/server.rs`'s `handle_get_memory` (`:2682`), at the access-count increment (`:2706`), call `record_promotion(namespace, &id)`; when it returns true, increment `recall_promotions`.

- [ ] **Step 6: Make the metrics body a composite**

  In `src/handlers/bus_metrics.rs:20-23`: serialize `bus.metrics()` to a `serde_json::Value`, insert `"domain_filter"` and `"recall"` as **extra top-level keys**, and serialize the result. The bus's eight integers stay exactly where they are — every existing reader of that body keeps working, and the bus never learns what a domain is. The handler's signature does not change, no `AppState` field is added, and no route plumbing is needed.

- [ ] **Step 7: Add `mod recall_metrics;` to `src/main.rs`**

  Alphabetically, between `mod parser;` and `mod registry;`.

- [ ] **Step 8: Run the tests to verify they pass**

  Run: `cargo test --bin neurostrata metrics` → Expected: PASS. Run: `cargo test` → Expected: the whole suite passes.

- [ ] **Step 9: Check the build's warning count has not moved**

  Expected: 14 production warnings, 17 test-unit warnings.

- [ ] **Step 10: Re-derive the spec citations this task moved**

  `src/handlers/bus_metrics.rs`, `src/server.rs`, `src/buffer.rs` and `src/main.rs` are cited in §2's table and in §3.4.1, §3.4.2, §4, §5 step 10, §6 criterion 18, §8, §10 and §12. Re-point them and mark the changed rows.

- [ ] **Step 11: Commit**

  ```bash
  git add src/recall_metrics.rs src/main.rs src/handlers/bus_metrics.rs src/server.rs src/buffer.rs docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md
  git commit -m "feat(metrics): count the domain filter and the recall promotions on the bus surface"
  ```

---

### Task 8: The mesh import unions the imported record's domain

No shared record is ever refused for a domain the local registry has not seen.

**Files:**
- Modify: `src/synaptic_mesh/import.rs:1244-1259` (the apply site)
- Test: `src/synaptic_mesh/import.rs`

**Interfaces:**
- Consumes: `crate::domain::domain_key(&serde_json::Value) -> Option<String>` (`src/domain.rs:29`); `crate::registry::write_registry(&Arc<dyn VectorStore>, &Arc<dyn Embedder>, &str, &[String]) -> Option<String>` (Task 3).
- Produces: nothing new.

- [ ] **Step 1: Write the failing test**

  In `src/synaptic_mesh/import.rs`'s test module:

  - `an_imported_records_domain_is_unioned_into_the_local_registry` — import a shareable `fact` whose metadata carries `domain: "auth"`; the local namespace's registry row lists `auth` afterwards.

- [ ] **Step 2: Run the test to verify it fails**

  Run: `cargo test --bin neurostrata imported_records_domain` → Expected: FAIL — the registry does not learn the imported domain.

- [ ] **Step 3: Union the domain after the apply**

  In `apply_effective` (`:1244-1259`), after the row's `store.upsert` succeeds, read the applied payload's domain with `crate::domain::domain_key` and, when there is one, union it into the local registry with `crate::registry::write_registry`. A failure here is a warning, never a refusal: an imported row is already stored, and the registry is a cache.

- [ ] **Step 4: Run the tests to verify they pass**

  Run: `cargo test --bin neurostrata imported_records_domain` → Expected: PASS. Run: `cargo test` → Expected: the whole suite passes.

- [ ] **Step 5: Check the build's warning count has not moved**

  Expected: 14 production warnings, 17 test-unit warnings.

- [ ] **Step 6: Re-derive the spec citations this task moved**

  `src/synaptic_mesh/import.rs` is cited in §2's table and in §3.2.4, §5 step 11, §6 criteria 1 and 10 and §12. Re-point them and mark the changed rows.

- [ ] **Step 7: Commit**

  ```bash
  git add src/synaptic_mesh/import.rs docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md
  git commit -m "feat(mesh): union an imported record's domain into the local registry"
  ```

---

### Task 9: The docs, the CHANGELOG, the acceptance sweep

**Files:**
- Modify: `CHANGELOG.md`
- Modify: the repo's recall documentation (the honest-recall note and the registry note)
- Modify: `docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md` (the final anchor sweep)

**Interfaces:**
- Consumes: everything above.
- Produces: the shipped documentation.

- [ ] **Step 1: Write the honest-recall note**

  In the repo's recall documentation, add the note the spec calls for: a `fact` carrying `metadata.attempt` is **evidence of one episode, not a normative rule**; it is returned by a search, it is rendered with its `Attempt [FAILED in …]` line, and its truncated snippet carries the `[FAILED ATTEMPT] ` prefix. A rule is normative; a failed attempt is evidence.

- [ ] **Step 2: Write the registry note**

  Add the registry note: each namespace has one registry row; the first domain write seeds it from the namespace's own rows; a slug that neither the registry nor the namespace's data carries is refused with the registered list and the `register_domain: true` remedy; the registry is a cache, so it can be stale and the reconcile repairs it.

- [ ] **Step 3: Update the CHANGELOG**

  One entry naming: the vocabulary bump to 6 and the two declared fields; the domain registry and the rejection with its remedy; the `[FAILED ATTEMPT]` rendering, the snippet prefix and the duplicate partition; the snapshot domain strap and the `Registered domains (N)` header; the `domain_filter` and `recall` sections on the metrics surface.

- [ ] **Step 4: Run the acceptance sweep**

  Walk the spec's §6 (lines 645–694) criterion by criterion and name the test that proves each one. Every criterion must have a test. Any that does not gets one, added to the task that owns the code, in that task's style.

  | Criterion | Proved by |
  |---|---|
  | 1 | Task 5's round-trip test + Task 8's import test |
  | 2 | Task 5's `a_malformed_attempt_warns_and_the_write_still_succeeds` |
  | 3 | Task 2's vocabulary test |
  | 4 | Task 5's `a_failed_attempt_renders_the_attempt_line` + a search test that returns both rows |
  | 5 | Task 5's `a_recalled_negative_exemplar_is_prefixed_and_bounded` |
  | 6 | Task 5's `a_negative_exemplar_never_blocks_a_positive_fact` |
  | 7 | Task 3's `a_write_in_a_namespace_with_no_registry_row_bootstraps_the_registry` |
  | 8 | Task 3's rejection, `register_domain` and reconcile tests |
  | 9 | Task 4's two tests + Task 6's `the_snapshot_names_the_registered_domains` |
  | 10 | Task 8's test + the registry row's `eligibility: private` |
  | 11 | Task 3's ordering (the cache write happens after the decision and never converts one into the other) |
  | 12 | Task 3's reconcile test (a slug the namespace used under another spelling is accepted; a never-used slug is refused) |
  | 13 | Task 3's `a_non_string_domain_is_rejected` |
  | 14 | Task 6's `the_domain_strap_shows_a_row_the_access_count_cut_would_hide` |
  | 15 | Task 6's `a_domain_tagged_row_already_in_the_top_five_is_not_duplicated` |
  | 16 | Task 6's `the_snapshot_does_not_mutate_any_access_count` |
  | 17 | Task 6's `the_strap_is_bounded_and_names_its_mode` and `an_empty_strap_renders_an_explicit_zero` |
  | 18 | Task 7's `the_metrics_body_carries_the_bus_counters_and_the_new_sections` |
  | 19 | Task 7's `a_domain_filtered_search_counts_the_filter` (a search with no `domain` behaves exactly as today) |
  | 20 | The spec's §11 is closed and every entry is decided or a named task |

- [ ] **Step 5: Final anchor sweep**

  Re-read the spec's §2 table, §3, §5 and §12 and confirm every citation resolves to the code it names **in the working tree now** — after eight tasks of edits, some of Task 1's re-pointing has moved again. Any citation that no longer resolves is re-pointed, and any row whose claim the implementation changed carries its `*[changed by neurostrata-r30c: …]*` marker.

- [ ] **Step 6: Run the whole gate**

  Run: `cargo test` → Expected: the whole suite passes.
  Run: `cargo build 2>&1 | grep -c '^warning'` → Expected: 14. Run: `cargo test --no-run 2>&1 | grep -c '^warning'` → Expected: 17.

- [ ] **Step 7: Commit**

  ```bash
  git add CHANGELOG.md docs/ docs/superpowers/specs/2026-10-21-recall-closure-and-negative-exemplars-design.md
  git commit -m "docs: the honest-recall note, the registry note and the closure changelog"
  ```

- [ ] **Step 8: Complete the task (Lock 2)**

  Extract the memory this work proves — the registry-as-cache rule and the one-predicate rule — with `metadata.extracted_from: ["neurostrata-r30c"]` and `metadata.source` naming the commit that proved each. Then `neurostrata_task_complete` for `neurostrata-r30c`. Then `git pull --rebase` and `git push`.
