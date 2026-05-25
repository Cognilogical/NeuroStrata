# Agent Review Panel Report: NeuroStrata

**Work reviewed:** `src/*.rs` in NeuroStrata
**Date:** 2026-04-27
**Panel:** 4 reviewers (Correctness Hawk, Architecture Critic, Security Auditor, Devil's Advocate) + Judge (Antigravity)
**Verdict:** The `NeuroStrata` MCP Server is functionally powerful and elegantly leverages traits for abstraction (`VectorStore`, `Embedder`). However, it suffers from a monolithic server handler and weak security checks that need immediate refactoring before scaling.

---

## 1. Executive Summary

The panel reviewed the core Rust backend of `NeuroStrata`, focusing on `main.rs`, `server.rs`, `traits.rs`, and `embed.rs`. While the architectural foundation (traits and embedded graph generation) is solid, the MCP server implementation is tightly coupled and difficult to test. The security mechanisms for scrubbing secrets are rudimentary and prone to false negatives and positives.

## 2. Consensus Points

* **Strong Abstractions:** The `VectorStore` and `Embedder` traits in `traits.rs` provide an excellent foundation for swapping backends (e.g., local FastEmbed vs cloud OpenAI).
* **Feature Richness:** The ability to dynamically generate Obsidian Canvas files and ingest ASTs natively via MCP is a very powerful capability for developer agents.

## 3. Persona Findings (Prioritized Action Items)

### Correctness Hawk (Focus: Bugs & Logic)
* **[P1] Hardcoded Model Selection:** In `embed.rs:88`, `FastEmbedder::new()` hardcodes the selected model to `acceptable_models[0]`. Even if the user specifies multiple valid models in `embedders.json`, only the first one is ever used.
* **[P2] Deep Nesting:** The tool handler in `server.rs` uses deeply nested `if let` and `match` blocks (over 7 levels deep in `neurostrata_add_memory`), making it prone to shadowing bugs and difficult to read.

### Architecture Critic (Focus: Design & Patterns)
* **[P0] Monolithic Server Handler:** `server.rs` contains an 800+ line `start_mcp_server` function. The `match` statement for tool execution handles everything inline, including complex Obsidian Canvas generation logic.
  * *Recommendation:* Extract tool handlers into separate functions (e.g., `handle_generate_canvas(arguments, store)`).
* **[P2] Hardcoded Schemas:** `main.rs:76-104` contains a hardcoded JSON AST parser schema. This should be extracted to a separate `schema.json` file embedded via `include_str!` or loaded from the config directory.

### Security Auditor (Focus: Vulnerabilities)
* **[P0] Naive Secret Scrubber:** The secret scrubber in `server.rs:204` uses basic substring matching (`content_lower.contains("api_key=")`, etc.).
  * *Vulnerability:* This is trivially bypassed by formatting changes (e.g., `api_key = "secret"` or `apiKey:`).
  * *False Positives:* It will block legitimate architectural rules such as "Never commit an api_key= value to the repo."
  * *Recommendation:* Implement proper regex-based entropy scanning (like `trufflehog`) or rely on external secret scanning tools before ingestion.

### Devil's Advocate (Focus: Alternatives)
* **[P2] CLI vs Server duality:** `main.rs` tries to be both a CLI tool and a long-running JSON-RPC MCP server. The CLI commands (`export-graph`, `ingest`, `delete`) share initialization logic with the MCP server but operate fundamentally differently. Consider splitting them into subcommands cleanly using `clap`.

---
**Prepared by:** Antigravity (Acting Judge)

