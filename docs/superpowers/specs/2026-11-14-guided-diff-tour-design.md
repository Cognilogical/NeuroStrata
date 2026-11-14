# Design: `neurostrata_diff_tour` — Guided Diff Tour with AST Topological Ordering, Blast-Radius & `GOVERNS` Audit

**Date:** 2026-11-14
> Status: DRAFT — tracked under Epic `neurostrata-mtqx`, Feature Task `neurostrata-737r` (Spec Task `neurostrata-sttv`).
**Approach:** VCS-Agnostic Core Diff Parser + Call-Graph Topological Stop Clustering (`Foundations` $\rightarrow$ `Core Logic` $\rightarrow$ `Surface & Callers` $\rightarrow$ `Verification & Docs`) + Automated `GOVERNS` Rule & Untested Blast-Radius Audit.

## 1. Problem

Sourcegraph's **Guided Diff Tour** demonstrated that reviewing multi-file diffs in alphabetical file order forces humans and code-review agents (`reviewer`, `checker-free`) to jump back and forth between callers, types, and tests.

NeuroStrata has a unique advantage over Sourcegraph for diff review: in addition to an AST call/import graph (`DEPENDS_ON`), NeuroStrata holds normative **`GOVERNS` rules**, **SHACL constraints**, and **failed `attempt` memories**. However, NeuroStrata currently has no tool that maps a changeset/diff onto the knowledge graph to:
1. Order changed hunks topologically (definitions/schemas before core logic before callers before tests).
2. Identify **un-edited callers in the blast radius** whose callee signatures or behavior changed.
3. Surface every active `GOVERNS` rule and historical failed `attempt` that applies to the modified line ranges.

## 2. Goals

1. **VCS-Agnostic Core Diff Engine (`src/graph_read/diff_tour.rs`):**
   - Strictly obey Rule `905f5ca4-b0b0-4d41-936c-73c4babd983d` (*"Universal Core remains Git-free; Core does not invoke Git"*): the core engine takes `diff_text: &str` (standard unified diff format) or pre-parsed `Vec<ChangedFileSpan>` and operates purely against `LadybugStore`.
   - CLI (`neurostrata-mcp diff-tour`) and MCP (`neurostrata_diff_tour`) accept `diff_text` directly, or an optional outer-boundary VCS adapter reads the diff from `project_root` (verified via `check_caller_path`).
2. **Hunk-to-Symbol Mapping:**
   - Parse unified diff `@@ -old_start,old_len +new_start,new_len @@` headers per file path.
   - Intersect modified line ranges `[new_start, new_start + new_len]` (and deleted ranges `[old_start, old_start + old_len]`) with active ingested `symbol` / `code_ast` / `doc_section` nodes in the namespace.
3. **Four-Stage Topological Tour Clustering:**
   - Partition modified symbols/hunks into ordered narrative stops:
     1. **Stop 1 — `Schema, Traits & Foundations`:** Modified structs, enums, traits, interfaces, schemas (`src/schema.json`, `src/traits.rs`), and leaf foundational helpers depended upon by other modified symbols.
     2. **Stop 2 — `Core Implementation`:** Interior call-graph functions/methods ordered topologically (callees before callers via Kahn's algorithm / Tarjan SCC over `DEPENDS_ON` edges).
     3. **Stop 3 — `Entrypoints, Surface & Blast Radius`:** CLI/MCP/HTTP handlers (`src/main.rs`, `src/server.rs`, `src/daemon.rs`) plus an explicit **`unchanged_callers` warnings list** naming symbols outside the diff that call modified symbols.
     4. **Stop 4 — `Tests & Documentation`:** Test functions (`#[test]`, `#[tokio::test]`, `tests/`, `*test*`) and documentation sections (`docs/`, `*.md`).
4. **Governance & Hazard Audit per Stop:**
   - Attach every active `GOVERNS` rule (direct symbol, file, or ancestor directory) and any `metadata.attempt.outcome == "failed"` memory touching the stop's files/symbols.
   - Flag `untested_modified_symbols`: modified non-test symbols in Stops 1–3 that have neither a modified caller in Stop 4 nor an unchanged test caller in the 1-hop `DEPENDS_ON` graph.

## 3. Non-Goals

- Invoking external VCS subprocesses inside `src/store/` or `src/graph_read/` (strictly forbidden by Rule `905f5ca4-b0b0-4d41-936c-73c4babd983d`).
- LLM-generated prose commentary inside the daemon (all stop rationales, dependency chains, and governance citations are deterministic).

## 4. Architecture

### 4.1 Data Structures (`src/graph_read/diff_tour.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffTourRequest {
    pub namespace: String,
    /// Optional project root (verified via check_caller_path on /diff-tour).
    pub project_root: Option<String>,
    /// Raw unified diff text. Preferred & 100% VCS-agnostic.
    pub diff_text: Option<String>,
    /// Optional explicit file/line spans when caller already parsed the diff.
    pub changed_spans: Option<Vec<ChangedFileSpan>>,
    pub max_blast_radius_callers: Option<usize>, // default 15
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFileSpan {
    pub path: String,
    pub old_lines: Option<(u32, u32)>,
    pub new_lines: Option<(u32, u32)>,
    pub change_kind: String, // "added" | "modified" | "deleted" | "renamed"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TourItem {
    pub path: String,
    pub lines: String,
    pub symbol: Option<String>,
    pub symbol_kind: Option<String>,
    pub change_kind: String,
    pub rationale: String, // e.g. "Trait definition depended on by 3 modified methods in Stop 2"
    pub modified_callees_in_diff: Vec<String>,
    pub modified_callers_in_diff: Vec<String>,
    pub unchanged_callers: Vec<String>,
    pub governing_rule_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TourStop {
    pub stop_number: u8,     // 1..=4
    pub category: String,    // "foundations" | "core_logic" | "surface_and_callers" | "tests_and_docs"
    pub title: String,
    pub items: Vec<TourItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffTourResponse {
    pub namespace: String,
    pub total_files_changed: usize,
    pub total_symbols_touched: usize,
    pub stops: Vec<TourStop>,
    pub governing_rules: Vec<LocateRuleSummary>,
    pub failed_attempts: Vec<LocateFailedAttempt>,
    pub untested_modified_symbols: Vec<String>,
    pub uningested_files: Vec<String>,
}
```

### 4.2 Topological Classification & Ordering

Given the set of touched symbol nodes $M = \{s_1, \dots, s_k\}$ and the induced subgraph $G_M = (M, E_M)$ where $(u, v) \in E_M \iff u \xrightarrow{\text{DEPENDS\_ON}} v$:

1. **Test / Doc Detection (Stop 4):**
   - Any symbol/file where `memory_type == "doc_section" | "markdown"`, `path` starts with `docs/` or `tests/`, or symbol name/attributes match `test_*` / `*_test` is assigned to Stop 4 (`tests_and_docs`).
2. **Foundation Detection (Stop 1):**
   - Any non-test symbol with `symbol_kind in ["structs", "impls", "classes", "types", "interfaces"]` (matching `src/schema.json` `extracts` keys), or `path` in schema/trait definitions (`src/traits.rs`, `src/schema.json`), or in-degree $> 0$ in $G_M$ with out-degree $== 0$ in $G_M$ (pure dependency sink within the diff) is assigned to Stop 1 (`foundations`).
3. **Surface Detection (Stop 3):**
   - Any non-test symbol in entrypoint paths (`src/main.rs`, `src/server.rs`, `src/daemon.rs`) or with in-degree $== 0$ and out-degree $> 0$ in $G_M$ is assigned to Stop 3 (`surface_and_callers`).
4. **Core Logic (Stop 2):**
   - All remaining modified symbols are assigned to Stop 2 (`core_logic`) and ordered in reverse topological order of `DEPENDS_ON` (callees before callers; ties broken by `path` ASC, `start_line` ASC).
5. **Unchanged Caller Blast Radius:**
   - Query 1-hop incoming `DEPENDS_ON` edges from symbols $u \notin M$ to $v \in M$. Attach up to `max_blast_radius_callers` to `v.unchanged_callers`.

## 5. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Newly added functions/files in the diff are not yet in `LadybugStore` | Report newly added paths not yet in the active `IngestGeneration` under `uningested_files` and classify their hunks by path/header heuristics (`tests/`, `docs/`, `src/traits.rs`) while resolving ancestor directory `GOVERNS` rules (which still match via directory prefix!). |
| Cyclic `DEPENDS_ON` edges (mutual recursion) | Collapse Strongly Connected Components (Tarjan's SCC) before topological sort and order within an SCC by `(path, start_line)`. |
| Oversized diffs ($>500$ hunks) | Cap parsed hunks at `MAX_DIFF_HUNKS = 500` and return `truncated: true` with a summary count when exceeded. |

## 6. Checkable Acceptance Criteria

1. Feeding a multi-file unified diff touching `src/traits.rs`, `src/store/ladybug.rs`, `src/server.rs`, and `tests/` produces 4 non-empty stops in deterministic order (`foundations` $\rightarrow$ `core_logic` $\rightarrow$ `surface_and_callers` $\rightarrow$ `tests_and_docs`).
2. When `fn callee()` in `src/b.rs` is modified by the diff and `fn caller()` in `src/a.rs` calls `callee()` but is **not** in the diff, `TourItem` for `callee()` lists `src/a.rs::caller` in `unchanged_callers`.
3. Every active `GOVERNS` rule and failed `attempt` covering any touched file or ancestor directory appears in `DiffTourResponse.governing_rules` / `failed_attempts` and is linked from the corresponding `TourItem.governing_rule_ids`.
4. Universal Core (`src/graph_read/` and `src/store/`) contains zero VCS subprocess invocations per Rule `905f5ca4-b0b0-4d41-936c-73c4babd983d`.

## 7. Next Step

On approval of this spec, invoke `writing-plans` for Task `neurostrata-737r` to produce the step-by-step TDD implementation plan.
