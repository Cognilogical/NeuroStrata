# Knowledge-Graph Embeddings (Structural Channel) Design

> Status: revised per Argon review (blocking defects fixed) — pending user review.
> Task: `neurostrata-84k2` (KGE structural channel, OxiRS §1.6).
> Companion: `docs/design-oxirs-concepts.md` §1.6, `docs/design-cognee-concepts.md`.

## 1. Problem

Retrieval uses text embeddings only; graph topology (GOVERNS/CONTAINS/RELATES_TO/DEPENDS_ON/EXTRACTED_FROM) never feeds similarity. Duplicate judgment is a stub (`LayaOnnxProvider` returns `Unavailable`, never 0.75). There is no local, deterministic structural signal for "these two memories are topologically close" or "these are duplicate candidates."

## 2. Goal

Add an **optional structural embedding channel** computed from the memory graph (TransE over typed edges) that:
- feeds a capped secondary term into retrieval score (deterministic, no LLM),
- produces duplicate-candidate rankings (replacing the Laya stub's role),
- is trained offline and stored as a derived artifact (rebuildable).

LadybugDB remains the sole store; embeddings are derived artifacts.

## 3. Constraints

- No LLM in search/dedup/guard/routing. KGE is a learned linear model, not an LLM; allowed as optional secondary signal.
- Deterministic inference: given trained weights, output is deterministic.
- Single store: embeddings stored as a derived file (`<db_path_parent>/kge/<scope>/`), rebuildable from LadybugDB.
- Feature-gated: `--features kge` (default off).
- Cap: structural term contributes at most 15% of the distance (cannot override text similarity), following the usage-boost capping pattern (`(gain).min(0.5 * distance)`).
- Trainable offline only; no online gradient updates during request handling.
- Laya stub remains retired (returns `Unavailable`); KGE does not resurrect it.

## 4. Scope

In scope:
- TransE model over 5 edge types (GOVERNS, CONTAINS, RELATES_TO, DEPENDS_ON, EXTRACTED_FROM), treating (head, relation, tail) triples.
- Training: offline batch (`neurostrata-mcp kge train`), deterministic seed, fixed epochs, SGD. Reads triples from LadybugDB.
- Inference: entity embeddings loaded at daemon start (if present); topological distance `||E[a] - E[b]||_2`.
- Retrieval integration: capped structural term added to `SearchResult.score` (distance convention: lower better).
- Duplicate candidates: `neurostrata_find_duplicate_candidates` returns ranked pairs for review (never auto-merges).
- Artifact: `<db_path_parent>/kge/<scope>/{model.bin, meta.json}` with version + trained_at + triple_count + scope.

Out of scope:
- ComplEx/RotatE/GraphSAGE (v2; TransE first).
- Online training / incremental updates (retrain on demand).
- Replacing text embeddings.
- Auto-merge of duplicates (review-only).

## 5. Approach

### 5.1 Feature gate
Cargo feature `kge` adds deps `ndarray`, `rand` (with fixed seed). Default off.

### 5.2 Triple extraction
From LadybugDB, for each non-retired, non-archived, non-conflicting memory pair connected by one of the 5 edge types in the active generation:
- emit (head_id, relation, tail_id).
- filter out `valid_to > 0` (superseded) edges? No — include active edges only (`valid_to == 0`); skip superseded.
- exclude registry rows and structural-only endpoints? Include all memory ids that are entities (any non-archived memory).
- relation IDs 0..4 in the order of `EDGE_PROVENANCE_TABLES` (ladybug.rs:1817).
- Entity IDs are memory ids (string → index map).

### 5.3 Model
TransE: entity embeddings E ∈ R^(N×d), relation embeddings R ∈ R^(5×d), d=64 default. Training minimizes margin ranking loss with negative sampling (corrupt tail). Topological distance for inference = `||E[a] - E[b]||_2` (entity closeness), NOT link-prediction error. Deterministic: fixed seed (42), fixed epoch count (50), fixed batch size (128), sorted triple order.

### 5.4 Training CLI
`neurostrata-mcp kge train [--namespace <ns> | --all] [--epochs N] [--dim D]`.
- `--namespace <ns>`: train on that namespace's subgraph; artifact at `<db_path_parent>/kge/<ns>/`.
- `--all`: train on the union of all namespaces' subgraphs; artifact at `<db_path_parent>/kge/global/`.
- Writes artifact. Reports final loss + triple count + entity count.
- Idempotent: re-running overwrites.
- Refuses if < 10 triples ("insufficient triples").

### 5.5 Inference & loading
At daemon start (if artifact present and version matches), load E for the namespace's scope into memory (Arc). Scope lookup order: `<db_path_parent>/kge/<ns>/` first, then `<db_path_parent>/kge/global/`. If neither present or version mismatch, channel disabled (no error, log warning). `structural_distance(a, b)` = `||E[a] - E[b]||_2` if both known, else `f32::INFINITY`.

### 5.6 Retrieval integration
In `search_inner`, the query has no entity id. Define the query anchor as the top-1 primary text match that is a known entity (if any). If no primary match is a known entity, skip the structural term entirely (text-only).

When active:
- Fetch a larger candidate pool: `limit * 4` instead of `limit` (disable early break at `stop_count >= limit`), so blended distances can be re-sorted before truncation.
- For each candidate that is a known entity and has a query anchor, compute `normalized = min(1.0, structural_distance(candidate, anchor) / STRUCTURAL_SCALE)` where STRUCTURAL_SCALE = 95th percentile distance from training (stored in meta.json). The anchor itself receives the full structural discount (intended: it cements the top text match at rank 1).
- `structural_gain = 0.15 * (1.0 - normalized)` (higher when closer).
- Cap: `structural_gain_capped = structural_gain.min(0.15 * distance)` (≤15% of distance, matching usage-boost cap pattern).
- `boosted_distance = distance - access_capped_gain - structural_gain_capped`.
- Sort candidates `ASC` by boosted_distance, then truncate to `limit`.

### 5.7 Duplicate candidates
Tool `neurostrata_find_duplicate_candidates(namespace, limit)`:
- Gather all non-structural, non-task, non-retired, non-archived, non-conflicting memories in namespace.
- If count > 500, sort ids and take first 500 deterministically (documented cap).
- For each pair (i, j) with i < j (sorted by id), compute `dup_score = 0.5 * text_cosine_similarity + 0.5 * (1.0 - normalized_structural_distance)` (higher better; normalized_structural_distance = 1.0 if either unknown). Return top-K by dup_score descending, then id ascending.
- Never auto-merge; review-only.
- If KGE not loaded, text-only fallback (structural term = 0.5 * 1.0 = 0.5 contribution? No: `normalized_structural_distance = 1.0` → structural contribution 0.0, so dup_score = 0.5 * text_sim). Document.

### 5.8 Determinism
Fixed seed (42), sorted triple iteration, fixed epoch/batch. Same input → same artifact (bitwise) given same BLAS. Cross-platform bitwise reproducibility is best-effort (floating point).

## 6. Interfaces

- Cargo feature `kge`.
- New module `src/kge.rs` behind cfg.
- CLI subcommand `neurostrata-mcp kge train`.
- Tool `neurostrata_find_duplicate_candidates`.
- Config: `kge_enabled: bool` (default false), `kge_weight: f32` (default 0.15, clamped [0,0.3]).

## 7. Edge cases

- Feature disabled: no code, no tool, clear message.
- No artifact: channel disabled, no error, text-only.
- Version mismatch: disable channel, log warning, suggest retrain.
- Unknown entity (new memory): structural distance infinity → no contribution (text-only for that pair).
- Empty graph (< 10 triples): training refuses with error "insufficient triples."
- Single namespace with no edges: no contribution.
- Superseded edges excluded (valid_to > 0).

## 8. Testing

- Unit: training on a tiny fixture graph converges (loss decreases); inference deterministic (same seed → same weights).
- Integration: `neurostrata_find_duplicate_candidates` returns ranked pairs; with KGE disabled, text-only fallback.
- Performance: training 10k triples × 50 epochs under 30s; inference per pair < 1ms; search with 4x candidate pool under 2x latency of default.

## 9. Acceptance

With `--features kge` and a trained artifact, retrieval gains a capped structural term (≤15%), and `neurostrata_find_duplicate_candidates` returns ranked review-only pairs. Default build unaffected. Laya stub remains retired (returns Unavailable) — KGE does not resurrect it.

## 10. Files touched (expected)

- `Cargo.toml`: feature `kge`, optional deps.
- New: `src/kge.rs` (model + train + infer).
- `src/main.rs`: CLI subcommand, daemon loading.
- `src/server.rs`: tool registration, handler.
- `src/store/ladybug.rs`: triple extraction helper (behind cfg), candidate pool expansion.
- `.gitignore`: `<db_path_parent>/kge/`.