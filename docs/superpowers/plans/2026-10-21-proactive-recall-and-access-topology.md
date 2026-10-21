# Proactive Recall and Access Topology Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the parent spec's mechanism — a domain-filtered recall that rides the `neurostrata_append_log` response, a declared `fact.fields.topology` shape, and a normalized `metadata.domain` key.

**Architecture:** One new leaf module (`src/domain.rs`) owns slug normalization. The store gains a domain-filtered search behind a **new** `VectorStore` trait method with a default implementation, so exactly one implementation changes. The append_log handler widens to take the embedder and the store, embeds the normalized tags once, and returns up to three `recalled` entries plus the `recalled_from` mode. The vocabulary gains `fact.fields.topology` with one 4 → 5 bump.

**Tech Stack:** Rust 2021, tokio, serde_json, LadybugDB (`lbug`), `async_trait`. Tests are inline `#[cfg(test)] mod tests` blocks inside each source file; run them with `cargo test --bin neurostrata-mcp <filter>`.

**Spec:** `docs/superpowers/specs/2026-10-21-proactive-recall-and-access-topology-design.md` — read it before starting. The plan argues from the spec; where the two differ, the spec wins and the plan gets corrected.

**Task:** `neurostrata-426k` (this plan is its first half; the second half is the implementation, tracked by the same task).

---

## Global Constraints

Every task's requirements implicitly include this section.

- **One seat.** The recall rides the `neurostrata_append_log` response and nowhere else. No new MCP tool, no new snapshot lane, no `get_snapshot` strap — the parent spec §4.1.4 deferred those and they are closed in the recall-closure spec (`neurostrata-r30c`), not here.
- **`recalled` is always present** on the append_log response: an explicit empty array when there is nothing to recall. Silence about "nothing was recalled" is a lie the caller has to guess about (the repo's explicit-zero convention).
- **Bounds.** At most **3** entries; `snippet` at most **120 characters**; one embedding per tagged turn and none at all when there are no tags.
- **The recall never mutates `access_count`.** Only `neurostrata_search_memory` (`src/server.rs:2019`) and the get-memory handler (`src/server.rs:2652`) increment it. A wrong recall must not be able to promote itself into a future snapshot's top-5.
- **`recalled_from` is always present** and is exactly `"domain"` or `"vector"`. `"domain"` means the domain filter chose the rows; `"vector"` means it did not — no filter was asked for, no tags survived normalization, the filter found nothing and was skipped, or the search returned nothing.
- **Slug shape.** `domain` and the append_log `tags` normalize to lowercase-kebab tokens matching `^[a-z0-9]+(-[a-z0-9]+)*$`. Normalization happens **on write** and **on comparison** — the comparison normalizes the candidate row's stored value, which is what makes this migration-free.
- **The domain filter is soft.** If it would empty the candidate set, it is skipped and the unfiltered candidates are returned. A wrong domain must never blind recall. A silent substitution is forbidden, so the mode is always reported.
- **`metadata.topology` is soft.** A malformed value raises a `WriteShapeWarning` (`src/store/ladybug.rs:413-418`) and the write still succeeds. A rejected write would lose the claim.
- **One vocabulary bump, here: 4 → 5**, carrying `fact.fields.topology`. The single assert is at `src/traits.rs:263`. The next bump (5 → 6, carrying `fact.fields.attempt` and `fact.fields.domain_registry`) belongs to the recall-closure spec (`neurostrata-r30c`) — not to this plan.
- **No storage migration.** `metadata` is a JSON blob; normalization happens at comparison time, so legacy free-text domains keep working without a rewrite or a grandfathering pass.
- **`VectorStore::search` keeps its signature.** The filter arrives as a new `search_with_domain` method with a default implementation; the `include_tasks` / `include_archived` flags are the precedent for adding a parameter to a search.
- **No consumer-system facts and no consumer vocabulary in the `NeuroStrata` namespace.** This plan ships generic machinery only.
- **One binary.** `src/main.rs` is the crate root (`mod` list at `src/main.rs:1-20`), so a new module is registered there and nowhere else.
- **Baseline.** 620 tests pass and the build emits 14 warnings before this plan starts. A task that adds a failing test, breaks a passing one, or adds a 15th build warning does not pass.
- **The supersede path needs no change.** It clones the old payload and replaces only `governs`, `related_to` and `contained_by` (`src/server.rs:1690`, `:1733`), so a stored `domain` is carried over, not rewritten. Correcting a wrong domain means a new write, not a supersede.
- **Never invent a source fact.** Cite `file:line`. If the code disagrees with this plan, the code wins: fix the plan in the same commit that fixes the code.

---

## Review Focus

The five input classes the spec implies but no acceptance criterion names. Each one gets a test, pinned to the task that owns the code.

1. **A tag that normalizes to nothing** (`"!!!"`, `"   "`). The recall must not build a query from it and must not filter on it; if every tag normalizes away, the turn recalls nothing and says so (`recalled: []`) instead of embedding a garbage query. → Task 6.
2. **A domain filter that matches nothing while the namespace has rows.** The filter must be skipped, the unfiltered rows returned, and the mode reported as `"vector"`. A wrong domain must never blind the recall. → Task 2.
3. **A legacy row whose stored domain is free text** (`"Database Access"`). It must match the slug `database-access` at comparison time, with no migration and with the stored value left exactly as it was. → Task 2.
4. **A `metadata.domain` that is not a string** (a number, an object). The write must not crash and must not guess: the value is stored as given, the comparison ignores it, and rejecting it is the recall-closure spec's write-path check, not this plan's. → Task 1.
5. **An untagged turn.** No embedding call, no search, `recalled: []`, `recalled_from: "vector"` — and the response must still have the same JSON shape as every other append_log response. → Task 6.

---

### Task 1: The domain slug normalizer, and the write path's use of it

**Files:**
- Create: `src/domain.rs`
- Modify: `src/main.rs:1-20` (add `mod domain;` between `mod daemon;` and `mod embed;`)
- Modify: `src/server.rs:1279-1281` (the add path's domain write)

**Interfaces:**
- Consumes: nothing. This is the first task.
- Produces:
  ```rust
  // src/domain.rs
  /// Normalize a raw domain or tag into a lowercase-kebab slug token.
  /// Returns None when nothing usable remains.
  pub fn normalize_domain_slug(raw: &str) -> Option<String>

  /// The normalized slug of a metadata object's `domain` field.
  /// Returns None when the field is absent, is not a string, or does not normalize.
  pub fn domain_key(metadata: &serde_json::Value) -> Option<String>
  ```
  Task 2 uses `domain_key` to compare a candidate row's domain; Task 6 uses `normalize_domain_slug` to normalize the append_log tags. Both live in `crate::domain`.

- [ ] **Step 1: Write the failing tests**

In `src/domain.rs`, add an inline `#[cfg(test)] mod tests` with:

```rust
#[test]
fn a_domain_normalizes_to_a_lowercase_kebab_slug() {
    assert_eq!(normalize_domain_slug("Database Access").as_deref(), Some("database-access"));
    assert_eq!(normalize_domain_slug("database_access").as_deref(), Some("database-access"));
    assert_eq!(normalize_domain_slug("database-access").as_deref(), Some("database-access"));
    assert_eq!(normalize_domain_slug("  Auth  ").as_deref(), Some("auth"));
    assert_eq!(normalize_domain_slug("eu-west-1 VPC").as_deref(), Some("eu-west-1-vpc"));
    assert_eq!(normalize_domain_slug("mTLS").as_deref(), Some("mtls"));
}

#[test]
fn a_domain_that_normalizes_to_nothing_is_none() {
    assert_eq!(normalize_domain_slug("!!!").as_deref(), None);
    assert_eq!(normalize_domain_slug("   ").as_deref(), None);
    assert_eq!(normalize_domain_slug("").as_deref(), None);
}

#[test]
fn a_non_string_domain_is_never_guessed() {
    assert_eq!(domain_key(&serde_json::json!({})), None);
    assert_eq!(domain_key(&serde_json::json!({"domain": 42})), None);
    assert_eq!(domain_key(&serde_json::json!({"domain": {"slug": "auth"}})), None);
    assert_eq!(domain_key(&serde_json::json!({"domain": "Database Access"})).as_deref(), Some("database-access"));
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --bin neurostrata-mcp domain::tests`
Expected: FAIL — the module does not exist yet (`could not find domain`).

- [ ] **Step 3: Implement `src/domain.rs`**

The algorithm, in order: lowercase the input; replace every maximal run of characters outside `[a-z0-9]` with a single `-`; trim leading and trailing `-`; if what remains is empty, return `None`, otherwise return the slug. `domain_key` reads `metadata["domain"]` as a string and passes it to `normalize_domain_slug`; anything that is not a string is `None`.

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test --bin neurostrata-mcp domain::tests`
Expected: PASS — 3 tests.

- [ ] **Step 5: Normalize the domain on write**

At `src/server.rs:1279-1281`, the add path inserts the `domain` argument into the metadata verbatim. After that insert, when the inserted value is a string, replace it with `normalize_domain_slug(value)`; when that returns `None`, leave the stored value exactly as it was given. A non-string `domain` is stored as given and is never coerced, converted or dropped — Review Focus #4.

- [ ] **Step 6: Add a test for the write path's behavior**

In `src/server.rs`'s inline test module, add a test that calls the add path with `domain: "Database Access"` and asserts the stored metadata's `domain` is `"database-access"`, and a second assertion with `domain: 42` that the stored value is still the number `42`. Use the same handler the existing add-path tests use.

- [ ] **Step 7: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 620 + the new tests, 0 failed.

- [ ] **Step 8: Commit**

```bash
git add src/domain.rs src/main.rs src/server.rs
git commit -m "feat(domain): normalize domain slugs on write and on comparison"
```

---

### Task 2: The domain-filtered search in the store

**Files:**
- Modify: `src/traits.rs` (the `VectorStore` trait, after `search` at `:165-172`; the new types near the other trait types)
- Modify: `src/store/ladybug.rs` (the `LadybugStore` search, `:1004-1135` and the expansion block that follows it)

**Interfaces:**
- Consumes: `crate::domain::domain_key(&serde_json::Value) -> Option<String>` (Task 1).
- Produces:
  ```rust
  // src/traits.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum DomainMatchMode {
      /// The rows were chosen by the domain filter.
      Domain,
      /// The rows were chosen without the domain filter: no key set was given,
      /// the filter found nothing and was skipped, or nothing was returned.
      Vector,
  }

  #[derive(Debug, Clone)]
  pub struct DomainSearchOutcome {
      pub results: Vec<SearchResult>,
      pub mode: DomainMatchMode,
  }

  // in `trait VectorStore`
  /// Search with an optional domain key set. The default implementation ignores
  /// the key set and reports `Vector`, so a store that cannot filter stays honest.
  async fn search_with_domain(
      &self,
      namespace: &str,
      vector: Vec<f32>,
      limit: usize,
      include_tasks: bool,
      include_archived: bool,
      domains: &[String],
  ) -> Result<DomainSearchOutcome>
  ```
  Task 3 and Task 6 call `search_with_domain`; both read `outcome.mode` to report how the rows were chosen.

- [ ] **Step 1: Write the failing tests**

In `src/store/ladybug.rs`'s inline test module, add three `#[tokio::test]`s. Use `LadybugStore::for_testing(path, 4)` (`:143`) and `store.init(namespace)`, exactly as the existing store tests do; the vector dimension is 4, so upsert and search with a 4-float vector.

```rust
#[tokio::test]
async fn a_domain_filter_narrows_the_candidates_to_the_matching_domain() { /* upsert two facts with
    different metadata.domain values, search with a one-element key set, assert only the
    matching fact comes back and outcome.mode == DomainMatchMode::Domain */ }

#[tokio::test]
async fn a_domain_filter_that_would_empty_the_candidates_falls_back_to_the_unfiltered_rows() { /* one
    fact with domain "auth", search with the key set ["database"], assert the fact IS returned and
    outcome.mode == DomainMatchMode::Vector */ }

#[tokio::test]
async fn a_legacy_free_text_domain_matches_the_slug_at_comparison_time() { /* one fact stored with
    metadata.domain "Database Access", search with the key set ["database-access"], assert the fact
    is returned with mode Domain, and assert the stored metadata value is still "Database Access" */ }
```

The third test is Review Focus #3; the second is Review Focus #2.

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --bin neurostrata-mcp store::ladybug::tests::a_domain`
Expected: FAIL — `search_with_domain` does not exist.

- [ ] **Step 3: Add the trait types and the default method**

Add `DomainMatchMode` and `DomainSearchOutcome` to `src/traits.rs`, and add `search_with_domain` to `trait VectorStore` with a body that calls `self.search(namespace, vector, limit, include_tasks, include_archived).await?` and returns `DomainSearchOutcome { results, mode: DomainMatchMode::Vector }`.

The default body is what keeps the blast radius at one implementation: `StoreNotAttached` (`src/events/bus.rs:55`), `RecordingStore` (`src/events/bus.rs:831`, `src/daemon.rs:1283`), `MockStore` (`src/guard/testkit.rs:30`) and `ListHidingStore` (`src/task/mod.rs:4305`) need no change. A store that cannot filter must say so with `Vector`, never pretend the filter applied.

- [ ] **Step 4: Run the trait tests**

Run: `cargo test --bin neurostrata-mcp traits::tests`
Expected: PASS — the existing trait tests still pass; the new method is additive.

- [ ] **Step 5: Override `search_with_domain` in `LadybugStore`**

`LadybugStore::search` (`src/store/ladybug.rs:1004`) does two things inside one `with_conn` closure: the base candidate loop (`:1046-1135`) and the graph expansion (`:1142` onward). The filter must sit inside the base candidate loop, so restructure that method as follows, all inside the same closure:

- The base loop keeps **two** lists: `all_primaries` (every row the loop accepts, exactly as today) and `filtered_primaries` (the rows whose `domain_key` is in the key set). The key set is an owned `HashSet<String>` built from the `domains` argument before the closure is entered — `with_conn` requires `FnOnce(&Connection) -> Result<T> + Send + 'static` (`:388-392`), so a borrowed slice cannot cross into it.
- When the key set is empty, every row goes to both lists and the mode is `Vector` — no filter was asked for, so the behaviour is byte-identical to today's `search`.
- When the key set is non-empty, a row whose `domain_key` is `None` or is not in the set is skipped for `filtered_primaries` but still counted in `all_primaries`. The filter check happens **after** the row-class filters (`task`, tombstoned, mesh-conflicting) and **before** the usage boost, so the boost and the budget are spent only on rows the filter admitted.
- The loop's stop condition counts `filtered_primaries`, so a filtered search may legitimately return fewer rows than `limit`.
- After the loop: if the key set was non-empty, `filtered_primaries` is empty and `all_primaries` is not, the filter is skipped — the chosen set is the first `limit` entries of `all_primaries` and the mode is `Vector`. Otherwise the chosen set is `filtered_primaries` and the mode is `Domain`.
- The graph expansion then runs on the chosen set, unchanged. The expansion is **not** domain-filtered: a topology fact's `GOVERNS` neighbour is the context that made the fact worth recalling, and filtering it away would break the one-hop path the parent spec §4.2.3 depends on.

`search` keeps its current signature and current behaviour; implement it by calling the same inner routine with an empty key set.

- [ ] **Step 6: Make the mock store domain-aware**

`MockStore` (`src/guard/testkit.rs:30`) needs a `search_with_domain` override so that callers can be tested without a database: it filters its rows by `crate::domain::domain_key(&row.payload.metadata)` against the key set, and when the filter would empty the set it returns the unfiltered rows with `DomainMatchMode::Vector` — the same soft fallback the real store has. A test double that behaves differently from the store it stands in for is a trap.

- [ ] **Step 7: Run the store tests**

Run: `cargo test --bin neurostrata-mcp store::ladybug::tests`
Expected: PASS — the three new tests and every existing store test.

- [ ] **Step 8: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed.

- [ ] **Step 9: Commit**

```bash
git add src/traits.rs src/store/ladybug.rs src/guard/testkit.rs
git commit -m "feat(store): filter search by normalized domain, with a soft fallback"
```

---

### Task 3: The `domain` argument on `neurostrata_search_memory`

**Files:**
- Modify: `src/server.rs:184-192` (the tool's `inputSchema`)
- Modify: `src/server.rs:1956-2035` (`handle_search_memory`)

**Interfaces:**
- Consumes: `VectorStore::search_with_domain(...) -> Result<DomainSearchOutcome>` (Task 2).
- Produces: nothing new — `handle_search_memory` keeps its signature. It gains an optional `domain` argument in the tool schema.

- [ ] **Step 1: Write the failing tests**

In `src/server.rs`'s inline test module, add two `#[tokio::test]`s. Build the store with `crate::guard::testkit::MockStore` (a `pub(crate)` module, `src/guard/mod.rs:13`) and the embedder with the `StubEmbedder` already in that test module (`src/server.rs:2972`); seed the store with two facts carrying different `metadata.domain` values.

```rust
#[tokio::test]
async fn search_memory_without_a_domain_behaves_exactly_as_today() { /* no domain argument; assert the
    rendered report is the same shape as a search with no domain, and that no mode line was added */ }

#[tokio::test]
async fn search_memory_with_a_domain_filters_the_results_and_reports_the_mode() { /* a domain argument;
    assert the non-matching rows are gone and the report carries the mode line */ }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --bin neurostrata-mcp server::tests::search_memory`
Expected: FAIL — the domain argument is ignored.

- [ ] **Step 3: Accept the argument**

Add to the `neurostrata_search_memory` `inputSchema` properties:

```json
"domain": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Optional domain filter. A slug, or a list of slugs. Normalized to lowercase-kebab. A filter that matches nothing is skipped rather than emptying the results, and the report says which happened." }
```

`namespace` and `query` stay required; `domain` is optional.

- [ ] **Step 4: Apply the filter**

In `handle_search_memory`, read `arguments["domain"]` as either a string or an array of strings, normalize each with `crate::domain::normalize_domain_slug`, and drop the values that do not normalize. An absent or empty `domain` yields an empty key set, which is the "no filter" case.

Replace the call at `src/server.rs:1993-2003` with `store.search_with_domain(namespace, vec, DEFAULT_LIMIT, include_tasks, include_archived, &key_set)`, and use `outcome.results` in place of `results`. Everything else in the handler — the `access_count` increment for direct hits (`:2019`), the "No relevant memories found." branch, the rendered report — stays as it is.

- [ ] **Step 5: Report the mode**

When a domain was requested, prepend one line to the rendered report — the repo's "one flattened line" style:

- filter applied → `domain filter: applied (domain)`
- filter skipped → `domain filter: skipped — no row matched the domain; the results are unfiltered (vector)`

When no domain was requested, the report is byte-identical to today's. This is an orchestrator decision, not a spec line: the spec requires the mode only on the recall response, but the same silent-substitution rule applies here, and the spec's §4.3.5 gives the reason. It is additive — the line appears only when a caller asked for a domain.

- [ ] **Step 6: Run the tests**

Run: `cargo test --bin neurostrata-mcp server::tests::search_memory`
Expected: PASS.

- [ ] **Step 7: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed.

- [ ] **Step 8: Commit**

```bash
git add src/server.rs
git commit -m "feat(search): an optional domain filter on neurostrata_search_memory"
```

---

### Task 4: The vocabulary bump — `fact.fields.topology`, version 4 → 5

**Files:**
- Modify: `src/schemas/memory-vocabulary.v3.json:2` (the version), `:66-69` (the `fact` type), `:158-161` (the two `tool_summaries` keys)
- Modify: `src/traits.rs:261-281` (the vocabulary test and its assert)

**Interfaces:**
- Consumes: nothing.
- Produces: the declared `fact.fields.topology` shape — `{component, target_system, channel, credential_boundary, reason}`, all five string-valued. Task 5's shape check enforces it; Task 7's round-trip tests rely on it.

- [ ] **Step 1: Write the failing test**

In `src/traits.rs`'s inline test module, rename `memory_vocabulary_v4_parses_and_has_exact_relations` to `memory_vocabulary_v5_parses_and_has_exact_relations`, change its version assert from 4 to 5, and add:

```rust
#[test]
fn memory_vocabulary_declares_the_topology_field_on_a_fact() {
    // parse the vocabulary; assert memory_types.fact.fields.topology exists and its definition
    // names all five keys: component, target_system, channel, credential_boundary, reason
}
```

Use the same vocabulary-loading helper the neighbouring vocabulary tests use (`src/traits.rs:252` onward).

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test --bin neurostrata-mcp traits::tests::memory_vocabulary`
Expected: FAIL — the version is still 4 and `fact` has no `fields`.

- [ ] **Step 3: Declare the field**

In `src/schemas/memory-vocabulary.v3.json`:

- `"vocabulary_version": 4` → `5`.
- `memory_types.fact` gains a `fields` object with one key, `topology`:

```json
"topology": {
  "type": "object",
  "definition": "The access-path shape of the fact. All five keys are required and each is a string: component (where the path starts), target_system (what it reaches), channel (the transport or protocol), credential_boundary (the network, identity or trust boundary the channel crosses), reason (why the path is constrained). A missing or malformed value warns; the fact is still written."
}
```

This mirrors the `rule.fields` precedent (`:39-64`): a flat `{type, definition}` entry, no nested schema — the vocabulary file has no `properties`/`required` convention, and the machine-checked shape lives in the code (Task 5), not in prose.

- `tool_summaries.add` and `tool_summaries.memory_type` (`:158-161`) each gain a mention of `metadata.topology`, so the agent discovers the shape rather than guessing it.

- [ ] **Step 4: Move the assert**

`src/traits.rs:263` asserts the vocabulary version — change it to 5.

- [ ] **Step 5: Run the tests**

Run: `cargo test --bin neurostrata-mcp traits::tests`
Expected: PASS.

- [ ] **Step 6: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed. The vocabulary's `relations` set is unchanged: `topology` is a field, not a relation, and the relation-count assertion in the renamed test must still hold.

- [ ] **Step 7: Commit**

```bash
git add src/schemas/memory-vocabulary.v3.json src/traits.rs
git commit -m "feat(vocabulary): declare fact.fields.topology and bump the version to 5"
```

---

### Task 5: The topology shape check — a soft warning, never a rejection

**Files:**
- Modify: `src/store/ladybug.rs:424-521` (`parse_edge_specs`; the `WriteShapeWarning` struct is at `:413-418`)

**Interfaces:**
- Consumes: the declared shape from Task 4 (`component`, `target_system`, `channel`, `credential_boundary`, `reason`).
- Produces: nothing new — `parse_edge_specs` keeps its signature and returns `(Vec<EdgeSpec>, Vec<WriteShapeWarning>)`.

- [ ] **Step 1: Write the failing tests**

In `src/store/ladybug.rs`'s inline test module, add:

```rust
#[test]
fn a_well_formed_topology_writes_without_a_warning() { /* a payload whose metadata.topology carries
    all five keys as strings; assert parse_edge_specs returns no warning mentioning topology */ }

#[test]
fn a_malformed_topology_warns_and_the_fact_is_still_written() { /* a payload whose metadata.topology is
    missing "channel", and one whose metadata.topology is a string; assert each yields a
    WriteShapeWarning whose field is "metadata.topology" */ }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --bin neurostrata-mcp store::ladybug::tests::topology`
Expected: FAIL — no warning is produced today.

- [ ] **Step 3: Add the check**

In `parse_edge_specs`, add a section (E) after the existing relation-key checks:

- `metadata.topology` absent → no warning.
- Present and an object carrying all five keys, each a string → no warning.
- Present and anything else → one `WriteShapeWarning { field: "metadata.topology", why: <what is wrong>, skipped: "the topology shape check; the fact is still written" }`.

The check never affects the returned edge specs and never rejects the write. `parse_edge_specs` returns warnings; the caller emits them (`src/store/ladybug.rs:907`) and the upsert continues.

- [ ] **Step 4: Run the tests**

Run: `cargo test --bin neurostrata-mcp store::ladybug::tests::topology`
Expected: PASS.

- [ ] **Step 5: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed.

- [ ] **Step 6: Commit**

```bash
git add src/store/ladybug.rs
git commit -m "feat(store): warn on a malformed metadata.topology without rejecting the write"
```

---

### Task 6: The recall on the `append_log` response

**Files:**
- Modify: `src/buffer.rs:129-181` (`handle_append_log`)
- Modify: `src/server.rs:371-381` (the `neurostrata_append_log` `inputSchema`)
- Modify: `src/server.rs:664` (the dispatch)

**Interfaces:**
- Consumes: `crate::domain::normalize_domain_slug` (Task 1); `VectorStore::search_with_domain` and `DomainMatchMode` (Task 2); `crate::server::resolve_namespace(&Arc<dyn VectorStore>, &str) -> String` (`src/server.rs:2051`).
- Produces:
  ```rust
  // src/buffer.rs
  pub async fn handle_append_log(
      arguments: Value,
      emb: Arc<dyn Embedder>,
      store: Arc<dyn VectorStore>,
  ) -> String
  ```
  The response gains two fields: `recalled` (always present, 0–3 entries of `{id, memory_type, domain, score, snippet}`) and `recalled_from` (`"domain"` or `"vector"`, always present).

- [ ] **Step 1: Write the failing tests**

In `src/buffer.rs`'s inline test module, add:

```rust
#[tokio::test]
async fn a_tagged_turn_recalls_the_matching_fact() { /* a store holding a fact with
    metadata.domain "database-access"; call handle_append_log with tags ["database-access"] and assert
    the response's `recalled` holds that fact's id and `recalled_from` is "domain" */ }

#[tokio::test]
async fn a_fact_with_no_access_count_is_still_recalled() { /* the recalled fact's metadata carries
    access_count 0; assert it is still in `recalled` */ }

#[tokio::test]
async fn the_recall_never_moves_an_access_count() { /* assert the store's stored access_count for the
    recalled fact is unchanged before and after the call */ }

#[tokio::test]
async fn an_untagged_turn_recalls_nothing_and_costs_no_embedding() { /* no tags; assert `recalled` is
    [] and `recalled_from` is "vector" */ }

#[tokio::test]
async fn a_tag_that_normalizes_to_nothing_is_dropped() { /* tags ["!!!", "  "]; assert no search ran
    and `recalled` is [] */ }
```

Use `crate::guard::testkit::{MockStore, StubEmbedder}` (a `pub(crate)` module, `src/guard/mod.rs:13`) for the store and the embedder, and the same config/buffer setup the existing buffer tests use.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --bin neurostrata-mcp buffer::tests`
Expected: FAIL — `handle_append_log` takes one argument and returns no `recalled`.

- [ ] **Step 3: Widen the handler and the dispatch**

Change `handle_append_log` to take `emb: Arc<dyn Embedder>` and `store: Arc<dyn VectorStore>` after `arguments`. At `src/server.rs:664`, call `crate::buffer::handle_append_log(arguments, emb.clone(), store.clone()).await` — exactly as the neighbouring dispatches do (`task_setup` at `:655-661`, `guard_validate` at `:667-673`).

- [ ] **Step 4: Add the optional `namespace` to the input schema**

In the `neurostrata_append_log` `inputSchema` properties, add:

```json
"namespace": { "type": "string", "description": "The exact project name. Optional: defaults to the project root's directory name, resolved against the known namespaces." }
```

It stays optional; `content` and `project_root` remain required.

- [ ] **Step 5: Resolve the namespace**

The handler needs a namespace to search. Read `arguments["namespace"]`; when it is absent, use `project_root.file_name()`. Pass the result through `crate::server::resolve_namespace(&store, candidate)` (it is `pub(crate)`, `src/server.rs:2051`), which maps a path-like or unknown name onto the real namespace.

- [ ] **Step 6: Build the query and the key set**

Normalize each tag with `crate::domain::normalize_domain_slug`, dropping the ones that do not normalize. The surviving slugs are both the query text (joined with a single space) and the domain filter's key set — one pass, one meaning and one key.

- [ ] **Step 7: Run the recall**

- No surviving slug → no embedding, no search: `recalled: []`, `recalled_from: "vector"`.
- Otherwise → embed the joined query **once** (`emb.embed(&query)`), then `store.search_with_domain(&namespace, vec, 3, false, false, &key_set)`. `include_tasks` and `include_archived` are both false: task rows are backlog chatter and archived rows are tombstones, exactly as `search_memory` treats them by default.
- The entries are the first three results the store returns — the store returns primaries before expansions, so these are the direct matches whenever three primaries exist.
- Each entry is `{id, memory_type, domain, score, snippet}`: `domain` is the row's normalized slug or `null` when it has none; `score` is the result's score; `snippet` is the content truncated to 120 characters on a character boundary (`content.chars().take(120).collect()`).
- `recalled_from` is `"domain"` when the outcome's mode is `DomainMatchMode::Domain`, otherwise `"vector"`.
- A store error must not fail the append: the log entry is the handler's primary duty, so the error is logged with `tracing::warn!` and the response carries `recalled: []` and `recalled_from: "vector"`.

- [ ] **Step 8: Add the fields to the response**

The response object at `src/buffer.rs:171-177` gains `recalled` and `recalled_from`. Both are always present, on every response — the empty array is the explicit zero.

- [ ] **Step 9: Run the tests**

Run: `cargo test --bin neurostrata-mcp buffer::tests`
Expected: PASS — 5 new tests plus the existing buffer tests.

- [ ] **Step 10: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed.

- [ ] **Step 11: Commit**

```bash
git add src/buffer.rs src/server.rs
git commit -m "feat(recall): ride a domain-filtered recall on the append_log response"
```

---

### Task 7: The cross-cutting acceptance sweep

**Files:**
- Modify: `src/store/ladybug.rs` (the graph test)
- Modify: `src/synaptic_mesh/` (the mesh round-trip test — put it beside the mesh's existing import tests)

**Interfaces:**
- Consumes: everything Task 1–6 produce.
- Produces: nothing.

- [ ] **Step 1: Write the failing test — the topology fact reaches its governed file**

In `src/store/ladybug.rs`'s inline test module: upsert a `file` node first (so the edge resolves on the fact's write), then upsert a `fact` node whose metadata carries `governs: [<the file node's id>]` and a well-formed `metadata.topology` (all five keys, string-valued). Assert three things: the fact's `metadata.topology` round-trips unchanged through a search for it; the fact reaches the `file` node through the graph — the `GOVERNS` edge is what the one-hop expansion traverses (`src/store/ladybug.rs:1142`) — with the expansion row carrying `EvidenceKind::OneHop`; and the file node is not a task row, so it survives the row-class filter.

- [ ] **Step 2: Write the failing test — the mesh round-trip**

Export a `fact` carrying `metadata.topology` through the mesh's record path and import it back; assert the imported memory's `memory_type` is still `fact` and its `metadata.topology` is intact. The mesh is closed over `SHAREABLE_MEMORY_TYPES` (`src/synaptic_mesh/import.rs:61`) and rejects an unknown `memory_type` at import (`:649-655`) — a `fact` with a new *field* crosses the mesh unchanged, which is what this test proves. Put the test beside the mesh's existing import tests, and reuse their harness.

- [ ] **Step 3: Run them**

Run: `cargo test --bin neurostrata-mcp topology`
Expected: FAIL until the pieces line up; then PASS. If a test fails for a reason the spec does not cover, fix the code, not the assertion.

- [ ] **Step 4: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed.

- [ ] **Step 5: Commit**

```bash
git add src/store/ladybug.rs src/synaptic_mesh/
git commit -m "test: the topology fact reaches its governed file and survives a mesh round-trip"
```

---

### Task 8: The rule-honesty conformance test — gated on `neurostrata-bzdn`

**Files:**
- Modify: `src/task/mod.rs` (the task validator's tests)

**Interfaces:**
- Consumes: the tightened `rule-honesty` contract from `neurostrata-bzdn`.
- Produces: nothing.

**This task is gated.** The parent's spec §4.4 makes the rule-honesty hardening land first and separately, because the pre-push gate runs namespace-wide: tightening the validator flags the 86 legacy NeuroStrata rule rows until they are backfilled. That work is task `neurostrata-bzdn` (P2, ready, unassigned) and it is **not** this plan's to fold in — a tightening that lands without its backfill would fail the gate.

Tasks 1–7 do not depend on this task. This task depends on `neurostrata-bzdn` landing. If `neurostrata-bzdn` has not landed when you reach this task, it waits — the blocker is named, tracked and owned; it is not dropped.

- [ ] **Step 1: Confirm the gate**

Run: `neurostrata-mcp task validate NeuroStrata`
Expected: the tightened rule-honesty contract is enforced — a rule missing `enforcement`, or carrying a value outside `{ENFORCED, PARTIAL, NOT_ENFORCED}`, is reported. If it is not, `neurostrata-bzdn` has not landed: stop here, leave the task blocked on it, and say so.

- [ ] **Step 2: Write the test**

In `src/task/mod.rs`'s inline test module: a `rule` carrying `metadata.source`, `metadata.enforcement: "PARTIAL"` and a `metadata.guard` naming a wire id that exists in `known_wire_ids()` passes `task_validate`. Add the negative halves the contract names: `ENFORCED` with a null or dangling guard fails; `PARTIAL` with a null guard fails; `NOT_ENFORCED` with a non-null guard fails.

- [ ] **Step 3: Run it**

Run: `cargo test --bin neurostrata-mcp task::tests`
Expected: PASS.

- [ ] **Step 4: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed.

- [ ] **Step 5: Commit**

```bash
git add src/task/mod.rs
git commit -m "test: a rule written under the tightened contract passes task_validate"
```

---

### Task 9: The honest-recall note and the CHANGELOG entry

**Files:**
- Create: `docs/recall.md`
- Modify: `CHANGELOG.md` (the `## [Unreleased]` section)

**Interfaces:**
- Consumes: everything Task 1–8 produce.
- Produces: nothing.

- [ ] **Step 1: Write `docs/recall.md`**

The note answers the questions a consumer of the recall will have, in prose: what `recalled` and `recalled_from` mean; that `recalled` is always present and empty means nothing matched; that the filter is soft and the mode tells you whether it applied; that the recall never changes `access_count`, so a recalled memory is not a promoted one; that the entries are pointers and the promotion is an explicit `neurostrata_get_memory` call; that the recall is additive and can never suppress anything.

- [ ] **Step 2: Write the CHANGELOG entry**

Under `## [Unreleased]`, add to `### Added` — in the file's existing voice, which explains the failure the change closes rather than listing the change:

- the recall that rides the append_log response, and the `access_count: 0` hole it closes;
- the `domain` filter on search, its soft fallback, and the mode that reports it;
- the declared `fact.fields.topology` shape and the soft warning that keeps a malformed claim writable.

- [ ] **Step 3: Check the docs build and the suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed. (The docs are prose; the suite is the check that nothing else moved.)

- [ ] **Step 4: Commit**

```bash
git add docs/recall.md CHANGELOG.md
git commit -m "docs: the honest-recall note and the changelog entry"
```

---

## Done means

- `cargo test --bin neurostrata-mcp` reports 0 failed, and the count is the baseline plus the new tests.
- `cargo build --bin neurostrata-mcp` reports no new warning.
- The vocabulary is at version 5, the assert at `src/traits.rs:263` says 5, and `fact.fields.topology` is declared and surfaced in both `tool_summaries` keys.
- `neurostrata_append_log` returns `recalled` and `recalled_from` on every response, with at most three entries and a 120-character snippet cap.
- `neurostrata_search_memory` behaves exactly as before when no `domain` argument is given.
- The plan's nine tasks are committed, and the CHANGELOG and `docs/recall.md` describe what shipped.
- The vocabulary bump belongs to this plan; the next bump (5 → 6) belongs to `neurostrata-r30c`.
