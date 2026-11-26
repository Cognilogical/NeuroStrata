use anyhow::Result;
use std::collections::HashMap;
use std::sync::Mutex;

use sha2::Digest;

use crate::learning::observation::PayloadHash;
use crate::traits::MemoryPayload;
use crate::util::lock_or_err;

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
        let mut cache = match lock_or_err(&self.cache) {
            Ok(cache) => cache,
            Err(error) => {
                tracing::error!("BlobStore::put: blob cache mutex is poisoned: {error}");
                return Err(error.into());
            }
        };
        cache.insert(id.clone(), payload.content.as_bytes().to_vec());
        Ok(id)
    }

    /// Returns the cached payload bytes for a previously stored hash.
    pub fn get(&self, hash: &PayloadHash) -> Option<Vec<u8>> {
        match lock_or_err(&self.cache) {
            Ok(cache) => cache.get(hash).cloned(),
            Err(error) => {
                tracing::error!("BlobStore::get: blob cache mutex is poisoned: {error}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;
    use serde_json::json;
    use std::sync::Arc;

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

    /// A cache lock poisoned by a panicking writer must fail the write loudly
    /// and degrade the read to None. Recovering the half-mutated map would
    /// pretend the failed mutation completed coherently.
    #[test]
    fn poisoned_lock_returns_error_not_panic() {
        let store = Arc::new(BlobStore::new().expect("create store"));
        let poisoner = Arc::clone(&store);
        let handle = std::thread::spawn(move || {
            let _guard = poisoner.cache.lock().expect("cache lock");
            panic!("poison the blob cache on purpose");
        });
        assert!(handle.join().is_err(), "the poisoner must unwind");

        let payload = MemoryPayload {
            content: "poisoned".to_string(),
            user_id: "u".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: "test".to_string(),
            location_lines: "1".to_string(),
            metadata: json!({}),
        };
        let error = store
            .put(&payload)
            .expect_err("put must propagate the poisoned-lock error");
        assert!(
            error.to_string().contains("poisoned"),
            "put error must name poisoning: {error}"
        );
        assert_eq!(
            store.get(&PayloadHash("sha256:any".to_string())),
            None,
            "get must degrade to None rather than adopt the recovered map"
        );
    }
}
