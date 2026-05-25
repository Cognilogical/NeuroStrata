# 📓 NeuroStrata Changelog

All notable changes to the NeuroStrata project will be documented in this file.

---

## [1.3.0] - 2026-05-25

### Added
- **Clap Subcommand CLI Integration**: Integrated `clap` v4 with derive-style command parsing to cleanly structure vector store CLI operations (`daemon`, `namespaces`, `list`, `ingest`, `export-graph`, `delete`, `add`, `edit`).
- **External Plugin Fallback**: Implemented robust prefix matching and cross-platform child subprocess spawning for unrecognized commands, ensuring 100% backward compatibility for external plugin runners.
- **GitHub Issue Synchronizer**: Created a standalone `sync_github_issues.sh` utility to sync outstanding tasks and post comments, with a smart offline/unauthenticated fallback to local markdown files under `docs/github_issues/`.
- **Dolt Beads Tracking**: Created and completed issue tracking beads for Phases 4-8 in the local beads Dolt database.

---

## [1.2.0] - 2026-05-25

### Added
- **Regex Secret Scrubber**: Implemented robust regex-based secret scanning prior to DB insertion.
- **Extracted AST Schema**: Separated AST schemas out to a declarative `schema.json` file for cleaner maintainability.

### Changed
- **Logarithmic Neural Gain**: Replaced linear boost in semantic search with logarithmic scaling to prevent query saturation blindness.
- **Subprocess Spawning**: Transitioned to cross-platform `.status()` execution over Unix-only `.exec()` hijacking.
- **Decoupled Handlers**: Completely refactored server route handling into independent controller functions.
- **Model Configuration Support**: Allowed FastEmbed models to be dynamically instantiated via the `NEUROSTRATA_MODEL` env var.

### Fixed
- **Graph Inlining Defect**: Removed context neighborhood concatenation on direct fetches to solve permanent visualization corruption during DB moves.
- **Ingestor Exclusion**: Corrected extension filter omitting structural graph processing on unspecified languages.

---

## [1.1.1] - 2026-05-25

### Added
- **Asynchronous Neural Gain Updates:** Every successful semantic query asynchronously increments the target memory's `access_count` in Kùzu Graph database in a non-blocking background tokio task.
- **Bi-Temporal Validation Logic:** Automatic runtime filtering of expired memories whose `valid_to` Unix timestamp has passed, avoiding outdated context injection.
- **Cypher-Injection Hardening:** Added robust escaping mechanisms for inputs (`escape_kuzu_string`) to neutralize trailing-backslash (`\\`) and single-quote (`\'`) Cypher-injection vectors.

### Changed
- **Single-Pass AST & Text Walk:** Replaced the legacy double-walker implementation with a single unified, highly optimized directory walker loop to ingest files and AST structures simultaneously, dramatically cutting ingestion times.
- **Improved MCP Error Responses:** Replaced raw `unwrap`s in JSON-RPC serialization with safe propagation, avoiding daemon crashes on invalid payload states.
- **Robust Path Resolution:** Upgraded path extraction in daemon and parser modules to safely handle systems with sparse files or specialized links.

### Fixed
- **Temporal Gate Logic Bug:** Resolved a regression where future-dated valid memories were incorrectly ignored under specific timezones.
- **Ladybug Store Access Counters:** Fixed missing `increment_access_count` trait implementations and integrated it into the main MCP server pipeline.
- **CLI Database Lock Conflict:** Added active check for running daemon port `34343` during CLI invocations, preventing simultaneous database write access crashes on locked Kùzu DB storage.
- **Dead Code Cleanup:** Pruned deprecated and unused mock structures (`start_mcp_server`, `generate_canvas`) to keep codebase clean and optimized.

---

## [1.0.0] - 2026-04-15

### Added
- **Rust Transition (Complete):** Core backend successfully migrated from Go to Rust for memory density, safety, and parallel search performance.
- **Kùzu Graph & Vector Engine:** Integrated local embedded Kùzu graph store, supporting semantic nodes, relations (`governs`, `relates_to`), and dense vector storage.
- **Tantivy FTS Hybrid Search:** Integrated hybrid exact-keyword and vector search using Reciprocal Rank Fusion (RRF).
- **Daemon Mode Proxying:** Implemented a persistent TCP daemon on port `34343` with a lightweight stdio proxy for fast, lock-free editor integration.
