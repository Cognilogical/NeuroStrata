//! SynapticMesh shareable-type allowlist (spec §6 R6.4).
//!
//! Crate-free by design: the production outbox uses this predicate to decide
//! what may leave the machine, and the privacy egress test includes this file
//! directly so it exercises the REAL predicate rather than a drifting copy.
//! Receipts are local JSON ledger events with no `memory_type`, so the
//! predicate can never classify one as exportable.

/// The memory types that may be shared over the mesh. Everything else —
/// receipts, `action_receipt`, tasks, structural/ingestion rows, embeddings and
/// bus sentinel rows — is strictly local.
pub const SHAREABLE_MEMORY_TYPES: &[&str] =
    &["rule", "fact", "procedure", "action_type", "thesis", "concept"];

/// Whether `memory_type` is a type the mesh may carry. Single source of truth
/// for both the subscriber (capture) and the worker (export).
pub fn is_shareable_memory_type(memory_type: &str) -> bool {
    SHAREABLE_MEMORY_TYPES.contains(&memory_type)
}
