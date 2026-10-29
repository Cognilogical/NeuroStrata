# ReBAC Design

## Goal
Relationship-based access control predicates enforced at search/export for multi-user/team memory.

## Architecture
- Module: `src/rebac.rs`: `allowed(user, resource_owner, relation)`, `check(predicate, user, resource)`.

## Constraints
- Namespaces remain the primary partition.
- Optional per-row access predicates.

## Testing
- Owner allowed.
- Stranger viewer.