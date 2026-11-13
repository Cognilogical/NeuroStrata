# `set_metadata` ownership authorization — contract (WP2.2)

Status: Phase 2 / WP2.2. Findings: panel 8 (`set_metadata` rewrites the
ownership metadata ReBAC trusts, with no write authorization); panel 9 owner
parity was closed in WP1.3 (#149) and is re-asserted here.

## Problem

`set_metadata` replaces the whole metadata document on an existing row and
performed no `can_write_memory` check. ReBAC derives the canonical owner and
the ACL from that document, so any caller able to reach the row could grant or
deny reads by rewriting `metadata.user_id` / `visible_to` (including on rows
whose payload owner is empty).

## Contracts

### C1 — write authorization

`set_metadata` takes the authenticated `principal`. The write is refused with
an `access denied by ReBAC policy` error unless
`rebac::can_write_memory(principal, payload.user_id, payload.metadata)` holds.
The existing secret scan still runs first, and the ingested/derived-row guard
is unchanged.

### C2 — ownership and visibility changes require the owner or admin

Replacing the document may change the canonical owner
(`extract_resource_owner`) or the ACL (`extract_visible_to`). When either
changes, only the current owner or `admin` may proceed; an editor may not
grant reads or hand the row to another principal.

### C3 — benign edits stay available

When the canonical owner and ACL are unchanged, any principal with write
access (owner, editor, admin) may update other metadata (provenance, notes,
tags). This keeps the operator's surgical path useful.

### C4 — local surfaces pass the admin default

The CLI and the bearer-authenticated daemon call `set_metadata(..., "admin")`,
matching the local-admin default the other local write paths use. A future
authenticated caller passes its resolved principal instead.

### C5 — lexical parity

Both lexical routes resolve the canonical owner with
`rebac::extract_resource_owner` (WP1.3), so the owner this gate protects is the
owner both routes enforce.

## Tests

| Test | Contract |
|---|---|
| `set_metadata_denies_a_viewer` | C1 |
| `set_metadata_allows_an_editor_to_fix_provenance` | C3 |
| `set_metadata_denies_an_editor_rewriting_visibility` | C2 |
| `set_metadata_allows_the_owner_to_rewrite_visibility` | C2 |
| `set_metadata_allows_admin_to_rewrite_ownership` | C2/C4 |
| `lexical_owner_parity` (WP1.3) | C5 |

## Acceptance

- A viewer cannot write metadata; an editor cannot change the ACL; the owner
  and admin can.
- A refused write leaves the stored document untouched.
- Mutation: removing the write gate admits the viewer; removing the
  ownership gate lets the editor widen visibility.
