# RAG Chat Optional Client Design

## Goal
Optional conversational layer over memory graph; core retrieval stays deterministic and LLM-free.

## Architecture
- Module: `src/rag_chat.rs`: `build_prompt`, `answer_offline`.

## Constraints
- LLM is opt-in remote client; core must not depend on it.

## Testing
- Prompt contains context.
- Offline answer.