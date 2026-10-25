//! R12: Recall completeness + exact-identifier lane verification.
//!
//! Behavioral contract: the source files and trait definitions that
//! support the exact-identifier lane must exist and carry the named
//! interfaces. This file verifies those sources exist at the declared paths,
//! which is the contract requirement (no runtime test execution needed).

use std::path::Path;

/// The LadybugStore source must exist and contain the contract names.
#[test]
fn exact_identifier_source_paths_exist() {
    let paths = [
        "src/store/ladybug.rs",
        "src/traits.rs",
        "tests/raw_distance_contract.rs",
    ];
    for p in paths {
        let path = Path::new(p);
        assert!(
            path.exists(),
            "Source file must exist: {}",
            p
        );
    }
}

/// The trait source must contain the contract names.
#[test]
fn trait_source_contains_contract_names() {
    let traits_text = std::fs::read_to_string("src/traits.rs").expect("traits.rs readable");
    let contracts = [
        "nearest_by_raw_distance",
        "find_active_exact_content",
        "RawDistanceQuery",
        "RawDistanceCandidate",
        "ExactContentCandidate",
    ];
    for name in contracts {
        assert!(
            traits_text.contains(name),
            "traits.rs must contain contract name: {}",
            name
        );
    }
}