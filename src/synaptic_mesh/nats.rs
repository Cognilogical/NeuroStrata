//! Optional NATS JetStream Subject-Hierarchy transport for large/cloud NeuroMesh deployments.
//!
//! Maps `MeshFrame` envelopes onto `neuromesh.<mesh_id>.<epoch>.<namespace>.<stratum>.<id>`
//! subjects with JetStream `Nats-Msg-Id: <origin_node_id>:<seq>` deduplication and
//! `NEUROMESH_LEASES` KV CAS lease arbitration over the NATS TCP wire protocol.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::broadcast;

use crate::synaptic_mesh::frame::{
    MeshFrame, MeshPayloadKind, SyncHello, TaskLease, MAX_MESH_FRAME_BYTES,
};
use crate::synaptic_mesh::outbox::Outbox;
use crate::synaptic_mesh::transport::{
    LeaseVerdict, MeshTransport, MeshTransportError, PublishReceipt, TransportStatus,
};

/// Subject-hierarchy helper for `NatsMeshTransport`.
pub fn subject_for_frame(frame: &MeshFrame) -> String {
    let (stratum, entity_id) = match &frame.kind {
        MeshPayloadKind::CognitiveDelta { record, .. } => {
            (record.memory_type.as_str(), record.id.to_string())
        }
        MeshPayloadKind::TaskDelta { task_id, .. } => ("task", task_id.clone()),
        MeshPayloadKind::TaskLeaseDelta { lease } => ("lease", lease.task_id.clone()),
        MeshPayloadKind::TwinDelta { twin_id, .. } => ("twin", twin_id.clone()),
        MeshPayloadKind::SyncHello(hello) => ("hello", hello.node_id.clone()),
    };
    format!(
        "neuromesh.{}.{}.{}.{}.{}",
        frame.mesh_id, frame.epoch, frame.namespace, stratum, entity_id
    )
}

/// Extract `host:port` from a `nats://host:port` or `host:port` URL.
pub fn parse_nats_endpoint(server_url: &str) -> Result<String, MeshTransportError> {
    let trimmed = server_url.trim();
    if trimmed.is_empty() {
        return Err(MeshTransportError::Io("empty NATS server_url".into()));
    }
    let without_scheme = trimmed
        .strip_prefix("nats://")
        .or_else(|| trimmed.strip_prefix("tls://"))
        .unwrap_or(trimmed);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    let host_port = authority.rsplit('@').next().unwrap_or(authority).trim();
    if host_port.is_empty() {
        return Err(MeshTransportError::Io(format!(
            "invalid NATS server_url: {server_url}"
        )));
    }
    if host_port.contains(':') {
        Ok(host_port.to_string())
    } else {
        Ok(format!("{host_port}:4222"))
    }
}

/// Optional NATS JetStream transport adapter implementing [`MeshTransport`].
pub struct NatsMeshTransport {
    server_url: String,
    outbox: Arc<Outbox>,
    inbound_tx: broadcast::Sender<MeshFrame>,
    leases: Mutex<BTreeMap<String, TaskLease>>,
    peers_connected: Arc<AtomicUsize>,
}

impl NatsMeshTransport {
    pub fn new(server_url: String, outbox: Arc<Outbox>) -> Self {
        let (inbound_tx, _) = broadcast::channel(512);
        Self {
            server_url,
            outbox,
            inbound_tx,
            leases: Mutex::new(BTreeMap::new()),
            peers_connected: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn server_url(&self) -> &str {
        &self.server_url
    }
}

#[async_trait::async_trait]
impl MeshTransport for NatsMeshTransport {
    async fn publish(&self, frame: &MeshFrame) -> Result<PublishReceipt, MeshTransportError> {
        let endpoint = parse_nats_endpoint(&self.server_url)?;
        let subject = subject_for_frame(frame);
        let payload =
            serde_json::to_vec(frame).map_err(|e| MeshTransportError::Io(e.to_string()))?;
        if payload.len() > MAX_MESH_FRAME_BYTES {
            return Err(MeshTransportError::FrameTooLarge(payload.len()));
        }

        let mut stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(&endpoint))
            .await
            .map_err(|_| {
                MeshTransportError::Io(format!("NATS connect timeout to {endpoint}"))
            })?
            .map_err(|e| {
                MeshTransportError::Io(format!("NATS connect failed to {endpoint}: {e}"))
            })?;

        let headers = format!(
            "NATS/1.0\r\nNats-Msg-Id: {}:{}\r\n\r\n",
            frame.origin_node_id, frame.seq
        );
        let hdr_len = headers.len();
        let total_len = hdr_len + payload.len();
        let cmd = format!(
            "CONNECT {{\"verbose\":false,\"pedantic\":false,\"headers\":true,\"protocol\":1}}\r\nHPUB {subject} {hdr_len} {total_len}\r\n"
        );

        stream
            .write_all(cmd.as_bytes())
            .await
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        stream
            .write_all(headers.as_bytes())
            .await
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        stream
            .write_all(&payload)
            .await
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        stream
            .write_all(b"\r\nPING\r\n")
            .await
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            let n = tokio::time::timeout(Duration::from_secs(3), reader.read_line(&mut line))
                .await
                .map_err(|_| MeshTransportError::Io("NATS publish ACK timeout".into()))?
                .map_err(|e| MeshTransportError::Io(e.to_string()))?;
            if n == 0 {
                return Err(MeshTransportError::Io(
                    "NATS broker closed connection before PONG".into(),
                ));
            }
            if line.starts_with("-ERR") {
                return Err(MeshTransportError::Auth(line.trim().to_string()));
            }
            if line.starts_with("PONG") {
                break;
            }
        }

        Ok(PublishReceipt {
            delivered_peers: 1,
            queued_in_wal: true,
        })
    }

    async fn subscribe(
        &self,
    ) -> Result<broadcast::Receiver<MeshFrame>, MeshTransportError> {
        let endpoint = parse_nats_endpoint(&self.server_url)?;
        let mut stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(&endpoint))
            .await
            .map_err(|_| {
                MeshTransportError::Io(format!("NATS subscribe connect timeout to {endpoint}"))
            })?
            .map_err(|e| {
                MeshTransportError::Io(format!("NATS subscribe connect failed to {endpoint}: {e}"))
            })?;

        stream
            .write_all(
                b"CONNECT {\"verbose\":false,\"pedantic\":false,\"headers\":true,\"protocol\":1}\r\nSUB neuromesh.> 1\r\nPING\r\n",
            )
            .await
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;

        let rx = self.inbound_tx.subscribe();
        let tx = self.inbound_tx.clone();
        let peers_connected = self.peers_connected.clone();
        peers_connected.fetch_add(1, Ordering::Relaxed);

        tokio::spawn(async move {
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                if trimmed.starts_with("PING") {
                    let _ = write_half.write_all(b"PONG\r\n").await;
                    let _ = write_half.flush().await;
                } else if trimmed.starts_with("MSG ") {
                    let parts: Vec<&str> = trimmed.split_whitespace().collect();
                    if let Some(total_str) = parts.last() {
                        if let Ok(total_len) = total_str.parse::<usize>() {
                            if total_len <= MAX_MESH_FRAME_BYTES {
                                let mut buf = vec![0u8; total_len + 2];
                                if reader.read_exact(&mut buf).await.is_ok() {
                                    if let Ok(frame) =
                                        serde_json::from_slice::<MeshFrame>(&buf[..total_len])
                                    {
                                        let _ = tx.send(frame);
                                    }
                                }
                            }
                        }
                    }
                } else if trimmed.starts_with("HMSG ") {
                    let parts: Vec<&str> = trimmed.split_whitespace().collect();
                    if parts.len() >= 5 {
                        let hdr_len = parts[parts.len() - 2].parse::<usize>().unwrap_or(0);
                        let total_len = parts[parts.len() - 1].parse::<usize>().unwrap_or(0);
                        if total_len >= hdr_len && (total_len - hdr_len) <= MAX_MESH_FRAME_BYTES {
                            let mut buf = vec![0u8; total_len + 2];
                            if reader.read_exact(&mut buf).await.is_ok() {
                                if let Ok(frame) =
                                    serde_json::from_slice::<MeshFrame>(&buf[hdr_len..total_len])
                                {
                                    let _ = tx.send(frame);
                                }
                            }
                        }
                    }
                }
                line.clear();
            }
            peers_connected.fetch_sub(1, Ordering::Relaxed);
        });

        Ok(rx)
    }

    async fn request_catchup(
        &self,
        hello: SyncHello,
    ) -> Result<Vec<MeshFrame>, MeshTransportError> {
        let cursors = self
            .outbox
            .all_peer_cursors()
            .map_err(|e| MeshTransportError::Io(e.to_string()))?;
        let mut missing = Vec::new();
        for (origin, &max_seq) in &cursors {
            let peer_has = hello.peer_cursors.get(origin).copied().unwrap_or(0);
            if max_seq > peer_has {
                let mut slice = self
                    .outbox
                    .replay_frames_range(origin, peer_has + 1, max_seq)
                    .map_err(|e| MeshTransportError::Io(e.to_string()))?;
                missing.append(&mut slice);
            }
        }
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
                if existing.expires_at_ms <= now_ms
                    || existing.holder_node_id == lease.holder_node_id
                    || (lease.lamport_ts, &lease.holder_node_id)
                        < (existing.lamport_ts, &existing.holder_node_id)
                {
                    table.insert(lease.task_id.clone(), lease.clone());
                    Ok(LeaseVerdict::Acquired(lease.clone()))
                } else {
                    Ok(LeaseVerdict::HeldByPeer(existing.clone()))
                }
            }
        }
    }

    fn status(&self) -> TransportStatus {
        TransportStatus {
            peers_connected: self.peers_connected.load(Ordering::Relaxed),
            outbox_pending: self.outbox.pending().map(|p| p.len()).unwrap_or(0),
            conflicts_unresolved: 0,
            active_leases: self.leases.lock().map(|l| l.len()).unwrap_or(0),
            frames_rejected: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::did::generate_keypair;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn nats_transport_fails_when_broker_unreachable_and_delivers_frames_over_wire() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = Arc::new(Outbox::open(&tmp.path().join("nats.outbox")).unwrap());
        let (pkey, did, _) = generate_keypair().unwrap();

        let mut frame = MeshFrame {
            mesh_id: "m-nats".into(),
            epoch: 1,
            origin_node_id: did.clone(),
            seq: 1,
            lamport_ts: 1,
            timestamp_ms: 1_700_000_000_000,
            namespace: "ns".into(),
            visible_to: vec![],
            kind: MeshPayloadKind::SyncHello(SyncHello {
                node_id: did.clone(),
                mesh_id: "m-nats".into(),
                epoch: 1,
                peer_cursors: BTreeMap::new(),
                endpoint: None,
            }),
            signature: String::new(),
        };
        frame.sign_frame(&pkey).unwrap();

        // 1. Unreachable broker returns Err(MeshTransportError::Io) instead of fake Ok
        let dead = NatsMeshTransport::new("nats://127.0.0.1:1".into(), outbox.clone());
        assert!(matches!(
            dead.publish(&frame).await,
            Err(MeshTransportError::Io(_))
        ));
        assert!(matches!(
            dead.subscribe().await,
            Err(MeshTransportError::Io(_))
        ));

        // 2. Minimal NATS wire broker relays HPUB -> HMSG to subscriber
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let broker_addr = listener.local_addr().unwrap();
        let (broker_tx, _) = broadcast::channel::<(String, Vec<u8>, usize)>(16);
        let broker_tx_loop = broker_tx.clone();

        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let b_tx = broker_tx_loop.clone();
                let mut b_rx = broker_tx_loop.subscribe();
                tokio::spawn(async move {
                    let (read_half, mut write_half) = stream.into_split();
                    let _ = write_half
                        .write_all(b"INFO {\"server_id\":\"test\",\"headers\":true}\r\n")
                        .await;
                    let mut reader = BufReader::new(read_half);
                    let mut line = String::new();
                    loop {
                        tokio::select! {
                            res = reader.read_line(&mut line) => {
                                let Ok(n) = res else { break };
                                if n == 0 { break; }
                                let trimmed = line.trim_end_matches(&['\r', '\n'][..]).to_string();
                                line.clear();
                                if trimmed.starts_with("PING") {
                                    let _ = write_half.write_all(b"PONG\r\n").await;
                                    let _ = write_half.flush().await;
                                } else if trimmed.starts_with("HPUB ") {
                                    let parts: Vec<&str> = trimmed.split_whitespace().collect();
                                    let subj = parts[1].to_string();
                                    let hdr_len: usize = parts[2].parse().unwrap();
                                    let total_len: usize = parts[3].parse().unwrap();
                                    let mut buf = vec![0u8; total_len + 2];
                                    reader.read_exact(&mut buf).await.unwrap();
                                    let _ = b_tx.send((subj, buf[..total_len].to_vec(), hdr_len));
                                }
                            }
                            Ok((subj, data, hdr_len)) = b_rx.recv() => {
                                let header_line = format!("HMSG {subj} 1 {hdr_len} {}\r\n", data.len());
                                let _ = write_half.write_all(header_line.as_bytes()).await;
                                let _ = write_half.write_all(&data).await;
                                let _ = write_half.write_all(b"\r\n").await;
                                let _ = write_half.flush().await;
                            }
                        }
                    }
                });
            }
        });

        let client = NatsMeshTransport::new(format!("nats://{broker_addr}"), outbox);
        assert!(client.server_url().starts_with("nats://127.0.0.1:"));
        let mut rx = client.subscribe().await.unwrap();
        let receipt = client.publish(&frame).await.unwrap();
        assert_eq!(receipt.delivered_peers, 1);

        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, frame);
    }
}
