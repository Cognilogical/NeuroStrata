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

## Central Executive Task & Procedure Tools

- `neurostrata_task_create` — create a new task.
- `neurostrata_task_list` — list tasks.
- `neurostrata_task_get` — retrieve a single task.
- `neurostrata_task_claim` — claim exclusivity over a task and stamp caller `worktree`/`branch` scope (from explicit `worktree`/`branch`/`project_root` arguments or the namespace's registered `project_root`).
- `neurostrata_task_update` — update status/notes on a task.
- `neurostrata_task_complete` — finish a task (requires extracted memory → Lock 2).
- `neurostrata_task_validate` — advisory gate report for a task.
- `neurostrata_procedure_perform` — fire a recurring procedure.
- `neurostrata_guard_validate` — validate PreToolUse actions against behavioral rules and Zero-Action Start (`git push`/`pull`/`fetch`/`status`/`log`/`diff`/`show`/`rev-parse` without file-output or exec-override flags like `--output`/`-c`, plus `neurostrata-mcp task` commands, are exempt from requiring an `in_progress` task so post-completion pushes pass without skipping the gate).

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

- `neurostrata_data_connector` — execute an `AgentKernel` connector command: `list_connectors`, `describe_pool`, `register_pointer`, `list_pointers`, `resolve_pointer`, `apply_effect`, `mediate` (runtime dispatch also accepts `register_pool`, `upsert_pool`, and `list_pools`). The MCP `tools/list` disclosure for its `args` object is operation-discriminated (`oneOf` on `operation`): each variant lists its own required fields and the nested `DataPointerRecord`, `DataSliceRequest`, and `ExternalEffectRequest` layouts. Omitted `namespace` defaults to `"global"` and omitted `principal` defaults to `None`, while present non-string or blank/whitespace `namespace` and `principal` values are rejected at the handler boundary with `{"ok": false, "error": ...}` before constructing `AgentKernel`. Optional `now_secs` (u64) on `resolve_pointer`, `apply_effect`, and `mediate` defaults to the current epoch when omitted; the schema is discovery-only and runtime validation is unchanged.

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
