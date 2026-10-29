# Multimodal Validation Design

## Goal
Validation for non-text memory (images/audio) if memory scope expands beyond text.

## Architecture
- Module: `src/multimodal.rs`: `embed_text`, `validate_mime`, `describe`.

## Constraints
- Optional; core memory remains text-first.
- MIME allow-list.

## Testing
- MIME validation.
- Describe length.