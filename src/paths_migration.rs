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
//! author, and an absolute path in one is another machine's path), and any
//! path under no root this namespace knows (a lookup of such a path queries
//! it as given, so it is still found).
//!
//! After `--apply`, the registry row records `paths_migrated`, and file
//! lookups in the namespace query only the project-relative spelling.

use std::sync::Arc;

use crate::traits::{Embedder, VectorStore};

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
    /// Mesh rows (left as their author wrote them) that carry a path this
    /// root would resolve differently; while any remain, lookups keep
    /// querying every spelling.
    pub mesh_unmigrated: Vec<String>,
    /// Ingested rows that still carry an absolute location (ingested before
    /// paths were stored relative). This command does not touch them; a
    /// re-ingest does. While any remain, lookups keep querying every spelling.
    pub ingested_absolute: usize,
    /// `(id, path)`: absolute paths that name the root itself or a directory
    /// above it. They cannot be made relative, and a one-spelling lookup of a
    /// file below the root would no longer reach them, so they also keep the
    /// namespace from being marked; correct those memories by hand.
    pub root_ancestors: Vec<(String, String)>,
    pub applied: bool,
    /// Whether `--apply` recorded `paths_migrated` on the registry row.
    pub marked: bool,
}

/// Rewrite (with `apply`) or list (without) the stored file paths of every
/// agent-written memory in `namespace`. Refuses to run without a known root
/// (`project_root`, else a usable registered root): with none, an absolute
/// path would fall back to its own nearest `.git`, which can be another
/// repository's.
pub async fn migrate_paths(
    store: &Arc<dyn VectorStore>,
    emb: Option<&Arc<dyn Embedder>>,
    namespace: &str,
    project_root: Option<&str>,
    apply: bool,
) -> anyhow::Result<MigrationReport> {
    let registered = crate::registry::cached_project_root(store, namespace)
        .await?
        .filter(|root| crate::paths::is_usable_root(root));
    let known_root = project_root
        .map(str::to_string)
        .or(registered.clone())
        .filter(|root| crate::paths::is_usable_root(root));
    let Some(known_root) = known_root else {
        anyhow::bail!(
            "namespace '{namespace}' has no project root usable on this machine; \
             pass --project-root <the project's directory>"
        );
    };
    // Stored paths must be relative to the root lookups resolve against; a
    // root other than the registered one would rewrite them against a root
    // the lookups never use.
    if let Some(registered) = &registered {
        if !same_directory(registered, &known_root) {
            anyhow::bail!(
                "--project-root {known_root} is not the namespace's registered root \
                 {registered}; migrate against the registered root"
            );
        }
    }
    // The registry row records the migration; make sure it exists, rooted
    // where the paths are resolved, before any row is rewritten. A moved
    // registered root (unusable here) is replaced by the given one.
    if apply && registered.is_none() {
        crate::registry::cache_project_root(store, emb, namespace, &known_root).await;
        let recorded = crate::registry::cached_project_root(store, namespace).await?;
        if !recorded.is_some_and(|root| same_directory(&root, &known_root)) {
            anyhow::bail!(
                "could not record {known_root} as the registered root of '{namespace}'; \
                 nothing was changed"
            );
        }
    }

    let mut report = MigrationReport {
        applied: apply,
        ..MigrationReport::default()
    };
    for row in store.list(namespace, None, None).await? {
        if row.payload.user_id == "auto-ingestor"
            || row.id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR)
        {
            report.skipped_ingested += 1;
            // Only rows an ingest wrote (files and symbols, stamped with their
            // generation) are read by a lookup's AST half under their
            // location. The placeholder nodes the store creates for a
            // memory's `governs` targets carry no generation and do not count.
            if row.payload.metadata.get("index_generation").is_some()
                && crate::paths::is_absolute_any(&row.payload.location)
            {
                report.ingested_absolute += 1;
            }
            continue;
        }
        if crate::registry::is_registry_row(&row.payload.metadata) {
            continue;
        }
        if row.payload.metadata.get("mesh").is_some() {
            report.skipped_mesh += 1;
            // Left as its author wrote it; note whether it still needs
            // another spelling to be found.
            let mut resolved = row.payload.clone();
            crate::server::store_paths_in_payload(
                store,
                namespace,
                Some(&known_root),
                &mut resolved,
            )
            .await;
            // A governed folder's trailing slash only types the node a write
            // creates; lookups match `governs` with or without it, so a
            // slash-only difference needs no other spelling.
            if !path_changes(&without_governed_slash(&row.payload), &without_governed_slash(&resolved))
                .is_empty()
            {
                report.mesh_unmigrated.push(row.id.clone());
            }
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
        for path in unplaced {
            if path_contains_root(&path, &known_root) {
                report.root_ancestors.push((row.id.clone(), path.clone()));
            }
            report.unplaced.push((row.id.clone(), path));
        }
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
    // One spelling is enough only when no row still needs another.
    if apply
        && report.mesh_unmigrated.is_empty()
        && report.ingested_absolute == 0
        && report.root_ancestors.is_empty()
    {
        crate::registry::mark_paths_migrated(store, namespace).await?;
        report.marked = true;
    }
    Ok(report)
}

/// `payload` with each `governs` entry's trailing slash removed.
fn without_governed_slash(payload: &crate::traits::MemoryPayload) -> crate::traits::MemoryPayload {
    let mut bare = payload.clone();
    if let Some(governs) = bare.metadata.get_mut("governs").and_then(|g| g.as_array_mut()) {
        for g in governs.iter_mut() {
            if let Some(s) = g.as_str() {
                *g = serde_json::json!(s.strip_suffix('/').unwrap_or(s));
            }
        }
    }
    bare
}

/// Whether absolute `path` is `root` itself or a directory above it.
fn path_contains_root(path: &str, root: &str) -> bool {
    same_directory(path, root) || crate::paths::relative_to_on_disk(root, path).is_some()
}

/// Whether two directories are the same on this machine (canonical forms).
fn same_directory(a: &str, b: &str) -> bool {
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
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
        let src_dir = root.join("src").to_string_lossy().to_string();

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
            ("root-rule", payload("", json!({"governs": [root_s.clone()]}), "agent")),
            ("shared", payload(&absolute, json!({"mesh": {"update_id": "u1"}}), "agent")),
            // A mesh folder entry that differs by more than its slash.
            (
                "shared-folder",
                payload("", json!({"mesh": {"update_id": "u3"}, "governs": [src_dir.clone()]}), "agent"),
            ),
            (
                "ingested",
                payload(&absolute, json!({"index_generation": 3}), "auto-ingestor"),
            ),
        ];
        for (id, p) in rows {
            store.upsert(ns, id, vec![0.1; 384], p).await.unwrap();
        }
        // The namespace's registry row, as cache_project_root writes it.
        let mut registry = crate::registry::registry_metadata(&[]);
        registry["project_root"] = json!(root_s);
        let row = MemoryPayload {
            content: crate::registry::registry_content(ns),
            user_id: "system".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: registry,
        };
        store
            .upsert(ns, &crate::registry::registry_row_id(ns), vec![0.1; 384], row)
            .await
            .unwrap();

        // Without a root there is nothing safe to resolve against.
        let missing = base.join("moved-away").to_string_lossy().to_string();
        assert!(migrate_paths(&store, None, ns, Some(&missing), false).await.is_err());

        let dry = migrate_paths(&store, None, ns, Some(&root_s), false).await.unwrap();
        assert!(!dry.applied);
        let state = crate::registry::cached_registry_paths(&store, ns).await.unwrap();
        assert!(!state.paths_migrated, "a dry run records nothing");
        assert_eq!(dry.changed.len(), 1, "{dry:?}");
        assert_eq!(dry.changed[0].id, "legacy");
        assert_eq!(dry.skipped_mesh, 2);
        // The ingested row, plus the file nodes the store derives from locations.
        assert!(dry.skipped_ingested >= 1, "{dry:?}");
        assert_eq!(dry.unplaced.len(), 2, "{:?}", dry.unplaced);
        assert!(dry.unplaced.contains(&("legacy".to_string(), elsewhere.clone())));
        assert!(dry.unplaced.contains(&("root-rule".to_string(), root_s.clone())));
        let untouched = store.get(ns, "legacy").await.unwrap().unwrap().1;
        assert_eq!(untouched.location, absolute, "a dry run writes nothing");

        // A root other than the registered one is refused.
        let other = base.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let other_s = other.to_string_lossy().to_string();
        assert!(migrate_paths(&store, None, ns, Some(&other_s), true).await.is_err());

        let applied = migrate_paths(&store, None, ns, Some(&root_s), true).await.unwrap();
        assert_eq!(applied.changed.len(), 1);
        // The mesh row still carries an absolute path under the root, so one
        // spelling would not find it: the namespace is not marked yet.
        let mut mesh_unmigrated = applied.mesh_unmigrated.clone();
        mesh_unmigrated.sort();
        assert_eq!(
            mesh_unmigrated,
            vec!["shared".to_string(), "shared-folder".to_string()],
            "an absolute folder entry still needs another spelling"
        );
        // The ingested row was written with an absolute location; only a
        // re-ingest stores it relative.
        assert_eq!(applied.ingested_absolute, 1);
        // A rule on the root itself cannot be made relative.
        assert_eq!(applied.root_ancestors, vec![("root-rule".to_string(), root_s.clone())]);
        assert!(!applied.marked);
        let state = crate::registry::cached_registry_paths(&store, ns).await.unwrap();
        assert!(!state.paths_migrated, "not while a row needs another spelling");
        let migrated = store.get(ns, "legacy").await.unwrap().unwrap().1;
        assert_eq!(migrated.location, "src/lib.rs");
        assert_eq!(migrated.metadata["governs"], json!(["src/lib.rs", elsewhere]));
        assert_eq!(migrated.metadata["refs"][0]["file"], json!("src/lib.rs"));
        let shared = store.get(ns, "shared").await.unwrap().unwrap().1;
        assert_eq!(shared.location, absolute, "a mesh row keeps its author's text");

        // Its author migrates and the mesh delivers the relative spelling.
        store
            .upsert(
                ns,
                "shared",
                vec![0.1; 384],
                // Its folder entry has no slash; that alone needs no spelling.
                payload("src/lib.rs", json!({"mesh": {"update_id": "u2"}, "governs": ["src"]}), "agent"),
            )
            .await
            .unwrap();
        store
            .upsert(
                ns,
                "shared-folder",
                vec![0.1; 384],
                payload("", json!({"mesh": {"update_id": "u4"}, "governs": ["src"]}), "agent"),
            )
            .await
            .unwrap();
        // The root rule is corrected by hand.
        store
            .upsert(ns, "root-rule", vec![0.1; 384], payload("", json!({"governs": ["src/"]}), "agent"))
            .await
            .unwrap();
        // And the project is re-ingested.
        store
            .upsert(
                ns,
                "ingested",
                vec![0.1; 384],
                payload("src/lib.rs", json!({"index_generation": 4}), "auto-ingestor"),
            )
            .await
            .unwrap();
        let again = migrate_paths(&store, None, ns, Some(&root_s), true).await.unwrap();
        assert!(again.changed.is_empty(), "a second run finds nothing to change");
        assert!(again.marked, "{again:?}");
        let state = crate::registry::cached_registry_paths(&store, ns).await.unwrap();
        assert!(state.paths_migrated, "--apply records the migration");
    }
}
