use async_trait::async_trait;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataPoolAccessMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPoolSchema {
    pub connector_id: String,
    pub pool_id: String,
    pub access_mode: DataPoolAccessMode,
    pub entity_types: Vec<String>,
    pub max_slice_rows: u32,
    pub max_slice_bytes: u32,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPointerRecord {
    pub pointer_id: String,
    pub connector_id: String,
    pub pool_id: String,
    pub pointer_uri: String,
    pub entity_type: String,
    pub expected_source_sha256: Option<String>,
    pub captured_at_secs: u64,
    pub max_staleness_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSliceRequest {
    pub pointer_id: String,
    pub offset: u32,
    pub limit: u32,
    pub max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSlicePayload {
    pub pointer_id: String,
    pub connector_id: String,
    pub pool_id: String,
    pub pointer_uri: String,
    pub records: Vec<Value>,
    pub payload_bytes: u32,
    pub source_sha256: String,
    pub slice_sha256: String,
    pub observed_at_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEffectRequest {
    pub pointer_id: String,
    pub operation: String,
    pub payload: Value,
    pub action_receipt_id: String,
    pub expected_source_sha256: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEffectReceipt {
    pub pointer_id: String,
    pub connector_id: String,
    pub pool_id: String,
    pub operation: String,
    pub idempotency_key: String,
    pub applied: bool,
    pub prior_source_sha256: String,
    pub new_source_sha256: String,
    pub details: String,
}

#[async_trait]
pub trait DataConnector: Send + Sync {
    fn connector_id(&self) -> &str;
    async fn describe_pool(&self, pool_id: &str) -> Result<DataPoolSchema>;
    async fn register_pool(&self, schema: DataPoolSchema) -> Result<DataPoolSchema> {
        let _ = schema;
        Err(anyhow!(
            "connector '{}' does not support pool registration",
            self.connector_id()
        ))
    }
    async fn seed_records(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        records: Vec<Value>,
    ) -> Result<()> {
        let _ = (pool_id, pointer_uri, records);
        Ok(())
    }
    async fn resolve_slice(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        offset: u32,
        limit: u32,
        max_bytes: u32,
    ) -> Result<DataSlicePayload>;
    async fn apply_external_effect(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        req: &ExternalEffectRequest,
    ) -> Result<ExternalEffectReceipt>;
}

pub struct DataConnectorRegistry {
    connectors: RwLock<HashMap<String, Arc<dyn DataConnector>>>,
}

impl DataConnectorRegistry {
    pub fn new() -> Self {
        DataConnectorRegistry {
            connectors: RwLock::new(HashMap::new()),
        }
    }

    fn default_pool_schemas(connector_id: &str) -> [DataPoolSchema; 2] {
        [
            DataPoolSchema {
                connector_id: connector_id.to_string(),
                pool_id: "default".to_string(),
                access_mode: DataPoolAccessMode::ReadWrite,
                entity_types: vec!["json_record".to_string()],
                max_slice_rows: 1000,
                max_slice_bytes: 1_048_576,
                description: "Default read-write data pool".to_string(),
            },
            DataPoolSchema {
                connector_id: connector_id.to_string(),
                pool_id: "readonly".to_string(),
                access_mode: DataPoolAccessMode::ReadOnly,
                entity_types: vec!["json_record".to_string()],
                max_slice_rows: 1000,
                max_slice_bytes: 1_048_576,
                description: "Default read-only data pool".to_string(),
            },
        ]
    }

    fn load_configured_pool_schemas(workspace_root: &Path) -> Vec<DataPoolSchema> {
        let config_path = workspace_root.join(".NeuroStrata").join("connector_pools.json");
        let Ok(raw) = std::fs::read_to_string(&config_path) else {
            return Vec::new();
        };
        if let Ok(schemas) = serde_json::from_str::<Vec<DataPoolSchema>>(&raw) {
            return schemas;
        }
        if let Ok(wrapper) = serde_json::from_str::<Value>(&raw) {
            if let Some(arr) = wrapper.get("pools") {
                if let Ok(schemas) = serde_json::from_value::<Vec<DataPoolSchema>>(arr.clone()) {
                    return schemas;
                }
            }
        }
        Vec::new()
    }

    pub fn new_with_defaults(workspace_root: PathBuf) -> Self {
        let reg = DataConnectorRegistry::new();
        let inmem = Arc::new(InMemoryPoolConnector::new());
        let json = Arc::new(JsonFilePoolConnector::new(workspace_root.clone()));
        for schema in Self::default_pool_schemas(inmem.connector_id()) {
            inmem.upsert_pool(schema);
        }
        for schema in Self::default_pool_schemas(json.connector_id()) {
            json.register_pool(schema);
        }
        for schema in Self::load_configured_pool_schemas(&workspace_root) {
            if schema.connector_id == inmem.connector_id() {
                inmem.upsert_pool(schema);
            } else if schema.connector_id == json.connector_id() {
                json.register_pool(schema);
            }
        }
        reg.register(inmem).expect("register in-memory connector");
        reg.register(json).expect("register json-file connector");
        reg
    }

    pub fn register(&self, connector: Arc<dyn DataConnector>) -> Result<()> {
        let id = connector.connector_id().to_string();
        let mut connectors = self.connectors.write().unwrap();
        if connectors.contains_key(&id) {
            return Err(anyhow!("connector_id '{}' already registered", id));
        }
        connectors.insert(id, connector);
        Ok(())
    }

    pub fn get(&self, connector_id: &str) -> Option<Arc<dyn DataConnector>> {
        let connectors = self.connectors.read().unwrap();
        connectors.get(connector_id).cloned()
    }

    pub fn list_connector_ids(&self) -> Vec<String> {
        let connectors = self.connectors.read().unwrap();
        let mut ids: Vec<String> = connectors.keys().cloned().collect();
        ids.sort();
        ids
    }
}

impl Default for DataConnectorRegistry {
    fn default() -> Self {
        DataConnectorRegistry::new()
    }
}

struct InMemoryPoolEntry {
    schema: DataPoolSchema,
    records: HashMap<String, Vec<Value>>,
    idempotency: HashMap<String, ExternalEffectReceipt>,
}

pub struct InMemoryPoolConnector {
    id: String,
    pools: RwLock<HashMap<String, InMemoryPoolEntry>>,
}

impl InMemoryPoolConnector {
    pub fn new() -> Self {
        InMemoryPoolConnector {
            id: "memory_pool".to_string(),
            pools: RwLock::new(HashMap::new()),
        }
    }

    pub fn upsert_pool(&self, schema: DataPoolSchema) {
        let mut pools = self.pools.write().unwrap();
        let pool_id = schema.pool_id.clone();
        if let Some(entry) = pools.get_mut(&pool_id) {
            entry.schema = schema;
        } else {
            pools.insert(pool_id, InMemoryPoolEntry {
                schema,
                records: HashMap::new(),
                idempotency: HashMap::new(),
            });
        }
    }

    pub fn seed_records(&self, pool_id: &str, pointer_uri: &str, records: Vec<Value>) {
        let mut pools = self.pools.write().unwrap();
        let pool = pools
            .get_mut(pool_id)
            .expect("pool must be created via upsert_pool before seeding");
        pool.records.insert(pointer_uri.to_string(), records);
    }

    fn compute_source_sha256(records: &[Value]) -> String {
        let mut hasher = Sha256::new();
        for r in records {
            hasher.update(serde_json::to_vec(r).expect("serialize record"));
        }
        hex::encode(hasher.finalize())
    }

    fn compute_slice_sha256(records: &[Value]) -> String {
        let mut hasher = Sha256::new();
        for r in records {
            hasher.update(serde_json::to_vec(r).expect("serialize record"));
        }
        hex::encode(hasher.finalize())
    }
}

impl Default for InMemoryPoolConnector {
    fn default() -> Self {
        InMemoryPoolConnector::new()
    }
}

#[async_trait]
impl DataConnector for InMemoryPoolConnector {
    fn connector_id(&self) -> &str {
        &self.id
    }

    async fn describe_pool(&self, pool_id: &str) -> Result<DataPoolSchema> {
        let pools = self.pools.read().unwrap();
        match pools.get(pool_id) {
            Some(entry) => Ok(entry.schema.clone()),
            None => Err(anyhow!("pool '{}' not found", pool_id)),
        }
    }

    async fn register_pool(&self, mut schema: DataPoolSchema) -> Result<DataPoolSchema> {
        if schema.pool_id.trim().is_empty() {
            return Err(anyhow!("pool_id must not be empty"));
        }
        if schema.connector_id.trim().is_empty() {
            schema.connector_id = self.connector_id().to_string();
        } else if schema.connector_id != self.connector_id() {
            return Err(anyhow!(
                "schema connector_id '{}' does not match '{}'",
                schema.connector_id,
                self.connector_id()
            ));
        }
        self.upsert_pool(schema.clone());
        Ok(schema)
    }

    async fn seed_records(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        records: Vec<Value>,
    ) -> Result<()> {
        let mut pools = self.pools.write().unwrap();
        let pool = pools
            .get_mut(pool_id)
            .ok_or_else(|| anyhow!("pool '{}' not found", pool_id))?;
        pool.records.insert(pointer_uri.to_string(), records);
        Ok(())
    }

    async fn resolve_slice(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        offset: u32,
        limit: u32,
        max_bytes: u32,
    ) -> Result<DataSlicePayload> {
        let pools = self.pools.read().unwrap();
        let pool = pools.get(pool_id).ok_or_else(|| anyhow!("pool '{}' not found", pool_id))?;
        let all_records = pool.records.get(pointer_uri).ok_or_else(|| {
            anyhow!(
                "pointer_uri '{}' not found in pool '{}'",
                pointer_uri,
                pool_id
            )
        })?;
        let source_sha = InMemoryPoolConnector::compute_source_sha256(all_records);
        let start = offset as usize;
        let end = (offset + limit) as usize;
        let slice = if start >= all_records.len() {
            Vec::new()
        } else {
            all_records[start..std::cmp::min(end, all_records.len())].to_vec()
        };
        let slice_sha = InMemoryPoolConnector::compute_slice_sha256(&slice);
        let payload_bytes = serde_json::to_vec(&slice).map(|v| v.len()).unwrap_or(0) as u32;
        Ok(DataSlicePayload {
            pointer_id: pointer_uri.to_string(),
            connector_id: self.connector_id().to_string(),
            pool_id: pool_id.to_string(),
            pointer_uri: pointer_uri.to_string(),
            records: slice,
            payload_bytes,
            source_sha256: source_sha,
            slice_sha256: slice_sha,
            observed_at_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        })
    }

    async fn apply_external_effect(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        req: &ExternalEffectRequest,
    ) -> Result<ExternalEffectReceipt> {
        let mut pools = self.pools.write().unwrap();
        let pool = pools.get_mut(pool_id).ok_or_else(|| anyhow!("pool '{}' not found", pool_id))?;
        let idempotency_key = format!("{}:{}:{}", pool_id, pointer_uri, req.idempotency_key);
        if pool.schema.access_mode == DataPoolAccessMode::ReadOnly {
            return Err(anyhow!("pool '{}' is read-only", pool_id));
        }
        if let Some(receipt) = pool.idempotency.get(&idempotency_key) {
            return Ok(receipt.clone());
        }
        let all_records = pool.records.get_mut(pointer_uri).ok_or_else(|| {
            anyhow!(
                "pointer_uri '{}' not found in pool '{}'",
                pointer_uri,
                pool_id
            )
        })?;
        let current_source_sha = InMemoryPoolConnector::compute_source_sha256(all_records);
        if let Some(expected) = &req.expected_source_sha256 {
            if *expected != current_source_sha {
                return Err(anyhow!(
                    "CAS conflict: expected source sha256 {}, got {}",
                    expected,
                    current_source_sha
                ));
            }
        }
        let operation = req.operation.to_lowercase();
        match operation.as_str() {
            "append" => {
                if let Some(arr) = req.payload.as_array() {
                    all_records.extend(arr.clone());
                } else {
                    all_records.push(req.payload.clone());
                }
            }
            "replace" => {
                if let Some(arr) = req.payload.as_array() {
                    *all_records = arr.clone();
                } else {
                    *all_records = vec![req.payload.clone()];
                }
            }
            _ => {
                return Err(anyhow!("unsupported operation '{}'", req.operation));
            }
        }
        let new_source_sha = InMemoryPoolConnector::compute_source_sha256(all_records);
        let receipt = ExternalEffectReceipt {
            pointer_id: pointer_uri.to_string(),
            connector_id: self.connector_id().to_string(),
            pool_id: pool_id.to_string(),
            operation: req.operation.clone(),
            idempotency_key: req.idempotency_key.clone(),
            applied: true,
            prior_source_sha256: current_source_sha,
            new_source_sha256: new_source_sha,
            details: format!("{} applied", req.operation),
        };
        pool.idempotency.insert(idempotency_key, receipt.clone());
        Ok(receipt)
    }
}

pub struct JsonFilePoolConnector {
    id: String,
    workspace_root: PathBuf,
    pools: RwLock<HashMap<String, DataPoolSchema>>,
    idempotency: RwLock<HashMap<String, ExternalEffectReceipt>>,
}

impl JsonFilePoolConnector {
    pub fn new(workspace_root: PathBuf) -> Self {
        JsonFilePoolConnector {
            id: "json_file".to_string(),
            workspace_root,
            pools: RwLock::new(HashMap::new()),
            idempotency: RwLock::new(HashMap::new()),
        }
    }

    pub fn register_pool(&self, schema: DataPoolSchema) {
        self.pools
            .write()
            .unwrap()
            .insert(schema.pool_id.clone(), schema);
    }

    fn resolve_path(&self, pointer_uri: &str) -> Result<PathBuf> {
        if pointer_uri.is_empty() {
            return Err(anyhow!("pointer_uri must not be empty"));
        }
        let p = PathBuf::from(pointer_uri);
        for comp in p.components() {
            match comp {
                std::path::Component::Normal(_) | std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    return Err(anyhow!("pointer_uri '{}' must not contain '..'", pointer_uri));
                }
                std::path::Component::RootDir => {
                    return Err(anyhow!("pointer_uri '{}' must be relative to workspace root", pointer_uri));
                }
                std::path::Component::Prefix(_) => {
                    return Err(anyhow!("pointer_uri '{}' must not contain a prefix component", pointer_uri));
                }
            }
        }
        let abs = if p.is_absolute() {
            p
        } else {
            self.workspace_root.join(&p)
        };
        let root_canonical = self.workspace_root.canonicalize().unwrap_or_else(|_| self.workspace_root.clone());
        let canonical = match abs.canonicalize() {
            Ok(c) => c,
            Err(_) => {
                // If pointer does not exist yet (e.g. for append), resolve the parent and join file name
                let parent = abs.parent().unwrap_or_else(|| Path::new("."));
                let file_name = abs.file_name().ok_or_else(|| anyhow!("invalid pointer path '{}'", pointer_uri))?;
                let parent_canonical = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
                let canonical = parent_canonical.join(file_name);
                canonical
            }
        };
        if !canonical.starts_with(&root_canonical) {
            return Err(anyhow!(
                "pointer_uri '{}' resolves to path '{}' outside workspace root '{}'",
                pointer_uri,
                canonical.display(),
                root_canonical.display()
            ));
        }
        Ok(canonical)
    }

    fn read_json_file(path: &Path) -> Result<Vec<Value>> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow!("failed to read {}: {}", path.display(), e))?;
        // Try to parse as JSON array or JSONL
        if let Ok(parsed) = serde_json::from_str::<Vec<Value>>(&content) {
            return Ok(parsed);
        }
        // Fallback: JSONL
        let mut records = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(line)
                .map_err(|e| anyhow!("invalid JSON line in {}: {} -> {}", path.display(), line, e))?;
            records.push(value);
        }
        if records.is_empty() {
            return Err(anyhow!("file {} is empty or not valid JSON/JSONL", path.display()));
        }
        Ok(records)
    }

    fn write_json_file(path: &Path, records: &[Value]) -> Result<()> {
        let content = serde_json::to_string_pretty(records)
            .map_err(|e| anyhow!("failed to serialize records: {}", e))?;
        std::fs::write(path, content)
            .map_err(|e| anyhow!("failed to write {}: {}", path.display(), e))?;
        Ok(())
    }
}

impl Default for JsonFilePoolConnector {
    fn default() -> Self {
        JsonFilePoolConnector::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

#[async_trait]
impl DataConnector for JsonFilePoolConnector {
    fn connector_id(&self) -> &str {
        &self.id
    }

    async fn describe_pool(&self, pool_id: &str) -> Result<DataPoolSchema> {
        let pools = self.pools.read().unwrap();
        match pools.get(pool_id) {
            Some(schema) => Ok(schema.clone()),
            None => Err(anyhow!("pool '{}' not found", pool_id)),
        }
    }

    async fn register_pool(&self, mut schema: DataPoolSchema) -> Result<DataPoolSchema> {
        if schema.pool_id.trim().is_empty() {
            return Err(anyhow!("pool_id must not be empty"));
        }
        if schema.connector_id.trim().is_empty() {
            schema.connector_id = self.connector_id().to_string();
        } else if schema.connector_id != self.connector_id() {
            return Err(anyhow!(
                "schema connector_id '{}' does not match '{}'",
                schema.connector_id,
                self.connector_id()
            ));
        }
        JsonFilePoolConnector::register_pool(self, schema.clone());
        Ok(schema)
    }

    async fn resolve_slice(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        offset: u32,
        limit: u32,
        max_bytes: u32,
    ) -> Result<DataSlicePayload> {
        if !self.pools.read().unwrap().contains_key(pool_id) {
            return Err(anyhow!("pool '{}' not found", pool_id));
        }
        let path = self.resolve_path(pointer_uri)?;
        let all_records = JsonFilePoolConnector::read_json_file(&path)?;
        let source_sha = JsonFilePoolConnector::compute_sha(&all_records);
        let start = offset as usize;
        let end = (offset + limit) as usize;
        let slice = if start >= all_records.len() {
            Vec::new()
        } else {
            all_records[start..std::cmp::min(end, all_records.len())].to_vec()
        };
        let slice_sha = JsonFilePoolConnector::compute_sha(&slice);
        let payload_bytes = serde_json::to_vec(&slice).map(|v| v.len()).unwrap_or(0) as u32;
        Ok(DataSlicePayload {
            pointer_id: pointer_uri.to_string(),
            connector_id: self.connector_id().to_string(),
            pool_id: pool_id.to_string(),
            pointer_uri: pointer_uri.to_string(),
            records: slice,
            payload_bytes,
            source_sha256: source_sha,
            slice_sha256: slice_sha,
            observed_at_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        })
    }

    async fn apply_external_effect(
        &self,
        pool_id: &str,
        pointer_uri: &str,
        req: &ExternalEffectRequest,
    ) -> Result<ExternalEffectReceipt> {
        let idempotency_key = format!("{}:{}:{}", pool_id, pointer_uri, req.idempotency_key);
        let mut idem = self.idempotency.write().unwrap();
        let schema = {
            let pools = self.pools.read().unwrap();
            pools
                .get(pool_id)
                .cloned()
                .ok_or_else(|| anyhow!("pool '{}' not found", pool_id))?
        };
        if schema.access_mode == DataPoolAccessMode::ReadOnly {
            return Err(anyhow!("pool '{}' is read-only", pool_id));
        }
        if let Some(receipt) = idem.get(&idempotency_key) {
            return Ok(receipt.clone());
        }
        let path = self.resolve_path(pointer_uri)?;
        let mut all_records = JsonFilePoolConnector::read_json_file(&path)?;
        let current_source_sha = JsonFilePoolConnector::compute_sha(&all_records);
        if let Some(expected) = &req.expected_source_sha256 {
            if *expected != current_source_sha {
                return Err(anyhow!(
                    "CAS conflict: expected source sha256 {}, got {}",
                    expected,
                    current_source_sha
                ));
            }
        }
        let operation = req.operation.to_lowercase();
        match operation.as_str() {
            "append" => {
                if let Some(arr) = req.payload.as_array() {
                    all_records.extend(arr.clone());
                } else {
                    all_records.push(req.payload.clone());
                }
            }
            "replace" => {
                if let Some(arr) = req.payload.as_array() {
                    all_records = arr.clone();
                } else {
                    all_records = vec![req.payload.clone()];
                }
            }
            _ => {
                return Err(anyhow!("unsupported operation '{}'", req.operation));
            }
        }
        let new_source_sha = JsonFilePoolConnector::compute_sha(&all_records);
        // Write back
        JsonFilePoolConnector::write_json_file(&path, &all_records)?;
        let receipt = ExternalEffectReceipt {
            pointer_id: pointer_uri.to_string(),
            connector_id: self.connector_id().to_string(),
            pool_id: pool_id.to_string(),
            operation: req.operation.clone(),
            idempotency_key: req.idempotency_key.clone(),
            applied: true,
            prior_source_sha256: current_source_sha,
            new_source_sha256: new_source_sha,
            details: format!("{} applied to {}", req.operation, path.display()),
        };
        idem.insert(idempotency_key, receipt.clone());
        Ok(receipt)
    }
}

impl JsonFilePoolConnector {
    fn compute_sha(records: &[Value]) -> String {
        let mut hasher = Sha256::new();
        for r in records {
            hasher.update(serde_json::to_vec(r).expect("serialize record"));
        }
        hex::encode(hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use tokio::runtime::Runtime;

    fn rt() -> Runtime {
        Runtime::new().unwrap()
    }

    #[test]
    fn test_duplicate_connector_rejection() {
        let mut reg = DataConnectorRegistry::new();
        let c1: Arc<dyn DataConnector> = Arc::new(InMemoryPoolConnector::new());
        let c2: Arc<dyn DataConnector> = Arc::new(InMemoryPoolConnector::new());
        reg.register(c1).unwrap();
        let err = reg.register(c2).unwrap_err();
        assert!(err.to_string().contains("already registered"));
    }

    #[test]
    fn test_inmemory_describe_pool() {
        let c = InMemoryPoolConnector::new();
        c.upsert_pool(DataPoolSchema {
            connector_id: "memory_pool".into(),
            pool_id: "test_pool".into(),
            access_mode: DataPoolAccessMode::ReadOnly,
            entity_types: vec![],
            max_slice_rows: 10,
            max_slice_bytes: 1024,
            description: "test".into(),
        });
        let rt = rt();
        let schema = rt.block_on(c.describe_pool("test_pool")).unwrap();
        assert_eq!(schema.pool_id, "test_pool");
        let err = rt.block_on(c.describe_pool("missing")).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_inmemory_resolve_slice_and_hash() {
        let c = InMemoryPoolConnector::new();
        c.upsert_pool(DataPoolSchema {
            connector_id: "memory_pool".into(),
            pool_id: "p".into(),
            access_mode: DataPoolAccessMode::ReadOnly,
            entity_types: vec![],
            max_slice_rows: 10,
            max_slice_bytes: 1024,
            description: "test".into(),
        });
        c.seed_records("p", "uri", vec![json!(1), json!(2), json!(3)]);
        let rt = rt();
        let payload = rt.block_on(c.resolve_slice("p", "uri", 1, 1, 1024)).unwrap();
        assert_eq!(payload.records.len(), 1);
        assert_eq!(payload.records[0], json!(2));
        // source hash = hash of full vec, slice hash = hash of slice
        let full = vec![json!(1), json!(2), json!(3)];
        let expected_source = InMemoryPoolConnector::compute_source_sha256(&full);
        let expected_slice = InMemoryPoolConnector::compute_slice_sha256(&[json!(2)]);
        assert_eq!(payload.source_sha256, expected_source);
        assert_eq!(payload.slice_sha256, expected_slice);
        let slice_bytes = serde_json::to_vec(&payload.records).unwrap().len() as u32;
        assert_eq!(payload.payload_bytes, slice_bytes);
    }

    #[test]
    fn test_inmemory_apply_effect_append_and_cas_and_idempotency() {
        let c = InMemoryPoolConnector::new();
        c.upsert_pool(DataPoolSchema {
            connector_id: "memory_pool".into(),
            pool_id: "p".into(),
            access_mode: DataPoolAccessMode::ReadWrite,
            entity_types: vec![],
            max_slice_rows: 10,
            max_slice_bytes: 1024,
            description: "test".into(),
        });
        c.seed_records("p", "uri", vec![json!(1)]);
        let rt = rt();
        let req = ExternalEffectRequest {
            pointer_id: "uri".into(),
            operation: "append".into(),
            payload: json!(2),
            action_receipt_id: "act".into(),
            expected_source_sha256: None,
            idempotency_key: "key1".into(),
        };
        let receipt = rt.block_on(c.apply_external_effect("p", "uri", &req)).unwrap();
        assert!(receipt.applied);
        // prior and new should differ after append
        assert_ne!(receipt.prior_source_sha256, receipt.new_source_sha256);
        // Replay idempotency
        let receipt2 = rt.block_on(c.apply_external_effect("p", "uri", &req)).unwrap();
        assert_eq!(receipt2.new_source_sha256, receipt.new_source_sha256);
        // CAS conflict
        let bad_req = ExternalEffectRequest {
            pointer_id: "uri".into(),
            operation: "replace".into(),
            payload: json!(3),
            action_receipt_id: "act2".into(),
            expected_source_sha256: Some("0000".into()),
            idempotency_key: "key2".into(),
        };
        let err = rt.block_on(c.apply_external_effect("p", "uri", &bad_req)).unwrap_err();
        assert!(err.to_string().contains("CAS conflict"));
    }

    #[test]
    fn test_inmemory_read_only_rejection() {
        let c = InMemoryPoolConnector::new();
        c.upsert_pool(DataPoolSchema {
            connector_id: "memory_pool".into(),
            pool_id: "ro".into(),
            access_mode: DataPoolAccessMode::ReadOnly,
            entity_types: vec![],
            max_slice_rows: 10,
            max_slice_bytes: 1024,
            description: "test".into(),
        });
        c.seed_records("ro", "uri", vec![json!(1)]);
        let rt = rt();
        let req = ExternalEffectRequest {
            pointer_id: "uri".into(),
            operation: "append".into(),
            payload: json!(2),
            action_receipt_id: "act".into(),
            expected_source_sha256: None,
            idempotency_key: "key1".into(),
        };
        let err = rt.block_on(c.apply_external_effect("ro", "uri", &req)).unwrap_err();
        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn test_json_file_connector() {
        let dir = std::env::temp_dir();
        let root = dir.join("ns_json_test");
        std::fs::create_dir_all(&root).unwrap();
        let file_path = root.join("data.json");
        let data = vec![json!({"a":1}), json!({"b":2})];
        std::fs::write(&file_path, serde_json::to_string_pretty(&data).unwrap()).unwrap();
        let c = JsonFilePoolConnector::new(root.clone());
        c.register_pool(DataPoolSchema {
            connector_id: "json_file".into(),
            pool_id: "jp".into(),
            access_mode: DataPoolAccessMode::ReadWrite,
            entity_types: vec![],
            max_slice_rows: 10,
            max_slice_bytes: 1024,
            description: "test".into(),
        });
        let rt = rt();
        let payload = rt.block_on(c.resolve_slice("jp", "data.json", 0, 10, 1024)).unwrap();
        assert_eq!(payload.records.len(), 2);
        assert_eq!(payload.source_sha256, payload.slice_sha256);
        // Path traversal
        let err = rt.block_on(c.resolve_slice("jp", "../outside.json", 0, 10, 1024)).unwrap_err();
        assert!(err.to_string().contains("must not contain '..'"));
        // Write-back
        let req = ExternalEffectRequest {
            pointer_id: "data.json".into(),
            operation: "append".into(),
            payload: json!({"c":3}),
            action_receipt_id: "act".into(),
            expected_source_sha256: None,
            idempotency_key: "k1".into(),
        };
        let receipt = rt.block_on(c.apply_external_effect("jp", "data.json", &req)).unwrap();
        assert!(receipt.applied);
        // Check file
        let new_content = std::fs::read_to_string(&file_path).unwrap();
        let parsed: Vec<Value> = serde_json::from_str(&new_content).unwrap();
        assert_eq!(parsed.len(), 3);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn test_json_file_connector_path_traversal_rejection() {
        let dir = std::env::temp_dir();
        let root = dir.join("ns_json_traversal");
        std::fs::create_dir_all(&root).unwrap();
        let file_path = root.join("data.json");
        let data = vec![json!({"a":1})];
        std::fs::write(&file_path, serde_json::to_string_pretty(&data).unwrap()).unwrap();
        let c = JsonFilePoolConnector::new(root.clone());
        c.register_pool(DataPoolSchema {
            connector_id: "json_file".into(),
            pool_id: "jp".into(),
            access_mode: DataPoolAccessMode::ReadWrite,
            entity_types: vec![],
            max_slice_rows: 10,
            max_slice_bytes: 1024,
            description: "test".into(),
        });
        let rt = rt();
        let err = rt.block_on(c.resolve_slice("jp", "nonexistent_dir/../../outside.json", 0, 10, 1024)).unwrap_err();
        assert!(err.to_string().contains("must not contain '..'"));
        let err = rt.block_on(c.resolve_slice("jp", "/etc/passwd", 0, 10, 1024)).unwrap_err();
        assert!(err.to_string().contains("must be relative to workspace root"));
        let req = ExternalEffectRequest {
            pointer_id: "../outside.json".into(),
            operation: "append".into(),
            payload: json!(2),
            action_receipt_id: "act".into(),
            expected_source_sha256: None,
            idempotency_key: "k1".into(),
        };
        let err = rt.block_on(c.apply_external_effect("jp", "../outside.json", &req)).unwrap_err();
        assert!(err.to_string().contains("must not contain '..'"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn test_new_with_defaults_preseeds_default_and_configured_pools() {
        let dir = tempfile::tempdir().unwrap();
        let ns_dir = dir.path().join(".NeuroStrata");
        std::fs::create_dir_all(&ns_dir).unwrap();
        std::fs::write(
            ns_dir.join("connector_pools.json"),
            serde_json::to_string_pretty(&json!({
                "pools": [
                    {
                        "connector_id": "json_file",
                        "pool_id": "custom_json",
                        "access_mode": "read_write",
                        "entity_types": ["record"],
                        "max_slice_rows": 50,
                        "max_slice_bytes": 4096,
                        "description": "Custom JSON file pool"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("items.json"),
            serde_json::to_string_pretty(&json!([{"k": 1}, {"k": 2}])).unwrap(),
        )
        .unwrap();

        let reg = DataConnectorRegistry::new_with_defaults(dir.path().to_path_buf());
        let rt = rt();
        let json_conn = reg.get("json_file").unwrap();
        let default_schema = rt.block_on(json_conn.describe_pool("default")).unwrap();
        assert_eq!(default_schema.access_mode, DataPoolAccessMode::ReadWrite);
        let ro_schema = rt.block_on(json_conn.describe_pool("readonly")).unwrap();
        assert_eq!(ro_schema.access_mode, DataPoolAccessMode::ReadOnly);
        let custom_schema = rt.block_on(json_conn.describe_pool("custom_json")).unwrap();
        assert_eq!(custom_schema.max_slice_rows, 50);

        let slice = rt
            .block_on(json_conn.resolve_slice("default", "items.json", 0, 10, 1024))
            .unwrap();
        assert_eq!(slice.records.len(), 2);

        let mem_conn = reg.get("memory_pool").unwrap();
        let mem_default = rt.block_on(mem_conn.describe_pool("default")).unwrap();
        assert_eq!(mem_default.access_mode, DataPoolAccessMode::ReadWrite);
    }
}
