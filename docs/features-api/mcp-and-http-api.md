# MCP Tools & HTTP Daemon API Reference

## Core Memory & Graph Retrieval Tools

- `neurostrata_add_memory` — store an architectural rule, project pattern, or task insight.
- `neurostrata_search_memory` — query memory contents via vector + graph RAG.
- `neurostrata_get_snapshot` — fetch current task state snapshot (<=800 chars).
- `neurostrata_supersede_memory` — replace active version with new one atomically.
- `neurostrata_memories_for_file` — inspect governing memories for a given path/file.
- `neurostrata_related` — find related entities (`CONTAINS`, `GOVERNS`).
- `neurostrata_diff_tour` — diff tour between versions.
- `neurostrata_ingest_directory` — ingest a directory into the property graph.
- `neurostrata_append_log` — append conversational transcript log entries.
- `neurostrata_bootstrap` — initialize a new project namespace.
- `neurostrata_task_setup` — configure task scaffolding.
- `neurostrata_call_hierarchy` — traverse the knowledge hierarchy.
- `neurostrata_find_references` — locate referenced entities.
- `neurostrata_find_duplicate_candidates` — locate duplicate memory candidates.

`neurostrata_memories_for_file` (and the file part of `neurostrata_context_plan`) accepts an absolute file path as well as one relative to the ingested root. An absolute path is made relative to the caller's `project_root` when it contains the file, else to the namespace's registered project root when that root exists on this machine, else to the nearest ancestor that holds `.git` (a directory, or the `.git` file of a linked worktree). A nested repository inside the root rejects only that known root and falls through to another known root before the `.git` fallback; when no valid known root remains, it is left as given and matches nothing. A linked worktree under a project registered at a subdirectory, and a file in another repository, are left as given and match nothing when a known root exists; without one they fall back to `.git`. A linked worktree of a project registered at its checkout root resolves against that worktree. A supplied `project_root` constrains that fallback even when it is unusable on this machine (for example a moved root), so a file in another repository still stays `Outside`; an unusable registered root is ignored. Path comparison normalizes separators and drive letters, not case; the file system decides case through each path's canonical form. The store is also queried with the caller's absolute spelling, and a relative path also joined onto a known root, so memories stored with absolute paths are still found; stored memories are not rewritten. Hits from every spelling are merged by memory id and cut to the requested `limit` (at most 20); the file's symbols and imports in the reply are read with the same spellings. `..` is resolved on disk, and a symlinked file is looked up both under its target and as the caller spelled it. The response still echoes `path`; for an absolute path it adds `resolved_path` and `resolved_from` (`project_root`, `registry_root`, `marker` or `outside`). `project_root` still selects the ingest generation; a root found by the lookup does not.

## Central Executive Task & Procedure Tools

- `neurostrata_task_create` — create a new task.
- `neurostrata_task_list` — list tasks.
- `neurostrata_task_get` — retrieve a single task.
- `neurostrata_task_claim` — claim exclusivity over a task.
- `neurostrata_task_update` — update status/notes on a task.
- `neurostrata_task_complete` — finish a task (requires extracted memory → Lock 2).
- `neurostrata_task_validate` — advisory gate report for a task.
- `neurostrata_procedure_perform` — fire a recurring procedure.

## Code Intelligence & SCIP Call-Graph Tools

- `neurostrata_Export_scip` — export SCIP code graph data.
- `neurostrata_import_scip` — import SCIP code graph data.
- `neurostrata_graphql_query` — execute GraphQL queries against the ontology.
- `neurostrata_sparql_query` — run SPARQL 1.1 ask offline.

## Enterprise Ontology, Digital Twins, Scenarios & Kinetic Actions

- `neurostrata_twin` — manage digital twin state.
- `neurostrata_scenario` — define counterfactual scenarios.
- `neurostrata_action` — apply kinetic actions (state-machine mutations) via `POST /action`.

## Central Agentic Kernel & DataConnector Tools

- `neurostrata_data_connector` — execute an `AgentKernel` connector command: `list_connectors`, `describe_pool`, `register_pointer`, `list_pointers`, `resolve_pointer`, `apply_effect`, `mediate`.

## Subject-Matter Domain Expertise & SKOS Taxonomy

- `neurostrata_taxonomy` — set up SKOS taxonomy (`set_thesis`, `upsert_concept`, `advise`, `validate_taxonomy_dag`).

## HTTP Daemon Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/mcp` | Root MCP server entry point |
| POST | `/ingest` | Ingest files/directories into the property graph |
| POST | `/backup` | Backup current state to archive |
| POST | `/export-graph` | Export the full knowledge graph (RDF/SPARQL) |
| POST | `/tasks/gate` | Run documentation gate checks |
| GET | `/twin` | Digital twin state retrieval |
| POST | `/scenario` | Create a counterfactual scenario |
| POST | `/action` | Apply a kinetic action (state machine mutation) |
| POST | `/data-connector` | Execute a DataConnector command (list/describe/register-pointer/resolve/apply-effect/mediate) |
| GET | `/taxonomy` | Query SKOS concept graph |
| POST | `/graphql` | Execute GraphQL queries over the ontology |

All endpoints are served by the **neurostrata-mcp daemon** on `127.0.0.1:34343`. Tauri acts as a lightweight HTTP client; Cursor MCP integration proxies stdio JSON-RPC requests.

`POST /mcp` follows JSON-RPC 2.0: a method the server does not implement gets error `-32601` (method not found) with the request's `id`, `ping` gets an empty result, and a notification (no `id`) gets `202 Accepted` with an empty body. For an unknown method, an explicit `"id": null` is a request id, not a notification: it receives the `-32601` response above with `"id": null`; only an omitted `id` suppresses the reply.

## Storage Status Surfaces

`storage status` has two provenance layers since #212. The path-only (no-open) surface — used by the CLI's dispatch and by callers that must not open the store — reports filesystem and store-state sidecar facts: `wal_bytes`, `wal_age_secs` (age of the active WAL, regular files only), `checkpoint_failures`, and the persisted `last_recovery { at, kind, wal_bytes }` history. The live-handle surface additionally owns `opened_via_recovery` and its kind *exclusively*: a verified live store never reports recovery because the sidecar holds a `last_recovery` entry. Identity writes (`has_data`, descriptor) and health writes (checkpoint health) are split (`write_store_state_identity` vs. the health writers), and a fresh process still reports the last real checkpoint because `record_checkpoint_outcome` persists to the sidecar (best effort, never propagated).
