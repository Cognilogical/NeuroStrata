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
//! generations; a failed staging leaves the previous generation active) is
//! exercised by `src/parser/ingest.rs` and `src/store/ladybug.rs` unit tests,
//! which drive the real LadybugStore. This target pins the id scheme and the
//! hash/identity rules those tests depend on, and asserts the staged
//! publication surface exists at the names the plan specifies.

#![allow(dead_code)]

#[path = "../../src/parser/generation.rs"]
mod generation;

use generation::{
    generation_scoped_id, logical_path_of, sha256_hex, split_scoped_id, symbol_id,
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

/// The staged publication surface must exist at the names the plan specifies,
/// in the files the plan says carry them.
#[test]
fn staged_publication_surface_exists() {
    use std::path::Path;

    for path in [
        "src/parser/generation.rs",
        "src/parser/ingest.rs",
        "src/store/ladybug.rs",
        "src/traits.rs",
    ] {
        assert!(Path::new(path).exists(), "source file must exist: {path}");
    }

    let traits = std::fs::read_to_string("src/traits.rs").expect("traits.rs readable");
    for name in [
        "begin_generation",
        "publish_generation",
        "abort_generation",
        "active_generation",
        "generation_nodes",
    ] {
        assert!(traits.contains(name), "traits.rs must declare `{name}`");
    }

    let ingest = std::fs::read_to_string("src/parser/ingest.rs").expect("ingest.rs readable");
    for key in [
        "source_identity",
        "index_generation",
        "file_sha256",
        "span_sha256",
    ] {
        assert!(
            ingest.contains(key),
            "ingest.rs must write `{key}` metadata"
        );
    }

    let store = std::fs::read_to_string("src/store/ladybug.rs").expect("ladybug.rs readable");
    assert!(
        store.contains("IngestGeneration"),
        "ladybug.rs must persist the generation pointer table"
    );
}
