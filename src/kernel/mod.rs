//! Central Agentic Kernel – Mediates between ports, workflow, and connectors.

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::connectors::{
    DataConnectorRegistry, DataPoolAccessMode, DataPointerRecord, DataSlicePayload,
    DataSliceRequest, ExternalEffectRequest, ExternalEffectReceipt,
};
use crate::events::ThalamicBus;
use crate::formal_kernel::{
    evaluate_connector_post_gate, evaluate_connector_pre_gate, KernelActionReceiptState,
    KernelConnectorOp, KernelConnectorPostGateInput, KernelConnectorPreGateInput,
    KernelConnectorPreVerdict, KernelConnectorPostVerdict,
};
use crate::kernel::ports::{
    ExecutivePort, KernelWorkflowTransitionVerdict, LadybugExecutivePort, LadybugMemoryPort,
    LadybugOntologyPort, MemoryPort, OntologyPort,
};
use crate::store::LadybugStore;
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
    store: Arc<LadybugStore>,
    memory: Arc<dyn MemoryPort>,
    executive: Arc<dyn ExecutivePort>,
    ontology: Arc<dyn OntologyPort>,
    workflows: Arc<WorkflowRegistry>,
    connectors: Arc<DataConnectorRegistry>,
    bus: Arc<ThalamicBus>,
}

impl AgentKernel {
    /// Construct kernel with default ports/registry.
    pub fn new(
        store: Arc<LadybugStore>,
        bus: Arc<ThalamicBus>,
        workspace_root: PathBuf,
    ) -> Self {
        let workflows = Arc::new(WorkflowRegistry::new_with_defaults(workspace_root));
        let memory: Arc<dyn MemoryPort> = Arc::new(LadybugMemoryPort {
            store: store.clone(),
        });
        let executive: Arc<dyn ExecutivePort> = Arc::new(LadybugExecutivePort {
            store: store.clone(),
        });
        let ontology: Arc<dyn OntologyPort> = Arc::new(LadybugOntologyPort {
            store: store.clone(),
        });
        AgentKernel {
            store,
            memory,
            executive,
            ontology,
            workflows,
            connectors: Arc::new(DataConnectorRegistry::new()),
            bus,
        }
    }

    pub fn with_ports(
        store: Arc<LadybugStore>,
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
        let rebac_allowed = crate::rebac::can_write_namespace(self.store.as_ref(), namespace, principal);
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
        let rebac_allowed = self
            .memory
            .get_data_pointer(namespace, &req.pointer_id, principal)
            .await?
            .is_some();
        let connector = self
            .connectors
            .get(&pointer.connector_id)
            .ok_or_else(|| anyhow::anyhow!("connector '{}' not registered", pointer.connector_id))?;
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
        let source_digest_matches = pointer.expected_source_sha256.as_ref()
            .map(|exp| exp == &slice.source_sha256)
            .unwrap_or(true);
        let post_input = KernelConnectorPostGateInput {
            op: KernelConnectorOp::ResolveSlice,
            observed_rows: slice.records.len() as u32,
            max_slice_rows: schema.max_slice_rows,
            observed_bytes: slice.payload_bytes,
            max_slice_bytes: req.max_bytes.min(schema.max_slice_bytes),
            source_digest_matches,
            cas_expected_matches: true,
        };
        if evaluate_connector_post_gate(post_input) != KernelConnectorPostVerdict::AllowPostOutput {
            anyhow::bail!("Post-gate denied slice resolution");
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
        let rebac_allowed = crate::rebac::can_write_namespace(self.store.as_ref(), namespace, principal);
        let receipt_state = self
            .ontology
            .verify_active_action_receipt(namespace, &req.action_receipt_id, &pointer.pointer_id, &pointer.pool_id)
            .await?;
        let connector = self
            .connectors
            .get(&pointer.connector_id)
            .ok_or_else(|| anyhow::anyhow!("connector '{}' not registered", pointer.connector_id))?;
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
        let source_digest_matches = req.expected_source_sha256.as_ref()
            .map(|exp| exp == &receipt.new_source_sha256)
            .unwrap_or(true);
        let post_input = KernelConnectorPostGateInput {
            op: KernelConnectorOp::ApplyWriteBack,
            observed_rows: 0,
            max_slice_rows: schema.max_slice_rows,
            observed_bytes: 0,
            max_slice_bytes: schema.max_slice_bytes,
            source_digest_matches,
            cas_expected_matches: true,
        };
        if evaluate_connector_post_gate(post_input) != KernelConnectorPostVerdict::AllowPostOutput {
            anyhow::bail!("Post-gate denied write-back");
        }
        let mut receipt = receipt;
        receipt.pointer_id = pointer.pointer_id.clone();
        Ok(receipt)
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
