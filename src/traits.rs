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
/// Remote Qdrant, LanceDB, or SQLite-VSS.
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

    /// List all memories
    async fn list(&self, namespace: &str, user_id: Option<&str>) -> Result<Vec<SearchResult>>;

    /// Get a specific memory by its ID, returning its vector and payload
    async fn get(&self, namespace: &str, id: &str) -> Result<Option<(Vec<f32>, MemoryPayload)>>;

    /// List all existing namespaces (tables)
    async fn list_namespaces(&self) -> Result<Vec<String>>;
}
