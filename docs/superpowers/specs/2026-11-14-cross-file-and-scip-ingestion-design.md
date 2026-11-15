# Design: Two-Tier Cross-File Call Graph — Workspace Tree-Sitter Symbol Resolution & SCIP Compiler-Grade Ingestion

**Date:** 2026-11-14
> Status: DRAFT — tracked under Epic `neurostrata-mtqx`, Feature Task `neurostrata-rbde` (Spec Task `neurostrata-sttv`).
**Approach:** Two-Tier Architecture — Tier 1 upgrades built-in Tree-sitter ingestion (`src/parser/ingest.rs`) with a pre-publish workspace symbol resolution pass using `dependency_map` (`src/parser/ingest.rs:1285, 1477`); Tier 2 adds SCIP (`scip.proto`) compiler index ingestion (`neurostrata_ingest_scip`) using the official license-compatible `scip` / `prost` crates for exact cross-file/cross-crate definitions, references, and trait/interface implementations.

## 1. Problem

NeuroStrata's AST call graph (`DEPENDS_ON` edges on `code_ast` rows) currently has a structural blind spot compared to compiler-grade SCIP code intelligence:

1. **Same-File-Only Call Resolution (`src/parser/ingest.rs:881-961`, `1285`, `1476-1501`):**
   - Inside `run_consumer` (`src/parser/ingest.rs:881-961`), `file_symbol_ids` (`HashMap<String, String>`) is constructed *per file* as `WorkItem::ParsedSymbols` arrives and dropped at the end of that file's loop iteration.
   - At lines 953–961, `called_names` are looked up exclusively in `file_symbol_ids.get(c)`:
     ```rust
     let depends_on: Vec<String> = called_names
         .iter()
         .filter_map(|c| file_symbol_ids.get(c))
         .filter(|target_id| *target_id != &sym_id)
         .cloned()
         .collect();
     ```
   - Meanwhile, file-level import resolution (`dependency_map: HashMap<String, Vec<String>>` produced by `resolve_dependency_paths`) is only returned when the producer completes (`src/parser/ingest.rs:1285-1292`) and stamped onto *file* nodes at lines 1476–1501 (`"depends_on"` and `"imports"`).
   - Because `run_consumer` never sees `dependency_map` or other files' symbols, any function or method call targeting a symbol defined in **another file** is silently dropped. Consequently, `neurostrata_memories_for_file` (`ast_summary.callers` / `callees` / `blast_radius_count`) misses cross-file callers.

2. **Naive `ident(` Byte-Scanner (`src/parser/ingest.rs:174-206`):**
   - `extract_called_identifiers` (`src/parser/ingest.rs:174-206`) scans raw bytes for `[A-Za-z0-9_]+` immediately followed by `(`, losing any preceding `module::` or `receiver.` qualifier and false-firing on `ident(` inside comments or string literals.

3. **No Compiler-Grade Index Ingestion (SCIP):**
   - Tree-sitter alone cannot resolve overloaded methods, generic trait dispatch (`VectorStore::search` -> `LadybugStore::search`), or type-inferred method receivers (`store.search(...)`).
   - The open **SCIP** format (`index.scip` emitted by `rust-analyzer scip .`, `scip-typescript`, `scip-python`, `scip-go`, `scip-java`) solves this deterministically using compiler typecheckers.

## 2. Goals

1. **Tier 1 — Built-In Cross-File Tree-Sitter Call Graph (`src/parser/ingest.rs`):**
   - Upgrade call extraction from raw byte scanning to AST-guided call extraction (`extract_ast_calls`), skipping comments and string literals and preserving optional `qualifier` (`module::func` / `Type::method`) alongside bare `callee_name`.
   - Persist `unresolved_calls: Vec<String>` on staged `code_ast` rows during `run_consumer` (`src/parser/ingest.rs:881-961`).
   - In the pre-publish stamping pass (`src/parser/ingest.rs:1476-1502`)—where **both** newly parsed symbols and carried-forward unchanged symbols (`lines 1363-1474`) are present in `generation` and `dependency_map` (`src_file -> Vec<imported_file>`) is available—build a `WorkspaceSymbolTable` and resolve cross-file `calls` (`depends_on`) in priority order:
     1. Same-file symbol match (`file_symbol_ids`, already resolved during `run_consumer`).
     2. Imported-file symbol match (symbols defined in files listed in `dependency_map[caller_file]` or 1-hop `mod.rs` / `index.ts` re-exports).
     3. Unambiguous workspace-wide symbol match (when exactly one symbol in `generation` bears that bare name and it is not in `COMMON_STD_IDENTIFIERS`).
2. **Tier 2 — SCIP Compiler-Grade Index Ingestion (`src/parser/scip.rs` + `neurostrata_ingest_scip`):**
   - Evaluate and adopt the official license-compatible `scip` crate (`Apache-2.0`) and/or `prost` (`Apache-2.0`) per Rule `227984e4-780c-4fe4-a44c-286d7d14eabf` to parse standard `index.scip` protobuf files rather than hand-rolling a protobuf wire decoder.
   - Map SCIP `Document`s and `SymbolInformation` definitions onto NeuroStrata's generation-scoped `file` and `code_ast` nodes, storing the canonical SCIP moniker in `metadata.scip_symbol` and signature documentation in `content`.
   - Map SCIP `Occurrence`s (`SymbolRoles::Definition`, references) and `Relationship`s (`is_implementation`, `is_reference`) into exact cross-file `DEPENDS_ON` edges (`calls`, `implements`, `references`) with `edge_sources` provenance (`extractor: "scip"`, exact line/column span).
3. **Preserve Generation Staging, Caller-Path Security, and Mesh Isolation:**
   - Reuse the atomic two-phase `IngestGeneration` staging protocol (`stage_generation` -> `publish_generation` in `src/store/ladybug.rs`).
   - Enforce `check_caller_path` (`src/daemon.rs`) on all `/ingest-scip` paths (`neurostrata-p1ex` J6 rule).
   - Keep all SCIP/AST rows `memory_type: "code_ast" | "file" | "directory"` so they **never** tee into the SynapticMesh outbox (Rule `460d2cc7-0e73-4af9-a052-8aaef57e4c0a`).

## 3. Non-Goals

- Embedding a live LSP server inside the `neurostrata-mcp` daemon. SCIP ingestion reads pre-built `index.scip` files.
- Indexing external third-party crate/package internals outside the repository workspace (external SCIP monikers are recorded as lightweight `metadata.external_references: Vec<String>` on the calling symbol node rather than inflating the graph with `std::` or `tokio::` nodes).

## 4. Architecture

### 4.1 Tier 1: Pre-Publish `WorkspaceSymbolTable` Pass in `src/parser/ingest.rs`

1. **AST-Guided Call Extraction (`extract_ast_calls` replacing `extract_called_identifiers` at `src/parser/ingest.rs:174-206`):**
   - Walk the symbol's Tree-sitter subtree (or mask out `comment`, `line_comment`, `block_comment`, `string_literal`, `raw_string_literal` byte ranges before scanning) so comments and strings never emit false calls.
   - Capture optional qualifier + identifier (`qualifier::ident(` or `ident(`).

2. **Pre-Publish Cross-File Symbol Linker (`src/parser/ingest.rs:1476-1502`):**
   - Right after the file-level `dependency_map` stamping loop (`src/parser/ingest.rs:1477-1501`) and **before** `publish_generation` (`line 1509`):
   - Build `WorkspaceSymbolTable` across all `code_ast` rows in `generation` (both newly parsed in `run_consumer` and carried forward at `lines 1363-1474`):
     ```rust
     pub struct WorkspaceSymbolTable {
         /// file_rel_path -> (symbol_name -> scoped_sym_node_id)
         pub by_file: HashMap<String, HashMap<String, String>>,
         /// symbol_name -> Vec<(file_rel_path, scoped_sym_node_id)>
         pub by_bare_name: HashMap<String, Vec<(String, String)>>,
     }
     ```
   - For each `code_ast` row in `generation` that has non-empty `unresolved_calls`:
     - Resolve each `(qualifier, callee_name)` against:
       1. `dependency_map.get(&caller_file)` imported target files (`by_file[imp_file].get(callee_name)`), including 1-hop `mod.rs`/`lib.rs`/`index.ts` re-exports.
       2. `by_bare_name.get(callee_name)` when `len() == 1`, `callee_name.len() >= 4`, and `!COMMON_STD_IDENTIFIERS.contains(callee_name)`.
     - Append resolved cross-file symbol IDs to `metadata.depends_on` (`calls`) and re-upsert the staged `code_ast` row before `publish_generation`.

### 4.2 Tier 2: SCIP Protobuf Reader in `src/parser/scip.rs`

1. **Library Reuse & Dependency Selection (Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`):**
   - Add the official `scip` crate (`Apache-2.0`) and/or `prost = "0.13"` (`Apache-2.0`) to `Cargo.toml` to decode `scip.proto` messages (`Index`, `Metadata`, `Document`, `SymbolInformation`, `Relationship`, `Occurrence`) and parse SCIP symbol monikers directly rather than writing a bespoke protobuf wire decoder.
2. **SCIP Moniker to NeuroStrata Node Mapping:**
   - For each `Document` (`relative_path`, `occurrences`, `symbols`):
     - Reject paths escaping `project_root` (`..`, absolute paths, or symlinks).
     - Match or create the generation-scoped `file` node (`NeuroStrata::<gen>::<relative_path>`).
     - For each `Occurrence` with `(symbol_roles & SymbolRoles::Definition) != 0`:
       - Skip local variables (`symbol.starts_with("local ")`).
       - Derive symbol kind & human-readable display name from the SCIP descriptor suffix (`.` = `functions`/`methods`, `#` = `structs`/`types`/`interfaces`, `/` = module).
       - Match to the corresponding `code_ast` node in the same file (by `(relative_path, bare_name, enclosing_line_range)`) or mint a deterministic `code_ast` node if Tree-sitter did not capture it.
       - Attach `metadata.scip_symbol = occurrence.symbol` and `metadata.scip_documentation = symbol_info.documentation`.
   - **Reference & Implementation Edges:**
     - For each non-definition `Occurrence` inside enclosing definition symbol $S_{\text{caller}}$ referencing workspace SCIP symbol $S_{\text{target}}$:
       - Add $S_{\text{target}}$ to $S_{\text{caller}}$'s `metadata.depends_on` (`calls` / `references`) and record `edge_sources` with `extractor: "scip"` and exact 1-based line `occurrence.range[0] + 1`.
     - For each `Relationship` (`is_implementation: true`) on $S_{\text{impl}}$ pointing to trait/interface $S_{\text{trait}}$:
       - Emit `DEPENDS_ON` edge with `logical_target: S_trait` and `rel_subtype: "implements"`, plus populate `metadata.implements: [S_trait]`.

### 4.3 Surface Additions (`src/server.rs`, `src/daemon.rs`, `src/main.rs`)

- **MCP Tool:** `neurostrata_ingest_scip({ namespace, project_root, scip_path?: string })` (defaults `scip_path` to `<project_root>/index.scip`).
- **Daemon Endpoint:** `POST /ingest-scip` gated by `check_caller_path` on both `project_root` and `scip_path`.
- **CLI Subcommand:** `neurostrata-mcp ingest-scip <path> [--scip-file <index.scip>] [--namespace <ns>]`.

## 5. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| False-positive edges in Tier 1 from common function names (`new`, `default`, `from`, `fmt`, `clone`) | Exclude `COMMON_STD_IDENTIFIERS` from unambiguous workspace fallback; require same-file or explicit `dependency_map` match for short/common method names unless Tier 2 SCIP is used. |
| Incremental ingest carry-forward missing cross-file targets | Running the Tier 1 cross-file linker at `src/parser/ingest.rs:1476-1502` (after carry-forward at lines `1363-1474`) guarantees both modified and unchanged carried-forward symbols are present in `WorkspaceSymbolTable`. |
| Arbitrary file read via malicious `scip_path` or `Document.relative_path` | Enforce `check_caller_path` on `scip_path` and `project_root`; canonicalize and verify every `Document.relative_path` stays strictly inside `project_root`. |

## 6. Checkable Acceptance Criteria

1. **Tier 1 Cross-File Call Resolution:** In a multi-file fixture where `src/a.rs` imports `src/b.rs` (`dependency_map["src/a.rs"]` contains `"src/b.rs"`) and `fn caller()` in `src/a.rs` calls `helper_fn()` defined in `src/b.rs`, `ingest_directory` creates a `DEPENDS_ON` edge from `caller` (`src/a.rs`) to `helper_fn` (`src/b.rs`), and `neurostrata_memories_for_file` on `src/b.rs` reports `caller` in `ast_summary.callers` with `blast_radius_count >= 1`.
2. **Tier 1 Comment/String Immunity:** A comment or string literal containing `helper_fn()` in `src/c.rs` produces zero `DEPENDS_ON` edges.
3. **Tier 2 SCIP Ingestion:** Ingesting a valid `index.scip` fixture populates `metadata.scip_symbol` on workspace `code_ast` nodes, resolves cross-file trait/method calls (`VectorStore::search` -> `LadybugStore::search`), and records `extractor: "scip"` in `metadata.edge_sources`.
4. **Caller-Path Security:** `/ingest-scip` rejects any `project_root` or `scip_path` outside `startup_root` / `allowed_external_dirs` via `check_caller_path`, and rejects symlinks or `..` traversal in `Document.relative_path`.

## 7. Next Step

On approval of this spec, invoke `writing-plans` for Task `neurostrata-rbde` to produce the step-by-step TDD implementation plan.
