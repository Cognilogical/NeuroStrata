//! Pluggable `MeshTransport` trait and explicit-zero status surfaces for NeuroMesh.

use crate::synaptic_mesh::frame::{MeshFrame, SyncHello, TaskLease};

/// Explicit-zero transport health snapshot surfaced by `neurostrata-mcp mesh status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportStatus {
    pub peers_connected: usize,
    pub outbox_pending: usize,
    pub conflicts_unresolved: usize,
    pub active_leases: usize,
    pub frames_rejected: usize,
}

impl TransportStatus {
    /// Format a single explicit-zero status line (no blank or omitted counters).
    pub fn format_status_line(&self, mesh_id: &str) -> String {
        format!(
            "mesh: {mesh_id} | peers_connected: {} | outbox_pending: {} | conflicts_unresolved: {} | active_leases: {} | frames_rejected: {}",
            self.peers_connected,
            self.outbox_pending,
            self.conflicts_unresolved,
            self.active_leases,
            self.frames_rejected,
        )
    }
}

/// Receipt returned when publishing a [`MeshFrame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReceipt {
    pub delivered_peers: usize,
    pub queued_in_wal: bool,
}

/// Outcome of distributed `task_claim` lease arbitration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseVerdict {
    Acquired(TaskLease),
    HeldByPeer(TaskLease),
}

/// Transport-layer error variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshTransportError {
    Io(String),
    Auth(String),
    FrameTooLarge(usize),
    Closed,
}

impl std::fmt::Display for MeshTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "mesh transport IO error: {e}"),
            Self::Auth(e) => write!(f, "mesh transport auth error: {e}"),
            Self::FrameTooLarge(n) => write!(f, "mesh frame size {n} exceeds limit"),
            Self::Closed => write!(f, "mesh transport closed"),
        }
    }
}

impl std::error::Error for MeshTransportError {}

/// Pluggable byte-stream transport abstraction over P2P, NATS JetStream, or Git fallback.
#[async_trait::async_trait]
pub trait MeshTransport: Send + Sync {
    /// Publish a signed frame to all connected peers.
    async fn publish(&self, frame: &MeshFrame) -> Result<PublishReceipt, MeshTransportError>;

    /// Subscribe to verified inbound frames from peers.
    async fn subscribe(
        &self,
    ) -> Result<tokio::sync::broadcast::Receiver<MeshFrame>, MeshTransportError>;

    /// Request missing frames from peers or local outbox based on `hello.peer_cursors`.
    async fn request_catchup(&self, hello: SyncHello)
        -> Result<Vec<MeshFrame>, MeshTransportError>;

    /// Deterministically arbitrate or renew a distributed Central Executive task lease.
    async fn acquire_or_renew_lease(
        &self,
        lease: &TaskLease,
        now_ms: i64,
    ) -> Result<LeaseVerdict, MeshTransportError>;

    /// Snapshot transport counters for `mesh status` (explicit-zero honest).
    fn status(&self) -> TransportStatus;

    /// Register a peer endpoint discovered via handshake (`SyncHello`).
    fn register_peer_endpoint(&self, _endpoint: &str) {}
}
