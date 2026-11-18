//! One-time rewrite of the file paths stored on memories (`migrate-paths`).
//!
//! New writes store a memory's `location`, `refs[].file` and `governs` in the
//! project-relative form lookups use (see `server::store_paths_in_payload`).
//! Memories written before that may still carry absolute paths, which a
//! lookup finds only by querying that spelling too. This module puts those
//! rows through the same resolver once, so lookups can use one spelling.
//!
//! Left as they are: ingested nodes (`auto-ingestor` rows and qualified node
//! ids), rows received through the SynapticMesh (they belong to their
//! author), and any path under no root this namespace knows.

use std::sync::Arc;

use crate::traits::VectorStore;

/// One row the migration changed, or would change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowChange {
    pub id: String,
    /// `(field, old, new)` for every value that changed.
    pub changes: Vec<(String, String, String)>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub scanned: usize,
    pub skipped_ingested: usize,
    pub skipped_mesh: usize,
    pub changed: Vec<RowChange>,
    /// `(id, path)`: absolute paths left as they are, under no known root.
    pub unplaced: Vec<(String, String)>,
    pub applied: bool,
}

/// Rewrite (with `apply`) or list (without) the stored file paths of every
/// agent-written memory in `namespace`. Refuses to run without a known root
/// (`project_root`, else a usable registered root): with none, an absolute
/// path would fall back to its own nearest `.git`, which can be another
/// repository's.
pub async fn migrate_paths(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
    project_root: Option<&str>,
    apply: bool,
) -> anyhow::Result<MigrationReport> {
    let registered = crate::registry::cached_project_root(store, namespace).await?;
    let known_root = project_root
        .map(str::to_string)
        .or(registered)
        .filter(|root| crate::paths::is_usable_root(root));
    let Some(known_root) = known_root else {
        anyhow::bail!(
            "namespace '{namespace}' has no project root usable on this machine; \
             pass --project-root <the project's directory>"
        );
    };

    let mut report = MigrationReport {
        applied: apply,
        ..MigrationReport::default()
    };
    for row in store.list(namespace, None, None).await? {
        if row.payload.user_id == "auto-ingestor"
            || row.id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR)
        {
            report.skipped_ingested += 1;
            continue;
        }
        if row.payload.metadata.get("mesh").is_some() {
            report.skipped_mesh += 1;
            continue;
        }
        report.scanned += 1;
        let Some((vector, payload)) = store.get(namespace, &row.id).await? else {
            continue;
        };
        let mut migrated = payload.clone();
        let unplaced = crate::server::store_paths_in_payload(
            store,
            namespace,
            Some(&known_root),
            &mut migrated,
        )
        .await;
        report
            .unplaced
            .extend(unplaced.into_iter().map(|path| (row.id.clone(), path)));
        let changes = path_changes(&payload, &migrated);
        if changes.is_empty() {
            continue;
        }
        if apply {
            store.upsert(namespace, &row.id, vector, migrated).await?;
        }
        report.changed.push(RowChange {
            id: row.id.clone(),
            changes,
        });
    }
    Ok(report)
}

/// The path values that differ between `before` and `after`.
fn path_changes(
    before: &crate::traits::MemoryPayload,
    after: &crate::traits::MemoryPayload,
) -> Vec<(String, String, String)> {
    let mut changes = Vec::new();
    if before.location != after.location {
        changes.push((
            "location".to_string(),
            before.location.clone(),
            after.location.clone(),
        ));
    }
    let files = |p: &crate::traits::MemoryPayload| -> Vec<String> {
        p.metadata
            .get("refs")
            .and_then(|r| r.as_array())
            .map(|refs| {
                refs.iter()
                    .map(|r| r.get("file").and_then(|f| f.as_str()).unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    for (old, new) in files(before).into_iter().zip(files(after)) {
        if old != new {
            changes.push(("refs.file".to_string(), old, new));
        }
    }
    let governs = |p: &crate::traits::MemoryPayload| p.metadata.get("governs").cloned();
    if governs(before) != governs(after) {
        let show = |v: Option<serde_json::Value>| v.map(|v| v.to_string()).unwrap_or_default();
        changes.push((
            "governs".to_string(),
            show(governs(before)),
            show(governs(after)),
        ));
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LadybugStore;
    use crate::traits::MemoryPayload;
    use serde_json::json;

    fn payload(location: &str, metadata: serde_json::Value, user: &str) -> MemoryPayload {
        MemoryPayload {
            content: "rule".to_string(),
            user_id: user.to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: location.to_string(),
            location_lines: String::new(),
            metadata,
        }
    }

    #[tokio::test]
    async fn absolute_paths_are_listed_then_rewritten_and_others_are_left() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn VectorStore> =
            Arc::new(LadybugStore::new(dir.path().join("db"), 384).expect("store"));
        let ns = "MigrateNs";
        store.init(ns).await.unwrap();
        let base = dunce::canonicalize(dir.path()).unwrap();
        let root = base.join("example-project");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("lib.rs"), "x").unwrap();
        let root_s = root.to_string_lossy().to_string();
        let absolute = root.join("src").join("lib.rs").to_string_lossy().to_string();
        let elsewhere = base.join("elsewhere.rs").to_string_lossy().to_string();

        let rows = [
            (
                "legacy",
                payload(
                    &absolute,
                    json!({"governs": [absolute, elsewhere], "refs": [{"file": absolute}]}),
                    "agent",
                ),
            ),
            ("current", payload("src/lib.rs", json!({"governs": ["src/lib.rs"]}), "agent")),
            ("shared", payload(&absolute, json!({"mesh": {"update_id": "u1"}}), "agent")),
            ("ingested", payload(&absolute, json!({}), "auto-ingestor")),
        ];
        for (id, p) in rows {
            store.upsert(ns, id, vec![0.1; 384], p).await.unwrap();
        }

        // Without a root there is nothing safe to resolve against.
        let missing = base.join("moved-away").to_string_lossy().to_string();
        assert!(migrate_paths(&store, ns, Some(&missing), false).await.is_err());

        let dry = migrate_paths(&store, ns, Some(&root_s), false).await.unwrap();
        assert!(!dry.applied);
        assert_eq!(dry.changed.len(), 1, "{dry:?}");
        assert_eq!(dry.changed[0].id, "legacy");
        assert_eq!(dry.skipped_mesh, 1);
        // The ingested row, plus the file nodes the store derives from locations.
        assert!(dry.skipped_ingested >= 1, "{dry:?}");
        assert_eq!(dry.unplaced, vec![("legacy".to_string(), elsewhere.clone())]);
        let untouched = store.get(ns, "legacy").await.unwrap().unwrap().1;
        assert_eq!(untouched.location, absolute, "a dry run writes nothing");

        let applied = migrate_paths(&store, ns, Some(&root_s), true).await.unwrap();
        assert_eq!(applied.changed.len(), 1);
        let migrated = store.get(ns, "legacy").await.unwrap().unwrap().1;
        assert_eq!(migrated.location, "src/lib.rs");
        assert_eq!(migrated.metadata["governs"], json!(["src/lib.rs", elsewhere]));
        assert_eq!(migrated.metadata["refs"][0]["file"], json!("src/lib.rs"));
        let shared = store.get(ns, "shared").await.unwrap().unwrap().1;
        assert_eq!(shared.location, absolute, "a mesh row keeps its author's text");

        let again = migrate_paths(&store, ns, Some(&root_s), true).await.unwrap();
        assert!(again.changed.is_empty(), "a second run finds nothing to change");
    }
}
