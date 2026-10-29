# TDB2 / Storage Hygiene Design

## Goal
Port mmap snapshot, term interning, WAL durability improvements into storage path or alternative backend.

## Architecture
- Candidate: adopt TDB2 or port hygiene into LadybugDB path.
- Improves cold start and RSS.

## Constraints
- LadybugDB remains default.
- No destructive migration.

## Testing
- Mmap snapshot smoke test (future).