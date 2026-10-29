# Full-Text Search Lexical Upgrade Design

> Status: draft — pending Argon review.
> Task: `neurostrata-m0oz` (full-text search lexical upgrade, OxiRS §1.9 in-house track).
> Companion: `docs/superpowers/specs/2026-10-27-tantivy-full-text-search-design.md` (optional Tantivy track), `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`.

## 1. Problem

`src/fts/mod.rs` is a toy BM25 approximation: it stores one `content` string per id, counts term document frequency with a `HashSet` (not true corpus DF), splits only on whitespace, and has no field awareness, phrase, fuzzy, or tombstone handling. The Tantivy track is optional and feature-gated; NeuroStrata still needs a deterministic, dependency-free lexical upgrade that ships in the default build and beats the exact-name pre-pass on partial/typo/phrase queries.

## 2. Goal

Upgrade the in-house `InvertedIndex` to a **multi-field, ranked lexical engine** with:

- fields `content`, `symbol`, `location`, `namespace`, `memory_type`,
- phrase queries (adjacent term sequences),
- fuzzy matching (Levenshtein distance ≤ 2, bounded),
- namespace and tombstone filtering,
- deterministic scoring (single rule, no RRF),
- zero new dependencies (pure Rust, always available).

LadybugDB remains the sole store; the index is a derived, rebuildable artifact.

## 3. Constraints

- No new crate dependencies; pure `std`.
- No LLM.
- Single auditable ranking rule; no fusion of multiple rankers.
- Deterministic output for identical inputs.
- Tombstone/retired/superseded/archived/conflicting/registry filtering consistent with `search_inner`.
- Namespace isolation.
- Score polarity: `SearchResult.score` is distance (lower better); BM25 converted to distance.

## 4. Scope

In scope:

- Rewrite `src/fts/mod.rs` as `LexicalIndex` with multi-field documents.
- Tokenizers: whitespace + code-aware (split on `/`, `.`, `:`, `_`, camelCase boundaries).
- Phrase and fuzzy query parsing.
- Deterministic BM25F-ish scoring with field boosts.
- Read-time filters and namespace isolation.
- Rebuild from LadybugDB.
- Integration with `neurostrata_search_memory` as `mode = "lexical-inhouse"` (default build) and optional blend.

Out of scope:

- Replacing embedding/vector track.
- Tantivy (separate optional track).
- Cross-namespace federated lexical search.

## 5. Approach

### 5.1 Data structures

```rust
pub struct LexicalDoc {
    pub id: String,              // composite "{namespace}::{memory_id}"
    pub namespace: String,
    pub memory_type: String,
    pub content: String,
    pub symbol: String,
    pub location: String,
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
    pub archived: bool,
    pub user_id: String,         // resource owner for ReBAC
    pub visible_to: Vec<String>, // ReBAC ACL
    pub domains: Vec<String>,    // domain slugs
}
pub struct NamespaceIndex {
    pub docs: BTreeMap<String, LexicalDoc>,   // sorted by id
    pub postings: BTreeMap<String, Vec<Posting>>, // term -> postings
    pub doc_freq: BTreeMap<String, usize>,    // distinct doc count per term (across all fields)
    pub doc_len: BTreeMap<String, usize>,     // total tokens across fields
    pub avgdl: f64,
    pub field_weights: [f64; 3],               // content, symbol, location
}
pub struct LexicalIndex {
    pub namespaces: BTreeMap<String, NamespaceIndex>, // partitioned by namespace
}
pub struct Posting { pub doc_id: String, pub field: Field, pub tf: usize, pub positions: Vec<usize> }
pub enum Field { Content, Symbol, Location }
```

- `BTreeMap` everywhere for deterministic iteration.
- Field weights default `[1.0, 2.0, 1.5]`.

### 5.2 Tokenization

- `tokenize_whitespace(s)`: lowercase, split on whitespace, strip leading/trailing punctuation. Used for `content`.
- `tokenize_code(s)`: split on `/`, `.`, `:`, `_`, `-`, then camelCase boundaries (`fooBar` -> `foo`, `bar`), lowercase. Used for `symbol` and `location`.
- Position tracking per field for phrase queries.
- Empty tokens discarded.
- Tokenizer binding is fixed: `content` → `tokenize_whitespace`; `symbol`, `location` → `tokenize_code`.
- Query tokenization per field: a query clause is tokenized with the target field's tokenizer before matching; if no field is specified, the clause is evaluated against all fields using their respective tokenizers (a term matches if it matches in any field).

### 5.3 Indexing

- `add(doc)`: inserts doc, updates postings and DF. If id exists, remove old doc first (idempotent replace).
- `remove(id)`: deletes doc and postings.
- `rebuild(docs: Vec<LexicalDoc>)`: clears and re-adds in id order.
- `len()`, `is_empty()`.

### 5.4 Query parsing

Query string is split into clauses:

- Plain terms: `foo bar` -> all terms must match (AND) by default; prefix with `OR` for OR (optional; default AND).
- Phrase: `"exact phrase"` -> adjacent positions in any field; if no adjacent match, the document does NOT match the phrase clause (phrase is a hard filter, not just a boost).
- Fuzzy: `term~` or `term~2` -> Levenshtein distance ≤ N (default 1, max 2). Fuzzy expansion is bounded to terms in the index with Levenshtein ≤ N; max 50 expansions per term, tie-broken by (edit distance ASC, term ASC). A fuzzy query token matches if any expansion matches (OR across expansions, taking max contribution per query token); distinct query tokens remain ANDed.

No boolean nesting beyond AND/OR/phrase/fuzzy.

### 5.5 Scoring

For each candidate document, compute per-term BM25 contribution:

```
idf = ln(1 + (N - df + 0.5) / (df + 0.5))   // df = distinct doc count containing term
tf_norm = (tf * (k1 + 1)) / (tf + k1 * (1 - b + b * dl/avgdl))
contrib = idf * tf_norm * field_weight
```

- `k1 = 1.2`, `b = 0.75`.
- Multi-field aggregation: for a given query term, sum `contrib` across all fields in which it appears (field-level BM25, not BM25F combined TF).
- Phrase match: if all phrase terms appear adjacent in a field, add `phrase_boost = 2.0 * idf_sum` (sum of idf over phrase terms).
- Fuzzy match: uses the expanded term's DF/tf; multiplies contribution by `0.5_f64.powi(edit_distance)` (decay, never increases score over exact).
- Sum over query terms (AND requires all terms present; missing term → score 0 for that doc).
- Convert to distance: `distance = 1.0 - (score / max_score_in_result_set)` clamped `[0,1]`; empty result → 0.0.

Sort `ASC` by distance, tie-break `id ASC`.

### 5.6 Filtering

At query time:

- namespace equality (required).
- Retirement filter:
  - `as_of = None`: include row if `valid_to.map_or(true, |v| v <= 0 || v > now)` (active rows).
  - `as_of = Some(ts)`: include row if `valid_from.unwrap_or(0) <= ts && valid_to.map_or(true, |v| v <= 0 || ts < v)`.
  - `valid_from`/`valid_to` are `Option<i64>`; `None` means unspecified.
- exclude archived unless `include_archived`.
- exclude task rows unless `include_tasks`.
- exclude structural/registry/mesh-conflicting always.
- exclude rows where `!allowed(principal, user_id, "viewer", &visible_to)` (ReBAC §5.2, 4-arg signature; `visible_to` from `metadata.visible_to`, empty = unrestricted).

Over-fetch `top_k * 4` from the index, filter, replace from next page until `top_k` survivors or index exhausted.

### 5.7 Domain fallback

If domain filter matches 0 survivors, fall back to unfiltered (within namespace/retirement rules) and report `domain filter: skipped — no row matched the domain; the results are unfiltered (lexical)`.

### 5.8 In-memory lifecycle

- `LexicalIndex` lives on `LadybugStore` as `RwLock<BTreeMap<String, NamespaceIndex>>` (lazy init per namespace on first query).
- Mutation hooks (behind no feature gate; always compiled): `upsert`, `delete`, `relocate`, `clear_ingested` update the in-memory index for the affected namespace; if the namespace index is not yet loaded, the mutation is a no-op on the cache (DB remains source of truth; next query rebuilds).
- `rebuild(namespace?)` clears and rebuilds from LadybugDB for the given namespace (or all).
- Thread safety: writes take `write_lock` then update index; reads take read lock.

### 5.9 Integration

- `neurostrata_search_memory` gains `mode: "lexical-inhouse"` (default build; always available).
- Skips embedding entirely.
- Maps surviving hits to `SearchResult` via LadybugDB lookup (payload/evidence from store).
- `derivation: ["lexical-inhouse"]`.
- Optional blend into default mode behind config `lexical_blend_inhouse: bool` (default false): `0.7 * vector_distance + 0.3 * lexical_distance`, with worst-case substitution for single-track candidates (same rule as Tantivy spec §5.4).
- Rebuild tool: `neurostrata_rebuild_lexical_index` accepts `engine: "inhouse" | "tantivy"` (default `inhouse`). In default builds (no `--features tantivy`), `engine = "tantivy"` returns the disabled error from Tantivy spec §5.1; `engine = "inhouse"` always runs. This reconciles with Tantivy spec §5.1/§6.

### 5.10 Evidence rendering

Reuse Tantivy spec §5.7 format: `\nMatch [lexical-inhouse]` on `DirectMatch` rows.

## 6. Interfaces

- `src/fts/mod.rs`: `LexicalDoc`, `LexicalIndex`, `tokenize_*`, `parse_query`, `score`, `search`.
- `neurostrata_search_memory`: `mode` accepts `"lexical-inhouse"`.
- Config: `lexical_blend_inhouse: bool` (default false), `lexical_fuzzy_max_distance: usize` (default 2, clamped 0..=2).
- Rebuild tool: `neurostrata_rebuild_lexical_index` accepts `engine: "inhouse" | "tantivy"` (default `inhouse`).

## 7. Edge cases

- Empty query: return empty (no error).
- Empty index: empty.
- All terms missing: doc score 0 (excluded).
- Fuzzy expansion cap (50) prevents blow-up.
- Phrase with no adjacent match: document is excluded (phrase is a hard filter, not a boost).
- Namespace isolation: no cross-namespace leakage.
- Retired/superseded: filtered per `as_of`.

## 8. Testing

- Unit: tokenization (whitespace, code-aware, camelCase).
- Unit: phrase adjacency detection.
- Unit: fuzzy expansion bounded.
- Unit: scoring monotonic (relevant > irrelevant).
- Unit: distance conversion and sort.
- Integration: namespace isolation.
- Integration: tombstone/retired filtering.
- Integration: domain fallback.
- Determinism: identical inputs → identical outputs.

## 9. Acceptance

In-house multi-field lexical search available in default build; phrase/fuzzy/namespace/tombstone support; deterministic single-rule scoring; no new deps; LLM-free.

## 10. Files touched (expected)

- `src/fts/mod.rs`: full rewrite.
- `src/store/ladybug.rs`: mutation hooks (upsert/delete/relocate) rebuild index.
- `src/server.rs`: mode enum + rebuild tool `engine` param.
- `Cargo.toml`: no change.