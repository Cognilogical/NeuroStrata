# Obsidian & NeuroVault Visual Memory UI

> **Why you care:** Your AI's memory is never trapped in an opaque binary blob — NeuroStrata synchronizes memories bidirectionally with local Markdown vaults and generates interactive JSON Canvas spatial maps out of the box.

---

## 1. Bidirectional Obsidian Vault Synchronization

NeuroStrata treats an Obsidian vault (or any Markdown directory) as a live mirror of your cognitive graph via the `neurostrata_sync_obsidian` MCP tool (`src/obsidian.rs`).

### How Synchronization Works
1. **Database → Vault (Export):** Active memories in a namespace are rendered as clean Markdown notes with structured YAML frontmatter and `[[WikiLinks]]` representing `RELATES_TO`, `GOVERNS`, and `DEPENDS_ON` graph edges.
2. **Vault → Database (Import):** Edits made to Markdown notes inside Obsidian are parsed, diffed against the stored node's content and frontmatter, re-embedded via the Dendritic Bridge, and committed back into LadybugDB while preserving bi-temporal audit history.

```json
{
  "name": "neurostrata_sync_obsidian",
  "arguments": {
    "namespace": "NeuroStrata",
    "vault_path": "/home/user/Obsidian/NeuroVault"
  }
}
```

---

## 2. Markdown Note & Frontmatter Schema

Every synchronized note carries deterministic YAML frontmatter so both humans and graph tools can inspect provenance, temporal validity, and structural anchors:

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

* **Bi-Temporal Safety:** Deleting or superseding a rule updates `valid_to` and `superseded_by` rather than destroying historical provenance.
* **Graph Edges as WikiLinks:** Outbound `RELATES_TO` and `GOVERNS` edges appear as native Obsidian links so Obsidian's local Graph View automatically mirrors your namespace topology.

---

## 3. JSON Canvas Spatial Topology (`*.canvas`)

In addition to individual Markdown notes, NeuroStrata exports Open JSON Canvas (`1.0`) layouts (`neurostrata_generate_canvas` / `export-graph`) that group nodes spatially by:
- **Tri-Strata Tier:** Global mandates, Domain architecture rules, and Task extractions.
- **Leiden Communities:** Automatically clustered thematic neighborhoods discovered by the graph community detector (`src/community.rs`).
- **Source File Governance:** Architectural rules positioned directly alongside the AST file/symbol nodes they govern.

See the [Visual Interface & UI Guide](../UI_GUIDE.md) for using the standalone Web UI and Tauri Desktop 3D Force Graph alongside Obsidian.
