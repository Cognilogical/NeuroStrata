# Universal Source-Anchor Configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bind source-specific graph paths and default export destinations to
local canonical `.NeuroStrata` contexts without introducing VCS behavior into
Core.

**Architecture:** Add a backward-compatible `project_contexts` registry to the
user config and config-only bind/list/unbind commands. Pass registry data into
graph export, resolve each node only from an unambiguous matching `source_id`,
and resolve the export destination from an explicit source, project root, or
discovered marker. Keep path projection deterministic and filesystem checks
limited to configured markers, not source files.

**Tech Stack:** Rust, Clap, Serde/JSON, Tokio, existing `VectorStore` trait and
CLI test conventions.

**Spec:**
`docs/superpowers/specs/2026-10-18-universal-source-anchor-registry-design.md`

## Global Constraints

- Core must not discover repositories, inspect Git refs, or run Git.
- Registry keys are stable, nonempty `source_id` values.
- Registry values are absolute paths to existing canonical `.NeuroStrata`
  directories; derive project roots from each marker's parent.
- Keep provider-specific `source_context` fields opaque and round-tripped.
- Never use CWD as a source-path root or rewrite unrelated sources.
- Never check target-file existence or canonicalize target paths during export.
- Preserve old config files, graph shape, and stored `absolute_path` when source
  identity is absent, unknown, or conflicting.
- Config management must not open the database or require the daemon to stop.
- `--source-id` selects the default output context; it never rebases every graph
  node onto that context.

## Review Focus

- Two sources in one graph: each `absolute_path` uses only its own registry
  entry; test in Task 2.
- Missing, empty, or conflicting source IDs: preserve stored paths; test in
  Task 2.
- Traversal paths: reject any relative location escaping its source root; test
  in Task 2.
- Legacy environment override: rebase only the matching source, and warn/ignore
  an unscoped override; test in Task 2 and Task 3.
- Missing or ambiguous export context: require `--source-id` or explicit output
  path and never create `.NeuroStrata` under CWD; test in Task 3.

---

### Task 1: Add local project-context configuration and CLI management

**Files:**
- Modify: `src/config.rs`
- Modify: `src/main.rs`
- Test: unit tests in `src/config.rs` and `src/main.rs`

**Interfaces:**
- Produces `Config.project_contexts: HashMap<String, PathBuf>` with Serde
  defaulting to an empty map.
- Produces `Config::load_from(path: &Path) -> Result<Config>` and
  `Config::save_to(path: &Path) -> Result<()>` for explicit-path tests.
- Produces `Commands::ProjectContext` actions `Bind`, `List`, and `Unbind`.

- [ ] **Step 1: Write failing config tests**
  - `config_without_project_contexts_deserializes_empty_registry`: deserialize
    an old JSON config and assert the map is empty.
  - `bind_project_context_requires_nonempty_id_and_existing_marker`: reject an
    empty/whitespace-only ID, a missing path, a non-directory, and a directory
    whose final component is not `.NeuroStrata`.
  - `bind_project_context_stores_canonical_marker_and_preserves_other_config`:
    bind an existing marker, then assert its canonical absolute path is stored
    while `db_path` and deduplication settings remain unchanged.
  - `invalid_bind_or_unbind_leaves_config_bytes_unchanged`: compare config
    bytes before and after a rejected mutation.
  - `project_context_unbind_removes_only_requested_id`: retain other bindings.
- [ ] **Step 2: Run the focused config tests and verify they fail**

  Run: `cargo test config::tests:: -- --nocapture`

  Expected: compile/test failures identify missing registry and helpers.
- [ ] **Step 3: Implement config registry helpers in `src/config.rs`**
  - Add `#[serde(default)] project_contexts` to `Config`.
  - Factor loading into `Config::load_from(path: &Path)`; keep
    `Config::from_default_path()` as the existing user-config entry point.
  - Add bind/unbind operations that validate and canonicalize the marker path.
  - Add atomic save using a sibling temporary file followed by rename; preserve
    every unrelated config field.
- [ ] **Step 4: Add CLI parse tests**
  - `project_context_bind_parses_source_id_and_path`.
  - `project_context_list_parses_without_arguments`.
  - `project_context_unbind_parses_source_id`.
- [ ] **Step 5: Implement config-only command dispatch in `src/main.rs`**
  - Route project-context actions before daemon checks, embedder creation, or
    store construction.
  - Bind prints the stored canonical marker path; list prints IDs and paths;
    unbind reports the removed ID or a clear unknown-ID error.
  - Do not create a database, access memories, or start/stop the daemon.
- [ ] **Step 6: Run focused tests and commit**

  Run: `cargo test config::tests:: -- --nocapture && cargo test
  project_context -- --nocapture`

  Expected: all focused config and CLI tests pass.

  ```bash
  git add src/config.rs src/main.rs
  git commit -m "feat: configure local project contexts"
  ```

### Task 2: Resolve graph paths by source identity

**Files:**
- Create: `src/source_context.rs`
- Modify: `src/traits.rs`
- Modify: `src/store/ladybug.rs`
- Modify: `src/daemon.rs`
- Modify: `src/main.rs`
- Modify: `src/events/bus.rs`
- Modify: `src/task/mod.rs`
- Modify: every other `VectorStore::export_graph` call site
- Test: unit tests in `src/source_context.rs`, plus export tests in
  `src/store/ladybug.rs`

**Interfaces:**
- `VectorStore::export_graph` receives the existing two filter flags plus
  `project_contexts: &HashMap<String, PathBuf>` and
  `anchor_override: Option<&ScopedAnchorOverride>`.
- `source_context::ScopedAnchorOverride` contains `source_id: String` and
  `project_root: PathBuf`.
- `source_context::scoped_anchor_override(anchor_root: Option<&Path>,
  source_id: Option<&str>) -> Option<ScopedAnchorOverride>` constructs the
  validated legacy override; warn once when a root is unscoped.
- `source_context::export_absolute_path(location: &str,
  stored_absolute_path: &str, metadata: &serde_json::Value,
  project_contexts: &HashMap<String, PathBuf>,
  anchor_override: Option<&ScopedAnchorOverride>) -> String` resolves one
  node's primary location without mutating stored metadata.

- [ ] **Step 1: Write failing source-resolution tests**
  - `projects_each_source_under_its_own_marker`: two `refs[]` entries with
    distinct source IDs resolve under their own marker parents.
  - `preserves_stored_path_for_missing_unknown_or_conflicting_source_id`.
  - `matches_refs_by_exact_location_and_ignores_empty_source_ids`.
  - `preserves_absolute_locations_and_provider_fields`.
  - `rejects_relative_locations_that_escape_the_source_root`; accept normalized
    in-root `.`/`..` components, but preserve stored path for an escaping path.
  - `scoped_environment_override_only_rebases_its_matching_source`; an
    override for source A leaves source B unchanged.
  - `unscoped_environment_override_is_ignored`.
- [ ] **Step 2: Run focused tests and verify they fail**

  Run: `cargo test source_context::tests:: -- --nocapture`

  Expected: failures identify the missing resolver and boundary checks.
- [ ] **Step 3: Implement pure resolution helpers in
  `src/source_context.rs`**
  - Register `mod source_context;` in `src/main.rs`.
  - Collect nonempty `source_context.source_id` values only from `metadata.refs`
    whose `file` exactly equals the node's primary `location`.
  - Resolve only when the distinct matching ID set contains exactly one ID.
  - Read registry values as marker directories and derive the project root from
    the marker's parent; require the marker to remain an existing directory.
  - Use `NEUROSTRATA_ANCHOR_ROOT` only with a matching
    `NEUROSTRATA_SOURCE_ID` and an existing `<root>/.NeuroStrata` marker.
  - Build the override through an injectable helper taking both optional
    environment values; when root is set without source ID, emit a deprecation
    warning and return no override.
  - Lexically normalize relative locations; if normalization escapes the root,
    preserve the stored `absolute_path`.
  - Do not canonicalize or stat the target path. Preserve absolute `location`
    values as legacy data.
- [ ] **Step 4: Pass context inputs through the export boundary**
  - Add the registry and scoped override parameters to
    `VectorStore::export_graph` in `src/traits.rs`.
  - Update the Ladybug implementation, test doubles, and all call sites.
  - Replace `configured_anchor_root()` and its process-wide application with
    `source_context::export_absolute_path` for each node.
  - In `handle_get_graph`, load the current config registry for each request and
    forward the request's scoped override; map config errors to the existing
    internal-error response.
  - Add optional anchor root/source ID fields to `GraphQuery`; the daemon-backed
    CLI forwards its paired legacy environment values, while direct export
    passes the same `ScopedAnchorOverride` directly.
- [ ] **Step 5: Run focused export tests**

  Run: `cargo test source_context::tests:: -- --nocapture && cargo test
  export_graph -- --nocapture`

  Expected: source resolver and existing graph export tests pass; legacy
  unscoped rebasing tests are replaced by scoped behavior tests.
- [ ] **Step 6: Commit**

  ```bash
  git add src/source_context.rs src/traits.rs src/store/ladybug.rs src/daemon.rs src/main.rs
  git commit -m "fix: resolve graph paths by source context"
  ```

### Task 3: Anchor default graph output to a selected marker

**Files:**
- Modify: `src/source_context.rs`
- Modify: `src/main.rs`
- Modify: `CLI-readme.md`
- Test: resolver and CLI tests in `src/source_context.rs` and `src/main.rs`

**Interfaces:**
- `Commands::ExportGraph` gains `source_id: Option<String>` exposed as
  `--source-id <id>`.
- `source_context::resolve_graph_output_path(out_path: Option<&Path>,
  source_id: Option<&str>, contexts: &HashMap<String, PathBuf>,
  project_root_env: Option<&Path>, invocation_dir: &Path) -> Result<PathBuf>`
  resolves output without creating a marker tree.

- [ ] **Step 1: Write failing output-selection tests**
  - `explicit_output_path_overrides_destination_only` returns the supplied path
    unchanged, including when no marker is discoverable.
  - `source_id_selects_registered_marker_for_default_output` returns
    `<marker>/graph/graph.json` and rejects an unknown ID.
  - `project_root_environment_selects_its_existing_marker` uses
    `NEUROSTRATA_PROJECT_ROOT/.NeuroStrata`.
  - `invalid_project_root_environment_fails_without_cwd_fallback` returns a
    clear error when the configured project has no marker.
  - `discovers_nearest_parent_marker_from_invocation_directory` starts at the
    supplied invocation directory and returns the marker, not the directory.
  - `source_id_output_selection_is_independent_of_invocation_directory` returns
    the same marker destination for canonical and unrelated invocation paths.
  - `missing_context_never_falls_back_to_cwd_output_tree` returns a diagnostic
    requiring `--source-id` or an explicit output path without creating files.
  - `source_id_option_parses_with_existing_export_flags`.
- [ ] **Step 2: Run focused tests and verify they fail**

  Run: `cargo test resolve_graph_output_path -- --nocapture`

  Expected: missing resolver and CLI option failures.
- [ ] **Step 3: Implement output selection in `src/source_context.rs`**
  - Precedence: explicit `--source-id`; otherwise
    `NEUROSTRATA_PROJECT_ROOT`; otherwise nearest existing marker among
    invocation-directory ancestors.
  - An explicit output path overrides the destination only; when supplied with
    `--source-id`, still validate that ID.
  - Return `<selected-marker>/graph/graph.json` for the default destination.
  - Treat invocation directory only as a marker-search start. Never use it as a
    project root or create `.NeuroStrata` there.
- [ ] **Step 4: Use one resolver in both CLI export paths**
  - Use the same resolved target for the daemon-backed and direct-store paths.
  - Preserve existing freshness, supersede, and archived-row behavior.
  - Ensure parent directories are created only for the selected output target.
- [ ] **Step 5: Document registry and export behavior**
  - Update `CLI-readme.md` with bind/list/unbind syntax, `--source-id`, marker
    discovery, `NEUROSTRATA_PROJECT_ROOT`, and explicit-output behavior.
  - State that Core does not inspect Git and that each graph node resolves from
    its own source identity.
- [ ] **Step 6: Run all checks and commit**

  Run: `cargo fmt --check && cargo test && git diff --check`

  Expected: formatting, all unit/integration tests, and whitespace checks pass;
  existing export/supersede behavior remains covered.

  ```bash
  git add src/source_context.rs src/main.rs CLI-readme.md
  git commit -m "feat: anchor graph export to project context"
  ```

## Final Verification

- Run `cargo fmt --check`.
- Run `cargo test`; baseline before implementation was 546 passed, 0 failed.
- Run `git diff --check` and inspect the final diff for client-specific behavior,
  ambient-CWD rebasing, target-file existence checks, or Git commands.
- Confirm both direct and daemon-backed export paths use identical source
  resolution and output selection.
