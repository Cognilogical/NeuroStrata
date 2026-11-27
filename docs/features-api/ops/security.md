# Security Architecture & OWASP LLM Top-10 Hardening

> **Why you care:** Giving an AI agent persistent memory and tool access should never open a backdoor into your machine or leak secrets into an embedding store. NeuroStrata enforces defense-in-depth across network binding, path traversal, fail-closed secret rejection, query escaping, and destructive-tool isolation.

---

## 1. Defense-in-Depth Summary

| Threat Vector (OWASP LLM Top-10) | NeuroStrata Mitigation | Source Module |
| :--- | :--- | :--- |
| **Unauthorized Network Access** | Loopback-only HTTP binding (`127.0.0.1:34343`) + mandatory 256-bit Bearer Token (`~/.config/neurostrata/daemon.token`, `0600` permissions). | `src/server.rs`, `src/main.rs` |
| **Arbitrary Path Traversal (`J6`)** | Caller-Path Authority (`check_caller_path`) canonicalizes every path passed to `/ingest`, `/backup`, `/export-graph`, `/tasks/import`, and the root passed to `/cli/root` (`hooks install` and `root set` setting a cached root), restricting access to `startup_root` or explicit `allowed_external_dirs`. | `src/server.rs`, `src/config.rs` |
| **Secret & Credential Leakage** | A fail-closed secret scanner **rejects** the write outright when the payload contains a credential, so sensitive material never reaches a vector index, the Episodic Buffer, or a Git-shared SynapticMesh record. | `src/secrets.rs`, `src/buffer.rs` |
| **Cypher / SPARQL Injection** | Strict string escaping (backslashes escaped prior to quotes) and validated identifier checks across all LadybugDB Cypher and SPARQL (`spargebra` / `sparopt`) queries. | `src/store/ladybug.rs`, `src/rdf/sparql.rs` |
| **Destructive Agent Hallucinations** | Zero destructive MCP tools: `delete`, `move`, and `restore` do not exist on the MCP JSON-RPC surface and can only be invoked by an operator via the CLI. | `src/server.rs`, `src/main.rs` |
| **Unsafe Shell / Action Execution** | Prefrontal Cortex intercepts candidate actions via `neurostrata_guard_validate`, evaluates semantic guard rules, and supports Podman-sandboxed verification. | `src/guard/` |

### 1.1 Canonical Namespace & ID Validation (DR-1 / DR-2)

Namespaces and memory ids are the store's addressing scheme, so one validator in `src/domain.rs` guards every entry point instead of each surface testing its own shape.

- **DR-1 namespaces:** 1–64 ASCII characters from `[A-Za-z0-9._-]`, starting with an ASCII letter or digit. `:` is banned so a qualified id always splits at its first `::`, and a leading letter or digit prevents CLI-option injection (`-x`, `--help`) and path-step spellings (`.`, `..`).
- **DR-2 ids:** a bare id is 1–128 ASCII characters from `[A-Za-z0-9._:-]`. A qualified id `<namespace>::<path>` validates compositely: the prefix passes DR-1, the path is non-empty, and the path contains no single quote, double quote, backslash, or control character. Spaces and `/` remain valid in the path because file-path ids need them.
- **Typed refusals:** `ValidationError::Namespace` and `ValidationError::Id` render `ERROR [NAMESPACE]: …` and `ERROR [ID]: …`, naming both the rejected value and the rule it broke.
- **Uniform enforcement:** the MCP server, `neurostrata_get_memory`, the task tools, `LadybugStore::{upsert, relocate, commit_action_batch}`, and SynapticMesh import run the same checks before any store access or write. The project root marker and harness-plugin namespace checks delegate to the same rule, and `src/schemas/root-config.schema.json` mirrors it. Qualified ids must name the namespace they address; an existing row does not excuse a foreign prefix. Legacy colon-bearing root-marker namespaces and bare file-path ids remain readable and updatable, but caller-supplied namespaces are strictly validated before catalog access and new writes use the strict shapes.

---

## 2. Loopback Daemon & Bearer Token Authentication

When running in HTTP daemon mode, `neurostrata-mcp` binds exclusively to `127.0.0.1` (default port `34343`). On startup, it generates or loads a high-entropy bearer token at `~/.config/neurostrata/daemon.token` with strict `0600` file permissions. Every mutating or read endpoint (`/mcp`, `/ingest`, `/backup`, `/export-graph`, `/tasks/import`, `/bus/metrics`) verifies the `Authorization: Bearer <token>` header.

---

## 3. Caller-Path Authority (`J6`)

Because multiple workspaces can share a single user-level daemon, NeuroStrata enforces strict path boundaries via `check_caller_path`:
- Every caller-supplied filesystem path is resolved to its canonical absolute path (`std::fs::canonicalize`), defeating `..` traversal and symlink escapes.
- The canonical path must reside inside the daemon's `startup_root` or an explicit directory listed in `allowed_external_dirs` within `~/.config/neurostrata/config.json`.
- Any path outside those boundaries is rejected before touching the filesystem.

---

## 4. Automatic Secret Rejection (Fail-Closed)

Before any memory node is embedded into LadybugDB or appended to the rolling Episodic Buffer (`.NeuroStrata/sessions/current.md`), NeuroStrata runs the payload through a fail-closed secret scanner (`src/secrets.rs`). The scanner compiles its regexes once, and **rejects the write outright** — it does not redact and does not silently drop the secret. There is no `[REDACTED]` substitution anywhere in the codebase.

The scanner recognizes:
- **Provider token forms** — `sk-ant-*`, `sk-*`, `sk-proj-*`, `ghp_` / `gho_` / `ghs_` / `ghr_` / `github_pat_*`, `xoxb-` / `xoxp-` / `xoxr-` / `xoxs-`, `AKIA*` — validated against per-provider length and character-class rules rather than a bare prefix match.
- **Key/value credentials** — `api_key`, `apikey`, `secret`, `token`, `password`, `passwd`, `private_key`, `client_secret`, `access_token`, `refresh_token`, `bearer` assigned a value.
- **JWTs** — three base64url segments separated by dots.
- **High-entropy assignments** — secret-like keys whose value is a long opaque token with no whitespace or path separators.

Rejection categories are `provider_token`, `jwt`, `password`, `high_entropy_secret`, `oversized_input`, and `max_depth_exceeded`; each rejection names the offending location (for example `add_memory.content` or `add_memory.metadata.governs`). The scanner is invoked at the write entry points `neurostrata_add_memory`, `neurostrata_supersede_memory`, `neurostrata_append_log`, `neurostrata_task_complete`, mesh import, and ingest dry-run. (`neurostrata-mcp edit` — the operator-side CLI edit — scans the same way.)

---

## 5. Non-Destructive MCP Surface & Bi-Temporal Auditability

Autonomous agents can create, search, relate, and supersede memories (`neurostrata_add_memory`, `neurostrata_supersede_memory`, `neurostrata_archive_memory`), which preserve full historical provenance via `valid_from` and `valid_to` timestamps. Permanent row deletion (`neurostrata-mcp delete`) is physically omitted from the MCP server schema so a prompt-injected or hallucinating agent cannot wipe project knowledge.
