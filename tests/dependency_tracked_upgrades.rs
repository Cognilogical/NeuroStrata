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

use serde_yaml::Value as YamlValue;

/// Values this gate enforces and the decision record must repeat verbatim.
/// Keeping them in one place makes a record-vs-gate drift a single-point edit.
const OXIRI_BAND: (u64, u64) = (0, 2);
/// Exact locked releases, not just bands: a patch bump still changes what CI
/// resolves, so the decision record pins these specific versions (#339).
const SPAREVAL_VERSION: &str = "0.2.9";
const OXIRI_VERSION: &str = "0.2.11";
const BASE_IMAGE: &str = "ubuntu:24.04";
const ICU_PACKAGE: &str = "libicu74";
const TYPESCRIPT_PIN: &str = "~6.0.3";

struct TrackedAction {
    name: &'static str,
    slot: usize,
    major: usize,
    sha: &'static str,
    version: &'static str,
}

const TRACKED_ACTIONS: [TrackedAction; 3] = [
    TrackedAction {
        name: "actions/upload-artifact",
        slot: 0,
        major: 7,
        sha: "cf430e030ddbb5b0abf93d22962f4752f3646cd9",
        version: "v7.0.2",
    },
    TrackedAction {
        name: "actions/download-artifact",
        slot: 1,
        major: 8,
        sha: "9000827ccba6bdab643e8b6fd33ac0654aef8333",
        version: "v8.0.2",
    },
    TrackedAction {
        name: "actions/checkout",
        slot: 2,
        major: 7,
        sha: "3d3c42e5aac5ba805825da76410c181273ba90b1",
        version: "v7.0.1",
    },
];

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
    // Only a plain, caret, tilde, or exact requirement naming a full
    // major.minor is confined to one band. Comparison ranges (`>=`, `>`),
    // wildcards, and multi-clause ranges can permit versions outside the
    // band even when their first number sits inside it, so reject them
    // instead of collapsing them onto that first number.
    assert!(
        !trimmed.contains([',', '*', '>', '<', '-']),
        "oxiri requirement {requirement:?} is a range or wildcard that can permit versions outside a single band; \
         Cargo hyphen ranges such as '0.2.0 - 0.3.0' are rejected before band derivation \
         because splitting on '-' would collapse them onto the first band (#339)"
    );
    let core = trimmed
        .strip_prefix('^')
        .or_else(|| trimmed.strip_prefix('~'))
        .or_else(|| trimmed.strip_prefix('='))
        .unwrap_or(trimmed);
    assert!(
        !core.is_empty() && core.chars().next().is_some_and(|c| c.is_ascii_digit()),
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
        .unwrap_or_else(|| {
            panic!(
                "oxiri requirement {requirement:?} names no minor component; a major-only requirement is not confined to one band"
            )
        })
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

/// All versions of a crate in the lockfile. Comparing a band instead of the
/// exact release lets a patch bump slip past the gate unnoticed.
fn locked_versions(lock: &toml::Value, name: &str) -> Vec<String> {
    lock_packages(lock)
        .iter()
        .filter(|package| package_name(package) == name)
        .map(package_version)
        .map(str::to_owned)
        .collect()
}

fn is_action_descriptor(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("action.yml" | "action.yaml")
    )
}

/// Local composite actions are workflow code: a tracked `uses` inside one is
/// executed just like a `uses` in a workflow, so it must join the scan.
fn action_descriptor_paths() -> Vec<PathBuf> {
    let directory = repo_path(".github/actions");
    if !directory.is_dir() {
        return Vec::new();
    }
    let mut paths = Vec::new();
    collect_files(&directory, &mut paths);
    paths.retain(|path| is_action_descriptor(path));
    paths.sort();
    paths
}

/// Is this file CI/build configuration that could invoke the deferred image?
/// Named directories and well-known build filenames cover consumers that live
/// outside `.github` and `scripts`; docs and source are deliberately excluded.
fn is_ci_config(path: &Path) -> bool {
    let components: Vec<String> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_owned)
        .collect();
    const EXCLUDED_DIRS: [&str; 5] = [".git", "target", "node_modules", "dist", ".NeuroStrata"];
    if components
        .iter()
        .any(|part| EXCLUDED_DIRS.contains(&part.as_str()))
    {
        return false;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_top_level = components
        .first()
        .is_some_and(|part| part == ".github" || part == "scripts");
    if is_top_level {
        return true;
    }
    const CI_DIRS: [&str; 6] = ["ci", ".ci", "build", ".build", "infra", ".devcontainer"];
    let parent_dirs = &components[..components.len().saturating_sub(1)];
    if parent_dirs
        .iter()
        .any(|part| CI_DIRS.contains(&part.as_str()))
    {
        return true;
    }
    let is_build_file = name.starts_with("dockerfile")
        || name.starts_with("docker-compose")
        || name.starts_with("makefile")
        || name == "justfile"
        || name.starts_with("taskfile")
        || name.ends_with(".mk")
        || name.ends_with(".nix")
        || name == "action.yml"
        || name == "action.yaml";
    is_build_file || (components.len() == 1 && (name.ends_with(".yml") || name.ends_with(".yaml")))
}

/// Walk the repository for every CI/build config, so a consumer added in a
/// new CI location cannot escape the no-consumer gate.
fn ci_paths() -> Vec<PathBuf> {
    fn walk(root: &Path, out: &mut Vec<PathBuf>) {
        let entries =
            fs::read_dir(root).unwrap_or_else(|error| panic!("read {}: {error}", root.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|error| panic!("read entry in {}: {error}", root.display()))
                .path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if matches!(
                    name,
                    ".git" | "target" | "node_modules" | "dist" | ".NeuroStrata"
                ) {
                    continue;
                }
                walk(&path, out);
            } else if is_ci_config(Path::new(&relative(&path))) {
                out.push(path);
            }
        }
    }
    let root = repo_path("");
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    paths.sort();
    paths.dedup();
    paths
}

/// `FROM` accepts option flags such as `--platform=` before the image name.
fn from_image(line: &str) -> Option<String> {
    line.split_whitespace()
        .skip(1)
        .find(|token| !token.starts_with("--"))
        .map(str::to_owned)
}

/// Parse Dockerfile stages once: global `ARG` before the first `FROM` is
/// legal and is not a stage directive; continuation lines are already joined.
fn dockerfile_stages(raw: &str) -> Vec<(String, Vec<String>)> {
    let mut stages: Vec<(String, Vec<String>)> = Vec::new();
    for line in dockerfile_logical_lines(raw) {
        if is_instruction(&line, "FROM") {
            let image = from_image(&line)
                .unwrap_or_else(|| panic!("Dockerfile.base FROM has no image: {line:?}"));
            stages.push((image, Vec::new()));
        } else if is_instruction(&line, "ARG") && stages.is_empty() {
            // Global build arguments precede the first stage without error.
            continue;
        } else if let Some((_, directives)) = stages.last_mut() {
            directives.push(line);
        } else {
            panic!("Dockerfile.base has a non-ARG directive before its first FROM: {line:?}");
        }
    }
    stages
}

/// Shell `#` comments only start at a word boundary and never inside quotes.
fn strip_shell_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut word_boundary = true;
    for (index, character) in line.char_indices() {
        match quote {
            Some(open) => {
                if character == open {
                    quote = None;
                }
                word_boundary = character.is_whitespace();
            }
            None => {
                if character == '\'' || character == '"' {
                    quote = Some(character);
                    word_boundary = false;
                } else if character == '#' && word_boundary {
                    return &line[..index];
                } else {
                    word_boundary = character.is_whitespace();
                }
            }
        }
    }
    line
}

/// Recognize an actual `apt`/`apt-get install <package>` command, ignoring
/// shell comments, `echo`, and other mentions of the package. Options may
/// appear before `install` (`apt-get -y install ...` is valid).
fn apt_installs_package(command: &str, package: &str) -> bool {
    let command = strip_shell_comment(command);
    for part in command.split([';', '|', '&']) {
        let tokens: Vec<&str> = part
            .split_whitespace()
            .map(|token| token.trim_matches(['\'', '"']))
            .filter(|token| !token.is_empty())
            .collect();
        let mut at = 0;
        if tokens.first() == Some(&"sudo") {
            at = 1;
        }
        while tokens.get(at).is_some_and(|token| token.contains('=')) {
            at += 1;
        }
        let Some(binary) = tokens.get(at) else {
            continue;
        };
        if *binary != "apt-get" && *binary != "apt" {
            continue;
        }
        let Some(install_at) = tokens[at + 1..]
            .iter()
            .position(|token| *token == "install")
        else {
            continue;
        };
        let arguments = &tokens[at + 1 + install_at + 1..];
        if arguments.contains(&package) {
            return true;
        }
    }
    false
}

fn is_full_sha(reference: &str) -> bool {
    reference.len() == 40
        && reference
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

/// Dockerfile instruction names are case-insensitive. Require a whitespace
/// delimiter so `FROMAGE` or `RUNNER` cannot be mistaken for an instruction.
fn is_instruction(line: &str, name: &str) -> bool {
    line.get(..name.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
        && line
            .as_bytes()
            .get(name.len())
            .is_some_and(u8::is_ascii_whitespace)
}

/// Join Dockerfile continuation lines and drop blank/comment lines. A stage's
/// `RUN ... apt-get install ...` may be split across several physical lines;
/// the build sees one logical instruction, so the gate must too.
fn dockerfile_logical_lines(raw: &str) -> Vec<String> {
    let mut pending = String::new();
    let mut logical = Vec::new();
    for physical in raw.lines() {
        let trimmed = physical.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(continued) = trimmed.strip_suffix('\\') {
            pending.push_str(continued.trim_end());
            pending.push(' ');
            continue;
        }
        pending.push_str(trimmed);
        logical.push(std::mem::take(&mut pending));
    }
    if !pending.is_empty() {
        logical.push(pending);
    }
    logical
}

/// Only step `uses` declarations are executed as actions. Nested data such
/// as `with.uses` is not an action reference and must not be collected.
fn collect_step_uses(steps: &YamlValue, out: &mut Vec<String>) {
    let Some(sequence) = steps.as_sequence() else {
        return;
    };
    for step in sequence {
        let Some(mapping) = step.as_mapping() else {
            continue;
        };
        if let Some(uses) = mapping.get("uses").and_then(|value| value.as_str()) {
            out.push(uses.trim().to_owned());
        }
    }
}

/// A reusable-workflow job declares `uses` at job level; its `steps` (if any)
/// are ordinary action steps. Only those two forms execute GitHub Actions.
fn collect_job_uses(job: &YamlValue, out: &mut Vec<String>) {
    let Some(mapping) = job.as_mapping() else {
        return;
    };
    if let Some(uses) = mapping.get("uses").and_then(|value| value.as_str()) {
        out.push(uses.trim().to_owned());
    }
    if let Some(steps) = mapping.get("steps") {
        collect_step_uses(steps, out);
    }
}

/// Collect executable `uses` declarations from a workflow or composite
/// action descriptor: workflow jobs and their steps, plus `runs.steps`.
fn collect_yaml_uses(value: &YamlValue, out: &mut Vec<String>) {
    let Some(root) = value.as_mapping() else {
        return;
    };
    if let Some(jobs) = root.get("jobs").and_then(|value| value.as_mapping()) {
        for (_, job) in jobs {
            collect_job_uses(job, out);
        }
    }
    if let Some(runs) = root.get("runs").and_then(|value| value.as_mapping()) {
        if let Some(steps) = runs.get("steps") {
            collect_step_uses(steps, out);
        }
    }
}

/// Pair a `uses` target with its version annotation. Folded scalars keep the
/// `# vN.N.N` text inside the value; ordinary steps write it as a YAML
/// comment on the same source line, which the parser strips. Only that exact
/// line may supply the annotation — a nearby unrelated comment is not
/// evidence for this reference.
fn version_annotation(raw: &str, target: &str) -> Option<String> {
    let mut annotations: Vec<Option<String>> = Vec::new();
    for line in raw.lines() {
        let Some(position) = line.find(target) else {
            continue;
        };
        let before = &line[..position];
        if !before.contains("uses") || !before.contains(':') {
            continue;
        }
        let after = &line[position + target.len()..];
        let comment = after.split_once('#').map(|(_, rest)| rest).unwrap_or("");
        annotations.push(
            comment
                .split_whitespace()
                .find(|token| token.starts_with('v') || token.starts_with('V'))
                .map(str::to_owned),
        );
    }
    if annotations.is_empty() || annotations.iter().any(Option::is_none) {
        return None;
    }
    let mut distinct: Vec<String> = annotations.into_iter().flatten().collect();
    distinct.sort();
    distinct.dedup();
    if distinct.len() == 1 {
        distinct.pop()
    } else {
        None
    }
}

/// Every tracked-action reference in a workflow, structurally parsed.
fn workflow_action_references(path: &Path) -> Vec<(String, Option<String>)> {
    let raw = read_path(path);
    let document: YamlValue = serde_yaml::from_str(&raw)
        .unwrap_or_else(|error| panic!("parse YAML {}: {error}", path.display()));
    let mut uses = Vec::new();
    collect_yaml_uses(&document, &mut uses);
    uses.into_iter()
        .map(|value| {
            let mut tokens = value.split_whitespace();
            let target = tokens.next().unwrap_or("").to_owned();
            let inline = tokens
                .find(|token| token.starts_with('v') || token.starts_with('V'))
                .map(str::to_owned);
            let annotation = inline.or_else(|| version_annotation(&raw, &target));
            (target, annotation)
        })
        .collect()
}

/// Every `typescript` declaration in a manifest. Peer and optional ranges
/// are executable constraints too, so a conflicting compiler there must be
/// as visible as one in `dependencies` or `devDependencies`.
fn typescript_declarations(document: &serde_json::Value) -> Vec<String> {
    [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ]
    .iter()
    .filter_map(|section| {
        document
            .get(*section)
            .and_then(|dependencies| dependencies.get("typescript"))
            .and_then(|value| value.as_str())
    })
    .map(|value| value.trim().to_owned())
    .collect()
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

    // Marker-only checks let the record drift away from what the gate
    // actually enforces. Every recorded version and disposition named by
    // the tests below must therefore appear verbatim in the record.
    let recorded = [
        format!("root pin `{}.{}`", OXIRI_BAND.0, OXIRI_BAND.1),
        format!("`spareval {SPAREVAL_VERSION}` edge `{OXIRI_VERSION}`"),
        format!("`{BASE_IMAGE}` + `{ICU_PACKAGE}`"),
        format!("`{TYPESCRIPT_PIN}` in all 8 manifests"),
        format!(
            "upload `{}`, download `{}`, checkout `{}`",
            TRACKED_ACTIONS[0].version, TRACKED_ACTIONS[1].version, TRACKED_ACTIONS[2].version
        ),
        format!("**Blocked** by `spareval {SPAREVAL_VERSION}`"),
        "**Deferred** to Ubuntu 26.04 LTS".to_owned(),
        "**Deferred** to TypeScript 7.1".to_owned(),
        "**Landed** in #340".to_owned(),
    ];
    for fact in &recorded {
        assert!(
            document.contains(fact),
            "decision record {} no longer records {fact:?}; versions and dispositions must move with the gate (#339)",
            path.display()
        );
    }
    for action in &TRACKED_ACTIONS {
        for token in [action.sha, action.version] {
            assert!(
                document.contains(token),
                "decision record {} no longer records {token:?} for {}; the SHA/version table must match the workflow gate (#339)",
                path.display(),
                action.name
            );
        }
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
    // The recorded band itself is part of the gate: a coordinated move of
    // both the root pin and the spareval edge to a new band must fail here
    // until this test and the decision record are updated together (#339).
    assert_eq!(
        pin_band,
        (0, 2),
        "Cargo.toml pins {OXIRI} = {requirement:?} (band {}.{}), but the decision record holds the pin at band 0.2; \
         a coordinated band move is the tracked upgrade itself and must update this gate and the record in the same change",
        pin_band.0, pin_band.1
    );

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
    for version in &pinned_band_packages {
        assert_eq!(
            *version, OXIRI_VERSION,
            "Cargo.lock resolves {OXIRI} {version} in the pinned band, but the decision record pins {OXIRI_VERSION}; \
             a patch bump is a tracked upgrade and must update the gate and record together (#339)"
        );
    }

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
    for spareval in &sparevals {
        let version = package_version(spareval);
        assert_eq!(
            version, SPAREVAL_VERSION,
            "Cargo.lock resolves {SPAREVAL} {version}, but the decision record pins {SPAREVAL_VERSION}; \
             a patch bump is a tracked upgrade and must update the gate and record together (#339)"
        );
    }

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
            assert_eq!(
                edge_version, OXIRI_VERSION,
                "{SPAREVAL} {spareval_version} depends on {OXIRI} {edge_version}, but the decision record pins {OXIRI_VERSION}; \
                 a patch bump must update the gate and record together (#339)"
            );

            assert!(
                packages.iter().any(|package| {
                    package_name(package) == OXIRI && package_version(package) == edge_version
                }),
                "{SPAREVAL} {spareval_version} names {OXIRI} {edge_version}, but Cargo.lock has no such {OXIRI} package"
            );

            let edge_band = (edge_parsed.major, edge_parsed.minor);
            assert_eq!(
                edge_band,
                (0, 2),
                "{SPAREVAL} {spareval_version} depends on {OXIRI} {edge_version} (band {}.{}), but the decision record holds the spareval edge at band 0.2 (#339)",
                edge_band.0, edge_band.1
            );
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
    // Check every stage, not just the first: a later stage on a different
    // base would build from an unrecorded image while an earlier stage still
    // satisfies a first-FROM-only gate. The parser joins continuations,
    // matches instruction casing as Docker does, skips `FROM --platform=`
    // options, and allows global `ARG` before the first stage.
    let stages = dockerfile_stages(&dockerfile);
    assert!(
        !stages.is_empty(),
        "Dockerfile.base has no active FROM directive"
    );

    for (index, (image, directives)) in stages.iter().enumerate() {
        assert_eq!(
            image, BASE_IMAGE,
            "Dockerfile.base stage {} builds from {image:?}; every active stage must stay on {BASE_IMAGE} until the 26.04 LTS migration in #339 is unblocked",
            index + 1
        );
        // A real apt install command, not a comment or an `echo` mentioning
        // the package. Options such as `-y` may precede `install`.
        let installs_icu = directives
            .iter()
            .any(|line| is_instruction(line, "RUN") && apt_installs_package(line, ICU_PACKAGE));
        assert!(
            installs_icu,
            "Dockerfile.base stage {} must install {ICU_PACKAGE} in an active RUN/apt directive; a different package means the base image moved",
            index + 1
        );
    }
}

#[test]
fn dockerfile_base_has_no_ci_consumer() {
    // Scan CI/build configuration across the repository, not just a fixed
    // list of directories: a consumer added in `ci/`, a root YAML pipeline,
    // or a new build config must still fail the no-consumer gate.
    let files = ci_paths();
    assert!(
        !files.is_empty(),
        "no CI/build configuration found to scan for Dockerfile.base consumers"
    );

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
        "Dockerfile.base is deferred (#339) but these CI/local-action/build-config files reference it: {offenders:?}; \
         either remove the reference or complete the 26.04 LTS migration first"
    );
}

#[test]
fn typescript_pins_are_6_0_3_across_plugins_and_web_ui() {
    const KNOWN_MANIFESTS: [&str; 8] = [
        "plugins/core/package.json",
        "plugins/claude/package.json",
        "plugins/codex/package.json",
        "plugins/hermes/package.json",
        "plugins/opencode/package.json",
        "plugins/github-cli/package.json",
        "plugins/openclaw/package.json",
        "web-ui/package.json",
    ];
    const PIN: &str = TYPESCRIPT_PIN;

    // The known eight must still exist — a deleted manifest fails here —
    // but the checked set is discovered from the repository, so a newly
    // added plugins/*/package.json is gated too instead of escaping.
    for manifest in KNOWN_MANIFESTS {
        assert!(
            repo_path(manifest).is_file(),
            "known TypeScript manifest {manifest} is missing; #339 pins all eight manifests to {PIN}"
        );
    }
    let mut manifests: Vec<String> = KNOWN_MANIFESTS
        .iter()
        .map(|manifest| (*manifest).to_owned())
        .collect();
    let plugins_dir = repo_path("plugins");
    let mut plugin_entries: Vec<PathBuf> = fs::read_dir(&plugins_dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", plugins_dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read entry in {}: {error}", plugins_dir.display()))
                .path()
        })
        .collect();
    plugin_entries.sort();
    for entry in plugin_entries {
        let candidate = entry.join("package.json");
        if candidate.is_file() {
            let discovered = relative(&candidate).replace('\\', "/");
            if !manifests.contains(&discovered) {
                manifests.push(discovered);
            }
        }
    }
    manifests.sort();

    for manifest in &manifests {
        let manifest = manifest.as_str();
        let path = repo_path(manifest);
        let raw = read_relative(manifest);
        let document: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        // Both dependency maps are checked: a manifest that declares
        // TypeScript twice must not pass on the dev pin while a conflicting
        // runtime pin silently selects a different compiler.
        let declarations = typescript_declarations(&document);
        assert!(
            !declarations.is_empty(),
            "{} has no typescript dependency; #339 pins every plugin and web-ui manifest to {PIN}",
            path.display()
        );
        for typescript in declarations {
            assert_eq!(
                typescript, PIN,
                "{} pins TypeScript {typescript:?}; #339 requires {PIN} across every plugin and web-ui manifest",
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
}

#[test]
fn artifact_actions_are_upload_v7_download_v8_and_checkout_sha_pinned() {
    let mut seen = [0_usize; TRACKED_ACTIONS.len()];

    let mut paths = workflow_paths();
    // Local composite actions execute workflow steps too; a tracked `uses`
    // inside one must be pinned exactly like a workflow reference.
    paths.extend(action_descriptor_paths());
    for path in paths {
        // Parse YAML structurally: line scanning skips valid forms such as
        // extra spacing after the dash, folded scalars, flow-style mappings,
        // and whitespace before the `uses` colon. Every tracked reference in
        // the document must be pinned, even when another ordinary step keeps
        // a presence check satisfied.
        for (index, (target, annotation)) in
            workflow_action_references(&path).into_iter().enumerate()
        {
            let Some((action, reference)) = target.split_once('@') else {
                continue;
            };
            let Some(tracked) = TRACKED_ACTIONS
                .iter()
                .find(|candidate| candidate.name == action)
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
                reference, tracked.sha,
                "{}:{}: {action} is pinned to SHA {reference}, but the decision record pins {} at {}; \
                 an unrecorded action revision must not pass the gate (#339)",
                relative(&path),
                index + 1,
                tracked.version,
                tracked.sha
            );

            let version_token = annotation.unwrap_or_else(|| {
                panic!(
                    "{}:{}: {action} SHA {reference} has no version annotation; the gate cannot verify upload v7 / download v8 / checkout v7",
                    relative(&path),
                    index + 1
                )
            });
            let version = Version::parse(&version_token);
            assert_eq!(
                version.major,
                tracked.major as u64,
                "{}:{}: {action} is annotated {version_token}; #339 requires major {}",
                relative(&path),
                index + 1,
                tracked.major
            );
            assert_eq!(
                version_token.trim_start_matches(['v', 'V']),
                tracked.version.trim_start_matches('v'),
                "{}:{}: {action} is annotated {version_token}; the decision record pins exactly {} for SHA {} (#339)",
                relative(&path),
                index + 1,
                tracked.version,
                tracked.sha
            );

            seen[tracked.slot] += 1;
        }
    }

    for tracked in &TRACKED_ACTIONS {
        assert!(
            seen[tracked.slot] > 0,
            "no {} step found in .github/workflows/*.yml or .github/actions/*/action.y*ml; #339 expects at least one SHA-pinned {} reference",
            tracked.name,
            tracked.version
        );
    }
}

#[test]
fn requirement_band_rejects_cargo_hyphen_ranges() {
    let outcome = std::panic::catch_unwind(|| requirement_band("0.2.0 - 0.3.0"));
    assert!(
        outcome.is_err(),
        "a Cargo hyphen range must be rejected before band derivation, not collapsed onto band 0.2"
    );
}

#[test]
fn dockerfile_logical_lines_join_continuations_and_match_case_insensitively() {
    // Lowercase instructions and a split apt install are both valid Docker.
    let raw = "from ubuntu:24.04\nrun apt-get update && \\\n  DEBIAN_FRONTEND=noninteractive apt-get install -y libicu74\n";
    let lines = dockerfile_logical_lines(raw);
    assert_eq!(
        lines.len(),
        2,
        "continuation lines must be joined: {lines:?}"
    );
    assert!(is_instruction(&lines[0], "FROM"));
    assert!(is_instruction(&lines[1], "RUN"));
    assert!(
        lines[1].contains("apt-get install")
            && lines[1]
                .split(|character: char| character.is_ascii_whitespace() || character == '&')
                .any(|token| token == ICU_PACKAGE)
    );
    assert!(!is_instruction("FROMAGE ubuntu:24.04", "FROM"));
}

#[test]
fn yaml_uses_collection_covers_all_valid_forms() {
    let yaml = "jobs:\n  build:\n    steps:\n      -   uses: actions/checkout@one # v7.0.1\n      - name: Folded\n        uses: >-\n          actions/upload-artifact@two\n      - { uses: 'actions/download-artifact@three', name: Fetch }\n      - uses : actions/checkout@four # v7.0.1\n";
    let document: YamlValue = serde_yaml::from_str(yaml).expect("valid YAML fixture");
    let mut uses = Vec::new();
    collect_yaml_uses(&document, &mut uses);
    let targets: Vec<&str> = uses
        .iter()
        .map(|value| value.split_whitespace().next().unwrap_or(""))
        .collect();
    assert_eq!(
        targets,
        [
            "actions/checkout@one",
            "actions/upload-artifact@two",
            "actions/download-artifact@three",
            "actions/checkout@four"
        ],
        "every valid YAML uses form must be found: {uses:?}"
    );
}

#[test]
fn version_annotation_uses_only_its_own_uses_line() {
    // A nearby comment on another line is not evidence for this reference.
    let raw = "steps:\n  - uses: actions/checkout@abc\n    name: step\n# v7.0.1\n";
    assert_eq!(
        version_annotation(raw, "actions/checkout@abc"),
        None,
        "an action with no inline annotation must fail even when a neighbouring line carries the expected version"
    );
    let annotated = "steps:\n  - uses: actions/checkout@abc # v7.0.1\n";
    assert_eq!(
        version_annotation(annotated, "actions/checkout@abc").as_deref(),
        Some("v7.0.1"),
        "the annotation on the same uses line must still be found"
    );
}

#[test]
fn apt_installs_package_parses_options_and_ignores_non_commands() {
    assert!(apt_installs_package(
        "apt-get update && apt-get -y install --no-install-recommends libicu74",
        "libicu74"
    ));
    assert!(apt_installs_package(
        "apt -qq install libicu74 ca-certificates",
        "libicu74"
    ));
    assert!(!apt_installs_package(
        "# apt-get install libicu74",
        "libicu74"
    ));
    assert!(!apt_installs_package(
        "echo 'apt-get install libicu74'",
        "libicu74"
    ));
    assert!(!apt_installs_package("apt-get remove libicu74", "libicu74"));
}

#[test]
fn locked_versions_exposes_patch_drift() {
    let lock: toml::Value = toml::from_str(
        "[[package]]\nname = \"spareval\"\nversion = \"0.2.10\"\n\
         [[package]]\nname = \"oxiri\"\nversion = \"0.2.11\"\n",
    )
    .expect("valid lock fixture");
    assert_eq!(
        locked_versions(&lock, "spareval"),
        ["0.2.10"],
        "a patch bump must be visible so the exact recorded version can be asserted"
    );
}

#[test]
fn action_descriptor_paths_recognize_composite_descriptors() {
    assert!(is_action_descriptor(Path::new(
        ".github/actions/setup/action.yml"
    )));
    assert!(is_action_descriptor(Path::new(
        ".github/actions/setup/action.yaml"
    )));
    assert!(!is_action_descriptor(Path::new(".github/workflows/ci.yml")));
}

#[test]
fn is_ci_config_covers_external_ci_locations() {
    for path in [
        ".github/actions/setup/action.yml",
        "ci/build.yml",
        ".ci/verify.yaml",
        "build/Dockerfile.custom",
        "Dockerfile.build",
        "Makefile",
    ] {
        assert!(
            is_ci_config(Path::new(path)),
            "{path} is a CI/build config and must be scanned for Dockerfile.base consumers"
        );
    }
    for path in [
        "docs/superpowers/specs/record.md",
        "tests/dependency_tracked_upgrades.rs",
        "src/main.rs",
    ] {
        assert!(
            !is_ci_config(Path::new(path)),
            "{path} is not a CI consumer; scanning it would produce false positives"
        );
    }
}

#[test]
fn typescript_declarations_collect_all_dependency_sections() {
    let document: serde_json::Value = serde_json::from_str(
        r#"{"dependencies":{"typescript":"7.0.0"},"devDependencies":{"typescript":"~6.0.3"},"peerDependencies":{"typescript":"^7.0.0"},"optionalDependencies":{"typescript":"6.1.0"}}"#,
    )
    .expect("valid JSON fixture");
    assert_eq!(
        typescript_declarations(&document),
        ["7.0.0", "~6.0.3", "^7.0.0", "6.1.0"],
        "peer and optional ranges are executable constraints too and must be visible to the gate"
    );
}

#[test]
fn from_image_skips_platform_options() {
    assert_eq!(
        from_image("FROM --platform=linux/amd64 ubuntu:24.04"),
        Some("ubuntu:24.04".to_owned())
    );
    assert_eq!(
        from_image("from ubuntu:24.04"),
        Some("ubuntu:24.04".to_owned())
    );
}

#[test]
fn dockerfile_stages_allow_global_arg() {
    let raw = "ARG BASE_IMAGE=ubuntu:24.04\n\
               from --platform=linux/amd64 ubuntu:24.04\n\
               run apt-get -y install libicu74\n";
    let stages = dockerfile_stages(raw);
    assert_eq!(stages.len(), 1, "global ARG must not be treated as a stage");
    assert_eq!(stages[0].0, "ubuntu:24.04");
    assert_eq!(stages[0].1.len(), 1);
}

#[test]
fn yaml_uses_collection_ignores_nested_data_fields() {
    let yaml = "jobs:\n  build:\n    steps:\n      - uses: actions/checkout@real # v7.0.1\n        with:\n          uses: actions/checkout@data # v1\n  call:\n    uses: actions/checkout@reusable # v7.0.1\n    with:\n      uses: actions/checkout@noise # v1\n";
    let document: YamlValue = serde_yaml::from_str(yaml).expect("valid YAML fixture");
    let mut uses = Vec::new();
    collect_yaml_uses(&document, &mut uses);
    assert_eq!(
        uses,
        ["actions/checkout@real", "actions/checkout@reusable"],
        "only executable step and reusable-job uses declarations are action references"
    );
}
