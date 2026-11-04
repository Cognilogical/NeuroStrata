# Security Architecture & OWASP LLM Top-10 Hardening

> **Why you care:** Giving an AI agent persistent memory and tool access should never open a backdoor into your machine or leak secrets into an embedding store. NeuroStrata enforces defense-in-depth across network binding, path traversal, secret scrubbing, query escaping, and destructive-tool isolation.

---

## 1. Defense-in-Depth Summary

| Threat Vector (OWASP LLM Top-10) | NeuroStrata Mitigation | Source Module |
| :--- | :--- | :--- |
| **Unauthorized Network Access** | Loopback-only HTTP binding (`127.0.0.1:34343`) + mandatory 256-bit Bearer Token (`~/.config/neurostrata/daemon.token`, `0600` permissions). | `src/server.rs`, `src/main.rs` |
| **Arbitrary Path Traversal (`J6`)** | Caller-Path Authority (`check_caller_path`) canonicalizes every path passed to `/ingest`, `/backup`, `/export-graph`, and `/tasks/import`, restricting access to `startup_root` or explicit `allowed_external_dirs`. | `src/server.rs`, `src/config.rs` |
| **Secret & Credential Leakage** | Pre-ingestion and pre-log regex scrubber redacts API keys, bearer tokens, private keys, AWS/GitHub tokens, and connection strings before embedding or disk writes. | `src/scrubber.rs`, `src/buffer.rs` |
| **Cypher / SPARQL Injection** | Strict string escaping (backslashes escaped prior to quotes) and validated identifier checks across all LadybugDB Cypher and OxiRS SPARQL queries. | `src/store/ladybug.rs`, `src/rdf/sparql.rs` |
| **Destructive Agent Hallucinations** | Zero destructive MCP tools: `delete`, `move`, and `restore` do not exist on the MCP JSON-RPC surface and can only be invoked by an operator via the CLI. | `src/server.rs`, `src/main.rs` |
| **Unsafe Shell / Action Execution** | Prefrontal Cortex intercepts candidate actions via `neurostrata_validate_action`, evaluates semantic guard rules, and supports Podman-sandboxed verification. | `src/guard/mod.rs` |

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

## 4. Automatic Secret Scrubbing

Before any memory node is embedded into LadybugDB or appended to the rolling Episodic Buffer (`.NeuroStrata/sessions/*.jsonl`), NeuroStrata passes the payload through its secret-scrubbing pipeline. High-entropy credentials, PEM private keys, `sk-*` / `ghp_*` / `AKIA*` tokens, and inline passwords in URLs are replaced with `[REDACTED]` markers so sensitive material never enters vector indices or Git-shared SynapticMesh records.

---

## 5. Non-Destructive MCP Surface & Bi-Temporal Auditability

Autonomous agents can create, search, relate, and supersede memories (`neurostrata_add_memory`, `neurostrata_supersede_memory`, `neurostrata_archive_memory`), which preserve full historical provenance via `valid_from` and `valid_to` timestamps. Permanent row deletion (`neurostrata-mcp delete`) is physically omitted from the MCP server schema so a prompt-injected or hallucinating agent cannot wipe project knowledge.
