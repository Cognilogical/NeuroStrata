# Streaming Delta Design

## Goal
RDF Patch delta encoding and optional NATS/MQTT real-time sync for SynapticMesh.

## Architecture
- Module: `src/streaming.rs`: `Stream` with `push`, `to_patch`; `Delta` to N-Triples.

## Constraints
- Delta encoding reduces push size.
- Real-time transport is optional/remote-configurable.

## Testing
- Patch output.
- Delta serialization.