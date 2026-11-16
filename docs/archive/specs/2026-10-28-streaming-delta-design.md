# Streaming Delta (RDF Patch) Design

> Status: **APPROVED** (2026-10-30).
> Task: `neurostrata-7rgp` (Streaming delta (RDF Patch) for SynapticMesh, OxiRS §1.10).
> Companion: `docs/superpowers/specs/2026-10-27-rdf-canonical-memory-data-model-migration-design.md`, `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`.

## 1. Problem

`src/streaming/mod.rs` is an in-house stub: `Stream` wraps `Vec<String>` and emits `A <raw string>` lines; `Delta` carries an `op` that `to_ntriples` drops. There is no parser, no SynapticMesh integration, no privacy/conflict handling. The Goal's NATS/MQTT transport is not implemented and has no deps; it is formally retired from this spec's scope.

## 2. Goal

Add **deterministic, in-house RDF Patch delta encoding** for SynapticMesh:

- Serialize memory mutations as W3C-style RDF Patch lines (`A`/`D` over N-Triples terms).
- Parse incoming patches back into structured `Delta` operations.
- Integrate with `synaptic_mesh::outbox` / `import` for push/pull delta sync.
- Enforce SynapticMesh privacy, counter-stripping, and conflict-preservation rules.
- No external transport (NATS/MQTT) in this spec; deltas are exchanged via the existing SynapticMesh git/outbox mechanism and MCP. Real-time transport is a separate, future spec.

## 3. Constraints

- LadybugDB remains the sole store.
- No LLM.
- No new transport dependencies (no `async-nats`, no `rumqttc`).
- Fail-soft: unreachable peers queue locally; never block source push.
- Privacy: only `shareable` rows with `eligibility` cross; tasks, structural, embeddings, private rows never travel.
- Procedure runtime counters (`remaining_fires`, `performance_count`, `last_performed_at`, `last_episodic_pointer`) are stripped from shared procedure definitions (neurostrata-7rgp, ns-7rgp).
- Conflicts are preserved, never silently resolved; unresolved conflicts block synthesis.

## 4. Scope

In scope:
- RDF Patch serialization (`A`/`D`) with N-Triples term escaping.
- Parser for incoming patches (round-trip).
- `Stream` over `Vec<Delta>`; `Delta` carries `op`, `subject`, `predicate`, `object`, and optional graph IRI.
- SynapticMesh outbox integration: emit patch on publish; apply patch on import.
- Privacy/counter-stripping/conflict rules.

Out of scope (formally retired):
- NATS/MQTT real-time transport (no deps; future spec if needed).

## 5. Approach

### 5.1 Delta & Stream types
`src/streaming/mod.rs`:
```rust
pub enum PatchOp { Add, Delete }
pub struct Delta { pub op: PatchOp, pub subject: String, pub predicate: String, pub object: String, pub graph: Option<String> }
pub struct Stream { pub events: Vec<Delta> }
```
- `Stream::push(delta)`, `len()`, `to_patch() -> String` emits one line per delta: `A <s> <p> <o> .` or `D <s> <p> <o> .`, with N-Triples escaping for IRIs (`<...>`), literals (`"..."^^<datatype>` or `"..."@lang`), and blank nodes (`_:...`).
- `Stream::parse_patch(input: &str) -> Result<Vec<Delta>, ParseError>` round-trips.

### 5.2 N-Triples escaping
- IRI: `<` + standard N-Triples validated IRI (reject `< > " { } | ^ \`` and `U+0000..=U+0020`; `\uXXXX`-escape) + `>`.
- Literal: `"..."` with `\"`, `\\`, `\n`, `\r`, `\t`, `\uXXXX` escaping; optional `^^<datatype>` or `@lang`.
- Blank node: `_:label`.
- Graph (named graph): optional; `A  <p> <o> <g> .` / `D  <p> <o> <g> .` W3C RDF Patch quad syntax; default graph (no named graph) if absent. Instead of `G <graphiri> { ... }` (TriG block).

### 5.3 SynapticMesh integration
- On publish (`synaptic_mesh::outbox::publish`): build a `Stream` from the record's canonical RDF quads (added lines `A`), emit patch to outbox.
- On import (`synaptic_mesh::import::apply`): parse patch, apply `A`/`D` lines to a transient dataset, validate against privacy rules, then apply to local store.
- Privacy enforcement: before serialization, filter quads whose subject is not `shareable`-eligible; strip procedure runtime counters; drop tasks/structural/embeddings/private rows. Reject (error) if a patch carries any stripped counter or private row.
- Conflict preservation: if import detects an id collision with differing content, preserve both as mesh-conflicting rows (per §3b); never silently overwrite.

### 5.4 Fail-soft
- Unreachable peer: queue patch locally in outbox; retry on next sync; never block source push; report one flattened status line.

## 6. Interfaces

- `src/streaming/mod.rs`: `PatchOp`, `Delta`, `Stream`, `ParseError`.
- `Stream::push`, `len`, `to_patch`, `parse_patch`.
- Integration hooks: `synaptic_mesh::outbox::emit_patch`, `synaptic_mesh::import::apply_patch`.

## 7. Edge cases

- Empty patch: valid, no-op.
- Malformed line: parse error with line number.
- Unknown op: parse error.
- Stripped counter present: reject patch (security).
- Private row present: reject patch.
- Conflict: preserve both rows.

## 8. Testing

- Unit: round-trip `to_patch` → `parse_patch` yields identical deltas.
- Unit: N-Triples escaping for IRIs/literals/blank nodes.
- Unit: stripped counter rejection.
- Integration: publish → import round-trip preserves content and conflicts.
- Fail-soft: offline peer queues locally.

## 9. Acceptance

RDF Patch delta encoding is available in-house: serialize/parse round-trips; SynapticMesh publish/import uses patches; privacy/counter-stripping/conflict rules enforced; no external transport deps; fail-soft on unreachable peers.

## 10. Files touched (expected)

- `src/streaming/mod.rs`: full implementation.
- `src/synaptic_mesh/outbox.rs`: emit patch.
- `src/synaptic_mesh/import.rs`: apply patch.
- `Cargo.toml`: no new deps.