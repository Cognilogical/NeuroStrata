//! Graph-read capability: deterministic, zero-embedding lookups beside the
//! vector store (`VectorStore`). See the plan Task 3 / spec §4 for the
//! `memories_for_file` contract, and Task 4 / spec §4.2 for the bounded
//! `related` traversal.
pub mod memories_for_file;
pub mod related;
