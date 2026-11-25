//! Offline dependency-advisory contract for issues #426–#429.
//!
//! These tests read only committed files: the desktop lockfile, the web-ui
//! lockfile, and the dated C2 decision record. They perform no network I/O and
//! invoke no external tools, so they are deterministic on every platform and
//! can run as the permanent `dependency-advisory` eval suite.
//!
//! Version checks compare numeric semver tuples, never strings, because
//! `"0.18.10" < "0.18.9"` is true lexically but false semantically.

use std::fs;
use std::path::{Path, PathBuf};

/// Resolve a repository-relative path from the crate root, so the tests run
/// from any working directory.
fn manifest_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// Parse the leading numeric `major.minor.patch` components of a version.
///
/// Pre-release/build suffixes are ignored (`2.0.0-alpha.6` -> `(2, 0, 0)`),
/// and missing components default to zero. Every version in this contract is
/// a plain `x.y.z`, but the parser stays total so a future advisory floor
/// cannot panic the suite.
fn version_tuple(raw: &str) -> (u64, u64, u64) {
    let mut parts = [0_u64; 3];
    let cleaned = raw.trim().trim_start_matches(['v', 'V']);
    for (slot, component) in cleaned.split('.').take(3).enumerate() {
        let digits: String = component
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        parts[slot] = digits.parse().unwrap_or(0);
    }
    (parts[0], parts[1], parts[2])
}

fn srctauri_lock() -> toml::Value {
    let path = manifest_path("src-tauri/Cargo.lock");
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    toml::from_str(&raw).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// Every `version` for `name` in `src-tauri/Cargo.lock`.
fn srctauri_versions(name: &str) -> Vec<String> {
    let lock = srctauri_lock();
    let packages = lock
        .get("package")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("src-tauri/Cargo.lock has no [[package]] array"));

    packages
        .iter()
        .filter_map(|package| {
            let package_name = package.get("name")?.as_str()?;
            if package_name != name {
                return None;
            }
            package
                .get("version")
                .and_then(|version| version.as_str())
                .map(str::to_owned)
        })
        .collect()
}

/// `(package path, version)` pairs for a package basename in
/// `web-ui/package-lock.json`'s `packages` map.
///
/// Matches `node_modules/<name>` and any nested `.../node_modules/<name>`.
/// Matching the final path component prevents `zod-validation-error` from
/// being treated as `zod`.
fn webui_versions(name: &str) -> Vec<(String, String)> {
    let path = manifest_path("web-ui/package-lock.json");
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let document: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));

    let packages = document
        .get("packages")
        .and_then(|value| value.as_object())
        .unwrap_or_else(|| panic!("web-ui/package-lock.json has no packages object"));

    let basename = format!("node_modules/{name}");
    let nested_suffix = format!("/{name}");

    packages
        .iter()
        .filter_map(|(package_path, package)| {
            if package_path != &basename && !package_path.ends_with(&nested_suffix) {
                return None;
            }
            let version = package.get("version")?.as_str()?;
            Some((package_path.clone(), version.to_owned()))
        })
        .collect()
}

fn format_webui_entries(entries: &[(String, String)]) -> String {
    entries
        .iter()
        .map(|(path, version)| format!("{path}@{version}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn srctauri_cookie_is_patched() {
    const FLOOR: (u64, u64, u64) = (0, 18, 3);
    let versions = srctauri_versions("cookie");

    assert!(
        !versions.is_empty(),
        "src-tauri/Cargo.lock has no cookie package; contract C1 requires cookie >= 0.18.3"
    );
    for version in &versions {
        assert!(
            version_tuple(version) >= FLOOR,
            "cookie {version} is below the C1 floor 0.18.3 (RUSTSEC-2026-0594)"
        );
    }
}

#[test]
fn srctauri_url_is_patched() {
    const URL_FLOOR: (u64, u64, u64) = (2, 5, 9);
    const FORM_URLENCODED_FLOOR: (u64, u64, u64) = (1, 2, 3);

    let urls = srctauri_versions("url");
    assert!(
        !urls.is_empty(),
        "src-tauri/Cargo.lock has no url package; contract C1 requires url >= 2.5.9"
    );
    for version in &urls {
        assert!(
            version_tuple(version) >= URL_FLOOR,
            "url {version} is below the C1 floor 2.5.9 (RUSTSEC-2026-0592)"
        );
    }

    let form_urlencoded = srctauri_versions("form_urlencoded");
    assert!(
        !form_urlencoded.is_empty(),
        "src-tauri/Cargo.lock has no form_urlencoded package; contract C1 requires >= 1.2.3"
    );
    for version in &form_urlencoded {
        assert!(
            version_tuple(version) >= FORM_URLENCODED_FLOOR,
            "form_urlencoded {version} is below the C1 floor 1.2.3"
        );
    }
}

#[test]
fn srctauri_glib_decision_recorded() {
    const PATCHED_FLOOR: (u64, u64, u64) = (0, 20, 0);
    let versions = srctauri_versions("glib");

    let all_patched = !versions.is_empty()
        && versions
            .iter()
            .all(|version| version_tuple(version) >= PATCHED_FLOOR);
    if all_patched {
        return;
    }

    let spec = manifest_path(
        "docs/superpowers/specs/2026-11-24-issues-426-429-dependency-remediation-spec.md",
    );
    let record = fs::read_to_string(&spec).unwrap_or_else(|error| {
        panic!(
            "glib {} is below 0.20.0 and the C2 decision record {} is unreadable: {error}",
            versions.join(", "),
            spec.display()
        )
    });

    assert!(
        record.contains("RUSTSEC-2024-0429"),
        "C2 decision record {} does not name RUSTSEC-2024-0429",
        spec.display()
    );
    assert!(
        record.contains("gtk >= 0.19"),
        "C2 decision record {} does not carry the upstream review trigger `gtk >= 0.19`",
        spec.display()
    );
}

#[test]
fn webui_has_no_vulnerable_minimatch() {
    const VULNERABLE_FLOOR: (u64, u64, u64) = (7, 0, 0);
    const FIXED_FLOOR: (u64, u64, u64) = (11, 0, 1);

    let entries = webui_versions("minimatch");
    assert!(
        !entries.is_empty(),
        "web-ui/package-lock.json has no minimatch package; contract C3 requires >= 11.0.1"
    );

    let vulnerable: Vec<String> = entries
        .iter()
        .filter(|(_, version)| {
            let parsed = version_tuple(version);
            parsed >= VULNERABLE_FLOOR && parsed < FIXED_FLOOR
        })
        .map(|(path, version)| format!("{path}@{version}"))
        .collect();
    assert!(
        vulnerable.is_empty(),
        "vulnerable minimatch entries (>= 7.0.0, < 11.0.1): {}",
        vulnerable.join(", ")
    );

    assert!(
        entries
            .iter()
            .any(|(_, version)| version_tuple(version) >= FIXED_FLOOR),
        "no minimatch >= 11.0.1 in web-ui/package-lock.json; entries: {}",
        format_webui_entries(&entries)
    );
}

#[test]
fn webui_has_no_vulnerable_zod() {
    const FLOOR: (u64, u64, u64) = (4, 6, 6);

    let entries = webui_versions("zod");
    assert!(
        !entries.is_empty(),
        "web-ui/package-lock.json has no zod package; contract C3 requires zod >= 4.6.6"
    );

    let vulnerable: Vec<String> = entries
        .iter()
        .filter(|(_, version)| version_tuple(version) < FLOOR)
        .map(|(path, version)| format!("{path}@{version}"))
        .collect();
    assert!(
        vulnerable.is_empty(),
        "zod entries below the 4.6.6 floor: {}",
        vulnerable.join(", ")
    );
}
