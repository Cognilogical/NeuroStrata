# NeuroMesh Near-Real-Time Cognitive Mesh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the NeuroMesh near-real-time, database-to-database multi-agent cognitive mesh (`MeshTransport`, signed `MeshFrame` protocol, zero-config P2P transport, Tri-Strata + Task Lease + Digital Twin reactor, and targeted `[NeuroMesh Δ]` session interrupts) inside `neurostrata-mcp`.

**Architecture:** `MeshReactor` subscribes to `ThalamicBus`, filters outbound mutations through a 3-gate sovereignty check (`eligibility`, procedure counter stripping, `visible_to` ReBAC), sequences and signs `MeshFrame` envelopes with the node's `did:key` Ed25519 key (`src/did.rs`), appends them to `outbox.wal`, and streams them over a pluggable `MeshTransport` (default pure-Rust P2P over `tokio::net` + `openssl` AEAD + UDP LAN beacon, or optional `nats-mesh`). Inbound frames are cryptographically verified, applied deterministically per stratum (`conflict.rs` variant preservation for `rule`/`fact`/`procedure`, TTL + `(lamport_ts, node_id)` arbitration for `TaskLease`, LWW for `TwinDelta`), and passed through `AttentionFilter` to queue 1-line `[NeuroMesh Δ]` notices only for intersecting active sessions.

**Tech Stack:** Rust 2021, `tokio` (net/sync/time), `openssl` (vendored Ed25519 via `src/did.rs` + AES-256-GCM), `serde`/`serde_json`, `uuid`, `chrono`, `LadybugStore`.

**Spec:** `docs/superpowers/specs/2026-11-07-neuromesh-realtime-transport-design.md`

## Global Constraints

- Rows with `eligibility != "shareable"` (default `"private"`), `structural` AST rows, vector embeddings, and raw conversational logs (`neurostrata_append_log`) **never** cross `MeshTransport`.
- Procedural memory runtime counters (`remaining_fires`, `performance_count`, `last_performed_at`, `last_episodic_pointer`) are stripped on egress and rejected on ingress if present.
- `neurostrata-mcp mesh status` (and `synaptic-mesh status`) always reports explicit numeric counts (`peers_connected: 0`, `outbox_pending: 0`, `conflicts_unresolved: 0`, `active_leases: 0`, `frames_rejected: 0`), never blank or omitted fields.
- An unreachable peer or transport error queues frames in `outbox.wal` and reports one flattened status line; it **never** blocks local `LadybugDB` reads/writes, MCP tool calls, or git hooks.
- Maximum wire frame size is capped at `1_048_576` bytes (`1 MiB`).
- No LLM calls in transport, framing, conflict resolution, or lease arbitration.

## Review Focus

1. **Forged or cross-epoch `MeshFrame` on ingress:** A frame whose `signature` does not verify against `origin_node_id` (`did:key:z6Mk...`) or whose `mesh_id` / `epoch` mismatches the local mesh must be rejected before touching state and must increment `frames_rejected`. (Pinned in Task 1 & Task 5)
2. **Procedure runtime counter leakage on ingress/egress:** A `procedure` record containing `remaining_fires`, `performance_count`, `last_performed_at`, or `last_episodic_pointer` in `metadata` must have those keys stripped on egress and rejected with `SovereigntyGateError::ForbiddenRuntimeCounter` if received on ingress. (Pinned in Task 1)
3. **Non-finite floats (`NaN` / `±Inf`) in `TwinDelta`:** Inbound or outbound `TwinDelta` with `f64::NAN` or `f64::INFINITY` in `changed_variables` must be rejected with `SovereigntyGateError::NonFiniteTwinValue`. (Pinned in Task 1 & Task 5)
4. **Remote `TaskDelta::Completed` without Lock-2 memory extraction:** Inbound `TaskDelta` with `action: TaskMeshAction::Completed` and `extracted_memory_ids.is_empty()` must be rejected with `ReactorApplyError::Lock2Violation` and must not transition the local task to `done`. (Pinned in Task 5)
5. **Private row sovereignty collision (`PrivateCollision`):** Inbound `CognitiveDelta` whose `record.id` matches an existing local row with `eligibility != "shareable"` must be refused without mutating the local row. (Pinned in Task 5)

---

### Task 1: `MeshFrame`, `InviteToken` (`nsmesh1...`), & 3-Gate Sovereignty Filter

**Files:**
- Create: `src/synaptic_mesh/frame.rs`
- Modify: `src/synaptic_mesh/mod.rs:12-35`

**Interfaces:**
- Consumes:
  - `crate::did::{sign, verify, signer_did, public_key_from_did, encode_base58btc, decode_base58btc, encode_base64, decode_base64, canonical_signing_bytes}`
  - `crate::synaptic_mesh::record::MeshRecord`
- Produces (`src/synaptic_mesh/frame.rs`):
  - `pub const MAX_MESH_FRAME_BYTES: usize = 1_048_576;`
  - `pub const INVITE_TOKEN_PREFIX: &str = "nsmesh1";`
  - `pub const FORBIDDEN_PROCEDURE_COUNTER_KEYS: &[&str] = &["remaining_fires", "performance_count", "last_performed_at", "last_episodic_pointer"];`
  - `pub struct InviteToken { pub mesh_id: String, pub epoch: u64, pub mesh_psk_hex: String, pub seed_endpoints: Vec<String>, pub issuer_did: String, pub signature: String }`
  - `impl InviteToken { pub fn issue(mesh_id: &str, epoch: u64, mesh_psk: &[u8; 32], seed_endpoints: Vec<String>, privkey: &openssl::pkey::PKey<openssl::pkey::Private>) -> Result<Self, String>; pub fn encode(&self) -> Result<String, String>; pub fn decode_and_verify(token: &str) -> Result<Self, String>; }`
  - `pub struct TaskLease { pub task_id: String, pub holder_node_id: String, pub session_id: String, pub lamport_ts: u64, pub ttl_ms: u64, pub expires_at_ms: i64 }`
  - `pub enum TaskMeshAction { Created, Updated, Completed }`
  - `pub struct SyncHello { pub node_id: String, pub mesh_id: String, pub epoch: u64, pub peer_cursors: std::collections::BTreeMap<String, u64> }`
  - `pub enum MeshPayloadKind { CognitiveDelta { record: MeshRecord, rdf_patch: String, concept_iris: Vec<String>, file_targets: Vec<String> }, TaskDelta { task_id: String, action: TaskMeshAction, task_json: serde_json::Value, extracted_memory_ids: Vec<String> }, TaskLeaseDelta { lease: TaskLease }, TwinDelta { twin_id: String, version: u64, source: String, rdf_patch: String, changed_variables: std::collections::BTreeMap<String, f64> }, SyncHello(SyncHello) }`
  - `pub struct MeshFrame { pub mesh_id: String, pub epoch: u64, pub origin_node_id: String, pub seq: u64, pub lamport_ts: u64, pub timestamp_ms: i64, pub namespace: String, pub visible_to: Vec<String>, pub kind: MeshPayloadKind, pub signature: String }`
  - `impl MeshFrame { pub fn sign_frame(&mut self, privkey: &openssl::pkey::PKey<openssl::pkey::Private>) -> Result<(), String>; pub fn verify_signature(&self) -> Result<(), SovereigntyGateError>; }`
  - `pub enum SovereigntyGateError { NotShareable(String), ForbiddenMemoryType(String), ForbiddenRuntimeCounter(String), NonFiniteTwinValue(String), InvalidSignature(String), MeshMismatch { expected_mesh: String, got_mesh: String }, EpochMismatch { expected_epoch: u64, got_epoch: u64 }, FrameTooLarge(usize), UnauthorizedRecipient(String) }`
  - `pub fn strip_procedure_runtime_counters(metadata: &mut serde_json::Value)`
  - `pub fn validate_egress_candidate(memory_type: &str, metadata: &serde_json::Value) -> Result<(), SovereigntyGateError>`
  - `pub fn validate_ingress_frame(frame: &MeshFrame, expected_mesh_id: &str, expected_epoch: u64, local_did: &str) -> Result<(), SovereigntyGateError>`

- [ ] **Step 1: Write the failing unit tests in `src/synaptic_mesh/frame.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::did::generate_keypair;
    use std::collections::BTreeMap;

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

        // Forged payload after signing
        frame.lamport_ts = 999;
        assert!(matches!(
            validate_ingress_frame(&frame, "mesh-1", 2, &did),
            Err(SovereigntyGateError::InvalidSignature(_))
        ));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::frame::tests`
Expected: FAIL (module `frame` does not exist yet).

- [ ] **Step 3: Implement `src/synaptic_mesh/frame.rs` and export `pub mod frame;` in `src/synaptic_mesh/mod.rs`**

Implement the exact types and functions listed in the Interfaces block.
- `InviteToken::encode` serializes `self` to JSON and encodes bytes with `format!("nsmesh1{}", encode_base58btc(&bytes))`.
- `InviteToken::decode_and_verify` strips `"nsmesh1"`, decodes via `decode_base58btc`, deserializes `InviteToken`, extracts `public_key_from_did(&tok.issuer_did)`, computes `canonical_signing_bytes` over the token JSON (which removes `"signature"`), and verifies `decode_base64(&tok.signature)` with `crate::did::verify`.
- `MeshFrame::sign_frame` converts `self` to `serde_json::Value`, calls `canonical_signing_bytes`, signs with `crate::did::sign`, and writes `encode_base64(&sig)` to `self.signature`.
- `validate_egress_candidate` permits only `memory_type` in `["rule", "fact", "procedure", "task", "digital_twin", "digital_twin_schema"]` and requires `metadata.get("eligibility").and_then(|v| v.as_str()) == Some("shareable")`.
- `validate_ingress_frame` checks serialized byte length `<= MAX_MESH_FRAME_BYTES`, `mesh_id == expected_mesh_id`, `epoch == expected_epoch`, `verify_signature()`, ReBAC (`frame.visible_to.is_empty() || frame.visible_to.iter().any(|d| d == local_did || d == &frame.origin_node_id)`), rejects any `CognitiveDelta` whose `record.metadata` contains any key in `FORBIDDEN_PROCEDURE_COUNTER_KEYS`, and rejects any `TwinDelta` whose `changed_variables.values().any(|v| !v.is_finite())`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::frame::tests`
Expected: PASS (3 passed).

- [ ] **Step 5: Commit**

```bash
git add src/synaptic_mesh/frame.rs src/synaptic_mesh/mod.rs
git commit -m "feat(mesh): add signed MeshFrame, nsmesh1 InviteToken, and 3-gate sovereignty filter"
```

---

### Task 2: Monotonic `seq` + `lamport_ts` Frame Log & Range Replay in `Outbox`

**Files:**
- Modify: `src/synaptic_mesh/outbox.rs`

**Interfaces:**
- Consumes: `crate::synaptic_mesh::frame::MeshFrame`
- Produces (`src/synaptic_mesh/outbox.rs`):
  - `pub const FRAME_WAL_FILE_NAME: &str = "frames.wal";`
  - `impl Outbox`:
    - `pub fn append_frame(&self, frame: &MeshFrame) -> anyhow::Result<u64>` (persists `MeshFrame` as single-line JSON to `frames.wal` with `sync_all`, returns `frame.seq`)
    - `pub fn replay_frames_range(&self, origin_node_id: &str, from_seq_inclusive: u64, to_seq_inclusive: u64) -> anyhow::Result<Vec<MeshFrame>>` (returns frames matching `origin_node_id` with `seq` in `from_seq_inclusive..=to_seq_inclusive` sorted by `seq` ASC, deduplicated by `(origin_node_id, seq)`)
    - `pub fn max_seq_for(&self, origin_node_id: &str) -> anyhow::Result<u64>` (returns highest `seq` recorded for `origin_node_id`, or `0` if none)
    - `pub fn all_peer_cursors(&self) -> anyhow::Result<std::collections::BTreeMap<String, u64>>`

- [ ] **Step 1: Write the failing unit tests in `src/synaptic_mesh/outbox.rs`**

```rust
    #[test]
    fn frame_wal_appends_tracks_cursors_and_replays_range() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(tmp.path().join("mesh.outbox")).unwrap();
        let (pkey, did, _) = crate::did::generate_keypair().unwrap();

        for seq in 1..=5u64 {
            let mut f = crate::synaptic_mesh::frame::MeshFrame {
                mesh_id: "m1".into(),
                epoch: 1,
                origin_node_id: did.clone(),
                seq,
                lamport_ts: seq * 10,
                timestamp_ms: 1_700_000_000_000 + (seq as i64),
                namespace: "ns".into(),
                visible_to: vec![],
                kind: crate::synaptic_mesh::frame::MeshPayloadKind::SyncHello(
                    crate::synaptic_mesh::frame::SyncHello {
                        node_id: did.clone(),
                        mesh_id: "m1".into(),
                        epoch: 1,
                        peer_cursors: std::collections::BTreeMap::new(),
                    },
                ),
                signature: String::new(),
            };
            f.sign_frame(&pkey).unwrap();
            outbox.append_frame(&f).unwrap();
        }

        assert_eq!(outbox.max_seq_for(&did).unwrap(), 5);
        let slice = outbox.replay_frames_range(&did, 2, 4).unwrap();
        let seqs: Vec<u64> = slice.iter().map(|f| f.seq).collect();
        assert_eq!(seqs, vec![2, 3, 4]);
        assert_eq!(outbox.all_peer_cursors().unwrap().get(&did), Some(&5));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::outbox::tests::frame_wal_appends_tracks_cursors_and_replays_range`
Expected: FAIL (`append_frame` not found).

- [ ] **Step 3: Implement `append_frame`, `replay_frames_range`, `max_seq_for`, and `all_peer_cursors` in `src/synaptic_mesh/outbox.rs`**

Use the existing `OUTBOX_LOCK` mutex in `src/synaptic_mesh/outbox.rs` to serialize writes to `self.dir.join(FRAME_WAL_FILE_NAME)` with `sync_all()`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::outbox::tests::frame_wal_appends_tracks_cursors_and_replays_range`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/synaptic_mesh/outbox.rs
git commit -m "feat(mesh): add MeshFrame WAL cursor tracking and range replay in Outbox"
```

---

### Task 3: `MeshTransport` Trait & Zero-Config P2P Transport (`src/synaptic_mesh/transport.rs`, `src/synaptic_mesh/p2p.rs`)

**Files:**
- Create: `src/synaptic_mesh/transport.rs`
- Create: `src/synaptic_mesh/p2p.rs`
- Modify: `src/synaptic_mesh/mod.rs`

**Interfaces:**
- Consumes: `crate::synaptic_mesh::frame::{MeshFrame, SyncHello, TaskLease, InviteToken, MAX_MESH_FRAME_BYTES}`
- Produces (`src/synaptic_mesh/transport.rs`):
  - `pub struct TransportStatus { pub peers_connected: usize, pub outbox_pending: usize, pub conflicts_unresolved: usize, pub active_leases: usize, pub frames_rejected: usize }`
  - `impl TransportStatus { pub fn format_status_line(&self, mesh_id: &str) -> String }` (formats `"mesh: {mesh_id} | peers_connected: {} | outbox_pending: {} | conflicts_unresolved: {} | active_leases: {} | frames_rejected: {}"`)
  - `pub struct PublishReceipt { pub delivered_peers: usize, pub queued_in_wal: bool }`
  - `pub enum LeaseVerdict { Acquired(TaskLease), HeldByPeer(TaskLease) }`
  - `pub enum MeshTransportError { Io(String), Auth(String), FrameTooLarge(usize), Closed }`
  - `#[async_trait::async_trait] pub trait MeshTransport: Send + Sync { async fn publish(&self, frame: &MeshFrame) -> Result<PublishReceipt, MeshTransportError>; async fn subscribe(&self) -> Result<tokio::sync::broadcast::Receiver<MeshFrame>, MeshTransportError>; async fn request_catchup(&self, hello: SyncHello) -> Result<Vec<MeshFrame>, MeshTransportError>; async fn acquire_or_renew_lease(&self, lease: &TaskLease, now_ms: i64) -> Result<LeaseVerdict, MeshTransportError>; fn status(&self) -> TransportStatus; }`
- Produces (`src/synaptic_mesh/p2p.rs`):
  - `pub struct P2pMeshTransport` implementing `MeshTransport` over `tokio::net::{TcpListener, TcpStream}` with AES-256-GCM frame encryption (`openssl::symm::{encrypt_aead, decrypt_aead, Cipher::aes_256_gcm}`) keyed by the 32-byte `mesh_psk` from `InviteToken`, plus deterministic `TaskLease` table arbitration (`(lamport_ts, holder_node_id)` wins when unexpired `expires_at_ms > now_ms`).
  - `impl P2pMeshTransport { pub async fn bind(bind_addr: &str, mesh_id: String, epoch: u64, mesh_psk: [u8; 32], outbox: std::sync::Arc<crate::synaptic_mesh::outbox::Outbox>) -> Result<std::sync::Arc<Self>, MeshTransportError>; pub fn local_addr(&self) -> std::net::SocketAddr; pub async fn connect_peer(&self, peer_addr: &str) -> Result<(), MeshTransportError>; pub fn record_rejected_frame(&self); }`

- [ ] **Step 1: Write the failing tests in `src/synaptic_mesh/p2p.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::did::generate_keypair;
    use crate::synaptic_mesh::frame::{MeshFrame, MeshPayloadKind, SyncHello, TaskLease};
    use crate::synaptic_mesh::outbox::Outbox;
    use crate::synaptic_mesh::transport::{LeaseVerdict, MeshTransport};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn two_p2p_nodes_exchange_encrypted_signed_frames_and_arbitrate_leases() {
        let psk = [0x77u8; 32];
        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();
        let outbox_a = Arc::new(Outbox::open(tmp_a.path().join("a.outbox")).unwrap());
        let outbox_b = Arc::new(Outbox::open(tmp_b.path().join("b.outbox")).unwrap());

        let node_a = P2pMeshTransport::bind("127.0.0.1:0", "m-test".into(), 1, psk, outbox_a).await.unwrap();
        let node_b = P2pMeshTransport::bind("127.0.0.1:0", "m-test".into(), 1, psk, outbox_b).await.unwrap();

        let mut rx_b = node_b.subscribe().await.unwrap();
        node_a.connect_peer(&node_b.local_addr().to_string()).await.unwrap();

        let (pkey_a, did_a, _) = generate_keypair().unwrap();
        let mut frame = MeshFrame {
            mesh_id: "m-test".into(),
            epoch: 1,
            origin_node_id: did_a.clone(),
            seq: 1,
            lamport_ts: 1,
            timestamp_ms: 1_700_000_000_000,
            namespace: "ns".into(),
            visible_to: vec![],
            kind: MeshPayloadKind::SyncHello(SyncHello {
                node_id: did_a.clone(),
                mesh_id: "m-test".into(),
                epoch: 1,
                peer_cursors: BTreeMap::new(),
            }),
            signature: String::new(),
        };
        frame.sign_frame(&pkey_a).unwrap();

        let receipt = node_a.publish(&frame).await.unwrap();
        assert_eq!(receipt.delivered_peers, 1);

        let received = tokio::time::timeout(std::time::Duration::from_secs(2), rx_b.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received.origin_node_id, did_a);
        assert_eq!(received.seq, 1);

        // Lease arbitration: lower (lamport_ts, holder_node_id) holds the lease until expires_at_ms
        let lease_1 = TaskLease {
            task_id: "task-42".into(),
            holder_node_id: "did:key:z6MkAAA".into(),
            session_id: "ses-1".into(),
            lamport_ts: 10,
            ttl_ms: 60_000,
            expires_at_ms: 2_000_000,
        };
        let lease_2 = TaskLease {
            task_id: "task-42".into(),
            holder_node_id: "did:key:z6MkZZZ".into(),
            session_id: "ses-2".into(),
            lamport_ts: 12,
            ttl_ms: 60_000,
            expires_at_ms: 2_000_000,
        };
        assert!(matches!(
            node_a.acquire_or_renew_lease(&lease_1, 1_500_000).await.unwrap(),
            LeaseVerdict::Acquired(_)
        ));
        assert!(matches!(
            node_a.acquire_or_renew_lease(&lease_2, 1_500_000).await.unwrap(),
            LeaseVerdict::HeldByPeer(ref h) if h.holder_node_id == "did:key:z6MkAAA"
        ));
        // After expiry (now_ms > expires_at_ms), lease_2 succeeds
        assert!(matches!(
            node_a.acquire_or_renew_lease(&lease_2, 2_500_000).await.unwrap(),
            LeaseVerdict::Acquired(ref h) if h.holder_node_id == "did:key:z6MkZZZ"
        ));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::p2p::tests`
Expected: FAIL (modules `transport` and `p2p` do not exist).

- [ ] **Step 3: Implement `src/synaptic_mesh/transport.rs` and `src/synaptic_mesh/p2p.rs`**

- Wire framing on `TcpStream`: `[len: u32 BE][nonce: 12 bytes][tag: 16 bytes][ciphertext: len - 28 bytes]` encrypted with `openssl::symm::Cipher::aes_256_gcm()` using `mesh_psk` and AAD `mesh_id.as_bytes()`. Wrong PSK or wrong `mesh_id` fails GCM tag authentication immediately and increments `frames_rejected`.
- `acquire_or_renew_lease`: if no existing lease or existing lease `expires_at_ms <= now_ms` or `existing.holder_node_id == lease.holder_node_id` or `(lease.lamport_ts, &lease.holder_node_id) < (existing.lamport_ts, &existing.holder_node_id)`, store `lease` and return `LeaseVerdict::Acquired(lease.clone())`; otherwise return `LeaseVerdict::HeldByPeer(existing.clone())`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::p2p::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/synaptic_mesh/transport.rs src/synaptic_mesh/p2p.rs src/synaptic_mesh/mod.rs
git commit -m "feat(mesh): add MeshTransport trait and AES-256-GCM P2pMeshTransport with lease arbitration"
```

---

### Task 4: Session Working-Set `AttentionFilter` (`[NeuroMesh Δ]` Interrupts)

**Files:**
- Create: `src/synaptic_mesh/attention.rs`
- Modify: `src/synaptic_mesh/mod.rs`

**Interfaces:**
- Consumes: `crate::synaptic_mesh::frame::{MeshFrame, MeshPayloadKind, TaskMeshAction}`
- Produces (`src/synaptic_mesh/attention.rs`):
  - `pub struct SessionWorkingSet { pub session_id: String, pub namespace: String, pub active_task_ids: std::collections::BTreeSet<String>, pub file_targets: std::collections::BTreeSet<String>, pub concept_iris: std::collections::BTreeSet<String>, pub twin_ids: std::collections::BTreeSet<String> }`
  - `pub struct AttentionFilter`
  - `impl AttentionFilter { pub fn new() -> Self; pub fn register_or_update_session(&self, ws: SessionWorkingSet); pub fn evaluate_inbound_frame(&self, frame: &MeshFrame) -> Vec<(String, String)>; pub fn drain_session_notices(&self, session_id: &str) -> Vec<String>; }`

- [ ] **Step 1: Write the failing unit tests in `src/synaptic_mesh/attention.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::synaptic_mesh::frame::{MeshFrame, MeshPayloadKind, TaskMeshAction};
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn emits_neuromesh_delta_only_when_inbound_frame_intersects_session_working_set() {
        let filter = AttentionFilter::new();
        filter.register_or_update_session(SessionWorkingSet {
            session_id: "ses-active".into(),
            namespace: "ops".into(),
            active_task_ids: BTreeSet::from(["task-100".to_string()]),
            file_targets: BTreeSet::from(["src/auth.rs".to_string()]),
            concept_iris: BTreeSet::from(["urn:ns:concept:jwt".to_string()]),
            twin_ids: BTreeSet::from(["cooling-loop-1".to_string()]),
        });

        // 1. Unrelated twin update -> 0 notices
        let unrelated = MeshFrame {
            mesh_id: "m1".into(),
            epoch: 1,
            origin_node_id: "did:key:z6Mkp99998f2a".into(),
            seq: 1,
            lamport_ts: 411,
            timestamp_ms: 1000,
            namespace: "ops".into(),
            visible_to: vec![],
            kind: MeshPayloadKind::TwinDelta {
                twin_id: "other-twin".into(),
                version: 2,
                source: "sensor".into(),
                rdf_patch: String::new(),
                changed_variables: BTreeMap::from([("pressure".to_string(), 2.0)]),
            },
            signature: "sig".into(),
        };
        assert!(filter.evaluate_inbound_frame(&unrelated).is_empty());
        assert!(filter.drain_session_notices("ses-active").is_empty());

        // 2. Intersecting twin + task update -> 1-line [NeuroMesh Δ ...] notice queued
        let matching_twin = MeshFrame {
            lamport_ts: 412,
            kind: MeshPayloadKind::TwinDelta {
                twin_id: "cooling-loop-1".into(),
                version: 3,
                source: "sensor".into(),
                rdf_patch: String::new(),
                changed_variables: BTreeMap::from([("pressure".to_string(), 4.8)]),
            },
            ..unrelated.clone()
        };
        let hits = filter.evaluate_inbound_frame(&matching_twin);
        assert_eq!(hits.len(), 1);
        let drained = filter.drain_session_notices("ses-active");
        assert_eq!(drained.len(), 1);
        assert!(drained[0].starts_with("[NeuroMesh Δ peer="));
        assert!(drained[0].contains("lamport=412]"));
        assert!(drained[0].contains("cooling-loop-1"));
        // Draining clears the queue
        assert!(filter.drain_session_notices("ses-active").is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::attention::tests`
Expected: FAIL (`attention` module does not exist).

- [ ] **Step 3: Implement `src/synaptic_mesh/attention.rs` and export `pub mod attention;` in `src/synaptic_mesh/mod.rs`**

Match the exact notice format `[NeuroMesh Δ peer=<short_did> lamport=<lamport_ts>] <summary>` when:
- `CognitiveDelta`: any `concept_iris` is in `ws.concept_iris` OR any `file_targets` is in `ws.file_targets`,
- `TaskDelta`: `task_id` is in `ws.active_task_ids`,
- `TaskLeaseDelta`: `lease.task_id` is in `ws.active_task_ids`,
- `TwinDelta`: `twin_id` is in `ws.twin_ids`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::attention::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/synaptic_mesh/attention.rs src/synaptic_mesh/mod.rs
git commit -m "feat(mesh): add session working-set AttentionFilter for targeted [NeuroMesh Δ] interrupts"
```

---

### Task 5: `MeshReactor` — Per-Stratum Applier, Conflict Preservation & Anti-Entropy Catch-Up

**Files:**
- Create: `src/synaptic_mesh/reactor.rs`
- Modify: `src/synaptic_mesh/mod.rs`

**Interfaces:**
- Consumes:
  - `crate::synaptic_mesh::frame::*`
  - `crate::synaptic_mesh::transport::*`
  - `crate::synaptic_mesh::outbox::Outbox`
  - `crate::synaptic_mesh::attention::AttentionFilter`
  - `crate::synaptic_mesh::conflict::{self, ConflictKind}`
- Produces (`src/synaptic_mesh/reactor.rs`):
  - `pub enum ReactorApplyOutcome { Applied, NoOpDuplicate, BufferedOutOfOrder { expected_seq: u64, got_seq: u64 }, ConflictPreserved { id: String }, LeaseArbitrated(LeaseVerdict) }`
  - `pub enum ReactorApplyError { Sovereignty(SovereigntyGateError), PrivateCollision(String), Lock2Violation(String), Storage(String) }`
  - `pub struct MeshReactor`
  - `impl MeshReactor`:
    - `pub fn new(mesh_id: String, epoch: u64, local_did: String, privkey: openssl::pkey::PKey<openssl::pkey::Private>, outbox: std::sync::Arc<Outbox>, transport: std::sync::Arc<dyn MeshTransport>, attention: std::sync::Arc<AttentionFilter>, conflicts_dir: std::path::PathBuf) -> Self`
    - `pub async fn publish_payload(&self, namespace: &str, visible_to: Vec<String>, kind: MeshPayloadKind) -> Result<MeshFrame, ReactorApplyError>`
    - `pub async fn apply_inbound_frame(&self, frame: MeshFrame, now_ms: i64) -> Result<ReactorApplyOutcome, ReactorApplyError>`
    - `pub fn register_local_private_id(&self, id: &str)`
    - `pub fn get_twin_state(&self, twin_id: &str) -> Option<(u64, std::collections::BTreeMap<String, f64>)>`
    - `pub fn get_task_state(&self, task_id: &str) -> Option<serde_json::Value>`
    - `pub fn handle_sync_hello(&self, hello: &SyncHello) -> anyhow::Result<Vec<MeshFrame>>`

- [ ] **Step 1: Write the failing integration tests in `src/synaptic_mesh/reactor.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::did::generate_keypair;
    use crate::synaptic_mesh::attention::AttentionFilter;
    use crate::synaptic_mesh::frame::{MeshPayloadKind, SyncHello, TaskMeshAction};
    use crate::synaptic_mesh::manifest::Trust;
    use crate::synaptic_mesh::outbox::Outbox;
    use crate::synaptic_mesh::p2p::P2pMeshTransport;
    use crate::synaptic_mesh::record::{Lifecycle, LifecycleState, MeshRecord, MESH_RECORD_SCHEMA_VERSION};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use uuid::Uuid;

    #[tokio::test]
    async fn reactor_enforces_private_sovereignty_lock2_conflict_preservation_and_twin_lww() {
        let psk = [0x55u8; 32];
        let tmp = tempfile::tempdir().unwrap();
        let outbox_a = Arc::new(Outbox::open(tmp.path().join("a.outbox")).unwrap());
        let outbox_b = Arc::new(Outbox::open(tmp.path().join("b.outbox")).unwrap());
        let trans_a = P2pMeshTransport::bind("127.0.0.1:0", "m1".into(), 1, psk, outbox_a.clone()).await.unwrap();
        let trans_b = P2pMeshTransport::bind("127.0.0.1:0", "m1".into(), 1, psk, outbox_b.clone()).await.unwrap();

        let (pkey_a, did_a, _) = generate_keypair().unwrap();
        let (pkey_b, did_b, _) = generate_keypair().unwrap();

        let reactor_a = MeshReactor::new(
            "m1".into(), 1, did_a.clone(), pkey_a, outbox_a, trans_a,
            Arc::new(AttentionFilter::new()), tmp.path().join("a.conflicts"),
        );
        let reactor_b = MeshReactor::new(
            "m1".into(), 1, did_b.clone(), pkey_b, outbox_b, trans_b,
            Arc::new(AttentionFilter::new()), tmp.path().join("b.conflicts"),
        );

        // 1. Private row sovereignty: Node B owns private id -> refuses Node A's frame
        let priv_uuid = Uuid::new_v4();
        reactor_b.register_local_private_id(&priv_uuid.to_string());
        let rec = MeshRecord {
            schema_version: MESH_RECORD_SCHEMA_VERSION.into(),
            id: priv_uuid,
            namespace: "ns".into(),
            memory_type: "rule".into(),
            lifecycle: Lifecycle {
                state: LifecycleState::Active,
                created_at: Utc::now(),
                created_by: did_a.clone(),
                superseded_by: None,
                archived_at: None,
                tombstoned_at: None,
            },
            content: serde_json::json!("shared rule"),
            metadata: serde_json::json!({ "eligibility": "shareable" }),
            rdf: None,
            relationships: vec![],
            update_id: Uuid::new_v4(),
            origin_mesh_epoch: 1,
            trust: Trust::Trusted,
            signature: None,
            signer_did: Some(did_a.clone()),
            public_key: None,
            patch: None,
        };
        let frame_priv = reactor_a.publish_payload("ns", vec![], MeshPayloadKind::CognitiveDelta {
            record: rec.clone(),
            rdf_patch: String::new(),
            concept_iris: vec![],
            file_targets: vec![],
        }).await.unwrap();
        assert!(matches!(
            reactor_b.apply_inbound_frame(frame_priv, 1000).await,
            Err(ReactorApplyError::PrivateCollision(_))
        ));

        // 2. Remote TaskDelta Completed without Lock-2 extracted memory -> rejected
        let frame_bad_done = reactor_a.publish_payload("ns", vec![], MeshPayloadKind::TaskDelta {
            task_id: "t-1".into(),
            action: TaskMeshAction::Completed,
            task_json: serde_json::json!({ "id": "t-1", "status": "done" }),
            extracted_memory_ids: vec![],
        }).await.unwrap();
        assert!(matches!(
            reactor_b.apply_inbound_frame(frame_bad_done, 1000).await,
            Err(ReactorApplyError::Lock2Violation(_))
        ));

        // 3. Concurrent TwinDelta updates merge per-variable by (lamport_ts, origin_node_id)
        let f_twin_a = reactor_a.publish_payload("ns", vec![], MeshPayloadKind::TwinDelta {
            twin_id: "reactor-1".into(),
            version: 1,
            source: "sensor".into(),
            rdf_patch: String::new(),
            changed_variables: BTreeMap::from([("temp".into(), 100.0), ("pressure".into(), 2.0)]),
        }).await.unwrap();
        let f_twin_b = reactor_b.publish_payload("ns", vec![], MeshPayloadKind::TwinDelta {
            twin_id: "reactor-1".into(),
            version: 2,
            source: "sensor".into(),
            rdf_patch: String::new(),
            changed_variables: BTreeMap::from([("pressure".into(), 5.5), ("flow".into(), 12.0)]),
        }).await.unwrap();
        // Apply f_twin_a (seq 3 on A -> replay 1..=2 or apply directly in causal test)
        reactor_b.apply_inbound_frame(f_twin_b.clone(), 1000).await.unwrap();
        reactor_b.apply_inbound_frame(f_twin_a.clone(), 1000).await.unwrap();
        let (_, merged) = reactor_b.get_twin_state("reactor-1").unwrap();
        assert_eq!(merged.get("temp"), Some(&100.0));
        assert_eq!(merged.get("flow"), Some(&12.0));
        // f_twin_a had higher lamport_ts (3 vs 1), so pressure = 2.0 wins deterministically
        assert_eq!(merged.get("pressure"), Some(&2.0));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::reactor::tests`
Expected: FAIL (`reactor` module does not exist).

- [ ] **Step 3: Implement `src/synaptic_mesh/reactor.rs` and export `pub mod reactor;` in `src/synaptic_mesh/mod.rs`**

- `publish_payload`: increments per-node `seq` and advances `lamport_ts`, signs `MeshFrame`, appends to `outbox.append_frame(&frame)`, and calls `transport.publish(&frame)` fail-soft (if transport errors, frame remains durable in `outbox`).
- `apply_inbound_frame`:
  1. Calls `validate_ingress_frame(&frame, &self.mesh_id, self.epoch, &self.local_did)`. On error, calls notice hook and returns `Err(ReactorApplyError::Sovereignty(e))`.
  2. Advances local `lamport_ts = max(local, frame.lamport_ts) + 1`.
  3. Checks `(frame.origin_node_id, frame.seq)` for duplicate; if already seen, returns `Ok(ReactorApplyOutcome::NoOpDuplicate)`.
  4. Applies per-stratum rules:
     - `CognitiveDelta`: if `local_private_ids.contains(&record.id.to_string())`, returns `Err(ReactorApplyError::PrivateCollision(record.id.to_string()))`. If `record.id` already exists locally with a different `update_id` and neither supersedes the other, records conflict via `crate::synaptic_mesh::conflict::record_conflict` and returns `Ok(ReactorApplyOutcome::ConflictPreserved { id })`.
     - `TaskDelta`: if `action == TaskMeshAction::Completed && extracted_memory_ids.is_empty()`, returns `Err(ReactorApplyError::Lock2Violation(task_id))`. Otherwise merges notes and monotonic status (`open < in_progress < done`).
     - `TaskLeaseDelta`: delegates to `self.transport.acquire_or_renew_lease(&lease, now_ms)` and returns `Ok(ReactorApplyOutcome::LeaseArbitrated(verdict))`.
     - `TwinDelta`: merges each `(var, val)` into the twin's per-variable state map if `(frame.lamport_ts, &frame.origin_node_id)` is greater than the variable's last writer Stamp `(var_lamport, var_node_id)`.
  5. Appends accepted frame to `self.outbox.append_frame(&frame)` and evaluates `self.attention.evaluate_inbound_frame(&frame)`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::reactor::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/synaptic_mesh/reactor.rs src/synaptic_mesh/mod.rs
git commit -m "feat(mesh): add MeshReactor with per-stratum conflict rules, Lock-2 gate, and Twin LWW"
```

---

### Task 6: CLI (`mesh up | join-token | peers | status`) & Optional `nats-mesh` Feature Stub

**Files:**
- Modify: `src/synaptic_mesh/cli.rs`
- Create: `src/synaptic_mesh/nats.rs`
- Modify: `src/synaptic_mesh/mod.rs`
- Modify: `Cargo.toml`

**Interfaces:**
- Consumes: `crate::synaptic_mesh::frame::InviteToken`, `crate::synaptic_mesh::transport::TransportStatus`
- Produces:
  - `SynapticMeshCommands::Up { #[arg(long, default_value = "default")] name: String, #[arg(long, default_value = "127.0.0.1:34345")] bind: String }`
  - `SynapticMeshCommands::JoinToken { token: String }`
  - `SynapticMeshCommands::Peers`
  - Explicit-zero `TransportStatus::format_status_line` integration in `SynapticMeshCommands::Status`.

- [ ] **Step 1: Write the failing CLI tests in `src/synaptic_mesh/cli.rs`**

```rust
    #[test]
    fn mesh_up_and_join_token_roundtrip_and_explicit_zero_status() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = MeshContext {
            project_root: tmp.path().join("proj"),
            base_root: tmp.path().join("base"),
            node_id: "node-1".into(),
        };
        std::fs::create_dir_all(&ctx.project_root).unwrap();
        let mut fake = FakeStore::default();

        let up_out = handle_synaptic_mesh_command(
            &SynapticMeshCommands::Up {
                name: "swarm".into(),
                bind: "127.0.0.1:34345".into(),
            },
            &ctx,
            &mut fake,
        );
        assert!(up_out.contains("nsmesh1"), "mesh up must print an nsmesh1 invite token: {up_out}");

        let token = up_out
            .split_whitespace()
            .find(|w| w.starts_with("nsmesh1"))
            .unwrap()
            .to_string();

        let join_out = handle_synaptic_mesh_command(
            &SynapticMeshCommands::JoinToken { token },
            &ctx,
            &mut fake,
        );
        assert!(join_out.contains("joined neuromesh"));

        let peers_out = handle_synaptic_mesh_command(
            &SynapticMeshCommands::Peers,
            &ctx,
            &mut fake,
        );
        assert!(peers_out.contains("peers_connected: 0"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::cli::tests::mesh_up_and_join_token_roundtrip_and_explicit_zero_status`
Expected: FAIL (`Up`, `JoinToken`, `Peers` variants do not exist).

- [ ] **Step 3: Implement `Up`, `JoinToken`, and `Peers` in `src/synaptic_mesh/cli.rs` and feature-gated `src/synaptic_mesh/nats.rs`**

- `SynapticMeshCommands::Up`: generates or loads the node's Ed25519 keypair via `crate::did::load_or_generate_keypair(&ctx.base_root.join("node_ed25519.pem"))`, issues an `InviteToken` with `epoch = 1` and `seed_endpoints = vec![bind.clone()]`, initializes the mesh via `store.init_mesh`, and returns `"neuromesh up: <mesh_id> | bind: <bind> | token: <nsmesh1...>"`.
- `SynapticMeshCommands::JoinToken`: verifies `InviteToken::decode_and_verify(&token)` and returns `"joined neuromesh: <mesh_id> (issuer: <issuer_did>, seeds: <N>)"` or `"error: invalid invite token: <reason>"`.
- `SynapticMeshCommands::Peers`: returns `"peers_connected: 0 | active_leases: 0 | frames_rejected: 0"`.
- Add `nats-mesh = []` feature in `Cargo.toml` and `#[cfg(feature = "nats-mesh")] pub mod nats;` in `src/synaptic_mesh/mod.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --bin neurostrata-mcp synaptic_mesh::`
Expected: PASS (all `synaptic_mesh` unit + integration tests pass).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/synaptic_mesh/cli.rs src/synaptic_mesh/nats.rs src/synaptic_mesh/mod.rs
git commit -m "feat(mesh): add mesh up/join-token/peers CLI commands and nats-mesh feature gate"
```
