//! Task 5 contract test — source/generation-scoped staged ingestion.
//!
//! The crate ships a binary target only (`src/main.rs`), so an integration
//! test cannot import the module tree. The id/hash helpers therefore live in
//! `src/parser/generation.rs`, a module deliberately free of `crate::`
//! dependencies, and are compiled here from their real path — the contract
//! under test is the file the binary ships, not a transcription that can drift
//! from it.
//!
//! The store-level behaviour (two worktrees into one namespace → distinct
//! generations; a mid-stage failure aborts and keeps the previous generation
//! active; superseded generations are filtered from reads) is exercised by
//! `src/parser/ingest.rs` and `src/store/ladybug.rs` unit tests, which drive
//! the real LadybugStore. This target pins the id/generation contract those
//! tests depend on, by exercising the shipped helper functions directly rather
//! than grepping source text for names that might drift from the behaviour.

#![allow(dead_code)]

#[path = "../../src/parser/generation.rs"]
mod generation;

use generation::{
    generation_key, generation_scoped_id, logical_path_of, sha256_hex, split_scoped_id, symbol_id,
    NAMESPACE_SEPARATOR,
};

/// The physical id is three segments: namespace, generation, logical path.
#[test]
fn a_physical_id_scopes_a_path_by_namespace_and_generation() {
    assert_eq!(
        generation_scoped_id("NeuroStrata", 7, "src/store/ladybug.rs"),
        "NeuroStrata::7::src/store/ladybug.rs"
    );
    assert_eq!(NAMESPACE_SEPARATOR, "::");
}

/// The core contract: two worktrees of one repository ingest into the same
/// namespace and the same path, and their rows never confluate onto one id.
#[test]
fn two_worktrees_never_share_a_physical_id() {
    assert_ne!(
        generation_scoped_id("shared-ns", 1, "src/lib.rs"),
        generation_scoped_id("shared-ns", 2, "src/lib.rs"),
        "generation is part of the primary key, so a second worktree must not overwrite the first"
    );
}

/// A legacy namespace-qualified id (`ns::path`) is not generation-scoped, and
/// a bare path is returned unchanged. The store uses this to keep stale and
/// legacy rows out of active-generation resolution.
#[test]
fn only_three_segment_ids_are_generation_scoped() {
    assert_eq!(split_scoped_id("ns::src/lib.rs"), None);
    assert_eq!(logical_path_of("ns::src/lib.rs"), "src/lib.rs");
    assert_eq!(split_scoped_id("src/lib.rs"), None);
    assert_eq!(logical_path_of("src/lib.rs"), "src/lib.rs");
    // A numeric middle segment without a third is not a generation.
    assert_eq!(split_scoped_id("ns::42"), None);
}

/// The logical path is what a rule names, so resolution indexes on it.
#[test]
fn logical_path_strips_the_scope() {
    let id = generation_scoped_id("ns", 3, "src/lib.rs");
    assert_eq!(
        split_scoped_id(&id),
        Some(("ns".to_string(), 3, "src/lib.rs".to_string()))
    );
    assert_eq!(logical_path_of(&id), "src/lib.rs");
}

/// `file_sha256` and `span_sha256` are lowercase hex SHA-256.
#[test]
fn hashes_are_lowercase_hex_sha256() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert!(sha256_hex(b"fn main() {}")
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

/// Symbol identity is the body hash, not the start line: moving a definition
/// down the file is still the same symbol, while an edit is a new one.
#[test]
fn symbol_identity_is_the_body_hash_not_the_start_line() {
    let body = sha256_hex("fn main() {}".as_bytes());
    let moved = symbol_id("a.rs", "functions", "main", &body);
    let same_body = symbol_id("a.rs", "functions", "main", &body);
    assert_eq!(moved, same_body);

    let edited = sha256_hex("fn main() { loop {} }".as_bytes());
    assert_ne!(moved, symbol_id("a.rs", "functions", "main", &edited));
}

/// A generation pointer is keyed by source (`generation_key`), not by namespace
/// alone. This is the contract that keeps two worktrees of one repository from
/// overwriting each other's active generation: the two source keys produce
/// distinct pointer ids, so publishing one source never flips the other's
/// active pointer.
#[test]
fn source_scoped_generation_keys_keep_worktrees_apart() {
    let a = generation_key("shared-ns", "/worktrees/a");
    let b = generation_key("shared-ns", "/worktrees/b");
    assert_ne!(
        a, b,
        "two sources must own distinct active-generation pointers"
    );
    assert_eq!(a, "shared-ns::/worktrees/a");
    assert_eq!(b, "shared-ns::/worktrees/b");
}

/// A superseded generation's rows are told apart from the live one by the
/// middle segment. The store's read filters drop any generation-scoped id whose
/// `(namespace, generation)` is not in the active set, and that filter is
/// exactly `split_scoped_id` — so this pins the behaviour the filter relies on.
#[test]
fn superseded_generation_rows_are_distinguishable() {
    let stale = generation_scoped_id("ns", 1, "src/lib.rs");
    let live = generation_scoped_id("ns", 2, "src/lib.rs");
    assert_ne!(stale, live, "a superseded row must not share the live id");
    assert_eq!(
        split_scoped_id(&stale),
        Some(("ns".to_string(), 1, "src/lib.rs".to_string()))
    );
    assert_eq!(
        split_scoped_id(&live),
        Some(("ns".to_string(), 2, "src/lib.rs".to_string()))
    );
    // Both resolve to the same logical path, so a rule naming the path still
    // re-attaches — but only the active generation's row is reachable.
    assert_eq!(logical_path_of(&stale), "src/lib.rs");
    assert_eq!(logical_path_of(&live), "src/lib.rs");
}
