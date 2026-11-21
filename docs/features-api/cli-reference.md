# 💻 NeuroStrata CLI Reference (`neurostrata-mcp`)

NeuroStrata provides a standalone compiled CLI binary (`neurostrata-mcp`) alongside its HTTP/stdio MCP daemon mode for direct administration, graph inspection, task-gate verification, team mesh synchronization, and local model lifecycle management.

> [!WARNING] > **Single-Writer Database Lock:** LadybugDB enforces single-process write access. Commands that mutate the store directly on disk (`delete`, `move`, `add`, `edit`, `archive`) require the daemon to be stopped (`neurostrata-mcp shutdown`), whereas daemon-proxied commands (`status`, `doctor`, `bus-metrics`, `backup`, `ingest`, `export-graph`, `task import`) communicate with a live daemon automatically when one is running.

---

## 1. Core Daemon & Setup Commands

### `daemon`

Starts the NeuroStrata MCP server (HTTP loopback on `127.0.0.1:34343` and/or stdio transport). Only one daemon instance may hold the store lock at a time. File-governance MCP calls accept absolute paths, resolving equivalent relative and absolute spellings against the caller and registered project roots; see [MCP & HTTP API](mcp-and-http-api.md).

```bash
neurostrata-mcp daemon
```

### `status`

Read-only preflight check reporting store health, lock holder, and whether the running daemon matches the on-disk binary build.

```bash
neurostrata-mcp status
```

- **Exit `0`:** Healthy daemon running the current build.
- **Exit `1`:** No daemon running; lock is free.
- **Exit `2`:** Lock held but daemon is busy or shutting down.
- **Exit `3`:** Stale daemon build running; restart with `neurostrata-mcp shutdown && neurostrata-mcp daemon`.

### `shutdown`

Gracefully checkpoints the database and stops the running daemon.

```bash
neurostrata-mcp shutdown
```

### `setup`

Installs or refreshes the user-level background service (`systemd` on Linux, `launchd` on macOS) and initializes default configuration under `~/.config/neurostrata/`.

```bash
neurostrata-mcp setup
```

---

## 2. Namespace & Memory Management

### `namespaces`

Lists all initialized memory namespaces in the database.

```bash
neurostrata-mcp namespaces
```

### `list`

Prints all active memory nodes stored in a namespace.

```bash
neurostrata-mcp list <namespace>
```

### `add`

Directly embeds and stores a new memory node in a namespace.

```bash
neurostrata-mcp add <namespace> <type> <content> [location]
```

_Example:_

```bash
neurostrata-mcp add my-rust-project rule "Avoid unwrap() in library modules" "src/lib.rs"
```

### `edit`

Modifies an existing memory node's namespace, content, and location anchor.

```bash
neurostrata-mcp edit <namespace> <id> <new_namespace> <content> <location>
```

### `archive`

Tombstones a memory by ID (`metadata.archived: true`) without deleting its historical audit record. Refuses `global` without `--allow-global`.

```bash
neurostrata-mcp archive <namespace> <id> [--allow-global]
```

### `move`

Copies a memory node into a target namespace and removes the original row. Used to consolidate split namespace spellings reported by `doctor`.

```bash
neurostrata-mcp move <source_namespace> <id> <target_namespace>
```

### `delete`

Permanently deletes a single memory node by ID. Deliberately restricted to the CLI so an autonomous agent cannot invoke it via MCP.

```bash
neurostrata-mcp delete <namespace> <id>
```

---

## 3. Codebase Ingestion & Graph Export

### `ingest` & `ingest-scip`

Scans a source directory with the six-language Tree-sitter AST engine (`rust`, `typescript`, `tsx`, `python`, `go`, `java`), extracts structural symbols (`function`, `class`, `struct`, `interface`, `trait`, `method`, `calls`, `imports`), and indexes them into the namespace, or ingests a precomputed SCIP index (`ingest-scip`).

```bash
neurostrata-mcp ingest <dir_path> <namespace> [schema_path]
neurostrata-mcp ingest-scip <scip_path> <namespace>
```

_Example:_

```bash
neurostrata-mcp ingest ./src my-rust-project
```

### `project-context` & `export-graph`

Binds stable source identifiers to `.NeuroStrata` project markers and exports the relational memory graph as JSON for the Web UI and Obsidian canvas.

```bash
neurostrata-mcp project-context bind <source-id> <marker-path>
neurostrata-mcp project-context list
neurostrata-mcp project-context unbind <source-id>

neurostrata-mcp export-graph [output_json_path] [--source-id <id>] [--exclude-superseded] [--include-archived]
```

- Enforces the **Export-Freshness Gate**: refuses when a namespace has unexported mutations marked `dirty: true` until reconciled.
- Excludes tombstoned (`archived: true`) rows by default unless `--include-archived` is passed.

---

## 4. Central Executive Task Gates & Hooks

### `task gate`

Runs the Central Executive enforcement check over a namespace. Executed automatically by `.git/hooks/pre-push`; a missing checker or non-zero gate result now blocks the push.

```bash
neurostrata-mcp task gate <namespace> [--strict] [--self-test]
```

- **Exit `0`:** Clean; zero blocking violations.
- **Exit `1`:** Blocking violation detected (`in_progress` task, `done` task without an `EXTRACTED_FROM` memory, or `P0` task open > 24h).
- **Exit `2`:** Infrastructure error (blocks when `--strict` is set).
- `--self-test`: Verifies the gate engine against an in-memory planted-violation fixture without touching the database.

### `task validate`

Advisory twin of `task gate`. Prints the full JSON/human readiness report (`violations`, `stale`, `unextracted_done`, `dangling_edges`, `counts`) and always exits `0`.

```bash
neurostrata-mcp task validate <namespace>
```

### `task import`

Idempotently imports legacy JSONL task records into the Central Executive store.

```bash
neurostrata-mcp task import <namespace> --from-beads <path>
```

### `hooks install`

Installs `.git/hooks/pre-push` (strict task gate) and `.git/hooks/pre-commit` (graph-query ledger) plus detected agent harness plugins.

```bash
neurostrata-mcp hooks install [--force]
```
The pre-push hook gates the project's configured namespace (see `namespace resolve`) and the checkout's folder name; the pre-commit hook records against the configured namespace. The pre-push hook blocks the push when `neurostrata-mcp` is missing, when `namespace resolve` fails, or when the gate exits with anything other than 0 or 1.

### `namespace resolve`
Prints the namespace configured for a project. Order: `NEUROSTRATA_NAMESPACE` in `.claude/settings.local.json` (its `env` block, else a hook command in the installer's exact form `NEUROSTRATA_NAMESPACE=<ns> node "<runner>"` that runs the installer's `.NeuroStrata/plugins/claude/cli-hook.mjs`), then `namespace` in the opencode, codex, hermes, openclaw or GitHub CLI plugin config, read from the checkout, else from the main checkout of a linked worktree; then `NEUROSTRATA_NAMESPACE` in the environment (surrounding quotes dropped); then the `namespace` field of `.NeuroStrata/session_queries.json`; the main checkout's folder name last (outside git, the same files in `dir` itself, then its folder name). A namespace is letters, digits, `-`, `_`, `.` and `:`, starting with a letter or digit; any other value is skipped, and a config value in quotes is not one. When harness configs name different namespaces, it prints a warning on stderr and uses the first in the order above (Claude, opencode, codex, hermes, openclaw, GitHub CLI). It exits non-zero with an error on stderr, so the `pre-push` hook blocks the push, when `dir` does not exist, when a harness config exists but cannot be read or parsed (a UTF-8 byte order mark included; the error names the file), or when nothing names a namespace and the folder name is not one (a space in it, say). A relative `dir` is taken against the current directory. It opens no database and starts no daemon. The `pre-push` and `pre-commit` hooks use it; the `pre-commit` hook also sets the ledger's `namespace` field to its answer.

`hooks install`, `get_snapshot`'s hook refresh and the daemon's startup install refresh the harness plugin files. They write a namespace into a harness config only when it came from a harness config or, for `hooks install` and the CLI, `NEUROSTRATA_NAMESPACE` (the daemon does not read its own environment: one daemon serves every project); a ledger or folder-name answer is never written there. Each config keeps a usable namespace it already names, a config that cannot be read is left as it is, and a config with no namespace to write is not written. Config files are written through a temporary file and a rename.
```bash
neurostrata-mcp namespace resolve [dir]
```

### `root show`
Read-only. Prints the project marker `.NeuroStrata/config.json` that applies to `dir` (default: the current folder) and how it was found, its namespace and `source_id`, whether git tracks it (`unknown` when git fails), the cached root, other markers below the marker root (in a linked worktree also those only in the main checkout, labeled), and project-context bindings to missing folders (`unknown` when the user config cannot be read). It notes a `.NeuroStrata` folder spelled in another letter case, which is not a marker. It starts no daemon; with no daemon it takes the store lock and holds it while it reads the store.
```bash
neurostrata-mcp root show [dir]
```
The marker is strict JSON: `version` 1, `namespace`, and optionally `source_id` (a string, not `null`); `src/schemas/root-config.schema.json` documents it. A new marker is published with a hard link, so the filesystem must support hard links (exFAT and FAT do not). The home folder, a filesystem or drive root, and a folder outside git with a checkout below it may not hold a marker; that last check looks four levels down and skips `node_modules` and `target`.

---

## 5. SynapticMesh (Git Team Memory Sync)

Synchronizes shareable `rule`, `fact`, and `procedure` definitions across teammates over standard Git (`--ff-only` pull and push) while preserving local private sovereignty.

```bash
neurostrata-mcp synaptic-mesh init <mesh_name> [--namespace <ns>] [--remote <url>]
neurostrata-mcp synaptic-mesh join <mesh_repo_path>
neurostrata-mcp synaptic-mesh push
neurostrata-mcp synaptic-mesh pull
neurostrata-mcp synaptic-mesh status
neurostrata-mcp synaptic-mesh leave
neurostrata-mcp synaptic-mesh recover
```

---

## 6. Local Learning & Laya

### `learn`

Manages the local deduplication learning loop: datasets, candidate training, evaluation gates, promotion, and rollback.

```bash
neurostrata-mcp learn status
neurostrata-mcp learn pause
neurostrata-mcp learn resume
neurostrata-mcp learn train
neurostrata-mcp learn rollback
neurostrata-mcp learn reset
```

### `laya`

Fetches a pinned, checksum-verified Laya ONNX duplicate-judgment bundle into `artifacts/laya-bundle` by default.

```bash
neurostrata-mcp laya [MODEL] [--out-dir <dir>]
```

### GLiNER (no CLI subcommand)

GLiNER entity/relation extraction runs **inside the daemon**, not as a CLI subcommand: `neurostrata_add_memory` with `extract_entities: true` runs the local ONNX GLiNER engine (`src/gliner.rs`) and stores the extracted entities as `metadata.gliner_candidates` on the memory, each marked `review_only`. There is no `neurostrata-mcp gliner` command.

---

## 7. Diagnostics, Telemetry & Backups

### `doctor`

Read-only upgrade and schema consistency audit across one or all namespaces, with every finding labeled by `[namespace]`.

```bash
neurostrata-mcp doctor [--namespace <ns>]
```

### `bus-metrics`

Queries the Thalamic Bus event dispatcher counters (`published`, `delivered`, `dropped`, `queue_depth`) over HTTP.

```bash
neurostrata-mcp bus-metrics
```

### `backup` & `restore`

Creates a checkpointed snapshot (`CHECKPOINT` + store copy + `manifest.json`) or restores a snapshot into a new database path.

```bash
neurostrata-mcp backup <backup_dir> [--no-open]
neurostrata-mcp restore <backup_dir> --into <new_db_path>
```

`backup --no-open` copies the database, its write-ahead log, the checkpoint shadow, and the store-state sidecar directly to disk without opening or checkpointing the store. It **takes the store lock and holds it for the whole copy** (`#209`, `#241`): it refuses while another process holds the lock, and it fails closed when the lock cannot be taken at all, because an untakeable lock is not evidence that the store is free. Holding the lock across the window means a daemon that starts mid-copy cannot change the files being captured. Use a plain `backup <dir>` (daemon-coordinated) or `shutdown` first when the daemon is running.

### `semantic-eval`

Offline v1.19.0 release gate: runs versioned SPARQL competency contracts and a 60-query four-arm retrieval ablation against real temporary LadybugDB stores, then writes one strict JSON receipt.

```bash
neurostrata-mcp semantic-eval \
  --competency eval/semantic/competency.json \
  --retrieval eval/semantic/retrieval.json \
  --iterations 3 \
  --check
```

- **Committed receipts:** the strict per-suite results are committed at `eval/semantic/competency-receipt.json` and `eval/semantic/retrieval-receipt.json`.
- `--iterations >= 3` is required by the corrected timing contract (warmup excluded; per-query latency = median across measured iterations; arm p95 over per-query medians).
- `--check` writes the receipt and exits nonzero on any gate failure or validation/execution error (fail-closed; an error never yields an empty-result PASS). Without `--check` the receipt is written and the verdict reported but the process exits zero.
- **Strict receipt (minimum):** `schema_version`, package version, fixture ids and SHA-256 digests, `evidence_class: "synthetic_deterministic"`, `production_roi_claimed: false`, competency per-query results, all four arms' aggregate/per-query metrics and rankings, ontology-targeted Recall@5, p95 overhead, derived-storage percentage, determinism SHA-256, `gate_violations`, `not_evaluated`, and overall `verdict: "PASS" | "FAIL"`.
- **Evidence boundary:** the corpus is `synthetic_deterministic`, not owner-labeled or production. `production_roi_claimed` is `false`, and `not_evaluated` names `owner_labeled_production_roi`, `blind_agent_answer_accuracy`, `owner_labeled_corpus`, `rollback_validation`, `curation_cost`, and `real_namespace_accuracy`. No production ROI, owner-labeled-corpus, blind-agent, rollback, curation-cost, or real-namespace claim is implied.
- **Current status (honest FAIL):** the registered mixed alias+hierarchy subset yields `insufficient_ontology_subset_lift 0.042857 < 0.10`; competency queries pass 8/8; overall `verdict = "FAIL"` and both final `--check` runs exit nonzero. This is the gate failing closed, not a CLI defect. Strict receipts are complete: remediation 65/65 with zero ignored/failed, six `not_evaluated` exclusions, and identical determinism SHA-256 `5f378d7b6d2666dbaef44a06fa80de1b75f5dec2c99951559593f52db4f45583`. The 1.19.0 release record references the `--output` receipt and remediation-runner JSON; no semantic-gate PASS or production claim is made.
- **Remediation runner:** `eval/registry.json` suite `semantic-evaluation` pins the 36 named `semantic_eval_metrics_test` cases plus 17 competency and 12 ablation inline tests with `ignored: 0`. Run `EVAL_ALLOW_DIRTY=1 scripts/remediation-eval.sh semantic-evaluation` with `--json` pointed at a repo-relative path on intentionally dirty trees only; it never ignores test failures. Suite-level results are reflected in the committed `eval/semantic/competency-receipt.json` and `eval/semantic/retrieval-receipt.json`.

---

## Workflow, Documentation Gate & Ontology Commands

### `doc-gate`

Evaluates the Documentation Manifest against the live repository. Checks required files, prohibited filename substrings, and surface-diff rules.

```bash
neurostrata-mcp doc-gate [--root <path>] [--json]
```

- **Exit `0`:** All tiers pass; required files present; no prohibited substrings.
- **Exit `1`:** Manifest violation (missing required files, prohibited substrings, surface-diff failures).
- `--json`: Machine-readable report for CI integration.

### `workflow`

Queries and manipulates the pluggable workflow lifecycle state machine and connector (`GitWorkflowConnector`, `ManifestWorkflowConnector`).

```bash
neurostrata-mcp workflow status [--root <path>] [--json]
neurostrata-mcp workflow connector [--root <path>] [--json]
neurostrata-mcp workflow transitions [--json]
neurostrata-mcp workflow install [--root <path>] [--json]
neurostrata-mcp workflow install-triggers [--root <path>] [--json]
```

- `status` (alias `connector`): Inspect active connector ID, captured scope (`branch`, `worktree`), state digest (`revision_id`, `dirty_state_hash`), and `changed_paths`.
- `transitions`: Print the static 8-stage, 23-edge `WORKFLOW_TABLE` transition table.
- `install` (alias `install-triggers`): Install and verify lifecycle triggers (Git `pre-push` and `pre-commit` hooks + `.NeuroStrata/workflow-triggers.json`) for the configured `WorkflowConnector`.

### `action`

Defines, validates, applies, and reverts Kinetic Action Types.

```bash
neurostrata-mcp action define <definition.json>  # strict validation, stores ActionTypeDefinition
neurostrata-mcp action validate <action_type> <parameters.json>
neurostrata-mcp action apply <action_type> <parameters.json>
neurostrata-mcp action revert <receipt_id> [--force]
neurostrata-mcp action log [--limit 100]
```

The 5-stage fail-closed gate (ReBAC → Typed Parameters → Offline SPARQL ASK → TwinVarPrecondition → Post-Image SHACL) runs on `validate` and `apply`.

### `taxonomy`

Manages SKOS domain taxonomies and invokes the SME Frontier Advisor.

```bash
neurostrata-mcp taxonomy set-thesis <thesis.json>
neurostrata-mcp taxonomy upsert-concept <concept.json>
neurostrata-mcp taxonomy tree [--concept <iri>]
neurostrata-mcp taxonomy briefing [--json]
neurostrata-mcp taxonomy compare <other_graph>
neurostrata-mcp taxonomy gaps
neurostrata-mcp taxonomy advise [--top-k 5]
neurostrata-mcp taxonomy validate-dag
```

### `scenario`, `object-view`, `diff-tour` & `insights`

Creates and evaluates counterfactual ontology scenarios (`create`, `stage-memory`, `stage-twin`, `apply-action`, `eval`, `diff`, `plan-ontology-diff`, `commit`), polymorphic object views (`object-view`), structural diff tours (`diff-tour`), and repository architectural insights (`insights`). Additional administration commands include `set-metadata`, `normalize-metadata`, `storage`, `chat`, `metrics`, `graphql`, and `kge`.

`storage status` (no-open surface since #212) reports `wal_bytes` and `wal_age_secs` (regular files only) alongside artifact accounting, and answers "did this store ever recover?" from the store-state sidecar's `last_recovery { at, kind, wal_bytes }` history rather than from any live handle's flag; identity (`has_data`, descriptor) and health (checkpoint outcome) writes are split, and checkpoint outcomes persist to the sidecar so a fresh process reports the last real checkpoint.

```bash
neurostrata-mcp scenario create <name> [--base <twin_id>]
neurostrata-mcp scenario stage-memory <scenario_id> <memory.json>
neurostrata-mcp scenario stage-twin <scenario_id> <twin.json>
neurostrata-mcp scenario apply-action <scenario_id> <action_type> <parameters.json>
neurostrata-mcp scenario eval <scenario_id> <query.rq>
neurostrata-mcp scenario diff <scenario_id> [--against <twin_id>]
neurostrata-mcp scenario plan-ontology-diff <namespace> \
  [--scenario-id <id>] [--add-turtle <ttl>] \
  [--retract-triples <ntriples>]... \
  [--candidate-shacl-turtle <ttl>] [--principal <p>]
neurostrata-mcp object-view <namespace> <id>
neurostrata-mcp diff-tour <namespace> [--range <rev_range>]
neurostrata-mcp insights <namespace>
```

### `twin`

Manages polymorphic digital twins and telemetry ingestion.

```bash
neurostrata-mcp twin create <schema.json>
neurostrata-mcp twin ingest <twin_id> <telemetry.json>
neurostrata-mcp twin state <twin_id>
neurostrata-mcp twin history <twin_id> [--limit 50]
```

### `task`

Central Executive task lifecycle commands.

```bash
neurostrata-mcp task create <namespace> --title "<title>" [--description "<desc>"] [--priority <0-3>] [--labels <labels>] [--parent <id>] [--blocked-by <id>]
neurostrata-mcp task list <namespace> [--status <open|in_progress|blocked|done>] [--ready] [--assignee <id>]
neurostrata-mcp task claim <namespace> <id>
neurostrata-mcp task update <namespace> <id> --status <open|in_progress|blocked> [--note "<note>"]
neurostrata-mcp task complete <namespace> <id> --extracted-memory <memory_id>
neurostrata-mcp task validate <namespace>
neurostrata-mcp task gate <namespace> [--strict] [--self-test]
```

---

## 8. Data Connectors & Agent Kernel

### `data-connector`

Interact with external data pools via registered DataConnectors.

```bash
neurostrata-mcp data-connector list
neurostrata-mcp data-connector describe --connector-id <connector_id> --pool-id <pool_id>
neurostrata-mcp data-connector register-pointer --namespace <namespace> --payload @pointer.json --connector-id <connector_id> --pool-id <pool_id> --pointer-id <pointer_id> --pointer-uri <uri> --entity-type <entity_type> --captured-at-secs <captured_at_secs> --max-staleness-secs <max_staleness_secs>
neurostrata-mcp data-connector resolve --namespace <namespace> --pointer-id <pointer_id> --offset <offset> --limit <limit> --max-bytes <max_bytes> --now-secs <now_secs>
neurostrata-mcp data-connector apply-effect --namespace <namespace> --pointer-id <pointer_id> --operation <operation> --payload @effect.json --action-receipt-id <action_receipt_id> --idempotency-key <idempotency_key> --expected-source-sha256 <expected_source_sha256> --now-secs <now_secs>
neurostrata-mcp data-connector mediate --namespace <namespace> --from-stage <from_stage> --to-stage <to_stage> --gate-passed <bool> --pointer-id <pointer_id> --offset <offset> --limit <limit> --max-bytes <max_bytes> --now-secs <now_secs>
```

---

## 9. Vault-LD Markdown Sync

### `vault-sync`

Bidirectional W3C YAML-LD 1.0 / Vault-LD sync between a namespace and a Markdown vault, with Open JSON Canvas 1.0 topology export. Positional arguments, in order: `action` (`export`|`import`), `namespace`, `vault_path`, `output_dir`, optional `principal`.

```bash
neurostrata-mcp vault-sync export NeuroStrata /path/to/vault /path/to/out [principal]
neurostrata-mcp vault-sync import NeuroStrata /path/to/vault /path/to/out [principal]
```

Export writes one YAML-LD Markdown note per memory readable by `principal` (ReBAC `can_read_memory`; default `default`), plus `topology.canvas` (Open JSON Canvas 1.0) under `output_dir`. Import secret-scans and SHACL-validates (`Mode::Native`) every `*.md` BEFORE any store write — violating notes are skipped and reported in `errors`, never partially stored; each stored note passes a ReBAC `can_write_memory` check. Unresolved `[[WikiLinks]]` materialize as `stub: true` notes. `vault_path`/`output_dir` resolve under the process cwd (CLI/MCP J6 boundary); the HTTP daemon instead validates against its startup root and `allowed_external_dirs`. The same behavior is available over HTTP as `POST /vault-sync`. See [`ops/obsidian.md`](ops/obsidian.md).
