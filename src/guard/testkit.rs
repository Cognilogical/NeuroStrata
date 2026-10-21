//! Test doubles for the guard's own tests.
//!
//! The guard's contract is about what it *records*, so its tests need a store
//! they can read back. The real LadybugDB store would do that, but it drags an
//! engine and a vector round-trip into every assertion; an in-memory store
//! keeps the assertions on the thing under test -- the recorded signal.

use crate::traits::{
    Embedder, MemoryPayload, RelocateOutcome, SearchResult, UpsertReport, VectorStore,
};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// One stored row: the namespace it belongs to, its id, its vector, its payload.
#[derive(Clone)]
struct Row {
    namespace: String,
    id: String,
    vector: Vec<f32>,
    payload: MemoryPayload,
}

/// An in-memory `VectorStore` for the guard's tests.
///
/// `search` ignores the query vector and returns the namespace's rows in
/// insertion order: the guard's evaluator asks for the top N candidates, and
/// the tests need to decide which rules are candidates and in what order.
pub struct MockStore {
    rows: Mutex<Vec<Row>>,
}

impl MockStore {
    pub fn new() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
        }
    }

    /// The `hit_count` the store holds for a row, exactly as it is persisted.
    pub fn hit_count_of(&self, namespace: &str, id: &str) -> Option<u64> {
        let rows = self.rows.lock().expect("mock store lock");
        rows.iter()
            .find(|r| r.namespace == namespace && r.id == id)
            .and_then(|r| r.payload.metadata.get("hit_count").and_then(|v| v.as_u64()))
    }

    /// The whole metadata of a row, for assertions about what was preserved.
    pub fn metadata_of(&self, namespace: &str, id: &str) -> Option<serde_json::Value> {
        let rows = self.rows.lock().expect("mock store lock");
        rows.iter()
            .find(|r| r.namespace == namespace && r.id == id)
            .map(|r| r.payload.metadata.clone())
    }
}

impl Default for MockStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl VectorStore for MockStore {
    async fn init(&self, _namespace: &str) -> Result<()> {
        Ok(())
    }

    async fn upsert(
        &self,
        namespace: &str,
        id: &str,
        vector: Vec<f32>,
        payload: MemoryPayload,
    ) -> Result<UpsertReport> {
        let mut rows = self.rows.lock().expect("mock store lock");
        let row = Row {
            namespace: namespace.to_string(),
            id: id.to_string(),
            vector,
            payload,
        };
        match rows
            .iter_mut()
            .find(|r| r.namespace == namespace && r.id == id)
        {
            Some(existing) => *existing = row,
            None => rows.push(row),
        }
        Ok(UpsertReport {
            id: id.to_string(),
            deferred_edges: Vec::new(),
        })
    }

    async fn search(
        &self,
        namespace: &str,
        _vector: Vec<f32>,
        limit: usize,
        _include_tasks: bool,
        _include_archived: bool,
    ) -> Result<Vec<SearchResult>> {
        let rows = self.rows.lock().expect("mock store lock");
        Ok(rows
            .iter()
            .filter(|r| r.namespace == namespace)
            .take(limit)
            .map(|r| SearchResult {
                id: r.id.clone(),
                score: 1.0,
                payload: r.payload.clone(),
                evidence: None,
            })
            .collect())
    }

    async fn delete(&self, namespace: &str, id: &str) -> Result<()> {
        let mut rows = self.rows.lock().expect("mock store lock");
        rows.retain(|r| !(r.namespace == namespace && r.id == id));
        Ok(())
    }

    async fn clear_ingested(&self, _namespace: &str) -> Result<()> {
        Ok(())
    }

    async fn relink_edges(&self, _namespace: &str) -> Result<usize> {
        Ok(0)
    }

    async fn list(&self, namespace: &str, user_id: Option<&str>) -> Result<Vec<SearchResult>> {
        let rows = self.rows.lock().expect("mock store lock");
        Ok(rows
            .iter()
            .filter(|r| r.namespace == namespace)
            .filter(|r| user_id.map(|u| r.payload.user_id == u).unwrap_or(true))
            .map(|r| SearchResult {
                id: r.id.clone(),
                score: 1.0,
                payload: r.payload.clone(),
                evidence: None,
            })
            .collect())
    }

    async fn get(&self, namespace: &str, id: &str) -> Result<Option<(Vec<f32>, MemoryPayload)>> {
        let rows = self.rows.lock().expect("mock store lock");
        Ok(rows
            .iter()
            .find(|r| r.namespace == namespace && r.id == id)
            .map(|r| (r.vector.clone(), r.payload.clone())))
    }

    async fn relocate(&self, _id: &str, _from: &str, _to: &str) -> Result<RelocateOutcome> {
        Ok(RelocateOutcome::SameNamespace)
    }

    async fn list_namespaces(&self) -> Result<Vec<String>> {
        let rows = self.rows.lock().expect("mock store lock");
        let mut names: Vec<String> = Vec::new();
        for r in rows.iter() {
            if !names.contains(&r.namespace) {
                names.push(r.namespace.clone());
            }
        }
        Ok(names)
    }

    async fn export_graph(
        &self,
        _include_retired: bool,
        _include_archived: bool,
        _project_contexts: &HashMap<String, PathBuf>,
        _anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
    ) -> Result<serde_json::Value> {
        Ok(serde_json::json!({"nodes": [], "links": []}))
    }

    async fn increment_access_count(&self, _namespace: &str, _id: &str) -> Result<()> {
        Ok(())
    }

    async fn export_database(&self, _dir: &str) -> Result<()> {
        Ok(())
    }

    async fn checkpoint(&self) -> Result<()> {
        Ok(())
    }
}

/// An embedder that produces a fixed-size vector without touching a model.
///
/// The mock store ignores the vector, so the values only have to be a valid
/// `Vec<f32>` of the declared length.
pub struct StubEmbedder;

#[async_trait::async_trait]
impl Embedder for StubEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = vec![0.0f32; 4];
        for (i, b) in text.bytes().enumerate() {
            v[i % 4] += b as f32;
        }
        for (i, x) in v.iter_mut().enumerate() {
            *x += (i as f32 + 1.0) * 0.001;
        }
        Ok(v)
    }

    fn dimensions(&self) -> usize {
        4
    }
}
