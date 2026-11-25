//! Offline dependency-advisory contract for issues #426–#429.
//!
//! These tests read only committed files: the desktop lockfile, the web-ui
//! lockfile, and the dated C2 decision record. They perform no network I/O and
//! invoke no external tools, so they are deterministic on every platform and
//! can run as the permanent `dependency-advisory` eval suite.
//!
//! Version checks compare parsed semver tuples, never strings, because
//! `"0.18.10" < "0.18.9"` is true lexically but false semantically. A
//! pre-release also sorts strictly below the release with the same triple, so
//! `11.0.1-rc.1` can never satisfy a `>= 11.0.1` security floor.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

/// Resolve a repository-relative path from the crate root, so the tests run
/// from any working directory.
fn manifest_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// A parsed `major.minor.patch[-prerelease]` version.
///
/// Ordering compares the numeric triple first. When the triples are equal, a
/// pre-release sorts below the release: `11.0.1-rc.1 < 11.0.1`. Build metadata
/// (`+build`) does not make a version a pre-release and is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    is_prerelease: bool,
}

impl Version {
    /// A plain release version, used for the contract floors.
    const fn release(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            is_prerelease: false,
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| {
                // `false < true` would put the release first; reverse it so a
                // pre-release is strictly less than its corresponding release.
                self.is_prerelease.cmp(&other.is_prerelease).reverse()
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Parse `major.minor.patch` plus an optional pre-release suffix.
///
/// Fails closed: a missing or non-numeric component panics instead of silently
/// defaulting to zero, so a malformed lockfile version can never satisfy a
/// floor by accident. A dangling separator such as `11.0.1-` is an empty
/// pre-release, which semver forbids; treating it as a plain release would let
/// it clear a security floor, so it panics too.
fn parse_version(raw: &str) -> Version {
    let cleaned = raw.trim().trim_start_matches(['v', 'V']);
    // Build metadata is everything after `+`; strip it before looking for the
    // pre-release separator, because a build tag may itself contain `-`.
    let without_build = cleaned.split('+').next().unwrap_or(cleaned);
    let (core, prerelease) = match without_build.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (without_build, None),
    };

    let mut numbers = [0_u64; 3];
    let mut components = core.split('.');
    for (slot, number) in numbers.iter_mut().enumerate() {
        let component = components
            .next()
            .unwrap_or_else(|| panic!("version {raw:?} has fewer than three numeric components"));
        *number = component.parse::<u64>().unwrap_or_else(|_| {
            panic!("version {raw:?} has a non-numeric component {component:?}")
        });
    }
    if components.next().is_some() {
        panic!("version {raw:?} has more than three numeric components");
    }

    let is_prerelease = match prerelease {
        Some(pre) => {
            assert!(
                !pre.is_empty(),
                "version {raw:?} has an empty pre-release suffix; semver requires at least one identifier"
            );
            true
        }
        None => false,
    };

    Version {
        major: numbers[0],
        minor: numbers[1],
        patch: numbers[2],
        is_prerelease,
    }
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

/// `(package path, version)` pairs for a package in
/// `web-ui/package-lock.json`'s `packages` map.
///
/// Matches exactly `node_modules/<name>` or any nested
/// `.../node_modules/<name>`. Anchoring on the `node_modules/` segment keeps a
/// scoped package such as `@types/zod` from being treated as `zod`, and the
/// exact final component keeps `zod-validation-error` from matching too.
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

    let top_level = format!("node_modules/{name}");
    let nested = format!("/node_modules/{name}");

    packages
        .iter()
        .filter_map(|(package_path, package)| {
            if package_path != &top_level && !package_path.ends_with(&nested) {
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

/// Slice a top-level `## ` section out of a Markdown document.
///
/// The section starts at `heading` and runs to the next `## ` heading. Asserting
/// against the returned slice keeps a marker that appears elsewhere in the
/// document from satisfying a section-scoped contract.
fn markdown_section<'a>(document: &'a str, heading: &str) -> Option<&'a str> {
    let start = document.find(heading)?;
    let body = &document[start..];
    let end = body[heading.len()..]
        .find("\n## ")
        .map_or(body.len(), |offset| heading.len() + offset);
    Some(&body[..end])
}

#[test]
fn srctauri_cookie_is_patched() {
    const FLOOR: Version = Version::release(0, 18, 3);
    let versions = srctauri_versions("cookie");

    assert!(
        !versions.is_empty(),
        "src-tauri/Cargo.lock has no cookie package; contract C1 requires cookie >= 0.18.3"
    );
    for version in &versions {
        assert!(
            parse_version(version) >= FLOOR,
            "cookie {version} is below the C1 floor 0.18.3 (RUSTSEC-2026-0594)"
        );
    }
}

#[test]
fn srctauri_url_is_patched() {
    const URL_FLOOR: Version = Version::release(2, 5, 9);
    const FORM_URLENCODED_FLOOR: Version = Version::release(1, 2, 3);

    let urls = srctauri_versions("url");
    assert!(
        !urls.is_empty(),
        "src-tauri/Cargo.lock has no url package; contract C1 requires url >= 2.5.9"
    );
    for version in &urls {
        assert!(
            parse_version(version) >= URL_FLOOR,
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
            parse_version(version) >= FORM_URLENCODED_FLOOR,
            "form_urlencoded {version} is below the C1 floor 1.2.3"
        );
    }
}

#[test]
fn srctauri_glib_decision_recorded() {
    const PATCHED_FLOOR: Version = Version::release(0, 20, 0);
    let versions = srctauri_versions("glib");

    let all_patched = !versions.is_empty()
        && versions
            .iter()
            .all(|version| parse_version(version) >= PATCHED_FLOOR);
    if all_patched {
        return;
    }

    const C2_HEADING: &str = "## 4. Contract C2";
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

    let section = markdown_section(&record, C2_HEADING).unwrap_or_else(|| {
        panic!(
            "glib {} is below 0.20.0 and the C2 decision record {} has no {C2_HEADING:?} section",
            versions.join(", "),
            spec.display()
        )
    });

    // Scope every marker to the C2 section itself; a stray mention elsewhere in
    // the spec must not satisfy a decision-record contract.
    for marker in ["RUSTSEC-2024-0429", "0.18.5", "2026-11-24", "gtk >= 0.19"] {
        assert!(
            section.contains(marker),
            "C2 decision record {} section {C2_HEADING:?} does not contain {marker:?}",
            spec.display()
        );
    }
}

#[test]
fn webui_has_no_vulnerable_minimatch() {
    // C3.2.1 / Spec §5.2.1: the affected band is [7.0.0, 11.0.1). No entry may
    // sit inside it, and at least one must reach 11.0.1; a pre-release of
    // 11.0.1 sorts below the release, so it does not clear the floor either.
    // Entries below 7.0.0 are outside the advisory range by definition.
    const MIN_AFFECTED: Version = Version::release(7, 0, 0);
    const FIXED_FLOOR: Version = Version::release(11, 0, 1);

    let entries = webui_versions("minimatch");
    assert!(
        !entries.is_empty(),
        "web-ui/package-lock.json has no minimatch package; contract C3 requires >= 11.0.1"
    );

    let vulnerable: Vec<String> = entries
        .iter()
        .filter(|(_, version)| {
            let parsed = parse_version(version);
            parsed >= MIN_AFFECTED && parsed < FIXED_FLOOR
        })
        .map(|(path, version)| format!("{path}@{version}"))
        .collect();
    assert!(
        vulnerable.is_empty(),
        "vulnerable minimatch entries in [7.0.0, 11.0.1): {}",
        vulnerable.join(", ")
    );

    assert!(
        entries
            .iter()
            .any(|(_, version)| parse_version(version) >= FIXED_FLOOR),
        "no minimatch >= 11.0.1 in web-ui/package-lock.json; entries: {}",
        format_webui_entries(&entries)
    );
}

#[test]
fn webui_has_no_vulnerable_zod() {
    const FLOOR: Version = Version::release(4, 6, 6);

    let entries = webui_versions("zod");
    assert!(
        !entries.is_empty(),
        "web-ui/package-lock.json has no zod package; contract C3 requires zod >= 4.6.6"
    );

    let vulnerable: Vec<String> = entries
        .iter()
        .filter(|(_, version)| parse_version(version) < FLOOR)
        .map(|(path, version)| format!("{path}@{version}"))
        .collect();
    assert!(
        vulnerable.is_empty(),
        "zod entries below the 4.6.6 floor: {}",
        vulnerable.join(", ")
    );
}
