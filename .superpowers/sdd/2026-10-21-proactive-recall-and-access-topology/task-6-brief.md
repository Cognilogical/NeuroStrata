### Task 6: The recall on the `append_log` response

**Files:**
- Modify: `src/buffer.rs:133-316` (`handle_append_log` + test module)
- Modify: `src/server.rs:371-381` (the `neurostrata_append_log` `inputSchema`)
- Modify: `src/server.rs:666` (the dispatch)
- Modify: `src/server.rs`'s inline test module (the real-store acceptance test + MCP wiring tests)
- Modify: `src/traits.rs:97-103` (shared `is_direct_match` helper)

**Interfaces:**
- Consumes: `crate::domain::normalize_domain_slug` (Task 1); `VectorStore::search_with_domain` and `DomainMatchMode` (Task 2); `crate::server::resolve_namespace(&Arc<dyn VectorStore>, &str) -> String` (`src/server.rs:2109`).
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
  A `tracing::warn!` is also emitted with the same error text.

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

The real-store test is acceptance criterion 1 (the free-text domain is written, the slug tag recalls it), criterion 2 (an `access_count: 0` fact is recalled) and criterion 4 (the recall moved no `access_count`) in the same namespace, through the real store, with no pre-seeded slug. `StubEmbedder` emits a 4-float vector (`src/guard/testkit.rs:327-345`) and `for_testing(dir, 4)` declares 4 dimensions, so the two agree. Use the established add-path pattern: `create_new_namespace: true` and no `project_root` in the add call (`src/server.rs:3685-3696` — the `.NeuroStrata` directory check only runs when a `project_root` is supplied, `src/server.rs:1214-1229`), and `quiet_bus()` (`src/server.rs:3357`) for the bus.

Use `crate::guard::testkit::{MockStore, StubEmbedder}` (a `pub(crate)` module, `src/guard/mod.rs:13`) for the mock store and the embedder, and the same config/buffer setup the existing buffer tests use (`scratch(tag)` at `src/buffer.rs:187`).

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test --bin neurostrata-mcp buffer::tests`
Expected: FAIL — `handle_append_log` takes one argument and returns no `recalled`.

- [ ] **Step 3: Widen the handler and the dispatch**

Change `handle_append_log` to take `emb: Arc<dyn Embedder>` and `store: Arc<dyn VectorStore>` after `arguments`. At `src/server.rs:664`, call `crate::buffer::handle_append_log(arguments, emb.clone(), store.clone()).await` — exactly as the neighbouring dispatches do (`task_setup` at `:656-662`, `guard_validate` at `:668-674`).

- [ ] **Step 4: Add the optional `namespace` to the input schema**

In the `neurostrata_append_log` `inputSchema` properties, add:

```json
"namespace": { "type": "string", "description": "The exact project name. Optional: defaults to the project root's directory name, resolved against the known namespaces." }
```

It stays optional; `content` and `project_root` remain required.

- [ ] **Step 5: Resolve the namespace**

The handler needs a namespace to search. Read `arguments["namespace"]`; when it is absent, use `project_root.file_name()`. **Treat an empty/whitespace-only `namespace` as absent** and fall back to the project root's directory name. Pass the result through `crate::server::resolve_namespace(&store, candidate)` (it is `pub(crate)`, `src/server.rs:2109`), which maps a path-like or unknown name onto the real namespace.

**Amendment (F4):** Extract the shared direct-match predicate into `src/traits.rs:97-103`:
```rust
pub fn is_direct_match(evidence: &Option<Vec<SearchEvidence>>) -> bool {
    evidence.as_ref().map_or(false, |evs| {
        evs.iter()
            .any(|e| e.kind == EvidenceKind::DirectMatch)
    })
}
```
Used by both `src/buffer.rs` (recall filtering) and `src/server.rs:2044-2047` (access-count bump).

- [ ] **Step 6: Build the query and the key set**

Normalize each tag with `crate::domain::normalize_domain_slug`, dropping the ones that do not normalize. The surviving slugs are both the query text (joined with a single space) and the domain filter's key set — one pass, one meaning and one key. Keep them in a `Vec<String>` named `slugs`: the trait takes `domains: &[String]`, and the store turns the slice into its own owned `HashSet` (Task 2 Step 5).

- [ ] **Step 7: Run the recall**

- No surviving slug → no embedding, no search: `recalled: []`, and **no `recalled_from` key**.
- Otherwise → embed the joined query **once** (`emb.embed(&query)`), then `store.init(&namespace)` (the search handler does exactly this at `src/server.rs:2023`; a search against an uninitialized namespace is a missing table, not a failed query), then `store.search_with_domain(&namespace, vec, 3, false, false, &slugs)` and work from `outcome.results` and `outcome.mode`. `include_tasks` and `include_archived` are both false: task rows are backlog chatter and archived rows are tombstones, exactly as `search_memory` treats them by default.
- **Keep only the direct matches.** The store returns primaries *and* bounded graph expansions — one-hop, episode siblings, two-hop governs (`src/store/ladybug.rs:191-197`). An expansion row is context for a matched row, not the matched fact: a governed AST `file` node is a structural type excluded from the base query (`src/store/ladybug.rs:1248-1254`) and reachable only through the expansion, and it carries no `metadata.domain` and a synthetic score of 1.0. Keep a result only when its evidence carries `EvidenceKind::DirectMatch` — the same test the search handler uses to decide what counts as a direct hit (`src/server.rs:2043-2046`). "The first three results the store returns" is **not** the rule: the store does not truncate at the primaries, so in any namespace with fewer than three primaries the first three results are expansion rows.
- Each entry is `{id, memory_type, domain, score, snippet}`: `domain` is the row's normalized slug or `null` when it has none; `score` is the result's score; `snippet` is the content truncated to 120 characters on a character boundary (`content.chars().take(120).collect()`).
- `recalled_from` is present **exactly when the search ran**: `"domain"` when the outcome's mode is `DomainMatchMode::Domain`, `"vector"` when the mode is `DomainMatchMode::Vector` (the filter matched nothing and was skipped, so the unfiltered rows came back). It is absent in the no-slug case above.
- A store error must not fail the append: the log entry is the handler's primary duty, so the error is logged with `tracing::warn!` and the response carries `recalled: []` and **no `recalled_from`** — no search produced rows, so there is no mode to report. Reporting `"vector"` there would claim a vector search that never happened.

- [ ] **Step 8: Add the fields to the response**

The response object at `src/buffer.rs:171-177` gains `recalled` and `recalled_from`. `recalled` is always present, on every response — the empty array is the explicit zero. `recalled_from` is present **only on the responses where a search ran**; build the response object conditionally rather than inserting a placeholder, so an untagged turn carries no mode key at all. A JSON consumer reading `recalled_from` must be able to conclude "a search ran and this is how it chose" — not "the mode is unknown".

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


---

## Binding Global Constraints (from the plan, apply to this task)

## Global Constraints

Every task's requirements implicitly include this section.

- **One seat.** The recall rides the `neurostrata_append_log` response and nowhere else. No new MCP tool, no new snapshot lane, no `get_snapshot` strap — the parent spec §4.1.4 deferred those and they are closed in the recall-closure spec (`neurostrata-r30c`), not here.
- **`recalled` is always present** on the append_log response: an explicit empty array when there is nothing to recall. Silence about "nothing was recalled" is a lie the caller has to guess about (the repo's explicit-zero convention).
- **`recalled_from` is present exactly when a search ran.** `"domain"` means the domain filter chose the rows; `"vector"` means the filter matched nothing and was skipped, so the unfiltered rows were returned. It is **absent** when no search ran (an untagged turn, or every tag normalized away) and when the search failed — in neither case did a search produce rows, so there is no mode to report. Reporting `"vector"` there would claim a vector search that never happened, which is the misreport the mode exists to prevent. The two values map exactly onto the closure spec's two domain-filter counters (`domain_narrowed_hits` / `domain_fallback_vector`, closure spec `:535-536`); a call with no mode increments neither.
- **Only direct matches are recalled.** The store returns primaries *and* bounded graph expansions — one-hop, episode siblings, two-hop governs (`src/store/ladybug.rs:191-197`). An expansion row is context for a matched row, not the matched fact: a governed AST `file` node carries no `metadata.domain` and a synthetic score of 1.0. The recall keeps only the results whose evidence carries `EvidenceKind::DirectMatch` — the same test the search handler uses (`src/server.rs:2043-2046`). Taking "the first three results the store returns" instead would recall governed nodes in any namespace with fewer than three primaries. `MockStore` must therefore stamp `DirectMatch` too (`src/guard/testkit.rs:144-148` stamps `DirectMatch`), or the mock-based tests would filter themselves to nothing.
- **Bounds.** At most **3** entries; `snippet` at most **120 characters**; one embedding per tagged turn and none at all when there are no tags.
- **The recall never mutates `access_count`.** Only `neurostrata_search_memory` (`src/server.rs:2051-2054`) and the get-memory handler (`src/server.rs:2708-2710`) increment it. A wrong recall must not be able to promote itself into a future snapshot's top-5.
- **Slug shape.** `domain` and the append_log `tags` normalize to lowercase-kebab tokens matching `^[a-z0-9]+(-[a-z0-9]+)*$`. Normalization happens **on write** and **on comparison** — the comparison normalizes the candidate row's stored value, which is what makes this migration-free.
- **The domain filter is soft.** If it would empty the candidate set, it is skipped and the unfiltered candidates are returned. A wrong domain must never blind recall. A silent substitution is forbidden, so the mode is always reported whenever a search ran.
- **`metadata.topology` is soft.** A malformed value raises a `WriteShapeWarning` (`src/store/ladybug.rs:935-939`) and the write still succeeds. A rejected write would lose the claim.
- **One vocabulary bump, here: 4 → 5**, carrying `fact.fields.topology`. The single assert is at `src/traits.rs:301`. The next bump (5 → 6, carrying `fact.fields.attempt` and `fact.fields.domain_registry`) belongs to the recall-closure spec (`neurostrata-r30c`) — not to this plan.
- **No storage migration.** `metadata` is a JSON blob; normalization happens at comparison time, so legacy free-text domains keep working without a rewrite or a grandfathering pass.
- **`VectorStore::search` keeps its signature.** The filter arrives as a new `search_with_domain` method with a default implementation; the `include_tasks` / `include_archived` flags are the precedent for adding a parameter to a search.
- **No consumer-system facts and no consumer vocabulary in the `NeuroStrata` namespace.** This plan ships generic machinery only.
- **The recall initializes the namespace before searching.** `store.init(&namespace)` first, exactly as `handle_search_memory` does (`src/server.rs:2023`). A search against an uninitialized namespace is not a failed query, it is a missing table.
- **A test that cannot fail is not a test.** `MockStore::increment_access_count` is no longer a no-op (`src/guard/testkit.rs:306-312`) — it records every id it was asked to increment — so "the recall never moves an `access_count`" is backed by a counter that can fail. The mock must **count** the increments, and the access-count assertions must be backed by the real `LadybugStore` somewhere, not only by the mock.
- **One binary.** `src/main.rs` is the crate root (`mod` list at `src/main.rs:1-21`), so a new module is registered there and nowhere else.
- **Baseline.** 638 tests pass and the build emits 14 warnings before Task 4 starts. A task that adds a failing test, breaks a passing one, or adds a 15th build warning does not pass.
- **The supersede path needs no change.** It clones the old payload and replaces only `governs`, `related_to` and `contained_by` (`src/server.rs:1700`, `:1743`), so a stored `domain` is carried over, not rewritten. Correcting a wrong domain means a new write, not a supersede.
- **Never invent a source fact.** Cite `file:line`. If the code disagrees with this plan, the code wins: fix the plan in the same commit that fixes the code.

---


## Review Focus (from the plan)

## Review Focus

The five input classes the spec implies but no acceptance criterion names. Each one gets a test, pinned to the task that owns the code.

1. **A tag that normalizes to nothing** (`"!!!"`, `"   "`). The recall must not build a query from it and must not filter on it; if every tag normalizes away, the turn recalls nothing and says so (`recalled: []`) instead of embedding a garbage query. → Task 6.
2. **A domain filter that matches nothing while the namespace has rows.** The filter must be skipped, the unfiltered rows returned, and the mode reported as `"vector"`. A wrong domain must never blind the recall. → Task 2.
3. **A legacy row whose stored domain is free text** (`"Database Access"`). It must match the slug `database-access` at comparison time, with no migration and with the stored value left exactly as it was. → Task 2.
4. **A `metadata.domain` that is not a string** (a number, an object). The write must not crash and must not guess: the value is stored as given, the comparison ignores it, and rejecting it is the recall-closure spec's write-path check, not this plan's. → Task 1.
5. **An untagged turn.** No embedding call, no search, `recalled: []`, **no `recalled_from` at all** — no search ran, so there is no mode to report — and the response must still have the same JSON shape as every other append_log response. → Task 6.

---

---

## Execution context

- BASE commit (before this task): `6488a47`
- Repo root / working directory: `/home/kenton/Documents/NeuroStrata`
- Branch: `main` (work in place; do not create a branch or worktree).
- Baseline: 644 tests pass; `cargo build --bin neurostrata-mcp` emits 14 warnings, `cargo build --tests --bin neurostrata-mcp` emits 17 for the test unit. Do not add a 15th.
- Tasks 1-5 are landed and reviewed. What you consume:
  - `crate::domain::normalize_domain_slug(&str) -> Option<String>` (Task 1) — the append_log `tags` normalize the same way the domain filter does.
  - `VectorStore::search_with_domain(&self, namespace: &str, vector: Vec<f32>, limit: usize, include_tasks: bool, include_archived: bool, domains: &[String]) -> Result<DomainSearchOutcome>` and `DomainMatchMode::{Domain, Vector}` (Task 2) — the recall calls it with `limit = 3`, `include_tasks = false`, `include_archived = false`, and the normalized tags as the key set.
  - `crate::domain::domain_key` (Task 1) — comparison-time normalization of a stored domain.
  - The `domain` argument on `neurostrata_search_memory` (Task 3) — the same three-way mode reporting exists there; the recall reports the mode as a field (`recalled_from`) rather than as a line.
- **The `recalled_from` presence rule (binding):** the field is present **exactly when a search ran**. It is absent for an untagged turn (no search ran), for a turn whose tags all normalize away (no search ran), and for a failed search (no rows were produced by a search that ran — the response carries no mode). Its two present values are `"domain"` (the domain filter narrowed the candidates) and `"vector"` (the filter matched nothing and was skipped). A call with no mode ran no filter and increments neither counter.
- **The recall keeps only direct matches** — rows whose evidence carries `EvidenceKind::DirectMatch`. The store returns primaries *and* bounded graph expansions; an expansion row is context for a matched row, not the matched fact. `MockStore` stamps `DirectMatch` too (Task 2), so the mock-based tests work.
- **The recall never mutates `access_count`.** Only `neurostrata_search_memory` and the get-memory handler increment it. A wrong recall must not be able to promote itself into a future snapshot's top-5.
- **The recall initializes the namespace before searching** — `store.init(&namespace)` first, exactly as `handle_search_memory` does.
- **The query text is the normalized tags joined by a space** (e.g. `"auth database"`), embedded **once** — one embedding call per tagged turn. The same normalized tags are the domain filter's key set, so a tagged turn is searched by both meaning and key in a single pass.
- The recall's entries are capped at 3 and each snippet is at most 120 characters.
- Cite `file:line` for anything you touch, and never invent a source fact: if the code disagrees with the brief, the code wins — fix the plan in the same commit that fixes the code (the plan's own rule).
