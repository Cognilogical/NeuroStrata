---
domain: "neurostrata_installer"
description: "Core installer and infrastructure stack for the 3-Tier NeuroStrata."
governs_paths:
  - "install.sh"
  - "podman-compose.yml"
  - "README.md"
---
# NeuroStrata Installer

## Overview
This is the turnkey infrastructure package that deploys the "Pointer-Wiki Hybrid" 3-Tier memory architecture locally.

## Architecture Constraints
- **Containers:** Strictly uses `podman-compose` (Docker is banned by global rule).
- **Vector Database:** Uses Qdrant for fast, local embedding retrieval (via `neurostrata`).
- **Local Models:** Uses Ollama with `nomic-embed-text` for embeddings and a lightweight model (like `llama3.2:1b`) for local inference to ensure complete privacy and offline capability.
- **Tooling:** Installs `neurostrata-mcp` (Go) for memory persistence, the `neurostrata-plugin` (TypeScript) to enforce NeuroStrata rules against OpenCode context compaction, `graph-engine-cli` (Python) for codebase spatial mapping, and `beads` (Go) for issue tracking.

## Philosophy
To prevent "Context Bloat" (legacy markdown files) and "Fragmented Amnesia" (pure RAG), the installer provisions a system where AI agents store compact pointers in the Vector DB (Tier 1/3) that point to synthesized narrative markdown files in the repository (Tier 2).
