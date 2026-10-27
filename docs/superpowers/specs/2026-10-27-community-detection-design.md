# Community Detection (Louvain/Leiden) Design

> Status: revised per Argon review (blocking gaps fixed) — pending user review.
> Task: `neurostrata-vhx3` (Community detection, OxiRS §1.7).
> Companion: `docs/design-oxirs-concepts.md` §1.7.

## 1. Problem

`get_snapshot` returns a flat top-5 by access count. There is no cluster rollup, so an agent reading a snapshot cannot tell "these 8 memories form a coherent subsystem" nor see the hubs of each cluster. Community detection over the memory graph (GOVERNS/CONTAINS/RELATES_TO/DEPENDS_ON/EXTRACTED_FROM) can produce hierarchical summaries.

## 2. Goal

Run **deterministic Leiden community detection** per namespace over the memory graph; store one summary memory per community (CONTAINS-linked to its members); surface root summary + hubs in `get_snapshot`. No LLM, no second store.

## 3. Constraints

- Deterministic: seeded, fixed iteration count, sorted node order.
- Single store: summary memories are ordinary memory rows in LadybugDB.
- No LLM: summary content is a deterministic concatenation of member titles/locations, not generated prose.
- Feature-gated: `--features community` (default off) to keep the default build lean; or always-on if the dep is light. Recommend feature-gated to match KGE/Tantivy pattern.

## 4. Scope

In scope:
- Leiden algorithm over an undirected, weighted graph where nodes = active-generation memories (including structural connectors: directory/file/symbol/markdown) excluding retired, archived, conflicting, and registry rows; edges = the 5 typed relations (weight 1.0 per edge; multi-edges summed). Structural members are excluded only when forming `Community` summaries, computing `size`, and selecting `min_member_id`/hub.
- Determinism: node order sorted by id; seeded RNG (seed 42); fixed max iterations (10) and resolution (1.0).
- Output: `Vec<Community>` where each has `id` (u32), `member_ids: Vec<String>`, `hub_id: String` (highest access_count, tie-break id), `size: usize`.
- Summary memory per community: memory_type `community_summary`, content = deterministic concatenation of member locations/content first lines, metadata `{community_id, member_count, hub_id, members: [ids]}`, edges: `CONTAINS` from summary to each member.
- Trigger: `neurostrata_rebuild_communities(namespace?)` CLI/tool; also runs during `neurostrata_ingest_directory` (optional, behind flag).
- Snapshot integration: `get_snapshot` includes a "Communities (N)" section listing community id, size, hub, and a one-line member summary, when communities exist.
- Idempotency: rebuild deletes prior `community_summary` rows for the namespace before recreating.

Out of scope:
- Hierarchical (multi-level) communities (v2; flat partition first).
- LLM-generated summaries.
- Cross-namespace communities.

## 5. Approach

### 3.1 Library evaluation

Prefer an existing crate over an in-house implementation:

- `leiden-rs` provides seeded, deterministic Leiden with modularity and CPM quality functions and optional `petgraph` adapters.
- `rust_igraph` is an alternative if broader graph algorithms are needed; it is heavier and should be evaluated against `leiden-rs` before adoption.
- Do not implement Leiden in-house unless a dependency review rejects both candidates.

### 5.1 Feature gate
Cargo feature `community` adds `leiden-rs` (and its `petgraph` adapter if needed). CI must run `cargo test --features community`; default-feature builds remain unaffected but do not compile this module.

### 5.2 Graph build
- Nodes: ALL active-generation memory ids (including structural connectors: directory/file/symbol/markdown) except retired, archived, conflicting, and registry rows. Node eligibility checks `is_active_id`, `is_retired_at(metadata, now)`, `is_archived`, `is_mesh_conflicting`, and `is_registry_row`.
- Edges: undirected, weight = count of typed edges between the pair (any of 5 types). Include only edges whose endpoints are eligible nodes and whose `valid_to == 0` (or `valid_to > now`); skip superseded edges.
- Self-loops ignored.

### 5.2a Canonical community numbering
- After Leiden converges, compute `size` and `min_member_id` over non-structural members only (excluding `STRUCTURAL_MEMORY_TYPES` plus `"symbol"`, `"code_ast"`, and `"community_summary"`).
- Sort communities by `(size DESC, min_member_id ASC)` and assign `community_id = 0..K-1`. Deterministic across runs.
- Snapshot top-10 ordering: `size DESC, community_id ASC`.

### 5.3 Leiden
Standard Leiden: (1) local moving, (2) refinement, (3) aggregation. Resolution γ=1.0. Repeat until no improvement or max 10 iterations. Deterministic node order (sorted ids). Seeded RNG only for tie-breaking in local moving (rare).

### 5.4 Summary memory
For each community with ≥ 2 non-structural members (singletons skipped — no summary):
- id = `<namespace>::community::<community_id>` (stable, no generation scope; survives re-ingest; rebuild deletes by prefix).
- content = non-structural members sorted by id, each contributing first line of content (truncated to 80 chars), joined by "; ".
- metadata as above (member list contains only non-structural ids).
- edges: CONTAINS from summary to each non-structural member.
- Stored with a zero vector and added to `STRUCTURAL_MEMORY_TYPES` in `src/store/ladybug.rs` (array length 5 -> 6). This excludes community summaries from exact-name match, base vector search, dedup, and embedding extraction automatically.
- Write directly through the store WITHOUT emitting `MemoryEvent::Created` on the ThalamicBus, so derived summaries never trigger `mesh_outbox` or `export_freshness` churn (SynapticMesh already rejects `community_summary` via `is_shareable_memory_type`).

### 5.4a Search expansion guard
`search_inner` Step 2 must exclude `memory_type == "community_summary"` from all expansion results and intermediates: `neighbor_out_q`, `neighbor_in_q`, `govern_out_q`, `govern_in_q`, and `govern_in_cont_q` skip community summaries as `b`, `r`, `f`, or `f1`. `fetch_related_subgraph` also excludes `community_summary` unless explicitly requested. This prevents community summaries from being pulled in as 1-hop `CONTAINS` expansions.

### 5.5 Rebuild & idempotency
`neurostrata_rebuild_communities(namespace?)`: in one `write_with_deadline` transaction, execute
`MATCH (m:Memory) WHERE m.namespace = '<safe_ns>' AND m.memory_type = 'community_summary' DETACH DELETE m`,
then run detection, then create summary rows and outgoing `CONTAINS` edges. Report counts. Idempotent.

### 5.5a Outgoing CONTAINS edge materialization
`LadybugStore::upsert` only creates incoming `contained_by` edges. Rebuild must therefore write community summaries through a dedicated store helper that inserts each summary row and then `MERGE (s:Memory {id: $sid})-[:CONTAINS]->(m:Memory {id: $mid})` for every non-structural member (or equivalent inline escaped Cypher following existing `ladybug.rs` conventions). Do not rely on summary metadata alone to materialize outgoing `CONTAINS` edges.

### 5.6 Snapshot integration
In `handle_get_snapshot`, partition `memory_type == "community_summary"` rows out of `all_memories` before `top5_ids`, `domain_strap`, `detect_conflicts`, and `all_memories.truncate(5)`. Then render a "Communities (N):" section (up to 10, ordered `size DESC, community_id ASC`) listing community id, size, hub location snippet, and a one-line member summary. Only if communities exist. No mutation.

### 5.7 Determinism
Sorted node order, seeded RNG, fixed iteration cap. Same graph → same partition. Cross-platform: floating point summation order fixed by sorted iteration.

## 6. Interfaces

- Cargo feature `community` (or always-on if in-house impl is light enough; recommend feature-gated for consistency).
- New module `src/community.rs`.
- Tool/CLI `neurostrata_rebuild_communities`.
- `get_snapshot` reads community_summary rows (no new store query beyond existing memory scan).

## 7. Edge cases

- Feature disabled: no tool, no section, no error.
- < 2 non-structural memories: no communities, no summaries.
- Single community (whole graph): one summary covering all (if size ≥ 2).
- Rebuild with no changes: identical output (idempotent).
- Conflicting/retired rows excluded.

## 8. Testing

- Unit: Leiden on a known fixture (two cliques connected by one edge) yields 2 communities.
- Determinism: same graph → identical partition across runs.
- Integration: rebuild creates summary rows with CONTAINS edges; snapshot includes Communities section.
- Idempotency: rebuild twice → same row count and ids.

## 9. Acceptance

With feature enabled, `neurostrata_rebuild_communities` creates deterministic `community_summary` rows with CONTAINS edges to members, and `get_snapshot` surfaces community id/size/hub. Default build unaffected.

## 10. Files touched (expected)

- `Cargo.toml`: feature `community` (optional).
- New: `src/community.rs` (Leiden + tests).
- `src/server.rs`: tool registration, handler, snapshot section.
- `src/store/ladybug.rs`: delete-by-type helper for rebuild (behind cfg).