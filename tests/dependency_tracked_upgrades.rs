//! Offline tracked-upgrade drift gate for issue #339.
//!
//! The four items recorded in
//! `docs/superpowers/specs/2026-11-25-tracked-upgrade-dispositions.md` are
//! upgrades the repository deliberately has **not** taken yet, or took in a
//! different change. Without a gate, each one silently becomes stale: the
//! `spareval` dependency edge drifts onto `oxiri 0.3`, `Dockerfile.base` picks
//! up a CI consumer, a plugin manifest floats onto TypeScript 7, or a workflow
//! action reference loses its SHA pin.
//!
//! These tests read only committed files. They perform no network I/O and
//! invoke no external tools, so they are deterministic on every platform and
//! can run as the permanent `tracked-upgrades` eval suite.

use std::fs;
use std::path::{Path, PathBuf};

/// Resolve a repository-relative path from the crate root, so the tests run
/// from any working directory.
fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_relative(relative: &str) -> String {
    let path = repo_path(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn read_path(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// A parsed `major.minor.patch` version. Pre-release and build suffixes are
/// stripped before the numeric triple is compared, because the contracts here
/// are about the release line, not a specific prerelease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    fn parse(raw: &str) -> Self {
        let cleaned = raw.trim().trim_start_matches(['v', 'V']);
        let core = cleaned
            .split('+')
            .next()
            .unwrap_or(cleaned)
            .split('-')
            .next()
            .unwrap_or(cleaned);

        let mut components = core.split('.');
        let mut next = |label: &str, index: usize| -> u64 {
            let component = components
                .next()
                .unwrap_or_else(|| panic!("version {raw:?} has no {label} component"));
            component.parse::<u64>().unwrap_or_else(|_| {
                panic!(
                    "version {raw:?} has a non-numeric {label} component {component:?} (component {index})"
                )
            })
        };
        let major = next("major", 0);
        let minor = next("minor", 1);
        let patch = next("patch", 2);
        assert!(
            components.next().is_none(),
            "version {raw:?} has more than three numeric components"
        );
        Self {
            major,
            minor,
            patch,
        }
    }
}

/// The compatibility band a Cargo requirement selects. For `0.x` crates the
/// minor version is the breaking boundary, so `0.2` and `0.3` are different
/// bands even though both share major version 0.
fn requirement_band(requirement: &str) -> (u64, u64) {
    let trimmed = requirement.trim();
    let core = trimmed.trim_start_matches(|character: char| {
        matches!(character, '^' | '~' | '=' | '>' | '<' | ' ')
    });
    assert!(
        !core.is_empty() && !core.starts_with('*'),
        "oxiri requirement {requirement:?} does not name a concrete version band"
    );
    let core = core
        .split('+')
        .next()
        .unwrap_or(core)
        .split('-')
        .next()
        .unwrap_or(core);
    let mut components = core.split('.');
    let major = components
        .next()
        .unwrap_or_else(|| panic!("oxiri requirement {requirement:?} has no major component"))
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("oxiri requirement {requirement:?} has a non-numeric major"));
    let minor = components
        .next()
        .unwrap_or("0")
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("oxiri requirement {requirement:?} has a non-numeric minor"));
    (major, minor)
}

fn cargo_toml() -> toml::Value {
    let raw = read_relative("Cargo.toml");
    toml::from_str(&raw).unwrap_or_else(|error| panic!("parse Cargo.toml: {error}"))
}

fn cargo_lock() -> toml::Value {
    let raw = read_relative("Cargo.lock");
    toml::from_str(&raw).unwrap_or_else(|error| panic!("parse Cargo.lock: {error}"))
}

fn lock_packages(lock: &toml::Value) -> &Vec<toml::Value> {
    lock.get("package")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("Cargo.lock has no [[package]] array"))
}

fn package_name(package: &toml::Value) -> &str {
    package
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("Cargo.lock package without a name: {package:?}"))
}

fn package_version(package: &toml::Value) -> &str {
    package
        .get("version")
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| {
            panic!(
                "Cargo.lock package {} without a version",
                package_name(package)
            )
        })
}

/// The root `oxiri` requirement string, whether written as `oxiri = "0.2"` or
/// as an inline table with a `version` key.
fn root_oxiri_requirement() -> String {
    let manifest = cargo_toml();
    let dependency = manifest
        .get("dependencies")
        .and_then(|dependencies| dependencies.get("oxiri"))
        .unwrap_or_else(|| panic!("Cargo.toml [dependencies] has no oxiri entry"));

    match dependency {
        toml::Value::String(requirement) => requirement.clone(),
        toml::Value::Table(table) => table
            .get("version")
            .and_then(|version| version.as_str())
            .unwrap_or_else(|| panic!("Cargo.toml oxiri table has no string version: {table:?}"))
            .to_owned(),
        other => panic!("Cargo.toml oxiri entry has an unexpected shape: {other:?}"),
    }
}

/// Every `.yml`/`.yaml` workflow, sorted for deterministic diagnostics.
fn workflow_paths() -> Vec<PathBuf> {
    let directory = repo_path(".github/workflows");
    let mut paths: Vec<PathBuf> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read entry in {}: {error}", directory.display()))
                .path()
        })
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("yml" | "yaml")
            )
        })
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no workflow files found in {}",
        directory.display()
    );
    paths
}

/// Recursively collect every regular file below `root`.
fn collect_files(root: &Path, files: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(root).unwrap_or_else(|error| panic!("read {}: {error}", root.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("read entry in {}: {error}", root.display()))
            .path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn relative(path: &Path) -> String {
    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .display()
        .to_string()
}

fn is_full_sha(reference: &str) -> bool {
    reference.len() == 40
        && reference
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

#[test]
fn tracked_upgrades_decision_record_names_all_four() {
    const RECORD: &str = "docs/superpowers/specs/2026-11-25-tracked-upgrade-dispositions.md";
    let path = repo_path(RECORD);
    assert!(
        path.is_file(),
        "decision record {} is missing; #339 requires one dated record for all four tracked upgrades",
        path.display()
    );
    let document = read_relative(RECORD);

    for marker in ["oxiri", "Dockerfile.base", "TypeScript", "artifact actions"] {
        assert!(
            document.contains(marker),
            "decision record {} does not name {marker:?}; all four tracked upgrades must stay documented",
            path.display()
        );
    }
}

#[test]
fn oxiri_pin_matches_the_spareval_dependency_edge() {
    const SPAREVAL: &str = "spareval";
    const OXIRI: &str = "oxiri";

    let lock = cargo_lock();
    let packages = lock_packages(&lock);

    let requirement = root_oxiri_requirement();
    let pin_band = requirement_band(&requirement);

    // Duplicate oxiri packages are a split only within the pinned band.
    // A different band legitimately coexists through the separate rudof_*
    // graph (decision record section 2), so duplicates are counted per band.
    let pinned_band_packages: Vec<&str> = packages
        .iter()
        .filter(|package| package_name(package) == OXIRI)
        .map(package_version)
        .filter(|version| {
            let parsed = Version::parse(version);
            (parsed.major, parsed.minor) == pin_band
        })
        .collect();
    assert_eq!(
        pinned_band_packages.len(),
        1,
        "Cargo.lock has {} {OXIRI} packages in the pinned band {}.{}: {pinned_band_packages:?}; \
         exactly one is allowed — a second one is the duplicate-version split #339 blocks",
        pinned_band_packages.len(),
        pin_band.0,
        pin_band.1
    );

    // Inspect every spareval entry and every oxiri edge it declares, not
    // just the first of each.
    let sparevals: Vec<&toml::Value> = packages
        .iter()
        .filter(|package| package_name(package) == SPAREVAL)
        .collect();
    assert!(
        !sparevals.is_empty(),
        "Cargo.lock has no {SPAREVAL} package"
    );

    for spareval in sparevals {
        let spareval_version = package_version(spareval).to_owned();
        let dependencies = spareval
            .get("dependencies")
            .and_then(|value| value.as_array())
            .unwrap_or_else(|| panic!("{SPAREVAL} {spareval_version} has no dependencies array"));
        let edges: Vec<&str> = dependencies
            .iter()
            .filter_map(|dependency| dependency.as_str())
            .filter(|dependency| *dependency == OXIRI || dependency.starts_with("oxiri "))
            .collect();
        assert!(
            !edges.is_empty(),
            "{SPAREVAL} {spareval_version} has no {OXIRI} dependency edge"
        );

        for edge in edges {
            let edge_version = if edge == OXIRI {
                let mut versions: Vec<&str> = packages
                    .iter()
                    .filter(|package| package_name(package) == OXIRI)
                    .map(package_version)
                    .collect();
                versions.sort_unstable();
                assert_eq!(
                    versions.len(),
                    1,
                    "Cargo.lock has an unversioned {OXIRI} edge from {SPAREVAL} but {} {OXIRI} packages: {versions:?}",
                    versions.len()
                );
                versions[0].to_owned()
            } else {
                edge.trim_start_matches(OXIRI).trim().to_owned()
            };
            let edge_parsed = Version::parse(&edge_version);

            assert!(
                packages.iter().any(|package| {
                    package_name(package) == OXIRI && package_version(package) == edge_version
                }),
                "{SPAREVAL} {spareval_version} names {OXIRI} {edge_version}, but Cargo.lock has no such {OXIRI} package"
            );

            let edge_band = (edge_parsed.major, edge_parsed.minor);
            assert_eq!(
                edge_band, pin_band,
                "{SPAREVAL} {spareval_version} depends on {OXIRI} {edge_version} (band {}.{}), but Cargo.toml pins {OXIRI} = {requirement:?} (band {}.{}). \
                 The spareval edge moved out of the pinned band; update the root pin and this gate together before upgrading (#339).",
                edge_band.0, edge_band.1, pin_band.0, pin_band.1
            );
        }
    }
}

#[test]
fn dockerfile_base_stays_on_ubuntu_24_04_lts() {
    let dockerfile = read_relative("Dockerfile.base");
    // Only active (non-comment) directives count: stale strings left in a
    // comment or an unused stage must not satisfy the gate.
    let active: Vec<&str> = dockerfile
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();

    let from_image = active
        .iter()
        .find(|line| line.starts_with("FROM "))
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_else(|| panic!("Dockerfile.base has no active FROM directive: {active:?}"));
    assert_eq!(
        from_image, "ubuntu:24.04",
        "Dockerfile.base must stay on Ubuntu 24.04 LTS until the 26.04 LTS migration in #339 is unblocked; active FROM is {from_image:?}"
    );

    let installs_libicu74 = active.iter().any(|line| {
        line.starts_with("RUN ")
            && (line.contains("apt-get install") || line.contains("apt install"))
            && line
                .split(|character: char| character.is_whitespace() || character == '&')
                .any(|token| token == "libicu74")
    });
    assert!(
        installs_libicu74,
        "Dockerfile.base must install libicu74 in its active RUN/apt directive, the ICU package shipped by Ubuntu 24.04; a different package means the base image moved"
    );
}

#[test]
fn dockerfile_base_has_no_ci_consumer() {
    let mut files = workflow_paths();
    collect_files(&repo_path("scripts"), &mut files);

    let offenders: Vec<String> = files
        .iter()
        .filter(|path| {
            let bytes =
                fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            String::from_utf8_lossy(&bytes).contains("Dockerfile.base")
        })
        .map(|path| relative(path))
        .collect();

    assert!(
        offenders.is_empty(),
        "Dockerfile.base is deferred (#339) but these CI/script files reference it: {offenders:?}; \
         either remove the reference or complete the 26.04 LTS migration first"
    );
}

#[test]
fn typescript_pins_are_6_0_3_across_plugins_and_web_ui() {
    const MANIFESTS: [&str; 8] = [
        "plugins/core/package.json",
        "plugins/claude/package.json",
        "plugins/codex/package.json",
        "plugins/hermes/package.json",
        "plugins/opencode/package.json",
        "plugins/github-cli/package.json",
        "plugins/openclaw/package.json",
        "web-ui/package.json",
    ];
    const PIN: &str = "~6.0.3";

    for manifest in MANIFESTS {
        let path = repo_path(manifest);
        let raw = read_relative(manifest);
        let document: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        let typescript = document
            .get("devDependencies")
            .and_then(|dependencies| dependencies.get("typescript"))
            .or_else(|| {
                document
                    .get("dependencies")
                    .and_then(|dependencies| dependencies.get("typescript"))
            })
            .and_then(|version| version.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "{} has no typescript dependency; #339 pins all eight manifests to {PIN}",
                    path.display()
                )
            });

        assert_eq!(
            typescript.trim(),
            PIN,
            "{} pins TypeScript {typescript:?}; #339 requires {PIN} across all eight manifests",
            path.display()
        );
        let parsed = Version::parse(typescript.trim().trim_start_matches('~'));
        assert_ne!(
            parsed.major, 7,
            "{} selects TypeScript 7.x ({typescript:?}); the 7.1 migration is a separate tracked upgrade (#339)",
            path.display()
        );
    }
}

#[test]
fn artifact_actions_are_upload_v7_download_v8_and_checkout_sha_pinned() {
    let mut seen = [0_usize; 3];
    // Exact SHA <-> version pairs recorded in the decision record
    // (tracked-upgrade dispositions, section 5). Any other SHA, even on the
    // same major line, is an unrecorded revision and must fail the gate.
    let actions = [
        (
            "actions/upload-artifact",
            7_usize,
            "cf430e030ddbb5b0abf93d22962f4752f3646cd9",
            "v7.0.2",
        ),
        (
            "actions/download-artifact",
            8_usize,
            "9000827ccba6bdab643e8b6fd33ac0654aef8333",
            "v8.0.2",
        ),
        (
            "actions/checkout",
            7_usize,
            "3d3c42e5aac5ba805825da76410c181273ba90b1",
            "v7.0.1",
        ),
    ];

    for path in workflow_paths() {
        let text = read_path(&path);
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            // A step is a YAML sequence entry: `- uses: ...`. Strip the
            // sequence marker first, or every unnamed step is skipped.
            let trimmed = trimmed.strip_prefix("- ").unwrap_or(trimmed);
            // The key may be quoted (`"uses": ...` / `'uses': ...`).
            let trimmed_owned = trimmed
                .replace("\"uses\"", "uses")
                .replace("'uses'", "uses");
            let trimmed = trimmed_owned.as_str();
            let Some(rest) = trimmed
                .strip_prefix("uses:")
                .or_else(|| trimmed.strip_prefix("uses :"))
            else {
                continue;
            };
            let rest = rest.trim();
            let Some(raw_target) = rest.split_whitespace().next() else {
                continue;
            };
            // The value may be quoted (`uses: "actions/checkout@..."`).
            let target = raw_target.trim_matches(['"', '\'']);
            let Some((action, reference)) = target.split_once('@') else {
                continue;
            };
            let Some((_, expected_major, expected_sha, expected_version)) =
                actions.iter().find(|(candidate, ..)| *candidate == action)
            else {
                continue;
            };

            assert!(
                is_full_sha(reference),
                "{}:{}: {action} uses floating ref {reference:?}; every artifact/checkout action must be pinned to a full 40-character SHA (#339)",
                relative(&path),
                index + 1
            );
            assert_eq!(
                reference,
                *expected_sha,
                "{}:{}: {action} is pinned to SHA {reference}, but the decision record pins {expected_version} at {expected_sha}; \
                 an unrecorded action revision must not pass the gate (#339)",
                relative(&path),
                index + 1
            );

            let comment = rest[raw_target.len()..].trim();
            let version_token = comment
                .split_whitespace()
                .find(|token| token.starts_with('v') || token.starts_with('V'))
                .unwrap_or_else(|| {
                    panic!(
                        "{}:{}: {action} SHA {reference} has no inline version comment; the gate cannot verify upload v7 / download v8 / checkout v7",
                        relative(&path),
                        index + 1
                    )
                });
            let version = Version::parse(version_token);
            let expected_label = if *expected_major == 8 { "v8" } else { "v7" };
            let expected_major = *expected_major;
            assert_eq!(
                version.major,
                expected_major as u64,
                "{}:{}: {action} is annotated {version_token}; #339 requires {expected_label} (major {expected_major})",
                relative(&path),
                index + 1
            );
            assert_eq!(
                version_token.trim_start_matches(['v', 'V']),
                expected_version.trim_start_matches('v'),
                "{}:{}: {action} is annotated {version_token}; the decision record pins exactly {expected_version} for SHA {expected_sha} (#339)",
                relative(&path),
                index + 1
            );

            let slot = actions
                .iter()
                .position(|(candidate, ..)| *candidate == action)
                .unwrap();
            seen[slot] += 1;
        }
    }

    for (index, (action, major, ..)) in actions.iter().enumerate() {
        assert!(
            seen[index] > 0,
            "no {action} step found in .github/workflows/*.yml; #339 expects at least one SHA-pinned v{major} reference"
        );
    }
}
