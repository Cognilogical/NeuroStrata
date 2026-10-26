//! Source/generation-scoped physical IDs and hash capture.
//!
//! Ingestion writes many nodes for one namespace. Two concurrent worktrees of
//! the same repository ingest into the same namespace, and the Memory table is
//! PRIMARY KEY (id) — one column — so "projects are separated by namespace" is
//! only true for a node if its id also separates *generations*. Otherwise the
//! second worktree's staging rows MERGE onto the first's and one worktree's
//! half-built tree silently overwrites the other's.
//!
//! A physical id is therefore three segments: `namespace`, `generation`, and
//! the logical path a rule names:
//!
//! ```text
//! NeuroStrata::7::src/store/ladybug.rs
//! ```
//!
//! The generation is a monotonic counter owned by the store
//! (`IngestGeneration`). Only the *active* generation's nodes are reachable
//! through edge resolution; earlier generations remain as history.
//!
//! This module is deliberately free of `crate::` dependencies so an
//! integration test can compile it from its real path and pin the id scheme
//! without a library target (see `tests/ingest/staged_publication_test.rs`).
//! `src/parser/ingest.rs` re-exports these helpers, and the store resolves
//! edge targets through them.

use sha2::{Digest, Sha256};

/// The separator between the segments of a physical id.
///
/// Two colons because a path cannot contain them on any platform we ingest
/// from, so splitting a qualified id back apart is unambiguous. `ingest.rs`
/// re-exports this as its `NAMESPACE_SEPARATOR` so the store and the server
/// keep referring to the same constant through the path they already use.
pub const NAMESPACE_SEPARATOR: &str = "::";

/// The physical id a node gets during staged ingestion: namespace, generation,
/// and the logical path a rule names. The generation is what keeps two
/// worktrees' nodes apart even when they share a namespace and a path.
pub fn generation_scoped_id(namespace: &str, generation: u64, logical_path: &str) -> String {
    format!(
        "{}{}{}{}{}",
        namespace, NAMESPACE_SEPARATOR, generation, NAMESPACE_SEPARATOR, logical_path
    )
}

/// Splits a generation-scoped id back into its three segments.
///
/// Returns `None` for anything that is not generation-scoped: a bare path
/// (`src/lib.rs`), a legacy namespace-qualified id (`NeuroStrata::src/lib.rs`),
/// or a memory id a UUID produced. The middle segment must parse as a `u64`;
/// a qualified id whose path happens to look numeric (`ns::42`) has no second
/// separator, so it does not pass for a generation.
pub fn split_scoped_id(id: &str) -> Option<(String, u64, String)> {
    let mut parts = id.splitn(3, NAMESPACE_SEPARATOR);
    let namespace = parts.next()?;
    let generation = parts.next()?;
    let logical_path = parts.next()?; // a third segment is what scopes it
    let generation: u64 = generation.parse().ok()?;
    Some((namespace.to_string(), generation, logical_path.to_string()))
}

/// The logical path inside a physical id: namespace and generation stripped.
///
/// This is the string a rule names (`src/lib.rs`), so edge resolution and
/// `KnownIds` index on it. A bare id (a UUID, or a pre-namespace path) is
/// returned unchanged; a generation is only stripped when the id carries three
/// segments and the middle one is a number.
pub fn logical_path_of(id: &str) -> &str {
    if let Some(first) = id.find(NAMESPACE_SEPARATOR) {
        let after_namespace = first + NAMESPACE_SEPARATOR.len();
        let rest = &id[after_namespace..];
        if let Some(second) = rest.find(NAMESPACE_SEPARATOR) {
            let middle = &rest[..second];
            if middle.parse::<u64>().is_ok() {
                return &rest[second + NAMESPACE_SEPARATOR.len()..];
            }
        }
        return rest;
    }
    id
}

/// The lowercase hex SHA-256 of `bytes`, used for `file_sha256` and
/// `span_sha256` capture.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Stable identity for one symbol.
///
/// The last segment is the SHA-256 of the symbol's full (untruncated)
/// definition text, not its start line. A definition that moves down the file
/// by twenty lines is still the same symbol, so re-ingesting unchanged code
/// upserts instead of accumulating a second copy — while a real edit changes
/// the hash and is a new symbol.
pub fn symbol_id(path: &str, kind: &str, name: &str, span_sha256: &str) -> String {
    format!("{}#{}:{}@{}", path, kind, name, span_sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generation_scoped_id_round_trips() {
        let id = generation_scoped_id("NeuroStrata", 7, "src/lib.rs");
        assert_eq!(id, "NeuroStrata::7::src/lib.rs");
        assert_eq!(
            split_scoped_id(&id),
            Some(("NeuroStrata".to_string(), 7, "src/lib.rs".to_string()))
        );
        assert_eq!(logical_path_of(&id), "src/lib.rs");
    }

    #[test]
    fn a_symbol_id_keeps_its_three_segments_when_scoped() {
        let id = generation_scoped_id("ns", 3, &symbol_id("a.rs", "functions", "main", "deadbeef"));
        let (ns, gen, path) = split_scoped_id(&id).expect("scoped");
        assert_eq!(ns, "ns");
        assert_eq!(gen, 3);
        assert_eq!(path, "a.rs#functions:main@deadbeef");
    }

    #[test]
    fn a_legacy_qualified_id_is_not_generation_scoped() {
        assert_eq!(split_scoped_id("ns::src/lib.rs"), None);
        assert_eq!(logical_path_of("ns::src/lib.rs"), "src/lib.rs");
    }

    #[test]
    fn a_bare_path_is_returned_unchanged() {
        assert_eq!(split_scoped_id("src/lib.rs"), None);
        assert_eq!(logical_path_of("src/lib.rs"), "src/lib.rs");
    }

    #[test]
    fn a_numeric_path_is_not_a_generation_without_a_third_segment() {
        assert_eq!(split_scoped_id("ns::42"), None);
        assert_eq!(logical_path_of("ns::42"), "42");
    }

    #[test]
    fn two_worktrees_never_share_a_physical_id() {
        assert_ne!(
            generation_scoped_id("ns", 1, "src/lib.rs"),
            generation_scoped_id("ns", 2, "src/lib.rs"),
            "two generations of one namespace must not confluate on one id"
        );
    }

    #[test]
    fn a_symbol_id_does_not_change_when_the_start_line_moves() {
        let hash = sha256_hex("fn main() {}".as_bytes());
        // Same body hash: stable regardless of the line the definition moved to.
        assert_eq!(
            symbol_id("a.rs", "functions", "main", &hash),
            symbol_id("a.rs", "functions", "main", &hash)
        );
        // A different body hash is a different symbol, even at the same spot.
        assert_ne!(
            symbol_id("a.rs", "functions", "main", &hash),
            symbol_id("a.rs", "functions", "main", "0".repeat(64).as_str())
        );
    }

    #[test]
    fn hashes_are_lowercase_hex() {
        let hash = sha256_hex(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
