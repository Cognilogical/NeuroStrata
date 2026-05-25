# Roundtable Review Report: NeuroStrata
**Adversarial Audit & Code Quality Assessment**
**Review Date:** May 21, 2026
**Overall Score:** 5.8 / 10 | **Security Score:** 4.2 / 10 | **Correctness Score:** 5.5 / 10

---

## 1. Executive Summary

This report presents the findings of the **Roundtable Multi-Agent Adversarial Review** conducted on the `NeuroStrata` codebase. The review followed the 15-phase Overseer protocol, utilizing a diverse panel of parallel, specialized AI subagents representing security, correctness, architecture, and code quality disciplines.

The review targeted the core vector database persistence layer (`store/ladybug.rs`), the AST ingestion pipeline (`parser/ingest.rs`), the tokio/axum daemon endpoint handlers (`daemon.rs`), the main MCP command router (`main.rs`), and the standard MCP server proxy (`server.rs`).

While the codebase exhibits highly sophisticated design choices—including temporal graph partitioning, hybrid GraphRAG semantic blast radius expansion, and Tree-sitter multi-language symbol extraction—it suffers from critical **P0** and **P1** vulnerabilities and logical composition defects that undermine runtime stability, search quality, and security boundaries.

---

## 2. Core Score Breakdown

| Dimension | Score | Assessment |
| :--- | :--- | :--- |
| **Security** | 4.2 / 10 | **Critical Vulnerability.** Manual escaping fails to escape backslashes first, enabling trivial Cypher injection. Hardcoded secret scrubbing filters are easily bypassed. |
| **Correctness** | 5.5 / 10 | **Logic Integrity Defects.** Appending neighborhood context directly to retrieved strings causes recursive DB pollution during move commands. Linear access frequency decays cause semantic search blindness. |
| **Architecture** | 6.5 / 10 | **Solid Foundation.** Strong temporal graph model, but suffers from decoupling gaps where AST walk nodes are mixed directly with high-level agent rules. |
| **Code Quality** | 7.0 / 10 | **Clean, Idiomatic Rust.** However, suffers from performance-killing double directory walks, un-indexed JSON parsing in key search paths, and brittle path unwraps. |

---

## 3. Adjudicated Findings

### [P0] Cypher Injection via Backslash Escaping Bypass
*   **File:** [ladybug.rs](file:///Volumes/dev/Git-SCM/NeuroStrata/src/store/ladybug.rs#L106-L124)
*   **Description:** The database layer's string escaping routine only replaces single quotes `'` with `\\'`. If an input contains `\'`, the sanitization replaces `'` with `\\'`, producing `\\\'`. Under LadybugDB query parser, this evaluates to an escaped backslash followed by an **unescaped single quote**, breaking the string literal. Since `NeuroStrata` is an auto-ingestion engine, a user or malicious actor can place a crafted source file in a directory being indexed, executing administrative commands or crashing the database.
*   **Remediation:** Escape backslashes first (replace `\` with `\\` before replacing `'` with `\\'`), or implement parameterized queries (prepared statements).

### [P0] Recursive Neighborhood-Inlining Composition Bug
*   **Files:** [ladybug.rs](file:///Volumes/dev/Git-SCM/NeuroStrata/src/store/ladybug.rs#L356-L385) & [server.rs](file:///Volumes/dev/Git-SCM/NeuroStrata/src/server.rs#L452-L466)
*   **Description:** For display visualization, `LadybugStore::get` appends 1-hop neighborhood context directly to the payload's `content` string. However, `neurostrata_move_memory` in `server.rs` implements move by pulling the node via `get` and saving it back via `upsert`. This permanently serializes the formatted visualization text as the node's permanent database content. Multiple moves recursively duplicate and nest these neighborhood logs, completely destroying semantic vector matching.
*   **Remediation:** Keep database retrieval raw. Perform display formatting exclusively in the presentation/MCP layer, or introduce a separate `get_enriched()` function.

### [P1] Linear Neural Gain Saturation ("Semantic Blindness")
*   **File:** [ladybug.rs](file:///Volumes/dev/Git-SCM/NeuroStrata/src/store/ladybug.rs#L188-L190)
*   **Description:** Access frequency boost is calculated via a linear subtraction: `distance - (access_count * 0.05)`. Since cosine vector distance lies in `[0.0, 2.0]`, a memory accessed 40 times is given a negative score, placing it permanently at the top of all search results. Highly accessed memories completely crowd out semantically matching nodes, causing "Semantic Blindness."
*   **Remediation:** Replace the linear formula with a bounded logarithmic or sigmoid decay boost.

### [P1] Process Image Hijacking via Unix `exec()`
*   **File:** [main.rs](file:///Volumes/dev/Git-SCM/NeuroStrata/src/main.rs#L206-L212)
*   **Description:** Unrecognized commands are assumed to be external plugins and executed under Unix using `.exec()`. This replaces the current process image entirely. The parent process (e.g. Tauri GUI wrapper) is hijacked and terminated upon plugin exit, diverging from the Windows implementation which spawns it as a standard child subprocess.
*   **Remediation:** Replace `.exec()` with standard cross-platform child process spawning and status waiting.

---

## 4. Completeness & Performance Audit Findings

### [P2] Ingestor Exclusion Gap for Non-Standard Extensions
*   **File:** [ingest.rs](file:///Volumes/dev/Git-SCM/NeuroStrata/src/parser/ingest.rs#L95-L101)
*   **Description:** The directory walker ignores any file not ending in `.md`, `.rs`, `.ts`, or `.tsx`. Crucial configuration items (`Cargo.toml`, `plasticity.json`) or Go, Python, and C++ source files are completely omitted from the structural graph walk, breaking AST graph relationship lineages.
*   **Remediation:** Ingest all non-binary files as structural file nodes, and filter extensions strictly in the AST symbol extractor step.

### [P2] Inefficient Double Directory-Walk
*   **File:** [ingest.rs](file:///Volumes/dev/Git-SCM/NeuroStrata/src/parser/ingest.rs#L29-L136)
*   **Description:** The ingestion script creates two separate `WalkBuilder` instances, traversing the host filesystem twice. Consolidated walk would double I/O efficiency on large repos.
*   **Remediation:** Consolidate structural node insertion and AST symbol extraction into a single directory loop.

---

## 5. Prioritized Action Plan

| Rank | Vulnerability | Severity | Action Item |
| :--- | :--- | :--- | :--- |
| **1** | SQL/Cypher Injection | **Critical (P0)** | Refactor `LadybugStore::upsert` string sanitization to escape backslashes first, or migrate to prepared statements. |
| **2** | Neighborhood Inlining | **Critical (P0)** | Separate visualization side-effects from primary `store.get()` database retrieval. |
| **3** | Linear Neural Gain | **High (P1)** | Refactor distance boosting in `LadybugStore::search` to use logarithmic decay. |
| **4** | Unix Process Hijacking | **High (P1)** | Refactor unrecognized command handler in `main.rs` to spawn subprocesses across all platforms. |
| **5** | Double directory walks | **Medium (P2)** | Refactor `parser/ingest.rs` to merge directory traversals. |
| **6** | Ingestion Exclusion Gap | **Medium (P2)** | Allow non-standard code files to be indexed as structural graph elements. |

