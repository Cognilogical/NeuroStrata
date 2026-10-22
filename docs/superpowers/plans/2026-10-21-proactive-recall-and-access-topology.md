# Proactive Recall and Access Topology Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the parent spec's mechanism — a domain-filtered recall that rides the `neurostrata_append_log` response, a declared `fact.fields.topology` shape, and a normalized `metadata.domain` key.

**Architecture:** One new leaf module (`src/domain.rs`) owns slug normalization. The store gains a domain-filtered search behind a **new** `VectorStore` trait method with a default implementation, so exactly one implementation changes. The append_log handler widens to take the embedder and the store, embeds the normalized tags once, and returns up to three `recalled` entries plus — when a search ran — the `recalled_from` mode. The vocabulary gains `fact.fields.topology` with one 4 → 5 bump.

**Tech Stack:** Rust 2021, tokio, serde_json, LadybugDB (`lbug`), `async_trait`. Tests are inline `#[cfg(test)] mod tests` blocks inside each source file; run them with `cargo test --bin neurostrata-mcp <filter>`.

**Spec:** `docs/superpowers/specs/2026-10-21-proactive-recall-and-access-topology-design.md` — read it before starting. The plan argues from the spec; where the two differ, the spec wins and the plan gets corrected.

**Task:** `neurostrata-426k` (this plan is its first half; the second half is the implementation, tracked by the same task).
**Anchors:** live against the working tree — verify each claim at its anchor. *(the anchors were re-derived on 2026-10-22)*

---

## Global Constraints

Every task's requirements implicitly include this section.

- **One seat.** The recall rides the `neurostrata_append_log` response and nowhere else. No new MCP tool, no new snapshot lane, no `get_snapshot` strap — the parent spec §4.1.4 deferred those and they are closed in the recall-closure spec (`neurostrata-r30c`), not here.
- **`recalled` is always present** on the append_log response: an explicit empty array when there is nothing to recall. Silence about "nothing was recalled" is a lie the caller has to guess about (the repo's explicit-zero convention).
- **`recalled_from` is present exactly when a search ran.** `"domain"` means the domain filter chose the rows; `"vector"` means the filter matched nothing and was skipped, so the unfiltered rows were returned. It is **absent** when no search ran (an untagged turn, or every tag normalized away) and when the search failed — in neither case did a search produce rows, so there is no mode to report. Reporting `"vector"` there would claim a vector search that never happened, which is the misreport the mode exists to prevent. The two values map exactly onto the closure spec's two domain-filter counters (`domain_narrowed_hits` / `domain_fallback_vector`, closure spec `:535-536`); a call with no mode increments neither.
- **Only direct matches are recalled.** The store returns primaries *and* bounded graph expansions — one-hop, episode siblings, two-hop governs (`src/store/ladybug.rs:207-213`). An expansion row is context for a matched row, not the matched fact: a governed AST `file` node carries no `metadata.domain` and a synthetic score of 1.0. The recall keeps only the results that satisfy `crate::traits::is_direct_match` (`src/traits.rs:100-105`) — the **shared predicate** called by both the recall path and the search handler's access-count bump (`src/server.rs:2044`). Taking "the first three results the store returns" instead would recall governed nodes in any namespace with fewer than three primaries. `MockStore` must therefore stamp `DirectMatch` too (`src/guard/testkit.rs:144-148` stamps `DirectMatch`), or the mock-based tests would filter themselves to nothing.
- **Bounds.** At most **3** entries; `snippet` at most **120 characters**; one embedding per tagged turn and none at all when there are no tags.
- **The recall never mutates `access_count`.** Only `neurostrata_search_memory` (`src/server.rs:2048-2051`) and the get-memory handler (`src/server.rs:2705-2707`) increment it. A wrong recall must not be able to promote itself into a future snapshot's top-5.
- **Slug shape.** `domain` and the append_log `tags` normalize to lowercase-kebab tokens matching `^[a-z0-9]+(-[a-z0-9]+)*$`. Normalization happens **on write** and **on comparison** — the comparison normalizes the candidate row's stored value, which is what makes this migration-free.
- **The domain filter is soft.** If it would empty the candidate set, it is skipped and the unfiltered candidates are returned. A wrong domain must never blind recall. A silent substitution is forbidden, so the mode is always reported whenever a search ran.
- **`metadata.topology` is soft.** A malformed value raises a `WriteShapeWarning` (`src/store/ladybug.rs:951-955`) and the write still succeeds. A rejected write would lose the claim.
- **One vocabulary bump, here: 4 → 5**, carrying `fact.fields.topology`. The single assert is at `src/traits.rs:312`. The next bump (5 → 6, carrying `fact.fields.attempt` and `fact.fields.domain_registry`) belongs to the recall-closure spec (`neurostrata-r30c`) — not to this plan.
- **No storage migration.** `metadata` is a JSON blob; normalization happens at comparison time, so legacy free-text domains keep working without a rewrite or a grandfathering pass.
- **`VectorStore::search` keeps its signature.** The filter arrives as a new `search_with_domain` method with a default implementation; the `include_tasks` / `include_archived` flags are the precedent for adding a parameter to a search.
- **No consumer-system facts and no consumer vocabulary in the `NeuroStrata` namespace.** This plan ships generic machinery only.
- **The recall initializes the namespace before searching.** `store.init(&namespace)` first, exactly as `handle_search_memory` does (`src/server.rs:2024`). A search against an uninitialized namespace is not a failed query, it is a missing table.
- **A test that cannot fail is not a test.** `MockStore::increment_access_count` is no longer a no-op (`src/guard/testkit.rs:306-312`) — it records every id it was asked to increment — so "the recall never moves an `access_count`" is backed by a counter that can fail. The mock must **count** the increments, and the access-count assertions must be backed by the real `LadybugStore` somewhere, not only by the mock.
- **One binary.** `src/main.rs` is the crate root (`mod` list at `src/main.rs:1-21`), so a new module is registered there and nowhere else.
- **Baseline.** 659 tests pass. The production binary builds with 14 warnings; the test unit builds with 17. A task that adds a failing test, breaks a passing one, or adds a 15th production warning does not pass.
- **The supersede path needs no change.** It clones the old payload and replaces only `governs`, `related_to` and `contained_by` (`src/server.rs:1701`, `:1744`), so a stored `domain` is carried over, not rewritten. Correcting a wrong domain means a new write, not a supersede.
- **Never invent a source fact.** Cite `file:line`. If the code disagrees with this plan, the code wins: fix the plan in the same commit that fixes the code.

---

## Review Focus

The five input classes the spec implies but no acceptance criterion names. Each one gets a test, pinned to the task that owns the code.

1. **A tag that normalizes to nothing** (`"!!!"`, `"   "`). The recall must not build a query from it and must not filter on it; if every tag normalizes away, the turn recalls nothing and says so (`recalled: []`) instead of embedding a garbage query. → Task 6.
2. **A domain filter that matches nothing while the namespace has rows.** The filter must be skipped, the unfiltered rows returned, and the mode reported as `"vector"`. A wrong domain must never blind the recall. → Task 2.
3. **A legacy row whose stored domain is free text** (`"Database Access"`). It must match the slug `database-access` at comparison time, with no migration and with the stored value left exactly as it was. → Task 2.
4. **A `metadata.domain` that is not a string** (a number, an object). The write must not crash and must not guess: the value is stored as given, the comparison ignores it, and rejecting it is the recall-closure spec's write-path check, not this plan's. → Task 1.
5. **An untagged turn.** No embedding call, no search, `recalled: []`, **no `recalled_from` at all** — no search ran, so there is no mode to report — and the response must still have the same JSON shape as every other append_log response. → Task 6.

---

### Task 1: The domain slug normalizer, and the write path's use of it

**Files:**
- Create: `src/domain.rs`
- Modify: `src/main.rs:1-21` (add `mod domain;` between `mod daemon;` and `mod embed;`)
- Modify: `src/server.rs:1280-1282` (the add path's domain write)

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

At `src/server.rs:1280-1282`, the add path inserts the `domain` argument into the metadata verbatim. Once the metadata is assembled, a **string** `metadata.domain` is replaced with `normalize_domain_slug(value)` — wherever it came from, the `domain` argument or the caller's own `metadata` object — because the spec's data contract holds for every memory's `metadata.domain`: "lowercase-kebab slug; normalized on write **and** on comparison" (`docs/superpowers/specs/2026-10-21-proactive-recall-and-access-topology-design.md:271`). When the normalization returns `None`, the stored value is left exactly as it was given. A non-string `domain` is stored as given and is never coerced, converted or dropped — Review Focus #4.

- [ ] **Step 6: Add a test for the write path's behavior**

In `src/server.rs`'s inline test module, add a test that calls the add path with `domain: "Database Access"` and asserts the stored metadata's `domain` is `"database-access"`, and a second assertion with `domain: 42` that the stored value is still the number `42`. Use the same handler the existing add-path tests use. The same module also asserts that a string domain which normalizes to nothing (`domain: "!!!"`) is stored exactly as given, and that a domain arriving in the caller's `metadata` (`metadata: {"domain": "Database Access"}`) is stored as `"database-access"`.

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
- Modify: `src/traits.rs` (the `VectorStore` trait, after `search` at `:193-200`; the new types near the other trait types)
- Modify: `src/store/ladybug.rs` (the `LadybugStore` search, now the shared `search_inner` at `:17-528` with the trait's `search` and `search_with_domain` at `:1565-1614`, and the expansion block that follows the base loop at `:191-507`)

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
      /// or the filter found nothing and was skipped.
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

In `src/store/ladybug.rs`'s inline test module, add six `#[tokio::test]`s. Use `LadybugStore::for_testing(path, 4)` (`:662`) and `store.init(namespace)`, exactly as the existing store tests do; the vector dimension is 4, so upsert and search with a 4-float vector.

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

#[tokio::test]
async fn a_filtered_search_with_limit_below_unfiltered_count_returns_only_matching_rows() { /* seed
    more than `limit` rows whose domains do NOT match the key set, plus one that does; assert the
    filtered result holds the matching row and mode is Domain, and the unfiltered set exceeds `limit` */ }

#[tokio::test]
async fn mock_store_search_with_domain_matches_real_store_behaviour() { /* seed same rows into
    LadybugStore (via retired_test_store) and MockStore, run same search_with_domain calls against both,
    assert same id SET (sorted), same mode for narrowing and soft-fallback cases; order NOT compared */ }

#[tokio::test]
async fn mock_store_search_with_domain_honours_limit() { /* seed TWO rows matching "database" plus one
    each for "auth" and "compute", call with limit=1 for empty-key branch (domains=&[]) and filtered
    branch (domains=&["database"]), assert exactly 1 row returned in each case and assert the row's id */ }
```

The third test is Review Focus #3; the second is Review Focus #2. The legacy row in the third test is **seeded straight into the store with an upsert**, not written through the add path — the add path normalizes on write (Task 1), so a row that still reads `"Database Access"` is exactly what a pre-migration row looks like. The fourth test exercises the stop condition counting `filtered_primaries` (a filtered search that legitimately returns fewer than `limit` rows while the unfiltered set exceeds `limit`). The fifth test is a differential comparison: same inputs into both stores, asserting identical returned id SET (order is a legitimate implementation difference — real store ranks by distance, mock returns insertion order), same mode, for narrowing and soft-fallback cases. The sixth test exercises the mock's `limit` fix (empty-key and filtered branches) with two matching rows in the filtered branch so truncation is observable.

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test --bin neurostrata-mcp store::ladybug::tests`
Expected: FAIL — `search_with_domain` does not exist.

- [ ] **Step 3: Add the trait types and the default method**

Add `DomainMatchMode` and `DomainSearchOutcome` to `src/traits.rs`, and add `search_with_domain` to `trait VectorStore` with a body that calls `self.search(namespace, vector, limit, include_tasks, include_archived).await?` and returns `DomainSearchOutcome { results, mode: DomainMatchMode::Vector }`.

The default body is what keeps the blast radius at one implementation. The five `impl VectorStore for` sites other than `LadybugStore` are `src/events/bus.rs:66` (`StoreNotAttached`), `src/events/bus.rs:836` (`RecordingStore`), `src/daemon.rs:1321` (`RecordingStore`, in the daemon's test module — the struct is at `:1283`), `src/guard/testkit.rs:92` (`MockStore`) and `src/task/mod.rs:4310` (`ListHidingStore`); the implementors that override the new method are `src/store/ladybug.rs:1602` (`LadybugStore`) and `src/guard/testkit.rs:153` (`MockStore`). A store that cannot filter must say so with `Vector`, never pretend the filter applied.

- [ ] **Step 4: Run the trait tests**

Run: `cargo test --bin neurostrata-mcp traits::tests`
Expected: PASS — the existing trait tests still pass; the new method is additive.

- [ ] **Step 5: Override `search_with_domain` in `LadybugStore`**

`LadybugStore::search` (`src/store/ladybug.rs:1581`) delegates to the shared `search_inner` (`src/store/ladybug.rs:33`), which does two things inside one `with_conn` closure: the base candidate loop (`:95-190`) and the graph expansion (`:213` onward). The filter must sit inside the base candidate loop, so restructure that method as follows, all inside the same closure:

- The base loop keeps **two** lists: `all_primaries` (every row the loop accepts, exactly as today) and `filtered_primaries` (the rows whose `domain_key` is in the key set). The key set is an owned `HashSet<String>` built from the `domains` argument before the closure is entered — `with_conn` requires `FnOnce(&Connection) -> Result<T> + Send + 'static` (`:923-927`), so a borrowed slice cannot cross into it. **Each provided domain is normalized with `crate::domain::normalize_domain_slug` before being added to the set, so the store's contract is: the key set is compared by normalized slug — the same comparison-time normalization applied to the row's stored domain.**
- When the key set is empty, every row goes to both lists and the mode is `Vector` — no filter was asked for, so the behaviour is byte-identical to today's `search`.
- When the key set is non-empty, a row whose `domain_key` is `None` or is not in the set is skipped for `filtered_primaries` but still counted in `all_primaries`. The filter check happens **after** the row-class filters (`task`, tombstoned, mesh-conflicting) and **before** the usage boost, so the boost and the budget are spent only on rows the filter admitted.
- The loop's stop condition counts `filtered_primaries`, so a filtered search may legitimately return fewer rows than `limit`. **That is why `all_primaries` can exceed `limit`**: the loop keeps scanning (to fill the filtered set) long after it has accepted `limit` rows overall. The truncation below is therefore real, not a no-op.
- After the loop: if the key set was non-empty, `filtered_primaries` is empty and `all_primaries` is not, the filter is skipped — the chosen set is the first `limit` entries of `all_primaries` and the mode is `Vector`. Otherwise the chosen set is `filtered_primaries` and the mode is `Domain`.
- The graph expansion then runs on the chosen set, unchanged. The expansion is **not** domain-filtered: a topology fact's `GOVERNS` neighbour is the context that made the fact worth recalling, and filtering it away would break the one-hop path the parent spec §4.2.3 depends on.

`search` keeps its current signature and current behaviour; implement it by calling the same inner routine with an empty key set.

- [ ] **Step 6: Make the mock store domain-aware, and honest about direct hits**

`MockStore` (`src/guard/testkit.rs:32`) needs a `search_with_domain` override so that callers can be tested without a database: it filters its rows by `crate::domain::domain_key(&row.payload.metadata)` against the key set, and when the filter would empty the set it returns the unfiltered rows with `DomainMatchMode::Vector` — the same soft fallback the real store has. A test double that behaves differently from the store it stands in for is a trap.

Three more corrections to the mock, all required by the recall's contract:

- **`search` must stamp `EvidenceKind::DirectMatch`.** The stamping lives at `src/guard/testkit.rs:144-148`, which carries `DirectMatch` like every primary row the real store returns (`Some(vec![EvidenceKind::DirectMatch])`, `src/store/ladybug.rs:165-169`). The recall keeps only direct matches (Global Constraints), so a mock that returns no evidence filters itself to nothing — every mock-based recall test would fail for the wrong reason, and a future change that loosened the filter to "the first three results" would still pass. Stamp `DirectMatch` on every row the mock returns.
- **`increment_access_count` must count.** It records every id it was asked to increment (`src/guard/testkit.rs:306-312`), so "the recall never moves an `access_count`" is a counter-backed assertion instead of a no-op that passes for any handler. Give the mock a counter of the ids it was asked to increment, and expose it (a `pub(crate) fn access_count_increments(&self) -> Vec<String>`, or an `AtomicUsize` plus the ids) so the assertion is real.
- **`search` must be able to fail on demand.** Add a `pub(crate) fn set_search_fails(&self, fails: bool)` toggle so the recall's error path — "a store error must not fail the append" — has a test. Without it the error branch is untested code in a handler whose whole point is that it never fails the append.

These changes are additive: the mock's public surface grows, nothing that uses it changes shape.

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
- Modify: `src/server.rs:186-195` (the tool's `inputSchema`; the tool entry itself is `src/server.rs:183-196`)
- Modify: `src/server.rs:1967-2090` (`handle_search_memory`)
- Modify: `src/traits.rs:117-122` (the `DomainSearchOutcome` type) and `src/traits.rs:253-271` (the trait's default `search_with_domain`) — drop the two transient `#[allow(dead_code)]` markers Task 2 left on the `mode` field and on the default method, because this task is where the API becomes live

**Interfaces:**
- Consumes: `VectorStore::search_with_domain(...) -> Result<DomainSearchOutcome>` (Task 2).
- Produces: nothing new — `handle_search_memory` keeps its signature. It gains an optional `domain` argument in the tool schema.

- [ ] **Step 1: Write the failing tests**

In `src/server.rs`'s inline test module, add two `#[tokio::test]`s. Build the store with `crate::guard::testkit::MockStore` (a `pub(crate)` module, `src/guard/mod.rs:13`) and the embedder with the `StubEmbedder` already in that test module (`src/server.rs:3024`); seed the store with two facts carrying different `metadata.domain` values.

```rust
#[tokio::test]
async fn search_memory_without_a_domain_behaves_exactly_as_today() { /* no domain argument; assert the
    rendered report is the same shape as a search with no domain, and that no mode line was added */ }

#[tokio::test]
async fn search_memory_with_a_domain_filters_the_results_and_reports_the_mode() { /* a domain argument;
    assert the non-matching rows are gone and the report carries the mode line */ }
```

The filter has more than one outcome, so the implementation carries four more tool-level tests, all driving `handle_search_memory` rather than the store: `search_memory_with_a_domain_that_matches_nothing_reports_the_skip` (the soft fallback), `search_memory_with_a_domain_that_does_not_normalize_reports_the_skip` (a domain that does not normalize, as a string and as an array), `search_memory_with_an_empty_domain_argument_reports_no_mode` (an argument that names no domain), and `search_memory_with_a_free_text_domain_matches_the_legacy_row` (a legacy `"Database Access"` row matched by the slug, with no migration).

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

In `handle_search_memory`, read `arguments["domain"]` as either a string or an array of strings, normalize each with `crate::domain::normalize_domain_slug`, and drop the values that do not normalize. The survivors go into a `Vec<String>` named `slugs`.

The handler also tracks **whether a domain was requested**, because a requested filter that never reached the store still has to be reported. `domain_requested` is true when the `domain` key is present **and** the raw value carries at least one non-empty string entry: a plain string that is not empty or whitespace-only, or an array with at least one such entry. `domain` absent, `[]`, `""`, `[""]` and a whitespace-only string name no domain, so `domain_requested` is false and the search reads exactly as it did before the filter existed — that is the "no filter" case, and it does not change. A value that was asked for but does not normalize (`"!!!"`) leaves `slugs` empty while `domain_requested` stays true: the filter is reported as skipped, never silently replaced by an unfiltered search.

Replace the call at `src/server.rs:2026-2034` with `store.search_with_domain(namespace, vec, DEFAULT_LIMIT, include_tasks, include_archived, &slugs)`, and use `outcome.results` in place of `results`. Everything else in the handler — the `access_count` increment for direct hits (`src/server.rs:2042-2054`), the "No relevant memories found." branch, the rendered report — stays as it is.

- [ ] **Step 5: Report the mode**

When a domain was requested (`domain_requested` is true), prepend one line to the rendered report — the repo's "one flattened line" style. There are three variants; the em dash in the two skipped lines is U+2014:

- filter applied — the store's mode is `Domain` → `domain filter: applied (domain)`
- filter skipped, and at least one slug survived to compare — the store's mode is `Vector` → `domain filter: skipped — no row matched the domain; the results are unfiltered (vector)`
- filter skipped, and no slug survived, because the domain did not normalize → `domain filter: skipped — the domain did not normalize; the results are unfiltered (vector)`

The "no slug survived" case is decided first (`src/server.rs:2066`), because the store's mode alone cannot tell the two kinds of skip apart: an empty key set was never compared with anything, so the store reports `Vector` for it as well.

When no domain was requested, the report is byte-identical to today's. This is an orchestrator decision, not a spec line: the spec requires the mode only on the recall response, but the same silent-substitution rule applies here, and the spec's §4.3.5 gives the reason. It is additive — the line appears only when a caller asked for a domain.

- [ ] **Step 6: Run the tests**

Run: `cargo test --bin neurostrata-mcp server::tests::search_memory`
Expected: PASS.

- [ ] **Step 7: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed.

- [ ] **Step 8: Commit**

```bash
git add src/server.rs src/traits.rs
git commit -m "feat(search): an optional domain filter on neurostrata_search_memory"
```

---

### Task 4: The vocabulary bump — `fact.fields.topology`, version 4 → 5

**Files:**
- Modify: `src/schemas/memory-vocabulary.v3.json:2` (the version), `:66-75` (the `fact` type), `:164-167` (the two `tool_summaries` keys)
- Modify: `src/traits.rs:304-330` (the vocabulary test and its assert)

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

Use the same vocabulary-loading helper the neighbouring vocabulary tests use (`src/traits.rs:300` onward).

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
  "definition": "The access-path shape of the fact. All five keys are required and each is a string: component (where the path starts), target_system (what it reaches), channel (the transport or protocol), credential_boundary (the network, identity or trust boundary the channel crosses), reason (why the path is constrained). Extra keys are ignored. A missing or malformed value warns; the fact is still written."
}
```

This mirrors the `rule.fields` precedent (`:42-64`): a flat `{type, definition}` entry, no nested schema — the vocabulary file has no `properties`/`required` convention, and the machine-checked shape lives in the code (Task 5), not in prose.

- `tool_summaries.add` and `tool_summaries.memory_type` (`:164-167`) each gain a mention of `metadata.topology`, so the agent discovers the shape rather than guessing it.

- [ ] **Step 4: Move the assert**

`src/traits.rs:312` asserts the vocabulary version — change it to 5.

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
- Modify: `src/store/ladybug.rs:970-1098` (`parse_edge_specs`; the `WriteShapeWarning` struct is at `:951-955`)

**Interfaces:**
- Consumes: the declared shape from Task 4 (`component`, `target_system`, `channel`, `credential_boundary`, `reason`).
- Produces: nothing new — `parse_edge_specs` keeps its signature and returns `(Vec<EdgeSpec>, Vec<WriteShapeWarning>)`.

- [ ] **Step 1: Write the failing tests**

In `src/store/ladybug.rs`'s inline test module, add a **nested `mod topology`** and put the tests
inside it. The nesting is what makes this task's own run command select them: the filter
`store::ladybug::tests::topology` matches the module path, not a flat test name. A flat
`a_well_formed_topology_…` would live at `store::ladybug::tests::a_well_formed_topology_…`, which
does not contain that substring — the command below would report a false green.

```rust
mod topology {
    #[test]
    fn a_well_formed_topology_writes_without_a_warning() { /* a payload whose metadata.topology carries
        all five keys as strings; assert parse_edge_specs returns no warning mentioning topology */ }

    #[test]
    fn a_malformed_topology_warns_and_the_fact_is_still_written() { /* a payload whose metadata.topology is
        missing "channel", and ones whose metadata.topology is a string, an array and a null; assert each
        yields a WriteShapeWarning whose field is "metadata.topology" */ }

    #[test]
    fn a_topology_on_a_memory_that_is_not_a_fact_is_not_checked() { /* a rule carrying a malformed
        topology; assert no warning, because the field is declared on a fact alone */ }

    #[tokio::test]
    async fn a_fact_with_a_malformed_topology_is_still_written() { /* the real store; assert the fact is
        written and its metadata.topology is stored exactly as it was given */ }

    #[test]
    fn the_topology_keys_match_the_vocabulary() { /* compare the check's own key list against the
        vocabulary's five names, so a vocabulary bump cannot leave the check enforcing a stale shape */ }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --bin neurostrata-mcp store::ladybug::tests::topology`
Expected: FAIL — no warning is produced today.

- [ ] **Step 3: Add the check**

In `parse_edge_specs`, add a section (E) after the existing relation-key checks:

- `metadata.topology` absent → no warning.
- Present on a memory that is not a `fact` → no warning: the field is declared on `fact` alone, and an undeclared metadata key is accepted unchanged.
- Present on a `fact` and an object carrying all five keys, each a string → no warning. **Extra keys are ignored** — an extra key is not a defect and never warns.
- Present on a `fact` and anything else — not an object, a missing required key, or a present key that is not a string → one `WriteShapeWarning { field: "metadata.topology", why: <what is wrong>, skipped: "the topology shape check; the fact is still written" }`.

The check never affects the returned edge specs and never rejects the write. `parse_edge_specs` returns warnings; the caller emits them (the emit loop is at `src/store/ladybug.rs:1483-1485`) and the upsert continues.

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
- Modify: `src/buffer.rs:133-298` (`handle_append_log`) and `src/buffer.rs:300-322` (the `success_response` helper — one response shape, four call sites)
- Modify: `src/server.rs:371-382` (the `neurostrata_append_log` `inputSchema`)
- Modify: `src/server.rs:665-666` (the dispatch)
- Modify: `src/server.rs`'s inline test module (the real-store acceptance test + MCP wiring tests)
- Modify: `src/traits.rs:100-105` (the shared `is_direct_match` predicate)
- Modify: `src/server.rs:2038-2047` (the search handler's direct-hit block, which now calls the shared predicate)

**Interfaces:**
- Consumes: `crate::domain::normalize_domain_slug` (Task 1); `VectorStore::search_with_domain` and `DomainMatchMode` (Task 2); `crate::server::resolve_namespace(&Arc<dyn VectorStore>, &str) -> String` (`src/server.rs:2105`).
- Produces:
  ```rust
  // src/buffer.rs
  pub async fn handle_append_log(
      arguments: Value,
      emb: Arc<dyn Embedder>,
      store: Arc<dyn VectorStore>,
  ) -> String
  ```
  The response gains two fields: `recalled` (always present, 0–3 entries of `{id, memory_type, domain, score, snippet}`) and `recalled_from` (`"domain"` or `"vector"`, present **exactly when a search ran** — absent when no search ran and absent when the search failed).
  The shared direct-match predicate is also produced:
  ```rust
  // src/traits.rs
  pub fn is_direct_match(evidence: &Option<Vec<SearchEvidence>>) -> bool
  ```
  — one definition, two callers (the recall path in `src/buffer.rs`, the search handler's access-count bump in `src/server.rs`), so they cannot drift.
  On append failure (Err from `append_entry`), the response is a JSON object reporting the failure honestly:
  ```json
  {
    "appended": false,
    "error": "could not append to the Episodic Buffer: {e}",
    "file": "",
    "size_bytes": 0,
    "rolled": false,
    "tags": [...],
    "recalled": []
  }
  ```
  The failure is also logged with `tracing::warn!`, but the log line and the `error` field
  carry deliberately different texts: the log line is an event and reads "Failed to append
  to the Episodic Buffer: {e}", while the `error` field is the caller-facing message and
  reads "could not append to the Episodic Buffer: {e}" — the wording the tool used before
  this change, preserved so existing callers are unaffected.

- [ ] **Step 1: Write the failing tests**

In `src/buffer.rs`'s inline test module, add six tests. All six run against the mock store and the stub embedder — fast and unit-level. The seventh test — the real-store one — lives in `src/server.rs`'s inline test module instead, because it must drive the **real write path** (`handle_add_memory` is module-private) and then the recall (`crate::buffer::handle_append_log` is `pub`).

Mock-backed:

```rust
#[tokio::test]
async fn a_tagged_turn_recalls_the_matching_fact() { /* a fact stored with
    metadata.domain "Database Access"; call handle_append_log with tags ["database-access"] and assert
    the response's `recalled` holds that fact's id and `recalled_from` is "domain" */ }

#[tokio::test]
async fn the_recall_never_exceeds_three_entries_or_a_120_character_snippet() { /* four facts with
    long contents; assert `recalled` holds at most 3 entries and every `snippet` is at most 120
    characters */ }

#[tokio::test]
async fn an_untagged_turn_recalls_nothing_and_costs_no_embedding() { /* no tags; assert `recalled` is
    [] and the response carries NO `recalled_from` key at all */ }

#[tokio::test]
async fn a_tag_that_normalizes_to_nothing_is_dropped() { /* tags ["!!!", "  "]; assert no search ran
    and `recalled` is [] and there is no `recalled_from` */ }

#[tokio::test]
async fn the_recall_never_moves_an_access_count() { /* assert the mock's increment counter is
    unchanged across the call — the counter is what makes this assertion able to fail */ }

#[tokio::test]
async fn a_failed_search_does_not_fail_the_append() { /* a store whose search returns Err; assert
    the response still carries `recalled: []` and no `recalled_from` */ }
```

Real-store-backed (in `src/server.rs`'s inline test module):

```rust
#[tokio::test]
async fn a_fact_written_with_a_free_text_domain_is_recalled_by_its_slug_tag() { /* a temp-dir
    LadybugStore::for_testing(dir, 4); store.init("probe"); call handle_add_memory with namespace
    "probe", create_new_namespace true, memory_type "fact", domain "Database Access" and some
    content; assert the reply starts with "Successfully added memory"; then call
    crate::buffer::handle_append_log with namespace "probe", project_root <the temp dir> and tags
    ["database-access"]; parse the response as JSON; assert `recalled` holds the new fact's id and
    `recalled_from` is "domain"; assert the fact's stored access_count is still 0 */ }
```

The write goes through the real add path, so the stored domain is the normalized slug `database-access` — the write path normalizes on write (Task 1 Step 5) and the recall matches on the slug (Task 2). A legacy row that still reads `"Database Access"` is covered by Task 2's third test (`a_legacy_free_text_domain_matches_the_slug_at_comparison_time`).

The real-store test is acceptance criterion 1 (the free-text domain is written, the slug tag recalls it), criterion 2 (an `access_count: 0` fact is recalled) and criterion 4 (the recall moved no `access_count`) in the same namespace, through the real store, with no pre-seeded slug. `StubEmbedder` emits a 4-float vector (`src/guard/testkit.rs:327-345`) and `for_testing(dir, 4)` declares 4 dimensions, so the two agree. Use the established add-path pattern: `create_new_namespace: true` and no `project_root` in the add call (`src/server.rs:3683-3694` — the `.NeuroStrata` directory check only runs when a `project_root` is supplied, `src/server.rs:1215-1230`), and `quiet_bus()` (`src/server.rs:3355`) for the bus.

Use `crate::guard::testkit::{MockStore, StubEmbedder}` (a `pub(crate)` module, `src/guard/mod.rs:13`) for the mock store and the embedder, and the same config/buffer setup the existing buffer tests use (`scratch(tag)` at `src/buffer.rs:332`).

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --bin neurostrata-mcp buffer::tests`
Expected: FAIL — `handle_append_log` takes one argument and returns no `recalled`.

- [ ] **Step 3: Widen the handler and the dispatch**

Change `handle_append_log` to take `emb: Arc<dyn Embedder>` and `store: Arc<dyn VectorStore>` after `arguments`. At `src/server.rs:666`, call `crate::buffer::handle_append_log(arguments, emb.clone(), store.clone()).await` — exactly as the neighbouring dispatches do (`task_setup` at `:657-664`, `guard_validate` at `:668-676`).

- [ ] **Step 4: Add the optional `namespace` to the input schema**

In the `neurostrata_append_log` `inputSchema` properties, add:

```json
"namespace": { "type": "string", "description": "The exact project name. Optional: defaults to the project root's directory name, resolved against the known namespaces." }
```

It stays optional; `content` and `project_root` remain required.

- [ ] **Step 5: Resolve the namespace**

The handler needs a namespace to search. Read `arguments["namespace"]`; when it is absent, use `project_root.file_name()`. **An empty or whitespace-only `namespace` is treated as absent**, so it falls back to `project_root.file_name()` — the same convention the handler already uses for `content` and `project_root`. Without this rule an empty namespace resolved to `""`, `init("")` failed, and the recall silently produced nothing. Pass the result through `crate::server::resolve_namespace(&store, candidate)` (it is `pub(crate)`, `src/server.rs:2105`), which maps a path-like or unknown name onto the real namespace. The namespace is only **resolved** (`resolve_namespace`, a `list_namespaces` round-trip) when a search will actually run; an untagged turn pays no round-trip.

- [ ] **Step 6: Build the query and the key set**

Normalize each tag with `crate::domain::normalize_domain_slug`, dropping the ones that do not normalize. The surviving slugs are both the query text (joined with a single space) and the domain filter's key set — one pass, one meaning and one key. Keep them in a `Vec<String>` named `slugs`: the trait takes `domains: &[String]`, and the store turns the slice into its own owned `HashSet` (Task 2 Step 5).

- [ ] **Step 7: Run the recall**

- No surviving slug → no embedding, no search: `recalled: []`, and **no `recalled_from` key**.
- Otherwise → embed the joined query **once** (`emb.embed(&query)`), then `store.init(&namespace)` (the search handler does exactly this at `src/server.rs:2024`; a search against an uninitialized namespace is a missing table, not a failed query), then `store.search_with_domain(&namespace, vec, 3, false, false, &slugs)` and work from `outcome.results` and `outcome.mode`. `include_tasks` and `include_archived` are both false: task rows are backlog chatter and archived rows are tombstones, exactly as `search_memory` treats them by default.
- **Keep only the direct matches.** The store returns primaries *and* bounded graph expansions — one-hop, episode siblings, two-hop governs (`src/store/ladybug.rs:207-213`). An expansion row is context for a matched row, not the matched fact: a governed AST `file` node is a structural type excluded from the base query (`src/store/ladybug.rs:1264-1270`) and reachable only through the expansion, and it carries no `metadata.domain` and a synthetic score of 1.0. Keep a result only when its evidence satisfies `crate::traits::is_direct_match` (`src/traits.rs:100-105`) — the **shared predicate** called by both the recall path and the search handler's access-count bump (`src/server.rs:2044`). "The first three results the store returns" is **not** the rule: the store does not truncate at the primaries, so in any namespace with fewer than three primaries the first three results are expansion rows.
- Each entry is `{id, memory_type, domain, score, snippet}`: `domain` is the row's normalized slug or `null` when it has none; `score` is the result's score; `snippet` is the content truncated to 120 characters on a character boundary (`content.chars().take(120).collect()`).
- `recalled_from` is present **exactly when the search ran**: `"domain"` when the outcome's mode is `DomainMatchMode::Domain`, `"vector"` when the mode is `DomainMatchMode::Vector` (the filter matched nothing and was skipped, so the unfiltered rows came back). It is absent in the no-slug case above.
- A store error must not fail the append: the log entry is the handler's primary duty, so the error is logged with `tracing::warn!` and the response carries `recalled: []` and **no `recalled_from`** — no search produced rows, so there is no mode to report. Reporting `"vector"` there would claim a vector search that never happened.
- **If the append itself fails**, the response must not report success. It stays a JSON object — one shape for the response family — carrying `"appended": false`, an `"error"` field with the message `could not append to the Episodic Buffer: {e}`, the remaining keys (`file`, `size_bytes`, `rolled`, `tags`) with empty/zero values, and `recalled: []` with **no `recalled_from`** (no search ran). The failure is logged with `tracing::warn!`. A failed write never reports `"appended": true`.

- [ ] **Step 8: Add the fields to the response**

The response object is built by the `success_response` helper (`src/buffer.rs:300-322`, the JSON literal at `:310-320`), and it gains `recalled` and `recalled_from`. `recalled` is always present, on every response — the empty array is the explicit zero. `recalled_from` is present **only on the responses where a search ran**; build the response object conditionally rather than inserting a placeholder, so an untagged turn carries no mode key at all. A JSON consumer reading `recalled_from` must be able to conclude "a search ran and this is how it chose" — not "the mode is unknown". Every successful append goes through that one helper — the three recall-failure early returns and the final response — so the response contract cannot drift across call sites.

- [ ] **Step 9: Run the tests**

Run: `cargo test --bin neurostrata-mcp buffer::tests`
Expected: PASS — the 6 new buffer tests plus the existing buffer tests, and the new server test (run it with `cargo test --bin neurostrata-mcp a_fact_written_with_a_free_text_domain_is_recalled_by_its_slug_tag`).

- [ ] **Step 10: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed.

- [ ] **Step 11: Commit**

```bash
git add src/buffer.rs src/server.rs
git commit -m "feat(recall): ride a domain-filtered recall on the append_log response"
```

---

### Review-round additions (fix round 1)

Fix round 1 added five tests beyond the seven originally specified in Step 1:

1. **the append-failure test** — a regular file where `.NeuroStrata` must be created (never `chmod`, the suite may run as root) → `appended == false`, the error text present, `recalled` present.
2. **the empty-namespace test** → the search still runs against the `project_root.file_name()` fallback (`recalled_from` present).
3. **the counting-embedder test** → exactly one embed call for a tagged turn, zero for an untagged turn.
4. **the `tools/list` test** → the `neurostrata_append_log` `inputSchema` carries a `namespace` property that is **not** in `required`.
5. **the `tools/call` test** → the MCP path reaches the handler and the text result is the handler's JSON with `recalled` present.

---

### Task 7: The cross-cutting acceptance sweep

**Files:**
- Modify: `src/store/ladybug.rs` (the graph test)
- Modify: `src/synaptic_mesh/` (the mesh round-trip test — put it beside the mesh's existing import tests)

**Interfaces:**
- Consumes: everything Task 1–6 produce.
- Produces: nothing.

- [ ] **Step 1: Write the failing test — the topology fact reaches its governed file, and exports**

In `src/store/ladybug.rs`'s inline test module: upsert a `file` node first (so the edge resolves on the fact's write), then upsert a `fact` node whose metadata carries `governs: [<the file node's id>]` and a well-formed `metadata.topology` (all five keys, string-valued). Assert four things:

1. the fact's `metadata.topology` round-trips unchanged through a search for it;
2. the fact reaches the `file` node through the graph — the `GOVERNS` edge is what the one-hop expansion traverses (`src/store/ladybug.rs:213`) — with the expansion row carrying `EvidenceKind::OneHop`;
3. the `file` node is a **structural** type (`src/store/ladybug.rs:1264-1270`), so it is excluded from the base candidate query and is reachable *only* through that expansion — which is the point of the one-hop path, and the reason the recall filters to direct matches;
4. `export_graph` carries the fact node **and** the `GOVERNS` link to the file node. The call is `store.export_graph(false, false, &HashMap::new(), None)` — four arguments (`src/store/ladybug.rs:1974`, the trait declaration at `src/traits.rs:242`) — and the result is a JSON object with `nodes` and `links`. Acceptance criterion 5 asks for the topology fact to appear in `export_graph` with its `GOVERNS` edges; asserting only the search would leave that criterion untested.

- [ ] **Step 2: Write the failing test — the mesh round-trip**

Export a `fact` carrying `metadata.topology` through the mesh's record path and import it back; assert the imported memory's `memory_type` is still `fact` and its `metadata.topology` is intact. The mesh is closed over `SHAREABLE_MEMORY_TYPES` (`src/synaptic_mesh/import.rs:61`) and rejects an unknown `memory_type` at import (`:649-655`) — a `fact` stays inside the closure, which is what this test proves: the **import** path preserves a record's metadata when it is handed one. The **publish** side is a different path with a different contract: the published record's `metadata` is the mesh's curated envelope (`eligibility`, `source`, `tags`; `src/synaptic_mesh/worker.rs:282-296`, called from `:268`), so a `fact`'s `metadata.topology` is **not** published — the topology stays node-local, and this plan does not change the mesh's wire format. The publish-side boundary is pinned by its own test in `src/synaptic_mesh/worker.rs`'s test module (`a_published_fact_keeps_the_curated_metadata_envelope`): a published `fact` keeps `memory_type == "fact"` and the curated envelope, and carries **no** `topology` key. Put the test beside the mesh's existing import tests, and reuse their harness.

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

**The sweep also found a pre-existing bug, filed rather than fixed.** The store's graph expansion reads the query's outgoing-direction flag with `format!("{}", row[10]) == "true"` (`src/store/ladybug.rs:272` for the one-hop rows, `:332` for the two-hop rows), but the `lbug` crate renders a boolean as `True`/`False` (`lbug-0.20.4/src/value.rs:289-290`), so the comparison is always false and `outgoing` is always false. The outgoing one-hop query matches `(a:Memory)-[e]->(b:Memory)` with `a` in the primaries (`src/store/ladybug.rs:209`), so the true direction is primary→result, while the code builds `(src, tgt) = (result_id, primary_id)` (`src/store/ladybug.rs:275-279`) — the outgoing one-hop and two-hop evidence paths are therefore printed in the wrong direction. It does not affect the recall (which keeps only direct matches, `src/traits.rs:100-105`), and it predates this plan (`6e5ffe8`), so it is **not** fixed here — this task is test-only. It is filed as **`neurostrata-ttmr`** (P2, owner `opencode-agent`): fix the comparison and add a test asserting the one-hop edge runs fact→file. *(the defect this paragraph reports was fixed on 2026-10-22 by `neurostrata-ttmr`; the anchors in this paragraph name the code as it stood on 2026-10-21, and are deliberately not re-pointed)*

---

### Task 8: The rule-honesty conformance test — gated on `neurostrata-bzdn`

**Files:**
- Modify: `src/task/mod.rs` (the task validator's tests)
- Modify: `web-ui/public/graph.json` (the committed export, refreshed after the rule row lands)
- Write: one `rule` memory row in the `NeuroStrata` namespace — a data write through `neurostrata_add_memory`, not a file

**Interfaces:**
- Consumes: the tightened `rule-honesty` contract from `neurostrata-bzdn`.
- Produces: one `rule` row that conforms to that contract (the parent's spec §4.4: "This spec writes its own rule rows conforming to the tightened contract from day one"), plus the conformance test that keeps the contract honest.

**This task is gated.** The parent's spec §4.4 makes the rule-honesty hardening land first and separately, because the pre-push gate runs namespace-wide: tightening the validator flags the legacy rule rows until they are backfilled. That work is task `neurostrata-bzdn` (P2, ready, and now owned by the project owner for the migration ruling) and it is **not** this plan's to fold in — a tightening that lands without its backfill would fail the gate.

Tasks 1–7 do not depend on this task. This task depends on `neurostrata-bzdn` landing. If `neurostrata-bzdn` has not landed when you reach this task, it waits — the blocker is named, tracked and owned; it is not dropped.

**Blocking dependency: `neurostrata-bzdn`.** Task 8 — the AC-9 rule row plus its conformance test — is gated on task `neurostrata-bzdn` (the rule-honesty hardening plus the rule backfill). The concrete next action when it unblocks: write the rule row under spec §4.4 and add the conformance test.

**The gate's measured blast radius (measured 2026-10-22T01:51:32Z — `neurostrata-mcp export-graph`, live rows only: `superseded` false, no `valid_to` expiry).** The tightening `neurostrata-bzdn` proposes (the rule check at `src/task/gate.rs:398-425`, the wire registry at `src/task/wiring.rs:56-133` and `:171-177`) requires every live `rule` row to carry a valid `metadata.enforcement`. Measured on the live database:

| Namespace | live `rule` rows | rows with no `metadata.enforcement` |
|---|---|---|
| `NeuroStrata` | 102 | **96** |
| `keywest.health` | 147 | **147** |
| `global` | 2 | **2** |

These counts are a **point-in-time reading of a live database**: another session was writing rule rows throughout this session's own working window — the `keywest.health` count went 143 → 147 inside it — so the absolute numbers drift and a reader must re-measure rather than trust them. The conclusion does not drift: every namespace's rule set is affected, and the excepted rows are a handful, not a migration policy.

(The parent spec's figure was 86 `NeuroStrata` rule rows, 84 of them missing `enforcement` (`docs/superpowers/specs/2026-10-21-proactive-recall-and-access-topology-design.md:334`); the live namespace holds 102 live rule rows, 96 of them missing it.) The tightening therefore fails **three** namespaces' strict gates until their rows are corrected — not one — and two owner-ruled ways out exist, neither of them this plan's:

- **Grandfather** the legacy rows by their `valid_from` — migration-free, and the legacy rows stay unchecked, which is the honesty the tightening exists to end.
- **Backfill** every namespace (`NeuroStrata` 96, `keywest.health` 147, `global` 2) with a truthful `enforcement` — the honest end state, but it writes 245 rows, 147 of them in another project's namespace, and `keywest.health` is not this repo's to decide for.

The plan does not choose between them: the choice changes what a rule *in another project* means, so it is the owner's ruling. It is filed on `neurostrata-bzdn` with this measurement and the two options, and it is why Task 8 waits rather than being dropped.

(Whether the cross-namespace counts above may live in this repo's docs is a separate owner ruling, tracked as `neurostrata-k7lv`; the two options are stated there.)

- [ ] **Step 1: Confirm the gate**

Run: `neurostrata-mcp task validate NeuroStrata`
Expected: the tightened rule-honesty contract is enforced — a rule missing `enforcement`, or carrying a value outside `{ENFORCED, PARTIAL, NOT_ENFORCED}`, is reported. If it is not, `neurostrata-bzdn` has not landed: stop here, leave the task blocked on it, and say so.

- [ ] **Step 2: Write this spec's own rule row**

The parent's spec §4.4 says: "**This spec writes its own rule rows conforming to the tightened contract from day one.**" A conformance test proves the contract accepts a conforming rule; it does not put a conforming rule in the namespace. Acceptance criterion 9 is about a rule that *exists*.

Write one `rule` memory into the `NeuroStrata` namespace, stating this spec's own invariant — not a consumer's fact, so the no-consumer-facts guardrail holds:

- **content:** the recall that rides the `append_log` response never mutates `access_count`, never suppresses anything, and never exceeds three entries or a 120-character snippet.
- **`metadata.source`:** a dated string naming where the claim comes from, e.g. `"owner 2026-10-21"` or a derived reference to the parent spec's §4.1.3. `has_source` (`src/traits.rs:49-54`) accepts any non-blank string or object; the pre-push gate rejects a rule without one (`src/task/gate.rs:398-407`).
- **`metadata.enforcement`:** `"PARTIAL"` — the rule is audited in the Episodic Buffer, not machine-enforced at write time.
- **`metadata.guard`:** `"episodic-log"` — a real id in `known_wire_ids()` (`src/task/wiring.rs:171-177`, the reminder at `:152-156`). The tightened contract requires a non-null guard for `PARTIAL`, and the audit artifact the spec names is the Episodic Buffer.

Write it with the `neurostrata_add_memory` tool, not the CLI: the CLI's write path fails while the daemon holds the database lock, and the MCP tool path works. Then refresh the committed export (`neurostrata-mcp export-graph` → `web-ui/public/graph.json`) so the backup matches the graph. The `export-freshness` wire warns rather than blocks (`src/task/wiring.rs:116-121`), so a stale export does not fail the push — refresh it anyway, because the repo's convention is that the export is the backup.

- [ ] **Step 3: Write the test**

In `src/task/mod.rs`'s inline test module: a `rule` carrying `metadata.source`, `metadata.enforcement: "PARTIAL"` and a `metadata.guard` naming a wire id that exists in `known_wire_ids()` passes `task_validate`. Add the negative halves the contract names: `ENFORCED` with a null or dangling guard fails; `PARTIAL` with a null guard fails; `NOT_ENFORCED` with a non-null guard fails.

- [ ] **Step 4: Run it**

Run: `cargo test --bin neurostrata-mcp task::tests`
Expected: PASS.

- [ ] **Step 5: Run the full suite**

Run: `cargo test --bin neurostrata-mcp`
Expected: PASS — 0 failed.

- [ ] **Step 6: Commit**

```bash
git add src/task/mod.rs web-ui/public/graph.json
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

The note answers the questions a consumer of the recall will have, in prose: what `recalled` and `recalled_from` mean; that `recalled` is always present and empty means nothing matched; that `recalled_from` appears **only when a search ran** — absent on an untagged turn and absent when the search failed, because no search produced rows to attribute; that the filter is soft and the mode tells you whether it applied; that only direct matches are recalled (a graph expansion is context for a match, not a match); that the recall never changes `access_count`, so a recalled memory is not a promoted one; that the entries are pointers and the promotion is an explicit `neurostrata_get_memory` call; that the recall is additive and can never suppress anything.

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
- The vocabulary is at version 5, the assert at `src/traits.rs:312` says 5, and `fact.fields.topology` is declared and surfaced in both `tool_summaries` keys.
- `neurostrata_append_log` returns `recalled` on every response and `recalled_from` on every response where a search ran, with at most three entries and a 120-character snippet cap.
- `neurostrata_search_memory` behaves exactly as before when no `domain` argument is given.
- Eight of the plan's nine tasks are committed, and the CHANGELOG and `docs/recall.md` describe what shipped. The ninth — Task 8, the AC-9 rule row plus its conformance test — waits on `neurostrata-bzdn`: the blocker is named in Task 8 above together with its measured cross-namespace blast radius and the owner ruling it needs, it is tracked, and it is owned.
- The vocabulary bump belongs to this plan; the next bump (5 → 6) belongs to `neurostrata-r30c`.

---

## Review record

This plan was put to two independent seats on 2026-10-21, each answering the same six questions without seeing the other's answer, and the orchestrator arbitrated and verified every anchor a seat asserted.

**Seats.** Argon (Gemini 4 Argon via Copilot) — `ses_ea0d9d855ffe6axnjcxAZzp33F` · K3 (Kimi K3 via Copilot) — `ses_ea0d95c08ffemB6X5HI6DYRRqk`.

| Question | Argon | K3 | Resolution |
|---|---|---|---|
| Q1 — is the default-method trait form a faithful reading of §4.3.2/§4.3.5? | SOUND | SOUND | Kept as written. |
| Q2 — is the two-list candidate loop with an unfiltered expansion correct? | SOUND | SOUND, one imprecision | Kept; the imprecision was wording, now stated explicitly — `all_primaries` can exceed `limit` because the loop stops on the *filtered* count, so the fallback truncation is real. |
| Q3 — "the first three results" vs only `EvidenceKind::DirectMatch` rows? | **UNSOUND** | **AMBIGUOUS** | Both converged on the fix: the recall filters to direct matches, and `MockStore` must stamp `DirectMatch` (it stamped `evidence: None` before Task 2). K3's point — the positional rule only worked because the mock returns no evidence — is why the mock had to change too. |
| Q4 — is the vocabulary-bump ordering coherent; does the parent spec need an amendment? | SOUND | SOUND | No amendment. Both seats confirmed the parent spec's §13 already records `neurostrata-426k` as the closure's prerequisite without claiming 4 → 5 is final. |
| Q5 — is `recalled_from: "vector"` honest when nothing was searched? | **UNSOUND** | **AMBIGUOUS** | Both agreed the error case misreports. Resolved by the mode being present **exactly when a search ran** — absent on an untagged turn and absent on a failed search. That also makes the two values land exactly on the closure spec's two counters. |
| Q6 — coverage of the spec's acceptance criteria; are the anchors real? | **MISSING** (3 gaps) | **MISSING** (4 gaps) | All absorbed: the `export_graph` assertion (AC 5), the ≤3-cap and ≤120-char assertions (AC 3), the free-text-domain → slug-tag round trip (AC 1), the rule row this spec must write (AC 9), and the non-vacuous `access_count` assertions. |

**Anchor corrections applied.** `WriteShapeWarning` is at `src/store/ladybug.rs:951-955` (not `:429-434`); the warnings are emitted at `:1483-1485` (not `:923`); `rule.fields` opens at `src/schemas/memory-vocabulary.v3.json:42`; the five `impl VectorStore for` sites other than `LadybugStore` are `src/events/bus.rs:66`, `src/events/bus.rs:836`, `src/daemon.rs:1321`, `src/guard/testkit.rs:92` and `src/task/mod.rs:4310`, and the daemon's `RecordingStore` implements the trait at `:1321` (its struct is at `:1283`). Both seats' anchors were accurate apart from that drift.

**Spec amendments made in the same commit.** The parent spec's §4.1.2 (the mode's presence rule; direct matches only), §4.3.5 (no mode without a search), §5 (the `recalled_from` contract row), §4.2.4 and §4.3.2 (anchor corrections), §6 step 0 and §10 (the dependency edge, narrowed and named); and the closure spec's §3.4.2 (a call with no mode increments neither domain-filter counter).
