# 🖌️ NeuroStrata Visual Interface & Obsidian Guide

NeuroStrata provides two complementary visual interfaces for exploring, auditing, and curating your AI agent's cognitive graph:
1. **The Embedded Web UI & Tauri Desktop App (`web-ui/` / `src-tauri/`):** An interactive 3D/2D Force Graph, Central Executive Task Board, SynapticMesh Inspector, and SPARQL/Digital-Twin console connected directly to the Rust daemon (`http://127.0.0.1:34343`).
2. **NeuroVault Obsidian Integration — Planned, not implemented:** Bidirectional Markdown note synchronization and JSON Canvas spatial topology export are tracked by task `neurostrata-dlag`; no `src/obsidian.rs` module exists in this release.

---

## 1. Interactive 3D & 2D SynapticGraph Canvas

The Web UI renders your project's exported graph (`.NeuroStrata/graph/graph.json` or live daemon stream) as an interactive spatial network:
- **Tri-Strata & Domain Filtering:** Filter nodes by namespace (`global` vs. project namespaces), memory type (`rule`, `fact`, `procedure`, `task`, `symbol`), or declarative domain shelf.
- **Directed Edge Inspection:** Visualize `GOVERNS`, `DEPENDS_ON`, `RELATES_TO`, `EXTRACTED_FROM`, and Tree-sitter AST (`calls`, `imports`) relationships in real time.
- **Leiden Community Coloring:** Color-code clusters discovered by NeuroStrata's Leiden community detection engine (`src/community.rs`) to spot architectural coupling at a glance.
- **Bi-Temporal Time Slider:** Inspect active memories (`valid_to == 0`) or toggle superseded and archived nodes (`--include-archived`) to audit how architectural rules evolved over time.

### Refreshing the Graph Export
```bash
neurostrata-mcp export-graph
```

---

## 2. Central Executive Task Board & Gate Inspector

The Task Board view surfaces the live state of the Central Executive (`src/task/`):
- **4-State Columns:** View `open`, `in_progress`, `blocked`, and `done` Goals alongside their active session claim leases and Breath history logs.
- **Lock 2 Extraction Links:** Click any `done` task to jump directly to the `rule` or `fact` nodes connected via `EXTRACTED_FROM` edges.
- **Gate Readiness Banner:** Displays the live `neurostrata-mcp task validate <namespace>` status (`violations`, `stale`, `dangling_edges`) so you know before pushing whether `.git/hooks/pre-push` will pass.

---

## 3. SynapticMesh, SPARQL & Digital Twin Panels

- **SynapticMesh Status:** Inspect queued outbox records, peer sync history, and any preserved conflict variants across your team's Git mesh (`docs/mesh/synaptic-mesh.md`).
- **SPARQL 1.1 & SHACL Console:** Run W3C SPARQL queries (`neurostrata_sparql_query`) and inspect SHACL shape conformance reports (`src/schemas/shacl/`).
- **Digital Twin Panel:** Inspect versioned state vectors, compare versions (`neurostrata_twin_diff`), and list the twins of a namespace (`neurostrata_twin_list`).

---

## 4. Obsidian Vault Synchronization (`NeuroVault`)

This feature is **not available in this release**. There is no `neurostrata_sync_obsidian` MCP tool, no `obsidian-sync` CLI subcommand, and no `src/obsidian.rs` module. The bidirectional Markdown sync and JSON Canvas export are planned and tracked by task `neurostrata-dlag`. See [`docs/ops/obsidian.md`](ops/obsidian.md) for the planned design and current status.
