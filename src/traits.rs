use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

/// The shipped vocabulary schema, embedded at compile time.
pub const MEMORY_VOCABULARY_JSON: &str = include_str!("schemas/memory-vocabulary.v1.json");

/// Parsed vocabulary, loaded once on first access.
pub fn memory_vocabulary() -> &'static serde_json::Value {
    static VOCAB: OnceLock<serde_json::Value> = OnceLock::new();
    VOCAB.get_or_init(|| {
        serde_json::from_str(MEMORY_VOCABULARY_JSON)
            .expect("shipped memory vocabulary parses")
    })
}

/// Represents a stored memory payload
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryPayload {
    pub content: String,
    pub user_id: String,
    pub memory_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    pub location: String,
    pub location_lines: String,
    #[serde(default)]
    pub metadata: Value,
}

/// Represents a search result from the vector database
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub payload: MemoryPayload,
    /// Transient evidence paths attached during graph expansion. Never persisted,
    /// never in metadata — only added as text lines to search results.
    #[serde(skip)]
    pub evidence: Option<Vec<SearchEvidence>>,
}

/// How a search result was discovered relative to the direct matches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    DirectMatch,
    OneHop,
    #[serde(rename = "governs_2hop")]
    Governs2hop,
}

/// One directed edge in an evidence path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct EvidenceEdge {
    pub source: String,
    pub relation: String,
    pub target: String,
}

/// A complete evidence path explaining why an expanded result surfaced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchEvidence {
    pub kind: EvidenceKind,
    pub path: Vec<EvidenceEdge>,
    pub explanation: String,
}

/// What an attempt to move a memory between namespaces found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RelocateOutcome {
    Moved,
    NotFound,
    /// Written by directory ingestion. Its id carries the namespace that owns
    /// it, so the move is refused: ingest the directory into the other
    /// namespace instead.
    Ingested,
    /// Source and target are the same namespace, so there is nothing to do.
    SameNamespace,
}

/// The core interface for generating vector embeddings from text.
/// By making this a trait, we can swap between Local (FastEmbed/ONNX),
/// Remote (Ollama), or Cloud (OpenAI) implementations.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Convert text into a dense vector representation.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Get the expected dimension of the embeddings (e.g., 768 for nomic-embed-text)
    fn dimensions(&self) -> usize;
}

/// The core interface for vector storage and retrieval.
/// By making this a trait, we can swap between Embedded Qdrant,
/// Remote Qdrant, LadybugDB, or SQLite-VSS.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Ensure the necessary collections/tables exist.
    async fn init(&self, namespace: &str) -> Result<()>;

    /// Insert or update a memory with its associated vector and metadata.
    async fn upsert(
        &self,
        namespace: &str,
        id: &str,
        vector: Vec<f32>,
        payload: MemoryPayload,
    ) -> Result<()>;

    /// Search for the closest memories to a given vector.
    async fn search(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<SearchResult>>;

    /// Delete a specific memory by its ID.
    async fn delete(&self, namespace: &str, id: &str) -> Result<()>;

    /// Remove every row owned by the directory ingester in a namespace: the AST
    /// symbols and the directory/file nodes, which ingestion then rebuilds.
    async fn clear_ingested(&self, namespace: &str) -> Result<()>;

    /// Rebuilds the edges a namespace's memories declare, and reports how many
    /// were materialised.
    ///
    /// An edge is written when the memory that declares it is written, so a
    /// target that did not exist yet simply produced nothing. Ingestion deletes
    /// and re-creates every code node, taking those edges with it -- and the
    /// rules pointing at that code are not rewritten, so the links stay lost
    /// until something replays them (bead neurostrata-sij).
    async fn relink_edges(&self, namespace: &str) -> Result<usize>;

    /// List all memories
    async fn list(&self, namespace: &str, user_id: Option<&str>) -> Result<Vec<SearchResult>>;

    /// Get a specific memory by its ID, returning its vector and payload
    async fn get(&self, namespace: &str, id: &str) -> Result<Option<(Vec<f32>, MemoryPayload)>>;

    /// Moves a memory to another namespace by changing that one field, so its
    /// id, vector and edges stay as they were and nothing is ever deleted.
    ///
    /// Not built from upsert and delete: upsert deliberately never rewrites a
    /// namespace, so that an ingest cannot pull another project's node into its
    /// own, which makes "write to the target, delete from the source" delete the
    /// only copy.
    async fn relocate(&self, id: &str, from: &str, to: &str) -> Result<RelocateOutcome>;

    /// List all existing namespaces (tables)
    async fn list_namespaces(&self) -> Result<Vec<String>>;

    /// Export the entire graph as a JSON object with `nodes` and `links`.
    /// When `include_retired` is false (the default), retired memories and
    /// their incident edges are excluded from the view.
    async fn export_graph(&self, include_retired: bool) -> Result<serde_json::Value>;

    /// Increment the access count of a specific memory by its ID.
    async fn increment_access_count(&self, namespace: &str, id: &str) -> Result<()>;

    /// Write a portable copy of the whole database into `dir`, which must be
    /// empty. The engine's own export: parquet per table plus the schema.
    async fn export_database(&self, dir: &str) -> Result<()>;

    /// Load a database previously written by `export_database`. Replays the
    /// exported schema, so it is destructive against a database that has one.
    async fn import_database(&self, dir: &str) -> Result<()>;

    /// Flush everything written so far to durable storage. A long-running
    /// process must call this; writes that only reached the WAL are discarded
    /// if the process dies before the engine checkpoints on its own.
    async fn checkpoint(&self) -> Result<()>;

    /// True when a write has landed that no checkpoint has flushed yet.
    ///
    /// A checkpoint waits for every active transaction to drain, so it cannot
    /// get that window while queries keep arriving. Knowing there is nothing to
    /// flush lets the daemon skip the attempt entirely rather than block on one
    /// that would only time out. Conservative by default: assume there is.
    fn is_dirty(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_vocabulary_v1_parses_and_has_exact_relations() {
        let v = memory_vocabulary();
        assert_eq!(v["vocabulary_version"], 1);
        let rels = v["relations"].as_object().unwrap();
        assert!(rels.contains_key("GOVERNS"));
        assert!(rels.contains_key("CONTAINS"));
        assert!(rels.contains_key("RELATES_TO"));
        assert_eq!(rels.len(), 3);
        assert_eq!(rels["GOVERNS"]["direction"], "directed");
        assert_eq!(rels["CONTAINS"]["direction"], "directed");
        assert_eq!(rels["RELATES_TO"]["direction"], "undirected");
    }

    #[test]
    fn memory_vocabulary_keeps_code_ast_as_symbol_alias() {
        let v = memory_vocabulary();
        assert_eq!(v["type_aliases"]["code_ast"], "symbol");
        // Structural types match STRUCTURAL_MEMORY_TYPES in ladybug.rs
        let types = v["memory_types"].as_object().unwrap();
        assert!(types["directory"]["structural"].as_bool().unwrap());
        assert!(types["file"]["structural"].as_bool().unwrap());
        assert!(types["markdown"]["structural"].as_bool().unwrap());
        assert!(!types["symbol"]["structural"].as_bool().unwrap());
    }

    #[test]
    fn search_result_serialization_never_exposes_evidence() {
        let sr = SearchResult {
            id: "test-id".to_string(),
            score: 1.0,
            payload: MemoryPayload {
                content: "c".into(), user_id: "u".into(),
                memory_type: "rule".into(), agent_name: None,
                location: String::new(), location_lines: String::new(),
                metadata: serde_json::json!({}),
            },
            evidence: Some(vec![SearchEvidence {
                kind: EvidenceKind::DirectMatch,
                path: vec![],
                explanation: "test".into(),
            }]),
        };
        let json = serde_json::to_value(&sr).unwrap();
        assert!(json.get("evidence").is_none(), "evidence must be skip-serialized");
    }

    #[test]
    fn search_result_deserialization_defaults_evidence_to_none() {
        let json_str = r#"{"id":"x","score":0.5,"payload":{"content":"c","user_id":"u","memory_type":"rule","location":"","location_lines":"","metadata":{}}}"#;
        let sr: SearchResult = serde_json::from_str(json_str).unwrap();
        assert_eq!(sr.id, "x");
        assert!(sr.evidence.is_none());
    }

    #[test]
    fn evidence_json_has_exact_kind_path_and_explanation() {
        let ev = SearchEvidence {
            kind: EvidenceKind::Governs2hop,
            path: vec![
                EvidenceEdge { source: "a".into(), relation: "CONTAINS".into(), target: "b".into() },
                EvidenceEdge { source: "c".into(), relation: "GOVERNS".into(), target: "b".into() },
            ],
            explanation: "explanation".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "governs_2hop");
        assert_eq!(json["path"][0]["source"], "a");
        assert_eq!(json["path"][0]["relation"], "CONTAINS");
        assert_eq!(json["path"][1]["target"], "b");
        assert_eq!(json["explanation"], "explanation");
    }
}
