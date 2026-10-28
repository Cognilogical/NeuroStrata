# Partial-Match Retrieval — Fuzzy Symbol Matching Without BM25 Design

> Status: draft — awaiting Argon review.
> Task: `neurostrata-ui0x` (Partial-match retrieval: fuzzy symbol matching without BM25, P3).

## 1. Problem

Keyword retrieval in NeuroStrata only supports exact symbol match. Need fuzzy/deterministic partial-match matching without BM25/RRF (banned by policy).

## 2. Goal

Deterministic partial-match / fuzzy symbol retrieval for memory names, KnownIds, and topology component labels; ranked by exactness.

## 3. Constraints

- Deterministic: same query → same ranked results; no probabilistic ranking.
- No LLM, no BM25, no RRF.
- Library hygiene: `std::string` matching; `sha2` for canonical form; `oxrdf` for IRI normalization; no `oxirs-core`/`scirs2`/`horned-owl`.
- Warn-first: unsupported patterns → exact match fallback.

## 4. Scope

- Fuzzy match on memory labels and KnownIds aliases.
- Rank: exact match > prefix match > substring match > contains match.
- Integration with `search_with_domain` and `related`.

## 5. Approach

- Canonical form: normalize with `oxrdf` IRI canonicalization, `sha2` for stable form.
- Prefix: first N characters (default 3) normalized form → all memories whose canonical label starts with it.
- Substring: normalized form appears anywhere in memory label's canonical form.
- Contains: normalized form is a token within label's canonical form (space-delimited).

## 6. Library evaluation

- `oxrdf` for IRI/label normalization (shared with `9fnf`/`2hxl`).
- `sha2` for canonical form (already present).
- `std::string`: exact/prefix/substring/contains; no probabilistic ranking.
- No new core libraries.

## 7. Interfaces / Files Touched (expected)

- `src/store/ladybug.rs`: partial-match hooks.
- `src/search/partial.rs` (new): deterministic fuzzy matching.
- `Cargo.toml`: no new deps.