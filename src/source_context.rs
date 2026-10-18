use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Once;

/// A legacy, process-provided project root scoped to one source identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopedAnchorOverride {
    pub(crate) source_id: String,
    pub(crate) project_root: PathBuf,
}

/// Validate the paired legacy environment values and scope.
pub(crate) fn scoped_anchor_override(
    anchor_root: Option<&Path>,
    source_id: Option<&str>,
) -> Option<ScopedAnchorOverride> {
    let root = anchor_root.filter(|root| !root.as_os_str().is_empty())?;
    if !root.is_absolute() {
        return None;
    }
    let Some(source_id) = source_id.filter(|id| !id.trim().is_empty()) else {
        static WARN_UNSCOPED_ANCHOR: Once = Once::new();
        WARN_UNSCOPED_ANCHOR.call_once(|| {
            eprintln!(
                "WARNING: NEUROSTRATA_ANCHOR_ROOT without NEUROSTRATA_SOURCE_ID is deprecated and ignored"
            );
        });
        return None;
    };

    if !is_valid_marker_directory(&root.join(".NeuroStrata")) {
        return None;
    }

    Some(ScopedAnchorOverride {
        source_id: source_id.to_string(),
        project_root: root.to_path_buf(),
    })
}

/// Resolve one node's location without changing its stored metadata.
pub(crate) fn export_absolute_path(
    location: &str,
    stored_absolute_path: &str,
    metadata: &serde_json::Value,
    project_contexts: &HashMap<String, PathBuf>,
    anchor_override: Option<&ScopedAnchorOverride>,
) -> String {
    if location.is_empty() {
        return stored_absolute_path.to_string();
    }
    let location_path = Path::new(location);
    if is_absolute_location(location, location_path) {
        return location.to_string();
    }

    let source_ids: BTreeSet<&str> = metadata
        .get("refs")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|reference| {
            reference.get("file").and_then(serde_json::Value::as_str) == Some(location)
        })
        .filter_map(|reference| {
            reference
                .get("source_context")
                .and_then(|context| context.get("source_id"))
                .and_then(serde_json::Value::as_str)
        })
        .filter(|source_id| !source_id.trim().is_empty())
        .collect();
    if source_ids.len() != 1 {
        return stored_absolute_path.to_string();
    }
    let source_id = *source_ids.iter().next().expect("one distinct source ID");

    let project_root = anchor_override
        .filter(|anchor| anchor.source_id == source_id)
        .map(|anchor| anchor.project_root.as_path())
        .or_else(|| {
            let marker = project_contexts.get(source_id)?;
            if !is_valid_marker_directory(marker) {
                return None;
            }
            marker.parent()
        });
    let Some(project_root) = project_root else {
        return stored_absolute_path.to_string();
    };

    let Some(normalized_location) = normalize_relative(location_path) else {
        return stored_absolute_path.to_string();
    };
    project_root
        .join(normalized_location)
        .to_string_lossy()
        .into_owned()
}

/// Select the destination for a graph export without creating any directories.
pub(crate) fn resolve_graph_output_path(
    out_path: Option<&Path>,
    source_id: Option<&str>,
    contexts: &HashMap<String, PathBuf>,
    project_root_env: Option<&Path>,
    invocation_dir: &Path,
) -> anyhow::Result<PathBuf> {
    if let Some(source_id) = source_id {
        let marker = contexts.get(source_id).ok_or_else(|| {
            anyhow::anyhow!("unknown source ID '{source_id}'; bind it with project-context bind")
        })?;
        if !is_valid_marker_directory(marker) {
            anyhow::bail!("source ID '{source_id}' is bound to an invalid .NeuroStrata marker");
        }
        return Ok(out_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| marker.join("graph/graph.json")));
    }

    if let Some(out_path) = out_path {
        return Ok(out_path.to_path_buf());
    }

    if let Some(project_root) = project_root_env {
        let marker = project_root.join(".NeuroStrata");
        if !is_valid_marker_directory(&marker) {
            anyhow::bail!(
                "NEUROSTRATA_PROJECT_ROOT does not contain a valid .NeuroStrata marker: {}",
                project_root.display()
            );
        }
        return Ok(marker.join("graph/graph.json"));
    }

    for ancestor in invocation_dir.ancestors() {
        let marker = ancestor.join(".NeuroStrata");
        if is_valid_marker_directory(&marker) {
            return Ok(marker.join("graph/graph.json"));
        }
    }

    anyhow::bail!("no .NeuroStrata marker found; pass --source-id or an explicit output path")
}

fn is_valid_marker_directory(marker: &Path) -> bool {
    if !marker.is_absolute()
        || marker.file_name().and_then(|name| name.to_str()) != Some(".NeuroStrata")
    {
        return false;
    }
    let Ok(metadata) = std::fs::metadata(marker) else {
        return false;
    };
    if !metadata.is_dir() {
        return false;
    }
    std::fs::canonicalize(marker)
        .map(|canonical| canonical.as_path() == marker)
        .unwrap_or(false)
}

fn is_absolute_location(location: &str, path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    let bytes = location.as_bytes();
    let windows_drive_absolute = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    let windows_unc_absolute = bytes.starts_with(b"\\\\");
    windows_drive_absolute || windows_unc_absolute
}

fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                normalized.push(part);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => {
                normalized.pop();
                depth -= 1;
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn project(root: &Path) -> PathBuf {
        let marker = root.join(".NeuroStrata");
        fs::create_dir_all(&marker).unwrap();
        fs::canonicalize(marker).unwrap()
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "neurostrata-source-context-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    #[test]
    fn resolve_graph_output_path_explicit_output_path_overrides_destination_only() {
        let invocation = temp_root("explicit-output");
        let output = invocation.join("exports/graph.json");

        assert_eq!(
            resolve_graph_output_path(Some(&output), None, &HashMap::new(), None, &invocation)
                .unwrap(),
            output
        );

        let error = resolve_graph_output_path(
            Some(&output),
            Some("unknown"),
            &HashMap::new(),
            None,
            &invocation,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown source ID"));
    }

    #[test]
    fn resolve_graph_output_path_source_id_selects_registered_marker_for_default_output() {
        let root = temp_root("source-output");
        let marker = project(&root.join("selected"));
        let contexts = HashMap::from([("source-a".to_string(), marker.clone())]);

        assert_eq!(
            resolve_graph_output_path(None, Some("source-a"), &contexts, None, &root).unwrap(),
            marker.join("graph/graph.json")
        );

        let error =
            resolve_graph_output_path(None, Some("unknown"), &contexts, None, &root).unwrap_err();
        assert!(error.to_string().contains("unknown source ID"));
    }

    #[test]
    fn resolve_graph_output_path_project_root_environment_selects_its_existing_marker() {
        let configured = temp_root("project-root-env-configured");
        let marker = project(&configured);
        let invocation = temp_root("project-root-env-invocation");
        project(&invocation);

        assert_eq!(
            resolve_graph_output_path(None, None, &HashMap::new(), Some(&configured), &invocation)
                .unwrap(),
            marker.join("graph/graph.json")
        );
    }

    #[test]
    fn resolve_graph_output_path_invalid_project_root_environment_fails_without_cwd_fallback() {
        let configured = temp_root("invalid-project-root-env");
        let cwd_root = temp_root("invalid-project-root-cwd");
        let cwd_marker = project(&cwd_root);
        let error =
            resolve_graph_output_path(None, None, &HashMap::new(), Some(&configured), &cwd_marker)
                .unwrap_err();

        assert!(error.to_string().contains("NEUROSTRATA_PROJECT_ROOT"));
        assert!(!cwd_marker.join("graph").exists());
    }

    #[test]
    fn resolve_graph_output_path_discovers_nearest_parent_marker_from_invocation_directory() {
        let root = temp_root("nearest-marker");
        project(&root);
        let nearest_marker = project(&root.join("nested"));
        let invocation = root.join("nested/work");
        fs::create_dir_all(&invocation).unwrap();

        assert_eq!(
            resolve_graph_output_path(None, None, &HashMap::new(), None, &invocation).unwrap(),
            nearest_marker.join("graph/graph.json")
        );
    }

    #[test]
    fn resolve_graph_output_path_source_id_output_is_independent_of_invocation_directory() {
        let root = temp_root("independent-source-output");
        let marker = project(&root.join("selected"));
        let unrelated = temp_root("unrelated-invocation");
        let contexts = HashMap::from([("source-a".to_string(), marker.clone())]);

        let canonical =
            resolve_graph_output_path(None, Some("source-a"), &contexts, None, &root).unwrap();
        let from_unrelated =
            resolve_graph_output_path(None, Some("source-a"), &contexts, None, &unrelated).unwrap();

        assert_eq!(canonical, marker.join("graph/graph.json"));
        assert_eq!(from_unrelated, canonical);
    }

    #[test]
    fn resolve_graph_output_path_missing_context_never_falls_back_to_cwd_output_tree() {
        let invocation = temp_root("missing-output-context");
        let error =
            resolve_graph_output_path(None, None, &HashMap::new(), None, &invocation).unwrap_err();

        assert!(error.to_string().contains("--source-id"));
        assert!(error.to_string().contains("explicit output path"));
        assert!(!invocation.join(".NeuroStrata").exists());
    }

    #[test]
    fn resolve_graph_output_path_checks_explicit_source_id_with_output_path() {
        let invocation = temp_root("invalid-source-with-output");
        let output = invocation.join("graph.json");
        let error = resolve_graph_output_path(
            Some(&output),
            Some("unknown"),
            &HashMap::new(),
            None,
            &invocation,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown source ID"));
    }

    fn relative_to_current_dir(path: &Path) -> PathBuf {
        let current = fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let target = fs::canonicalize(path).unwrap();
        let current_components: Vec<_> = current.components().collect();
        let target_components: Vec<_> = target.components().collect();
        let shared = current_components
            .iter()
            .zip(&target_components)
            .take_while(|(left, right)| left == right)
            .count();

        let mut relative = PathBuf::new();
        for component in &current_components[shared..] {
            if matches!(component, Component::Normal(_)) {
                relative.push("..");
            }
        }
        for component in &target_components[shared..] {
            if let Component::Normal(part) = component {
                relative.push(part);
            }
        }
        relative
    }

    fn refs(location: &str, source_ids: &[&str]) -> serde_json::Value {
        json!({
            "refs": source_ids.iter().map(|source_id| json!({
                "file": location,
                "source_context": { "source_id": source_id },
                "provider": { "name": "opaque-provider" }
            })).collect::<Vec<_>>()
        })
    }

    #[test]
    fn projects_each_source_under_its_own_marker_parents() {
        let temp = temp_root("projects");
        let root_a = temp.join("project-a");
        let root_b = temp.join("project-b");
        let marker_a = project(&root_a);
        let marker_b = project(&root_b);
        let root_b = marker_b.parent().unwrap().to_path_buf();
        let contexts = HashMap::from([
            ("source-a".to_string(), marker_a),
            ("source-b".to_string(), marker_b),
        ]);

        assert_eq!(
            export_absolute_path(
                "src/lib.rs",
                "old-a",
                &refs("src/lib.rs", &["source-a"]),
                &contexts,
                None
            ),
            root_a.join("src/lib.rs").to_string_lossy()
        );
        assert_eq!(
            export_absolute_path(
                "src/lib.rs",
                "old-b",
                &refs("src/lib.rs", &["source-b"]),
                &contexts,
                None
            ),
            root_b.join("src/lib.rs").to_string_lossy()
        );
    }

    #[test]
    fn preserves_stored_path_for_missing_unknown_or_conflicting_source_id() {
        let temp = temp_root("fallback");
        let marker = project(&temp.join("known"));
        let contexts = HashMap::from([("known".to_string(), marker)]);

        for metadata in [
            json!({}),
            refs("src/lib.rs", &["unknown"]),
            refs("src/lib.rs", &["known", "unknown"]),
        ] {
            assert_eq!(
                export_absolute_path("src/lib.rs", "/stored/lib.rs", &metadata, &contexts, None),
                "/stored/lib.rs"
            );
        }

        let known = refs("src/lib.rs", &["known"]);
        fs::remove_dir_all(contexts.get("known").unwrap()).unwrap();
        assert_eq!(
            export_absolute_path("src/lib.rs", "/stored/lib.rs", &known, &contexts, None),
            "/stored/lib.rs",
            "a removed marker must not keep resolving"
        );
        assert_eq!(
            export_absolute_path("", "/stored/empty", &known, &contexts, None),
            "/stored/empty",
            "an empty location must not resolve to the project root"
        );
    }

    #[test]
    fn preserves_stored_path_for_relative_registry_marker() {
        let temp = temp_root("relative-registry");
        let marker = project(&temp.join("project"));
        let relative_marker = relative_to_current_dir(&marker);
        assert!(!relative_marker.is_absolute());
        let contexts = HashMap::from([("source".to_string(), relative_marker)]);

        assert_eq!(
            export_absolute_path(
                "src/lib.rs",
                "/stored/lib.rs",
                &refs("src/lib.rs", &["source"]),
                &contexts,
                None
            ),
            "/stored/lib.rs"
        );
    }

    #[test]
    fn preserves_stored_path_for_noncanonical_registry_marker() {
        let temp = temp_root("noncanonical-registry");
        let project_root = temp.join("project");
        let marker = project(&project_root);
        fs::create_dir_all(project_root.join("nested")).unwrap();
        let noncanonical = project_root.join("nested/../.NeuroStrata");
        assert!(noncanonical.is_dir());
        assert_ne!(fs::canonicalize(&noncanonical).unwrap(), noncanonical);
        let contexts = HashMap::from([("source".to_string(), noncanonical)]);

        assert_eq!(
            export_absolute_path(
                "src/lib.rs",
                "/stored/lib.rs",
                &refs("src/lib.rs", &["source"]),
                &contexts,
                None
            ),
            "/stored/lib.rs"
        );
        assert!(marker.is_dir());
    }

    #[test]
    fn preserves_stored_path_for_registry_directory_without_marker_name() {
        let temp = temp_root("wrong-registry-name");
        let wrong_marker = temp.join(".NeuroStrata-copy");
        fs::create_dir_all(&wrong_marker).unwrap();
        let contexts = HashMap::from([("source".to_string(), wrong_marker)]);

        assert_eq!(
            export_absolute_path(
                "src/lib.rs",
                "/stored/lib.rs",
                &refs("src/lib.rs", &["source"]),
                &contexts,
                None
            ),
            "/stored/lib.rs"
        );
    }

    #[test]
    fn matches_refs_by_exact_location_and_ignores_empty_source_ids() {
        let temp = temp_root("exact-refs");
        let root = temp.join("project");
        let marker = project(&root);
        let contexts = HashMap::from([("source".to_string(), marker)]);
        let metadata = json!({ "refs": [
            { "file": "src/lib.rs", "source_context": { "source_id": "" } },
            { "file": "src/lib.rs", "source_context": { "source_id": "source" } },
            { "file": "src/other.rs", "source_context": { "source_id": "other" } }
        ]});

        assert_eq!(
            export_absolute_path("src/lib.rs", "old", &metadata, &contexts, None),
            root.join("src/lib.rs").to_string_lossy()
        );
    }

    #[test]
    fn preserves_absolute_locations_and_provider_fields() {
        let metadata = json!({ "refs": [{
            "file": "/legacy/absolute.rs",
            "source_context": { "source_id": "source", "provider": "external" },
            "provider": { "opaque": [1, 2, 3] }
        }]});
        let original = metadata.clone();

        assert_eq!(
            export_absolute_path(
                "/legacy/absolute.rs",
                "stored",
                &metadata,
                &HashMap::new(),
                None
            ),
            "/legacy/absolute.rs"
        );
        assert_eq!(metadata, original);
        assert_eq!(metadata["refs"][0]["provider"]["opaque"], json!([1, 2, 3]));
        assert_eq!(
            export_absolute_path(
                r"C:\legacy\absolute.rs",
                "stored",
                &serde_json::Value::Null,
                &HashMap::new(),
                None
            ),
            r"C:\legacy\absolute.rs"
        );
    }

    #[test]
    fn rejects_relative_locations_that_escape_the_source_root() {
        let temp = temp_root("escape");
        let root = temp.join("project");
        let marker = project(&root);
        let contexts = HashMap::from([("source".to_string(), marker)]);
        let metadata = refs("src/../lib.rs", &["source"]);

        assert_eq!(
            export_absolute_path("src/../lib.rs", "old", &metadata, &contexts, None),
            root.join("lib.rs").to_string_lossy()
        );
        assert_eq!(
            export_absolute_path(
                "./src/../lib.rs",
                "old",
                &refs("./src/../lib.rs", &["source"]),
                &contexts,
                None
            ),
            root.join("lib.rs").to_string_lossy()
        );
        assert_eq!(
            export_absolute_path(
                "../../outside.rs",
                "/stored/outside.rs",
                &refs("../../outside.rs", &["source"]),
                &contexts,
                None,
            ),
            "/stored/outside.rs"
        );
    }

    #[test]
    fn scoped_environment_override_only_rebases_its_matching_source() {
        let temp = temp_root("override");
        let root_a = temp.join("project-a");
        let root_b = temp.join("project-b");
        let marker_a = project(&root_a);
        let marker_b = project(&root_b);
        let root_b = marker_b.parent().unwrap().to_path_buf();
        let contexts = HashMap::from([
            ("source-a".to_string(), marker_a),
            ("source-b".to_string(), marker_b),
        ]);
        let override_root = temp.join("override");
        project(&override_root);
        let anchor_override =
            scoped_anchor_override(Some(&override_root), Some("source-a")).unwrap();

        assert_eq!(
            export_absolute_path(
                "src/lib.rs",
                "old-a",
                &refs("src/lib.rs", &["source-a"]),
                &contexts,
                Some(&anchor_override)
            ),
            override_root.join("src/lib.rs").to_string_lossy()
        );
        assert_eq!(
            export_absolute_path(
                "src/lib.rs",
                "old-b",
                &refs("src/lib.rs", &["source-b"]),
                &contexts,
                Some(&anchor_override)
            ),
            root_b.join("src/lib.rs").to_string_lossy()
        );
    }

    #[test]
    fn unscoped_environment_override_is_ignored() {
        let temp = temp_root("unscoped");
        let root = temp.join("project");
        project(&root);

        assert!(scoped_anchor_override(Some(&root), None).is_none());
        assert!(scoped_anchor_override(Some(&root), Some("  ")).is_none());
    }

    #[test]
    fn scoped_environment_override_rejects_relative_roots() {
        let temp = temp_root("relative-override");
        let absolute_root = temp.join("project");
        project(&absolute_root);
        let relative_root = relative_to_current_dir(&absolute_root);
        assert!(!relative_root.is_absolute());

        assert!(scoped_anchor_override(Some(&relative_root), Some("source")).is_none());
    }
}
