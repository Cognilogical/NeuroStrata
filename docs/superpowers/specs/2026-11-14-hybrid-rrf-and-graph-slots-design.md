# Design: True Hybrid RRF Search Fusion, Code-Aware Tantivy Indexing & Graph-Expansion Slot Reservation

**Date:** 2026-11-14
> Status: DRAFT — tracked under Epic `neurostrata-mtqx`, Feature Task `neurostrata-5xjo` (Spec Task `neurostrata-sttv`).
**Approach:** True Multi-Retriever Reciprocal Rank Fusion (RRF) across HNSW Vector + Tantivy BM25 + Exact Symbol Pre-Pass, Code-Identifier Sub-Tokenization in Tantivy, and Explicit Graph-Expansion Slot Quota in `reserve_normative_slots`.

## 1. Problem

A source audit against Sourcegraph's retrieval stack (`zoekt` + hybrid ranking) identified three concrete retrieval gaps in NeuroStrata's current search pipeline:

1. **`SearchMode::Hybrid` is single-retriever re-scoring, not multi-retriever fusion (`src/store/ladybug.rs:1057-1138`, `src/graph_read/hybrid.rs:47-64`):**
   - `SearchMode::Hybrid` currently runs only on the candidate pool already returned by the HNSW vector scan and its Cypher expansion (`src/store/ladybug.rs:48-1056`). It never queries `tantivy_lexical` (`src/lexical.rs`) or the in-house `fts::LexicalIndex` (`src/fts.rs`).
   - If a memory or symbol has high lexical/BM25 relevance (e.g., exact error codes, config keys, function signatures, CLI flags) but falls outside the initial HNSW vector page, `SearchMode::Hybrid` misses it completely.
   - Furthermore, the MCP `neurostrata_search_memory` schema (`src/server.rs`) only exposes `mode: "vector" | "lexical" | "lexical-inhouse"`, leaving `SearchMode::Hybrid` unreachable from the primary MCP tool surface without config overrides.

2. **Tantivy lacks code-identifier sub-tokenization and query-time `include_code_ast` gating (`src/lexical.rs:65-68, 97-105, 261-265, 430+`):**
   - `TantivyLexicalIndex::is_excluded` (`src/lexical.rs:261-265`) excludes `STRUCTURAL_MEMORY_TYPES` (`"directory"`, `"file"`, `"markdown"`, `"freshness_flag"`, `"guard_event"`, `"community_summary"`, `"digital_twin"`, `"digital_twin_schema"` at lines 65–68), while indexing `code_ast` and `doc_section` rows. However, `search_with_active_generations` (`src/lexical.rs:430+`) only supports `include_tasks: bool` (`MustNot` on `"task"`) and has no `include_code_ast: bool` filter to exclude `code_ast` / `doc_section` rows when a caller requests normative-only lexical search.
   - `TantivyLexicalIndex::build_schema` (`src/lexical.rs:97-105`) registers `content`, `symbol`, and `location` with Tantivy's default `TEXT` options (`default` prose tokenizer), which does not split `camelCase`, `PascalCase`, `snake_case`, or `::` / `/` path segments into searchable sub-tokens. Searching `checkCallerPath` or `caller_path` fails to match `check_caller_path` in `src/daemon.rs`.

3. **Graph-expanded neighbors (`OneHop`, `EpisodeSibling`, `Governs2hop`) have no reserved slot quota when primaries are normative (`src/store/ladybug.rs:933-968`, `1030-1138`, `3966-3994`):**
   - In `src/store/ladybug.rs:933`, graph expansions receive `EXPANSION_BASE_SCORE = 1.0`.
   - In `SearchMode::Hybrid` (`src/store/ladybug.rs:1066, 1137`), expansions start with `d_vec = 1.0` and then pass through `reserve_normative_slots(hybrid_results, limit)`.
   - `reserve_normative_slots` (`src/store/ladybug.rs:3966-3994`) partitions strictly by `NORMATIVE_MEMORY_TYPES` (`rule`, `fact`, `decision`, `procedure`) vs. non-normative—it does **not** partition `DirectMatch` primaries vs. graph-expanded (`OneHop` / `EpisodeSibling` / `Governs2hop`) neighbors. When direct vector matches already contain `>= limit` normative rows with lower `d_vec`, every 1-hop/2-hop graph-expanded rule or AST neighbor is truncated out at `limit`.

## 2. Goals

1. **True Multi-Retriever RRF Fusion (`mode: "hybrid"`):** Execute HNSW vector search + Tantivy BM25 lexical search (with automatic fallback to `fts::LexicalIndex` if Tantivy is unavailable) + deterministic exact-symbol pre-pass in parallel, and fuse their candidate lists via Reciprocal Rank Fusion (RRF) combined with graph co-occurrence and feedback boosts.
2. **Expose `"hybrid"` on MCP (`src/server.rs`) and Default to Hybrid Search:** Add `"hybrid"` to `neurostrata_search_memory` (`mode` enum in `src/server.rs`) and route default searches through `SearchMode::Hybrid`.
3. **Code-Aware Tantivy Tokenization with Query-Time `include_code_ast` Gating (`src/lexical.rs`):** Bind `content`, `symbol`, and `location` fields in `TantivyLexicalIndex::build_schema` (`src/lexical.rs:97-105`) to a custom code-identifier analyzer (`"code_text"`) that emits both raw identifiers and split sub-tokens (`camelCase`, `PascalCase`, `snake_case`, `kebab-case`, `::`, `/`, `.`), and add `include_code_ast: bool` to `search_with_active_generations` (`src/lexical.rs:430+`) to gate `code_ast` and `doc_section` rows at query time.
4. **Guaranteed Graph-Expansion Slot Quota:** Upgrade slot reservation (`reserve_normative_and_expansion_slots`) so that when graph expansion produces valid active 1-hop/2-hop neighbors (`OneHop`, `EpisodeSibling`, `Governs2hop`), a bounded quota of slots (`expansion_slots = (limit / 4).clamp(1, 4)` when `limit >= 4`) is guaranteed for the highest-ranked expanded neighbors, ordered by parent rank + canonical edge weight (`GOVERNS > SUPERSEDES > EXTRACTED_FROM > RELATES_TO/CONTAINS > DEPENDS_ON`).

## 3. Non-Goals

- Full raw-file positional trigram indexing across un-ingested files (NeuroStrata indexes ingested AST symbols (`code_ast`), doc sections (`doc_section`), and memories; raw file grep remains handled by `neurostrata_locate` / host tools).
- Changing `VectorStore` embedding dimensions or HNSW index parameters.
- Breaking backward compatibility for callers explicitly passing `mode: "vector"`, `"lexical"`, or `"lexical-inhouse"`.

## 4. Architecture

### 4.1 Code-Aware Tokenizer & Query-Time `include_code_ast` Gating in `src/lexical.rs`

1. **Register `code_text` Analyzer in `TantivyLexicalIndex` (`src/lexical.rs:97-105`):**
   - Implement a `CodeSubwordFilter` token filter wrapped in a custom Tantivy `TextAnalyzer` registered as `"code_text"` on `index.tokenizers()`.
   - For any token `t` (e.g., `LadybugStore::search_tantivy_lexical`):
     - Emit the lowercased full token (`ladybugstore::search_tantivy_lexical`) and bare identifier parts (`ladybugstore`, `search_tantivy_lexical`).
     - Emit camelCase / PascalCase / snake_case / digit-boundary sub-tokens (`ladybug`, `store`, `search`, `tantivy`, `lexical`).
   - Configure `content_f`, `symbol_f`, and `location_f` in `TantivyLexicalIndex::build_schema` (`src/lexical.rs:102-104`) with `TextOptions::default().set_indexing_options(TextFieldIndexing::default().set_tokenizer("code_text").set_index_option(IndexRecordOption::WithFreqsAndPositions)).set_stored()`.

2. **Gate `code_ast` and `doc_section` at Query Time (`src/lexical.rs:430+`):**
   - Keep `STRUCTURAL_MEMORY_TYPES` (`src/lexical.rs:65-68`) excluding non-searchable container rows (`directory`, `file`, `markdown`, `freshness_flag`, `guard_event`, `community_summary`, `digital_twin`, `digital_twin_schema`), while `code_ast` and `doc_section` remain indexed.
   - In `TantivyLexicalIndex::search_with_active_generations`, add parameter `include_code_ast: bool`. When `include_code_ast == false`, append `BooleanQuery` `MustNot` term clauses for `memory_type == "code_ast"` and `memory_type == "doc_section"` (matching the existing `include_tasks == false` `MustNot` clause for `"task"`).

### 4.2 Reciprocal Rank Fusion (RRF) Engine in `src/graph_read/hybrid.rs`

Add a deterministic RRF fusion function in `src/graph_read/hybrid.rs`:

```rust
pub const RRF_K: f32 = 60.0;

#[derive(Clone, Debug)]
pub struct RankedCandidate {
    pub id: String,
    pub vec_rank: Option<usize>,         // 1-based rank in HNSW vector primaries
    pub vec_distance: Option<f32>,       // raw boosted L2 distance
    pub lex_rank: Option<usize>,         // 1-based rank in Tantivy/FTS lexical hits
    pub lex_score: Option<f32>,          // BM25 score
    pub sym_grade: SymbolMatchGrade,     // Exact, Prefix, TokenSubstring, None
    pub graph_co_occurrence: u64,        // distinct primary sources / edge count
    pub parent_best_rank: Option<usize>, // for OneHop/2Hop expansions: best rank of originating primary
    pub edge_priority_weight: f32,       // GOVERNS=1.0, SUPERSEDES=0.9, EXTRACTED_FROM=0.8, RELATES_TO=0.6, DEPENDS_ON=0.5
    pub feedback_gain: f32,
}
```

- **Parent-Rank & Edge-Priority Propagation (`src/store/ladybug.rs:850-968`):**
  - During `search_inner` Phase 1/2 Cypher expansion, build a `primary_rank_by_id: HashMap<String, usize>` (1-based rank among admitted `DirectMatch` primaries) before scanning expansion edges.
  - Extend `CandidateEntry` with `best_primary_rank: Option<usize>` and `best_edge_weight: f32` (computed from the traversed `EvidenceEdge.relation`: `GOVERNS => 1.0`, `SUPERSEDES => 0.9`, `EXTRACTED_FROM => 0.8`, `RELATES_TO | CONTAINS => 0.6`, `DEPENDS_ON => 0.5`).
  - Persist `best_primary_rank` and `best_edge_weight` onto each admitted expansion entry in `search_inner` so the `SearchMode::Hybrid` RRF combiner consumes exact parent rank and edge priority without re-querying.
- **RRF Score Computation (converted to normalized distance `d_rrf in [0.0, 1.0]`, lower = better, preserving NeuroStrata's distance sort invariant):**
  - For each retrieval track $m \in \{\text{vec}, \text{lex}\}$ with weight $w_m$:
    $$\text{rrf\_gain}(r) = \sum_{m} \frac{w_m \cdot (k + 1)}{k + \text{rank}_m(r)}$$
  - Exact/prefix symbol match (`SymbolMatchGrade::Exact` / `Prefix`) adds a bounded reciprocal boost $w_{\text{sym}} \cdot (1.0 - d_{\text{sym}})$.
  - For graph-expanded neighbors (`vec_rank == None && lex_rank == None`, `parent_best_rank == Some(p_rank)`), inherit a hop-decayed parent RRF gain:
    $$\text{rrf\_gain}_{\text{exp}}(r) = \text{decay}_{\text{hop}} \cdot \text{edge\_priority\_weight} \cdot \frac{w_{\text{graph}} \cdot (k + 1)}{k + p\_rank} \cdot \left(1.0 + 0.1 \cdot \ln(1 + \text{co\_occurrence})\right)$$
  - Convert total gain into a bounded distance in `[0.0, 1.0)`:
    $$d_{\text{hybrid}}(r) = \text{clamp}(1.0 - \text{normalized\_gain}(r) - \text{decision\_boost}(r) - \text{feedback\_adj}(r),\ 0.0,\ 1.0)$$

### 4.3 Two-Pool Slot Reservation (`reserve_normative_and_expansion_slots`)

Replace the flat `reserve_normative_slots` truncation on hybrid/vector results (`src/store/ladybug.rs:1033-1138, 3966-3994`) with a two-dimension quota when `limit >= 4`:

1. **Primary Normative Quota:** Reserve up to `normative_slot_limit(limit)` slots for `DirectMatch` normative rows (`rule`, `fact`, `decision`, `procedure`) passing `MAX_NORMATIVE_DISTANCE`.
2. **Graph-Expansion Quota:** Reserve up to `expansion_slot_limit(limit) = (limit / 4).clamp(1, 4)` slots for graph-expanded rows (`OneHop`, `EpisodeSibling`, `Governs2hop`), prioritizing `GOVERNS` rules and multi-primary co-occurring neighbors.
3. **Remaining Slots:** Fill up to `limit` in ascending `d_hybrid` score order across all remaining primary and expanded candidates, deduplicated by logical ID (`bare_id_of`).
4. **Provenance Derivation Tags:** Rows matched by both vector and lexical tracks carry `derivation: ["vector", "lexical", "rrf"]` in `SearchEvidence`.

### 4.4 MCP Surface (`src/server.rs`)

- Update `neurostrata_search_memory` tool schema and dispatch handler in `src/server.rs`:
  - `mode`: `"hybrid" | "vector" | "lexical" | "lexical-inhouse"` (default `"hybrid"`).
  - Pass `include_code_ast` through to `search_tantivy_lexical` (`src/store/ladybug.rs:5618`) and `TantivyLexicalIndex::search_with_active_generations`.

### 4.5 Library Reuse (Rule `227984e4-780c-4fe4-a44c-286d7d14eabf`) & Formal Verification (`proofs/NeuroStrata/HybridSlots.lean`)

1. **Library Reuse:**
   - Leverage `tantivy::tokenizer` (`TextAnalyzer`, `LowerCaser`, `RemoveLongFilter`, and composable `TokenStream` filters in the existing MIT-licensed `tantivy` crate) rather than maintaining a separate parallel lexical index.
2. **Formal Verification (`src/formal_kernel.rs`, `proofs/NeuroStrata/HybridSlots.lean`, `tests/lean_parity_test.rs`):**
   - Extract the pure slot-allocation arithmetic into `src/formal_kernel.rs`:
     - `pub const fn allocate_hybrid_slots(limit: u32, avail_normative: u32, avail_expansion: u32, avail_other: u32) -> HybridSlotAllocation`
   - Prove in `proofs/NeuroStrata/HybridSlots.lean`:
     - `hybrid_slots_never_exceed_limit`: `alloc.normative_taken + alloc.expansion_taken + alloc.remaining_taken ≤ limit`.
     - `hybrid_slots_never_exceed_available`: each pool's allocation is bounded by its available candidate count (`normative_taken ≤ avail_normative`, `expansion_taken ≤ avail_expansion`).
     - `hybrid_slots_guarantees_expansion_when_available`: whenever `limit ≥ 4 ∧ avail_expansion ≥ 1`, `alloc.expansion_taken ≥ 1`.
   - Verify exhaustive parity in `tests/lean_parity_test.rs` against `lake --dir proofs exe neurostrata-proofs`.

## 5. Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Latency increase from querying both HNSW and Tantivy | Tantivy lookup is in-process mmap (`<2ms` for $10^5$ docs); run Tantivy candidate retrieval concurrently with HNSW pre-filtering and hydrate only the union top-$4K$ IDs. |
| Tantivy index rebuild required when switching `content`/`symbol`/`location` to `"code_text"` analyzer | Bump the lexical schema marker in `TantivyLexicalIndex` so existing indexes automatically trigger `rebuild_tantivy_lexical` once on open. |
| Existing unit tests expecting `mode: "vector"` exact scores | Keep `SearchMode::Vector`, `Lexical`, and `LexicalInhouse` behavior unchanged when explicitly requested; add dedicated RRF + expansion-slot tests for `SearchMode::Hybrid`. |
| Generation drift between Tantivy and LadybugDB | Reuse `ActiveGenerations::read(conn, namespace)` (`src/store/ladybug.rs:5637`) once per `search_inner` call and apply `active.reaches_source(...)` to both vector and Tantivy candidates. |

## 6. Checkable Acceptance Criteria

1. Searching an exact `camelCase` or `snake_case` symbol identifier via `mode: "hybrid"` (with `include_code_ast: true`) surfaces the matching `code_ast` row in top-3 even when its embedding vector distance ranks outside the top-10 vector hits.
2. Searching a query that matches a `DirectMatch` memory in both HNSW and Tantivy ranks that dual-matched memory strictly higher (lower distance score) than a single-track match at equivalent rank, and stamps `derivation: ["vector", "lexical", "rrf"]`.
3. When `limit >= 4` and a query matches `>= limit` normative primary memories **plus** at least one active 1-hop `GOVERNS` or `RELATES_TO` expanded memory, the final truncated `results` (`len == limit`) retains at least `expansion_slot_limit(limit)` expanded neighbor(s) instead of dropping all expansions.
4. `neurostrata_search_memory` with `include_code_ast: false` returns zero `code_ast` or `doc_section` rows from Tantivy even though those rows are present in the Tantivy index.
5. `cargo test --locked` passes all existing `search_*` and `lexical_*` tests plus new RRF and expansion-quota regression tests.
6. `proofs/NeuroStrata/HybridSlots.lean` builds cleanly with zero `sorry`, and `cargo test --locked --test lean_parity_test` passes all `HybridSlots` parity rows.

## 7. Next Step

On approval of this spec, invoke `writing-plans` for Task `neurostrata-5xjo` to produce the step-by-step TDD implementation plan.
