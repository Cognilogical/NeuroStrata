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
        
        // SHA-256 of the content string
        let mut hasher = sha2::Sha256::default();
        hasher.update(payload.content.as_bytes());
        let result = hasher.finalize();
        let id = PayloadHash(format!("sha256:{}", hex::encode(result)));
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), payload.content.as_bytes().to_vec());
        Ok(id)
    }

    /// Returns the cached payload bytes for a previously stored hash.
    pub fn get(&self, hash: &PayloadHash) -> Option<Vec<u8>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(hash).cloned()
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
        // get() must return the cached payload bytes (poison-tolerant lock).
        assert_eq!(
            store.get(&hash1).as_deref(),
            Some(payload.content.as_bytes()),
            "get must return cached payload for a stored hash"
        );
        assert_eq!(
            store.get(&PayloadHash("sha256:absent".to_string())),
            None,
            "get must return None for an unknown hash"
        );
    }
}