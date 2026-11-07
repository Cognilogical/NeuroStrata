# Autonomous Memory & Task Control Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repair OpenCode V2 (`v2.2.6`) plugin propagation and hook contracts, eliminate multi-generation AST/doc flooding in hybrid recall and SPARQL projection, ship epoch-tracked multi-surface context injection with 2-hop cross-domain bridge synthesis, and unify task/memory candidate disposition with provenance spans and Lock 2 task-obligation gating.

**Architecture:** Five sequential waves spanning `@neurostrata/plugin-core` (`plugins/core/index.ts` bundled into all six harness adapters via `plugins/core/bundle-adapter.mjs`), the Rust harness plugin installer (`src/task/harness_plugins.rs`, `src/daemon.rs`), the LadybugDB hybrid search and SPARQL engines (`src/store/ladybug.rs`, `src/rdf/sparql.rs`), the task graph context builder (`src/task/mod.rs`), and the durable task/memory intake ledger (`src/task/intake.rs`, `src/server.rs`).

**Tech Stack:** TypeScript / Node.js (`>=18`, `node --test`), Rust (`cargo test`, `cargo clippy`, LadybugDB / Oxigraph).

---

## File Structure

- `plugins/core/index.ts` — Core harness plugin implementation (`resolveConfig`, `initialize`, `executeBeforeHandler`, `executeAfterHandler`, `sessionContextHandler`, `sessionCompactionHandler`, `sessionEndHandler`, `prefetchMemoriesForFile`, `enrichAndInjectSubagentPrompt`).
- `plugins/core/test.mjs` — Core harness plugin unit tests (`node --test`).
- `plugins/opencode/test.mjs` — OpenCode V2 (`v2.2.6`) plugin adapter unit tests (`node --test`).
- `plugins/*/dist/index.js` & `plugins/*/dist/index.d.ts` — Compiled + bundled harness adapters embedded by `include_str!` into `src/task/harness_plugins.rs`.
- `src/task/harness_plugins.rs` — Harness detection, plugin bundle writing, OpenCode config wiring/self-healing (`wire_harness_config`), and tests.
- `src/daemon.rs` — Daemon startup migration sweeps (`project_contexts` + `startup_root` plugin refresh).
- `src/store/ladybug.rs` — `active_generations_map_of`, `is_active_scoped_id`, `search_inner` active-generation filtering, normative reserve (`min_normative_slots`), and unit tests.
- `src/rdf/sparql.rs` — `project_sparql_dataset_with_meta_async` active-generation filtering and `union_default_graph` support eliminating duplicate default-graph quad insertion.
- `src/task/mod.rs` — `build_task_graph_context` normative + structural split, 2-hop cross-domain `conceptual_bridges` synthesis, `handle_task_complete` undisposed task-obligation check, and unit tests.
- `src/task/intake.rs` — `ProvenanceSpan`, `CandidateKind` (`TaskObligation` | `MemoryCandidate`), `MemoryCandidateCategory` (`Fact` | `Decision` | `RuleCandidate`), tiered auto-commit vs. candidate persistence, and `undisposed_high_confidence_tasks_for_session`.
- `src/server.rs` — Extended `neurostrata_task_candidates` tool schema (`kind`, `category`, `auto_commit`, `provenance`) and `handle_get_snapshot` active-task/bridge payload.

---

## Wave 1: OpenCode V2 (`v2.2.6`) Plugin Contract Repair & Daemon Startup Propagation

### Task 1.1: Fix OpenCode V2 `execute.after`, `location` Root Resolution, and Epoch Hooks in `plugins/core/index.ts`

**Files:**
- Modify: `/home/kenton/Documents/NeuroStrata/plugins/core/index.ts`
- Modify: `/home/kenton/Documents/NeuroStrata/plugins/core/test.mjs`
- Modify: `/home/kenton/Documents/NeuroStrata/plugins/opencode/test.mjs`

- [ ] **Step 1: Write failing tests in `plugins/core/test.mjs` and `plugins/opencode/test.mjs` for OpenCode V2 contracts**

Add tests verifying:
1. `initialize(ctx)` resolves the project root from `ctx.location.project.canonical` (then `ctx.location.directory`, `ctx.projectDir`, `ctx.cwd`, `process.cwd()`).
2. `execute.after` mutates `event.result.content` in-place when invoked with OpenCode V2's `{ name, callID, args, status: "completed", result: { content, output, metadata } }` envelope, both for `TASK INTENT DETECTED` nudges and for file-read/grep/edit JIT memory recall (`[NeuroStrata File Context — <path>]`).
3. `ctx.session.hook("context", fn)` and `ctx.session.hook("compaction", fn)` are registered when `ctx.session?.hook` is a function; `context` injects `[NeuroStrata Active Context]` once per epoch and `compaction` increments the epoch so the next `context` hook re-injects.

```javascript
// Add to plugins/opencode/test.mjs
test("OpenCode V2 execute.after mutates event.result.content and registers session.hook context/compaction", async () => {
  const tmpDir = mkdtempSync(join(tmpdir(), "ns-oc2-v226-"));
  const nsDir = join(tmpDir, ".NeuroStrata");
  mkdirSync(nsDir, { recursive: true });
  writeFileSync(
    join(nsDir, "active_session.json"),
    JSON.stringify({
      namespace: "NeuroStrata",
      task_id: "neurostrata-a1si",
      status: "in_progress",
      session_id: "ses_v226",
      updated_at: new Date().toISOString(),
    }),
  );
  writeFileSync(
    join(nsDir, "PluginConfig.json"),
    JSON.stringify({
      guardMode: "warn",
      enableSubagentContextPropagation: true,
      daemonUrl: "http://127.0.0.1:59999",
    }),
  );

  const sessionHooks = new Map();
  const toolHooks = new Map();
  const ctx = {
    location: {
      directory: tmpDir,
      project: { root: tmpDir, canonical: tmpDir },
    },
    session: {
      hook(name, fn) {
        sessionHooks.set(name, fn);
      },
    },
    tool: {
      hook(name, fn) {
        toolHooks.set(name, fn);
      },
    },
  };

  await plugin.initialize(ctx);
  assert.equal(typeof sessionHooks.get("context"), "function");
  assert.equal(typeof sessionHooks.get("compaction"), "function");
  assert.equal(typeof toolHooks.get("execute.after"), "function");

  const afterEvent = {
    name: "read",
    callID: "call_1",
    args: { path: "src/server.rs" },
    status: "completed",
    result: {
      content: "TODO: wire task candidate provenance in server.rs",
      output: "TODO: wire task candidate provenance in server.rs",
    },
  };
  await toolHooks.get("execute.after")(afterEvent);
  assert.match(afterEvent.result.content, /TASK INTENT DETECTED/);
  assert.match(afterEvent.result.output, /TASK INTENT DETECTED/);

  const ctxEvent1 = { sessionID: "ses_v226", messages: [] };
  await sessionHooks.get("context")(ctxEvent1);
  assert.equal(typeof ctxEvent1.additionalContext, "string");
  assert.match(ctxEvent1.additionalContext, /neurostrata-a1si/);

  const ctxEvent2 = { sessionID: "ses_v226", messages: [] };
  await sessionHooks.get("context")(ctxEvent2);
  assert.equal(ctxEvent2.additionalContext, undefined);

  await sessionHooks.get("compaction")({ sessionID: "ses_v226" });
  const ctxEvent3 = { sessionID: "ses_v226", messages: [] };
  await sessionHooks.get("context")(ctxEvent3);
  assert.match(ctxEvent3.additionalContext, /neurostrata-a1si/);
});
```

- [ ] **Step 2: Run plugin tests to verify the new test fails**

Run: `node --test plugins/opencode/test.mjs`
Expected: FAIL (`sessionHooks.get("context")` is undefined and `afterEvent.result.content` is not mutated when `ctx.location.project.canonical` is used instead of `process.cwd()`).

- [ ] **Step 3: Implement OpenCode V2 `location` resolution, `event.result.content` mutation, JIT file-touch recall, and `ctx.session.hook("context" | "compaction")` in `plugins/core/index.ts` and bundle all adapters**

Update `plugins/core/index.ts`:
1. Add helper `resolveProjectRoot(ctx?: any): string` that checks:
   - `ctx?.location?.project?.canonical`
   - `ctx?.location?.project?.root`
   - `ctx?.location?.directory`
   - `ctx?.projectDir`
   - `ctx?.directory`
   - `ctx?.cwd`
   - `process.cwd()`
2. Update `executeAfterHandler(event, config)` to:
   - Extract tool output from `event?.result?.content ?? event?.result?.output ?? event?.output ?? event?.result ?? event?.response ?? event?.content ?? ""`
   - Extract touched file path from `event?.args?.path ?? event?.args?.filePath ?? event?.input?.path ?? event?.toolInput?.path` for `read`/`edit`/`write`/`grep` tools, fetch `prefetchMemoriesForFile(projectRoot, namespace, filePath, config)` (deduplicated per `(sessionId, epoch, canonicalPath)`), and format a compact `[NeuroStrata File Context — <relPath>]` block (up to 2 rules/decisions/facts + 1 cross-domain bridge).
   - Detect task-intent signals on non-NeuroStrata tools and append the `[NeuroStrata Plugin — TASK INTENT DETECTED]` nudge.
   - Write the combined suffix back into `event.result.content` (and `event.result.output` if string) when `event.result` is an object, while preserving legacy `event.output` / `event.content` / `event.additionalContext` fallback for V1/Claude/Hermes harnesses.
3. Add `sessionContextHandler(event, config)` and `sessionCompactionHandler(event, config)` and register them via `ctx.session.hook("context", ...)` and `ctx.session.hook("compaction", ...)` in `initialize(ctx)`.

Rebuild core and bundle all six adapters:
Run:
```bash
npm --prefix plugins/core run build && \
for d in opencode openclaw claude hermes codex github-cli; do \
  node plugins/core/bundle-adapter.mjs "plugins/$d"; \
done
```

- [ ] **Step 4: Run plugin test suites across all adapters**

Run:
```bash
for d in core opencode openclaw claude hermes codex github-cli; do \
  node --test "plugins/$d/test.mjs"; \
done
```
Expected: PASS (0 failures across all 7 test suites).

### Task 1.2: Fix OpenCode `.opencode/opencode.json` Self-Healing & Daemon Startup Plugin Refresh in Rust

**Files:**
- Modify: `/home/kenton/Documents/NeuroStrata/src/task/harness_plugins.rs`
- Modify: `/home/kenton/Documents/NeuroStrata/src/daemon.rs`

- [ ] **Step 1: Write a failing Rust unit test in `src/task/harness_plugins.rs` for `.opencode/opencode.json` self-healing**

Add unit test `opencode_wire_harness_config_removes_invalid_file_plugin_entry_and_installs_autoload_shim`:
```rust
#[test]
fn opencode_wire_harness_config_removes_invalid_file_plugin_entry_and_installs_autoload_shim() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let opencode_dir = root.join(".opencode");
    std::fs::create_dir_all(&opencode_dir).expect("mkdir .opencode");
    std::fs::write(
        opencode_dir.join("opencode.json"),
        r#"{
  "plugins": [
    "./.NeuroStrata/plugins/opencode/dist/index.js",
    "custom-plugin-dir"
  ]
}"#,
    )
    .expect("seed opencode.json");

    let report = install_harness_plugin(root, HarnessKind::OpenCode).expect("install opencode");
    assert!(report.config_wired);
    assert!(root.join(".opencode/plugins/neurostrata.js").is_file());

    let parsed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(opencode_dir.join("opencode.json")).expect("read opencode.json"),
    )
    .expect("valid json");
    let plugins = parsed
        .get("plugins")
        .and_then(|v| v.as_array())
        .expect("plugins array preserved for remaining entry");
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].as_str(), Some("custom-plugin-dir"));
}
```

- [ ] **Step 2: Run `cargo test` for `harness_plugins` to confirm failure**

Run: `cargo test --lib task::harness_plugins::tests::opencode_wire_harness_config_removes_invalid_file_plugin_entry_and_installs_autoload_shim`
Expected: FAIL (because `wire_harness_config(HarnessKind::OpenCode)` currently inserts `"./.NeuroStrata/plugins/opencode/dist/index.js"` into `.opencode/opencode.json`).

- [ ] **Step 3: Update `wire_harness_config(root, HarnessKind::OpenCode)` in `src/task/harness_plugins.rs` and add daemon startup refresh in `src/daemon.rs`**

1. In `src/task/harness_plugins.rs`:
   - In `wire_harness_config(root, HarnessKind::OpenCode)`:
     - Always write `.opencode/plugins/neurostrata.js` (`export { plugin, default } from "../../.NeuroStrata/plugins/opencode/dist/index.js";\n`).
     - If `.opencode/opencode.json` exists, inspect `"plugins"` and strip any legacy `"./.NeuroStrata/plugins/opencode/dist/index.js"` or `".NeuroStrata/plugins/opencode/dist/index.js"` file-path entries; if `"plugins"` becomes empty, remove the `"plugins"` key (and only write `.opencode/opencode.json` when creating/cleaning it as needed, leaving clean configs untouched).
2. In `src/daemon.rs`:
   - During daemon startup (alongside existing startup sweeps), iterate `startup_root` and every configured directory in `Config::load().map(|c| c.project_contexts).unwrap_or_default().values()` and run `crate::task::harness_plugins::auto_install_detected_plugins(std::path::Path::new(dir))` fail-soft so existing workspaces (`NeuroStrata` and all configured project contexts) are upgraded immediately on daemon boot.

- [ ] **Step 4: Run `cargo test` on `harness_plugins`**

Run: `cargo test --lib task::harness_plugins`
Expected: PASS.

---

## Wave 2: Active-Generation Isolation & Normative Reserve in Hybrid Search & SPARQL

### Task 2.1: Filter Inactive AST/Doc Generations and Reserve Normative Slots in `LadybugStore::search_inner`

**Files:**
- Modify: `/home/kenton/Documents/NeuroStrata/src/store/ladybug.rs`

- [ ] **Step 1: Write a failing unit test in `src/store/ladybug.rs` proving stale `code_ast`/`doc_section` generations crowd out normative memories in `search`**

Add test `search_excludes_inactive_ingest_generations_and_preserves_normative_memories`:
```rust
#[tokio::test]
async fn search_excludes_inactive_ingest_generations_and_preserves_normative_memories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LadybugStore::new(dir.path(), 4).expect("store");
    let ns = "gen_isolation_ns";

    // Seed an active normative rule
    let rule = Memory {
        id: "rule-1".to_string(),
        namespace: ns.to_string(),
        content: "Zero-Action Start mandatory workflow rule for task claim".to_string(),
        memory_type: MemoryType::Rule,
        metadata: serde_json::json!({}),
        access_count: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.insert(rule, vec![1.0, 0.0, 0.0, 0.0]).await.expect("insert rule");

    // Seed active generation = 3 for source_key "docs"
    store.set_active_generation_for_test(ns, "docs", 3).await;

    // Insert stale gen 1 and gen 2 doc_section rows + active gen 3 doc_section row
    for gen in [1u64, 2, 3] {
        let doc = Memory {
            id: format!("{}::{}::doc:agents:zero-action", ns, gen),
            namespace: ns.to_string(),
            content: "Zero-Action Start mandatory workflow rule for task claim".to_string(),
            memory_type: MemoryType::DocSection,
            metadata: serde_json::json!({ "source_key": "docs" }),
            access_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        store.insert(doc, vec![1.0, 0.0, 0.0, 0.0]).await.expect("insert doc");
    }

    let results = store
        .search(ns, Some("Zero-Action Start"), Some(vec![1.0, 0.0, 0.0, 0.0]), 5, None, false)
        .await
        .expect("search");

    let ids: Vec<String> = results.iter().map(|r| r.memory.id.clone()).collect();
    assert!(ids.contains(&"rule-1".to_string()), "normative rule must be returned: {:?}", ids);
    assert!(
        !ids.iter().any(|id| id.contains("::1::") || id.contains("::2::")),
        "inactive generations must never be returned by search: {:?}",
        ids
    );
}
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `cargo test --lib store::ladybug::tests::search_excludes_inactive_ingest_generations_and_preserves_normative_memories`
Expected: FAIL (`::1::` and `::2::` rows are returned by `search_inner`).

- [ ] **Step 3: Implement `active_generations_by_source_of`, `is_active_scoped_row`, and normative slot reservation in `src/store/ladybug.rs`**

In `src/store/ladybug.rs`:
1. Add `active_generations_by_source_of(conn: &Connection, namespace: &str) -> (HashMap<String, i64>, HashSet<i64>, Option<i64>)` that queries `MATCH (g:IngestGeneration) WHERE g.namespace = '...' RETURN g.source_key, g.active_gen`, returning:
   - `by_source`: `source_key -> active_gen`
   - `active_set`: set of active generations across sources
   - `max_code_gen`: max active generation across non-`"docs"` sources (because `code_ast` rows ingested via `ingest_directory` use per-directory `source_key` while their row metadata does not duplicate `source_key`, and each `ingest_directory` increments `max_gen + 1` monotonically).
2. In both `list()` and `search_inner()`, validate scoped IDs (`split_scoped_id(&id)`):
   - If `memory_type == "doc_section"`, require `gen == by_source.get("docs")`.
   - If `memory_type == "code_ast"`, check `metadata.get("source_key")` in `by_source` if present, otherwise require `Some(gen) == max_code_gen` (and in `active_set` when `by_source` has explicit source matches).
   - Strip internal `ns::gen::` prefix from returned `Memory.id` (or normalize consistently with `list()`) so duplicate generation copies never survive into RRF fusion.
3. In `search_inner()`, when `memory_type.is_none()` and `limit >= 3`, reserve up to `min(3, limit.div_ceil(2))` slots for normative memories (`rule`, `decision`, `fact`, `procedure`) that pass the similarity/BM25 relevance floor (`>= 0.20`), filling remaining slots with the highest-RRF structural (`code_ast`, `doc_section`) rows.

- [ ] **Step 4: Run `LadybugStore` search and generation tests**

Run: `cargo test --lib store::ladybug::tests::search`
Expected: PASS.

### Task 2.2: Deduplicate SPARQL Dataset Projection & Filter Inactive Generations in `src/rdf/sparql.rs`

**Files:**
- Modify: `/home/kenton/Documents/NeuroStrata/src/rdf/sparql.rs`

- [ ] **Step 1: Write a failing test in `src/rdf/sparql.rs` for active-generation filtering and single-graph quad budget**

Add a test in `src/rdf/sparql.rs` verifying that `project_sparql_dataset_with_meta_async` skips inactive generation rows (`ns::1::...` when active gen is `2`) and does not double-count quads across named + default graph when projecting a namespace.

- [ ] **Step 2: Run the SPARQL test to confirm failure**

Run: `cargo test --lib rdf::sparql::tests`
Expected: FAIL on the new assertion.

- [ ] **Step 3: Update `project_sparql_dataset_with_meta_async` and `execute_sparql_query_with_config` in `src/rdf/sparql.rs`**

1. In `project_sparql_dataset_with_meta_async`:
   - Call `store.list(ns, None, ...)` or apply `store.filter_active_generation_memories(ns, memories)` so inactive `ns::gen::` rows are excluded before RDF triple projection.
   - Insert each projected triple into the namespace NamedGraph `GraphName::NamedNode(graph)` ONLY (stopping the duplicate `GraphName::DefaultGraph` insertion that doubled `quad_count` to `256,550`), and configure `QueryOptions::default()` / `SparqlEvaluator` with `.with_union_default_graph()` (or default graph alias) so both `SELECT ... WHERE { ?s ?p ?o }` and `GRAPH <urn:neurostrata:namespace:...> { ?s ?p ?o }` work with half the memory footprint.

- [ ] **Step 4: Run all RDF and SPARQL tests**

Run: `cargo test --lib rdf::`
Expected: PASS.

---

## Wave 3: Multi-Surface Recall & 2-Hop Cross-Domain Bridge Synthesis (Areas 2 & 3)

### Task 3.1: Synthesize 2-Hop Cross-Domain `conceptual_bridges` in `build_task_graph_context` and `memories_for_file`

**Files:**
- Modify: `/home/kenton/Documents/NeuroStrata/src/task/mod.rs`
- Modify: `/home/kenton/Documents/NeuroStrata/src/graph_read/memories_for_file.rs`
- Modify: `/home/kenton/Documents/NeuroStrata/src/server.rs`

- [ ] **Step 1: Write a failing test in `src/task/mod.rs` for `conceptual_bridges` and normative/structural split in `build_task_graph_context`**

Add a test in `src/task/mod.rs` that creates:
- File A (`src/server.rs`) governed by Rule M1 (`"Daemon caller path check required on ingest"`)
- File B (`plugins/core/index.ts`) linked via a 2-hop `RELATES_TO` / `DEPENDS_ON` edge to M1
- Claims a task touching `plugins/core/index.ts` and asserts `auto_retrieved_context` contains:
  - `normative_memories` (including M1)
  - `conceptual_bridges` with `why_connected` path explanation (`plugins/core/index.ts -> ... -> rule M1`)
  - `subagent_prompt_injection` including a `### Cross-Domain Bridges` section.

- [ ] **Step 2: Run the test to confirm failure**

Run: `cargo test --lib task::tests`
Expected: FAIL (`conceptual_bridges` field does not exist yet).

- [ ] **Step 3: Implement 2-hop bridge synthesis in `src/task/mod.rs` and `src/graph_read/memories_for_file.rs`**

1. In `src/task/mod.rs` (`build_task_graph_context`):
   - Partition matches into `normative_memories` (`rule`, `decision`, `fact`, `procedure`, up to 4) and `structural_anchors` (`code_ast`, `doc_section`, up to 3).
   - For the top seed memories, traverse 2 hops (`store.get_related_multi_hop` with `max_hops = 2`) and select up to 2 cross-domain bridge candidates whose `memory_type` is normative (`rule`, `decision`, `fact`, `procedure`) and whose governed file prefix / tags differ from the primary seed (`jaccard_similarity < 0.35`).
   - Emit `conceptual_bridges: [{ id, memory_type, content, why_connected }]` and include them in `subagent_prompt_injection` and `active_session.json`.
2. In `src/graph_read/memories_for_file.rs`:
   - Include up to 2 2-hop cross-domain bridge memories in the `memories_for_file` response metadata (`conceptual_bridges`) so the plugin's `execute.after` JIT file-touch hook can surface hidden architectural connections when reading/editing a file.

- [ ] **Step 4: Run `task` and `graph_read` tests**

Run: `cargo test --lib task:: && cargo test --lib graph_read::`
Expected: PASS.

---

## Wave 4: Tiered Task & Memory Obligation Ledger with Provenance Spans (Areas 1 & 4)

### Task 4.1: Extend `src/task/intake.rs` and `src/server.rs` with Tiered Auto-Commit, Provenance Spans, and Lock 2 Obligation Check

**Files:**
- Modify: `/home/kenton/Documents/NeuroStrata/src/task/intake.rs`
- Modify: `/home/kenton/Documents/NeuroStrata/src/task/mod.rs`
- Modify: `/home/kenton/Documents/NeuroStrata/src/server.rs`
- Modify: `/home/kenton/Documents/NeuroStrata/plugins/core/index.ts`

- [ ] **Step 1: Write failing tests in `src/task/intake.rs` and `src/task/mod.rs` for tiered auto-commit and Lock 2 undisposed obligation gating**

Add tests verifying:
1. High-confidence `Fact` / `Decision` candidates with `ProvenanceSpan` (`turn_id`, `role`, `source_tool`, `evidence_quote`, `confidence >= 0.85`) submitted with `auto_commit: true` create a linked `Memory` (`metadata.extracted_from: [task_id]`, `metadata.provenance: ...`) and mark the candidate `Promoted`.
2. `RuleCandidate` items are NEVER auto-committed as `MemoryType::Rule` even if `auto_commit: true` is passed — they remain `status: "pending"` candidates requiring explicit promotion or user confirmation.
3. High-confidence `TaskObligation` candidates (`role: "user"` or `"checker"`, `confidence >= 0.85`) with `status: "pending"` block `handle_task_complete` with an actionable error listing the candidate IDs and how to promote (`neurostrata_task_create`) or dismiss (`neurostrata_task_candidates` `action: "dismiss"`) them.

- [ ] **Step 2: Run the intake and task completion tests to verify failure**

Run: `cargo test --lib task::intake::tests`
Expected: FAIL.

- [ ] **Step 3: Implement `ProvenanceSpan`, `CandidateKind`, tiered auto-commit, and Lock 2 obligation enforcement**

1. In `src/task/intake.rs`:
   - Add `ProvenanceSpan { turn_id: Option<String>, role: Option<String>, source_tool: Option<String>, evidence_quote: Option<String>, confidence: f64 }`.
   - Add `kind: String` (`"task_obligation"` | `"memory_candidate"`, defaulting to `"task_obligation"`) and `category: Option<String>` (`"fact"` | `"decision"` | `"rule_candidate"`) to `TaskCandidate`.
   - Add `undisposed_high_confidence_obligations(store, namespace, session_id, parent_task_id)` returning pending `task_obligation` rows with `confidence >= 0.85` from `"user"` or `"checker"` provenance.
2. In `src/server.rs` (`handle_task_candidates`):
   - Support `kind`, `category`, `provenance`, and `auto_commit` on `action: "record"`.
   - When `auto_commit == true` and `provenance.confidence >= 0.85`:
     - If `kind == "memory_candidate"` and `category` is `"fact"` or `"decision"`, insert the `Memory` with `metadata.extracted_from = [active_task_id]` and `metadata.provenance`, and mark the candidate `Promoted`.
     - If `category == "rule_candidate"`, refuse auto-promotion to `MemoryType::Rule` and keep as `"pending"` candidate.
3. In `src/task/mod.rs` (`handle_task_complete`):
   - Query `undisposed_high_confidence_obligations`; if non-empty, return a Lock 2 block listing each undisposed obligation ID + title + dismissal/promotion command.
4. In `plugins/core/index.ts`:
   - Update `executeAfterHandler` to extract high-confidence decision/fact markers and task-intent obligations with provenance spans and post them fail-soft to the daemon/intake ledger.

- [ ] **Step 4: Run Rust and plugin test suites**

Run:
```bash
npm --prefix plugins/core run build && \
for d in opencode openclaw claude hermes codex github-cli; do \
  node plugins/core/bundle-adapter.mjs "plugins/$d"; \
done && \
cargo test --lib task::
```
Expected: PASS.

---

## Wave 5: End-to-End Verification, Local Installation, and Pull Request

### Task 5.1: Full Test Matrix, Binary Rebuild, Local Workspace Refresh, and PR Creation

**Files:**
- Modify: `/home/kenton/Documents/NeuroStrata/plugins/*/dist/index.js`
- Modify: `/home/kenton/Documents/NeuroStrata/plugins/*/dist/index.d.ts`

- [ ] **Step 1: Run all Node.js plugin tests across all 7 packages**

Run:
```bash
for d in core opencode openclaw claude hermes codex github-cli; do \
  node --test "plugins/$d/test.mjs"; \
done
```
Expected: PASS (0 failures).

- [ ] **Step 2: Run full Rust test suite and clippy gates**

Run:
```bash
cargo test --lib && cargo clippy --all-targets -- -D warnings
```
Expected: PASS with zero warnings.

- [ ] **Step 3: Install updated `neurostrata-mcp` binary and refresh local `.opencode` / `.NeuroStrata` plugin bundles**

Run:
```bash
cargo install --path . --force
```
Expected: `/home/kenton/.cargo/bin/neurostrata-mcp` updated and `neurostrata_get_snapshot` refreshes `.opencode/plugins/neurostrata.js` and cleans `.opencode/opencode.json`.

- [ ] **Step 4: Extract architectural memory, complete task `neurostrata-a1si`, push branch, and open PR**

1. Extract memory via `neurostrata_add_memory` with `metadata.extracted_from: ["neurostrata-a1si"]`.
2. Complete task via `neurostrata_task_complete`.
3. Commit all changes on `opencode/v116-autonomous-memory-control-plane`, push to `origin`, and open PR via GitHub MCP / `gh pr create`.
