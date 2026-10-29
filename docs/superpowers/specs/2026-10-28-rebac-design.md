# ReBAC (Relationship-Based Access Control) Design

> Status: revised per Argon review — pending re-review.
> Task: `neurostrata-azjv` (ReBAC per-row access predicates, OxiRS §1.14).
> Companion: `docs/superpowers/specs/2026-10-27-neurostrata-rdf-semantic-layer-design.md`, `docs/superpowers/specs/2026-10-28-did-signed-mesh-records-design.md`.

## 1. Problem

`src/rebac.rs` is a broken placeholder: `allowed` grants any non-empty user access to any `viewer`/`editor`/`owner` relation without consulting an ACL, and `check` uses naive substring matching (spoofable). ReBAC is never wired into search, export, SPARQL, or MCP handlers. OxiRS §1.14 requires optional per-row access predicates; this spec defines them and fixes the helpers.

## 2. Goal

Add **optional per-row relationship-based access control**:

- `metadata.visible_to: Vec<String>` declares which principals may read a row.
- Correct `allowed`/`check` semantics (no blanket grants, no substring spoofing).
- Enforcement at search, export, SPARQL, and MCP read paths.
- Backward compatible: rows without `visible_to` remain visible to all namespace readers.
- No LLM.

## 3. Constraints

- Namespaces remain the primary partition; ReBAC is a secondary filter within a namespace.
- No blocking of existing rows: absence of `visible_to` means unrestricted (namespace-level) access.
- Caller identity is resolved from `user_id`/`node_id`/`verified_signer_did` (DID) or an explicit principal claim; never from untrusted client input without auth.
- Deterministic filtering; no LLM.
- SynapticMesh: `visible_to` is NOT propagated unless `eligibility: "shareable"` AND `visible_to` explicitly includes `"*"` or a peer's principal; restricted rows are never published.

## 4. Scope

In scope:
- `metadata.visible_to` schema and validation.
- Correct `allowed(user, resource_owner, relation, acl)` and `check(predicate, user, resource)` semantics.
- Principal identity resolution.
- Enforcement in `search_inner`, `export_graph`, SPARQL projection, MCP read tools.
- RDF projection (`ns:visibleTo`) and SHACL shape.
- SynapticMesh publish/import handling.

Out of scope:
- Role hierarchy / group expansion (v2).
- Cross-namespace ACLs.

## 5. Approach

### 5.1 Data model
- `metadata.visible_to: Vec<String>` (optional). Each entry is a principal identifier: a `user_id`, `node_id`, `did:key:...`, or `"*"` (wildcard = all authenticated principals in the namespace).
- If absent or empty → unrestricted (namespace-level read).
- `resource_owner` is derived from `metadata.user_id` if present, else `lifecycle.created_by`, else `verified_signer_did`. If all are absent, `resource_owner = ""`; then `allowed` treats no user as owner (write denied unless admin), and read access follows `acl`/unrestricted rules.

### 5.2 Helper semantics (`src/rebac.rs`)
```rust
pub fn allowed(user: &str, resource_owner: &str, relation: &str, acl: &[String]) -> bool {
    if user.is_empty() { return false; }                       // no anonymous access
    if user == "admin" { return true; }                        // admin bypass (explicit, logged)
    if !resource_owner.is_empty() && user == resource_owner { return true; }  // owner always allowed
    // v1: acl entries are viewer principals; editor/owner require ownership/admin
    match relation {
        "viewer" => acl.is_empty() || acl.iter().any(|e| e == "*" || e == user),
        "editor" | "owner" => false, // only owner/admin (handled above) may write in v1
        _ => false,
    }
}
```
- `check(predicate: &str, user: &str, resource: &str) -> bool`: exact equality only — `predicate == user || predicate == resource || user == "admin"`. No substring matching. (Legacy helper retained for compatibility; prefer `allowed`.)

Role granularity (editor/owner roles beyond owner) is v2; v1 keeps `editor`/`owner` writes restricted to `resource_owner`/`admin`.

### 5.3 Principal identity resolution
- MCP tools: `principal` is resolved from daemon/session auth context (bearer token → `node_id`/`user_id`/`verified_signer_did`). An explicit `principal` parameter is accepted ONLY when the caller is authenticated as `admin`; otherwise the client-supplied value is ignored and the authenticated principal is used.
- Search/export/SPARQL: `principal` passed via `SearchFilters` / query context from the authenticated daemon principal; default namespace owner.
- Admin: `"admin"` principal bypasses all checks (explicit, logged).

### 5.4 Enforcement
- `search_inner`: after row filters, drop rows where `!allowed(principal, owner, "viewer", acl)`.
- `export_graph` / N-Quads export: same filter.
- SPARQL projection (`project_sparql_dataset`): exclude restricted rows unless principal is allowed; for federated projection, exclude restricted rows entirely (no principal context on remote).
- MCP read tools (`get_memory`, `get_snapshot`): return `403`/`denied` if not allowed.
- Write tools: only owner or `editor`/`owner` role principals may update; others denied.

### 5.5 RDF projection
- Emit `ns:visibleTo` predicate (one per ACL entry) for restricted rows; omit for unrestricted rows.
- SHACL: `metadata.visible_to` validated as array of strings; `"*"` allowed.

### 5.6 SynapticMesh handling
- Publish: if `visible_to` is set and does not include `"*"` or a peer principal, the row is NOT published (restricted). If `visible_to` includes `"*"` or peer principals, publish with `visible_to` preserved.
- Import: preserve `visible_to` as-is; restricted rows remain restricted on receiver.

### 5.7 Backward compatibility
- Rows without `visible_to` (or empty): unrestricted, visible to all namespace readers. Existing tests/data unaffected.

## 6. Interfaces

- `src/rebac.rs`: `allowed(user, resource_owner, relation, acl: &[String]) -> bool`, `check(predicate, user, resource) -> bool` (exact match).
- `SearchFilters`: add `principal: Option<String>`.
- MCP tools: add optional `principal` parameter to read tools.
- Config: `rebac_enabled: bool` (default true).

## 7. Edge cases

- Empty `user`: denied (no anonymous).
- `acl` empty: unrestricted.
- `"*"` in acl: all authenticated principals allowed.
- Admin principal: bypass.
- Substring collision: `"alice"` does not match `"malice"` (exact match).
- Restricted row in federated projection: excluded entirely.

## 8. Testing

- Unit: `allowed` owner/stranger/wildcard/admin/empty-user cases.
- Unit: `check` exact match, no substring spoofing.
- Unit: backward compat (no `visible_to` → allowed).
- Integration: restricted row filtered from search/export/SPARQL.
- Integration: write denied for non-owner/non-editor.
- Negative: stranger denied on viewer/editor/owner for restricted row.

## 9. Acceptance

ReBAC helpers are correct (no blanket grants, no substring spoofing); `metadata.visible_to` schema validated; enforcement at search/export/SPARQL/MCP; backward compatible; SynapticMesh preserves restrictions; no LLM.

## 10. Files touched (expected)

- `src/rebac.rs`: fix `allowed`/`check`.
- `src/store/ladybug.rs`: filter in `search_inner`, `export_graph`.
- `src/rdf/projection.rs`: emit `ns:visibleTo`.
- `src/rdf/shacl.rs`: validate `visible_to`.
- `src/server.rs`: principal param on read tools.
- `src/sparql/mod.rs`: filter in projection.
- `src/synaptic_mesh/worker.rs`, `import.rs`: publish/import handling.
- `src/search.rs`: `SearchFilters.principal`.