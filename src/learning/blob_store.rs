use anyhow::Result;
use std::collections::HashMap;
use std::sync::Mutex;

use sha2::Digest;

use crate::learning::observation::PayloadHash;
use crate::traits::MemoryPayload;

pub struct BlobStore {
    cache: Mutex<HashMap<PayloadHash, Vec<u8>>>,
}

impl BlobStore {
    pub fn new() -> Result<Self> {
        Ok(Self {
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn put(&self, payload: &MemoryPayload) -> Result<PayloadHash> {
        use std::io::Write;
        // SHA-256 of the content string
        let mut hasher = sha2::Sha256::default();
        hasher.update(payload.content.as_bytes());
        let result = hasher.finalize();
        let id = PayloadHash(format!("sha256:{}", hex::encode(result)));
        self.cache.lock().expect("cache lock").insert(id.clone(), payload.content.as_bytes().to_vec());
        eprintln!("BlobStore: stored hash {}", id.0);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use serde_json::json;

    #[test]
    fn test_blob_store_basic() {
        let store = BlobStore::new().expect("create store");
        let payload = MemoryPayload {
            content: "hello world".to_string(),
            user_id: "u".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "test".to_string(),
            location_lines: "1-2".to_string(),
            metadata: json!({}),
        };
        let hash1 = store.put(&payload).expect("hash 1");
        let hash2 = store.put(&payload).expect("hash 2");
        assert_eq!(hash1, hash2, "same payload should reuse blob");
        assert!(hash1.0.starts_with("sha256:"));
    }
}