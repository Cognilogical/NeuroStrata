//! Archive one memory by marking it with a tombstone.
//!
//! Archived, not destroyed: the row keeps its content, its id and everything
//! else its metadata already said, and `archived: true` is *merged* in beside
//! it. Replacing the whole metadata object would silently strip the
//! provenance, the access count and the structural pointers of a row at the
//! exact moment someone stops trusting it -- the opposite of what an audit
//! trail should do.
//!
//! Scope in 1.8.0: **metadata-only, no retrieval change.** Writing the
//! tombstone is the whole of it; no read path filters on `archived` yet, so an
//! archived row still surfaces in `search_memory` exactly as before. The
//! filter is a follow-up task.

use crate::events::{RecursionToken, ThalamicBus, ThalamicPulse};
use crate::traits::{MemoryPayload, VectorStore};
use serde_json::Value;
use std::sync::Arc;

/// What an archive attempt actually did.
///
/// A refusal is a distinct value rather than a sentence, because a mutating
/// endpoint that answers `200` for a row it did not archive is read as success
/// by `curl -f` and `reqwest::error_for_status()` alike. Every caller that
/// wants a status code picks it from the variant; every caller that wants
/// prose gets it from [`Display`], which says the same thing it always said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveOutcome {
    /// The row was tombstoned and `Archived` was emitted for it.
    Archived { namespace: String, id: String },
    /// No such row. Not an error in the daemon's sense: the request named
    /// something that is not there, which is a 404 and not a 500.
    NotFound { namespace: String, id: String },
    /// The request itself could not be acted on: no id, no namespace, or a
    /// row whose legacy metadata cannot carry a tombstone. A 400.
    Invalid(String),
    /// The store refused or failed: a read error or a failed write. A 500.
    Failed(String),
}

impl ArchiveOutcome {
    /// True only for the variant that means the row really was tombstoned.
    pub fn is_archived(&self) -> bool {
        matches!(self, ArchiveOutcome::Archived { .. })
    }
}

/// Read-time answer to "was this row tombstoned?". The producer
/// ([`archive_memory`]) writes `metadata.archived: true`; every read surface
/// asks this one question of each row before letting it through.
///
/// Strict: a row is archived iff `metadata.archived` is the literal JSON
/// `true`. Anything else -- absent, `false`, a string, a number, a wrong
/// type, malformed metadata -- reads as not archived. A typo or a wrong-typed
/// sentinel must not hide a row; the safe reading of an unknown shape is
/// "this row has not been tombstoned".
pub fn is_archived(payload: &MemoryPayload) -> bool {
    payload
        .metadata
        .get("archived")
        .and_then(|v: &serde_json::Value| v.as_bool())
        .unwrap_or(false)
}

/// The same verdicts, in the words every existing surface already expects.
///
/// Kept as a `Display` rather than duplicated as string literals so the MCP
/// tool text and the daemon's HTTP body cannot drift into two different
/// sentences for one outcome.
impl std::fmt::Display for ArchiveOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveOutcome::Archived { namespace, id } => {
                write!(f, "Archived {} in namespace {}.", id, namespace)
            }
            ArchiveOutcome::NotFound { namespace, id } => {
                write!(f, "No memory with id {} in namespace {}.", id, namespace)
            }
            ArchiveOutcome::Invalid(reason) | ArchiveOutcome::Failed(reason) => write!(f, "{}", reason),
        }
    }
}

/// Tombstone `id` in `namespace` and announce it on the bus.
///
/// Emits only after the write succeeds, so a subscriber is never told a row was
/// retired while it is still active.
///
/// This is the typed core. [`handle_archive_memory`] is the prose wrapper the
/// MCP tool surface uses, where the whole answer is one text block and there
/// is no status code to be wrong.
pub async fn archive_memory(
    arguments: Value,
    store: Arc<dyn VectorStore>,
    bus: Arc<ThalamicBus>,
) -> ArchiveOutcome {
    let id = match arguments.get("id").and_then(|v| v.as_str()) {
        Some(v) => v.to_string(),
        None => return ArchiveOutcome::Invalid("Missing 'id' parameter.".to_string()),
    };
    let namespace_arg = match arguments.get("namespace").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => {
            return ArchiveOutcome::Invalid("ERROR [NAMESPACE]: 'namespace' is missing. You MUST explicitly provide the namespace the memory lives in.".to_string())
        }
    };
    let namespace = crate::server::resolve_namespace(&store, namespace_arg).await;

    // Retiring a row hides it from every future search, so the machine-wide
    // stratum keeps the same guard the destructive tools carry. The wording
    // is the contract `handle_supersede_memory` already publishes; any new
    // retirement-class mutation has to match it rather than invent its own,
    // so the operator gets one shape of consent across the tool surface.
    if namespace == "global"
        && !arguments
            .get("allow_global")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        return ArchiveOutcome::Invalid(
            "ERROR [GLOBAL]: Refusing to archive a memory in the machine-wide 'global' namespace. Rules there apply to every project on this machine. Pass allow_global=true only if you are certain, or archive the project-local rule instead."
                .to_string(),
        );
    }

    let (vector, mut payload) = match store.get(&namespace, &id).await {
        Ok(Some(found)) => found,
        Ok(None) => {
            return ArchiveOutcome::NotFound {
                namespace,
                id,
            }
        }
        Err(e) => return ArchiveOutcome::Failed(format!("Failed to read memory {}: {}", id, e)),
    };
    // A legacy scalar/string metadata cannot carry the tombstone without
    // discarding whatever it did hold, so it is refused rather than clobbered.
    if !payload.metadata.is_object() {
        return ArchiveOutcome::Invalid(format!(
            "Memory {} has legacy-shaped metadata (not a JSON object) and cannot be archived automatically. Re-add it with object metadata first.",
            id
        ));
    }

    payload
        .metadata
        .as_object_mut()
        .expect("object checked above")
        .insert("archived".to_string(), serde_json::json!(true));
    payload
        .metadata
        .as_object_mut()
        .expect("object checked above")
        .insert("archived_at".to_string(), serde_json::json!(chrono::Utc::now().timestamp()));
    if let Err(e) = store.upsert(&namespace, &id, vector, payload).await {
        return ArchiveOutcome::Failed(format!("Failed to archive memory {}: {}", id, e));
    }

    bus.emit(
        ThalamicPulse::Archived { id: id.clone(), namespace: namespace.clone() },
        &RecursionToken::root(),
    );

    ArchiveOutcome::Archived { namespace, id }
}

/// The archive handler as the MCP tool surface wants it: one sentence, either
/// way.
///
/// A thin [`Display`] over [`archive_memory`] rather than a second
/// implementation, so `neurostrata_archive_memory`, `POST /memory/archive` and
/// `neurostrata-mcp archive` are three faces of one decision.
pub async fn handle_archive_memory(
    arguments: Value,
    store: Arc<dyn VectorStore>,
    bus: Arc<ThalamicBus>,
) -> String {
    archive_memory(arguments, store, bus).await.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use serde_json::json;

    // ---- allow_global guard parity --------------------------------------
    //
    // `handle_supersede_memory` refuses the machine-wide 'global' namespace
    // without an explicit `allow_global: true`. Archive is the same class of
    // mutation -- it retires a row from every future read -- so it carries
    // the same guard. A rule that applies to every project on this machine
    // deserves a moment of "are you sure?" before it disappears from
    // recall, no matter which retirement tool the operator reached for.

    /// Seeds a throwaway row in the `global` namespace. The store is fresh
    /// per call so tests cannot read each other's writes.
    async fn seed_global_row(tag: &str) -> (std::sync::Arc<dyn crate::traits::VectorStore>, String) {
        use crate::traits::VectorStore;
        let dir = std::env::temp_dir().join(format!(
            "ns-archive-allow-global-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&dir, 4).expect("open temp"));
        store.init("global").await.expect("global table exists");
        let id = uuid::Uuid::new_v4().to_string();
        let vector = vec![0.1, 0.2, 0.3, 0.4];
        store
            .upsert(
                "global",
                &id,
                vector,
                MemoryPayload {
                    content: "machine-wide rule".to_string(),
                    user_id: "system".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .expect("seed global row");
        (store, id)
    }

    /// Archiving a row in `global` without an explicit `allow_global: true`
    /// is refused -- the same gate `handle_supersede_memory` runs. The row
    /// stays live and no tombstone pulse is emitted.
    #[tokio::test]
    async fn archiving_global_without_allow_global_is_refused() {
        let (store, id) = seed_global_row("refused").await;
        let bus = Arc::new(crate::events::ThalamicBus::new(16));
        let outcome = archive_memory(
            serde_json::json!({ "namespace": "global", "id": id }),
            store.clone(),
            bus.clone(),
        )
        .await;
        // The refusal class is `Invalid`, mirroring the supersede pattern's
        // 400-style error response.
        assert!(
            matches!(outcome, ArchiveOutcome::Invalid(_)),
            "the refusal class is Invalid, not Archived: {outcome:?}"
        );
        // The row is unchanged -- no metadata.archived, no pulse.
        let (_, payload) = store.get("global", &id).await.unwrap().expect("still present");
        assert!(
            !crate::handlers::archive_memory::is_archived(&payload),
            "the row's metadata still says live"
        );
    }

    /// `allow_global: true` is the explicit consent the gate demands. With
    /// it, the archive proceeds and the row's metadata is tombstoned.
    #[tokio::test]
    async fn archiving_global_with_allow_global_true_succeeds() {
        let (store, id) = seed_global_row("succeeds").await;
        let bus = Arc::new(crate::events::ThalamicBus::new(16));
        let outcome = archive_memory(
            serde_json::json!({
                "namespace": "global",
                "id": id,
                "allow_global": true
            }),
            store.clone(),
            bus.clone(),
        )
        .await;
        assert!(outcome.is_archived(), "the archive really lands: {outcome:?}");
        let (_, payload) = store.get("global", &id).await.unwrap().expect("still present");
        assert!(crate::handlers::archive_memory::is_archived(&payload));
    }

    /// Non-global namespaces do not need the flag. The guard is the
    /// supersede pattern's exact `global && !allow_global` predicate, so a
    /// project-local row is unaffected.
    #[tokio::test]
    async fn archiving_a_project_local_namespace_does_not_need_allow_global() {
        use crate::traits::VectorStore;
        let dir = std::env::temp_dir().join(format!(
            "ns-archive-project-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&dir, 4).expect("open temp"));
        store.init("probe").await.expect("probe table exists");
        let id = uuid::Uuid::new_v4().to_string();
        store
            .upsert(
                "probe",
                &id,
                vec![0.1, 0.2, 0.3, 0.4],
                MemoryPayload {
                    content: "project-local rule".to_string(),
                    user_id: "system".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .expect("seed");
        let bus = Arc::new(crate::events::ThalamicBus::new(16));
        let outcome = archive_memory(
            serde_json::json!({ "namespace": "probe", "id": id }),
            store.clone(),
            bus,
        )
        .await;
        assert!(outcome.is_archived(), "project-local archive proceeds without the flag: {outcome:?}");
    }

    /// The error message is specific: it names the namespace, names the
    /// flag, and points at the project-local alternative. The supersede
    /// guard's wording is the contract every retirement-class tool matches.
    #[tokio::test]
    async fn the_refusal_message_names_global_and_allow_global() {
        let (store, id) = seed_global_row("message").await;
        let bus = Arc::new(crate::events::ThalamicBus::new(16));
        let outcome = archive_memory(
            serde_json::json!({ "namespace": "global", "id": id }),
            store.clone(),
            bus,
        )
        .await;
        let message = match outcome {
            ArchiveOutcome::Invalid(m) => m,
            other => panic!("expected Invalid, got {other:?}"),
        };
        assert!(message.contains("global"), "names the namespace: {message}");
        assert!(message.contains("allow_global"), "names the flag: {message}");
    }

    /// Read-time filter helper. The tombstone (`metadata.archived: true`) is
    /// the producer's signal; every read surface asks this one question of
    /// each row before letting it through.
    ///
    /// `metadata.archived == true` is archived. Anything else -- absent,
    /// false, non-boolean, malformed -- is not. A typo or a wrong-typed
    /// sentinel must not hide a row from the operator; the safe reading is
    /// "this row has not been tombstoned".
    #[test]
    fn is_archived_is_true_only_for_the_typed_true() {
        let mut p = payload_fixture();
        p.metadata = json!({ "archived": true });
        assert!(is_archived(&p));
    }

    #[test]
    fn is_archived_is_false_for_every_other_shape() {
        for metadata in [
            json!({}),
            json!({ "archived": false }),
            json!({ "archived": null }),
            json!({ "archived": "true" }),
            json!({ "archived": 1 }),
            json!({ "archived": [] }),
            json!({ "archived": {} }),
            json!("not-an-object"),
        ] {
            let mut p = payload_fixture();
            p.metadata = metadata.clone();
            assert!(
                !is_archived(&p),
                "a row with `metadata = {metadata}` is not archived"
            );
        }
    }

    /// The producer of the field gets one shape and the reader must agree on
    /// it: a row that round-tripped through `archive_memory` reads as
    /// archived through `is_archived`. If the two ever drift, the read
    /// surface silently shows what the write surface tombstoned.
    #[test]
    fn is_archived_round_trips_with_archive_memorys_tombstone() {
        // Construct a payload with object metadata (the shape archive_memory
        // requires) and the field it sets.
        let mut p = payload_fixture();
        p.metadata = json!({ "archived": true, "archived_at": 1_700_000_000 });
        assert!(is_archived(&p), "the field archive_memory writes reads as archived");
    }

    fn payload_fixture() -> MemoryPayload {
        MemoryPayload {
            content: "fixture".to_string(),
            user_id: "test".to_string(),
            memory_type: "rule".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        }
    }

    #[test]
    fn only_the_archived_variant_reads_as_success() {
        let archived = ArchiveOutcome::Archived {
            namespace: "ns".into(),
            id: "m1".into(),
        };
        assert!(archived.is_archived());
        for refused in [
            ArchiveOutcome::NotFound {
                namespace: "ns".into(),
                id: "m1".into(),
            },
            ArchiveOutcome::Invalid("bad request".into()),
            ArchiveOutcome::Failed("store said no".into()),
        ] {
            assert!(
                !refused.is_archived(),
                "a refusal read as success is the hazard this enum exists to remove: {refused:?}"
            );
        }
    }

    #[test]
    fn every_variant_still_says_what_it_always_said() {
        // The tool surface's sentences are a wire contract of their own --
        // server.rs asserts on the word "Archived" -- so Display is held to
        // them here rather than left to drift.
        assert_eq!(
            ArchiveOutcome::Archived {
                namespace: "ns".into(),
                id: "m1".into()
            }
            .to_string(),
            "Archived m1 in namespace ns."
        );
        assert_eq!(
            ArchiveOutcome::NotFound {
                namespace: "ns".into(),
                id: "m1".into()
            }
            .to_string(),
            "No memory with id m1 in namespace ns."
        );
        assert_eq!(ArchiveOutcome::Invalid("nope".into()).to_string(), "nope");
        assert_eq!(ArchiveOutcome::Failed("nope".into()).to_string(), "nope");
    }
}