# PAR-10: literal code-token search and pre-TopDocs filtering

Baseline: `parity/retrieval-visibility` @ `5390f2f5` (PAR-02).
Reference: fork origin/develop `3d693fd96c5fcad07d3f740584203e05975b53ce`,
fix commit `a914f63`. Scope: `src/lexical.rs` (Tantivy adapter) and its
callers. No schema or dependency change.

## The gap being closed

`TantivyLexicalIndex::search` tokenizes the query and hands it to Tantivy's
query parser unquoted (`src/lexical.rs:352-353`). Ordinary code tokens are
therefore read as query syntax:

- `std::sync::Mutex` looks like a field reference;
- unbalanced quotes, `(`, `)`, `+`, `-`, `^`, `~`, `/` can be grouping,
  occur operators, boosts, fuzzy markers or regex delimiters;
- a parse error propagates instead of returning matches.

Separately, namespace, task, archive and validity predicates are applied
after `TopDocs` collects (`src/lexical.rs:357` and the per-hit loop), so
higher-scoring hidden or foreign rows consume the fetch budget and a valid
local row never reaches the result page.

## Acceptance contract (specified before test/production edits)

| Criterion | Expected behavior | Test |
| --- | --- | --- |
| E1 literal code tokens | `std::sync::Mutex`, `foo<T>`, `a(b)`, `x+y`, `path/to/file`, `C++`, and quoted strings are searched literally and return their matching rows. No parse error escapes. | `par10_code_tokens_are_literal` |
| E2 operator-looking tokens | Tokens such as `AND`, `OR`, `NOT`, `+term`, `-term`, `term^2`, `term~1`, `*` no longer change query structure; they match as text. This is an intentional behavior change and must be stated in the release note. Advanced syntax, if retained, is a separate explicit API, not the default. | `par10_operator_tokens_are_literal` |
| E3 edge inputs | Escaped quotes, backslashes, Unicode, empty query, whitespace-only query and `limit == 0` return an explicit result (no panic, no error). | `par10_query_edge_cases` |
| E4 pre-TopDocs filtering | Namespace, task/archived and validity predicates are part of the query, before `TopDocs`. A fixture with many high-scoring foreign/hidden rows and one eligible local row returns the local row. Per-hit checks stay as defense in depth. | `par10_hidden_rows_cannot_starve_the_page` |
| E5 scoped rebuild | Rebuilding one namespace replaces only that namespace's documents and preserves the others. | `par10_rebuild_scoped_preserves_other_namespaces` |
| E6 time boundary | `as_of` uses the same validity boundary as the per-hit check: `valid_to == as_of` is retired, `valid_to > as_of` is visible. | `par10_as_of_boundary_matches_per_hit_rule` |
| E7 ranked parity | On a shared fixture, eligible ordered results agree between the in-house lexical route and Tantivy for the same query. | E1/E4 comparisons |

## Behavior contract

- Quote each token after `tokenize_code_for_tantivy`; escape `\` and `"` inside
  the quotes. A single-term literal stays a term query; a multi-token literal
  becomes the same phrase query a bare multi-token literal produced, so
  per-field boosts and matching semantics are unchanged.
- Apply namespace, task/archived and validity filters as zero-score `Must`
  clauses (`ConstScoreQuery`) because tantivy 0.27 has no `Occur::Filter`.
- Keep the per-hit checks. They are the second line of defense, not the
  filtering mechanism.
- `rebuild_scoped` replaces only the named namespace; unscoped `rebuild`
  keeps its replace-everything contract.

TDD order: this tracked spec → tests with unchanged production code → record
assertion RED (including one parse-error fixture and one starvation fixture) →
minimal quoting/filter/rebuild port → GREEN plus focused regressions. Evidence
logs go to `NeuroStrata-parity-audit-2026-11-10/implementation/par-10/`.
