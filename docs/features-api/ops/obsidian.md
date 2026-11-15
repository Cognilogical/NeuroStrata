# Obsidian & NeuroVault — Not Yet Implemented

> **Why you care (intent, not shipped):** The vision is for your AI's memory to be human-curatable rather than trapped in an opaque binary blob. The planned feature would mirror the NeuroStrata cognitive graph bidirectionally with a local Markdown vault and generate interactive JSON Canvas spatial maps. **This feature is not yet implemented.**

---

## 1. Bidirectional Obsidian Vault Synchronization — Planned

NeuroStrata does **not** currently ship an Obsidian / NeuroVault Markdown sync. Specifically, the following do **not** exist in this codebase:

- No `neurostrata_sync_obsidian` MCP tool (it is not registered in `src/server.rs`).
- No `obsidian-sync` CLI subcommand (the `Commands` enum in `src/main.rs` has no such variant).
- No `src/obsidian.rs` module (the file has never existed in any commit).

The name `neurostrata_sync_obsidian` is kept in this document only so that a reader searching the docs for it receives a truthful answer: it is a planned interface, not a callable one.

### Planned Implementation

When built, the bidirectional sync would cover:

1. **Database → Vault (Export):** Active memories in a namespace would be rendered as clean Markdown notes with structured YAML frontmatter and `[[WikiLinks]]` representing `RELATES_TO`, `GOVERNS`, and `DEPENDS_ON` graph edges.
2. **Vault → Database (Import):** Edits made to Markdown notes inside Obsidian would be parsed, diffed against the stored node's content and frontmatter, re-embedded via the Dendritic Bridge, and committed back into LadybugDB while preserving bi-temporal audit history.
3. **JSON Canvas Spatial Topology (`*.canvas`):** Open JSON Canvas (1.0) layouts grouping nodes spatially by Tri-Strata tier, Leiden communities, and source-file governance.

### Tracking

This work is tracked by task **`neurostrata-dlag`** — "Implement Obsidian/NeuroVault sync (or formally retire the claim)". See the task board in the Web UI or run `neurostrata-mcp task validate NeuroStrata` for status.

### What Exists Today

For a human-readable view of the graph today, use:

- The **Web UI** (`web-ui/`) — interactive 3D/2D Force Graph, Task Board, SynapticMesh Inspector, and SPARQL console. See `docs/UI_GUIDE.md`.
- The **graph export** — `neurostrata-mcp export-graph <output_json_path>` writes the memory graph as JSON for offline inspection (the output path is yours to choose). See `docs/ops/cli-reference.md`.

---

## 2. Markdown Note & Frontmatter Schema — Planned

Every synchronized note would carry deterministic YAML frontmatter so both humans and graph tools can inspect provenance, temporal validity, and structural anchors:

```markdown
---
id: "494a50ec-9333-49b8-b318-b7567f23e83f"
namespace: "NeuroStrata"
memory_type: "rule"
valid_from: 1791744000
valid_to: 0
governs:
  - "NeuroStrata::src/server.rs"
  - "NeuroStrata::docs"
related_to:
  - "[[33370220-ee0a-46e2-9d7c-8dcc3082e187]]"
---

# Rule: Identifier Discipline

Always quote commit hashes, memory IDs, and task IDs directly from tool output.
```

* **Bi-Temporal Safety (planned):** Deleting or superseding a rule would update `valid_to` and `superseded_by` rather than destroying historical provenance.
* **Graph Edges as WikiLinks (planned):** Outbound `RELATES_TO` and `GOVERNS` edges would appear as native Obsidian links so Obsidian's local Graph View automatically mirrors your namespace topology.

---

## 3. JSON Canvas Spatial Topology (`*.canvas`) — Planned

In addition to individual Markdown notes, the planned export would produce Open JSON Canvas (`1.0`) layouts (extending the existing `export-graph` command) that group nodes spatially by:

- **Tri-Strata Tier:** Global mandates, Domain architecture rules, and Task extractions.
- **Leiden Communities:** Automatically clustered thematic neighborhoods discovered by the graph community detector (`src/community.rs`).
- **Source File Governance:** Architectural rules positioned directly alongside the AST file/symbol nodes they govern.

See the [Visual Interface & UI Guide](../UI_GUIDE.md) for using the standalone Web UI and Tauri Desktop 3D Force Graph alongside Obsidian.
