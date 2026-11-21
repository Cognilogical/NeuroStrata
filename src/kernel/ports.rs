//! Principal-scoped internal kernel domain ports (spec §3.2).
//!
//! `AgentKernel` never exposes the raw `LadybugStore`; every read and write
//! goes through one of these three async traits, and each implementation
//! evaluates ReBAC before touching storage. `MemoryPort` owns the private
//! `data_pointer` registry, `ExecutivePort` wraps the task ledger and the
//! workflow state machine, and `OntologyPort` answers ontology queries and
//! verifies kinetic action receipts.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::connectors::{
    DataPointerRecord, DataPoolSchema, ExternalEffectReceipt, WriteBackSnapshot,
};
use crate::formal_kernel::KernelActionReceiptState;
use crate::rebac::can_read_memory;
use crate::traits::{MemoryPayload, OntologyStore, TaskStore, TaskSummaryRow, VectorStore};
use crate::workflow::machine::WorkflowStage;

/// Kernel workflow verdict – allows or denies a transition based on gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelWorkflowTransitionVerdict {
    /// Transition allowed by gate condition.
    AllowTransition,
    /// Transition denied by gate condition.
    DenyTransition,
}

/// §3.2.1 – In‑memory recall, data pointer registry, and private store view.
#[async_trait]
pub trait MemoryPort: Send + Sync {
    /// Retrieve one memory payload under ReBAC.
    async fn get_visible_memory(
        &self,
        namespace: &str,
        id: &str,
        principal: Option<&str>,
    ) -> Result<Option<MemoryPayload>>;

    /// Register a new `data_pointer` in the private registry.
    async fn register_data_pointer(
        &self,
        namespace: &str,
        record: &DataPointerRecord,
        principal: Option<&str>,
    ) -> Result<DataPointerRecord>;

    /// Load a pointer for trusted kernel use (no ReBAC) or for caller (ReBAC).
    async fn get_data_pointer(
        &self,
        namespace: &str,
        pointer_id: &str,
        principal: Option<&str>,
    ) -> Result<Option<DataPointerRecord>>;

    /// List pointers, optionally filtered by pool id and ReBAC.
    async fn list_data_pointers(
        &self,
        namespace: &str,
        pool_id: Option<&str>,
        principal: Option<&str>,
    ) -> Result<Vec<DataPointerRecord>>;

    /// Persist a `DataPoolSchema` (and optional initial in-memory records) in the private registry.
    async fn register_data_pool(
        &self,
        namespace: &str,
        schema: &DataPoolSchema,
        records: Option<&HashMap<String, Vec<Value>>>,
        principal: Option<&str>,
    ) -> Result<DataPoolSchema> {
        let _ = (namespace, records, principal);
        Ok(schema.clone())
    }

    /// Merge a connector write-back snapshot into the persisted pool state.
    /// The snapshot carries the written pointer's prior/post records, so a
    /// stored row that no longer matches is a concurrent-write conflict.
    async fn register_pool_writeback(
        &self,
        namespace: &str,
        schema: &DataPoolSchema,
        pointer_uri: &str,
        snapshot: &WriteBackSnapshot,
        principal: Option<&str>,
    ) -> Result<()> {
        let _ = (namespace, schema, pointer_uri, snapshot, principal);
        Ok(())
    }

    /// Persisted idempotency receipts for a pool (empty when none).
    async fn pool_idempotency(
        &self,
        namespace: &str,
        schema: &DataPoolSchema,
    ) -> Result<HashMap<String, ExternalEffectReceipt>> {
        let _ = (namespace, schema);
        Ok(HashMap::new())
    }

    /// List persisted `DataPoolSchema` entries in the namespace, optionally filtered by `connector_id`.
    async fn list_data_pools(
        &self,
        namespace: &str,
        connector_id: Option<&str>,
        principal: Option<&str>,
    ) -> Result<Vec<(DataPoolSchema, Option<HashMap<String, Vec<Value>>>)>> {
        let _ = (namespace, connector_id, principal);
        Ok(Vec::new())
    }
}

/// §3.2.2 – Executive interface over tasks and workflow stage transitions.
#[async_trait]
pub trait ExecutivePort: Send + Sync {
    /// List tasks visible to the principal.
    async fn list_tasks(
        &self,
        namespace: &str,
        principal: Option<&str>,
    ) -> Result<Vec<TaskSummaryRow>>;

    /// Load a single task row under ReBAC.
    async fn get_task(
        &self,
        namespace: &str,
        task_id: &str,
        principal: Option<&str>,
    ) -> Result<Option<MemoryPayload>>;

    /// Count live extractions for a task (Lock‑2 enforcement).
    async fn verify_task_extraction_count(&self, namespace: &str, task_id: &str) -> Result<usize>;

    /// Evaluate a workflow transition as a high‑level gate. The `gate_passed`
    /// boolean represents whether the specific guard for the transition is
    /// satisfied (e.g., `has_valid_scope`, `mutations_since_note_after`).
    async fn evaluate_workflow_transition(
        &self,
        from: WorkflowStage,
        to: WorkflowStage,
        gate_passed: bool,
    ) -> KernelWorkflowTransitionVerdict;
}

/// §3.2.3 – Ontology interface over action receipts and static entities.
#[async_trait]
pub trait OntologyPort: Send + Sync {
    /// List ontology entities under ReBAC filtering.
    async fn list_ontology_entities(
        &self,
        namespace: &str,
        memory_types: &[&str],
        principal: Option<&str>,
    ) -> Result<Vec<(String, MemoryPayload)>>;

    /// Load a single ontology entity under ReBAC.
    async fn get_ontology_entity(
        &self,
        namespace: &str,
        id: &str,
        principal: Option<&str>,
    ) -> Result<Option<MemoryPayload>>;

    /// Inspect an action receipt’s kinetic state (bound, reverted, etc.).
    async fn verify_active_action_receipt(
        &self,
        namespace: &str,
        receipt_id: &str,
        pointer_id: &str,
        pool_id: &str,
    ) -> Result<KernelActionReceiptState>;

    /// Zero-mutation ontology change-impact plan (spec §3.2). Read-only: never
    /// commits to the store, scenarios, tasks, edges or the ontology cache.
    async fn plan_ontology_diff(
        &self,
        namespace: &str,
        request: &crate::rdf::ontology::OntologyPlanRequest,
        principal: Option<&str>,
    ) -> Result<crate::rdf::ontology::OntologyPlanReport>;
}

/// Persistent `MemoryPort` backed by `LadybugStore`.
pub struct LadybugMemoryPort {
    store: Arc<dyn VectorStore>,
}

impl LadybugMemoryPort {
    /// Create a new `LadybugMemoryPort` backed by the given store.
    pub fn new(store: Arc<dyn VectorStore>) -> Self {
        // Fail closed on non-Ladybug backends.
        crate::traits::VectorStore::as_any(store.as_ref())
            .downcast_ref::<crate::store::ladybug::LadybugStore>()
            .expect("memory port requires the LadybugDB store backend");
        LadybugMemoryPort { store }
    }

    /// Trusted internal path – no authentication, no ReBAC.
    async fn trusted_get_memory(&self, namespace: &str, id: &str) -> Result<Option<MemoryPayload>> {
        let lb = self.ladybug_store();
        lb.get_memory_by_id(namespace, id).await
    }

    /// ReBAC‑checked path – returns `None` when principal lacks visibility.
    async fn rebac_get_memory(
        &self,
        namespace: &str,
        id: &str,
        principal: Option<&str>,
    ) -> Result<Option<MemoryPayload>> {
        let lb = self.ladybug_store();
        let Some(payload) = lb.get_memory_by_id(namespace, id).await? else {
            return Ok(None);
        };
        let visible = match principal {
            None => false, // unauthenticated callers may read nothing
            Some(p) => can_read_memory(p, payload.user_id.as_str(), &payload.metadata),
        };
        Ok(visible.then(|| payload))
    }

    fn ladybug_store(&self) -> &crate::store::ladybug::LadybugStore {
        crate::traits::VectorStore::as_any(self.store.as_ref())
            .downcast_ref::<crate::store::ladybug::LadybugStore>()
            .expect("memory port requires the LadybugDB store backend")
    }
}

/// Returns `true` when a `data_pointer` / pool row is retired or inactive:
/// - `valid_to` is expired (`store::ladybug::is_retired_at(metadata, now)`),
/// - `archived == true`,
/// - `retired == true`, or
/// - `superseded_by` is a non-blank string.
fn is_retired_data_metadata(metadata: &Value, now: i64) -> bool {
    if crate::store::ladybug::is_retired_at(metadata, now) {
        return true;
    }
    if metadata
        .get("archived")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if metadata
        .get("retired")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if metadata
        .get("superseded_by")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
    {
        return true;
    }
    false
}

#[async_trait]
impl MemoryPort for LadybugMemoryPort {
    async fn get_visible_memory(
        &self,
        namespace: &str,
        id: &str,
        principal: Option<&str>,
    ) -> Result<Option<MemoryPayload>> {
        // None means trusted kernel internal read – bypass ReBAC.
        match principal {
            None => self.trusted_get_memory(namespace, id).await,
            Some(p) => self.rebac_get_memory(namespace, id, Some(p)).await,
        }
    }

    async fn register_data_pointer(
        &self,
        namespace: &str,
        record: &DataPointerRecord,
        principal: Option<&str>,
    ) -> Result<DataPointerRecord> {
        let lb = self.ladybug_store();
        // Namespace‑level write gate via ReBAC (§5.4 / §5.6).
        if !crate::rebac::can_write_namespace(lb, namespace, principal) {
            anyhow::bail!("write denied for namespace '{}'", namespace);
        }
        // Reject duplicate active pointer IDs in the namespace.
        if let Some(existing) = self
            .trusted_get_memory(namespace, &record.pointer_id)
            .await?
        {
            if !is_retired_data_metadata(&existing.metadata, chrono::Utc::now().timestamp()) {
                anyhow::bail!("pointer_id '{}' already exists", record.pointer_id);
            }
        }
        let metadata = serde_json::json!({
            "eligibility": "private",
            "pointer": serde_json::to_value(record)?,
            "source": {
                "kind": "data_connector",
                "ref": record.pointer_uri,
                "captured_at": record.captured_at_secs.to_string()
            }
        });
        // Store owner = authenticated principal; empty → unauthenticated (private data pointer).
        let user_id = principal.unwrap_or("");
        let payload = MemoryPayload {
            content: format!(
                "data pointer {} -> {}",
                record.pointer_id, record.pointer_uri
            ),
            user_id: user_id.to_string(),
            memory_type: "data_pointer".to_string(),
            agent_name: None,
            location: record.pointer_uri.clone(),
            location_lines: String::new(),
            metadata,
        };
        self.ladybug_store()
            .put_scoped_memory(namespace, &record.pointer_id, &payload)
            .await?;
        Ok(record.clone())
    }

    async fn get_data_pointer(
        &self,
        namespace: &str,
        pointer_id: &str,
        principal: Option<&str>,
    ) -> Result<Option<DataPointerRecord>> {
        let Some(payload) = self.trusted_get_memory(namespace, pointer_id).await? else {
            return Ok(None);
        };
        if payload.memory_type != "data_pointer"
            || is_retired_data_metadata(&payload.metadata, chrono::Utc::now().timestamp())
        {
            return Ok(None);
        }
        let Some(pointer_val) = payload.metadata.get("pointer") else {
            return Ok(None);
        };
        // Trusted internal path: `principal == None` returns the raw record without ReBAC;
        // otherwise enforce `can_read_memory`.
        let visible = match principal {
            None => true,
            Some(p) => can_read_memory(p, payload.user_id.as_str(), &payload.metadata),
        };
        if !visible {
            return Ok(None);
        }
        let record: DataPointerRecord = serde_json::from_value(pointer_val.clone())?;
        Ok(Some(record))
    }

    async fn list_data_pointers(
        &self,
        namespace: &str,
        pool_id: Option<&str>,
        principal: Option<&str>,
    ) -> Result<Vec<DataPointerRecord>> {
        let mut out = Vec::new();
        let lb = self.ladybug_store();
        // Retired rows are history, not live connector locators. Capture the
        // query instant before materializing rows for consistent filtering.
        let now = chrono::Utc::now().timestamp();
        // List all memory_type == "data_pointer" in the namespace.
        let rows = lb
            .list_memories_by_types(namespace, &["data_pointer"])
            .await?;
        for (_id, payload) in rows {
            if is_retired_data_metadata(&payload.metadata, now) {
                continue;
            }
            let Some(pointer_val) = payload.metadata.get("pointer") else {
                continue;
            };
            // ReBAC filtering: principal == None => trusted (include all) else require can_read_memory.
            let include = match principal {
                None => true,
                Some(p) => can_read_memory(p, payload.user_id.as_str(), &payload.metadata),
            };
            if !include {
                continue;
            }
            let rec: DataPointerRecord = serde_json::from_value(pointer_val.clone())?;
            if let Some(pid) = pool_id {
                if rec.pool_id != pid {
                    continue;
                }
            }
            out.push(rec);
        }
        out.sort_by(|a, b| a.pointer_id.cmp(&b.pointer_id));
        Ok(out)
    }

    async fn register_data_pool(
        &self,
        namespace: &str,
        schema: &DataPoolSchema,
        records: Option<&HashMap<String, Vec<Value>>>,
        principal: Option<&str>,
    ) -> Result<DataPoolSchema> {
        let lb = self.ladybug_store();
        if !crate::rebac::can_write_namespace(lb, namespace, principal) {
            anyhow::bail!("write denied for namespace '{}'", namespace);
        }
        let row_id = format!(
            "__data_pool__:{}:{}:{}",
            namespace, schema.connector_id, schema.pool_id
        );
        let now = chrono::Utc::now().timestamp();
        let existing = self
            .trusted_get_memory(namespace, &row_id)
            .await?
            .filter(|p| !is_retired_data_metadata(&p.metadata, now));
        let existing_records = if records.is_none() {
            existing
                .as_ref()
                .and_then(|p| p.metadata.get("pool_records").cloned())
        } else {
            None
        };
        // Review 490f0340#r4650391606: a schema-only update retains the
        // records, so it must retain the receipts bound to them as well.
        let existing_idempotency = if records.is_none() {
            existing
                .as_ref()
                .and_then(|p| p.metadata.get("pool_idempotency").cloned())
        } else {
            None
        };
        let mut metadata = serde_json::json!({
            "eligibility": "private",
            "pool_schema": serde_json::to_value(schema)?,
            "source": {
                "kind": "data_connector_pool",
                "ref": format!("{}:{}", schema.connector_id, schema.pool_id)
            }
        });
        if let Some(recs) = records {
            metadata["pool_records"] = serde_json::to_value(recs)?;
        } else if let Some(prev_recs) = existing_records {
            metadata["pool_records"] = prev_recs;
        }
        if let Some(prev_idempotency) = existing_idempotency {
            metadata["pool_idempotency"] = prev_idempotency;
        }
        let user_id = principal.unwrap_or("");
        let payload = MemoryPayload {
            content: format!(
                "data pool {}:{} ({})",
                schema.connector_id, schema.pool_id, schema.description
            ),
            user_id: user_id.to_string(),
            memory_type: "data_pointer".to_string(),
            agent_name: None,
            location: format!("{}:{}", schema.connector_id, schema.pool_id),
            location_lines: String::new(),
            metadata,
        };
        lb.put_scoped_memory(namespace, &row_id, &payload).await?;
        Ok(schema.clone())
    }

    async fn register_pool_writeback(
        &self,
        namespace: &str,
        schema: &DataPoolSchema,
        pointer_uri: &str,
        snapshot: &WriteBackSnapshot,
        principal: Option<&str>,
    ) -> Result<()> {
        let lb = self.ladybug_store();
        if !crate::rebac::can_write_namespace(lb, namespace, principal) {
            anyhow::bail!("write denied for namespace '{}'", namespace);
        }
        let row_id = format!(
            "__data_pool__:{}:{}:{}",
            namespace, schema.connector_id, schema.pool_id
        );
        let now = chrono::Utc::now().timestamp();
        let existing = self
            .trusted_get_memory(namespace, &row_id)
            .await?
            .filter(|p| !is_retired_data_metadata(&p.metadata, now));
        let mut records: HashMap<String, Vec<Value>> = existing
            .as_ref()
            .and_then(|p| p.metadata.get("pool_records"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let mut stored_idempotency: HashMap<String, ExternalEffectReceipt> = existing
            .as_ref()
            .and_then(|p| p.metadata.get("pool_idempotency"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        // Compare only the pointer this write-back touched: other pointers
        // are merged, so concurrent kernels writing different pointers both
        // survive. A same-pointer concurrent write is a conflict, not a last-
        // writer-wins overwrite.
        if let Some(current) = records.get(pointer_uri) {
            if current.as_slice() != snapshot.prior_records.as_slice() {
                anyhow::bail!(
                    "concurrent pool write conflict for pointer '{}'",
                    pointer_uri
                );
            }
        }
        records.insert(pointer_uri.to_string(), snapshot.pointer_records.clone());
        stored_idempotency.extend(
            snapshot
                .idempotency
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );

        let metadata = serde_json::json!({
            "eligibility": "private",
            "pool_schema": serde_json::to_value(schema)?,
            "pool_records": serde_json::to_value(&records)?,
            "pool_idempotency": serde_json::to_value(&stored_idempotency)?,
            "source": {
                "kind": "data_connector_pool",
                "ref": format!("{}:{}", schema.connector_id, schema.pool_id)
            }
        });
        let payload = MemoryPayload {
            content: format!(
                "data pool {}:{} ({})",
                schema.connector_id, schema.pool_id, schema.description
            ),
            user_id: principal.unwrap_or("").to_string(),
            memory_type: "data_pointer".to_string(),
            agent_name: None,
            location: format!("{}:{}", schema.connector_id, schema.pool_id),
            location_lines: String::new(),
            metadata,
        };
        lb.put_scoped_memory(namespace, &row_id, &payload).await?;
        Ok(())
    }

    async fn pool_idempotency(
        &self,
        namespace: &str,
        schema: &DataPoolSchema,
    ) -> Result<HashMap<String, ExternalEffectReceipt>> {
        let row_id = format!(
            "__data_pool__:{}:{}:{}",
            namespace, schema.connector_id, schema.pool_id
        );
        let now = chrono::Utc::now().timestamp();
        let Some(row) = self.trusted_get_memory(namespace, &row_id).await? else {
            return Ok(HashMap::new());
        };
        if is_retired_data_metadata(&row.metadata, now) {
            return Ok(HashMap::new());
        }
        Ok(row
            .metadata
            .get("pool_idempotency")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default())
    }

    async fn list_data_pools(
        &self,
        namespace: &str,
        connector_id: Option<&str>,
        principal: Option<&str>,
    ) -> Result<Vec<(DataPoolSchema, Option<HashMap<String, Vec<Value>>>)>> {
        let mut out = Vec::new();
        let lb = self.ladybug_store();
        let now = chrono::Utc::now().timestamp();
        let rows = lb
            .list_memories_by_types(namespace, &["data_pointer"])
            .await?;
        for (_id, payload) in rows {
            if is_retired_data_metadata(&payload.metadata, now) {
                continue;
            }
            let Some(schema_val) = payload.metadata.get("pool_schema") else {
                continue;
            };
            let include = match principal {
                None => true,
                Some(p) => can_read_memory(p, payload.user_id.as_str(), &payload.metadata),
            };
            if !include {
                continue;
            }
            let schema: DataPoolSchema = serde_json::from_value(schema_val.clone())?;
            if let Some(cid) = connector_id {
                if schema.connector_id != cid {
                    continue;
                }
            }
            let records = payload
                .metadata
                .get("pool_records")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            out.push((schema, records));
        }
        out.sort_by(|a, b| {
            a.0.connector_id
                .cmp(&b.0.connector_id)
                .then_with(|| a.0.pool_id.cmp(&b.0.pool_id))
        });
        Ok(out)
    }
}

/// Persistent `ExecutivePort` backed by `LadybugStore`.
pub struct LadybugExecutivePort {
    store: Arc<dyn VectorStore>,
}

impl LadybugExecutivePort {
    /// Create a new `LadybugExecutivePort` backed by the given store.
    pub fn new(store: Arc<dyn VectorStore>) -> Self {
        // Fail closed on non-Ladybug backends.
        crate::traits::VectorStore::as_any(store.as_ref())
            .downcast_ref::<crate::store::ladybug::LadybugStore>()
            .expect("executive port requires the LadybugDB store backend");
        LadybugExecutivePort { store }
    }

    fn transition_event(
        from: WorkflowStage,
        to: WorkflowStage,
        gate_passed: bool,
    ) -> crate::workflow::machine::WorkflowEvent {
        use crate::workflow::machine::WorkflowEvent as WE;
        use WorkflowStage::*;
        // Map the simplified (from, to, gate_passed) abstraction to the richer WorkflowEvent.
        match (from, to) {
            (Unpreflighted, ContextLoaded) => WE::LoadContext,
            (ContextLoaded, ContextLoaded) => WE::LoadContext,
            (Claimed | Mutating, Claimed) => WE::ClaimTask {
                has_valid_scope: gate_passed,
            },
            (Claimed | Mutating, Mutating) => WE::Mutate {
                mutations_since_note_after: 0,
                breath_limit: 5,
            },
            (Claimed | Mutating, BreathRequired) => WE::Mutate {
                mutations_since_note_after: 5,
                breath_limit: 5,
            },
            (BreathRequired, Mutating) => WE::RecordBreathNote,
            (Claimed | Mutating, PreCheckpoint) => WE::TriggerPreCheckpoint {
                has_receipt: gate_passed,
            },
            (PreCheckpoint, Mutating) => WE::CheckpointPassed,
            (Claimed | Mutating | PreCheckpoint, PrePublish) => WE::TriggerPrePublish {
                task_gate_ok: gate_passed,
                doc_gate_ok: gate_passed,
            },
            (PrePublish, Mutating) => WE::PublishPassed,
            (Claimed | Mutating | PreCheckpoint | PrePublish, Completed) => WE::CompleteTask {
                has_extracted_memory: gate_passed,
                dirty_targets: 0,
            },
            _ => WE::LoadContext,
        }
    }

    fn ladybug_store(&self) -> &crate::store::ladybug::LadybugStore {
        crate::traits::VectorStore::as_any(self.store.as_ref())
            .downcast_ref::<crate::store::ladybug::LadybugStore>()
            .expect("executive port requires the LadybugDB store backend")
    }
}

#[async_trait]
impl ExecutivePort for LadybugExecutivePort {
    async fn list_tasks(
        &self,
        namespace: &str,
        principal: Option<&str>,
    ) -> Result<Vec<TaskSummaryRow>> {
        let lb = self.ladybug_store();
        let rows = lb.list_task_rows(namespace).await?;
        let mut filtered = Vec::new();
        for row in rows {
            let visible = match principal {
                None => false,
                Some(p) => can_read_memory(p, row.payload.user_id.as_str(), &row.payload.metadata),
            };
            if visible {
                filtered.push(row);
            }
        }
        Ok(filtered)
    }

    async fn get_task(
        &self,
        namespace: &str,
        task_id: &str,
        principal: Option<&str>,
    ) -> Result<Option<MemoryPayload>> {
        if let Some(payload) = self
            .ladybug_store()
            .load_task_row(namespace, task_id)
            .await?
        {
            let visible = match principal {
                None => false,
                Some(p) => can_read_memory(p, payload.user_id.as_str(), &payload.metadata),
            };
            if visible {
                Ok(Some(payload))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    async fn verify_task_extraction_count(&self, namespace: &str, task_id: &str) -> Result<usize> {
        self.ladybug_store()
            .count_extracted_memories_for_task(namespace, task_id)
            .await
    }

    async fn evaluate_workflow_transition(
        &self,
        from: WorkflowStage,
        to: WorkflowStage,
        gate_passed: bool,
    ) -> KernelWorkflowTransitionVerdict {
        let event = Self::transition_event(from, to, gate_passed);
        let outcome = crate::workflow::machine::evaluate_workflow_transition(from, event);
        if outcome.allowed && outcome.to == Some(to) {
            KernelWorkflowTransitionVerdict::AllowTransition
        } else {
            KernelWorkflowTransitionVerdict::DenyTransition
        }
    }
}

/// Persistent `OntologyPort` backed by `LadybugStore`.
pub struct LadybugOntologyPort {
    store: Arc<dyn VectorStore>,
}

impl LadybugOntologyPort {
    /// Create a new `LadybugOntologyPort` backed by the given store.
    pub fn new(store: Arc<dyn VectorStore>) -> Self {
        // Fail closed on non-Ladybug backends.
        crate::traits::VectorStore::as_any(store.as_ref())
            .downcast_ref::<crate::store::ladybug::LadybugStore>()
            .expect("ontology port requires the LadybugDB store backend");
        LadybugOntologyPort { store }
    }

    fn ladybug_store(&self) -> &crate::store::ladybug::LadybugStore {
        crate::traits::VectorStore::as_any(self.store.as_ref())
            .downcast_ref::<crate::store::ladybug::LadybugStore>()
            .expect("ontology port requires the LadybugDB store backend")
    }
}

#[async_trait]
impl OntologyPort for LadybugOntologyPort {
    async fn list_ontology_entities(
        &self,
        namespace: &str,
        memory_types: &[&str],
        principal: Option<&str>,
    ) -> Result<Vec<(String, MemoryPayload)>> {
        let lb = self.ladybug_store();
        let mut rows = lb.list_memories_by_types(namespace, memory_types).await?;
        // ReBAC filtering – if principal is None, treat as unauthenticated (no reads).
        if let Some(p) = principal {
            rows.retain(|(_, payload)| {
                can_read_memory(p, payload.user_id.as_str(), &payload.metadata)
            });
        } else {
            rows.clear();
        }
        Ok(rows)
    }

    async fn get_ontology_entity(
        &self,
        namespace: &str,
        id: &str,
        principal: Option<&str>,
    ) -> Result<Option<MemoryPayload>> {
        let Some(payload) = self.ladybug_store().get_memory_by_id(namespace, id).await? else {
            return Ok(None);
        };
        let visible = match principal {
            None => false,
            Some(p) => can_read_memory(p, payload.user_id.as_str(), &payload.metadata),
        };
        Ok(visible.then_some(payload))
    }

    async fn verify_active_action_receipt(
        &self,
        namespace: &str,
        receipt_id: &str,
        pointer_id: &str,
        pool_id: &str,
    ) -> Result<KernelActionReceiptState> {
        if receipt_id.trim().is_empty() {
            return Ok(KernelActionReceiptState::MissingOrNotFound);
        }
        let Some(payload) = self
            .ladybug_store()
            .get_memory_by_id(namespace, receipt_id)
            .await?
        else {
            return Ok(KernelActionReceiptState::MissingOrNotFound);
        };
        if payload.memory_type != "action_receipt" {
            return Ok(KernelActionReceiptState::MissingOrNotFound);
        }
        if payload
            .metadata
            .get("reverted_at")
            .is_some_and(|v| !v.is_null())
            || payload
                .metadata
                .get("valid_to")
                .is_some_and(|v| !v.is_null())
            || payload.metadata.get("tx_to").is_some_and(|v| !v.is_null())
        {
            return Ok(KernelActionReceiptState::RevertedOrSuperseded);
        }
        let (metadata_pointer_id, metadata_pool_id) = match (
            payload.metadata.get("pointer_id").and_then(|v| v.as_str()),
            payload.metadata.get("pool_id").and_then(|v| v.as_str()),
        ) {
            (Some(pid), Some(p)) => (pid, p),
            _ => return Ok(KernelActionReceiptState::TargetMismatch),
        };
        if metadata_pointer_id == pointer_id && metadata_pool_id == pool_id {
            return Ok(KernelActionReceiptState::ActiveBound);
        }
        if let Some(target_pointer) = payload
            .metadata
            .get("target_pointer_id")
            .and_then(|v| v.as_str())
        {
            if let Some(target_receipt) = payload.metadata.get("receipt").and_then(|v| v.as_str()) {
                if target_pointer == pointer_id && target_receipt == receipt_id {
                    return Ok(KernelActionReceiptState::ActiveBound);
                }
            }
        }
        Ok(KernelActionReceiptState::TargetMismatch)
    }

    async fn plan_ontology_diff(
        &self,
        namespace: &str,
        request: &crate::rdf::ontology::OntologyPlanRequest,
        principal: Option<&str>,
    ) -> Result<crate::rdf::ontology::OntologyPlanReport> {
        crate::rdf::scenario::plan_ontology_diff(
            self.ladybug_store(),
            namespace,
            request,
            principal,
        )
        .await
        .map_err(anyhow::Error::msg)
    }
}
