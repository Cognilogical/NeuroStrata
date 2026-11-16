---
domain: "neurostrata_installer"
description: "Zero-touch single-binary Rust installer, embedded storage stack, harness plugin auto-wiring, and dual git hooks."
governs_paths:
  - "scripts/install-hooks.sh"
  - "scripts/build.sh"
  - "README.md"
---
# NeuroStrata Zero-Touch Installer & Bootstrap Architecture

## Overview
NeuroStrata ships as a single compiled Rust binary (`neurostrata-mcp`) that embeds the entire cognitive stack in-process and automates project onboarding, harness plugin installation, and Git hook wiring with zero manual configuration.

## Architecture Constraints
- **Single-Binary Embedded Engine:** Compiles **LadybugDB** (property graph + HNSW vector index), **Tantivy** (BM25 lexical search), **FastEmbed** (`NomicEmbedTextV15` local ONNX embeddings), **Tree-sitter** (six-language AST parser: `rust`, `typescript`, `tsx`, `python`, `go`, `java`), and the W3C semantic stack — `oxrdf`/`oxttl` (RDF), `spargebra`/`sparopt`/`spareval`/`sparesults` (SPARQL 1.1/1.2), `shacl` (SHACL Core) — into one executable. (`petgraph` is also a direct dependency, but it backs the in-memory graph type that `leiden-rs` operates on for community detection; the RDFS/OWL closure is computed in-house by `src/rdf/ontology.rs` and `src/rdf/reasoning.rs`, not by a graph library.)
- **Containers:** Any containerized build or sandboxed guard verification strictly uses `podman` and `podman-compose` (Docker is prohibited by global rule).
- **Zero-Touch Harness Plugin Auto-Install (`src/task/harness_plugins.rs`):** Calling `neurostrata_bootstrap`, `neurostrata_task_setup`, or `neurostrata_get_snapshot` automatically detects the active agent harness (`opencode`, `claude`, `codex`, `openclaw`, `hermes`, `github-cli`) and extracts the embedded zero-dependency plugin bundle into `.NeuroStrata/plugins/<harness>/`.
- **Dual Git Hooks (`pre-push` + `pre-commit`):** `neurostrata-mcp hooks install` (and automatic `hook_hygiene` on `get_snapshot`) installs `.git/hooks/pre-push` (running `neurostrata-mcp task gate <namespace> --strict` and project gates) and `.git/hooks/pre-commit` (recording the graph-query verification ledger).

## Philosophy
Developers should never have to manually wire vector databases, write custom Git hooks, or maintain flat markdown TODO lists. A single prompt (*"Initialize NeuroStrata"*) bootstraps the project marker, ingests the AST, installs the harness plugin, and activates the three soft locks automatically.
