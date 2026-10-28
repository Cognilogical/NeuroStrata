# Import / Module Dependency Extraction Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-6el3` (Dependency edges: extract imports/module dependencies into graph, P2, ranked #7, M-L effort).

## 1. Problem

`related` traversal relies on `GOVERNS` / `CONTAINS` / `RELATES_TO` / `DEPENDS_ON` / `EXTRACTED_FROM` edges. There is no mechanism to extract module-level import/dependency relationships from source code and project them into the graph. Without this, dependency-related queries are blind.

## 2. Goal

Extract source-file import/module dependency relationships and project them into the memory graph as `DEPENDS_ON` edges, transient, not materialized in LadybugDB.

## 3. Constraints

- Extract from source files (Python `import`, Rust `use`, TypeScript `import`, etc.) — not from execution.
- No LLM in extraction path.
- Library hygiene: prefer `syn` (Rust parser) / `tree-sitter` (multi-language) only where needed; avoid `oxirs-core`; use `oxrdf` for projection if RDF layer active; otherwise `std` + file-level parsing.
- Deterministic: sorted dependency list, stable IRI mapping.
- Warn-first: unsupported file types or parse errors reported, not fatal.

## 4. Scope

- Source dependency parsing (file-level).
- Projection to `DEPENDS_ON` edges with `extractor`, `source`, `generation`.
- Storage in `src/store/ladybug.rs` read-time projection.
- Integration with `relink_edges` / `staged_publication`.

## 5. Approach

- `syn` (Rust parser) or `tree-sitter` (multi-language parser) per language.
- Project to `DEPENDS_ON` edges via `oxrdf` dataset if RDF layer active; otherwise direct `LadybugStore` edge creation.
- Bound to namespace per `project_federated_sparql_dataset` patterns.
- Deterministic: sort dependencies by module IRI, stable ordering.

## 6. Interfaces / Files Touched (expected)

- `src/dependency/parser.rs` (new): multi-language import extraction using `syn` (Rust) / `tree-sitter` (Python/TS/JS) / `pylsp` Python language server protocol.
- `src/store/ladybug.rs`: dependency edge write hooks, `upsert` with `DEPENDS_ON` schema.
- `Cargo.toml`: add `tree-sitter = "0.18"` for multi-language parsing, or `syn = "2.0"` if Rust-only; no `oxirs-core` dependency.