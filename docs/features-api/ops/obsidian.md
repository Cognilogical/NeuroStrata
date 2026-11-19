# Obsidian & NeuroVault — Implemented (v1.19.0)

> **Why you care:** Your AI's memory is human-curatable rather than trapped in an opaque binary blob. The bidirectional Vault-LD sync mirrors the NeuroStrata cognitive graph into a local Markdown vault and generates an interactive JSON Canvas spatial map. **This feature ships in v1.19.0.**

---

## 1. Bidirectional Obsidian Vault Synchronization — Shipped

NeuroStrata ships the Vault-LD Markdown sync as of v1.19.0. The surfaces are:

- MCP tool `neurostrata_vault_sync` (registered in `src/server.rs` `tools/list` and dispatch).
- CLI subcommand `neurostrata-mcp vault-sync` (the `Commands` enum in `src/main.rs`).
- HTTP endpoint `POST /vault-sync` (`src/daemon.rs`); `vault_path` / `output_dir` are validated through `check_caller_path` against the daemon startup root and `allowed_external_dirs` before any filesystem access.
- Module `src/vault_ld/` (`mod.rs` + `vault_sync.rs`) — the shared handler `handle_vault_sync` backs all three surfaces.

The name `neurostrata_sync_obsidian` from earlier drafts does not exist; the shipped tool is `neurostrata_vault_sync`.

### Implementation

1. **Database → Vault (Export):** Active memories in a namespace are listed from the store, filtered by ReBAC `can_read_memory` for the caller's `principal`, and rendered as YAML-LD 1.0 Markdown notes (`@context`, `@id`, `@type`, `epistemic_tier`, `canonical_sha256`, `related`) with `[[WikiLinks]]` for `related_to`, `governs`, and `depends_on` edges. The parser-generated edge block is separated from user body text by a `<!-- vault-ld:structural-edges -->` marker, so an author's own standalone `[[WikiLinks]]` in the body survive a round-trip. An Open JSON Canvas 1.0 `topology.canvas` groups the notes spatially.
2. **Vault → Database (Import):** Markdown notes are parsed, secret-scanned (`SecretScanner::scan_str`) and SHACL-validated (`ontobug::validate::shacl_turtle`, `Mode::Native`) BEFORE any store write — violations are collected per-note and the note is skipped, never stored. A write-side ReBAC `can_write_memory` check gates each upsert (imported notes owned by the importing principal); an existing row is additionally refused when it carries mesh provenance not written by this principal, is foreign-owned, or is a private row owned by someone else — mesh sovereignty holds even for admin; a byte-identical re-import is skipped as a no-op before this gate, so an untouched protected row is never touched. The whole import runs under a single process-level write turn: every note is preflighted (validated, gated, embedded) before any write, so an embed failure cannot leave an orphan history row, and a failed head write rolls back its staged history. Content is embedded with the active embedder, so vectors match the configured store dimensions. Unresolved `[[WikiLinks]]` materialize as `stub: true` notes.
3. **JSON Canvas Spatial Topology (`*.canvas`):** Export writes `<output_dir>/topology.canvas` (Open JSON Canvas 1.0) with one node per exported note and edges per WikiLink.

### Tracking

This work is tracked by task **`neurostrata-dlag`** — "Implement Obsidian/NeuroVault sync (or formally retire the claim)". See the task board in the Web UI or run `neurostrata-mcp task validate NeuroStrata` for status.

## 4. Selected Path (A) — W3C YAML-LD 1.0 / Vault-LD Specification

NeuroStrata resolves `neurostrata-dlag` via Path (A): implementing a W3C `YAML-LD 1.0` / Vault-LD Markdown vault sync with Open JSON Canvas `1.0` spatial topology, as specified in [`../../superpowers/specs/2026-11-17-v1-19-0-semantic-graph-and-agentic-reasoning-design.md`](../../superpowers/specs/2026-11-17-v1-19-0-semantic-graph-and-agentic-reasoning-design.md).

### Frontmatter Schema
Every exported Markdown note is simultaneously human-readable Markdown and a W3C `YAML-LD 1.0` document carrying frontmatter fields `@context`, `@id`, `@type`, `epistemic_tier`, `canonical_sha256`, and `[[WikiLinks]]` graph edges.

### Pre-Import Gate
Vault → Database import secret-scans every note (`SecretScanner::scan_str` in `src/secrets.rs`), then runs W3C SHACL-Core validation via `ontobug::validate::shacl_turtle` (`Mode::Native`, checking `report.conforms`) before any store write. A rejection skips that note; nothing is partially stored.

### Bitemporal Supersession
Edits to an existing note's body, epistemic tier or `related` edges change its `canonical_sha256` fingerprint (a hash of content + tier + sorted edges), so even a related-only edit becomes a new version; an unchanged re-import is a no-op. `import_vault` copies the prior row to a `<id>.vault-history.<uuid>` row stamped `valid_to` / `superseded_by`, then writes the replacement head under the original id with `valid_from` / `supersedes`. All notes are preflighted before the write phase and history is written BEFORE its head, so a failed embed or head write cannot leave a head without its history.

### Caller-Path Authority (`J6`)
Both the vault directory and `output_dir` passed to `neurostrata_vault_sync` are canonicalized via `check_caller_path` (`src/daemon.rs`) and must reside inside the project root or `allowed_external_dirs`. The canonical results — never the raw caller strings — are used for every create/read/write, and safe nested `output_dir` paths are created on demand.

### Open JSON Canvas `1.0`
The export writes an Open JSON Canvas `1.0` spatial map (`topology.canvas`) with one column per epistemic tier and curved edges between resolved WikiLink targets. Leiden-community clustering and source-directory governance are design-spec material, not shipped layout.

### What Exists Today

For a human-readable view of the graph today, use:

- The **Vault-LD sync** — `neurostrata-mcp vault-sync <action> <namespace> <vault_path> <output_dir> [principal]` (or `neurostrata_vault_sync` over MCP / `POST /vault-sync` on the daemon) writes one Markdown note per readable memory plus `topology.canvas`.
- The **Web UI** (`web-ui/`) — interactive 3D/2D Force Graph, Task Board, SynapticMesh Inspector, and SPARQL console. See [`../ui-guide.md`](../ui-guide.md).
- The **graph export** — `neurostrata-mcp export-graph <output_json_path>` writes the memory graph as JSON for offline inspection (the output path is yours to choose). See [`../cli-reference.md`](../cli-reference.md).

---

## 2. Markdown Note & Frontmatter Schema — Shipped

Every synchronized note carries deterministic YAML-LD 1.0 frontmatter so both humans and graph tools can inspect provenance, temporal validity, and structural anchors. The shipped shape:

```markdown
---
@context: https://w3id.org/yaml/vocab
@id: "494a50ec-9333-49b8-b318-b7567f23e83f"
@type: "yaml:rule"
epistemic_tier: rule
canonical_sha256: "5f2c…"
related:
  - "[[33370220-ee0a-46e2-9d7c-8dcc3082e187]]"
---

Rule: Identifier Discipline

Always quote commit hashes, memory IDs, and task IDs directly from tool output.

[[33370220-ee0a-46e2-9d7c-8dcc3082e187]]
```

* **Bi-Temporal Safety:** Exports carry a `canonical_sha256` fingerprint of content, epistemic tier and sorted `related` edges, so a body, tier or edge-only edit round-trip is detected as a new bitemporal version rather than an in-place mutation.
* **Graph Edges as WikiLinks:** Outbound `related_to`, `governs`, and `depends_on` edges appear as native Obsidian links so Obsidian's local Graph View automatically mirrors your namespace topology.

---

## 3. JSON Canvas Spatial Topology (`*.canvas`) — Shipped

In addition to individual Markdown notes, the export produces an Open JSON Canvas (`1.0`) map (`topology.canvas`) that groups nodes spatially by:

- **Epistemic Tier:** One column per tier (`rule` red, `fact` blue, `procedure` purple, `structural` green, `task` orange, `symbol` grey), in that order; any unknown/custom tier is retained in a final fallback column rather than dropped.
- **WikiLink Edges:** Curved canvas edges between notes whose frontmatter `related` targets also exist in the vault.

Leiden-community clustering and source-file governance positioning are not part of the shipped layout; they remain design-spec material.

See the [Visual Interface & UI Guide](../ui-guide.md) for using the standalone Web UI and Tauri Desktop 3D Force Graph alongside Obsidian.
