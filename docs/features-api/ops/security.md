# Security Architecture & OWASP LLM Top-10 Hardening

> **Why you care:** Giving an AI agent persistent memory and tool access should never open a backdoor into your machine or leak secrets into an embedding store. NeuroStrata enforces defense-in-depth across network binding, path traversal, fail-closed secret rejection, query escaping, and destructive-tool isolation.

---

## 1. Defense-in-Depth Summary

| Threat Vector (OWASP LLM Top-10)                   | NeuroStrata Mitigation                                                                                                                                                                                                                                                                                      | Source Module                                                      |
| :------------------------------------------------- | :---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | :----------------------------------------------------------------- |
| **Unauthorized Network Access**                    | Loopback-only HTTP binding (`127.0.0.1:34343`) + mandatory 256-bit Bearer Token (`~/.config/neurostrata/daemon.token`, `0600` permissions).                                                                                                                                                                 | `src/server.rs`, `src/main.rs`                                     |
| **Arbitrary Path Traversal (`J6`)**                | Caller-Path Authority (`check_caller_path`) canonicalizes every path passed to `/ingest`, `/backup`, `/export-graph`, `/tasks/import`, and the root passed to `/cli/root` (`hooks install` and `root set` setting a cached root), restricting access to `startup_root` or explicit `allowed_external_dirs`. | `src/server.rs`, `src/config.rs`                                   |
| **Secret & Credential Leakage**                    | A fail-closed secret scanner **rejects** the write outright when the payload contains a credential, so sensitive material never reaches a vector index, the Episodic Buffer, or a Git-shared SynapticMesh record.                                                                                           | `src/secrets.rs`, `src/buffer.rs`                                  |
| **Cypher / SPARQL Injection**                      | Strict string escaping (backslashes escaped prior to quotes) and validated identifier checks across all LadybugDB Cypher and SPARQL (`spargebra` / `sparopt`) queries.                                                                                                                                      | `src/store/ladybug.rs`, `src/rdf/sparql.rs`                        |
| **Destructive Agent Hallucinations**               | Zero destructive MCP tools: `delete`, `move`, and `restore` do not exist on the MCP JSON-RPC surface and can only be invoked by an operator via the CLI.                                                                                                                                                    | `src/server.rs`, `src/main.rs`                                     |
| **Unsafe Shell / Action Execution**                | Prefrontal Cortex intercepts candidate actions via `neurostrata_guard_validate`, evaluates semantic guard rules, and supports Podman-sandboxed verification.                                                                                                                                                | `src/guard/`                                                       |
| **Webview Code Injection / Unbounded URL Opening** | A strict five-directive Content Security Policy plus a single, scoped `shell:allow-open` capability covering only web (`http(s)://`) links.                                                                                                                                                                    | `src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json` |

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

---

## 6. Webview Content Security Policy & Shell Open Capability (`#413`)

The Tauri desktop shell renders graph content that originates from ingested repositories and agent-written memories. Such content is untrusted input, so the webview runs under a strict, closed policy rather than Tauri's permissive default (`csp: null`).

### 6.1 Shipped CSP

`src-tauri/tauri.conf.json` pins exactly five directives:

```text
default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' http://127.0.0.1:34343
```

- `default-src 'self'` and `script-src 'self'` deny remote and inline scripts, so injected memory text cannot execute as code.
- `style-src 'self' 'unsafe-inline'` is the single retained relaxation: the bundled UI applies computed inline styles (graph highlighting and layout), which do not execute script.
- `img-src 'self' data:` permits bundled art and inline `data:` images while rejecting remote image beacons.
- `connect-src 'self' http://127.0.0.1:34343` is deliberately loopback-only: the only network peer the webview may reach is the local NeuroStrata daemon, so a compromised renderer cannot exfiltrate graph or memory data to a remote host.

`tests/tauri_config_shape.rs` pins the policy byte-for-byte (including the exact directive count and `connect-src` token set), so a future edit that widens the CSP fails CI instead of silently shipping.

### 6.2 Scoped `shell:allow-open`

`src-tauri/capabilities/default.json` declares exactly one `shell:allow-open` permission, scoped to:

```json
[
  { "url": "^https?://.*" }
]
```

The only retained target is the web: links the visualizer emits for documentation and external references. The previously duplicated bare `"shell:allow-open"` entry, the `^file://.*` and `^/.*` local-launch patterns, and branded editor deep-link schemes are removed: they turned the opener into an arbitrary local-file or executable launcher reachable from untrusted node content. `src-tauri/gen/schemas/capabilities.json` mirrors this capability and is kept in sync.

`plugins.shell.open` is `false` in `src-tauri/tauri.conf.json`: external URLs go through `open_external`, whose Rust allowlist is the enforced gate, so plugin-shell's own `open` command stays disabled and `tests/tauri_config_shape.rs` pins the key to `false` (absence would be read as `true` by tauri-plugin-shell).

### 6.3 Port override and dev-mode caveats

- **`NEUROSTRATA_DAEMON_PORT`:** the CSP hardcodes the default daemon origin `http://127.0.0.1:34343`. If the daemon port is overridden through `NEUROSTRATA_DAEMON_PORT`, the webview's `connect-src` will block the overridden origin; update the CSP and `tests/tauri_config_shape.rs` together, or keep the daemon on `34343` for the desktop app.
- **Dev CSP:** `devUrl` serves the UI from `http://localhost:5173`, whose Vite HMR websocket (`ws://localhost:5173`) is not permitted by the production policy. `app.security.devCsp` is therefore configured separately: it is the production policy plus only `http://localhost:5173` and `ws://localhost:5173` in `connect-src`, so HMR works under `tauri dev` while the release build keeps the strict policy above (never loosen the shipped `csp` for development). `tests/tauri_config_shape.rs` pins this relationship.
