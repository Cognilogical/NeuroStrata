# Task 3 Report — Graph-read capability + `memories_for_file`

## Result

**DONE** — committed and pushed to `origin/main`.

| Field | Value |
| --- | --- |
| Commit SHA | `843f3c15239263f07ad703f26ab0553032443774` |
| Parent SHA | `3aebc86e364dc6d3dd90158bd903df6a7674bba8` |
| Branch | `main` (pushed `HEAD:main`, detached worktree) |
| Remote | `git@github.com:Cognilogical/NeuroStrata.git` |
| Test count | **839 passed, 0 failed** (`cargo test`) |
| Commit message | `feat: graph-read capability + memories_for_file tool` |

## What was built

### 1. `src/graph_read/` (crate-free ranking primitives)
- `mod.rs` — `pub mod memories_for_file;`
- `memories_for_file.rs` — `GraphMatchKind` (priority order:
  `ExactSymbol > ExactFile > SymbolTwoHop > AncestorDirectory > MetadataFallback`),
  `MemoryHit`, `memory_type_priority`, `freshness_priority`, `rank_hits`,
  constants `DEFAULT_MEMORY_TYPES` / `DEFAULT_LIMIT` / `MAX_LIMIT`.
  Depends only on serde + serde_json + std, so integration tests compile it
  without the full crate.

### 2. `GraphRead` trait (`src/traits.rs`)
- New `GraphRead` trait, separate from `VectorStore`.
- Re-exports `MemoryHit` / `GraphMatchKind` via `pub use`.

### 3. `LadybugStore::memories_for_file` (`src/store/ladybug.rs`)
- Uses `self.list()` for live nodes.
- Builds candidates: exact file / exact symbol / ancestor directory.
- Queries GOVERNS edges (direct + 2-hop symbol via CONTAINS).
- Adds metadata fallback; dedups by id; ranks; truncates.
- Reuses `crate::parser::ingest::normalize_node_path` (no path normalization
  duplication).

### 4. Stub auto-materialization (`src/store/ladybug.rs`)
- Unqualified `GOVERNS` targets resolving `Missing` now materialize a stub
  `file` node: id = `qualified_id(namespace, normalize_node_path(declared))`,
  `memory_type:"file"`, `user_id:"auto-ingestor"`,
  `agent_name:"neurostrata-stub"`, metadata `{"stub":true,"absolute_path":path}`,
  zero vector (decimal `0.0`).
- Qualified cross-namespace targets still defer (`Missing`).

### 5. MCP tool (`src/server.rs`)
- Registered `neurostrata_memories_for_file` + handler `handle_memories_for_file`
  + dispatch arm.
- Fixed `as_any` ambiguity with explicit
  `crate::traits::VectorStore::as_any(store.as_ref())`.
- Unsupported backends (e.g. `MockStore`) return
  `{"memories":[],"unsupported":true}`.
- tools/list test updated (now 31 tools).

### 6. Tests (`tests/graph_read/memories_for_file_test.rs`)
- Crate-free ranking contract tests (6 passed).
- Ladybug store tests: stub materialization + graph-read surfacing the
  governing rule for a stubbed file.

## Notes
- Detached-HEAD worktree; prior agent falsely claimed completion (no commit
  `a7b7c21`). This report carries the real SHA.
