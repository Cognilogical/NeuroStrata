# NeuroMesh: Near-Real-Time Multi-Agent Cognitive Mesh Design

> Status: **APPROVED** (2026-11-07).
> Companion: `docs/mesh/synaptic-mesh.md`, `docs/organs/thalamic-bus.md`, `docs/superpowers/specs/2026-10-28-digital-twin-design.md`, `docs/superpowers/specs/2026-10-28-rebac-design.md`.

## 1. Problem

`src/synaptic_mesh/` currently synchronizes only shareable `rule`, `fact`, and `procedure` definitions via batch Git pull/push (`--ff-only`) on a shared repository or one-shot HTTP peer probes (`src/synaptic_mesh/federation.rs`). This model has four limitations when NeuroStrata operates as a general-purpose distributed agentic memory system across independent machines, non-code domains, and multi-agent swarms:

1. **Batch latency & Git coupling:** Agents collaborating in real time cannot wait for `git push` cycles, and non-code agent deployments (operations, research, robotics, digital twins) should not require a shared Git remote to exchange memory.
2. **Incomplete Tri-Strata coverage:** Central Executive `task` rows, distributed `task_claim` exclusivity leases, and `digital_twin` state deltas (`src/digital_twin/mod.rs`) do not cross `SynapticMesh`, leaving swarm agents unable to coordinate live work or world state.
3. **Broker operational friction:** Requiring operators to deploy, configure, and secure an external message broker before linking two agents violates NeuroStrata's single-binary, zero-touch ergonomics.
4. **Silent staleness during active sessions:** While agents should remain mesh-ignorant by default (reading and writing their local `LadybugDB` via MCP), an agent actively working on a task or concept has no low-noise interrupt when a peer invalidates a governing rule, supersedes a twin variable, or claims a sibling task.

## 2. Goal

Evolve `SynapticMesh` into **NeuroMesh** — a near-real-time, event-oriented, database-to-database cognitive mesh built directly into `neurostrata-mcp` that:

- links independent `LadybugDB` instances in real time while keeping connected LLM agents completely mesh-ignorant (`MCP` tools still read/write local `LadybugDB`),
- ships with a **zero-config pure-Rust P2P transport** (`mDNS` LAN auto-discovery + localhost Unix/named-pipe IPC + Ed25519-authenticated QUIC streams) compiled into the single `neurostrata-mcp` binary,
- supports an **optional `nats://` transport adapter** (`async-nats` JetStream + KV, feature-gated `nats-mesh`) for cloud/scale-out swarm topologies without changing higher-level mesh semantics,
- synchronizes the full **Tri-Strata + World State** (`rule`, `fact`, `procedure` definitions, `task` rows + distributed `task_claim` TTL leases, and `digital_twin` `RDF-Patch` deltas),
- injects a targeted, 1-line `[NeuroMesh Δ]` context interrupt on the next MCP tool response **only** when an inbound peer delta intersects the local session's active task, domain, or concept IRIs.

### 2.1 Architectural Decision: Why `LadybugDB` Is the Peer Store (Not a NATS JetStream Storage Engine Replacement)

We explicitly reject forking `nats-server` (Go) to replace its internal JetStream FileStore/MemoryStore with `LadybugDB` via C-FFI:

- `nats-server` tightly couples JetStream storage to its internal Go Raft state machine and append-only segment format; shimming a graph/vector database underneath it adds high FFI fragility for zero semantic gain.
- Every `neurostrata-mcp` daemon **already** embeds `LadybugDB` as its durable local store, `src/synaptic_mesh/outbox.rs` (`outbox.wal`) as its crash-safe write-ahead log, and `src/did.rs` (`did:key:z6Mk...` Ed25519 keypairs) as its cryptographic identity.
- Therefore, `LadybugDB` + `outbox.wal` is the authoritative state and replay log on every peer, and the network layer is abstracted behind a narrow Rust trait (`MeshTransport`) that moves signed `MeshFrame` envelopes over either direct P2P QUIC streams (default) or NATS JetStream subjects (optional scale-out).

## 3. Constraints & Non-Negotiable Invariants

1. **Sovereignty & Privacy Gate:**
   - Rows with `eligibility != "shareable"` (default `"private"`), `structural` AST rows, vector embeddings, and raw conversational logs (`neurostrata_append_log`) **never** cross `MeshTransport`.
   - Procedural memory runtime counters (`remaining_fires`, `performance_count`, `last_performed_at`, `last_episodic_pointer`) are stripped on egress via `src/streaming/mod.rs` (`PRIVATE_QUAD_PREDICATES`) and rejected on ingress if present.
   - ReBAC (`src/rebac.rs`): if a row specifies `metadata.visible_to`, outbound frames are encrypted/routed only to authorized peer DIDs or tagged with the required principal set.
2. **Fail-Soft Everywhere:**
   - An unreachable peer, broken QUIC stream, or disconnected NATS broker queues frames in `outbox.wal` and reports one flattened status line; it **never** blocks local `LadybugDB` reads/writes, MCP tool calls, or git hooks.
3. **Explicit-Zero Status Honesty:**
   - `neurostrata-mcp mesh status` (and `synaptic-mesh status` alias) always reports explicit numeric counts (`peers_connected: 0`, `outbox_pending: 0`, `conflicts_unresolved: 0`, `active_leases: 0`), never blank or omitted fields.
4. **No LLM in Transport or Reconciliation:**
   - Framing, signature verification, causal ordering, conflict detection, and lease coordination are 100% deterministic Rust.
5. **Single-Binary Zero-Config UX:**
   - Default P2P mode requires zero external daemons, zero config files, and zero TLS certificate ceremonies.

## 4. Scope

In scope:

- `MeshTransport` trait (`src/synaptic_mesh/transport.rs`) and `MeshFrame` signed wire envelope (`src/synaptic_mesh/frame.rs`).
- Default `P2pMeshTransport` (`src/synaptic_mesh/p2p.rs`): localhost IPC socket discovery + LAN `mDNS` (`_neurostrata-mesh._udp.local`) + Ed25519-authenticated QUIC streams (`quinn` + `rustls`).
- Optional `NatsMeshTransport` (`src/synaptic_mesh/nats.rs`, Cargo feature `nats-mesh`): `async-nats` JetStream stream + KV lease adapter.
- Legacy `GitMeshTransport` (`src/synaptic_mesh/git_transport.rs`): existing `--ff-only` Git sync wrapped in `MeshTransport` for offline/air-gapped repository archives.
- Event-driven `ThalamicBus` → `Outbox` → `MeshTransport` pipeline (`src/synaptic_mesh/reactor.rs`), replacing poll-only worker draining.
- Full Tri-Strata + World State payload variants:
  - Semantic (`rule`, `fact`) `MeshRecord` + `RDF-Patch` (`src/streaming/mod.rs`).
  - Procedural (`procedure` definitions without local runtime counters).
  - Central Executive (`task` creation/update/completion rows + distributed `TaskLease` TTL exclusivity).
  - Digital Twin (`digital_twin`, `digital_twin_schema` `RDF-Patch` variable deltas).
- Cursor-based offline catch-up (`SyncHello` / `SyncCatchup`) backed by `outbox.wal`.
- Session-scoped `[NeuroMesh Δ]` attention filter (`src/synaptic_mesh/attention.rs`) for targeted MCP response piggybacking.
- CLI (`neurostrata-mcp mesh up | join | status | peers | lease`) and MCP status surfaces.

Out of scope:

- Forking `nats-server` or embedding a Go binary inside `neurostrata-mcp`.
- Cross-organization public DHTs or untrusted open-internet gossip (all peers in a mesh share a signed `nsmesh1...` capability token).
- Streaming raw audio/video/binary blobs over `MeshTransport` (NeuroMesh carries structured cognitive records and `RDF-Patch` deltas, capped at 1 MiB per frame).

## 5. Architecture & Data Flow

### 5.1 Section 1: Pluggable `MeshTransport` & Zero-Config Discovery

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         neurostrata-mcp daemon                          │
│                                                                         │
│  ┌─────────────┐   ThalamicPulse    ┌──────────────┐   AttentionFilter  │
│  │ LadybugStore│ ─────────────────► │ ThalamicBus  │ ─────────────────► │
│  └──────▲──────┘                    └──────┬───────┘   ([NeuroMesh Δ])  │
│         │                                  │                            │
│   Inbound Apply                     Outbound Filter                     │
│  (SHACL + Conflict                  (Shareable + ReBAC                  │
│   + Supersede)                       + Strip Counters)                  │
│         │                                  ▼                            │
│  ┌──────┴──────┐                    ┌──────────────┐                    │
│  │MeshReactor  │ ◄───────────────── │  outbox.wal  │                    │
│  └──────▲──────┘    Seq + Lamport   └──────┬───────┘                    │
│         │                                  │                            │
│         └──────────────┬───────────────────┘                            │
│                        ▼                                                │
│              ┌───────────────────┐                                      │
│              │   MeshTransport   │ (Trait)                              │
│              └──┬──────┬──────┬──┘                                      │
└─────────────────┼──────┼──────┼─────────────────────────────────────────┘
                  │      │      │
       ┌──────────┘      │      └──────────┐
       ▼                 ▼                 ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│ P2pTransport │  │NatsTransport │  │ GitTransport │
│ (Default:    │  │ (Optional:   │  │ (Fallback:   │
│  IPC + mDNS  │  │  nats://     │  │  --ff-only   │
│  + QUIC)     │  │  JetStream)  │  │  git repo)   │
└──────────────┘  └──────────────┘  └──────────────┘
```

#### 5.1.1 Zero-Config UX (`neurostrata-mcp mesh`)

1. **Create or start a mesh on Node A:**
   ```bash
   neurostrata-mcp mesh up
   # → Generates mesh_id + shared capability secret (if new)
   # → Binds localhost IPC + QUIC port + advertises _neurostrata-mesh._udp.local
   # → Prints compact invite token: nsmesh1<base58(mesh_id ||psk || seed_addrs || inviter_did || sig)>
   ```
2. **Join from Node B (same host, LAN, or WAN):**
   ```bash
   neurostrata-mcp mesh join nsmesh1...
   # → Same host / LAN: discovers peer automatically via IPC or mDNS (ignores WAN addr if LAN reachable)
   # → WAN / Tailscale / WireGuard: dials seed_addrs from token over QUIC
   # → Scale-out broker (if token embeds nats:// URL): connects via NatsMeshTransport
   ```

#### 5.1.2 Authentication & Transport Security

- Every daemon already possesses an Ed25519 identity keypair in `src/did.rs` (`did:key:z6Mk...`).
- The `nsmesh1...` invite token encodes:
  - `mesh_id: [u8; 16]` (UUIDv4),
  - `epoch: u64` (key rotation epoch, starts at 1),
  - `mesh_psk: [u8; 32]` (256-bit pre-shared capability key for QUIC ALPN `neurostrata-mesh/1` handshake and frame HMAC/AEAD),
  - `seed_endpoints: Vec<String>` (host:port or `nats://...`),
  - `issuer_did: String` and `issuer_sig: [u8; 64]`.
- During the QUIC handshake, peers prove possession of `mesh_psk` and exchange their `did:key` public keys; every subsequent `MeshFrame` is individually signed by the sender's Ed25519 key (`src/did.rs`).

#### 5.1.3 `MeshTransport` Trait (`src/synaptic_mesh/transport.rs`)

```rust
#[async_trait::async_trait]
pub trait MeshTransport: Send + Sync {
    /// Publish a signed frame to all connected peers (or subject in NATS mode).
    async fn publish(&self, frame: &MeshFrame) -> Result<PublishReceipt, MeshTransportError>;

    /// Subscribe to inbound verified frames from peers.
    async fn subscribe(&self) -> Result<tokio::sync::mpsc::Receiver<MeshFrame>, MeshTransportError>;

    /// Request/response catch-up stream from a specific peer or log store.
    async fn request_catchup(
        &self,
        hello: SyncHello,
    ) -> Result<Vec<MeshFrame>, MeshTransportError>;

    /// Atomic lease acquisition/renewal for distributed Central Executive task claims.
    async fn acquire_or_renew_lease(
        &self,
        lease: &TaskLease,
    ) -> Result<LeaseVerdict, MeshTransportError>;

    /// Snapshot of transport health for `mesh status` (explicit-zero honest).
    fn status(&self) -> TransportStatus;
}
```

### 5.2 Section 2: Tri-Strata Event Protocol & Targeted Agent Interrupts

#### 5.2.1 Wire Envelope (`MeshFrame`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshFrame {
    pub mesh_id: String,
    pub epoch: u64,
    pub origin_node_id: String,   // did:key:z6Mk...
    pub seq: u64,                 // Monotonic per-node sequence number from outbox.wal
    pub lamport_ts: u64,          // Causal Lamport clock across the mesh
    pub timestamp_ms: i64,        // Wall-clock UTC ms (informational / lease TTL check)
    pub namespace: String,        // Source namespace or domain scope
    pub kind: MeshPayloadKind,
    pub signature: String,        // Base64 Ed25519 signature over canonical CBOR/JSON bytes
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MeshPayloadKind {
    /// Semantic (`rule`, `fact`) or Procedural (`procedure` minus counters) upsert/supersede/archive
    CognitiveDelta {
        record: MeshRecord,
        rdf_patch: String,        // Serialized src/streaming/mod.rs Delta
        concept_iris: Vec<String>,
        file_targets: Vec<String>,
    },
    /// Central Executive task row creation, status/note update, or Lock-2 completion
    TaskDelta {
        task_id: String,
        action: TaskMeshAction,   // Created | Updated | Completed
        task_json: serde_json::Value,
        extracted_memory_ids: Vec<String>,
    },
    /// Distributed Central Executive claim exclusivity lease
    TaskLeaseDelta {
        lease: TaskLease,
    },
    /// Digital Twin variable state update or schema upsert
    TwinDelta {
        twin_id: String,
        version: u64,
        source: String,
        rdf_patch: String,
        changed_variables: BTreeMap<String, f64>,
    },
    /// Heartbeat + vector cursor advertisement for anti-entropy
    SyncHello(SyncHello),
}
```

#### 5.2.2 Event Pipeline (`ThalamicBus` → `Outbox` → `MeshTransport`)

1. **Local Mutation:** An agent calls `neurostrata_add_memory`, `neurostrata_task_claim`, `neurostrata_task_complete`, or `neurostrata_twin_set_state` against its local `LadybugDB`.
2. **Pulse Emission:** `LadybugStore` / `server.rs` emits `ThalamicPulse` on `ThalamicBus` (`src/events/`).
3. **Outbound Gate (`MeshReactor`):**
   - Subscribes to `ThalamicBus`.
   - Applies the 3-gate sovereignty filter (Section 5.3.1).
   - Increments the node's monotonic `seq` and advances `lamport_ts = max(local_lamport,last_seen) + 1`.
   - Appends the signed `MeshFrame` to `outbox.wal` (`src/synaptic_mesh/outbox.rs`) and immediately dispatches `transport.publish(&frame)`.
4. **Inbound Application (`MeshReactor`):**
   - Verifies `MeshFrame.signature` against `origin_node_id` (`did:key`), checks `mesh_id` and `epoch`, and deduplicates by `(origin_node_id, seq)`.
   - Updates local `lamport_ts = max(local_lamport, frame.lamport_ts) + 1`.
   - Validates payload via `src/rdf/shacl.rs` and privacy-predicate check (`src/streaming/mod.rs`).
   - Applies to local `LadybugDB` using per-stratum conflict rules (Section 5.3.3).
   - Hands the applied delta to `AttentionFilter` (Section 5.2.3).

#### 5.2.3 Targeted `[NeuroMesh Δ]` Agent Interrupts (`src/synaptic_mesh/attention.rs`)

Agents remain completely unaware of the mesh during normal operation. To prevent an agent from acting on stale assumptions when a peer mutates overlapping state mid-session, `AttentionFilter` tracks each active local MCP session's **working set**:

- `active_task_id` and its `parent_id` / `blocked_by` task IDs,
- `namespace` and active `domain` facets,
- `prefetched_file_paths` and `queried_concept_iris` touched by the session.

When an inbound `MeshFrame` is applied, `AttentionFilter` checks if `(frame.task_id, frame.concept_iris, frame.file_targets, frame.twin_id)` intersects the session's working set.
- **No intersection:** Applied silently to local `LadybugDB`. Zero token overhead in the agent's context window.
- **Intersection:** Queues a compact, 1-line `[NeuroMesh Δ]` notice in the session's pending recall buffer (delivered on the next MCP tool response alongside `execute.after` recall):
  ```text
  [NeuroMesh Δ peer=z6Mkp...8f2a lamport=412] Rule `auth-jwt-expiry` superseded; Task `neurostrata-a91c` claimed by peer; Twin `cooling-loop-1.pressure` → 4.8 bar.
  ```

### 5.3 Section 3: Sovereignty, Offline Catch-Up & Deterministic Conflicts

#### 5.3.1 Three-Gate Outbound Sovereignty Filter

Every candidate outbound mutation must pass all three gates before entering `outbox.wal`:

1. **Eligibility Gate:**
   - `rule`, `fact`, `procedure`: must carry `metadata.eligibility == "shareable"`.
   - `task` and `TaskLease`: must belong to a mesh-bound namespace (`mesh_enabled: true` in namespace config or `metadata.eligibility == "shareable"`).
   - `digital_twin` and `digital_twin_schema`: must carry `metadata.eligibility == "shareable"`.
   - `structural` rows, embeddings, and episodic session logs (`neurostrata_append_log`) are unconditionally blocked.
2. **Privacy Sanitizing Gate:**
   - Strips `remaining_fires`, `performance_count`, `last_performed_at`, `last_episodic_pointer`, and local filesystem paths outside workspace-relative anchors using `src/streaming/mod.rs`.
3. **ReBAC Gate (`src/rebac.rs`):**
   - If `metadata.visible_to` is non-empty, verifies each recipient peer DID is a member of `visible_to` before transmitting over that peer's QUIC stream (or encrypts the payload to the recipient DIDs' X25519 keys derived from their Ed25519 keys when publishing over a shared NATS subject).

#### 5.3.2 Offline Catch-Up & Anti-Entropy (`SyncHello` / `SyncCatchup`)

Every node maintains a durable `peer_cursors: BTreeMap<String, u64>` table in `LadybugDB` mapping each known `origin_node_id` to its highest contiguous applied `seq`.

1. On connection establishment (and every 30s idle heartbeat), peers exchange `SyncHello { node_id, mesh_id, epoch, peer_cursors }`.
2. If Node A sees that Node B's cursor for Node X (`peer_cursors.get(X).copied().unwrap_or(0)`) is behind Node A's retained frames for Node X in `outbox.wal`, Node A streams the missing `MeshFrame` sequence `(cursor + 1 ..= max_seq)` in causal order.
3. **Log Compaction Fallback:** `outbox.wal` retains the last `max_wal_frames` (default `50,000` frames or `64 MiB`). If a returning node's cursor is older than the earliest retained `seq` in `outbox.wal`, Node A sends a deterministic `SnapshotBaseline` of all active shareable rows in the namespace followed by the tail of `outbox.wal`.

#### 5.3.3 Per-Stratum Deterministic Conflict Resolution

| Stratum / Surface | Concurrent Mutation Condition | Resolution Rule |
|---|---|---|
| **Semantic & Procedural** (`rule`, `fact`, `procedure`) | Two nodes concurrently mutate the same `memory_id` without one superseding the other's `content_hash` | **Preserve Both Variants (`src/synaptic_mesh/conflict.rs`):** Neither variant is silently overwritten. Both rows are stored (`id` and `id#conflict:<origin_short_did>`), linked via a conflict record, and surfaced in `mesh status` (`conflicts_unresolved: N`). Agents querying that ID receive a conflict warning until resolved via `neurostrata_supersede_memory`. |
| **Private Row Sovereignty** | Inbound `MeshFrame` targets an `id` owned by a local `private` row | **Refuse Import (`PrivateCollision`):** Local private row is untouched; inbound frame is logged to the conflict ledger and rejected. |
| **Central Executive Lease** (`task_claim`) | Two nodes concurrently claim the same `task_id` | **Deterministic Lease Arbitration:** Each claim carries `TaskLease { task_id, holder_node_id, session_id, lamport_ts, ttl_ms: 3_600_000, expires_at_ms }`. In `NatsMeshTransport`, JetStream KV `create`/`update` CAS picks a single winner atomically. In `P2pMeshTransport`, if two claims overlap within the RTT window, the lower `(lamport_ts, holder_node_id)` wins deterministically; the losing node auto-unclaims locally and emits a `[NeuroMesh Δ]` notice to its session so the agent picks another ready task. Heartbeats renew active leases every 60s; crashed nodes' leases expire automatically at `expires_at_ms`. |
| **Task Notes & Status** (`task_update` / `task_complete`) | Concurrent notes or status transitions on a task | **Append-Only Notes + Monotonic Status Lattice:** Task notes are unioned and sorted by `(lamport_ts, origin_node_id)`. Task status follows the monotonic lattice `open < in_progress < done` (with `blocked` ordered by `(lamport_ts, origin_node_id)` against `in_progress`). `done` is only accepted if `extracted_memory_ids` is non-empty (Lock 2 enforced across the mesh). |
| **Digital Twin Variables** (`digital_twin`) | Concurrent variable updates on the same `twin_id` | **Per-Variable LWW (`(lamport_ts, origin_node_id)`):** Variable maps merge key-by-key using Last-Writer-Wins ordered by `(lamport_ts, origin_node_id)`, producing a deterministic merged supersede row while preserving both parent versions in `twin_history`. |

## 6. Interfaces

- `src/synaptic_mesh/transport.rs`: `MeshTransport`, `TransportStatus`, `PublishReceipt`, `LeaseVerdict`, `MeshTransportError`.
- `src/synaptic_mesh/frame.rs`: `MeshFrame`, `MeshPayloadKind`, `TaskMeshAction`, `TaskLease`, `SyncHello`, `InviteToken` (`nsmesh1...` parser/encoder + Ed25519 signature verifier).
- `src/synaptic_mesh/p2p.rs`: `P2pMeshTransport` (localhost Unix domain socket / named pipe discovery, `mdns-sd` `_neurostrata-mesh._udp.local` LAN discovery, `quinn` QUIC framed streams).
- `src/synaptic_mesh/nats.rs` (feature `nats-mesh`): `NatsMeshTransport` (`async-nats` JetStream subjects `neuromesh.<mesh_id>.<namespace>.delta` + KV bucket `neuromesh_leases_<mesh_id>`).
- `src/synaptic_mesh/reactor.rs`: `MeshReactor` (subscribes to `ThalamicBus`, writes `outbox.wal`, drives `MeshTransport`, applies inbound frames, updates `peer_cursors`).
- `src/synaptic_mesh/attention.rs`: `AttentionFilter` (tracks per-session working set and formats `[NeuroMesh Δ]` piggyback notices).
- `src/cli.rs` & `src/main.rs`: `neurostrata-mcp mesh up | join <token> | status | peers | leave` (preserving `synaptic-mesh` alias).

## 7. Edge Cases

1. **Same-machine multi-daemon swarm:** Multiple worktrees or agents pointing at distinct `LadybugDB` directories on the same host discover each other via `~/.config/NeuroStrata/mesh-ipc/<mesh_id>.sock` even when UDP multicast (`mDNS`) is blocked by container/VPN firewalls.
2. **Split-brain during `task_claim`:** If Node B is offline when it claims `task-1` and reconnects after Node A has already claimed `task-1` (with an earlier `(lamport_ts, node_id)`), Node B's lease loses arbitration on reconnect; Node B's task row reverts assignee to Node A while preserving any notes Node B wrote in the task history, and Node B's session receives a `[NeuroMesh Δ]` collision alert.
3. **Remote `task_complete` without Lock-2 memory:** If a peer sends `TaskDelta { action: Completed, extracted_memory_ids: [] }`, inbound validation rejects the `done` transition (`Lock2Violation`) and keeps the task `in_progress`.
4. **Malformed or forged `MeshFrame`:** Invalid Ed25519 signature, mismatched `mesh_id`/`epoch`, `NaN`/`±Inf` in `TwinDelta`, or presence of forbidden procedure runtime counters causes immediate frame rejection before touching `LadybugDB`, incrementing `frames_rejected` in `mesh status`.
5. **Out-of-order frame delivery:** If `seq = 12` arrives from Node X when local `peer_cursors[X] == 10`, frame `12` is buffered in a bounded per-peer reorder window (max 64 frames) and triggers an immediate `SyncHello` catch-up request for `seq = 11`.

## 8. Testing

- **Unit (`src/synaptic_mesh/frame.rs`):**
  - `nsmesh1...` invite token round-trip encoding, tamper rejection, and expiry/epoch checks.
  - `MeshFrame` Ed25519 signing and verification via `src/did.rs`.
  - Sovereignty gate: verify `private` rows, `structural` rows, embeddings, and `procedure` fire counters are blocked/stripped on egress and rejected on ingress.
- **Unit (`src/synaptic_mesh/attention.rs`):**
  - Working-set intersection triggers `[NeuroMesh Δ]` only for matching `task_id`, `concept_iris`, `file_targets`, or `twin_id`; unrelated peer traffic produces zero output.
- **Integration (In-Memory / Localhost P2P Two-Node Mesh):**
  - Spin up two independent `LadybugStore` + `MeshReactor` instances over localhost QUIC/IPC.
  - Verify live propagation (<50ms) of:
    1. Shareable `rule` creation and supersede.
    2. Shareable `procedure` creation (confirming `remaining_fires` is stripped on the wire and initialized locally on Node B).
    3. `task_create` → `task_claim` lease exclusivity (Node B blocked from claiming an active lease held by Node A) → `task_complete` with Lock-2 extracted memory.
    4. Concurrent conflicting `rule` edits on Node A and Node B preserve both variants in `src/synaptic_mesh/conflict.rs`.
    5. Concurrent `digital_twin` variable updates merge deterministically by `(lamport_ts, origin_node_id)`.
- **Integration (Offline Catch-Up):**
  - Disconnect Node B, perform 20 mutations on Node A, reconnect Node B, verify `SyncHello` replays frames `1..=20` from `outbox.wal` and converges `peer_cursors`.

## 9. Acceptance Criteria

1. Two independent `neurostrata-mcp` instances can link via `neurostrata-mcp mesh up` and `neurostrata-mcp mesh join <token>` with zero external broker or Git remote required.
2. Shareable `rule`, `fact`, `procedure` (without counters), `task` + `TaskLease`, and `digital_twin` mutations propagate in near real time via `ThalamicBus` → `outbox.wal` → `MeshTransport`.
3. Private rows, embeddings, AST structural rows, and unauthorized ReBAC rows never leave the originating node.
4. `neurostrata-mcp mesh status` reports explicit-zero metrics (`peers_connected`, `outbox_pending`, `conflicts_unresolved`, `active_leases`, `frames_rejected`).
5. Active MCP sessions receive `[NeuroMesh Δ]` notices only when inbound deltas intersect their active working set.

## 10. Files Touched (Expected)

- `src/synaptic_mesh/mod.rs`: export `transport`, `frame`, `p2p`, `reactor`, `attention`, and optional `nats`.
- `src/synaptic_mesh/transport.rs`: `MeshTransport` trait and status types.
- `src/synaptic_mesh/frame.rs`: `MeshFrame`, `MeshPayloadKind`, `TaskLease`, `SyncHello`, `InviteToken`.
- `src/synaptic_mesh/p2p.rs`: default localhost IPC + `mDNS` + QUIC transport implementation.
- `src/synaptic_mesh/nats.rs`: feature-gated (`nats-mesh`) `async-nats` JetStream + KV transport implementation.
- `src/synaptic_mesh/reactor.rs`: event-driven `ThalamicBus` subscriber, 3-gate outbound filter, inbound validator/applier, and catch-up coordinator.
- `src/synaptic_mesh/attention.rs`: session working-set tracker and `[NeuroMesh Δ]` formatter.
- `src/synaptic_mesh/outbox.rs`: add monotonic `seq` + `lamport_ts` indexing and range replay `(from_seq..=to_seq)` for `SyncHello` catch-up.
- `src/server.rs`: wire `MeshReactor` into daemon startup, enforce remote `TaskLease` checks in `task_claim`, and attach pending `[NeuroMesh Δ]` notices to MCP tool responses.
- `src/cli.rs` & `src/main.rs`: add `mesh` subcommand (`up`, `join`, `status`, `peers`, `leave`).
- `Cargo.toml`: add pure-Rust P2P dependencies (`quinn`, `rustls`, `mdns-sd`) and optional `nats-mesh` feature (`async-nats`).
