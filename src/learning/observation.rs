use crate::traits::MemoryPayload;
use crate::judgment::policy::DedupDecision;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningContext {
    pub connection_id: String,
    pub active_task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PayloadHash(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSource {
    RawDistance,
    ExactContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedCandidate {
    pub candidate_id: String,
    pub payload_hash: PayloadHash,
    pub source: CandidateSource,
    pub raw_distance: Option<f32>,
    pub provider_confidence: Option<f32>,
    pub shadow_prediction: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionObservation {
    pub schema_version: u32,
    pub observation_id: String,
    pub occurred_at: i64,
    pub namespace: String,
    pub context: LearningContext,
    pub result_memory_id: Option<String>,
    pub input_payload_hash: PayloadHash,
    pub candidates: Vec<ObservedCandidate>,
    pub embedder_id: String,
    pub embedder_dimensions: usize,
    pub selected_provider: String,
    pub policy_version: String,
    pub model_version: Option<String>,
    pub shadow_provider: Option<String>,
    pub shadow_model_version: Option<String>,
    pub decision: DedupDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningOutcome {
    pub outcome_id: String,
    pub occurred_at: i64,
    pub namespace: String,
    pub connection_id: Option<String>,
    pub task_id: Option<String>,
    pub target_ids: Vec<String>,
    pub kind: LearningOutcomeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningOutcomeKind {
    MemoryCreated,
    MemorySuperseded,
    MemoryArchived,
    TaskCompleted,
    MemoryInspected,
    ProcedurePerformed,
}

pub struct LearningRecorder { root: std::path::PathBuf }
impl LearningRecorder {
    pub async fn open(root: std::path::PathBuf) -> Result<Self> {
        // placeholder
        unimplemented!("TODO: create .NeuroStrata/learning directory, initialize JSONL files")
    }
    pub async fn record_observation(&self, _value: &DecisionObservation) -> Result<()> {
        unimplemented!("TODO: implement observation writing")
    }
    pub async fn record_outcome(&self, _value: &LearningOutcome) -> Result<()> {
        unimplemented!("TODO: implement outcome writing")
    }
    pub async fn put_payload(&self, _value: &MemoryPayload) -> Result<PayloadHash> {
        unimplemented!("TODO: implement payload hashing + blob storage")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use std::collections::HashMap;

    #[test]
    fn failing_blob_reuse_content_addressed() {
        // Same content payload should produce same hash -> same blob record
        // This test fails until the BlobStore is implemented.
        let payload1 = MemoryPayload {
            content: "hello".to_string(),
            user_id: "u".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "src/lib.rs".to_string(),
            location_lines: "1-2".to_string(),
            metadata: serde_json::json!({}),
        };
        let mut store = HashMap::<PayloadHash, Vec<u8>>::new();
        // TODO: implement BlobStore.put(payload) -> PayloadHash
        // assert!(store.is_empty()); // before putting
        // let hash1 = BlobStore::put(payload1);
        // assert_eq!(hash1.0, "sha256:...".to_string()); // placeholder
        // let hash2 = BlobStore::put(payload1);
        // assert_eq!(hash1, hash2, "same payload should reuse blob");
        panic!("TODO: implement blob store with Sha256 hashing and reuse")
    }

    #[test]
    fn observation_jsonl_append_fail() {
        let recorder = LearningRecorder { root: std::path::PathBuf::from("/tmp/test_obs") };
        // TODO: test JSONL append with reopen, line-atomic writes.
        panic!("TODO: implement JSONL file handling")
    }
}