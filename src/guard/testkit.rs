//! Test doubles for the guard's own tests.
//!
//! The guard's contract is about what it *records*, so its tests need a store
//! they can read back. The real LadybugDB store would do that, but it drags an
//! engine and a vector round-trip into every assertion; an in-memory store
//! keeps the assertions on the thing under test -- the recorded signal.

use crate::domain::domain_key;
use crate::traits::{
    DedupCandidateScope, DomainMatchMode, DomainSearchOutcome, Embedder, ExactContentCandidate,
    MemoryPayload, RawDistanceCandidate, RawDistanceQuery, RelocateOutcome, SearchResult,
    UpsertReport, VectorStore,
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
    /// Toggle to make search/search_with_domain return an error.
    search_fails: Mutex<bool>,
    /// Track which (namespace, id) pairs have had increment_access_count called.
    incremented_ids: Mutex<Vec<(String, String)>>,
}

impl MockStore {
    pub fn new() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
            search_fails: Mutex::new(false),
            incremented_ids: Mutex::new(Vec::new()),
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

    /// List of (namespace, id) pairs that have had `increment_access_count` called.
    /// Used by tests to verify the access count was incremented.
    /// Temporarily allowed until Task 6's tests consume it.
    #[allow(dead_code)]
    pub(crate) fn incremented_ids(&self) -> Vec<(String, String)> {
        self.incremented_ids
            .lock()
            .expect("mock store lock")
            .clone()
    }

    /// Configure whether `search` and `search_with_domain` should fail.
    /// Used by tests to exercise error paths.
    /// Temporarily allowed until Task 6's tests consume it.
    #[allow(dead_code)]
    pub(crate) fn set_search_fails(&self, fails: bool) {
        *self.search_fails.lock().expect("mock store lock") = fails;
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
        if *self.search_fails.lock().expect("mock store lock") {
            return Err(anyhow::anyhow!("MockStore search configured to fail"));
        }
        let rows = self.rows.lock().expect("mock store lock");
        Ok(rows
            .iter()
            .filter(|r| r.namespace == namespace)
            .take(limit)
            .map(|r| SearchResult {
                id: r.id.clone(),
                score: 1.0,
                payload: r.payload.clone(),
                evidence: Some(vec![crate::traits::SearchEvidence {
                    kind: crate::traits::EvidenceKind::DirectMatch,
                    path: vec![],
                    explanation: "This memory matched the query vector.".to_string(),
                }]),
            })
            .collect())
    }

    async fn search_with_domain(
        &self,
        namespace: &str,
        _vector: Vec<f32>,
        limit: usize,
        _include_tasks: bool,
        _include_archived: bool,
        domains: &[String],
    ) -> Result<DomainSearchOutcome> {
        if *self.search_fails.lock().expect("mock store lock") {
            return Err(anyhow::anyhow!(
                "MockStore search_with_domain configured to fail"
            ));
        }

        // Normalize the provided domains into a set (lowercase-kebab).
        let domain_keys: std::collections::HashSet<String> = domains
            .iter()
            .filter_map(|d| crate::domain::normalize_domain_slug(d))
            .collect();

        let rows = self.rows.lock().expect("mock store lock");
        let mut all_primaries = Vec::new();
        let mut filtered_primaries = Vec::new();

        for r in rows.iter().filter(|r| r.namespace == namespace) {
            // Domain filter check: happens BEFORE we consider the row for the filtered set.
            // The domain_keys set is already normalized (lowercase-kebab).
            let domain_match = if domain_keys.is_empty() {
                true // empty key set = no filter asked for
            } else {
                domain_key(&r.payload.metadata)
                    .map(|k| domain_keys.contains(&k))
                    .unwrap_or(false)
            };

            let sr = SearchResult {
                id: r.id.clone(),
                score: 1.0,
                payload: r.payload.clone(),
                evidence: Some(vec![crate::traits::SearchEvidence {
                    kind: crate::traits::EvidenceKind::DirectMatch,
                    path: vec![],
                    explanation: "This memory matched the query vector.".to_string(),
                }]),
            };

            // Every accepted row goes to all_primaries
            all_primaries.push(sr.clone());

            // Only domain-matching rows go to filtered_primaries
            if domain_match {
                filtered_primaries.push(sr);
            }

            // Stop condition: honour limit in the no-filter branch too,
            // matching the real store's behaviour (src/store/ladybug.rs:164-172).
            let stop_count = if domain_keys.is_empty() {
                all_primaries.len()
            } else {
                filtered_primaries.len()
            };
            if stop_count >= limit {
                break;
            }
        }

        // Determine chosen set and mode (soft fallback when filter empties candidates).
        let (chosen_primaries, mode) = if domain_keys.is_empty() {
            // No filter asked for: all rows go to both lists, mode is Vector
            (all_primaries, DomainMatchMode::Vector)
        } else if filtered_primaries.is_empty() && !all_primaries.is_empty() {
            // Filter would empty the candidates: skip it, use unfiltered, mode is Vector
            (
                all_primaries.into_iter().take(limit).collect(),
                DomainMatchMode::Vector,
            )
        } else {
            // Filter applied successfully
            (
                filtered_primaries.into_iter().take(limit).collect(),
                DomainMatchMode::Domain,
            )
        };

        Ok(DomainSearchOutcome {
            results: chosen_primaries,
            mode,
        })
    }

    async fn nearest_by_raw_distance(
        &self,
        namespace: &str,
        _vector: Vec<f32>,
        query: RawDistanceQuery,
    ) -> Result<Vec<RawDistanceCandidate>> {
        let rows = self.rows.lock().expect("mock store lock");
        let mut out = Vec::new();
        for r in rows.iter().filter(|r| r.namespace == namespace) {
            if out.len() >= query.limit {
                break;
            }
            out.push(RawDistanceCandidate {
                id: r.id.clone(),
                raw_distance: 0.0,
                payload: r.payload.clone(),
            });
        }
        Ok(out)
    }

    async fn find_active_exact_content(
        &self,
        namespace: &str,
        exact_content: &str,
        _scope: DedupCandidateScope,
    ) -> Result<Vec<ExactContentCandidate>> {
        let rows = self.rows.lock().expect("mock store lock");
        Ok(rows
            .iter()
            .filter(|r| r.namespace == namespace && r.payload.content == exact_content)
            .map(|r| ExactContentCandidate {
                id: r.id.clone(),
                payload: r.payload.clone(),
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

    async fn increment_access_count(&self, namespace: &str, id: &str) -> Result<()> {
        self.incremented_ids
            .lock()
            .expect("mock store lock")
            .push((namespace.to_string(), id.to_string()));
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
