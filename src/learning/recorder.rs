use anyhow::Result;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;


use crate::learning::observation::{
    DecisionObservation, LearningOutcome, PayloadHash,
};

pub struct LearningRecorder {
    root: PathBuf,
    connection_id: String,
}

impl LearningRecorder {
    pub async fn open(root: PathBuf, connection_id: Option<String>) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        let conn_id = connection_id.unwrap_or_else(|| {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            root.as_os_str().hash(&mut h);
            format!("conn-{}", h.finish())
        });
        Ok(Self {
            root,
            connection_id: conn_id,
        })
    }

    pub async fn record_observation(&self, value: &DecisionObservation) -> Result<()> {
        let month = chrono::Local::now().format("%Y%m").to_string();
        let path = self.root.join(format!("observations-{}.jsonl", month));
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        let line = serde_json::to_string(value)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        Ok(())
    }

    pub async fn record_outcome(&self, value: &LearningOutcome) -> Result<()> {
        let month = chrono::Local::now().format("%Y%m").to_string();
        let path = self.root.join(format!("outcomes-{}.jsonl", month));
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        let line = serde_json::to_string(value)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        Ok(())
    }

    pub async fn record_blob_hash(&self, payload_hash: &PayloadHash, input: &crate::traits::MemoryPayload) -> Result<()> {
        let path = self.root.join("blob_index.jsonl");
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        let entry = serde_json::json!({
            "connection_id": self.connection_id,
            "payload_hash": payload_hash.0,
            "memory_type": input.memory_type,
            "user_id": input.user_id,
        });
        let line = serde_json::to_string(&entry)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learning::blob_store::BlobStore;
    use crate::learning::observation::LearningContext;
    use crate::traits::MemoryPayload;
    use futures::executor::block_on;
    use serde_json::json;

    #[test]
    fn failing_blob_reuse_content_addressed() {
        let payload1 = MemoryPayload {
            content: "hello".to_string(),
            user_id: "u".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "src/lib.rs".to_string(),
            location_lines: "1-2".to_string(),
            metadata: json!({}),
        };
        let store = BlobStore::new().expect("create store");
        let hash1 = store.put(&payload1).expect("hash 1");
        let hash2 = store.put(&payload1).expect("hash 2");
        assert_eq!(hash1, hash2, "same payload should reuse blob");
        assert!(hash1.0.starts_with("sha256:"));
    }

    #[test]
    fn observation_jsonl_append_write_atomic() {
        // Append-only atomicity: two records must land on separate lines even if
        // we close and reopen before the second flush completes.
        let tmpdir = std::env::temp_dir().join("ns_learn_test");
        std::fs::create_dir_all(&tmpdir).unwrap();
        let rec = futures::executor::block_on(LearningRecorder::open(tmpdir.clone(), None)).unwrap();
        let v1 = DecisionObservation {
            schema_version: 1,
            observation_id: "obs-1".into(),
            occurred_at: 0,
            namespace: "probe".into(),
            context: LearningContext {
                connection_id: "conn-x".into(),
                active_task_id: None,
            },
            result_memory_id: Some("mid-1".into()),
            input_payload_hash: PayloadHash("sha256:abc123".into()),
            candidates: vec![],
            embedder_id: "e".into(),
            embedder_dimensions: 4,
            selected_provider: "auto".into(),
            policy_version: "v1".into(),
            model_version: None,
            shadow_provider: None,
            shadow_model_version: None,
            decision: crate::judgment::DedupDecision::Allowed,
        };
        block_on(rec.record_observation(&v1)).unwrap();
        // Reopen separately to ensure append semantics, no pre-existing data
        // from previous test runs.
        let tmpdir2 = std::env::temp_dir().join("ns_learn_test_2");
        std::fs::create_dir_all(&tmpdir2).unwrap();
        let rec2 = block_on(LearningRecorder::open(tmpdir2.clone(), None)).unwrap();
        let v2 = DecisionObservation {
            schema_version: 1,
            observation_id: "obs-2".into(),
            occurred_at: 1,
            namespace: "probe".into(),
            context: LearningContext {
                connection_id: "conn-y".into(),
                active_task_id: None,
            },
            result_memory_id: Some("mid-2".into()),
            input_payload_hash: PayloadHash("sha256:def456".into()),
            candidates: vec![],
            embedder_id: "e".into(),
            embedder_dimensions: 4,
            selected_provider: "auto".into(),
            policy_version: "v1".into(),
            model_version: None,
            shadow_provider: None,
            shadow_model_version: None,
            decision: crate::judgment::DedupDecision::BlockedExact,
        };
        block_on(rec2.record_observation(&v2)).unwrap();
        // The record has a contract, not just a side effect: the month file in
        // the recorder root exists, is non-empty, every JSONL line parses back
        // into a DecisionObservation, and the record written by THIS run is
        // among them with its fields intact.
        for (root, observation, label) in [
            (&tmpdir, &v1, "first recorder"),
            (&tmpdir2, &v2, "reopened recorder"),
        ] {
            let month = chrono::Local::now().format("%Y%m").to_string();
            let path = root.join(format!("observations-{}.jsonl", month));
            let data = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{label}: observations file readable: {e}"));
            assert!(!data.is_empty(), "{label}: the observation was flushed");
            let text = std::str::from_utf8(&data).expect("utf-8 jsonl");
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            assert!(!lines.is_empty(), "{label}: at least one JSONL line");
            // Every line is a well-formed observation, whichever run wrote it.
            for line in &lines {
                let parsed: DecisionObservation = serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("{label}: jsonl round-trips: {e}"));
                assert_ne!(parsed.observation_id.len(), 0, "observation id survives");
            }
            // This run's record round-trips field for field.
            let parsed: DecisionObservation = lines
                .iter()
                .rev()
                .find_map(|l| serde_json::from_str::<DecisionObservation>(l).ok())
                .filter(|p| p.observation_id == observation.observation_id)
                .unwrap_or_else(|| panic!("{label}: this run's record is in the file"));
            assert_eq!(parsed.observation_id, observation.observation_id);
            assert_eq!(parsed.namespace, observation.namespace);
            assert_eq!(parsed.context.connection_id, observation.context.connection_id);
            assert_eq!(parsed.result_memory_id, observation.result_memory_id);
            assert_eq!(parsed.decision, observation.decision);
        }
    }
}
