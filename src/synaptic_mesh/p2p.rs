//! Default zero-config pure-Rust P2P transport for NeuroMesh.
//!
//! Uses `tokio::net::{TcpListener, TcpStream}` with AES-256-GCM authenticated
//! framing (`openssl::symm`) keyed by the 32-byte `mesh_psk` from `nsmesh1...`
//! invite tokens, plus deterministic `(lamport_ts, holder_node_id)` task lease
//! arbitration with TTL expiry.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use openssl::symm::{decrypt_aead, encrypt_aead, Cipher};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

use crate::synaptic_mesh::frame::{
    validate_wire_envelope, MeshFrame, SyncHello, TaskLease, MAX_MESH_FRAME_BYTES,
};
use crate::synaptic_mesh::outbox::Outbox;
use crate::synaptic_mesh::transport::{
    LeaseVerdict, MeshTransport, MeshTransportError, PublishReceipt, TransportStatus,
};

const GCM_NONCE_LEN: usize = 12;
const GCM_TAG_LEN: usize = 16;

/// Pure-Rust P2P mesh transport with AES-256-GCM wire encryption and lease table.
pub struct P2pMeshTransport {
    local_addr: SocketAddr,
    mesh_id: String,
    epoch: u64,
    mesh_psk: [u8; 32],
    outbox: Arc<Outbox>,
    peer_addrs: Mutex<Vec<String>>,
    inbound_tx: broadcast::Sender<MeshFrame>,
    leases: Mutex<BTreeMap<String, TaskLease>>,
    frames_rejected: AtomicUsize,
    nonce_counter: AtomicU64,
}

impl P2pMeshTransport {
    /// Bind a local TCP listener on `bind_addr` and spawn the inbound accept loop.
    pub async fn bind(
        bind_addr: &str,
        mesh_id: String,
        epoch: u64,
        mesh_psk: [u8; 32],
        outbox: Arc<Outbox>,
    ) -> Result<Arc<Self>, MeshTransportError> {
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        let (inbound_tx, _) = broadcast::channel(512);
        let transport = Arc::new(Self {
            local_addr,
            mesh_id,
            epoch,
            mesh_psk,
            outbox,
            peer_addrs: Mutex::new(Vec::new()),
            inbound_tx,
            leases: Mutex::new(BTreeMap::new()),
            frames_rejected: AtomicUsize::new(0),
            nonce_counter: AtomicU64::new(1),
        });

        let weak = Arc::downgrade(&transport);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let Some(this) = weak.upgrade() else {
                    break;
                };
                tokio::spawn(async move {
                    let _ = this.handle_inbound_stream(&mut stream).await;
                });
            }
        });

        Ok(transport)
    }

    /// Bound local socket address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Register and verify connectivity to a peer endpoint (`host:port`).
    pub async fn connect_peer(&self, peer_addr: &str) -> Result<(), MeshTransportError> {
        let _probe = TcpStream::connect(peer_addr)
            .await
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        let mut peers = self
            .peer_addrs
            .lock()
            .map_err(|_| MeshTransportError::Io("peer_addrs lock poisoned".into()))?;
        if !peers.iter().any(|p| p == peer_addr) {
            peers.push(peer_addr.to_string());
        }
        Ok(())
    }

    /// Increment the rejected-frame counter (surfaced in `mesh status`).
    pub fn record_rejected_frame(&self) {
        self.frames_rejected.fetch_add(1, Ordering::Relaxed);
    }

    fn encrypt_wire_packet(&self, frame: &MeshFrame) -> Result<Vec<u8>, MeshTransportError> {
        let plain =
            serde_json::to_vec(frame).map_err(|e| MeshTransportError::Io(e.to_string()))?;
        if plain.len() > MAX_MESH_FRAME_BYTES {
            return Err(MeshTransportError::FrameTooLarge(plain.len()));
        }
        let mut nonce = [0u8; GCM_NONCE_LEN];
        let seq_n = self.nonce_counter.fetch_add(1, Ordering::Relaxed);
        nonce[..4].copy_from_slice(&(self.local_addr.port() as u32).to_be_bytes());
        nonce[4..12].copy_from_slice(&seq_n.to_be_bytes());

        let mut tag = [0u8; GCM_TAG_LEN];
        let cipher = Cipher::aes_256_gcm();
        let ciphertext = encrypt_aead(
            cipher,
            &self.mesh_psk,
            Some(&nonce),
            self.mesh_id.as_bytes(),
            &plain,
            &mut tag,
        )
        .map_err(|e| MeshTransportError::Auth(e.to_string()))?;

        let payload_len = (GCM_NONCE_LEN + GCM_TAG_LEN + ciphertext.len()) as u32;
        let mut packet = Vec::with_capacity(4 + payload_len as usize);
        packet.extend_from_slice(&payload_len.to_be_bytes());
        packet.extend_from_slice(&nonce);
        packet.extend_from_slice(&tag);
        packet.extend_from_slice(&ciphertext);
        Ok(packet)
    }

    async fn handle_inbound_stream(&self, stream: &mut TcpStream) -> Result<(), MeshTransportError> {
        loop {
            let mut len_buf = [0u8; 4];
            if stream.read_exact(&mut len_buf).await.is_err() {
                return Err(MeshTransportError::Closed);
            }
            let payload_len = u32::from_be_bytes(len_buf) as usize;
            if payload_len < GCM_NONCE_LEN + GCM_TAG_LEN
                || payload_len > MAX_MESH_FRAME_BYTES + GCM_NONCE_LEN + GCM_TAG_LEN
            {
                self.record_rejected_frame();
                return Err(MeshTransportError::FrameTooLarge(payload_len));
            }
            let mut body = vec![0u8; payload_len];
            stream
                .read_exact(&mut body)
                .await
                .map_err(|e| MeshTransportError::Io(e.to_string()))?;

            let nonce = &body[..GCM_NONCE_LEN];
            let tag = &body[GCM_NONCE_LEN..GCM_NONCE_LEN + GCM_TAG_LEN];
            let ciphertext = &body[GCM_NONCE_LEN + GCM_TAG_LEN..];

            let cipher = Cipher::aes_256_gcm();
            let plain = match decrypt_aead(
                cipher,
                &self.mesh_psk,
                Some(nonce),
                self.mesh_id.as_bytes(),
                ciphertext,
                tag,
            ) {
                Ok(p) => p,
                Err(e) => {
                    self.record_rejected_frame();
                    return Err(MeshTransportError::Auth(format!("AEAD decrypt failed: {e}")));
                }
            };

            let frame: MeshFrame = match serde_json::from_slice(&plain) {
                Ok(f) => f,
                Err(e) => {
                    self.record_rejected_frame();
                    return Err(MeshTransportError::Auth(format!(
                        "invalid MeshFrame json: {e}"
                    )));
                }
            };

            if let Err(e) = validate_wire_envelope(&frame, &self.mesh_id, self.epoch) {
                self.record_rejected_frame();
                return Err(MeshTransportError::Auth(e.to_string()));
            }

            let _ = self.inbound_tx.send(frame);
        }
    }
}

#[async_trait::async_trait]
impl MeshTransport for P2pMeshTransport {
    async fn publish(&self, frame: &MeshFrame) -> Result<PublishReceipt, MeshTransportError> {
        let packet = self.encrypt_wire_packet(frame)?;
        let peers = self
            .peer_addrs
            .lock()
            .map_err(|_| MeshTransportError::Io("peer_addrs lock poisoned".into()))?
            .clone();

        let mut delivered_peers = 0usize;
        for addr in peers {
            if let Ok(mut stream) = TcpStream::connect(&addr).await {
                if stream.write_all(&packet).await.is_ok() && stream.flush().await.is_ok() {
                    delivered_peers += 1;
                }
            }
        }
        Ok(PublishReceipt {
            delivered_peers,
            queued_in_wal: true,
        })
    }

    async fn subscribe(
        &self,
    ) -> Result<broadcast::Receiver<MeshFrame>, MeshTransportError> {
        Ok(self.inbound_tx.subscribe())
    }

    async fn request_catchup(
        &self,
        hello: SyncHello,
    ) -> Result<Vec<MeshFrame>, MeshTransportError> {
        let local_cursors = self
            .outbox
            .all_peer_cursors()
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        let mut missing = Vec::new();
        for (origin, &max_seq) in &local_cursors {
            let peer_has = hello.peer_cursors.get(origin).copied().unwrap_or(0);
            if max_seq > peer_has {
                let mut frames = self
                    .outbox
                    .replay_frames_range(origin, peer_has + 1, max_seq)
                    .map_err(|e| MeshTransportError::Io(e.to_string()))?;
                missing.append(&mut frames);
            }
        }
        missing.sort_by_key(|f| (f.lamport_ts, f.seq));
        Ok(missing)
    }

    async fn acquire_or_renew_lease(
        &self,
        lease: &TaskLease,
        now_ms: i64,
    ) -> Result<LeaseVerdict, MeshTransportError> {
        let mut table = self
            .leases
            .lock()
            .map_err(|_| MeshTransportError::Io("leases lock poisoned".into()))?;
        match table.get(&lease.task_id) {
            None => {
                table.insert(lease.task_id.clone(), lease.clone());
                Ok(LeaseVerdict::Acquired(lease.clone()))
            }
            Some(existing) => {
                let expired = existing.expires_at_ms <= now_ms;
                let same_holder = existing.holder_node_id == lease.holder_node_id;
                let wins_tiebreak = (lease.lamport_ts, &lease.holder_node_id)
                    < (existing.lamport_ts, &existing.holder_node_id);
                if expired || same_holder || wins_tiebreak {
                    table.insert(lease.task_id.clone(), lease.clone());
                    Ok(LeaseVerdict::Acquired(lease.clone()))
                } else {
                    Ok(LeaseVerdict::HeldByPeer(existing.clone()))
                }
            }
        }
    }

    fn status(&self) -> TransportStatus {
        let peers_connected = self.peer_addrs.lock().map(|p| p.len()).unwrap_or(0);
        let outbox_pending = self.outbox.pending().map(|p| p.len()).unwrap_or(0);
        let active_leases = self.leases.lock().map(|l| l.len()).unwrap_or(0);
        TransportStatus {
            peers_connected,
            outbox_pending,
            conflicts_unresolved: 0,
            active_leases,
            frames_rejected: self.frames_rejected.load(Ordering::Relaxed),
        }
    }
}

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
        let outbox_a = Arc::new(Outbox::open(&tmp_a.path().join("a.outbox")).unwrap());
        let outbox_b = Arc::new(Outbox::open(&tmp_b.path().join("b.outbox")).unwrap());

        let node_a = P2pMeshTransport::bind("127.0.0.1:0", "m-test".into(), 1, psk, outbox_a)
            .await
            .unwrap();
        let node_b = P2pMeshTransport::bind("127.0.0.1:0", "m-test".into(), 1, psk, outbox_b)
            .await
            .unwrap();

        let mut rx_b = node_b.subscribe().await.unwrap();
        node_a
            .connect_peer(&node_b.local_addr().to_string())
            .await
            .unwrap();

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
            node_a
                .acquire_or_renew_lease(&lease_1, 1_500_000)
                .await
                .unwrap(),
            LeaseVerdict::Acquired(_)
        ));
        assert!(matches!(
            node_a
                .acquire_or_renew_lease(&lease_2, 1_500_000)
                .await
                .unwrap(),
            LeaseVerdict::HeldByPeer(ref h) if h.holder_node_id == "did:key:z6MkAAA"
        ));
        // After expiry (now_ms > expires_at_ms), lease_2 succeeds
        assert!(matches!(
            node_a
                .acquire_or_renew_lease(&lease_2, 2_500_000)
                .await
                .unwrap(),
            LeaseVerdict::Acquired(ref h) if h.holder_node_id == "did:key:z6MkZZZ"
        ));
    }
}
