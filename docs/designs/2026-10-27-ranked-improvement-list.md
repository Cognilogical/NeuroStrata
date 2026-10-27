# Ranked improvement list

Ordered by impact. No phases. Each row is a candidate spec.
Effort: **S** under a week, **M** one to three weeks, **L** over three weeks.

| # | Idea | What it is | Why it matters | Effort |
|---|---|---|---|---|
| 1 | **Search exact names** | When a query contains a symbol, file path, env var, error string, or quoted phrase, match it exactly before the vector search. | Embeddings miss exact identifiers. This is the biggest known retrieval failure for coding agents. | M |
| 2 | **Re-ingest only what changed** | Keep a content hash per file. Reuse existing nodes and embeddings for unchanged files. Re-parse when the parser or schema version changes, even if the file did not. | Today a re-ingest re-embeds everything and rebuilds unchanged nodes. That is slow and creates churn. | M–L |
| 3 | **Make docs real** | Markdown and doc files are currently stored as empty `Path: ...` rows. Pull out headings, backticked symbols, file paths, and links with deterministic code, then connect them to the graph. | Docs are anchors today, not searchable content. This unlocks the documentation already in the repo without any model. | M |
| 4 | **Fix the trust bugs** | Model pins use the SHA-256 of the empty string, so they verify nothing. Laya reports a fake `0.75` duplicate confidence whenever a model file exists. Fix the pins and either repair or retire Laya. | Unverified model downloads and fake dedup judgments corrupt behavior silently. Fix before any model work. | S |
| 5 | **Optional GLiNER extraction** | Use GLiNER2.5 to pull entities and relations from prose, and propose graph edges for review. Review-only at first; auto-materialize only above a measured precision bar. | Biggest potential gain, but heavy (~590 MB), unproven, and must never touch search, dedup, guard, or routing. | L |
| 6 | **Cite edge sources** | Each generated edge stores the source file, content hash, text span, and which extractor produced it. | Lets you trust and debug the graph. Without this, an edge is an unexplained assertion. | M |
| 7 | **Dependency edges** | Extract imports and module dependencies into the graph so `related` can walk code structure. Directed calls need a separate table; do not disguise them as undirected links. | Improves code navigation and impact analysis. | M–L |
| 8 | **Dry-run ingest** | Show what would change: files added/changed/removed, embeddings avoided, secret hits, estimated work. Write nothing, download nothing. | Safe preview before a large or expensive ingest. | M |
| 9 | **Multi-domain memories** | Let one memory belong to several domains (`domain: string | string[]`). Update the add schema, registry, storage, and search. | Useful for monorepos and cross-cutting rules. | S–M |
| 10 | **Conflict candidates** | Flag active memories that look contradictory for review. Never auto-mark truth; near-duplicates are not contradictions. | Catches stale or conflicting rules without pretending the system can settle them. | M |
| 11 | **Surface procedures** | When a due procedure matches the current query or task, show it in the snapshot. | Makes procedural memory useful at the moment it is needed. | S |
| 12 | **Watermarks** | Record what was last ingested or enriched, with input and extractor fingerprints. | Repeat runs skip work that is already done. | S |
| 13 | **Dangling-edge report** | Show unresolved `GOVERNS` / `RELATES_TO` targets in `doctor` and `task_validate`. | Makes broken graph links visible instead of silently skipped. | S–M |
| 14 | **Enrichment telemetry** | Per-stage counts, failures, latency, and fingerprints for ingest and enrichment. | Makes the pipeline debuggable instead of a black box. | S–M |

## Skip these

- OWL/RDF ontology and strict entity dropping.
- Generated global summaries.
- Date-based temporal reranking.
- A separate SQLite/Redis session store.
- A keyword index, BM25, or rank fusion.
- A Python sidecar in normal installs.
- GLiNER in search, dedup, guard, or routing.
- Automatic conflict verdicts.
- Directed calls faked as undirected relations.

## If we spec only three

1. Exact-name search.
2. Re-ingest only what changed.
3. Make docs real.

Those three need no model, close known gaps, and make everything after them cheaper.

---

*Derived from `docs/design-cognee-concepts.md`, Argon’s evaluation, and the
Argon/K3 adjudication. No implementation is authorized by this list.*
