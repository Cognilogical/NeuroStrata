//! Central Agentic Kernel – Mediates between ports, workflow, and connectors.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::connectors::{
    DataConnectorRegistry, DataPoolAccessMode, DataPointerRecord, DataSlicePayload,
    DataSliceRequest, ExternalEffectRequest, ExternalEffectReceipt,
};
use crate::events::ThalamicBus;
use crate::formal_kernel::{
    build_resolve_post_gate_input, build_writeback_post_gate_input, evaluate_connector_post_gate,
    evaluate_connector_pre_gate, KernelActionReceiptState, KernelConnectorOp,
    KernelConnectorPreGateInput, KernelConnectorPreVerdict, KernelConnectorPostVerdict,
};
use crate::kernel::ports::{
    ExecutivePort, KernelWorkflowTransitionVerdict, LadybugExecutivePort, LadybugMemoryPort,
    LadybugOntologyPort, MemoryPort, OntologyPort,
};
use crate::store::LadybugStore;
use crate::traits::VectorStore;
use crate::workflow::connector::{GitWorkflowConnector, WorkflowConnector};
use crate::workflow::machine::WorkflowStage;

pub mod ports;

/// Registry of pluggable workflow connectors keyed by `connector_id`.
pub struct WorkflowRegistry {
    #[allow(dead_code)]
    workspace_root: PathBuf,
    registry: RwLock<HashMap<String, Arc<dyn WorkflowConnector>>>,
}

impl WorkflowRegistry {
    pub fn new_with_defaults(workspace_root: PathBuf) -> Self {
        let reg = WorkflowRegistry {
            workspace_root,
            registry: RwLock::new(HashMap::new()),
        };
        let git = Arc::new(GitWorkflowConnector);
        reg.registry
            .write()
            .unwrap()
            .insert("git".to_string(), git);
        reg
    }

    pub fn new() -> Self {
        WorkflowRegistry::new_with_defaults(PathBuf::from("."))
    }

    pub fn register(&self, connector: Arc<dyn WorkflowConnector>) -> Result<()> {
        let mut reg = self.registry.write().unwrap();
        let id = connector.connector_id().to_string();
        if reg.contains_key(&id) {
            return Err(anyhow::anyhow!("connector_id '{}' already registered", id));
        }
        reg.insert(id, connector);
        Ok(())
    }

    pub fn get(&self, connector_id: &str) -> Option<Arc<dyn WorkflowConnector>> {
        self.registry.read().unwrap().get(connector_id).cloned()
    }

    pub fn list_connector_ids(&self) -> Vec<String> {
        self.registry.read().unwrap().keys().cloned().collect()
    }
}

/// Central Agentic Kernel facade.
pub struct AgentKernel {
    store: Arc<dyn VectorStore>,
    memory: Arc<dyn MemoryPort>,
    executive: Arc<dyn ExecutivePort>,
    ontology: Arc<dyn OntologyPort>,
    workflows: Arc<WorkflowRegistry>,
    connectors: Arc<DataConnectorRegistry>,
    bus: Arc<ThalamicBus>,
}

impl AgentKernel {
    /// Construct kernel with default ports/registry.
    ///
    /// Accepts the serving layers' trait object (`Arc<dyn VectorStore>`).
    /// The backend is resolved privately and the kernel fails closed on any
    /// backend other than the LadybugDB concrete store.
    pub fn new(
        store: Arc<dyn VectorStore>,
        bus: Arc<ThalamicBus>,
        workspace_root: PathBuf,
    ) -> Self {
        let workflows = Arc::new(WorkflowRegistry::new_with_defaults(workspace_root.clone()));
        let memory: Arc<dyn MemoryPort> = Arc::new(LadybugMemoryPort::new(store.clone()));
        let executive: Arc<dyn ExecutivePort> = Arc::new(LadybugExecutivePort::new(store.clone()));
        let ontology: Arc<dyn OntologyPort> = Arc::new(LadybugOntologyPort::new(store.clone()));
        AgentKernel {
            store,
            memory,
            executive,
            ontology,
            workflows,
            connectors: Arc::new(DataConnectorRegistry::new_with_defaults(workspace_root)),
            bus,
        }
    }

    pub fn with_ports(
        store: Arc<dyn VectorStore>,
        bus: Arc<ThalamicBus>,
        memory: Arc<dyn MemoryPort>,
        executive: Arc<dyn ExecutivePort>,
        ontology: Arc<dyn OntologyPort>,
        workflows: Arc<WorkflowRegistry>,
        connectors: Arc<DataConnectorRegistry>,
    ) -> Self {
        AgentKernel {
            store,
            memory,
            executive,
            ontology,
            workflows,
            connectors,
            bus,
        }
    }

    /// Return the concrete LadybugStore behind the trait object; fail closed on mismatch.
    fn ladybug_store(&self) -> &crate::store::ladybug::LadybugStore {
        crate::traits::VectorStore::as_any(self.store.as_ref())
            .downcast_ref::<crate::store::ladybug::LadybugStore>()
            .expect("kinetic kernel requires the LadybugDB store backend")
    }

    /// Current epoch seconds for dispatcher-level `now_secs` defaults, keeping
    /// CLI/MCP/HTTP parity: a pre-epoch system clock maps to 0 and the pre-gate
    /// then fails closed on the resulting future-clock-skew denial.
    fn current_epoch_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    pub fn memory(&self) -> &Arc<dyn MemoryPort> {
        &self.memory
    }

    pub fn executive(&self) -> &Arc<dyn ExecutivePort> {
        &self.executive
    }

    pub fn ontology(&self) -> &Arc<dyn OntologyPort> {
        &self.ontology
    }

    pub fn workflows(&self) -> &Arc<WorkflowRegistry> {
        &self.workflows
    }

    pub fn connectors(&self) -> &Arc<DataConnectorRegistry> {
        &self.connectors
    }

    pub fn bus(&self) -> &Arc<ThalamicBus> {
        &self.bus
    }

    /// Pre-gate helper for `RegisterPointer`.
    async fn evaluate_register_pre_gate(
        &self,
        namespace: &str,
        record: &DataPointerRecord,
        principal: Option<&str>,
    ) -> Result<KernelConnectorPreVerdict> {
        let connector_registered = self.connectors.get(&record.connector_id).is_some();
        let rebac_allowed = crate::rebac::can_write_namespace(self.ladybug_store(), namespace, principal);
        Ok(evaluate_connector_pre_gate(KernelConnectorPreGateInput {
            connector_registered,
            rebac_allowed,
            op: KernelConnectorOp::RegisterPointer,
            pool_writable: false,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: 0,
            max_slice_rows: 0,
            requested_bytes: 0,
            max_slice_bytes: 0,
            clock_skew_future: false,
            age_secs: 0,
            max_staleness_secs: 0,
        }))
    }

    /// Register an external data pointer under the kernel’s ReBAC + pre-gate
    /// enforcement.
    pub async fn register_pointer(
        &self,
        namespace: &str,
        record: &DataPointerRecord,
        principal: Option<&str>,
    ) -> Result<DataPointerRecord> {
        let verdict = self
            .evaluate_register_pre_gate(namespace, record, principal)
            .await?;
        if verdict != KernelConnectorPreVerdict::AllowPreRegister {
            anyhow::bail!("Pre-gate denied pointer registration: {:?}", verdict);
        }
        self.memory
            .register_data_pointer(namespace, record, principal)
            .await
    }

    /// Resolve a bounded slice from a trusted pointer.
    pub async fn resolve_pointer(
        &self,
        namespace: &str,
        req: &DataSliceRequest,
        now_secs: u64,
        principal: Option<&str>,
    ) -> Result<DataSlicePayload> {
        let pointer = self
            .memory
            .get_data_pointer(namespace, &req.pointer_id, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("pointer_id '{}' not found", req.pointer_id))?;
        let connector_opt = self.connectors.get(&pointer.connector_id);
        let rebac_allowed = self
            .memory
            .get_data_pointer(namespace, &req.pointer_id, principal)
            .await?
            .is_some();
        // Fail closed on unregistered connector or ReBAC denial BEFORE any
        // connector I/O (not even describe_pool runs in that case).
        if connector_opt.is_none() || !rebac_allowed {
            let pre_verdict = evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: connector_opt.is_some(),
                rebac_allowed,
                op: KernelConnectorOp::ResolveSlice,
                pool_writable: false,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: req.limit,
                max_slice_rows: 0,
                requested_bytes: req.max_bytes,
                max_slice_bytes: 0,
                clock_skew_future: now_secs < pointer.captured_at_secs,
                age_secs: now_secs.saturating_sub(pointer.captured_at_secs),
                max_staleness_secs: pointer.max_staleness_secs,
            });
            anyhow::bail!("Pre-gate denied slice resolution: {:?}", pre_verdict);
        }
        let connector = connector_opt.expect("connector present after early-gate check");
        let schema = connector.describe_pool(&pointer.pool_id).await?;
        let pre_input = KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed,
            op: KernelConnectorOp::ResolveSlice,
            pool_writable: schema.access_mode == DataPoolAccessMode::ReadWrite,
            receipt_state: KernelActionReceiptState::NotRequired,
            requested_rows: req.limit,
            max_slice_rows: schema.max_slice_rows,
            requested_bytes: req.max_bytes,
            max_slice_bytes: schema.max_slice_bytes,
            clock_skew_future: now_secs < pointer.captured_at_secs,
            age_secs: now_secs.saturating_sub(pointer.captured_at_secs),
            max_staleness_secs: pointer.max_staleness_secs,
        };
        let pre_verdict = evaluate_connector_pre_gate(pre_input);
        if pre_verdict != KernelConnectorPreVerdict::AllowPreResolve {
            anyhow::bail!("Pre-gate denied slice resolution: {:?}", pre_verdict);
        }
        let mut slice = connector
            .resolve_slice(&pointer.pool_id, &pointer.pointer_uri, req.offset, req.limit, req.max_bytes)
            .await?;
        slice.pointer_id = pointer.pointer_id.clone();
        let source_digest_matches = match &pointer.expected_source_sha256 {
            Some(expected) => expected == &slice.source_sha256,
            None => true,
        };
        let post_verdict = evaluate_connector_post_gate(build_resolve_post_gate_input(
            slice.records.len(),
            req.limit,
            schema.max_slice_rows,
            slice.payload_bytes,
            req.max_bytes,
            schema.max_slice_bytes,
            source_digest_matches,
        ));
        if post_verdict != KernelConnectorPostVerdict::AllowPostOutput {
            anyhow::bail!("Post-gate denied slice resolution: {:?}", post_verdict);
        }
        Ok(slice)
    }

    /// Apply an external write-back guarded by receipt + CAS + pre/post gates.
    pub async fn apply_external_effect(
        &self,
        namespace: &str,
        req: &ExternalEffectRequest,
        now_secs: u64,
        principal: Option<&str>,
    ) -> Result<ExternalEffectReceipt> {
        let pointer = self
            .memory
            .get_data_pointer(namespace, &req.pointer_id, None)
            .await?
            .ok_or_else(|| anyhow::anyhow!("pointer_id '{}' not found", req.pointer_id))?;
        let connector_opt = self.connectors.get(&pointer.connector_id);
        let rebac_allowed = crate::rebac::can_write_namespace(self.ladybug_store(), namespace, principal);
        // Fail closed on unregistered connector or ReBAC denial BEFORE any
        // connector I/O (not even describe_pool runs in that case).
        if connector_opt.is_none() || !rebac_allowed {
            let pre_verdict = evaluate_connector_pre_gate(KernelConnectorPreGateInput {
                connector_registered: connector_opt.is_some(),
                rebac_allowed,
                op: KernelConnectorOp::ApplyWriteBack,
                pool_writable: false,
                receipt_state: KernelActionReceiptState::NotRequired,
                requested_rows: 0,
                max_slice_rows: 0,
                requested_bytes: 0,
                max_slice_bytes: 0,
                clock_skew_future: now_secs < pointer.captured_at_secs,
                age_secs: now_secs.saturating_sub(pointer.captured_at_secs),
                max_staleness_secs: pointer.max_staleness_secs,
            });
            anyhow::bail!("Pre-gate denied write-back: {:?}", pre_verdict);
        }
        let receipt_state = self
            .ontology
            .verify_active_action_receipt(namespace, &req.action_receipt_id, &pointer.pointer_id, &pointer.pool_id)
            .await?;
        let connector = connector_opt.expect("connector present after early-gate check");
        let schema = connector.describe_pool(&pointer.pool_id).await?;
        let pre_input = KernelConnectorPreGateInput {
            connector_registered: true,
            rebac_allowed,
            op: KernelConnectorOp::ApplyWriteBack,
            pool_writable: schema.access_mode == DataPoolAccessMode::ReadWrite,
            receipt_state,
            requested_rows: 0,
            max_slice_rows: schema.max_slice_rows,
            requested_bytes: 0,
            max_slice_bytes: schema.max_slice_bytes,
            clock_skew_future: now_secs < pointer.captured_at_secs,
            age_secs: now_secs.saturating_sub(pointer.captured_at_secs),
            max_staleness_secs: pointer.max_staleness_secs,
        };
        let pre_verdict = evaluate_connector_pre_gate(pre_input);
        if pre_verdict != KernelConnectorPreVerdict::AllowPreWriteBack {
            anyhow::bail!("Pre-gate denied write-back: {:?}", pre_verdict);
        }
        let receipt = connector.apply_external_effect(&pointer.pool_id, &pointer.pointer_uri, req).await?;
        let pointer_source_digest_matches = match &pointer.expected_source_sha256 {
            Some(expected) => expected == &receipt.prior_source_sha256,
            None => true,
        };
        let request_cas_expected_matches = match &req.expected_source_sha256 {
            Some(expected) => expected == &receipt.prior_source_sha256,
            None => true,
        };
        let post_verdict = evaluate_connector_post_gate(build_writeback_post_gate_input(
            schema.max_slice_rows,
            schema.max_slice_bytes,
            pointer_source_digest_matches,
            request_cas_expected_matches,
        ));
        if post_verdict != KernelConnectorPostVerdict::AllowPostOutput {
            anyhow::bail!("Post-gate denied write-back: {:?}", post_verdict);
        }
        let mut receipt = receipt;
        receipt.pointer_id = pointer.pointer_id.clone();
        Ok(receipt)
    }

    /// Execute a connector command dispatching on `args["operation"]`.
    /// Supported operations: list_connectors, describe_pool, register_pointer,
    /// list_pointers, resolve_pointer, apply_effect, mediate.
    pub async fn execute_connector_command(
        &self,
        namespace: &str,
        args: &serde_json::Value,
        principal: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let operation = args
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'operation' in args"))?;
        match operation {
            "list_connectors" => Ok(serde_json::json!(
                self.connectors().list_connector_ids()
            )),
            "describe_pool" => {
                let pool_id = args
                    .get("pool_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing 'pool_id' in args"))?;
                let connector_id = args
                    .get("connector_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing 'connector_id' in args"))?;
                let connector = self
                    .connectors()
                    .get(connector_id)
                    .ok_or_else(|| anyhow::anyhow!("connector '{}' not found", connector_id))?;
                let schema = connector.describe_pool(pool_id).await?;
                Ok(serde_json::to_value(&schema)?)
            }
            "register_pointer" => {
                let record: DataPointerRecord = args
                    .get("record")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .ok_or_else(|| anyhow::anyhow!("missing or invalid 'record' in args"))?;
                let result = self.register_pointer(namespace, &record, principal).await?;
                Ok(serde_json::to_value(&result)?)
            }
            "list_pointers" => {
                let pointers =
                    self
                        .memory
                        .list_data_pointers(namespace, None, principal)
                        .await?;
                Ok(serde_json::json!(pointers))
            }
            "resolve_pointer" => {
                let req: DataSliceRequest = args
                    .get("req")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .ok_or_else(|| anyhow::anyhow!("missing or invalid 'req' in args"))?;
                let now_secs = args
                    .get("now_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_else(Self::current_epoch_secs);
                let result = self.resolve_pointer(namespace, &req, now_secs, principal).await?;
                Ok(serde_json::to_value(&result)?)
            }
            "apply_effect" => {
                let req: ExternalEffectRequest = args
                    .get("req")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .ok_or_else(|| anyhow::anyhow!("missing or invalid 'req' in args"))?;
                let now_secs = args
                    .get("now_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_else(Self::current_epoch_secs);
                let result = self.apply_external_effect(namespace, &req, now_secs, principal).await?;
                Ok(serde_json::to_value(&result)?)
            }
            "mediate" => {
                let from_stage: WorkflowStage = match args.get("from_stage").and_then(|v| v.as_str()) {
                    Some(s) => match s.to_lowercase().as_str() {
                        "unpreflighted" => WorkflowStage::Unpreflighted,
                        "contextloaded" => WorkflowStage::ContextLoaded,
                        "claimed" => WorkflowStage::Claimed,
                        "mutating" => WorkflowStage::Mutating,
                        "breathrequired" => WorkflowStage::BreathRequired,
                        "precheckpoint" => WorkflowStage::PreCheckpoint,
                        "prepublish" => WorkflowStage::PrePublish,
                        "completed" => WorkflowStage::Completed,
                        _ => anyhow::bail!("invalid 'from_stage' value: {}", s),
                    },
                    None => anyhow::bail!("missing 'from_stage' in args"),
                };
                let to_stage: WorkflowStage = match args.get("to_stage").and_then(|v| v.as_str()) {
                    Some(s) => match s.to_lowercase().as_str() {
                        "unpreflighted" => WorkflowStage::Unpreflighted,
                        "contextloaded" => WorkflowStage::ContextLoaded,
                        "claimed" => WorkflowStage::Claimed,
                        "mutating" => WorkflowStage::Mutating,
                        "breathrequired" => WorkflowStage::BreathRequired,
                        "precheckpoint" => WorkflowStage::PreCheckpoint,
                        "prepublish" => WorkflowStage::PrePublish,
                        "completed" => WorkflowStage::Completed,
                        _ => anyhow::bail!("invalid 'to_stage' value: {}", s),
                    },
                    None => anyhow::bail!("missing 'to_stage' in args"),
                };
                let gate_passed = args
                    .get("gate_passed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let slice_req: DataSliceRequest = args
                    .get("slice_req")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .ok_or_else(|| anyhow::anyhow!("missing or invalid 'slice_req' in args"))?;
                let now_secs = args
                    .get("now_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_else(Self::current_epoch_secs);
                let (_, slice) = self
                    .mediate_workflow_data(namespace, from_stage, to_stage, gate_passed, &slice_req, now_secs, principal)
                    .await?;
                Ok(serde_json::to_value(&slice)?)
            }
            _ => anyhow::bail!("unknown connector operation: {}", operation),
        }
    }

    /// End-to-end: workflow gate -> data slice resolution.
    pub async fn mediate_workflow_data(
        &self,
        namespace: &str,
        from_stage: WorkflowStage,
        to_stage: WorkflowStage,
        gate_passed: bool,
        slice_req: &DataSliceRequest,
        now_secs: u64,
        principal: Option<&str>,
    ) -> Result<(WorkflowStage, DataSlicePayload)> {
        let verdict = self
            .executive
            .evaluate_workflow_transition(from_stage, to_stage, gate_passed)
            .await;  // returns KernelWorkflowTransitionVerdict, not Result
        if verdict != KernelWorkflowTransitionVerdict::AllowTransition {
            anyhow::bail!("Workflow transition denied: {:?}", verdict);
        }
        let slice = self
            .resolve_pointer(namespace, slice_req, now_secs, principal)
            .await?;
        Ok((to_stage, slice))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{DataConnector, DataPoolSchema};
    use crate::traits::{MemoryPayload, OntologyStore, VectorStore};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A configurable spy connector that records every `resolve_slice` /
    /// `apply_external_effect` invocation so the pre-gate can be proven to
    /// perform zero connector I/O on denial paths.
    struct SpyConnector {
        id: String,
        connector_id: String,
        pool_id: String,
        access_mode: DataPoolAccessMode,
        max_slice_rows: u32,
        max_slice_bytes: u32,
        records: Vec<serde_json::Value>,
        bad_source_digest: bool,
        inflate_rows: bool,
        inflate_bytes: bool,
        resolve_calls: AtomicUsize,
        apply_calls: AtomicUsize,
    }

    impl SpyConnector {
        fn new(id: &str, access_mode: DataPoolAccessMode, max_rows: u32, max_bytes: u32) -> Self {
            SpyConnector {
                id: id.to_string(),
                connector_id: id.to_string(),
                pool_id: "pool1".to_string(),
                access_mode,
                max_slice_rows: max_rows,
                max_slice_bytes: max_bytes,
                records: vec![json!({"v": 1})],
                bad_source_digest: false,
                inflate_rows: false,
                inflate_bytes: false,
                resolve_calls: AtomicUsize::new(0),
                apply_calls: AtomicUsize::new(0),
            }
        }

        fn schema(&self) -> DataPoolSchema {
            DataPoolSchema {
                connector_id: self.connector_id.clone(),
                pool_id: self.pool_id.clone(),
                access_mode: self.access_mode,
                entity_types: vec![],
                max_slice_rows: self.max_slice_rows,
                max_slice_bytes: self.max_slice_bytes,
                description: "spy".to_string(),
            }
        }
    }

    #[async_trait]
    impl DataConnector for SpyConnector {
        fn connector_id(&self) -> &str {
            &self.id
        }

        async fn describe_pool(&self, _pool_id: &str) -> Result<DataPoolSchema> {
            Ok(self.schema())
        }

        async fn resolve_slice(
            &self,
            _pool_id: &str,
            pointer_uri: &str,
            offset: u32,
            limit: u32,
            _max_bytes: u32,
        ) -> Result<DataSlicePayload> {
            self.resolve_calls.fetch_add(1, Ordering::SeqCst);
            let rows = if self.inflate_rows {
                (0..(self.max_slice_rows + 5) as usize)
                    .map(|i| json!({"i": i}))
                    .collect::<Vec<_>>()
            } else {
                let start = offset as usize;
                let end = (offset + limit) as usize;
                if start >= self.records.len() {
                    Vec::new()
                } else {
                    self.records[start..std::cmp::min(end, self.records.len())].to_vec()
                }
            };
            let payload_bytes = if self.inflate_bytes {
                self.max_slice_bytes + 4096
            } else {
                serde_json::to_vec(&rows).map(|v| v.len() as u32).unwrap_or(0)
            };
            let source_sha = if self.bad_source_digest {
                "bad-source-digest".to_string()
            } else {
                "good-source-digest".to_string()
            };
            Ok(DataSlicePayload {
                pointer_id: "unset".to_string(),
                connector_id: self.connector_id.clone(),
                pool_id: self.pool_id.clone(),
                pointer_uri: pointer_uri.to_string(),
                records: rows,
                payload_bytes,
                source_sha256: source_sha,
                slice_sha256: "slice-sha".to_string(),
                observed_at_secs: 0,
            })
        }

        async fn apply_external_effect(
            &self,
            _pool_id: &str,
            _pointer_uri: &str,
            req: &ExternalEffectRequest,
        ) -> Result<ExternalEffectReceipt> {
            self.apply_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ExternalEffectReceipt {
                pointer_id: req.pointer_id.clone(),
                connector_id: self.connector_id.clone(),
                pool_id: self.pool_id.clone(),
                operation: req.operation.clone(),
                idempotency_key: req.idempotency_key.clone(),
                applied: true,
                prior_source_sha256: "prior".to_string(),
                new_source_sha256: "new-source-digest".to_string(),
                details: "applied".to_string(),
            })
        }
    }

    fn build_kernel(spy: Arc<SpyConnector>) -> (AgentKernel, Arc<LadybugStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(LadybugStore::new(dir.path().join("db"), 4).expect("store init"));
        let bus = Arc::new(ThalamicBus::new(64));
        let connectors = Arc::new(DataConnectorRegistry::new());
        connectors.register(spy).expect("register spy");
        let kernel = AgentKernel::with_ports(
            store.clone(),
            bus,
            Arc::new(LadybugMemoryPort::new(store.clone())),
            Arc::new(LadybugExecutivePort::new(store.clone())),
            Arc::new(LadybugOntologyPort::new(store.clone())),
            Arc::new(WorkflowRegistry::new()),
            connectors,
        );
        (kernel, store, dir)
    }

    fn pointer_record(pointer_id: &str, connector: &str, captured_at: u64, max_staleness: u64) -> DataPointerRecord {
        DataPointerRecord {
            pointer_id: pointer_id.to_string(),
            connector_id: connector.to_string(),
            pool_id: "pool1".to_string(),
            pointer_uri: "uri1".to_string(),
            entity_type: "item".to_string(),
            expected_source_sha256: None,
            captured_at_secs: captured_at,
            max_staleness_secs: max_staleness,
        }
    }

    fn slice_request(pointer_id: &str, limit: u32, max_bytes: u32) -> DataSliceRequest {
        DataSliceRequest {
            pointer_id: pointer_id.to_string(),
            offset: 0,
            limit,
            max_bytes,
        }
    }

    fn writeback_request(pointer_id: &str, receipt_id: &str) -> ExternalEffectRequest {
        ExternalEffectRequest {
            pointer_id: pointer_id.to_string(),
            operation: "append".to_string(),
            payload: json!({"v": 2}),
            action_receipt_id: receipt_id.to_string(),
            expected_source_sha256: None,
            idempotency_key: format!("idem-{}", receipt_id),
        }
    }

    async fn seed_pointer(
        store: &LadybugStore,
        namespace: &str,
        record: &DataPointerRecord,
        owner: &str,
        visible_to: Option<Vec<&str>>,
    ) {
        let mut metadata = json!({
            "eligibility": "private",
            "pointer": serde_json::to_value(record).unwrap(),
            "source": {"kind": "data_connector", "ref": record.pointer_uri}
        });
        if let Some(vt) = visible_to {
            metadata["visible_to"] = serde_json::to_value(vt).unwrap();
        }
        let payload = MemoryPayload {
            content: format!("data pointer {}", record.pointer_id),
            user_id: owner.to_string(),
            memory_type: "data_pointer".to_string(),
            agent_name: None,
            location: record.pointer_uri.clone(),
            location_lines: String::new(),
            metadata,
        };
        store
            .put_scoped_memory(namespace, &record.pointer_id, &payload)
            .await
            .expect("seed pointer");
    }

    async fn seed_receipt(
        store: &LadybugStore,
        namespace: &str,
        receipt_id: &str,
        pointer_id: &str,
        pool_id: &str,
        reverted: bool,
    ) {
        let mut metadata = json!({"pointer_id": pointer_id, "pool_id": pool_id});
        if reverted {
            metadata["reverted_at"] = json!("2026-01-01T00:00:00Z");
        }
        let payload = MemoryPayload {
            content: "receipt".to_string(),
            user_id: "alice".to_string(),
            memory_type: "action_receipt".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata,
        };
        store
            .put_scoped_memory(namespace, receipt_id, &payload)
            .await
            .expect("seed receipt");
    }

    #[tokio::test]
    async fn pre_gate_deny_performs_zero_connector_io() {
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let spy_ro = Arc::new(SpyConnector::new("spy_ro", DataPoolAccessMode::ReadOnly, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        // Register the read-only spy so its write-back can be exercised.
        kernel.connectors().register(spy_ro.clone()).expect("register ro spy");
        let ns = "ns1";
        store.init(ns).await.expect("init");

        // 1. Unregistered connector pointer registration -> denied, 0 spy calls.
        let ghost = pointer_record("ghost_ptr", "ghost", 1000, 100);
        let err = kernel.register_pointer(ns, &ghost, Some(ns)).await.unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        assert_eq!(spy.resolve_calls.load(Ordering::SeqCst), 0);
        assert_eq!(spy.apply_calls.load(Ordering::SeqCst), 0);

        // 2. ReBAC denial on resolve_pointer (restricted pointer) -> 0 resolve calls.
        let restricted = pointer_record("ptr_restricted", "spy", 1000, 100);
        seed_pointer(&store, ns, &restricted, "alice", Some(vec!["alice"])).await;
        let err = kernel
            .resolve_pointer(ns, &slice_request("ptr_restricted", 1, 64), 1000, Some("mallory"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        assert_eq!(spy.resolve_calls.load(Ordering::SeqCst), 0);

        // 3. Slice limit exceeded (limit == 0 and limit > max_slice_rows) -> 0 resolve calls.
        kernel
            .register_pointer(ns, &pointer_record("ptr_ok", "spy", 1000, 100), Some(ns))
            .await
            .expect("register pointer");
        let err = kernel
            .resolve_pointer(ns, &slice_request("ptr_ok", 0, 64), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        let err = kernel
            .resolve_pointer(ns, &slice_request("ptr_ok", 11, 64), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        assert_eq!(spy.resolve_calls.load(Ordering::SeqCst), 0);

        // 4. Future clock skew (now < captured_at) -> 0 resolve calls.
        let err = kernel
            .resolve_pointer(ns, &slice_request("ptr_ok", 1, 64), 999, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        assert_eq!(spy.resolve_calls.load(Ordering::SeqCst), 0);

        // 5. Stale pointer (now > captured_at + max_staleness) -> 0 resolve calls.
        let err = kernel
            .resolve_pointer(ns, &slice_request("ptr_ok", 1, 64), 1101, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        assert_eq!(spy.resolve_calls.load(Ordering::SeqCst), 0);

        // 6. Write-back on ReadOnly pool -> denied, 0 apply calls.
        kernel
            .register_pointer(ns, &pointer_record("ptr_ro", "spy_ro", 1000, 100), Some(ns))
            .await
            .expect("register ro pointer");
        seed_receipt(&store, ns, "rcpt_ro", "ptr_ro", "pool1", false).await;
        let err = kernel
            .apply_external_effect(ns, &writeback_request("ptr_ro", "rcpt_ro"), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        assert_eq!(spy_ro.apply_calls.load(Ordering::SeqCst), 0);

        // 7. Write-back with missing, reverted, and target-mismatched receipts -> 0 apply calls.
        // missing receipt
        let err = kernel
            .apply_external_effect(ns, &writeback_request("ptr_ok", "rcpt_missing"), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        // reverted receipt
        seed_receipt(&store, ns, "rcpt_reverted", "ptr_ok", "pool1", true).await;
        let err = kernel
            .apply_external_effect(ns, &writeback_request("ptr_ok", "rcpt_reverted"), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        // target-mismatched receipt
        seed_receipt(&store, ns, "rcpt_mismatch", "some_other_pointer", "other_pool", false).await;
        let err = kernel
            .apply_external_effect(ns, &writeback_request("ptr_ok", "rcpt_mismatch"), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");
        assert_eq!(spy.apply_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn post_gate_rejects_source_digest_mismatch_and_observed_bounds() {
        let ns = "ns1";

        // Source-digest mismatch: pointer expects a digest the spy does not return.
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        store.init(ns).await.expect("init");
        let mut rec = pointer_record("ptr_digest", "spy", 1000, 100);
        rec.expected_source_sha256 = Some("expected-digest".to_string());
        kernel.register_pointer(ns, &rec, Some(ns)).await.expect("register");
        let err = kernel
            .resolve_pointer(ns, &slice_request("ptr_digest", 1, 64), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Post-gate"), "unexpected: {err}");

        // Observed rows exceed max_slice_rows.
        let mut spy_rows = SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024);
        spy_rows.inflate_rows = true;
        let spy_rows = Arc::new(spy_rows);
        let (kernel_rows, store_rows, _dir) = build_kernel(spy_rows.clone());
        store_rows.init(ns).await.expect("init");
        kernel_rows
            .register_pointer(ns, &pointer_record("ptr_rows", "spy", 1000, 100), Some(ns))
            .await
            .expect("register");
        let err = kernel_rows
            .resolve_pointer(ns, &slice_request("ptr_rows", 1, 64), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Post-gate"), "unexpected: {err}");

        // Observed bytes exceed max_slice_bytes.
        let mut spy_bytes = SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024);
        spy_bytes.inflate_bytes = true;
        let spy_bytes = Arc::new(spy_bytes);
        let (kernel_bytes, store_bytes, _dir) = build_kernel(spy_bytes.clone());
        store_bytes.init(ns).await.expect("init");
        kernel_bytes
            .register_pointer(ns, &pointer_record("ptr_bytes", "spy", 1000, 100), Some(ns))
            .await
            .expect("register");
        let err = kernel_bytes
            .resolve_pointer(ns, &slice_request("ptr_bytes", 1, 64), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Post-gate"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn trusted_pointer_id_resolution_and_writeback_with_active_receipt() {
        let ns = "ns1";
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        store.init(ns).await.expect("init");
        // Register pointer
        kernel
            .register_pointer(ns, &pointer_record("ptr_trusted", "spy", 1000, 100), Some(ns))
            .await
            .expect("register");
        // Seed an active, target-bound action_receipt
        seed_receipt(&store, ns, "rcpt_active", "ptr_trusted", "pool1", false).await;
        // resolve_pointer works
        let slice = kernel
            .resolve_pointer(ns, &slice_request("ptr_trusted", 1, 64), 1000, Some(ns))
            .await
            .expect("resolve");
        assert_eq!(slice.pointer_id, "ptr_trusted");
        // apply_external_effect with active receipt succeeds
        let receipt = kernel
            .apply_external_effect(ns, &writeback_request("ptr_trusted", "rcpt_active"), 1000, Some(ns))
            .await
            .expect("writeback");
        assert!(receipt.applied);
        assert_eq!(spy.apply_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn duplicate_pointer_id_registration_rejected() {
        let ns = "ns1";
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        store.init(ns).await.expect("init");
        // First registration succeeds
        kernel
            .register_pointer(ns, &pointer_record("dup_ptr", "spy", 1000, 100), Some(ns))
            .await
            .expect("first register");
        // Second registration of same pointer_id fails
        let err = kernel
            .register_pointer(ns, &pointer_record("dup_ptr", "spy", 1000, 100), Some(ns))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "unexpected: {err}"
        );
    }

    #[tokio::test]
    async fn mediate_workflow_data_end_to_end() {
        let ns = "ns1";
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        store.init(ns).await.expect("init");
        kernel
            .register_pointer(ns, &pointer_record("ptr_wf", "spy", 1000, 100), Some(ns))
            .await
            .expect("register");
        // Claimed -> Mutating with gate_passed=true should succeed
        let (stage, slice) = kernel
            .mediate_workflow_data(
                ns,
                WorkflowStage::Claimed,
                WorkflowStage::Mutating,
                true,
                &slice_request("ptr_wf", 1, 64),
                1000,
                Some(ns),
            )
            .await
            .expect("mediate");
        assert_eq!(stage, WorkflowStage::Mutating);
        assert_eq!(slice.pointer_id, "ptr_wf");
        assert_eq!(spy.resolve_calls.load(Ordering::SeqCst), 1);
        // Illegal transition (e.g., Claimed -> Claimed with gate_passed=false) should reject and make zero resolve_slice calls
        let err = kernel
            .mediate_workflow_data(
                ns,
                WorkflowStage::Claimed,
                WorkflowStage::Claimed,
                false,
                &slice_request("ptr_wf", 1, 64),
                1000,
                Some(ns),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Workflow transition denied"), "unexpected: {err}");
        assert_eq!(spy.resolve_calls.load(Ordering::SeqCst), 1); // no additional call
    }

    fn now_epoch() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after UNIX epoch")
            .as_secs()
    }

    #[tokio::test]
    async fn write_back_post_gate_compares_expected_digest_against_prior_source() {
        let ns = "ns1";
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        store.init(ns).await.expect("init");
        kernel
            .register_pointer(ns, &pointer_record("ptr_cas", "spy", 1000, 100), Some(ns))
            .await
            .expect("register");
        seed_receipt(&store, ns, "rcpt_cas", "ptr_cas", "pool1", false).await;

        // expected == prior (the CAS digest is the PRE-write source digest) must
        // pass even though the spy returns a different post-write new digest.
        let mut req_ok = writeback_request("ptr_cas", "rcpt_cas");
        req_ok.expected_source_sha256 = Some("prior".to_string());
        let receipt = kernel
            .apply_external_effect(ns, &req_ok, 1000, Some(ns))
            .await
            .expect("expected==prior must pass the post-gate");
        assert!(receipt.applied);

        // expected == the post-write new digest must now be rejected, and any
        // other digest likewise denies at the post-gate.
        let mut req_new = writeback_request("ptr_cas", "rcpt_cas");
        req_new.expected_source_sha256 = Some("new-source-digest".to_string());
        req_new.idempotency_key = "idem-cas-new".to_string();
        let err = kernel
            .apply_external_effect(ns, &req_new, 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Post-gate"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn omitted_now_secs_defaults_to_current_epoch_for_resolve() {
        let now = now_epoch();
        let ns = "ns1";
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        store.init(ns).await.expect("init");
        kernel
            .register_pointer(ns, &pointer_record("ptr_now_resolve", "spy", now, 3600), Some(ns))
            .await
            .expect("register");

        // Explicit now_secs must still be honored verbatim.
        let err = kernel
            .execute_connector_command(
                ns,
                &json!({
                    "operation": "resolve_pointer",
                    "req": {"pointer_id": "ptr_now_resolve", "offset": 0, "limit": 1, "max_bytes": 64},
                    "now_secs": 0
                }),
                Some(ns),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Pre-gate"), "unexpected: {err}");

        // Omitted now_secs must track the current epoch, not 0.
        let value = kernel
            .execute_connector_command(
                ns,
                &json!({
                    "operation": "resolve_pointer",
                    "req": {"pointer_id": "ptr_now_resolve", "offset": 0, "limit": 1, "max_bytes": 64}
                }),
                Some(ns),
            )
            .await
            .expect("omitted now_secs must resolve against the current epoch");
        assert_eq!(value["pointer_id"], "ptr_now_resolve");
    }

    #[tokio::test]
    async fn omitted_now_secs_defaults_to_current_epoch_for_apply() {
        let now = now_epoch();
        let ns = "ns1";
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        store.init(ns).await.expect("init");
        kernel
            .register_pointer(ns, &pointer_record("ptr_now_apply", "spy", now, 3600), Some(ns))
            .await
            .expect("register");
        seed_receipt(&store, ns, "rcpt_now", "ptr_now_apply", "pool1", false).await;

        let value = kernel
            .execute_connector_command(
                ns,
                &json!({
                    "operation": "apply_effect",
                    "req": {
                        "pointer_id": "ptr_now_apply",
                        "operation": "append",
                        "payload": {"v": 3},
                        "action_receipt_id": "rcpt_now",
                        "idempotency_key": "idem-now"
                    }
                }),
                Some(ns),
            )
            .await
            .expect("omitted now_secs must apply against the current epoch");
        assert_eq!(value["applied"], true);
    }

    #[tokio::test]
    async fn omitted_now_secs_defaults_to_current_epoch_for_mediate() {
        let now = now_epoch();
        let ns = "ns1";
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        store.init(ns).await.expect("init");
        kernel
            .register_pointer(ns, &pointer_record("ptr_now_mediate", "spy", now, 3600), Some(ns))
            .await
            .expect("register");

        let value = kernel
            .execute_connector_command(
                ns,
                &json!({
                    "operation": "mediate",
                    "from_stage": "claimed",
                    "to_stage": "mutating",
                    "gate_passed": true,
                    "slice_req": {"pointer_id": "ptr_now_mediate", "offset": 0, "limit": 1, "max_bytes": 64}
                }),
                Some(ns),
            )
            .await
            .expect("omitted now_secs must mediate against the current epoch");
        assert_eq!(value["pointer_id"], "ptr_now_mediate");
    }

    #[tokio::test]
    async fn kernel_construction_and_execution_accept_trait_object_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The serving layers (MCP / daemon / CLI) hold the store as the trait
        // object; the kernel must accept that shape and fail closed on backends
        // that are not the LadybugDB concrete store.
        let store: Arc<dyn VectorStore> = Arc::new(LadybugStore::new(dir.path().join("db"), 4).expect("store init"));
        let bus = Arc::new(ThalamicBus::new(64));
        let kernel = AgentKernel::new(store, bus, std::path::PathBuf::from("."));
        let value = kernel
            .execute_connector_command(
                "NeuroStrata",
                &json!({ "operation": "list_connectors" }),
                None,
            )
            .await
            .expect("list_connectors");
        let ids = value.as_array().expect("connector ids array");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&json!("json_file")));
        assert!(ids.contains(&json!("memory_pool")));
    }

    #[tokio::test]
    async fn post_gate_rejects_rows_exceeding_request_limit_below_schema_cap() {
        struct OverReturningConnector;
        #[async_trait]
        impl DataConnector for OverReturningConnector {
            fn connector_id(&self) -> &str {
                "over_rows"
            }
            async fn describe_pool(&self, _pool_id: &str) -> Result<DataPoolSchema> {
                Ok(DataPoolSchema {
                    connector_id: "over_rows".to_string(),
                    pool_id: "pool1".to_string(),
                    access_mode: DataPoolAccessMode::ReadWrite,
                    entity_types: vec![],
                    max_slice_rows: 10,
                    max_slice_bytes: 1024,
                    description: "over".to_string(),
                })
            }
            async fn resolve_slice(
                &self,
                _pool_id: &str,
                pointer_uri: &str,
                _offset: u32,
                _limit: u32,
                _max_bytes: u32,
            ) -> Result<DataSlicePayload> {
                // Returns 5 rows (<= schema.max_slice_rows=10, but > req.limit=2).
                Ok(DataSlicePayload {
                    pointer_id: "unset".to_string(),
                    connector_id: "over_rows".to_string(),
                    pool_id: "pool1".to_string(),
                    pointer_uri: pointer_uri.to_string(),
                    records: (0..5).map(|i| json!({"i": i})).collect(),
                    payload_bytes: 32,
                    source_sha256: "good-source-digest".to_string(),
                    slice_sha256: "slice-sha".to_string(),
                    observed_at_secs: 0,
                })
            }
            async fn apply_external_effect(
                &self,
                _pool_id: &str,
                _pointer_uri: &str,
                req: &ExternalEffectRequest,
            ) -> Result<ExternalEffectReceipt> {
                Ok(ExternalEffectReceipt {
                    pointer_id: req.pointer_id.clone(),
                    connector_id: "over_rows".to_string(),
                    pool_id: "pool1".to_string(),
                    operation: req.operation.clone(),
                    idempotency_key: req.idempotency_key.clone(),
                    applied: true,
                    prior_source_sha256: "prior".to_string(),
                    new_source_sha256: "new".to_string(),
                    details: "ok".to_string(),
                })
            }
        }

        let ns = "ns1";
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(LadybugStore::new(dir.path().join("db"), 4).expect("store init"));
        let bus = Arc::new(ThalamicBus::new(64));
        let store_trait: Arc<dyn VectorStore> = store.clone();
        let memory: Arc<dyn MemoryPort> = Arc::new(LadybugMemoryPort::new(store_trait.clone()));
        let executive: Arc<dyn ExecutivePort> = Arc::new(LadybugExecutivePort::new(store_trait.clone()));
        let ontology: Arc<dyn OntologyPort> = Arc::new(LadybugOntologyPort::new(store_trait.clone()));
        let workflows = Arc::new(WorkflowRegistry::new());
        let connectors = DataConnectorRegistry::new();
        connectors.register(Arc::new(OverReturningConnector)).expect("register");
        let kernel = AgentKernel::with_ports(
            store_trait,
            bus,
            memory,
            executive,
            ontology,
            workflows,
            Arc::new(connectors),
        );
        store.init(ns).await.expect("init");
        kernel
            .register_pointer(ns, &pointer_record("ptr_over", "over_rows", 1000, 100), Some(ns))
            .await
            .expect("register");
        let err = kernel
            .resolve_pointer(ns, &slice_request("ptr_over", 2, 256), 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("DenyObservedBoundsExceeded"),
            "expected DenyObservedBoundsExceeded, got: {err}"
        );
    }

    #[tokio::test]
    async fn write_back_post_gate_enforces_pointer_digest_and_cas_conflict_distinctly() {
        let ns = "ns1";
        let spy = Arc::new(SpyConnector::new("spy", DataPoolAccessMode::ReadWrite, 10, 1024));
        let (kernel, store, _dir) = build_kernel(spy.clone());
        store.init(ns).await.expect("init");

        // 1. Pointer pins expected_source_sha256 = "pinned-sha" (!= receipt.prior_source_sha256 "prior").
        // Even when req.expected_source_sha256 is None, post-gate must reject with DenySourceDigestMismatch.
        let mut pinned_ptr = pointer_record("ptr_pinned_mismatch", "spy", 1000, 100);
        pinned_ptr.expected_source_sha256 = Some("pinned-sha".to_string());
        kernel
            .register_pointer(ns, &pinned_ptr, Some(ns))
            .await
            .expect("register");
        seed_receipt(&store, ns, "rcpt_pinned", "ptr_pinned_mismatch", "pool1", false).await;
        let err = kernel
            .apply_external_effect(
                ns,
                &writeback_request("ptr_pinned_mismatch", "rcpt_pinned"),
                1000,
                Some(ns),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("DenySourceDigestMismatch"),
            "expected DenySourceDigestMismatch, got: {err}"
        );

        // 2. Pointer digest matches ("prior"), but request CAS expected_source_sha256 mismatches ("wrong-cas").
        // Post-gate must reject with DenyCasConflict.
        let mut matching_ptr = pointer_record("ptr_cas_conflict", "spy", 1000, 100);
        matching_ptr.expected_source_sha256 = Some("prior".to_string());
        kernel
            .register_pointer(ns, &matching_ptr, Some(ns))
            .await
            .expect("register");
        seed_receipt(&store, ns, "rcpt_cas_conflict", "ptr_cas_conflict", "pool1", false).await;
        let mut cas_req = writeback_request("ptr_cas_conflict", "rcpt_cas_conflict");
        cas_req.expected_source_sha256 = Some("wrong-cas".to_string());
        let err = kernel
            .apply_external_effect(ns, &cas_req, 1000, Some(ns))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("DenyCasConflict"),
            "expected DenyCasConflict, got: {err}"
        );
    }
}
