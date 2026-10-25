# R24: Storage Engine Migration Evaluation — SQLite + sqlite-vec

## Status
Completed. Task: neurostrata-95x5.

## Motivation
ladybug (LadybugDB, lbug 0.20.4) pain points driving the evaluation:
- SIGSEGV under concurrent writes (single-writer lock workaround, R5)
- WAL corruption on crash (durability anecdotal, R21)
- No native bind API (R16 defense-in-depth float-array formatting)
- 0.20.4-specific bugs requiring workarounds throughout codebase
- ~7.8k lines in ladybug.rs (R28)

## Candidate: SQLite + sqlite-vec
- rusqlite 0.41.0 (ergonomic wrapper, mature, widely used)
- sqlite-vec 0.1.10-alpha.4 (FFI bindings to sqlite-vec extension)
- SQLite: ACID, WAL mode, concurrent readers + single writer, crash recovery
- sqlite-vec: vector search via virtual table, KNN queries

## Trait Surface to Implement (VectorStore)
Full list from src/traits.rs — 20 methods:
1. init(namespace) — create tables
2. as_any() — downcast
3. upsert(ns, id, vector, payload) — insert/update
4. search(ns, vector, limit, include_tasks, include_archived) — KNN + graph expansion
5. nearest_by_raw_distance(ns, vector, query) — unboosted dedup
6. find_active_exact_content(ns, content, scope) — exact UTF-8 lookup
7. delete(ns, id)
8. clear_ingested(ns) — remove AST nodes
9. relink_edges(ns) — rebuild edges
10. list(ns, user_id, memory_type) — filtered listing
11. get(ns, id) — retrieve vector+payload
12. relocate(id, from, to) — move namespace
13. list_namespaces() — list tables/dbs
14. export_graph(include_retired, include_archived, contexts, anchor) — full graph export
15. increment_access_count(ns, id)

Plus IntakeStore (src/task/intake.rs): create_receipt, list_receipts, list_candidates, resolve_candidate.

## Feasibility Assessment

### Strengths of sqlite-vec
- ACID transactions, WAL mode, crash recovery (solves R21 durability)
- Concurrent readers (solves single-writer contention, R5)
- Parameterized queries natively (solves R16 injection risk)
- Mature ecosystem, well-tested
- Smaller codebase potential (no Cypher string building)

### Risks / Gaps
- sqlite-vec is alpha (0.1.10-alpha.4) — API instability
- Graph traversal (edges, governs, 2-hop expansion) requires recursive CTEs or app-side joins — lbug gives this natively via Cypher
- export_graph is complex (nodes + links with filtering) — would need significant SQL
- Multi-namespace model: lbug uses separate tables per namespace; SQLite would need namespace column or separate DBs
- Performance: vector search in sqlite-vec is brute-force-ish for small datasets; HNSW not available
- Migration cost: 7.8k lines of ladybug.rs + callers; IntakeStore downcast (R23) would need rework

### Effort Estimate
- Prototype SqliteVectorStore implementing full VectorStore: 1-2 weeks
- Graph expansion porting: 3-5 days
- IntakeStore: 1-2 days
- Benchmark harness (p50/p95/p99, crash recovery): 2-3 days
- Migration + cutover: 1 week
- Total: 3-5 weeks

## Recommendation
**Do not migrate now.** Reasons:
1. sqlite-vec is still alpha; API churn risk
2. Graph traversal is the core value (governs edges, 2-hop expansion); SQLite would lose native graph support, requiring app-side joins that reintroduce complexity
3. lbug pain points are being addressed incrementally (R5, R16, R21, R25)
4. The 7.8k lines in ladybug.rs are partly due to Cypher string building (R16) which could be cleaned up without migration

## Follow-up
Migration remains a research option. Any future work will be triggered when one of these conditions is met:
- sqlite-vec reaches stable 1.0 release, OR
- lbug SIGSEGV/corruption recurs despite R5/R21 fixes, OR
- graph traversal requirements are simplified (e.g., edges removed)

If migration work is later decided, it will be tracked as a separate task with owner and concrete next actions.

## Decision
Stay on lbug 0.20.4. Reassess at next major release cycle. Migration remains a research option, not a committed path.
