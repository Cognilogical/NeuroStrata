# 2026-11-21 — Data-Connector Principal Authority on Pointer Reads (P1)

> **Baseline**: `cognilogical/main` @ `a75b243a`
> **Issue**: #348 (P1)
> **Branch**: `fix/data-connector-principal-authority`
> **Workflow**: Gate 0 reproduce + caller census → spec → red eval → minimal fix → green eval

---

## 1. Defect

`handle_data_connector` (`src/server.rs:2181`) accepts an optional
`principal` and forwards an omitted key as `None`. `execute_connector_command`
(`src/kernel/mod.rs:588`) passes that `None` to `list_data_pointers` and
`resolve_pointer`, whose port reads (`src/kernel/ports.rs:304`, `:361`)
document `None` as the trusted internal path and skip ReBAC. A caller who
omits `principal` therefore lists and resolves `visible_to`-restricted
pointers, while a caller who names a stranger is denied pre-gate. The write
path already fails closed on the same input (`can_write_namespace` maps
`None` to `""`).

## 2. Invariant (P)

Every caller-originated data-connector **pointer read** — pointer listing,
slice resolution, and workflow mediation that resolves a slice — executes
only with a named, non-blank principal.

The port's trusted `None` path remains reachable from kernel-internal reads,
which are not caller-originated output (e.g. the pre-gate record lookups at
`src/kernel/mod.rs` that fetch a pointer before deciding visibility).

## 3. External entry-point census

| Surface | Route | Path to kernel | Caller principal source |
| --- | --- | --- | --- |
| MCP tool `neurostrata_data_connector` | `process_mcp_request` → `handle_data_connector` (`src/server.rs:1954`) | `handle_data_connector` → `AgentKernel::new` → `execute_connector_command` | `arguments["principal"]`, optional |
| HTTP `POST /data-connector` | `handle_http_data_connector` (`src/daemon.rs:2174`) | same `handle_data_connector`; the transport injects no authenticated principal | request body `principal`, optional |
| CLI `neurostrata-mcp data-connector resolve\|mediate` | `src/main.rs:8122` | direct `execute_connector_command` | `--principal` flag, optional |

HTTP has no separate boundary: it shares `handle_data_connector`, so one
check covers both MCP and HTTP. The CLI bypasses the handler and needs the
same check at its dispatch site.

## 4. Decision: reject at the external boundary

`src/server.rs` gains one predicate/validator pair:

- `connector_operation_is_pointer_read(operation)` — true for
  `list_pointers`, `resolve_pointer`, and `mediate`.
- `enforce_external_connector_principal(operation, principal)` — returns an
  error naming the principal for a pointer read with `principal == None`.

`handle_data_connector` calls it before constructing `AgentKernel`; the CLI
dispatcher calls it before `execute_connector_command`. A denied read returns
`{"ok": false, "error": "principal is required ..."}` over MCP/HTTP and a
non-zero exit over CLI, before any pointer or connector I/O.

Why `mediate` is included: it forwards its principal to `resolve_pointer` and
returns the same slice contents (issue scoping names resolve/list; mediate is
the same read exposure on the same boundary, so leaving it open would retain
the bypass).

Out of scope (unchanged, per issue scoping): `apply_effect` (already fails
closed), pool listing/description, and any non-data-connector surface.

## 5. Write path is unchanged

`apply_effect` with an omitted principal keeps reaching
`apply_external_effect`, where `can_write_namespace(..., None)` denies and the
pre-gate rejects it. The new boundary check does not short-circuit writes, so
their existing error shape (`Pre-gate denied write-back`) and zero-I/O
property are preserved.

## 6. Red evaluations (committed before the fix)

`src/server.rs` tests, all through `handle_data_connector` (the MCP/HTTP
boundary) over `LadybugStore::for_testing`:

1. `data_connector_pointer_reads_require_principal` — a seeded
   `visible_to: ["alice"]` pointer is not listed and not resolved when
   `principal` is omitted; the error names `principal`; the payload never
   contains the pointer id, URI, or record content. The resolve arm uses a
   persisted `memory_pool` pool so the pre-fix code returns the restricted
   record instead of failing earlier on an unregistered connector.
2. Named owner `alice` lists the pointer and resolves the restricted record.
3. Named stranger `mallory` lists nothing and is denied resolution.
4. Write path: `apply_effect` with an omitted principal is still denied with
   the pre-gate error (matches pre-fix behavior).

`src/kernel/mod.rs` tests keep the internal trusted `None` contract pinned:
`execute_connector_command(..., None)` remains available to internal callers
and is not the external contract.

## 7. Non-goals

- Transport authentication: principals remain caller-asserted; this change
  removes only the omitted-principal trust escalation.
- `#331` write-back persistence and `#296` import-time ACL loss.
- Port signature changes: `Option<&str>` stays; only caller-originated
  omission is rejected.

## 8. Verification

- `cargo test --bin neurostrata-mcp` (full binary suite; report counts).
- Targeted red/green: `cargo test --bin neurostrata-mcp data_connector_`.
- Serial rerun only if the known parallel `LadybugStore` mmap flake appears.
