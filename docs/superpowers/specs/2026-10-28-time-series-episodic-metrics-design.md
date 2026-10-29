# Time-Series / Episodic Metrics Design

## Goal
Optional time-series access metrics over memory usage.

## Architecture
- Module: `src/time_series.rs`: `Series` with push, mean, last.

## Constraints
- Optional; not a core memory primitive.

## Testing
- Mean computation.
- Last value.