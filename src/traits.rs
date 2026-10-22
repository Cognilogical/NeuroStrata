use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The shipped vocabulary schema, embedded at compile time. v2 adds the
/// `task` memory type and the EXTRACTED_FROM relation that carries the
/// extraction edge behind the completion gate (Lock 2).
pub const MEMORY_VOCABULARY_JSON: &str = include_str!("schemas/memory-vocabulary.v3.json");

/// Parsed vocabulary, loaded once on first access.
pub fn memory_vocabulary() -> &'static serde_json::Value {
    static VOCAB: OnceLock<serde_json::Value> = OnceLock::new();
    VOCAB.get_or_init(|| {
        serde_json::from_str(MEMORY_VOCABULARY_JSON).expect("shipped memory vocabulary parses")
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

/// FEATURE-4: does this memory say where it came from?
///
/// A memory is an assertion until it names its origin, so `source` is the
/// difference between a claim you can audit and one you can only take on
/// faith. It is deliberately total over every shape the field legitimately
/// takes: absent, JSON null, and a blank string all read as unsourced, while a
/// non-empty string ("owner 2026-10-08") or a structured object
/// ({kind, ref, captured_at}) read as sourced.
///
/// The task gate fails rules without one and the search ranker rewards them.
/// They must agree on that line, so both call this rather than each carrying a
/// copy -- a gate that demands provenance the ranker cannot see is a rule nobody
/// can satisfy.
pub fn has_source(metadata: &Value) -> bool {
    metadata
        .get("source")
        .map(|s| !s.is_null() && (s.as_str().map(|x| !x.trim().is_empty()).unwrap_or(true)))
        .unwrap_or(false)
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
    #[serde(rename = "episode_sibling")]
    EpisodeSibling,
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

/// Returns true when the evidence carries a DirectMatch — the shared
/// predicate used by both the recall path (buffer.rs) and the search
/// handler's access-count bump (server.rs). One definition, two callers,
/// so they cannot drift.
pub fn is_direct_match(evidence: &Option<Vec<SearchEvidence>>) -> bool {
    evidence.as_ref().map_or(false, |evs| {
        evs.iter()
            .any(|e| e.kind == EvidenceKind::DirectMatch)
    })
}

/// How the domain filter affected the search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainMatchMode {
    /// The rows were chosen by the domain filter.
    Domain,
    /// The rows were chosen without the domain filter: no key set was given,
    /// or the filter found nothing and was skipped.
    Vector,
}

/// The outcome of a domain-filtered search.
#[derive(Debug, Clone)]
pub struct DomainSearchOutcome {
    pub results: Vec<SearchResult>,
    pub mode: DomainMatchMode,
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

    /// Stable identity of the embedder (model + provider). Used to scope
    /// raw-distance dedup candidates and observations. Defaults to "unknown"
    /// for test stubs; production embedders override with a real id.
    fn embedder_id(&self) -> &str {
        "unknown"
    }
}

#[derive(Debug, Clone)]
pub enum TargetResolution {
    Resolved(String),
    Missing,
    Ambiguous(Vec<String>),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct EdgeResolutionStatus {
    pub rel_type: String,
    pub declared_target: String,
    pub declaration_index: usize,
    pub resolution: TargetResolution,
}

#[derive(Debug, Clone)]
pub struct UpsertReport {
    pub id: String,
    pub deferred_edges: Vec<EdgeResolutionStatus>,
}

/// A candidate returned by the unboosted raw-distance dedup query.
#[derive(Debug, Clone)]
pub struct RawDistanceCandidate {
    pub id: String,
    pub raw_distance: f32,
    pub payload: MemoryPayload,
}

/// A candidate returned by the exact-content dedup lookup.
#[derive(Debug, Clone)]
pub struct ExactContentCandidate {
    pub id: String,
    pub payload: MemoryPayload,
}

/// Scope for a dedup candidate query.
#[derive(Debug, Clone)]
pub enum DedupCandidateScope {
    Domain,
    Procedure { trigger: String },
}

/// Query parameters for `nearest_by_raw_distance`.
#[derive(Debug, Clone)]
pub struct RawDistanceQuery {
    pub limit: usize,
    pub scope: DedupCandidateScope,
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
    ) -> Result<UpsertReport>;

    /// Search for the closest memories to a given vector.
    ///
    /// Returns up to `limit` recall-eligible primary matches PLUS their bounded
    /// graph expansions. Row-class filtering (tasks, archived, mesh conflicts)
    /// is applied BEFORE the candidate budget is spent.
    async fn search(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
    ) -> Result<Vec<SearchResult>>;

    /// Unboosted raw-distance dedup candidate query. Returns up to `query.limit`
    /// active domain-memory candidates ordered by the unboosted distance metric.
    /// Default: empty (stores that cannot filter stay honest).
    async fn nearest_by_raw_distance(
        &self,
        _namespace: &str,
        _vector: Vec<f32>,
        _query: RawDistanceQuery,
    ) -> Result<Vec<RawDistanceCandidate>> {
        Ok(Vec::new())
    }

    /// Exact UTF-8 content lookup over eligible active rows, independent of
    /// vector rank. Default: empty.
    async fn find_active_exact_content(
        &self,
        _namespace: &str,
        _exact_content: &str,
        _scope: DedupCandidateScope,
    ) -> Result<Vec<ExactContentCandidate>> {
        Ok(Vec::new())
    }

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
    /// their incident edges are excluded from the view. When
    /// `include_archived` is false (the default), tombstoned rows
    /// (`metadata.archived == true`) and their incident edges are excluded.
    async fn export_graph(
        &self,
        include_retired: bool,
        include_archived: bool,
        project_contexts: &HashMap<String, PathBuf>,
        anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
    ) -> Result<serde_json::Value>;

    /// Increment the access count of a specific memory by its ID.
    async fn increment_access_count(&self, namespace: &str, id: &str) -> Result<()>;

    /// Search with an optional domain key set. The default implementation ignores
    /// the key set and reports `Vector`, so a store that cannot filter stays honest.
    async fn search_with_domain(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
        include_tasks: bool,
        include_archived: bool,
        _domains: &[String],
    ) -> Result<DomainSearchOutcome> {
        let results = self
            .search(namespace, vector, limit, include_tasks, include_archived)
            .await?;
        Ok(DomainSearchOutcome {
            results,
            mode: DomainMatchMode::Vector,
        })
    }

    /// Write a self-contained snapshot of the store into `dir`.
    ///
    /// This is a checkpointed file copy, not the engine's EXPORT DATABASE:
    /// lbug 0.20.4 SIGSEGVs planning EXPORT DATABASE on every store (gdb:
    /// planExportTableData -> std::__format on a dangling string_view), so the
    /// SQL export path cannot be used. The store is a single file whose only
    /// writer is one daemon, so a copy taken right after CHECKPOINT is a
    /// consistent snapshot.
    async fn export_database(&self, dir: &str) -> Result<()>;

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
    use serde_json::json;

    /// The relation-count assertion moved here from the v1 test: v2 declares
    /// exactly four relations, the fourth being the extraction edge. v3 keeps
    /// them and adds the rule-honesty fields (A6); v4 adds `procedure` and
    /// keeps every relation as it was; v5 adds the fact `topology` field,
    /// which is a field and not a relation; v6 adds the fact `attempt` and `domain_registry` fields.
    #[test]
    fn memory_vocabulary_v6_parses_and_has_exact_relations() {
        let v = memory_vocabulary();
        assert_eq!(v["vocabulary_version"], 6);
        let rels = v["relations"].as_object().unwrap();
        assert!(rels.contains_key("GOVERNS"));
        assert!(rels.contains_key("CONTAINS"));
        assert!(rels.contains_key("RELATES_TO"));
        assert!(rels.contains_key("EXTRACTED_FROM"));
        assert_eq!(rels.len(), 4);
        assert_eq!(rels["GOVERNS"]["direction"], "directed");
        assert_eq!(rels["CONTAINS"]["direction"], "directed");
        assert_eq!(rels["RELATES_TO"]["direction"], "undirected");
        assert_eq!(rels["EXTRACTED_FROM"]["direction"], "directed");
        assert_eq!(rels["EXTRACTED_FROM"]["source_role"], "memory");
        assert_eq!(rels["EXTRACTED_FROM"]["target_role"], "task");
        assert_eq!(
            rels["EXTRACTED_FROM"]["declaration_direction"],
            "self_to_target"
        );
        assert_eq!(rels["EXTRACTED_FROM"]["metadata_key"], "extracted_from");
        let fact = &v["memory_types"]["fact"]["fields"];
        assert!(
            fact.get("attempt").is_some(),
            "the fact declares the attempt shape: {v}"
        );
        assert!(
            fact.get("domain_registry").is_some(),
            "the fact declares the registry shape: {v}"
        );
        assert_eq!(fact["attempt"]["type"], "object");
        assert_eq!(fact["domain_registry"]["type"], "object");
    }

    #[test]
    fn memory_vocabulary_declares_task_as_a_non_structural_type() {
        let v = memory_vocabulary();
        let types = v["memory_types"].as_object().unwrap();
        assert!(types.contains_key("task"));
        assert!(!types["task"]["structural"].as_bool().unwrap());
    }

    /// A6 rule honesty (v3): a rule declares whether any machine enforces it.
    #[test]
    fn memory_vocabulary_rule_declares_enforcement_fields() {
        let v = memory_vocabulary();
        let fields = &v["memory_types"]["rule"]["fields"];
        assert_eq!(
            fields["enforcement"]["enum"],
            serde_json::json!(["ENFORCED", "PARTIAL", "NOT_ENFORCED"])
        );
        assert!(fields["source"].is_object());
        assert!(fields["guard"].is_object());
    }

    /// v4 procedural memory: a non-structural type carrying all six of its
    /// fields. The trigger enum lives here, in the vocabulary, rather than in
    /// wiring.rs's closed `fires_on` registry -- a procedure is data an
    /// operator writes, not code.
    #[test]
    fn memory_vocabulary_declares_procedure_with_its_six_fields() {
        let v = memory_vocabulary();
        let procedure = &v["memory_types"]["procedure"];
        assert!(!procedure["structural"].as_bool().unwrap());
        let fields = procedure["fields"].as_object().unwrap();
        for field in [
            "trigger",
            "remaining_fires",
            "valid_to",
            "last_performed_at",
            "performance_count",
            "last_episodic_pointer",
        ] {
            assert!(fields.contains_key(field), "procedure declares {}", field);
        }
        assert_eq!(
            fields["trigger"]["enum"],
            json!([
                "session-start",
                "before-edit",
                "after-mutation",
                "every-n-sessions:N",
                "every-n-days:N"
            ])
        );
        for nullable in ["remaining_fires", "valid_to", "last_performed_at"] {
            assert_eq!(
                fields[nullable]["type"],
                json!(["integer", "null"]),
                "{} is nullable",
                nullable
            );
        }
        assert_eq!(fields["performance_count"]["type"], json!("integer"));
        assert_eq!(
            fields["last_episodic_pointer"]["type"],
            json!(["string", "null"])
        );
        // The memory_type summary is what tools/list shows an agent, so it has
        // to name the new type.
        assert!(v["tool_summaries"]["memory_type"]
            .as_str()
            .unwrap()
            .contains("procedure"));
    }

    /// v5 access topology: a fact declares the access path it describes, so a
    /// reader can see which component reaches which system over which
    /// channel, and which boundary that channel crosses.
    #[test]
    fn memory_vocabulary_declares_the_topology_field_on_a_fact() {
        let v = memory_vocabulary();
        let topology = &v["memory_types"]["fact"]["fields"]["topology"];
        assert_eq!(topology["type"], "object");
        let definition = topology["definition"]
            .as_str()
            .expect("the topology field carries a definition");
        for key in [
            "component",
            "target_system",
            "channel",
            "credential_boundary",
            "reason",
        ] {
            assert!(
                definition.contains(key),
                "the topology definition names {}",
                key
            );
        }
        assert!(
            definition.contains("Extra keys are ignored."),
            "the topology definition states what happens to an extra key"
        );
        // The summaries are how the agent discovers the shape, so both of
        // them have to carry it.
        for key in ["add", "memory_type"] {
            let summary = v["tool_summaries"][key].as_str().unwrap();
            assert!(
                summary.contains("metadata.topology"),
                "the {} summary mentions metadata.topology",
                key
            );
            assert!(
                summary.contains("component, target_system, channel, credential_boundary, reason"),
                "the {} summary names the five topology keys",
                key
            );
        }
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
                content: "c".into(),
                user_id: "u".into(),
                memory_type: "rule".into(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: serde_json::json!({}),
            },
            evidence: Some(vec![SearchEvidence {
                kind: EvidenceKind::DirectMatch,
                path: vec![],
                explanation: "test".into(),
            }]),
        };
        let json = serde_json::to_value(&sr).unwrap();
        assert!(
            json.get("evidence").is_none(),
            "evidence must be skip-serialized"
        );
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
                EvidenceEdge {
                    source: "a".into(),
                    relation: "CONTAINS".into(),
                    target: "b".into(),
                },
                EvidenceEdge {
                    source: "c".into(),
                    relation: "GOVERNS".into(),
                    target: "b".into(),
                },
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

    /// The provenance line the gate enforces and the ranker rewards. Every shape
    /// the field legitimately takes has to land on the right side of it, and a
    /// blank has to be indistinguishable from absent -- otherwise padding a rule
    /// with `"source": ""` would satisfy neither.
    #[test]
    fn has_source_covers_every_shape_the_field_takes() {
        for absent in [json!({}), json!({ "kind": "rule" })] {
            assert!(
                !has_source(&absent),
                "absent source is not provenance: {}",
                absent
            );
        }
        for blank in [
            json!({ "source": null }),
            json!({ "source": "" }),
            json!({ "source": "  " }),
        ] {
            assert!(
                !has_source(&blank),
                "blank source is not provenance: {}",
                blank
            );
        }
        for real in [
            json!({ "source": "owner 2026-10-08" }),
            json!({ "source": { "kind": "derived", "ref": "task-wiot", "captured_at": "2026-10-09" } }),
            json!({ "source": ["a"] }),
        ] {
            assert!(has_source(&real), "real source is provenance: {}", real);
        }
    }
}
