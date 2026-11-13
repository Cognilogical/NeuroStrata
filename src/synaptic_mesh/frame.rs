//! NeuroMesh signed wire envelope (`MeshFrame`), compact capability token
//! (`nsmesh1...`), and 3-gate sovereignty filter.

use std::collections::BTreeMap;

use openssl::pkey::{PKey, Private};
use serde::{Deserialize, Serialize};

use crate::did::{
    canonical_signing_bytes, decode_base58btc, decode_base64, encode_base58btc, encode_base64,
    public_key_from_did, sign, signer_did, verify,
};
use crate::formal_kernel::{evaluate_sovereignty_gate, KernelSovereigntyDecision};
use crate::synaptic_mesh::record::MeshRecord;

/// Maximum wire frame size (1 MiB).
pub const MAX_MESH_FRAME_BYTES: usize = 1_048_576;

/// Prefix for compact NeuroMesh capability invite tokens.
pub const INVITE_TOKEN_PREFIX: &str = "nsmesh1";

/// Procedural memory runtime counters that are strictly node-local and must
/// never cross `MeshTransport`.
pub const FORBIDDEN_PROCEDURE_COUNTER_KEYS: &[&str] = &[
    "remaining_fires",
    "performance_count",
    "last_performed_at",
    "last_episodic_pointer",
];

/// Memory types permitted to cross NeuroMesh when `eligibility == "shareable"`.
/// Tasks (`"task"`), `"structural"` AST nodes, embeddings, and private records
/// are strictly node-local and never cross `MeshTransport`.
pub const PERMITTED_MESH_MEMORY_TYPES: &[&str] = &[
    "rule",
    "fact",
    "procedure",
    "digital_twin",
    "digital_twin_schema",
];

/// Signed capability token (`nsmesh1<base58btc(json)>`) used for zero-config peer join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteToken {
    pub mesh_id: String,
    pub epoch: u64,
    pub mesh_psk_hex: String,
    pub seed_endpoints: Vec<String>,
    pub issuer_did: String,
    pub signature: String,
}

impl InviteToken {
    /// Issue and Ed25519-sign a new invite token for `mesh_id` at `epoch`.
    pub fn issue(
        mesh_id: &str,
        epoch: u64,
        mesh_psk: &[u8; 32],
        seed_endpoints: Vec<String>,
        privkey: &PKey<Private>,
    ) -> Result<Self, String> {
        let pubkey = privkey
            .raw_public_key()
            .map_err(|e| format!("raw_public_key: {e}"))?;
        let issuer_did = signer_did(&pubkey);
        let mut token = Self {
            mesh_id: mesh_id.to_string(),
            epoch,
            mesh_psk_hex: hex::encode(mesh_psk),
            seed_endpoints,
            issuer_did,
            signature: String::new(),
        };
        let val = serde_json::to_value(&token).map_err(|e| format!("to_value: {e}"))?;
        let signing_bytes = canonical_signing_bytes(&val)?;
        let sig = sign(&signing_bytes, privkey)?;
        token.signature = encode_base64(&sig);
        Ok(token)
    }

    /// Encode this token as a compact `nsmesh1...` Base58btc string.
    pub fn encode(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self).map_err(|e| format!("to_vec: {e}"))?;
        Ok(format!("{INVITE_TOKEN_PREFIX}{}", encode_base58btc(&bytes)))
    }

    /// Decode an `nsmesh1...` string and verify its issuer Ed25519 signature.
    pub fn decode_and_verify(token: &str) -> Result<Self, String> {
        let body = token
            .strip_prefix(INVITE_TOKEN_PREFIX)
            .ok_or_else(|| "missing nsmesh1 prefix".to_string())?;
        let raw = decode_base58btc(body)?;
        let parsed: Self =
            serde_json::from_slice(&raw).map_err(|e| format!("invalid token json: {e}"))?;
        if parsed.mesh_id.trim().is_empty() {
            return Err("empty mesh_id".to_string());
        }
        let psk = hex::decode(&parsed.mesh_psk_hex)
            .map_err(|e| format!("invalid mesh_psk_hex: {e}"))?;
        if psk.len() != 32 {
            return Err(format!("expected 32-byte mesh_psk, got {}", psk.len()));
        }
        let pubkey = public_key_from_did(&parsed.issuer_did)?;
        let sig_bytes = decode_base64(&parsed.signature)?;
        let val = serde_json::to_value(&parsed).map_err(|e| format!("to_value: {e}"))?;
        let signing_bytes = canonical_signing_bytes(&val)?;
        if !verify(&signing_bytes, &pubkey, &sig_bytes) {
            return Err("invite token signature verification failed".to_string());
        }
        Ok(parsed)
    }

    /// Decode the 32-byte pre-shared key carried in this token.
    pub fn psk_bytes(&self) -> Result<[u8; 32], String> {
        let decoded = hex::decode(&self.mesh_psk_hex)
            .map_err(|e| format!("invalid mesh_psk_hex: {e}"))?;
        decoded
            .try_into()
            .map_err(|_| "mesh_psk_hex is not 32 bytes".to_string())
    }
}

/// Distributed Central Executive task claim lease with TTL expiry and causal ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLease {
    pub task_id: String,
    pub holder_node_id: String,
    pub session_id: String,
    pub lamport_ts: u64,
    pub ttl_ms: u64,
    pub expires_at_ms: i64,
}

/// Central Executive task mutation action across the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskMeshAction {
    Created,
    Updated,
    Completed,
}

/// Anti-entropy heartbeat and per-peer contiguous sequence cursor map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncHello {
    pub node_id: String,
    pub mesh_id: String,
    pub epoch: u64,
    pub peer_cursors: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// Tri-Strata + World State payload variants carried inside a [`MeshFrame`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MeshPayloadKind {
    CognitiveDelta {
        record: MeshRecord,
        rdf_patch: String,
        concept_iris: Vec<String>,
        file_targets: Vec<String>,
    },
    TaskDelta {
        task_id: String,
        action: TaskMeshAction,
        task_json: serde_json::Value,
        extracted_memory_ids: Vec<String>,
    },
    TaskLeaseDelta {
        lease: TaskLease,
    },
    TwinDelta {
        twin_id: String,
        version: u64,
        source: String,
        rdf_patch: String,
        changed_variables: BTreeMap<String, f64>,
    },
    SyncHello(SyncHello),
}

/// Signed wire envelope exchanged across [`MeshTransport`](super::transport::MeshTransport).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshFrame {
    pub mesh_id: String,
    pub epoch: u64,
    pub origin_node_id: String,
    pub seq: u64,
    pub lamport_ts: u64,
    pub timestamp_ms: i64,
    pub namespace: String,
    #[serde(default)]
    pub visible_to: Vec<String>,
    pub kind: MeshPayloadKind,
    pub signature: String,
}

impl MeshFrame {
    /// Sign this frame in-place using the origin node's Ed25519 private key.
    pub fn sign_frame(&mut self, privkey: &PKey<Private>) -> Result<(), String> {
        let val = serde_json::to_value(&*self).map_err(|e| format!("to_value: {e}"))?;
        let signing_bytes = canonical_signing_bytes(&val)?;
        let sig = sign(&signing_bytes, privkey)?;
        self.signature = encode_base64(&sig);
        Ok(())
    }

    /// Verify `self.signature` against `self.origin_node_id` (`did:key:z6Mk...`).
    pub fn verify_signature(&self) -> Result<(), SovereigntyGateError> {
        let pubkey = public_key_from_did(&self.origin_node_id)
            .map_err(SovereigntyGateError::InvalidSignature)?;
        let sig_bytes =
            decode_base64(&self.signature).map_err(SovereigntyGateError::InvalidSignature)?;
        let val = serde_json::to_value(self)
            .map_err(|e| SovereigntyGateError::InvalidSignature(e.to_string()))?;
        let signing_bytes =
            canonical_signing_bytes(&val).map_err(SovereigntyGateError::InvalidSignature)?;
        if !verify(&signing_bytes, &pubkey, &sig_bytes) {
            return Err(SovereigntyGateError::InvalidSignature(
                "Ed25519 signature verification failed".to_string(),
            ));
        }
        Ok(())
    }
}

/// Sovereignty, authentication, and schema errors on egress or ingress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SovereigntyGateError {
    NotShareable(String),
    ForbiddenMemoryType(String),
    ForbiddenRuntimeCounter(String),
    NonFiniteTwinValue(String),
    InvalidSignature(String),
    MeshMismatch {
        expected_mesh: String,
        got_mesh: String,
    },
    EpochMismatch {
        expected_epoch: u64,
        got_epoch: u64,
    },
    FrameTooLarge(usize),
    UnauthorizedRecipient(String),
}

impl std::fmt::Display for SovereigntyGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotShareable(m) => write!(f, "row is not shareable: {m}"),
            Self::ForbiddenMemoryType(t) => write!(f, "forbidden mesh memory_type: {t}"),
            Self::ForbiddenRuntimeCounter(k) => {
                write!(f, "forbidden procedure runtime counter on wire: {k}")
            }
            Self::NonFiniteTwinValue(k) => write!(f, "non-finite twin variable value: {k}"),
            Self::InvalidSignature(r) => write!(f, "invalid frame signature: {r}"),
            Self::MeshMismatch {
                expected_mesh,
                got_mesh,
            } => write!(f, "mesh_id mismatch: expected {expected_mesh}, got {got_mesh}"),
            Self::EpochMismatch {
                expected_epoch,
                got_epoch,
            } => write!(f, "epoch mismatch: expected {expected_epoch}, got {got_epoch}"),
            Self::FrameTooLarge(n) => write!(f, "frame size {n} exceeds limit {MAX_MESH_FRAME_BYTES}"),
            Self::UnauthorizedRecipient(d) => write!(f, "local node {d} not in visible_to"),
        }
    }
}

impl std::error::Error for SovereigntyGateError {}

/// Strip node-local procedural runtime counters recursively before a record is shared.
pub fn strip_procedure_runtime_counters(metadata: &mut serde_json::Value) {
    match metadata {
        serde_json::Value::Object(obj) => {
            for key in FORBIDDEN_PROCEDURE_COUNTER_KEYS {
                obj.remove(*key);
            }
            for child in obj.values_mut() {
                strip_procedure_runtime_counters(child);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr {
                strip_procedure_runtime_counters(child);
            }
        }
        _ => {}
    }
}

/// Egress Gate 1: verify `memory_type` is permitted and `metadata.eligibility == "shareable"`.
pub fn validate_egress_candidate(
    memory_type: &str,
    metadata: &serde_json::Value,
) -> Result<(), SovereigntyGateError> {
    let permitted_memory_type = PERMITTED_MESH_MEMORY_TYPES.contains(&memory_type);
    let eligibility = metadata
        .get("eligibility")
        .and_then(|v| v.as_str())
        .unwrap_or("private");
    let is_shareable = eligibility == "shareable";
    match evaluate_sovereignty_gate(permitted_memory_type, is_shareable, false, true, true) {
        KernelSovereigntyDecision::Allow => Ok(()),
        KernelSovereigntyDecision::ForbiddenMemoryType => Err(SovereigntyGateError::ForbiddenMemoryType(
            memory_type.to_string(),
        )),
        KernelSovereigntyDecision::NotShareable => Err(SovereigntyGateError::NotShareable(format!(
            "eligibility is '{eligibility}', required 'shareable'"
        ))),
        _ => Ok(()),
    }
}

/// Recursively inspect a JSON tree for any forbidden node-local procedure runtime counter key.
pub fn find_forbidden_procedure_counter(value: &serde_json::Value) -> Option<&'static str> {
    match value {
        serde_json::Value::Object(map) => {
            for &key in FORBIDDEN_PROCEDURE_COUNTER_KEYS {
                if map.contains_key(key) {
                    return Some(key);
                }
            }
            for child in map.values() {
                if let Some(found) = find_forbidden_procedure_counter(child) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for child in arr {
                if let Some(found) = find_forbidden_procedure_counter(child) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Wire-level envelope verification (size, `mesh_id`, `epoch`, Ed25519 signature,
/// absence of forbidden procedure runtime counters, and finite `TwinDelta` numbers).
pub fn validate_wire_envelope(
    frame: &MeshFrame,
    expected_mesh_id: &str,
    expected_epoch: u64,
) -> Result<(), SovereigntyGateError> {
    let encoded_len = serde_json::to_vec(frame)
        .map(|v| v.len())
        .unwrap_or(MAX_MESH_FRAME_BYTES + 1);
    if encoded_len > MAX_MESH_FRAME_BYTES {
        return Err(SovereigntyGateError::FrameTooLarge(encoded_len));
    }
    if frame.mesh_id != expected_mesh_id {
        return Err(SovereigntyGateError::MeshMismatch {
            expected_mesh: expected_mesh_id.to_string(),
            got_mesh: frame.mesh_id.clone(),
        });
    }
    if frame.epoch != expected_epoch {
        return Err(SovereigntyGateError::EpochMismatch {
            expected_epoch,
            got_epoch: frame.epoch,
        });
    }
    frame.verify_signature()?;

    match &frame.kind {
        MeshPayloadKind::CognitiveDelta { record, .. } => {
            validate_egress_candidate(&record.memory_type, &record.metadata)?;
            if let Some(key) = find_forbidden_procedure_counter(&record.metadata)
                .or_else(|| find_forbidden_procedure_counter(&record.content))
            {
                return Err(SovereigntyGateError::ForbiddenRuntimeCounter(
                    key.to_string(),
                ));
            }
        }
        MeshPayloadKind::TwinDelta {
            changed_variables, ..
        } => {
            for (k, v) in changed_variables {
                if !v.is_finite() {
                    return Err(SovereigntyGateError::NonFiniteTwinValue(k.clone()));
                }
            }
        }
        MeshPayloadKind::TaskDelta { .. } => {
            return Err(SovereigntyGateError::ForbiddenMemoryType(
                "task".to_string(),
            ));
        }
        MeshPayloadKind::TaskLeaseDelta { .. } | MeshPayloadKind::SyncHello(_) => {}
    }

    Ok(())
}

/// Ingress Gate: verify wire envelope plus recipient ReBAC `visible_to`.
pub fn validate_ingress_frame(
    frame: &MeshFrame,
    expected_mesh_id: &str,
    expected_epoch: u64,
    local_did: &str,
) -> Result<(), SovereigntyGateError> {
    validate_wire_envelope(frame, expected_mesh_id, expected_epoch)?;

    if !frame.visible_to.is_empty() && !frame.visible_to.iter().any(|did| did == local_did) {
        return Err(SovereigntyGateError::UnauthorizedRecipient(
            local_did.to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::did::generate_keypair;
    use crate::synaptic_mesh::manifest::Trust;
    use crate::synaptic_mesh::record::{Lifecycle, LifecycleState, MESH_RECORD_SCHEMA_VERSION};
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn invite_token_roundtrip_and_tamper_rejection() {
        let (pkey, did, _) = generate_keypair().unwrap();
        let psk = [0x42u8; 32];
        let tok = InviteToken::issue(
            "mesh-alpha",
            1,
            &psk,
            vec!["127.0.0.1:34345".to_string()],
            &pkey,
        )
        .unwrap();
        let encoded = tok.encode().unwrap();
        assert!(encoded.starts_with("nsmesh1"));
        let decoded = InviteToken::decode_and_verify(&encoded).unwrap();
        assert_eq!(decoded.mesh_id, "mesh-alpha");
        assert_eq!(decoded.issuer_did, did);
        assert_eq!(decoded.epoch, 1);
        assert_eq!(decoded.psk_bytes().unwrap(), psk);

        // Tamper with the last character
        let mut tampered = encoded.clone();
        tampered.pop();
        tampered.push(if encoded.ends_with('a') { 'b' } else { 'a' });
        assert!(InviteToken::decode_and_verify(&tampered).is_err());
    }

    #[test]
    fn sovereignty_gate_blocks_private_structural_and_strips_counters() {
        let private_meta = serde_json::json!({ "eligibility": "private" });
        assert!(matches!(
            validate_egress_candidate("rule", &private_meta),
            Err(SovereigntyGateError::NotShareable(_))
        ));

        let structural_meta = serde_json::json!({ "eligibility": "shareable" });
        assert!(matches!(
            validate_egress_candidate("structural", &structural_meta),
            Err(SovereigntyGateError::ForbiddenMemoryType(_))
        ));
        assert!(matches!(
            validate_egress_candidate("task", &structural_meta),
            Err(SovereigntyGateError::ForbiddenMemoryType(ref t)) if t == "task"
        ));

        let mut proc_meta = serde_json::json!({
            "eligibility": "shareable",
            "trigger": "session-start",
            "remaining_fires": 5,
            "performance_count": 2,
            "last_performed_at": "2026-11-07T00:00:00Z",
            "last_episodic_pointer": "ep-1"
        });
        strip_procedure_runtime_counters(&mut proc_meta);
        for k in FORBIDDEN_PROCEDURE_COUNTER_KEYS {
            assert!(proc_meta.get(*k).is_none(), "counter {k} must be stripped");
        }
        assert_eq!(proc_meta.get("trigger").unwrap(), "session-start");
    }

    #[test]
    fn ingress_frame_verifies_signature_rebac_and_rejects_nan_or_counters() {
        let (pkey, did, _) = generate_keypair().unwrap();
        let mut vars = BTreeMap::new();
        vars.insert("temperature".to_string(), 42.5);
        let mut frame = MeshFrame {
            mesh_id: "mesh-1".to_string(),
            epoch: 2,
            origin_node_id: did.clone(),
            seq: 1,
            lamport_ts: 10,
            timestamp_ms: 1_700_000_000_000,
            namespace: "ops".to_string(),
            visible_to: vec![did.clone()],
            kind: MeshPayloadKind::TwinDelta {
                twin_id: "cooling-1".to_string(),
                version: 1,
                source: "sensor".to_string(),
                rdf_patch: String::new(),
                changed_variables: vars,
            },
            signature: String::new(),
        };
        frame.sign_frame(&pkey).unwrap();
        assert!(validate_ingress_frame(&frame, "mesh-1", 2, &did).is_ok());

        // Unauthorized recipient DID
        assert!(matches!(
            validate_ingress_frame(&frame, "mesh-1", 2, "did:key:z6MkOther"),
            Err(SovereigntyGateError::UnauthorizedRecipient(_))
        ));

        // Epoch mismatch
        assert!(matches!(
            validate_ingress_frame(&frame, "mesh-1", 99, &did),
            Err(SovereigntyGateError::EpochMismatch { .. })
        ));

        // Non-finite float (Infinity) rejected
        let mut nan_vars = BTreeMap::new();
        nan_vars.insert("pressure".to_string(), f64::INFINITY);
        let mut bad_twin = frame.clone();
        bad_twin.kind = MeshPayloadKind::TwinDelta {
            twin_id: "cooling-1".to_string(),
            version: 2,
            source: "sensor".to_string(),
            rdf_patch: String::new(),
            changed_variables: nan_vars,
        };
        bad_twin.sign_frame(&pkey).unwrap();
        assert!(matches!(
            validate_ingress_frame(&bad_twin, "mesh-1", 2, &did),
            Err(SovereigntyGateError::NonFiniteTwinValue(ref k)) if k == "pressure"
        ));

        // Procedure counter on ingress rejected even when validly signed
        let leaky_rec = MeshRecord {
            schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
            id: Uuid::new_v4(),
            namespace: "ops".to_string(),
            memory_type: "procedure".to_string(),
            lifecycle: Lifecycle {
                state: LifecycleState::Active,
                created_at: Utc::now(),
                created_by: did.clone(),
                superseded_by: None,
                archived_at: None,
                tombstoned_at: None,
            },
            content: serde_json::json!({ "trigger": "session-start" }),
            metadata: serde_json::json!({
                "eligibility": "shareable",
                "remaining_fires": 3
            }),
            rdf: None,
            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 2,
            trust: Trust::Trusted,
            signature: None,
            signer_did: Some(did.clone()),
            public_key: None,
            patch: None,
        };
        let mut leaky_frame = frame.clone();
        leaky_frame.kind = MeshPayloadKind::CognitiveDelta {
            record: leaky_rec.clone(),
            rdf_patch: String::new(),
            concept_iris: vec![],
            file_targets: vec![],
        };
        leaky_frame.sign_frame(&pkey).unwrap();
        assert!(matches!(
            validate_ingress_frame(&leaky_frame, "mesh-1", 2, &did),
            Err(SovereigntyGateError::ForbiddenRuntimeCounter(ref k)) if k == "remaining_fires"
        ));

        // Deeply nested procedure runtime counter inside content or metadata is also rejected
        let mut nested_leaky_rec = leaky_rec;
        nested_leaky_rec.metadata = serde_json::json!({
            "eligibility": "shareable",
            "nested": [{ "inner": { "performance_count": 9 } }]
        });
        let mut nested_leaky_frame = frame.clone();
        nested_leaky_frame.kind = MeshPayloadKind::CognitiveDelta {
            record: nested_leaky_rec,
            rdf_patch: String::new(),
            concept_iris: vec![],
            file_targets: vec![],
        };
        nested_leaky_frame.sign_frame(&pkey).unwrap();
        assert!(matches!(
            validate_ingress_frame(&nested_leaky_frame, "mesh-1", 2, &did),
            Err(SovereigntyGateError::ForbiddenRuntimeCounter(ref k)) if k == "performance_count"
        ));

        // TaskDelta is rejected on wire/ingress
        let mut task_frame = frame.clone();
        task_frame.kind = MeshPayloadKind::TaskDelta {
            task_id: "t-1".into(),
            action: TaskMeshAction::Created,
            task_json: serde_json::json!({"id": "t-1"}),
            extracted_memory_ids: vec![],
        };
        task_frame.sign_frame(&pkey).unwrap();
        assert!(matches!(
            validate_ingress_frame(&task_frame, "mesh-1", 2, &did),
            Err(SovereigntyGateError::ForbiddenMemoryType(ref t)) if t == "task"
        ));

        // Forged payload after signing
        frame.lamport_ts = 999;
        assert!(matches!(
            validate_ingress_frame(&frame, "mesh-1", 2, &did),
            Err(SovereigntyGateError::InvalidSignature(_))
        ));
    }
}
