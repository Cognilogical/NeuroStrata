# GLiNER Review-Only Candidate Emitter Design

## Goal

Provide a deterministic, review-only entity candidate emitter that mirrors GLiNER-style
extraction without affecting search, dedup, guard, or query routing.

## Architecture

- Module: `src/gliner.rs`
- Public type: `GlinerCandidate { text, label, score, review_only }`
- Public function: `extract_candidates(text: &str) -> Vec<GlinerCandidate>`
- Candidates are marked `review_only = true` and never auto-promoted.

## Constraints

- No ONNX inference in default build.
- Full ONNX sidecar remains feature-gated (`gliner` feature) for future work.
- Deterministic extraction: capitalized multi-word phrases only.
- Never invoked by search/dedup/guard/routing paths.

## Testing

- `extracts_capitalized` verifies candidates emitted for capitalized tokens.
- `empty_for_lowercase` verifies no candidates for all-lowercase input.
- All candidates have `review_only == true`.

## Future Work

- Wire ONNX session runner with SHA-256 bundle verifier when model bundles available.
- Emit review-only `IntakeCandidate` records into enrichment pipeline.
"""