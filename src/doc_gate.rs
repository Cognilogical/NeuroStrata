//! Documentation Manifest & Surface-Diff Gate.
//!
//! Consumes the pluggable workflow connector for the changed-surface set and
//! evaluates the 3-tier Documentation Manifest (`docs/manifest.json`).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocTierSpec {
    pub dir: String,
    pub description: String,
    #[serde(default)]
    pub required_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocTiers {
    pub vision: DocTierSpec,
    pub features_api: DocTierSpec,
    pub extend: DocTierSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceDocRule {
    pub id: String,
    pub tier: String,
    #[serde(default)]
    pub watch_paths: Vec<String>,
    pub target_doc: String,
    #[serde(default)]
    pub required_headings: Vec<String>,
    #[serde(default)]
    pub required_tokens: Vec<String>,
    #[serde(default)]
    pub require_co_change_on_touch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocManifest {
    pub schema_version: u32,
    #[serde(default)]
    pub allowed_root_files: Vec<String>,
    pub tiers: DocTiers,
    #[serde(default)]
    pub support_dirs: Vec<String>,
    #[serde(default)]
    pub prohibited_filename_substrings: Vec<String>,
    #[serde(default)]
    pub surface_rules: Vec<SurfaceDocRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocViolation {
    pub rule_id: String,
    pub kind: String,
    pub path: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocGateReport {
    pub passed: bool,
    pub connector_id: String,
    pub checked_tiers: usize,
    pub checked_rules: usize,
    pub changed_paths: Vec<String>,
    pub violations: Vec<DocViolation>,
}

pub fn load_manifest(root: &Path) -> Result<DocManifest> {
    let manifest_path = root.join("docs").join("manifest.json");
    let json = std::fs::read_to_string(&manifest_path)?;
    Ok(serde_json::from_str(&json)?)
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_files_recursive(&p, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

/// Markdown files under `docs/` (including `support_dirs`) subject to the
/// machine-path content rule.
///
/// Inside a git worktree (`root.join(".git").exists()`), only git-tracked
/// markdown files under `docs/` are scanned so an untracked local scratch file
/// never blocks the gate. Outside a git worktree (temp-dir unit tests), every
/// markdown file under `docs/` is scanned.
fn collect_doc_markdown_files(root: &Path) -> Vec<std::path::PathBuf> {
    let tracked: Option<std::collections::BTreeSet<String>> = if root.join(".git").exists() {
        std::process::Command::new("git")
            .args(["ls-files", "--", "docs"])
            .current_dir(root)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(|line| line.trim().replace('\\', "/"))
                    .filter(|line| !line.is_empty())
                    .collect()
            })
    } else {
        None
    };

    let mut files = Vec::new();
    collect_files_recursive(&root.join("docs"), &mut files);
    files
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .filter(|p| {
            let Some(tracked) = &tracked else {
                return true;
            };
            let Ok(rel) = p.strip_prefix(root) else {
                return false;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            tracked.contains(&rel)
        })
        .collect()
}

/// Extracts public surface identifiers (CLI `#[command(name = "...")]` subcommands,
/// MCP `"name": "neurostrata_..."` tool identifiers, and `pub trait <Name>` declarations)
/// from added diff lines or source text.
pub fn extract_public_surface_identifiers(text: &str) -> Vec<String> {
    let mut idents = std::collections::BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(idx) = trimmed.find("#[command(name = \"") {
            let rest = &trimmed[idx + "#[command(name = \"".len()..];
            if let Some(end) = rest.find('"') {
                let name = &rest[..end];
                if !name.is_empty() && name != "neurostrata-mcp" {
                    idents.insert(name.to_string());
                }
            }
        }
        if let Some(idx) = trimmed.find("\"name\": \"neurostrata_") {
            let rest = &trimmed[idx + "\"name\": \"".len()..];
            if let Some(end) = rest.find('"') {
                let name = &rest[..end];
                if !name.is_empty() {
                    idents.insert(name.to_string());
                }
            }
        }
        if let Some(idx) = trimmed.find("pub trait ") {
            let rest = &trimmed[idx + "pub trait ".len()..];
            let ident: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !ident.is_empty() && ident != "AsAny" {
                idents.insert(ident);
            }
        }
    }
    idents.into_iter().collect()
}

fn rename_source(root: &Path, range: &str, rel_path: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["diff", "-M", "--name-status", "--diff-filter=R", range])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            (fields.len() == 3 && fields[2] == rel_path).then(|| fields[1].to_string())
        })
}

fn watched_added_lines(root: &Path, rel_path: &str) -> String {
    if root.join(".git").exists() {
        let tracked = std::process::Command::new("git")
            .args(["ls-files", "--error-unmatch", "--", rel_path])
            .current_dir(root)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        let mut added = Vec::new();
        let mut inspected = false;
        // Shared committed baseline (connector::committed_diff_range):
        // @{u}..HEAD, else origin/main..HEAD, else no committed range —
        // deliberately no silent HEAD~1..HEAD degradation.
        let range = crate::workflow::connector::committed_diff_range(root);
        let mut diff_paths = vec![rel_path.to_string()];
        if let Some(source) = range
            .as_deref()
            .and_then(|range| rename_source(root, range, rel_path))
            .or_else(|| rename_source(root, "HEAD", rel_path))
        {
            // Both endpoints are required for Git to recognize pure renames.
            diff_paths.insert(0, source);
        }
        let mut diffs: Vec<Vec<String>> = Vec::new();
        if let Some(range) = &range {
            let mut args = vec![
                "diff".into(),
                "-U0".into(),
                "-M".into(),
                range.clone(),
                "--".into(),
            ];
            args.extend(diff_paths.clone());
            diffs.push(args);
        }
        let mut args = vec![
            "diff".into(),
            "-U0".into(),
            "-M".into(),
            "HEAD".into(),
            "--".into(),
        ];
        args.extend(diff_paths);
        diffs.push(args);
        for args in diffs
            .iter()
            .map(|v| v.iter().map(|s| s.as_str()).collect::<Vec<_>>())
        {
            if let Ok(out) = std::process::Command::new("git")
                .args(&args)
                .current_dir(root)
                .output()
            {
                if out.status.success() {
                    inspected = true;
                    for line in String::from_utf8_lossy(&out.stdout).lines() {
                        if line.starts_with('+') && !line.starts_with("+++") {
                            added.push(line[1..].to_string());
                        }
                    }
                }
            }
        }
        // Successful Git inspection is authoritative, including deletion-only
        // diffs. Only new/untracked files or failed Git inspection fall back.
        if tracked && inspected {
            return added.join("\n");
        }
    }
    std::fs::read_to_string(root.join(rel_path)).unwrap_or_default()
}

pub fn evaluate_doc_gate(
    root: &Path,
    changed_paths_override: Option<Vec<String>>,
) -> DocGateReport {
    // The doc gate is a trust-bearing consumer (PR #206 V-3): its changed
    // set must be observed by the Git connector, never declared by a
    // repo-supplied manifest connector.
    use crate::workflow::connector::WorkflowConnector as _;
    let connector = crate::workflow::connector::GitWorkflowConnector;
    let changed_paths = changed_paths_override.unwrap_or_else(|| connector.changed_paths(root));

    let mut violations: Vec<DocViolation> = Vec::new();
    let checked_tiers: usize;
    let checked_rules: usize;

    // 3. Load docs/manifest.json; missing or invalid JSON fails the gate.
    let manifest = match load_manifest(root) {
        Ok(m) => m,
        Err(e) => {
            return DocGateReport {
                passed: false,
                connector_id: connector.connector_id().to_string(),
                checked_tiers: 0,
                checked_rules: 0,
                changed_paths,
                violations: vec![DocViolation {
                    rule_id: "manifest".to_string(),
                    kind: "missing_or_invalid_manifest".to_string(),
                    path: "docs/manifest.json".to_string(),
                    detail: format!("docs/manifest.json missing or invalid: {e}"),
                }],
            };
        }
    };

    // 4. Tier layout hygiene: only tier + support subdirectories and
    //    allowed_root_files may live directly under docs/.
    let docs_root = root.join("docs");
    let mut hygiene_failed = false;
    for entry in std::fs::read_dir(&docs_root).into_iter().flatten() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            let allowed = manifest.tiers.vision.dir.ends_with(&name)
                || manifest.tiers.features_api.dir.ends_with(&name)
                || manifest.tiers.extend.dir.ends_with(&name)
                || manifest.support_dirs.iter().any(|d| d.ends_with(&name));
            if !allowed {
                hygiene_failed = true;
                violations.push(DocViolation {
                    rule_id: "tier-layout".to_string(),
                    kind: "unclassified_docs_directory".to_string(),
                    path: path.to_string_lossy().to_string(),
                    detail: format!("unclassified docs directory '{}' at docs/ root", name),
                });
            }
        } else if path.is_file() {
            if !manifest.allowed_root_files.iter().any(|f| f == &name) {
                hygiene_failed = true;
                violations.push(DocViolation {
                    rule_id: "tier-layout".to_string(),
                    kind: "unclassified_root_file".to_string(),
                    path: path.to_string_lossy().to_string(),
                    detail: format!("unclassified docs file '{}' at docs/ root", name),
                });
            }
        }
    }

    // 5. Prohibited filename substrings (competitor naming) recursively across docs/.
    let mut all_doc_files = Vec::new();
    collect_files_recursive(&docs_root, &mut all_doc_files);
    for path in all_doc_files {
        let Some(fname) = path.file_name() else {
            continue;
        };
        let name = fname.to_string_lossy().to_lowercase();
        if let Some(sub) = manifest
            .prohibited_filename_substrings
            .iter()
            .find(|s| name.contains(&s.to_lowercase()))
        {
            hygiene_failed = true;
            violations.push(DocViolation {
                rule_id: "competitor-naming".to_string(),
                kind: "prohibited_filename".to_string(),
                path: path.to_string_lossy().to_string(),
                detail: format!("prohibited filename substring '{}'", sub),
            });
        }
    }

    // 5b. No maintainer machine paths in any tracked doc under docs/ (including
    // support_dirs such as docs/superpowers/ and docs/releases/).
    const MACHINE_PATH_NEEDLES: [&str; 2] = ["/home/kenton", "/tmp/opencode"];
    for path in collect_doc_markdown_files(root) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let found: Vec<&str> = MACHINE_PATH_NEEDLES
            .iter()
            .copied()
            .filter(|needle| content.contains(needle))
            .collect();
        if found.is_empty() {
            continue;
        }
        hygiene_failed = true;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        violations.push(DocViolation {
            rule_id: "machine-path".to_string(),
            kind: "maintainer_machine_path".to_string(),
            path: rel.clone(),
            detail: format!(
                "maintainer machine path(s) {:?} found in tracked doc '{}'",
                found, rel
            ),
        });
    }

    if hygiene_failed {
        return DocGateReport {
            passed: false,
            connector_id: connector.connector_id().to_string(),
            checked_tiers: 0,
            checked_rules: 0,
            changed_paths,
            violations,
        };
    }

    // 6. Check all 3 tiers.
    checked_tiers = 3;
    for (tier_name, tier_spec) in [
        ("vision", &manifest.tiers.vision),
        ("features_api", &manifest.tiers.features_api),
        ("extend", &manifest.tiers.extend),
    ] {
        let tier_dir = root.join(&tier_spec.dir);
        if !tier_dir.exists() {
            violations.push(DocViolation {
                rule_id: tier_name.to_string(),
                kind: "missing_required_tier_file".to_string(),
                path: tier_dir.to_string_lossy().to_string(),
                detail: format!("tier directory '{}' missing", tier_spec.dir),
            });
            continue;
        }
        for required_file in &tier_spec.required_files {
            let file_path = if required_file.starts_with("docs/") {
                root.join(required_file)
            } else {
                tier_dir.join(required_file)
            };
            let len = file_path.metadata().map(|m| m.len()).unwrap_or(0);
            if !file_path.exists() || len == 0 {
                violations.push(DocViolation {
                    rule_id: tier_name.to_string(),
                    kind: "missing_required_tier_file".to_string(),
                    path: file_path.to_string_lossy().to_string(),
                    detail: format!("required file '{}' missing or empty", required_file),
                });
            }
        }
    }

    // 7. Evaluate surface rules.
    checked_rules = manifest.surface_rules.len();
    for rule in &manifest.surface_rules {
        if !["vision", "features_api", "extend"].contains(&rule.tier.as_str()) {
            violations.push(DocViolation {
                rule_id: rule.id.clone(),
                kind: "tier_boundary_mismatch".to_string(),
                path: String::new(),
                detail: format!("invalid tier '{}' in surface rule {}", rule.tier, rule.id),
            });
            continue;
        }
        let expected_prefix = match rule.tier.as_str() {
            "vision" => manifest.tiers.vision.dir.as_str(),
            "features_api" => manifest.tiers.features_api.dir.as_str(),
            _ => manifest.tiers.extend.dir.as_str(),
        };
        if !rule.target_doc.starts_with(expected_prefix) {
            violations.push(DocViolation {
                rule_id: rule.id.clone(),
                kind: "tier_boundary_mismatch".to_string(),
                path: rule.target_doc.clone(),
                detail: format!(
                    "surface rule {} target_doc '{}' outside tier {}",
                    rule.id, rule.target_doc, rule.tier
                ),
            });
            continue;
        }
        let target_doc_path = if rule.target_doc.starts_with("docs/") {
            root.join(&rule.target_doc)
        } else {
            root.join("docs").join(&rule.target_doc)
        };
        let target_len = target_doc_path.metadata().map(|m| m.len()).unwrap_or(0);
        if !target_doc_path.exists() || target_len == 0 {
            violations.push(DocViolation {
                rule_id: rule.id.clone(),
                kind: "missing_target_doc".to_string(),
                path: target_doc_path.to_string_lossy().to_string(),
                detail: format!("target_doc '{}' missing or empty", rule.target_doc),
            });
            continue;
        }
        let content = std::fs::read_to_string(&target_doc_path).unwrap_or_default();
        for heading in &rule.required_headings {
            if !content.contains(heading) {
                violations.push(DocViolation {
                    rule_id: rule.id.clone(),
                    kind: "missing_required_heading".to_string(),
                    path: target_doc_path.to_string_lossy().to_string(),
                    detail: format!(
                        "required heading '{}' missing in {}",
                        heading, rule.target_doc
                    ),
                });
            }
        }
        for token in &rule.required_tokens {
            if !content.contains(token) {
                violations.push(DocViolation {
                    rule_id: rule.id.clone(),
                    kind: "missing_required_token".to_string(),
                    path: target_doc_path.to_string_lossy().to_string(),
                    detail: format!("required token '{}' missing in {}", token, rule.target_doc),
                });
            }
        }
        for cp in &changed_paths {
            let matches_watch = rule
                .watch_paths
                .iter()
                .any(|wp| cp == wp || cp.starts_with(wp));
            if matches_watch {
                let added = watched_added_lines(root, cp);
                for ident in extract_public_surface_identifiers(&added) {
                    if !content.contains(&ident) {
                        violations.push(DocViolation {
                            rule_id: rule.id.clone(),
                            kind: "undocumented_public_identifier".to_string(),
                            path: rule.target_doc.clone(),
                            detail: format!(
                                "added public identifier '{}' in '{}' is not documented in {}",
                                ident, cp, rule.target_doc
                            ),
                        });
                    }
                }
            }
        }
    }

    // 8. Surface-diff check: watched public-surface change without doc update.
    for rule in &manifest.surface_rules {
        if !rule.require_co_change_on_touch {
            continue;
        }
        let watched = changed_paths.iter().any(|cp| {
            rule.watch_paths
                .iter()
                .any(|wp| cp == wp || cp.starts_with(wp))
        });
        let doc_changed = changed_paths.iter().any(|cp| cp == &rule.target_doc);
        if watched && !doc_changed {
            violations.push(DocViolation {
                rule_id: rule.id.clone(),
                kind: "surface_changed_without_doc_update".to_string(),
                path: rule.target_doc.clone(),
                detail: format!(
                    "watched public surface changed without target_doc update: {}",
                    rule.target_doc
                ),
            });
        }
    }

    let passed = violations.is_empty();
    DocGateReport {
        passed,
        connector_id: connector.connector_id().to_string(),
        checked_tiers,
        checked_rules,
        changed_paths,
        violations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(docs_dir: &std::path::Path, surface_rules_json: &str) {
        let manifest = format!(
            r#"{{
            "schema_version": 1,
            "allowed_root_files": ["README.md", "manifest.json"],
            "tiers": {{
                "vision": {{
                    "dir": "docs/vision",
                    "description": "Vision tier",
                    "required_files": []
                }},
                "features_api": {{
                    "dir": "docs/features-api",
                    "description": "Features API tier",
                    "required_files": []
                }},
                "extend": {{
                    "dir": "docs/extend",
                    "description": "Extend tier",
                    "required_files": []
                }}
            }},
            "support_dirs": ["docs/releases", "docs/assets"],
            "prohibited_filename_substrings": [],
            "surface_rules": {}
        }}"#,
            surface_rules_json
        );
        std::fs::write(docs_dir.join("manifest.json"), manifest).unwrap();
    }

    fn write_manifest_support_dirs(docs_dir: &std::path::Path, support_dirs_json: &str) {
        let manifest = format!(
            r#"{{
            "schema_version": 1,
            "allowed_root_files": ["README.md", "manifest.json"],
            "tiers": {{
                "vision": {{
                    "dir": "docs/vision",
                    "description": "Vision tier",
                    "required_files": []
                }},
                "features_api": {{
                    "dir": "docs/features-api",
                    "description": "Features API tier",
                    "required_files": []
                }},
                "extend": {{
                    "dir": "docs/extend",
                    "description": "Extend tier",
                    "required_files": []
                }}
            }},
            "support_dirs": {},
            "prohibited_filename_substrings": [],
            "surface_rules": []
        }}"#,
            support_dirs_json
        );
        std::fs::write(docs_dir.join("manifest.json"), manifest).unwrap();
    }

    fn run_git(root: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git available in test env");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_git_repo(root: &Path) {
        run_git(root, &["init", "-q"]);
        run_git(root, &["config", "user.email", "worker@example.com"]);
        run_git(root, &["config", "user.name", "worker"]);
    }

    #[test]
    fn rejects_unclassified_root_markdown_file() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let docs_dir = tmp_dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("unclassified.md"), "# Bad File").unwrap();
        write_manifest(&docs_dir, "[]");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();

        let report = evaluate_doc_gate(tmp_dir.path(), None);
        assert!(!report.passed);
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].kind, "unclassified_root_file");
        assert_eq!(report.violations[0].rule_id, "tier-layout");
    }

    #[test]
    fn rejects_prohibited_competitor_filename() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let docs_dir = tmp_dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir.join("vision")).unwrap();
        let bad_token_a = concat!("cog", "nee");
        let bad_token_b = concat!("oxi", "rs");
        let bad_name = format!("design-{}", bad_token_a);
        std::fs::write(
            docs_dir.join("vision").join(format!("{}.md", bad_name)),
            "# Bad",
        )
        .unwrap();
        let manifest = serde_json::json!({
            "schema_version": 1,
            "allowed_root_files": ["README.md", "manifest.json"],
            "tiers": {
                "vision": { "dir": "docs/vision", "description": "Vision tier", "required_files": [] },
                "features_api": { "dir": "docs/features-api", "description": "Features API tier", "required_files": [] },
                "extend": { "dir": "docs/extend", "description": "Extend tier", "required_files": [] }
            },
            "support_dirs": ["docs/releases", "docs/assets"],
            "prohibited_filename_substrings": [bad_token_a, bad_token_b],
            "surface_rules": []
        })
        .to_string();
        std::fs::write(docs_dir.join("manifest.json"), manifest).unwrap();

        let report = evaluate_doc_gate(tmp_dir.path(), None);
        assert!(!report.passed);
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].kind, "prohibited_filename");
        assert_eq!(report.violations[0].rule_id, "competitor-naming");
    }

    #[test]
    fn rejects_missing_required_heading_or_token_in_mapped_doc() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let docs_dir = tmp_dir.path().join("docs");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();
        std::fs::create_dir_all(docs_dir.join("features-api")).unwrap();
        std::fs::create_dir_all(docs_dir.join("extend")).unwrap();
        std::fs::write(
            docs_dir.join("vision").join("target_doc.md"),
            "## Introduction\nMissing token here.",
        )
        .unwrap();
        let manifest = r###"{
            "schema_version": 1,
            "allowed_root_files": ["README.md", "manifest.json"],
            "tiers": {
                "vision": { "dir": "docs/vision", "description": "Vision tier", "required_files": [] },
                "features_api": { "dir": "docs/features-api", "description": "Features API tier", "required_files": [] },
                "extend": { "dir": "docs/extend", "description": "Extend tier", "required_files": [] }
            },
            "support_dirs": ["docs/releases", "docs/assets"],
            "prohibited_filename_substrings": [],
            "surface_rules": [
                {
                    "id": "test-rule-1",
                    "tier": "vision",
                    "watch_paths": [],
                    "target_doc": "docs/vision/target_doc.md",
                    "required_headings": ["## Introduction"],
                    "required_tokens": ["required-missing-token"],
                    "require_co_change_on_touch": false
                }
            ]
        }"###;
        std::fs::write(docs_dir.join("manifest.json"), manifest).unwrap();

        let report = evaluate_doc_gate(tmp_dir.path(), None);
        assert!(!report.passed);
        assert_eq!(report.violations.len(), 1);
        assert!(
            report.violations[0].kind == "missing_required_heading"
                || report.violations[0].kind == "missing_required_token"
        );
        assert_eq!(report.violations[0].rule_id, "test-rule-1");
    }

    #[test]
    fn internal_change_passes_without_doc_churn() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let docs_dir = tmp_dir.path().join("docs");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();
        std::fs::create_dir_all(docs_dir.join("features-api")).unwrap();
        std::fs::create_dir_all(docs_dir.join("extend")).unwrap();
        std::fs::write(
            docs_dir.join("vision").join("target_doc.md"),
            "## Introduction\nThe core target_doc and feature one.",
        )
        .unwrap();
        let manifest = r###"{
            "schema_version": 1,
            "allowed_root_files": ["README.md", "manifest.json"],
            "tiers": {
                "vision": { "dir": "docs/vision", "description": "Vision tier", "required_files": [] },
                "features_api": { "dir": "docs/features-api", "description": "Features API tier", "required_files": [] },
                "extend": { "dir": "docs/extend", "description": "Extend tier", "required_files": [] }
            },
            "support_dirs": ["docs/releases", "docs/assets"],
            "prohibited_filename_substrings": [],
            "surface_rules": [
                {
                    "id": "test-rule-1",
                    "tier": "vision",
                    "watch_paths": ["src/internal/module.rs"],
                    "target_doc": "docs/vision/target_doc.md",
                    "required_headings": ["## Introduction"],
                    "required_tokens": ["target_doc", "feature one"],
                    "require_co_change_on_touch": false
                }
            ]
        }"###;
        std::fs::write(docs_dir.join("manifest.json"), manifest).unwrap();

        let report = evaluate_doc_gate(
            tmp_dir.path(),
            Some(vec!["src/internal/module.rs".to_string()]),
        );
        assert!(report.passed);
        assert_eq!(report.violations.len(), 0);
    }

    #[test]
    fn surface_change_missing_co_change_or_token_blocks() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let docs_dir = tmp_dir.path().join("docs");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();
        std::fs::create_dir_all(docs_dir.join("features-api")).unwrap();
        std::fs::create_dir_all(docs_dir.join("extend")).unwrap();
        std::fs::write(
            docs_dir.join("vision").join("target_doc.md"),
            "## Introduction\nThe target_doc surface.",
        )
        .unwrap();
        let manifest = r###"{
            "schema_version": 1,
            "allowed_root_files": ["README.md", "manifest.json"],
            "tiers": {
                "vision": { "dir": "docs/vision", "description": "Vision tier", "required_files": [] },
                "features_api": { "dir": "docs/features-api", "description": "Features API tier", "required_files": [] },
                "extend": { "dir": "docs/extend", "description": "Extend tier", "required_files": [] }
            },
            "support_dirs": ["docs/releases", "docs/assets"],
            "prohibited_filename_substrings": [],
            "surface_rules": [
                {
                    "id": "test-rule-1",
                    "tier": "vision",
                    "watch_paths": ["src/server.rs"],
                    "target_doc": "docs/vision/target_doc.md",
                    "required_headings": ["## Introduction"],
                    "required_tokens": ["target_doc"],
                    "require_co_change_on_touch": true
                }
            ]
        }"###;
        std::fs::write(docs_dir.join("manifest.json"), manifest).unwrap();

        let report = evaluate_doc_gate(tmp_dir.path(), Some(vec!["src/server.rs".to_string()]));
        assert!(!report.passed);
        assert_eq!(report.violations.len(), 1);
        assert_eq!(
            report.violations[0].kind,
            "surface_changed_without_doc_update"
        );
    }

    #[test]
    fn rejects_undocumented_added_public_identifier_in_watched_source() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let docs_dir = tmp_dir.path().join("docs");
        let src_dir = tmp_dir.path().join("src");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();
        std::fs::create_dir_all(docs_dir.join("features-api")).unwrap();
        std::fs::create_dir_all(docs_dir.join("extend")).unwrap();
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            docs_dir.join("features-api").join("cli-reference.md"),
            "## Workflow, Documentation Gate & Ontology Commands\ndoc-gate workflow",
        )
        .unwrap();
        std::fs::write(
            src_dir.join("main.rs"),
            "#[command(name = \"undocumented-new-cmd\")]\nstruct Foo;",
        )
        .unwrap();
        let manifest = r###"{
            "schema_version": 1,
            "allowed_root_files": ["README.md", "manifest.json"],
            "tiers": {
                "vision": { "dir": "docs/vision", "description": "Vision tier", "required_files": [] },
                "features_api": { "dir": "docs/features-api", "description": "Features API tier", "required_files": [] },
                "extend": { "dir": "docs/extend", "description": "Extend tier", "required_files": [] }
            },
            "support_dirs": ["docs/releases", "docs/assets"],
            "prohibited_filename_substrings": [],
            "surface_rules": [
                {
                    "id": "cli-reference-surface",
                    "tier": "features_api",
                    "watch_paths": ["src/main.rs"],
                    "target_doc": "docs/features-api/cli-reference.md",
                    "required_headings": ["## Workflow, Documentation Gate & Ontology Commands"],
                    "required_tokens": ["doc-gate", "workflow"],
                    "require_co_change_on_touch": false
                }
            ]
        }"###;
        std::fs::write(docs_dir.join("manifest.json"), manifest).unwrap();

        let report = evaluate_doc_gate(tmp_dir.path(), Some(vec!["src/main.rs".to_string()]));
        assert!(!report.passed);
        assert!(report.violations.iter().any(|v| {
            v.kind == "undocumented_public_identifier" && v.detail.contains("undocumented-new-cmd")
        }));
    }

    #[test]
    fn committed_surface_change_without_doc_update_blocks_on_production_path() {
        // No changed_paths_override: the gate must see a real committed
        // changed set (baseline pinned at refs/remotes/origin/main).
        let tmp_dir = tempfile::tempdir().unwrap();
        let root = tmp_dir.path();
        let docs_dir = root.join("docs");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();
        std::fs::create_dir_all(docs_dir.join("features-api")).unwrap();
        std::fs::create_dir_all(docs_dir.join("extend")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            docs_dir.join("vision").join("target_doc.md"),
            "## Introduction\nThe target_doc surface.",
        )
        .unwrap();
        std::fs::write(root.join("src").join("server.rs"), "fn v1() {}\n").unwrap();
        let manifest = r###"{
            "schema_version": 1,
            "allowed_root_files": ["README.md", "manifest.json"],
            "tiers": {
                "vision": { "dir": "docs/vision", "description": "Vision tier", "required_files": [] },
                "features_api": { "dir": "docs/features-api", "description": "Features API tier", "required_files": [] },
                "extend": { "dir": "docs/extend", "description": "Extend tier", "required_files": [] }
            },
            "support_dirs": ["docs/releases", "docs/assets"],
            "prohibited_filename_substrings": [],
            "surface_rules": [
                {
                    "id": "test-rule-1",
                    "tier": "vision",
                    "watch_paths": ["src/server.rs"],
                    "target_doc": "docs/vision/target_doc.md",
                    "required_headings": ["## Introduction"],
                    "required_tokens": ["target_doc"],
                    "require_co_change_on_touch": true
                }
            ]
        }"###;
        std::fs::write(docs_dir.join("manifest.json"), manifest).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .expect("git available in test env");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "worker@example.com"]);
        run(&["config", "user.name", "worker"]);
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "baseline"]);
        run(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(root.join("src").join("server.rs"), "fn v2() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "surface change without doc update"]);

        let report = evaluate_doc_gate(root, None);
        assert!(
            report.changed_paths.iter().any(|p| p == "src/server.rs"),
            "production changed set must include the committed change, got {:?}",
            report.changed_paths
        );
        assert!(!report.passed);
        assert!(
            report.violations.iter().any(|v| {
                v.kind == "surface_changed_without_doc_update" && v.rule_id == "test-rule-1"
            }),
            "expected co-change violation, got {:?}",
            report.violations
        );
    }

    #[test]
    fn deletion_only_watched_change_does_not_scan_remaining_source() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let root = tmp_dir.path();
        let docs_dir = root.join("docs");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();
        std::fs::create_dir_all(docs_dir.join("features-api")).unwrap();
        std::fs::create_dir_all(docs_dir.join("extend")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            docs_dir.join("features-api").join("target.md"),
            "# Target\nNo public identifiers documented here.",
        )
        .unwrap();
        std::fs::write(
            root.join("src").join("main.rs"),
            "pub trait LegacyTrait {}\nfn remove_me() {}\n",
        )
        .unwrap();
        write_manifest(
            &docs_dir,
            r#"[{
                "id": "cli-surface",
                "tier": "features_api",
                "watch_paths": ["src/main.rs"],
                "target_doc": "docs/features-api/target.md",
                "required_headings": [],
                "required_tokens": [],
                "require_co_change_on_touch": false
            }]"#,
        );
        init_git_repo(root);
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "baseline"]);
        run_git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(
            root.join("src").join("main.rs"),
            "pub trait LegacyTrait {}\n",
        )
        .unwrap();
        run_git(root, &["add", "src/main.rs"]);
        run_git(root, &["commit", "-q", "-m", "deletion only"]);

        let report = evaluate_doc_gate(root, None);
        assert!(
            report.passed,
            "deletion-only diff produced violations: {:?}",
            report.violations
        );
    }

    #[test]
    fn pure_rename_does_not_treat_existing_source_as_added() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let root = tmp_dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src").join("old.rs"),
            "pub trait LegacyTrait {}\n",
        )
        .unwrap();
        init_git_repo(root);
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "baseline"]);
        run_git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        run_git(root, &["mv", "src/old.rs", "src/new.rs"]);
        run_git(root, &["commit", "-q", "-m", "pure rename"]);

        let added = watched_added_lines(root, "src/new.rs");
        assert!(
            added.is_empty(),
            "pure rename treated existing source as added: {added:?}"
        );
    }

    #[test]
    fn untracked_watched_file_still_scans_new_public_identifiers() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let root = tmp_dir.path();
        let docs_dir = root.join("docs");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();
        std::fs::create_dir_all(docs_dir.join("features-api")).unwrap();
        std::fs::create_dir_all(docs_dir.join("extend")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            docs_dir.join("features-api").join("target.md"),
            "# Target\nNo public identifiers documented here.",
        )
        .unwrap();
        write_manifest(
            &docs_dir,
            r#"[{
                "id": "cli-surface",
                "tier": "features_api",
                "watch_paths": ["src/main.rs"],
                "target_doc": "docs/features-api/target.md",
                "required_headings": [],
                "required_tokens": [],
                "require_co_change_on_touch": false
            }]"#,
        );
        std::fs::write(root.join("src").join("keep.rs"), "fn keep() {}\n").unwrap();
        init_git_repo(root);
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "baseline"]);
        run_git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(
            root.join("src").join("main.rs"),
            "pub trait UndocumentedNewTrait {}\n",
        )
        .unwrap();

        let report = evaluate_doc_gate(root, None);
        assert!(
            report.violations.iter().any(|v| {
                v.kind == "undocumented_public_identifier"
                    && v.detail.contains("UndocumentedNewTrait")
            }),
            "untracked file was not scanned: {:?}",
            report
        );
    }

    #[test]
    fn rejects_maintainer_machine_path_in_support_dir_doc() {
        // No .git in a temp dir: every markdown file under docs/ (including
        // support_dirs) must be scanned for maintainer machine paths.
        let tmp_dir = tempfile::tempdir().unwrap();
        let root = tmp_dir.path();
        let docs_dir = root.join("docs");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();
        std::fs::create_dir_all(docs_dir.join("features-api")).unwrap();
        std::fs::create_dir_all(docs_dir.join("extend")).unwrap();
        std::fs::create_dir_all(docs_dir.join("superpowers").join("plans")).unwrap();
        write_manifest_support_dirs(&docs_dir, r#"["docs/superpowers"]"#);
        std::fs::write(
            docs_dir.join("superpowers").join("plans").join("leak.md"),
            "See /tmp/opencode/semantic-eval.json for the strict receipt.",
        )
        .unwrap();

        let report = evaluate_doc_gate(root, None);
        assert!(!report.passed);
        assert!(
            report
                .violations
                .iter()
                .any(|v| { v.kind == "maintainer_machine_path" && v.path.contains("leak.md") }),
            "expected maintainer_machine_path violation, got {:?}",
            report.violations
        );
    }

    #[test]
    fn machine_path_rule_ignores_untracked_but_flags_tracked_doc() {
        // Inside a git worktree only tracked docs are scanned, so an
        // untracked scratch file under docs/superpowers/ never blocks the gate.
        let tmp_dir = tempfile::tempdir().unwrap();
        let root = tmp_dir.path();
        let docs_dir = root.join("docs");
        std::fs::create_dir_all(docs_dir.join("vision")).unwrap();
        std::fs::create_dir_all(docs_dir.join("features-api")).unwrap();
        std::fs::create_dir_all(docs_dir.join("extend")).unwrap();
        std::fs::create_dir_all(docs_dir.join("superpowers").join("plans")).unwrap();
        write_manifest_support_dirs(&docs_dir, r#"["docs/superpowers"]"#);
        init_git_repo(root);
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "baseline"]);

        let scratch = docs_dir
            .join("superpowers")
            .join("plans")
            .join("scratch.md");
        std::fs::write(&scratch, "run from /home/kenton/worktree/<id>").unwrap();
        let report = evaluate_doc_gate(root, None);
        assert!(
            report.passed,
            "untracked scratch doc must be ignored: {:?}",
            report.violations
        );

        run_git(root, &["add", "docs/superpowers/plans/scratch.md"]);
        let report = evaluate_doc_gate(root, None);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.kind == "maintainer_machine_path"),
            "tracked doc with machine path must be flagged: {:?}",
            report.violations
        );
    }

    #[test]
    fn doc_gate_passes_on_live_repository() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let report = evaluate_doc_gate(root, None);
        assert!(
            report.passed,
            "live repository failed doc-gate: {:?}",
            report.violations
        );
        assert_eq!(report.checked_tiers, 3);
        assert!(report.checked_rules >= 8);
    }
}
