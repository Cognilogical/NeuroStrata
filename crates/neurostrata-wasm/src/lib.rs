use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[path = "../../../src/fts/mod.rs"]
pub mod fts;
#[path = "../../../src/graph_read/related.rs"]
pub mod related;

// §5.2 Snapshot schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub version: u32,
    pub records: Vec<SnapshotRecord>,
    #[serde(default)]
    pub edges: Vec<SnapshotEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub id: String,
    pub namespace: String,
    pub payload: SnapshotPayload,
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
    #[serde(default)]
    pub embedding_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotPayload {
    pub content: String,
    pub user_id: String,
    pub memory_type: String,
    pub agent_name: String,
    pub location: String,
    pub location_lines: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEdge {
    pub source: String,
    pub relation: String,
    pub target: String,
}

// §5.4 Ported pure JSON predicates (native sources: src/synaptic_mesh/conflict.rs, src/registry.rs)
pub fn is_conflicting_metadata(metadata: &serde_json::Value) -> bool {
    metadata.get("mesh").and_then(|m| m.get("conflict")).and_then(|v| v.as_bool()).unwrap_or(false)
}

pub fn is_registry_row(metadata: &serde_json::Value) -> bool {
    metadata.get("domain_registry").is_some()
}

// §5.4 Row-class visibility filter
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ReadOptions {
    #[serde(default)]
    pub include_tasks: bool,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default)]
    pub include_structural: bool,
}

pub const STRUCTURAL_TYPES: [&str; 8] = [
    "directory", "file", "markdown", "freshness_flag", "guard_event",
    "community_summary", "digital_twin", "digital_twin_schema",
];

pub fn is_visible(rec: &SnapshotRecord, now_unix: i64, opts: &ReadOptions) -> bool {
    if rec.payload.metadata.get("valid_to").and_then(|v| v.as_i64()).map(|vt| vt > 0 && vt <= now_unix).unwrap_or(false) {
        return false;
    }
    if rec.payload.metadata.get("archived").and_then(|v| v.as_bool()).unwrap_or(false) && !opts.include_archived {
        return false;
    }
    if is_conflicting_metadata(&rec.payload.metadata) {
        return false;
    }
    if is_registry_row(&rec.payload.metadata) {
        return false;
    }
    if rec.payload.memory_type == "task" && !opts.include_tasks {
        return false;
    }
    if STRUCTURAL_TYPES.contains(&rec.payload.memory_type.as_str()) && !opts.include_structural {
        return false;
    }
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadWarning {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadReport {
    pub ok: bool,
    pub loaded: usize,
    pub skipped: usize,
    pub warnings: Vec<LoadWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: String,
    pub score: f32,
}

pub struct NamespaceData {
    pub records: BTreeMap<String, SnapshotRecord>,
    pub edges: Vec<SnapshotEdge>,
    pub lexical: fts::LexicalIndex,
    pub vector_dims: Option<usize>,
    pub embedding_id: Option<String>,
}

impl NamespaceData {
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            edges: Vec::new(),
            lexical: fts::LexicalIndex::new(),
            vector_dims: None,
            embedding_id: None,
        }
    }
}

pub struct WasmStore {
    pub namespaces: BTreeMap<String, NamespaceData>,
    pub now_unix: i64,
}

impl WasmStore {
    pub fn new(now_unix: i64) -> Self {
        Self { namespaces: BTreeMap::new(), now_unix }
    }

    pub fn from_snapshot(file: SnapshotFile, now_unix: i64) -> Result<(Self, LoadReport), String> {
        if file.version != 1 {
            return Err("unsupported snapshot version".to_string());
        }
        let mut store = Self::new(now_unix);
        let mut loaded = 0usize;
        let mut skipped = 0usize;
        let mut warnings: Vec<LoadWarning> = Vec::new();
        for rec in file.records {
            if rec.id.trim().is_empty() || rec.namespace.trim().is_empty()
                || rec.payload.content.trim().is_empty() || rec.payload.memory_type.trim().is_empty()
                || !rec.payload.metadata.is_object() {
                skipped += 1;
                warnings.push(LoadWarning { id: rec.id.clone(), reason: "InvalidRecord".to_string() });
                continue;
            }
            let ns = rec.namespace.clone();
            let id = rec.id.clone();
            if let Some(vec) = &rec.vector {
                if vec.is_empty() {
                    skipped += 1;
                    warnings.push(LoadWarning { id: id.clone(), reason: "DimensionMismatch".to_string() });
                    continue;
                }
                // Validate against existing namespace state BEFORE creating or
                // mutating any entry: a namespace whose only records were all
                // skipped (dimension/embedding mismatch) must never materialize
                // in `namespaces()`.
                if let Some(existing) = store.namespaces.get(&ns) {
                    if existing.vector_dims.map_or(false, |dims| dims != vec.len()) {
                        skipped += 1;
                        warnings.push(LoadWarning { id: id.clone(), reason: "DimensionMismatch".to_string() });
                        continue;
                    }
                    if let Some(expected) = &existing.embedding_id {
                        if !expected.is_empty()
                            && rec.embedding_id.as_deref().map_or(true, |e| e != expected) {
                            skipped += 1;
                            warnings.push(LoadWarning { id: id.clone(), reason: "EmbeddingMismatch".to_string() });
                            continue;
                        }
                    }
                }
            }
            let ns_data = store.namespaces.entry(ns.clone()).or_insert_with(NamespaceData::new);
            if let Some(vec) = &rec.vector {
                if ns_data.vector_dims.is_none() {
                    ns_data.vector_dims = Some(vec.len());
                }
            }
            // Backfill so a first record with `embedding_id: None` does not
            // permanently block a later record from establishing it.
            if ns_data.embedding_id.is_none() && rec.embedding_id.is_some() {
                ns_data.embedding_id = rec.embedding_id.clone();
            }
            let was_duplicate = ns_data.records.contains_key(&id);
            if was_duplicate {
                warnings.push(LoadWarning { id: id.clone(), reason: "DuplicateId".to_string() });
                ns_data.lexical.remove(&ns, &id);
            }
            let mut doc = fts::LexicalDoc::new(&id, &ns, &rec.payload.memory_type, &rec.payload.content);
            doc.location = rec.payload.location.clone();
            doc.symbol = rec.payload.metadata.get("symbol").and_then(|v| v.as_str()).unwrap_or("").to_string();
            doc.archived = rec.payload.metadata.get("archived").and_then(|v| v.as_bool()).unwrap_or(false);
            doc.valid_from = rec.payload.metadata.get("valid_from").and_then(|v| v.as_i64());
            doc.valid_to = rec.payload.metadata.get("valid_to").and_then(|v| v.as_i64());
            doc.structural = STRUCTURAL_TYPES.contains(&rec.payload.memory_type.as_str());
            doc.registry = is_registry_row(&rec.payload.metadata);
            doc.conflicting = is_conflicting_metadata(&rec.payload.metadata);
            ns_data.lexical.add(doc);
            ns_data.records.insert(id.clone(), rec);
            if !was_duplicate { loaded += 1; }
        }
        for edge in file.edges {
            for (_, ns_data) in store.namespaces.iter_mut() {
                // An edge is only meaningful inside a namespace that contains
                // BOTH endpoints.
                if ns_data.records.contains_key(&edge.source)
                    && ns_data.records.contains_key(&edge.target) {
                    ns_data.edges.push(edge.clone());
                }
            }
        }
        Ok((store, LoadReport { ok: true, loaded, skipped, warnings, error: None }))
    }
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let mut dot = 0.0f32;
    let mut norm_a_sq = 0.0f32;
    let mut norm_b_sq = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a_sq += a[i] * a[i];
        norm_b_sq += b[i] * b[i];
    }
    let norm_a = norm_a_sq.sqrt();
    let norm_b = norm_b_sq.sqrt();
    if norm_a <= f32::EPSILON || norm_b <= f32::EPSILON { 0.0 } else { dot / (norm_a * norm_b) }
}

#[wasm_bindgen]
pub struct WasmEngine {
    store: WasmStore,
}

#[wasm_bindgen]
impl WasmEngine {
    #[wasm_bindgen(constructor)]
    pub fn new(now_unix: f64) -> Self {
        let n = if now_unix.is_finite() { now_unix as i64 } else { 0 };
        Self { store: WasmStore::new(n) }
    }

    pub fn load_snapshot(&mut self, json: &str) -> String {
        let parsed: SnapshotFile = match serde_json::from_str(json) {
            Ok(f) => f,
            Err(e) => return serde_json::to_string(&LoadReport { ok: false, loaded: 0, skipped: 0, warnings: vec![], error: Some(format!("invalid snapshot JSON: {}", e)) }).unwrap(),
        };
        match WasmStore::from_snapshot(parsed, self.store.now_unix) {
            Ok((new_store, report)) => {
                self.store = new_store;
                serde_json::to_string(&report).unwrap()
            }
            Err(msg) => {
                serde_json::to_string(&LoadReport { ok: false, loaded: 0, skipped: 0, warnings: vec![], error: Some(msg) }).unwrap()
            }
        }
    }

    pub fn search(&self, namespace: &str, query: &str, limit: usize, include_tasks: bool, include_archived: bool) -> String {
        if query.trim().is_empty() || !self.store.namespaces.contains_key(namespace) {
            return "[]".to_string();
        }
        let ns = self.store.namespaces.get(namespace).unwrap();
        let clamped_limit = limit.max(1).min(100);
        let opts = fts::SearchOptions {
            now: self.store.now_unix,
            as_of: None,
            include_tasks,
            include_archived,
            principal: "admin".to_string(),
            domains: Vec::new(),
            fuzzy_max_distance: 2,
            limit: clamped_limit,
        };
        let hits = ns.lexical.search(namespace, query, &opts);
        let mut result: Vec<SearchHit> = Vec::new();
        for hit in hits.hits {
            let sim = 1.0 - hit.score;
            result.push(SearchHit { id: hit.id, score: sim });
        }
        result.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap().then_with(|| a.id.cmp(&b.id))
        });
        result.truncate(clamped_limit);
        serde_json::to_string(&result).unwrap()
    }

    pub fn get_memory(&self, namespace: &str, id: &str, include_tasks: bool, include_archived: bool) -> String {
        let opts = ReadOptions { include_tasks, include_archived, include_structural: false };
        if let Some(ns) = self.store.namespaces.get(namespace) {
            if let Some(rec) = ns.records.get(id) {
                if is_visible(rec, self.store.now_unix, &opts) {
                    return serde_json::to_string(rec).unwrap();
                }
            }
        }
        "null".to_string()
    }

    pub fn list(&self, namespace: &str, filter_json: &str) -> String {
        #[derive(Debug, Deserialize, Default)]
        #[serde(deny_unknown_fields)]
        struct ListFilter {
            memory_type: Option<String>,
            domain: Option<String>,
            include_tasks: Option<bool>,
            include_archived: Option<bool>,
        }
        let filter: ListFilter = if filter_json.trim().is_empty() {
            ListFilter { memory_type: None, domain: None, include_tasks: None, include_archived: None }
        } else {
            match serde_json::from_str(filter_json) {
                Ok(f) => f,
                Err(_) => return serde_json::to_string(&LoadReport { ok: false, loaded: 0, skipped: 0, warnings: vec![], error: Some("invalid filter JSON".to_string()) }).unwrap(),
            }
        };
        let opts = ReadOptions {
            include_tasks: filter.include_tasks.unwrap_or(false),
            include_archived: filter.include_archived.unwrap_or(false),
            include_structural: false,
        };
        if let Some(ns) = self.store.namespaces.get(namespace) {
            let mut matches: Vec<&SnapshotRecord> = ns.records.values()
                .filter(|rec| is_visible(rec, self.store.now_unix, &opts))
                .filter(|rec| {
                    if let Some(mt) = &filter.memory_type { rec.payload.memory_type == *mt } else { true }
                })
                .filter(|rec| {
                    if let Some(dom) = &filter.domain {
                        rec.payload.metadata.get("domain").and_then(|v| v.as_str()).map_or(false, |s| s == dom)
                    } else { true }
                })
                .collect();
            matches.sort_by(|a, b| a.id.cmp(&b.id));
            // Return array of records (serialize owned copies)
            let owned: Vec<SnapshotRecord> = matches.into_iter().cloned().collect();
            return serde_json::to_string(&owned).unwrap();
        }
        "[]".to_string()
    }

    pub fn neighbors(&self, namespace: &str, id: &str, relation: &str, direction: &str, max_hops: usize, limit: usize) -> String {
        let allowed = if relation.trim().is_empty() {
            related::DEFAULT_RELATIONS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        } else {
            if !related::is_allowed_relation(relation) {
                return r#"{"error":"invalid relation"}"#.to_string();
            }
            vec![relation.to_string()]
        };
        // Accept the common shorthand before delegating; unknown values still
        // degrade to `Both` inside parse_direction.
        let norm_dir = match direction.trim() {
            "out" => "outgoing",
            "in" => "incoming",
            other => other,
        };
        let dir = related::parse_direction(Some(norm_dir));
        let ns = match self.store.namespaces.get(namespace) {
            Some(n) => n,
            None => return r#"{"error":"unknown namespace"}"#.to_string(),
        };
        let opts = ReadOptions { include_tasks: true, include_archived: true, include_structural: true };
        let mut nodes: Vec<related::NodeSpec> = Vec::new();
        let mut edges: Vec<related::EdgeSpec> = Vec::new();
        for (_id, rec) in &ns.records {
            let visible = is_visible(rec, self.store.now_unix, &opts);
            nodes.push(related::NodeSpec {
                id: rec.id.clone(),
                memory_type: rec.payload.memory_type.clone(),
                content: rec.payload.content.clone(),
                domain: rec.payload.metadata.get("domain").and_then(|v| v.as_str()).map(String::from),
                freshness: rec.payload.metadata.get("freshness").and_then(|v| v.as_str()).map(String::from),
                lifecycle: rec.payload.metadata.get("lifecycle").and_then(|v| v.as_str()).map(String::from),
                visible,
            });
        }
        for edge in &ns.edges {
            edges.push(related::EdgeSpec {
                source: edge.source.clone(),
                relation: edge.relation.clone(),
                target: edge.target.clone(),
                valid_from: 0,
                valid_to: 0,
            });
        }
        let result = related::graph_read_related(
            id,
            &nodes,
            &edges,
            &allowed,
            dir,
            max_hops.min(related::MAX_HOPS).max(1),
            &[],
            limit.min(related::MAX_LIMIT).max(1),
            0,
            None,
        );
        serde_json::to_string(&result).unwrap()
    }

    pub fn namespaces(&self) -> String {
        let mut names: Vec<String> = self.store.namespaces.keys().cloned().collect();
        names.sort();
        serde_json::to_string(&names).unwrap()
    }

    pub fn vector_search(&self, namespace: &str, query_vec: &[f32], embedding_id: &str, limit: usize, include_tasks: bool, include_archived: bool) -> String {
        let ns = match self.store.namespaces.get(namespace) {
            Some(n) => n,
            None => return "[]".to_string(),
        };
        if ns.vector_dims.is_none() {
            return "[]".to_string();
        }
        let expected_dims = ns.vector_dims.unwrap();
        if query_vec.len() != expected_dims {
            return serde_json::to_string(&LoadReport { ok: false, loaded: 0, skipped: 0, warnings: vec![], error: Some(format!("vector dimension mismatch: expected {}, got {}", expected_dims, query_vec.len())) }).unwrap();
        }
        if ns.embedding_id.as_deref() != Some(embedding_id) && !embedding_id.is_empty() && ns.embedding_id.is_some() {
            return serde_json::to_string(&LoadReport { ok: false, loaded: 0, skipped: 0, warnings: vec![], error: Some("embedding mismatch".to_string()) }).unwrap();
        }
        let clamped_limit = limit.max(1).min(100);
        let opts = ReadOptions { include_tasks, include_archived, include_structural: false };
        let mut hits: Vec<SearchHit> = Vec::new();
        for (_id, rec) in &ns.records {
            if !is_visible(rec, self.store.now_unix, &opts) {
                continue;
            }
            if let Some(vec) = &rec.vector {
                if vec.len() == expected_dims {
                    let score = cosine_similarity(query_vec, vec);
                    hits.push(SearchHit { id: rec.id.clone(), score });
                }
            }
        }
        hits.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap().then_with(|| a.id.cmp(&b.id))
        });
        hits.truncate(clamped_limit);
        serde_json::to_string(&hits).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(id: &str, ns: &str, content: &str, memory_type: &str) -> SnapshotRecord {
        SnapshotRecord {
            id: id.to_string(),
            namespace: ns.to_string(),
            payload: SnapshotPayload {
                content: content.to_string(),
                user_id: "u".to_string(),
                memory_type: memory_type.to_string(),
                agent_name: "a".to_string(),
                location: "l".to_string(),
                location_lines: "1".to_string(),
                metadata: serde_json::json!({}),
            },
            vector: None,
            embedding_id: None,
        }
    }

    #[test]
    fn load_version_reject() {
        let file = SnapshotFile { version: 99, records: vec![], edges: vec![] };
        assert!(WasmStore::from_snapshot(file, 0).is_err());
    }

    #[test]
    fn duplicate_id_warning() {
        let rec = sample_record("x", "ns", "hello", "rule");
        let file = SnapshotFile { version: 1, records: vec![rec.clone(), rec], edges: vec![] };
        let (_, report) = WasmStore::from_snapshot(file, 0).unwrap();
        assert!(report.ok);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].reason, "DuplicateId");
        assert_eq!(report.loaded, 1); // first loaded, second replaces, not double-counted
    }

    #[test]
    fn dimension_mismatch() {
        let mut rec = sample_record("x", "ns", "hello", "rule");
        rec.vector = Some(vec![1.0, 2.0]);
        let mut rec2 = sample_record("y", "ns", "hello", "rule");
        rec2.vector = Some(vec![1.0, 2.0, 3.0]);
        let file = SnapshotFile { version: 1, records: vec![rec, rec2], edges: vec![] };
        let (_, report) = WasmStore::from_snapshot(file, 0).unwrap();
        assert!(report.skipped >= 1);
        assert!(report.warnings.iter().any(|w| w.reason == "DimensionMismatch"));
    }

    #[test]
    fn embedding_mismatch() {
        let mut rec = sample_record("x", "ns", "hello", "rule");
        rec.vector = Some(vec![1.0]);
        rec.embedding_id = Some("a".to_string());
        let mut rec2 = sample_record("y", "ns", "hello", "rule");
        rec2.vector = Some(vec![1.0]);
        rec2.embedding_id = Some("b".to_string());
        let file = SnapshotFile { version: 1, records: vec![rec, rec2], edges: vec![] };
        let (_, report) = WasmStore::from_snapshot(file, 0).unwrap();
        assert!(report.skipped >= 1);
        assert!(report.warnings.iter().any(|w| w.reason == "EmbeddingMismatch"));
    }

    #[test]
    fn is_visible_filters() {
        let mut rec = sample_record("r", "ns", "hello", "rule");
        rec.payload.metadata = serde_json::json!({ "valid_to": 100 });
        assert!(!is_visible(&rec, 100, &ReadOptions::default()));
        rec.payload.metadata = serde_json::json!({ "archived": true });
        assert!(!is_visible(&rec, 0, &ReadOptions { include_archived: false, ..Default::default() }));
        assert!(is_visible(&rec, 0, &ReadOptions { include_archived: true, ..Default::default() }));
        rec.payload.metadata = serde_json::json!({});
        rec.payload.memory_type = "task".to_string();
        assert!(!is_visible(&rec, 0, &ReadOptions::default()));
        assert!(is_visible(&rec, 0, &ReadOptions { include_tasks: true, ..Default::default() }));
    }

    #[test]
    fn cosine_similarity_known() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 0.001);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 0.001);
        assert!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]) == 0.0);
    }

    #[test]
    fn list_unknown_filter_field() {
        let engine = WasmEngine::new(0.0);
        let res = engine.list("ns", r#"{"badfield":"x"}"#);
        assert!(res.contains("error"));
    }

    #[test]
    fn wasm_engine_load_snapshot_and_namespaces_and_get_memory() {
        let mut engine = WasmEngine::new(0.0);
        let json = r#"{"version":1,"records":[{"id":"m1","namespace":"ns","payload":{"content":"hello world","user_id":"u","memory_type":"rule","agent_name":"a","location":"l","location_lines":"1","metadata":{}},"vector":[1.0,2.0],"embedding_id":"e1"},{"id":"m2","namespace":"ns2","payload":{"content":"another","user_id":"u","memory_type":"fact","agent_name":"a","location":"l","location_lines":"1","metadata":{}},"vector":null}],"edges":[]}"#;
        let report = engine.load_snapshot(json);
        assert!(report.contains("\"ok\":true"), "load report: {report}");
        let ns = engine.namespaces();
        assert!(ns.contains("ns"), "namespaces: {ns}");
        assert!(ns.contains("ns2"), "namespaces: {ns}");
        let mem = engine.get_memory("ns", "m1", false, false);
        assert!(mem.contains("m1"), "get_memory: {mem}");
        let missing = engine.get_memory("ns", "nope", false, false);
        assert_eq!(missing, "null");
    }

    #[test]
    fn wasm_engine_search_and_vector_search_and_neighbors() {
        let mut engine = WasmEngine::new(0.0);
        let json = r#"{"version":1,"records":[{"id":"m1","namespace":"ns","payload":{"content":"podman container","user_id":"u","memory_type":"rule","agent_name":"a","location":"l","location_lines":"1","metadata":{}},"vector":[1.0,0.0],"embedding_id":"e1"},{"id":"m2","namespace":"ns","payload":{"content":"docker container","user_id":"u","memory_type":"rule","agent_name":"a","location":"l","location_lines":"1","metadata":{}},"vector":[0.0,1.0],"embedding_id":"e1"}],"edges":[{"source":"m1","relation":"GOVERNS","target":"m2"}]}"#;
        engine.load_snapshot(json);
        let search = engine.search("ns", "podman", 10, false, false);
        assert!(search.contains("m1"), "search: {search}");
        let vs = engine.vector_search("ns", &[1.0, 0.0], "e1", 10, false, false);
        assert!(vs.contains("m1"), "vector_search: {vs}");
        let nb = engine.neighbors("ns", "m1", "", "out", 1, 10);
        assert!(nb.contains("m2"), "neighbors: {nb}");
    }

    #[test]
    fn wasm_engine_neighbors_direction_shorthand_out_in() {
        let mut engine = WasmEngine::new(0.0);
        let json = r#"{"version":1,"records":[{"id":"m1","namespace":"ns","payload":{"content":"node a","user_id":"u","memory_type":"rule","agent_name":"a","location":"l","location_lines":"1","metadata":{}},"vector":null},{"id":"m2","namespace":"ns","payload":{"content":"node b","user_id":"u","memory_type":"rule","agent_name":"a","location":"l","location_lines":"1","metadata":{}},"vector":null}],"edges":[{"source":"m1","relation":"GOVERNS","target":"m2"}]}"#;
        let report = engine.load_snapshot(json);
        assert!(report.contains("\"ok\":true"), "load report: {report}");
        // Directed edge m1 GOVERNS m2: "out" from m1 traverses to m2.
        let nb_out = engine.neighbors("ns", "m1", "", "out", 1, 10);
        assert!(nb_out.contains(r#""id":"m2""#), "neighbors(out): {nb_out}");
        // RELATES_TO is the only undirected relation, so GOVERNS honors
        // direction: "in" from m1 must reach nothing beyond the seed node.
        let nb_in = engine.neighbors("ns", "m1", "", "in", 1, 10);
        assert!(!nb_in.contains(r#""id":"m2""#), "neighbors(in) from m1 must not reach m2, got: {nb_in}");
        // Symmetry check from the target side.
        let nb_out_from_m2 = engine.neighbors("ns", "m2", "", "out", 1, 10);
        assert!(!nb_out_from_m2.contains(r#""id":"m1""#), "neighbors(out) from m2 must not reach m1, got: {nb_out_from_m2}");
        let nb_in_target = engine.neighbors("ns", "m2", "", "in", 1, 10);
        assert!(nb_in_target.contains(r#""id":"m1""#), "neighbors(in) from m2: {nb_in_target}");
    }

    #[test]
    fn fully_skipped_namespace_not_listed() {
        // Every record in this namespace is skipped (empty vector =>
        // DimensionMismatch), so the namespace must never appear.
        let mut rec = sample_record("x", "skipped-ns", "hello", "rule");
        rec.vector = Some(vec![]);
        let file = SnapshotFile { version: 1, records: vec![rec], edges: vec![] };
        let (store, report) = WasmStore::from_snapshot(file, 0).unwrap();
        assert_eq!(report.skipped, 1);
        assert!(!store.namespaces.contains_key("skipped-ns"),
            "store namespaces: {:?}", store.namespaces.keys());
        let mut engine = WasmEngine::new(0.0);
        let json = r#"{"version":1,"records":[{"id":"x","namespace":"skipped-ns","payload":{"content":"hello","user_id":"u","memory_type":"rule","agent_name":"a","location":"l","location_lines":"1","metadata":{}},"vector":[]}],"edges":[]}"#;
        engine.load_snapshot(json);
        let ns = engine.namespaces();
        assert_eq!(ns, "[]", "namespaces: {ns}");
    }

}
