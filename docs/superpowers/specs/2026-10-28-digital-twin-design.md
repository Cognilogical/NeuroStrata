# Digital Twin Domain Module Design

## Goal
Optional digital-twin support for domain-targeted deployments; not a core memory primitive.

## Architecture
- Module: `src/digital_twin.rs`: `Twin { id, state }` with set/get.

## Constraints
- Only adopt if NeuroStrata targets digital-twin domains.
- Core memory retrieval must not depend on it.

## Testing
- State round-trip.