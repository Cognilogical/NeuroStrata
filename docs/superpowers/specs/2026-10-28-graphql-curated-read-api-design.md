# GraphQL Curated Read API Design

## Goal
Curated, versioned read-only GraphQL API over memory graph with pagination; no auto-generated unstable schema.

## Architecture
- Module: `src/graphql/mod.rs`: `execute(query)`, `schema_from_vocabulary(vocab)`.

## Constraints
- Read-only.
- Versioned schema; pagination required.

## Testing
- Schema builds from vocabulary.
- Execute returns JSON.