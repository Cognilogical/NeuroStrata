# WASM Browser/Edge Engine Design

## Goal
Compile read-only subset to WASM for browser-based NeuroVault and edge deployments.

## Architecture
- Read-only subset of memory engine.
- Storage adapters for browser.

## Constraints
- Blocked partly by LadybugDB ABI; easier after RDF/TDB2 migration.
- Not justified until demand.

## Testing
- WASM smoke build (future).