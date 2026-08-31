use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

    /// List all existing namespaces (tables)
    async fn list_namespaces(&self) -> Result<Vec<String>>;

    /// Export the entire graph as a JSON object with `nodes` and `links`.
    async fn export_graph(&self) -> Result<serde_json::Value>;

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
