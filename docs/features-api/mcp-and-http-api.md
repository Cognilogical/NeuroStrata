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
