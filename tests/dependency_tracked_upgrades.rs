use std::path::{Path, PathBuf};

use serde_yaml::Value as YamlValue;

const OXIRI_BAND: (u64, u64) = (0, 2);
/// Exact locked releases, not just bands: a patch bump still changes what CI
/// resolves, so the decision record pins these specific versions (#339).
const SPAREVAL_VERSION: &str = "0.2.9";
const OXIRI_VERSION: &str = "0.2.11";
const BASE_IMAGE: &str = "ubuntu:24.04";
const ICU_PACKAGE: &str = "libicu74";
const TYPESCRIPT_PIN: &str = "~6.0.3";
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

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn git_stdout(args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap_or_else(|error| panic!("git {args:?} output is not UTF-8: {error}"))
}

/// Every path tracked in `HEAD`. Git lists symlinks as blobs and never
/// recurses into them, so evidence enumeration cannot loop or escape the
/// checkout through a crafted directory symlink.
fn head_ref() -> String {
    std::env::var("TRACKED_UPGRADES_COMMIT").unwrap_or_else(|_| "HEAD".to_owned())
}

fn committed_files() -> Vec<String> {
    let head = head_ref();
    git_stdout(&["ls-tree", "-r", "--name-only", &head])
        .lines()
        .map(str::to_owned)
        .collect()
}

fn read_relative(relative: &str) -> String {
    git_stdout(&["show", &format!("{}:{relative}", head_ref())])
}

fn read_relative_lossy(relative: &str) -> String {
    let head = head_ref();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(env!("CARGO_MANIFEST_DIR"))
        .args(["show", &format!("{head}:{relative}")])
        .output()
        .unwrap_or_else(|error| panic!("read {relative}: {error}"));
    assert!(output.status.success(), "git show {head}:{relative} failed");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn read_path(path: &Path) -> String {
    read_relative(&relative(path))
}

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

fn relative(path: &Path) -> String {
    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Select committed paths with a predicate. Enumerating from `git ls-tree`
/// instead of walking the worktree pins evidence to HEAD and never recurses
/// into a directory symlink (git records a symlink as a single blob), so a
/// crafted link cannot loop or escape the checkout and exhaust CI.
fn committed_paths_where(predicate: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    select_committed_paths(&committed_files(), predicate)
}

fn select_committed_paths(committed: &[String], predicate: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = committed
        .iter()
        .filter(|path| predicate(path))
        .map(|path| repo_path(path))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

fn workflow_paths() -> Vec<PathBuf> {
    let paths = committed_paths_where(|path| {
        path.starts_with(".github/workflows/")
            && matches!(
                Path::new(path)
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("yml" | "yaml")
            )
    });
    assert!(
        !paths.is_empty(),
        "no committed workflow files found in .github/workflows; #339 expects at least one"
    );
    paths
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
/// executed just like a `uses` in a workflow, so it must join the scan. A
/// workflow may invoke a local action anywhere in the repository, so every
/// `action.yml`/`action.yaml` is scanned, not just `.github/actions`.
fn action_descriptor_paths() -> Vec<PathBuf> {
    committed_paths_where(|path| is_action_descriptor(Path::new(path)))
}

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
    const CI_DIRS: [&str; 11] = [
        "ci",
        ".ci",
        "build",
        ".build",
        "infra",
        ".devcontainer",
        ".circleci",
        ".buildkite",
        ".gitlab",
        ".azuredevops",
        "tools",
    ];
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
        || name == "jenkinsfile"
        || name == "azure-pipelines.yml"
        || name.starts_with(".travis")
        || name == "action.yml"
        || name == "action.yaml";
    is_build_file
        || (components.len() == 1
            && (name.ends_with(".yml")
                || name.ends_with(".yaml")
                || name.ends_with(".sh")
                || name.ends_with(".py")))
}

/// Every committed CI/build config, so a consumer added in a new CI location
/// cannot escape the no-consumer gate. Paths come from HEAD rather than a
/// filesystem walk, which also keeps symlinks from being traversed.
fn ci_paths() -> Vec<PathBuf> {
    committed_paths_where(|path| is_ci_config(Path::new(path)))
}

fn consumer_scan_paths(committed: &[String]) -> Vec<String> {
    const BINARY_EXTENSIONS: [&str; 12] = [
        "png", "jpg", "jpeg", "gif", "ico", "pdf", "zip", "gz", "woff", "woff2", "ttf", "bin",
    ];
    committed
        .iter()
        .filter(|path| {
            if path.starts_with("docs/")
                || path.starts_with(".NeuroStrata/")
                || path.starts_with("target/")
                || *path == "tests/dependency_tracked_upgrades.rs"
            {
                return false;
            }
            let extension = Path::new(path.as_str())
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            !BINARY_EXTENSIONS.contains(&extension.as_str())
        })
        .cloned()
        .collect()
}

fn from_image(line: &str) -> Option<String> {
    line.split_whitespace()
        .skip(1)
        .find(|token| !token.starts_with("--"))
        .map(str::to_owned)
}

fn dockerfile_stages(raw: &str) -> Vec<(String, Vec<String>)> {
    let mut stages: Vec<(String, Vec<String>)> = Vec::new();
    for line in dockerfile_logical_lines(raw) {
        if is_instruction(&line, "FROM") {
            let image = from_image(&line)
                .unwrap_or_else(|| panic!("Dockerfile.base FROM has no image: {line:?}"));
            stages.push((image, Vec::new()));
        } else if is_instruction(&line, "ARG") && stages.is_empty() {
            continue;
        } else if let Some((_, directives)) = stages.last_mut() {
            directives.push(line);
        } else {
            panic!("Dockerfile.base has a non-ARG directive before its first FROM: {line:?}");
        }
    }
    stages
}

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

fn shell_segments(command: &str) -> (Vec<Vec<String>>, Vec<String>) {
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    let mut operators = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(character) = chars.next() {
        match quote {
            Some(open) => {
                if character == open {
                    quote = None;
                } else if character == '\\' && open == '"' {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                } else {
                    word.push(character);
                }
            }
            None => match character {
                '\'' | '"' => quote = Some(character),
                '\\' => {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                }
                ';' | '|' | '&' => {
                    if !word.is_empty() {
                        segments.last_mut().unwrap().push(std::mem::take(&mut word));
                    }
                    let mut operator = character.to_string();
                    if chars.peek() == Some(&character) && (character == '|' || character == '&') {
                        operator.push(chars.next().unwrap());
                    }
                    operators.push(operator);
                    segments.push(Vec::new());
                }
                c if c.is_whitespace() => {
                    if !word.is_empty() {
                        segments.last_mut().unwrap().push(std::mem::take(&mut word));
                    }
                }
                _ => word.push(character),
            },
        }
    }
    if !word.is_empty() {
        segments.last_mut().unwrap().push(word);
    }
    (segments, operators)
}

fn strip_run(words: &[String]) -> &[String] {
    if words
        .first()
        .is_some_and(|word| word == "RUN" || word == "run")
    {
        &words[1..]
    } else {
        words
    }
}

fn segment_installs_package(words: &[String], package: &str) -> bool {
    let words = strip_run(words);
    let mut at = 0;
    if words.get(at).is_some_and(|word| word == "sudo") {
        at += 1;
    }
    while words.get(at).is_some_and(|word| word.contains('=')) {
        at += 1;
    }
    if !words
        .get(at)
        .is_some_and(|word| word == "apt-get" || word == "apt")
    {
        return false;
    }
    let Some(install_at) = words[at + 1..].iter().position(|word| word == "install") else {
        return false;
    };
    words[at + 1 + install_at + 1..]
        .iter()
        .any(|word| word == package)
}

/// Count only an apt install that is actually reached. `false &&` and
/// `true ||` short-circuit before the install, so they cannot satisfy the
/// base-image contract even though the text is present.
fn apt_installs_package(command: &str, package: &str) -> bool {
    let (segments, operators) = shell_segments(strip_shell_comment(command));
    let mut reached = true;
    for (index, segment) in segments.iter().enumerate() {
        if reached && segment_installs_package(segment, package) {
            return true;
        }
        let truth = match strip_run(segment).first().map(String::as_str) {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        };
        if let Some(operator) = operators.get(index) {
            reached = match operator.as_str() {
                "&&" => reached && truth != Some(false),
                "||" => reached && truth != Some(true),
                _ => reached,
            };
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

fn is_instruction(line: &str, name: &str) -> bool {
    line.get(..name.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
        && line
            .as_bytes()
            .get(name.len())
            .is_some_and(u8::is_ascii_whitespace)
}

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
        // Bind the annotation to a YAML `uses` key on this exact source line.
        // A `run:` command that merely mentions the target with a version
        // comment is not an action reference and cannot annotate one.
        let Some(key) = before.rfind("uses") else {
            continue;
        };
        if !before[key + 4..].trim_start().starts_with(':') {
            continue;
        }
        if !before[..key]
            .chars()
            .all(|character| matches!(character, ' ' | '\t' | '-' | '{' | ',' | '\'' | '"'))
        {
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

/// Parse `uses` values from raw YAML. A `uses` field is a single action
/// target: folding a version note into the scalar produces an invalid GitHub
/// reference, so the whole value must be one token with no comment marker.
fn parse_workflow_action_references(raw: &str, label: &str) -> Vec<(String, Option<String>)> {
    let document: YamlValue =
        serde_yaml::from_str(raw).unwrap_or_else(|error| panic!("parse YAML {label}: {error}"));
    let mut uses = Vec::new();
    collect_yaml_uses(&document, &mut uses);
    uses.into_iter()
        .map(|value| {
            assert!(
                !value.chars().any(char::is_whitespace) && !value.contains('#'),
                "{label}: invalid action reference {value:?}; a version note belongs in a YAML comment, not inside `uses`"
            );
            let target = value.trim().to_owned();
            let annotation = version_annotation(raw, &target);
            (target, annotation)
        })
        .collect()
}

fn workflow_action_references(path: &Path) -> Vec<(String, Option<String>)> {
    parse_workflow_action_references(&read_path(path), &relative(path))
}

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

fn discover_manifests(committed: &[String]) -> Vec<String> {
    let mut manifests: Vec<String> = KNOWN_MANIFESTS
        .iter()
        .map(|manifest| (*manifest).to_owned())
        .collect();
    for path in committed {
        if path.starts_with("plugins/")
            && path.ends_with("/package.json")
            && !manifests.contains(path)
        {
            manifests.push(path.clone());
        }
    }
    manifests.sort();
    manifests
}

fn manifest_count_matches_record(manifests: &[String]) -> bool {
    manifests.len() == KNOWN_MANIFESTS.len()
}

fn markdown_tables(document: &str) -> Vec<Vec<String>> {
    let mut tables: Vec<Vec<String>> = Vec::new();
    let mut rows: Vec<String> = Vec::new();
    for line in document.lines() {
        if line.starts_with('|') {
            rows.push(line.to_owned());
        } else if !rows.is_empty() {
            tables.push(std::mem::take(&mut rows));
        }
    }
    if !rows.is_empty() {
        tables.push(rows);
    }
    tables
}

fn current_state_table(document: &str) -> String {
    markdown_tables(document)
        .first()
        .map(|rows| rows.join("\n"))
        .unwrap_or_default()
}

fn table_cells(row: &str) -> Vec<String> {
    row.split('|')
        .map(|cell| cell.trim().trim_matches('`').to_owned())
        .collect()
}

fn current_state_row(tables: &[Vec<String>], item: &str) -> Option<String> {
    tables
        .first()?
        .iter()
        .find(|row| {
            table_cells(row)
                .get(1)
                .is_some_and(|cell| cell.eq_ignore_ascii_case(item))
        })
        .cloned()
}

fn action_row_tables<'a>(
    tables: &'a [Vec<String>],
    action: &TrackedAction,
) -> Vec<&'a Vec<String>> {
    tables
        .iter()
        .filter(|table| {
            table.iter().any(|row| {
                table_cells(row)
                    .get(1)
                    .is_some_and(|cell| cell == action.name)
            })
        })
        .collect()
}

fn decision_record_action_row(tables: &[Vec<String>], action: &TrackedAction) -> bool {
    let name = format!("`{}`", action.name);
    let version = format!("`{}`", action.version);
    let sha = format!("`{}`", action.sha);
    let tables = action_row_tables(tables, action);
    tables.len() == 1
        && tables[0].iter().any(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            cells.contains(&name.as_str())
                && cells.contains(&version.as_str())
                && cells.contains(&sha.as_str())
        })
}

/// Recognize a tracked action before splitting off its reference, so an
/// unqualified `uses: actions/checkout` is still seen and can be rejected.
fn match_tracked_reference(target: &str) -> Option<(&'static TrackedAction, &str)> {
    let (name, reference) = target.split_once('@').unwrap_or((target, ""));
    TRACKED_ACTIONS
        .iter()
        .find(|action| action.name == name)
        .map(|action| (action, reference))
}

#[test]
fn tracked_upgrades_decision_record_names_all_four() {
    const RECORD: &str = "docs/superpowers/specs/2026-11-25-tracked-upgrade-dispositions.md";
    assert!(
        committed_files().iter().any(|path| path == RECORD),
        "decision record {RECORD} is missing from HEAD; #339 requires one committed dated record for all four tracked upgrades"
    );
    let document = read_relative(RECORD);

    for marker in ["oxiri", "Dockerfile.base", "TypeScript", "artifact actions"] {
        assert!(
            document.contains(marker),
            "decision record {RECORD} does not name {marker:?}; all four tracked upgrades must stay documented"
        );
    }

    let tables = markdown_tables(&document);
    assert!(
        !current_state_table(&document).is_empty(),
        "decision record {RECORD} has no canonical current-state table"
    );
    let recorded: [(&str, Vec<String>); 4] = [
        (
            "oxiri",
            vec![
                format!("root pin `{}.{}`", OXIRI_BAND.0, OXIRI_BAND.1),
                format!("`spareval {SPAREVAL_VERSION}` edge `{OXIRI_VERSION}`"),
                format!("**Blocked** by `spareval {SPAREVAL_VERSION}`"),
            ],
        ),
        (
            "Dockerfile.base",
            vec![
                format!("`{BASE_IMAGE}` + `{ICU_PACKAGE}`"),
                "**Deferred** to Ubuntu 26.04 LTS".to_owned(),
            ],
        ),
        (
            "TypeScript",
            vec![
                format!("`{TYPESCRIPT_PIN}` in all 8 manifests"),
                "**Deferred** to TypeScript 7.1".to_owned(),
            ],
        ),
        (
            "artifact actions",
            vec![
                format!(
                    "upload `{}`, download `{}`, checkout `{}`",
                    TRACKED_ACTIONS[0].version,
                    TRACKED_ACTIONS[1].version,
                    TRACKED_ACTIONS[2].version
                ),
                "**Landed** in #340".to_owned(),
            ],
        ),
    ];
    for (item, facts) in &recorded {
        let row = current_state_row(&tables, item).unwrap_or_else(|| {
            panic!("canonical current-state table in {RECORD} has no row for {item}")
        });
        for fact in facts {
            assert!(
                row.contains(fact),
                "row for {item} in {RECORD} no longer records {fact:?}; facts must stay bound to their own row (#339)"
            );
        }
    }
    for action in &TRACKED_ACTIONS {
        assert!(
            decision_record_action_row(&tables, action),
            "decision record {RECORD} has no unique row pairing {} with {} and {}; swapping SHAs between actions or keeping a stale duplicate table must fail the gate (#339)",
            action.name, action.version, action.sha
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
    let committed = committed_files();
    let ci = ci_paths();
    assert!(
        !ci.is_empty(),
        "no CI/build configuration found to scan for Dockerfile.base consumers"
    );
    let files = consumer_scan_paths(&committed);
    assert!(
        ci.iter().all(|path| files.contains(&relative(path))),
        "CI configuration must be part of the consumer scan"
    );
    let offenders: Vec<String> = files
        .iter()
        .filter(|path| read_relative_lossy(path).contains("Dockerfile.base"))
        .cloned()
        .collect();

    assert!(
        offenders.is_empty(),
        "Dockerfile.base is deferred (#339) but these CI/local-action/build-config files reference it: {offenders:?}; \
         either remove the reference or complete the 26.04 LTS migration first"
    );
}

#[test]
fn typescript_pins_are_6_0_3_across_plugins_and_web_ui() {
    const PIN: &str = TYPESCRIPT_PIN;

    let committed = committed_files();
    for manifest in KNOWN_MANIFESTS {
        assert!(
            committed.iter().any(|path| path == manifest),
            "known TypeScript manifest {manifest} is missing from HEAD; #339 pins all eight manifests to {PIN}"
        );
    }
    let manifests = discover_manifests(&committed);
    assert!(
        manifest_count_matches_record(&manifests),
        "found {} manifests but the decision record pins TypeScript in all 8; a new plugin manifest must update the record and gate together",
        manifests.len()
    );

    for manifest in &manifests {
        let manifest = manifest.as_str();
        let raw = read_relative(manifest);
        let document: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or_else(|error| panic!("parse {manifest}: {error}"));
        let declarations = typescript_declarations(&document);
        assert!(
            !declarations.is_empty(),
            "{manifest} has no typescript dependency; #339 pins every plugin and web-ui manifest to {PIN}"
        );
        for typescript in declarations {
            assert_eq!(
                typescript, PIN,
                "{manifest} pins TypeScript {typescript:?}; #339 requires {PIN} across every plugin and web-ui manifest"
            );
            let parsed = Version::parse(typescript.trim().trim_start_matches('~'));
            assert_ne!(
                parsed.major, 7,
                "{manifest} selects TypeScript 7.x ({typescript:?}); the 7.1 migration is a separate tracked upgrade (#339)"
            );
        }
    }
}

#[test]
fn artifact_actions_are_upload_v7_download_v8_and_checkout_sha_pinned() {
    let mut seen = [0_usize; TRACKED_ACTIONS.len()];

    let mut paths = workflow_paths();
    paths.extend(action_descriptor_paths());
    for path in paths {
        for (index, (target, annotation)) in
            workflow_action_references(&path).into_iter().enumerate()
        {
            let Some((tracked, reference)) = match_tracked_reference(&target) else {
                continue;
            };
            let action = tracked.name;
            assert!(
                !reference.is_empty(),
                "{}:{}: {action} has no @ reference; every tracked action must be pinned to a full SHA even when another reference is correct (#339)",
                relative(&path),
                index + 1
            );

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

#[test]
fn version_annotation_ignores_run_line_that_mentions_uses() {
    let raw = "jobs:\n  build:\n    steps:\n      - uses: actions/checkout@abc\n      - run: echo \"uses: actions/checkout@abc # v7.0.1\"\n";
    assert_eq!(
        version_annotation(raw, "actions/checkout@abc"),
        None,
        "a run: command that mentions the target is not a uses key and cannot annotate it"
    );
    let flow =
        "jobs:\n  build:\n    steps:\n      - { uses: 'actions/checkout@abc', name: c } # v7.0.1\n";
    assert_eq!(
        version_annotation(flow, "actions/checkout@abc").as_deref(),
        Some("v7.0.1"),
        "a flow-style uses key on the same line still carries its annotation"
    );
}

#[test]
fn is_ci_config_recognizes_hosted_ci_and_root_pipelines() {
    for path in [
        ".circleci/config.yml",
        ".buildkite/pipeline.yml",
        ".gitlab-ci.yml",
        "Jenkinsfile",
        "azure-pipelines.yml",
    ] {
        assert!(
            is_ci_config(Path::new(path)),
            "{path} is a conventional CI config and must be scanned for Dockerfile.base consumers"
        );
    }
}

#[test]
fn committed_enumeration_never_follows_symlinks() {
    // A directory symlink is a single blob in git and is never recursed into.
    // Enumeration driven by the committed path list cannot reach paths that
    // exist only behind such a link.
    let committed = vec![
        "ci/config.yml".to_owned(),
        ".github/actions/local/action.yml".to_owned(),
        ".github/actions/local/link".to_owned(),
        "docs/notes.md".to_owned(),
    ];
    let ci = select_committed_paths(&committed, |path| is_ci_config(Path::new(path)));
    assert!(ci.iter().any(|path| relative(path) == "ci/config.yml"));
    assert!(ci
        .iter()
        .all(|path| committed.iter().any(|entry| entry == &relative(path))));
    assert!(!ci.iter().any(|path| relative(path).starts_with("ci/link/")));
    let descriptors =
        select_committed_paths(&committed, |path| is_action_descriptor(Path::new(path)));
    assert_eq!(
        descriptors
            .iter()
            .map(|path| relative(path))
            .collect::<Vec<_>>(),
        [".github/actions/local/action.yml"],
        "only committed descriptors are collected; a symlink blob is not expanded"
    );
}

#[test]
fn match_tracked_reference_detects_unqualified_target() {
    let (action, reference) =
        match_tracked_reference("actions/checkout").expect("checkout is tracked");
    assert_eq!(action.name, "actions/checkout");
    assert_eq!(
        reference, "",
        "an unqualified tracked target must be visible to the gate"
    );
    assert!(match_tracked_reference("actions/checkout@abc").is_some());
    assert!(match_tracked_reference("actions/untracked@abc").is_none());
}

#[test]
fn decision_record_action_rows_reject_swapped_shas() {
    let document = "| Action | Version | SHA |\n| --- | --- | --- |\n| `actions/upload-artifact` | `v7.0.2` | `aaa` |\n| `actions/checkout` | `v7.0.1` | `bbb` |\n";
    let upload = TrackedAction {
        name: "actions/upload-artifact",
        slot: 0,
        major: 7,
        sha: "aaa",
        version: "v7.0.2",
    };
    let tables = markdown_tables(document);
    assert!(decision_record_action_row(&tables, &upload));
    let swapped = TrackedAction {
        name: "actions/upload-artifact",
        slot: 0,
        major: 7,
        sha: "bbb",
        version: "v7.0.2",
    };
    assert!(
        !decision_record_action_row(&tables, &swapped),
        "swapping SHAs keeps every token present but must break the action/version/SHA pairing"
    );
}

#[test]
fn current_state_table_ignores_stale_prose() {
    let document = "| Item | Current state | Disposition |\n| --- | --- | --- |\n| `oxiri` | root pin `0.3` | **Blocked** by `spareval 0.2.9` |\n\nOld note: root pin `0.2` was recorded here.\n";
    let table = current_state_table(document);
    assert!(table.contains("root pin `0.3`"));
    assert!(
        !table.contains("root pin `0.2`"),
        "a stale copy in later prose must not satisfy the canonical current-state gate"
    );
}

#[test]
fn apt_installs_package_only_counts_reachable_commands() {
    assert!(apt_installs_package(
        "RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y git curl libicu74 ca-certificates",
        ICU_PACKAGE
    ));
    assert!(!apt_installs_package(
        "RUN echo \"apt-get install -y libicu74\"",
        ICU_PACKAGE
    ));
    assert!(!apt_installs_package(
        "RUN false && apt-get install -y libicu74",
        ICU_PACKAGE
    ));
    assert!(!apt_installs_package(
        "RUN true || apt-get install -y libicu74",
        ICU_PACKAGE
    ));
    assert!(!apt_installs_package(
        "RUN apt-get update && echo apt-get install libicu74",
        ICU_PACKAGE
    ));
}

#[test]
fn consumer_discovery_covers_ci_invoked_helpers() {
    for path in [
        ".azuredevops/pipelines/build.yml",
        "ci/build.sh",
        "tools/build_base.py",
        "build.sh",
    ] {
        assert!(
            is_ci_config(Path::new(path)),
            "{path} is a committed CI/build config and must be scanned"
        );
    }
    let committed = vec![
        ".github/workflows/ci.yml".to_owned(),
        "tools/build_base.py".to_owned(),
        "docs/superpowers/specs/notes.md".to_owned(),
        "tests/dependency_tracked_upgrades.rs".to_owned(),
    ];
    let scanned = consumer_scan_paths(&committed);
    assert!(scanned.contains(&"tools/build_base.py".to_owned()));
    assert!(!scanned.contains(&"docs/superpowers/specs/notes.md".to_owned()));
    assert!(!scanned.contains(&"tests/dependency_tracked_upgrades.rs".to_owned()));
}

#[test]
fn extra_manifest_breaks_the_recorded_count() {
    let mut committed = vec!["plugins/core/package.json".to_owned()];
    for name in [
        "claude",
        "codex",
        "hermes",
        "opencode",
        "github-cli",
        "openclaw",
        "ninth",
    ] {
        committed.push(format!("plugins/{name}/package.json"));
    }
    committed.push("web-ui/package.json".to_owned());
    let manifests = discover_manifests(&committed);
    assert_eq!(manifests.len(), 9);
    assert!(
        !manifest_count_matches_record(&manifests),
        "a ninth plugin manifest must fail the 'all 8 manifests' claim in the decision record"
    );
}

#[test]
fn invalid_action_reference_is_rejected() {
    let quoted = "jobs:\n  build:\n    steps:\n      - uses: 'actions/checkout@abc # v7.0.1'\n";
    assert!(
        std::panic::catch_unwind(|| parse_workflow_action_references(quoted, "quoted.yml"))
            .is_err()
    );
    let folded =
        "jobs:\n  build:\n    steps:\n      - uses: >-\n          actions/checkout@abc # v7.0.1\n";
    assert!(
        std::panic::catch_unwind(|| parse_workflow_action_references(folded, "folded.yml"))
            .is_err()
    );
    let valid = "jobs:\n  build:\n    steps:\n      - uses: actions/checkout@abc # v7.0.1\n";
    let refs = parse_workflow_action_references(valid, "valid.yml");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].0, "actions/checkout@abc");
    assert_eq!(refs[0].1.as_deref(), Some("v7.0.1"));
}

#[test]
fn current_state_facts_bind_to_their_own_row() {
    let document = "\
| Item | Current state | Disposition |
| --- | --- | --- |
| `oxiri` | root pin `0.2` | **Blocked** by `spareval 0.2.9` |
| `Dockerfile.base` | `ubuntu:24.04` + `libicu74` | **Deferred** to Ubuntu 26.04 LTS |

Stale: root pin `0.3` was recorded here.
";
    let tables = markdown_tables(document);
    assert_eq!(tables.len(), 1);
    assert_eq!(
        current_state_row(&tables, "oxiri").as_deref(),
        Some("| `oxiri` | root pin `0.2` | **Blocked** by `spareval 0.2.9` |")
    );
    assert!(!current_state_row(&tables, "oxiri")
        .unwrap()
        .contains("root pin `0.3`"));

    let swapped = "\
| Item | Current state | Disposition |
| --- | --- | --- |
| `oxiri` | `ubuntu:24.04` + `libicu74` | **Deferred** to Ubuntu 26.04 LTS |
| `Dockerfile.base` | root pin `0.2` | **Blocked** by `spareval 0.2.9` |
";
    let swapped_tables = markdown_tables(swapped);
    assert!(!current_state_row(&swapped_tables, "oxiri")
        .unwrap()
        .contains("root pin `0.2`"));

    let stale = format!(
        "{}\n\n| Action | Version | SHA |\n| --- | --- | --- |\n| `{}` | `{}` | `{}` |\n",
        document, TRACKED_ACTIONS[0].name, TRACKED_ACTIONS[0].version, TRACKED_ACTIONS[0].sha
    );
    let action = &TRACKED_ACTIONS[0];
    assert_eq!(action_row_tables(&markdown_tables(&stale), action).len(), 1);
    let duplicated = format!(
        "{stale}\n\n| Action | Version | SHA |\n| --- | --- | --- |\n| `{}` | `{}` | `{}` |\n",
        action.name, action.version, action.sha
    );
    assert_eq!(
        action_row_tables(&markdown_tables(&duplicated), action).len(),
        2,
        "a stale duplicate action table must not satisfy the gate"
    );
}
