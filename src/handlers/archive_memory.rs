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
use crate::traits::VectorStore;
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