//! The CLI surface for SynapticMesh: pure argument shaping. Every variant is
//! dispatched to one [`StorageApi`] method and answered with a one-line
//! reply, in the same terse register as the other handlers in `main.rs`.
//!
//! Nothing here touches git or the filesystem — the storage layer (slice 2a)
//! owns that. This file is safe to run with no mesh joined: `push`, `pull`,
//! and `status` answer "no mesh joined" rather than panicking.

use super::storage_api::StorageApi;
use clap::Subcommand;
use std::path::{Path, PathBuf};

/// `neurostrata-mcp synaptic-mesh <cmd>` (alias: `mesh`).
#[derive(Subcommand, Debug, Clone, PartialEq)]
pub enum SynapticMeshCommands {
    /// Create a new mesh and join this project to it
    Init {
        /// Human-readable mesh name
        name: String,

        /// Namespace to share (repeatable). Defaults to the project namespace.
        #[arg(long)]
        namespace: Vec<String>,

        /// Git remote URL the mesh pushes to
        #[arg(long)]
        remote: Option<String>,
    },

    /// Join an existing mesh, or adopt a mesh repo already on this machine
    Join {
        /// Path to the mesh repo (or a teammate's mesh URL's checkout)
        dir: std::path::PathBuf,
    },

    /// Pull-before-push to the mesh remote. Non-interactive, fail-soft.
    Push,

    /// Fetch and replay the mesh remote. Non-interactive, fail-soft.
    Pull,

    /// Report mesh id, namespaces, outbox, heads, and health
    Status,

    /// Leave the mesh: drop this project's pointer. Records stay on disk.
    Leave,

    /// [internal hook target] Pull-then-push in one non-interactive,
    /// fail-soft call for the project-side pre-push hook (spec §Hooks).
    /// ALWAYS one line and the CLI never exits non-zero: a hook must never
    /// block the source push. Manual push/pull remains first-class — this
    /// command is only the hook's target, and manual wiring is documented
    /// at `install_pre_push_hook` in main.rs.
    #[command(hide = true)]
    SyncEnqueue,

    /// Print the mesh checkout's recovery states (dirty tree, detached
    /// HEAD, interrupted merge) with the manual fix for each. Reads only —
    /// never auto-fixes, never `reset --hard` (spec §Recovery).
    Recover,

    /// Start a zero-config real-time NeuroMesh P2P node and issue an `nsmesh1...` invite token
    Up {
        /// Human-readable mesh name
        #[arg(long, default_value = "default")]
        name: String,

        /// Local P2P TCP bind address
        #[arg(long, default_value = "127.0.0.1:34345")]
        bind: String,
    },

    /// Join a real-time NeuroMesh using an `nsmesh1...` capability invite token
    JoinToken {
        /// Compact Ed25519-signed capability token (`nsmesh1...`)
        token: String,
    },

    /// Report real-time NeuroMesh connected peers, active leases, and rejected frames (explicit-zero)
    Peers,
}

/// Everything the handler needs to know about *where* it is running.
///
/// The MCP schemas advertise `project_root` and `node_id` as tool arguments,
/// so those values have to arrive through a parameter — reading the process
/// cwd or an environment variable inside the handler would make the advertised
/// schema a lie and make the handler untestable off-machine. The call site
/// resolves the defaults: cwd, `~/.NeuroStrata/synaptic-mesh` (computed in
/// `mesh_service::conventional_base_root` — deliberately NOT a method here,
/// so no handler path can reach for it), and the configured node id.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshContext {
    /// Project whose `.NeuroStrata/synaptic-mesh-ref` this session reads.
    pub project_root: std::path::PathBuf,
    /// Root under which mesh repos live (`base_root/<mesh_id>/`).
    pub base_root: std::path::PathBuf,
    /// Identity stamped on every record this node publishes.
    pub node_id: String,
}

/// The one message every "no mesh joined" path answers with.
fn no_mesh_joined() -> &'static str {
    "no mesh joined — run `neurostrata-mcp synaptic-mesh init <name>` first"
}

/// Dispatch one synaptic-mesh command and render a one-line reply.
///
/// `Err` from the storage layer is rendered, never propagated as a panic:
/// the CLI prints one line and the caller decides the exit code.
pub fn handle_synaptic_mesh_command(
    cmd: &SynapticMeshCommands,
    ctx: &MeshContext,
    store: &mut dyn StorageApi,
) -> String {
    match cmd {
        SynapticMeshCommands::Init {
            name,
            namespace,
            remote,
        } => render(
            store.init_mesh(
                &ctx.base_root,
                &ctx.node_id,
                name,
                namespace,
                remote.as_deref(),
            ),
            |mesh_id| format!("mesh created: {mesh_id} — joined this project"),
        ),
        // `open_mesh` owns the whole adoption: it opens the repo AND writes
        // the project-side pointer (see StorageApi::open_mesh). Splitting
        // those here would let a caller adopt a mesh with no pointer.
        SynapticMeshCommands::Join { dir } => render(store.open_mesh(dir), |()| {
            format!("joined mesh at {}", dir.display())
        }),
        SynapticMeshCommands::Push => render(store.push(), |line| line),
        SynapticMeshCommands::Pull => render(store.pull(), |line| line),
        SynapticMeshCommands::Status => render(store.status(), |line| line),
        SynapticMeshCommands::Leave => {
            let pointer = super::storage::MeshStorage::pointer_path(&ctx.project_root);
            render(store.remove_pointer(&ctx.project_root), |()| {
                format!("left mesh — pointer removed at {}", pointer.display())
            })
        }
        // The hook target renders its own single line — the value IS the
        // reply, never collapsed and never an Err (exit-0 rule).
        SynapticMeshCommands::SyncEnqueue => super::mesh_service::sync_enqueue_line(store),
        // Recovery is print-only: it reads the checkout, names each paused
        // state, and hands back the manual fix.
        SynapticMeshCommands::Recover => super::mesh_service::recover_report(&ctx.project_root),
        SynapticMeshCommands::Up { name, bind } => {
            if let Err(e) = std::fs::create_dir_all(&ctx.base_root) {
                return format!("error: failed to create base root: {e}");
            }
            let key_path = ctx.base_root.join("node_ed25519.pem");
            let (privkey, did, _) = match crate::did::load_or_generate_keypair(&key_path) {
                Ok(k) => k,
                Err(e) => return format!("error: failed to load or generate node keypair: {e}"),
            };
            let mesh_id = match store.init_mesh(&ctx.base_root, &ctx.node_id, name, &[], None) {
                Ok(id) if !id.trim().is_empty() => id,
                Ok(_) => name.clone(),
                Err(e) => return format!("error: {e}"),
            };
            let mut psk = [0u8; 32];
            if let Err(e) = openssl::rand::rand_bytes(&mut psk) {
                return format!("error: CSPRNG failure generating mesh PSK: {e}");
            }
            let outbox = match super::outbox::Outbox::open(&ctx.base_root.join("neuromesh.outbox"))
            {
                Ok(o) => std::sync::Arc::new(o),
                Err(e) => return format!("error: failed to open neuromesh outbox: {e}"),
            };
            let bind_str = bind.clone();
            let mesh_id_cl = mesh_id.clone();
            let did_cl = did.clone();
            let privkey_cl = privkey.clone();
            let conflicts_dir = ctx.base_root.join("neuromesh.conflicts");
            let node_res = run_on_mesh_runtime(async move {
                start_active_mesh_node(
                    &bind_str,
                    mesh_id_cl,
                    1,
                    psk,
                    did_cl,
                    privkey_cl,
                    outbox,
                    conflicts_dir,
                )
                .await
            });
            let active_node = match node_res {
                Ok(n) => n,
                Err(e) => return format!("error: failed to start neuromesh transport: {e}"),
            };
            let bound_endpoint = active_node.bind_addr.to_string();
            let token = match super::frame::InviteToken::issue(
                &mesh_id,
                1,
                &psk,
                vec![bound_endpoint.clone()],
                &privkey,
            )
            .and_then(|t| t.encode())
            {
                Ok(t) => t,
                Err(e) => return format!("error: failed to issue invite token: {e}"),
            };
            if let Ok(mut map) = active_mesh_nodes().lock() {
                map.insert(ctx.base_root.clone(), active_node);
            }
            format!("neuromesh up: {mesh_id} | bind: {bound_endpoint} | token: {token}")
        }
        SynapticMeshCommands::JoinToken { token } => {
            let inv = match super::frame::InviteToken::decode_and_verify(token) {
                Ok(inv) => inv,
                Err(e) => return format!("error: invalid invite token: {e}"),
            };
            let psk = match inv.psk_bytes() {
                Ok(b) => b,
                Err(e) => return format!("error: invalid invite token PSK: {e}"),
            };
            if inv.seed_endpoints.is_empty() {
                return "error: invite token contains no seed endpoints".to_string();
            }
            if let Err(e) = std::fs::create_dir_all(&ctx.base_root) {
                return format!("error: failed to create base root: {e}");
            }
            let key_path = ctx.base_root.join("node_ed25519.pem");
            let (privkey, did, _) = match crate::did::load_or_generate_keypair(&key_path) {
                Ok(k) => k,
                Err(e) => return format!("error: failed to load or generate node keypair: {e}"),
            };
            let outbox = match super::outbox::Outbox::open(&ctx.base_root.join("neuromesh.outbox"))
            {
                Ok(o) => std::sync::Arc::new(o),
                Err(e) => return format!("error: failed to open neuromesh outbox: {e}"),
            };
            let mesh_id = inv.mesh_id.clone();
            let epoch = inv.epoch;
            let seeds = inv.seed_endpoints.clone();
            let conflicts_dir = ctx.base_root.join("neuromesh.conflicts");
            let join_res = run_on_mesh_runtime(async move {
                let node = start_active_mesh_node(
                    "127.0.0.1:0",
                    mesh_id.clone(),
                    epoch,
                    psk,
                    did.clone(),
                    privkey,
                    outbox.clone(),
                    conflicts_dir,
                )
                .await?;
                let mut connected = 0usize;
                let mut last_err = String::new();
                for seed in &seeds {
                    match node.transport.connect_peer(seed).await {
                        Ok(()) => connected += 1,
                        Err(e) => last_err = e.to_string(),
                    }
                }
                if connected == 0 {
                    return Err(format!(
                        "failed to connect to any seed endpoint ({last_err})"
                    ));
                }
                let cursors = outbox.all_peer_cursors().unwrap_or_default();
                let hello = super::frame::SyncHello {
                    node_id: did,
                    mesh_id,
                    epoch,
                    peer_cursors: cursors,
                    endpoint: Some(node.bind_addr.to_string()),
                };
                node.reactor
                    .publish_payload(
                        "default",
                        vec![],
                        super::frame::MeshPayloadKind::SyncHello(hello),
                    )
                    .await
                    .map_err(|e| format!("failed to publish SyncHello handshake: {e}"))?;
                Ok(node)
            });
            match join_res {
                Ok(active_node) => {
                    let bind_addr = active_node.bind_addr;
                    if let Ok(mut map) = active_mesh_nodes().lock() {
                        map.insert(ctx.base_root.clone(), active_node);
                    }
                    format!(
                        "joined neuromesh: {} (issuer: {}, seeds: {}, bind: {})",
                        inv.mesh_id,
                        inv.issuer_did,
                        inv.seed_endpoints.len(),
                        bind_addr
                    )
                }
                Err(e) => format!("error: {e}"),
            }
        }
        SynapticMeshCommands::Peers => {
            let status = get_active_mesh_node(&ctx.base_root)
                .map(|n| n.reactor.transport_status())
                .unwrap_or_default();
            status.format_status_line(&ctx.node_id)
        }
    }
}

/// Retained live NeuroMesh P2P transport + reactor runtime for a `base_root`.
pub struct ActiveMeshNode {
    pub mesh_id: String,
    pub epoch: u64,
    pub node_did: String,
    pub bind_addr: std::net::SocketAddr,
    pub transport: std::sync::Arc<super::p2p::P2pMeshTransport>,
    pub reactor: std::sync::Arc<super::reactor::MeshReactor>,
    pub outbox: std::sync::Arc<super::outbox::Outbox>,
}

fn mesh_runtime() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("neuromesh-rt")
            .build()
            .expect("failed to build neuromesh background runtime")
    })
}

fn active_mesh_nodes(
) -> &'static std::sync::Mutex<std::collections::BTreeMap<PathBuf, std::sync::Arc<ActiveMeshNode>>>
{
    static NODES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<PathBuf, std::sync::Arc<ActiveMeshNode>>>,
    > = std::sync::OnceLock::new();
    NODES.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// Look up the retained live [`ActiveMeshNode`] for `base_root`, if started via `Up` or `JoinToken`.
pub fn get_active_mesh_node(base_root: &Path) -> Option<std::sync::Arc<ActiveMeshNode>> {
    active_mesh_nodes()
        .lock()
        .ok()
        .and_then(|map| map.get(base_root).cloned())
}

fn run_on_mesh_runtime<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    mesh_runtime().spawn(async move {
        let out = fut.await;
        let _ = tx.send(out);
    });
    rx.recv().expect("neuromesh runtime task failed")
}

async fn start_active_mesh_node(
    bind: &str,
    mesh_id: String,
    epoch: u64,
    psk: [u8; 32],
    did: String,
    privkey: openssl::pkey::PKey<openssl::pkey::Private>,
    outbox: std::sync::Arc<super::outbox::Outbox>,
    conflicts_dir: PathBuf,
) -> Result<std::sync::Arc<ActiveMeshNode>, String> {
    use super::transport::MeshTransport;
    let transport = super::p2p::P2pMeshTransport::bind(
        bind,
        mesh_id.clone(),
        epoch,
        psk,
        outbox.clone(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let bind_addr = transport.local_addr();
    let mut rx = transport.subscribe().await.map_err(|e| e.to_string())?;
    let reactor = std::sync::Arc::new(super::reactor::MeshReactor::new(
        mesh_id.clone(),
        epoch,
        did.clone(),
        privkey,
        outbox.clone(),
        transport.clone(),
        super::attention::global_attention_filter(),
        conflicts_dir,
    ));
    let reactor_loop = reactor.clone();
    tokio::spawn(async move {
        while let Ok(frame) = rx.recv().await {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let _ = reactor_loop.apply_inbound_frame(frame, now_ms).await;
        }
    });
    Ok(std::sync::Arc::new(ActiveMeshNode {
        mesh_id,
        epoch,
        node_did: did,
        bind_addr,
        transport,
        reactor,
        outbox,
    }))
}

/// Map a storage outcome to a one-line reply. An `Err` beginning with the
/// canonical no-mesh prefix collapses onto that single message so every
/// no-mesh path reads identically, whatever the storage layer called it.
fn render<T>(result: Result<T, String>, ok: impl FnOnce(T) -> String) -> String {
    match result {
        Ok(value) => ok(value),
        Err(why) if why.starts_with(NO_MESH_PREFIX) => no_mesh_joined().to_string(),
        Err(why) => format!("error: {why}"),
    }
}

/// The prefix the storage layer uses to signal "no mesh joined"; everything
/// after it is the storage layer's own phrasing — paths, reasons — dropped in
/// favour of the canonical CLI wording.
const NO_MESH_PREFIX: &str = "no mesh joined";
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::{Path, PathBuf};

    fn ctx() -> MeshContext {
        MeshContext {
            project_root: PathBuf::from("/proj"),
            base_root: PathBuf::from("/base"),
            node_id: "node-1".to_string(),
        }
    }

    #[derive(Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: SynapticMeshCommands,
    }

    fn parse(args: &[&str]) -> SynapticMeshCommands {
        Harness::parse_from(args).cmd
    }

    #[test]
    fn cli_parses_all_subcommands() {
        assert_eq!(
            parse(&["h", "init", "team"]),
            SynapticMeshCommands::Init {
                name: "team".to_string(),
                namespace: vec![],
                remote: None
            }
        );
        assert_eq!(
            parse(&[
                "h",
                "init",
                "team",
                "--namespace",
                "Alpha",
                "--namespace",
                "Beta",
                "--remote",
                "git@example.com:team/mesh.git"
            ]),
            SynapticMeshCommands::Init {
                name: "team".to_string(),
                namespace: vec!["Alpha".to_string(), "Beta".to_string()],
                remote: Some("git@example.com:team/mesh.git".to_string())
            }
        );
        assert_eq!(
            parse(&["h", "join", "/tmp/mesh"]),
            SynapticMeshCommands::Join {
                dir: PathBuf::from("/tmp/mesh")
            }
        );
        assert_eq!(parse(&["h", "push"]), SynapticMeshCommands::Push);
        assert_eq!(parse(&["h", "pull"]), SynapticMeshCommands::Pull);
        assert_eq!(parse(&["h", "status"]), SynapticMeshCommands::Status);
        assert_eq!(parse(&["h", "leave"]), SynapticMeshCommands::Leave);
        assert_eq!(
            parse(&["h", "sync-enqueue"]),
            SynapticMeshCommands::SyncEnqueue
        );
        assert_eq!(parse(&["h", "recover"]), SynapticMeshCommands::Recover);
    }

    #[derive(Default, Debug)]
    struct MockMesh {
        init_calls: Vec<(PathBuf, String, String, Vec<String>, Option<String>)>,
        mesh_id: String,
        pointer_removed: Option<PathBuf>,
        /// What push/status/pull answer with; `None` = a bare no-mesh error.
        push_err: Option<String>,
        status_err: Option<String>,
        pull_err: Option<String>,
        /// Ok-answers for pull/push (the mock defaults to failing with the
        /// no-mesh error; these let a test speak the success arms too).
        pull_ok: Option<String>,
        push_ok: Option<String>,
        /// Every trait method called, in order — makes "which calls happened"
        /// assertable without one field per method.
        calls: Vec<String>,
    }

    impl StorageApi for MockMesh {
        fn init_mesh(
            &mut self,
            base_root: &Path,
            node_id: &str,
            mesh_name: &str,
            namespaces: &[String],
            remote: Option<&str>,
        ) -> Result<String, String> {
            self.calls.push("init_mesh".into());
            self.init_calls.push((
                base_root.to_path_buf(),
                node_id.to_string(),
                mesh_name.to_string(),
                namespaces.to_vec(),
                remote.map(str::to_string),
            ));
            Ok(self.mesh_id.clone())
        }
        fn open_mesh(&mut self, mesh_dir: &Path) -> Result<(), String> {
            self.calls.push(format!("open_mesh:{}", mesh_dir.display()));
            Ok(())
        }
        fn write_pointer(
            &mut self,
            project: &Path,
            mesh_id: &str,
            mesh_dir: &Path,
            _remote: Option<&str>,
        ) -> Result<(), String> {
            self.calls.push(format!(
                "write_pointer:{}:{}:{}",
                project.display(),
                mesh_id,
                mesh_dir.display()
            ));
            Ok(())
        }
        fn read_pointer(
            &self,
            _project: &Path,
        ) -> Result<Option<(String, PathBuf, Option<String>)>, String> {
            Ok(None)
        }
        fn remove_pointer(&mut self, project: &Path) -> Result<(), String> {
            self.calls
                .push(format!("remove_pointer:{}", project.display()));
            self.pointer_removed = Some(project.to_path_buf());
            Ok(())
        }
        fn status(&self) -> Result<String, String> {
            Err(self.status_err.clone().unwrap_or_else(|| no_mesh_err()))
        }
        fn push(&mut self) -> Result<String, String> {
            if let Some(line) = self.push_ok.clone() {
                self.calls.push("push:ok".into());
                return Ok(line);
            }
            Err(self.push_err.clone().unwrap_or_else(no_mesh_err))
        }
        fn pull(&mut self) -> Result<String, String> {
            self.calls.push("pull".into());
            if let Some(line) = self.pull_ok.clone() {
                return Ok(line);
            }
            Err(self.pull_err.clone().unwrap_or_else(no_mesh_err))
        }
    }

    /// The storage layer's own phrasing of "nothing is joined here". It must
    /// NOT equal the CLI's canonical message — the collapse is the contract.
    fn no_mesh_err() -> String {
        format!("{NO_MESH_PREFIX} (no pointer at /proj/.NeuroStrata)")
    }

    #[test]
    fn sync_enqueue_pulls_before_pushing_and_renders_one_line() {
        let mut store = MockMesh {
            pull_ok: Some("pulled mesh x".to_string()),
            push_ok: Some("pushed mesh x".to_string()),
            ..Default::default()
        };
        let reply =
            handle_synaptic_mesh_command(&SynapticMeshCommands::SyncEnqueue, &ctx(), &mut store);
        assert_eq!(
            reply, "mesh sync-enqueued: pulled mesh x; pushed mesh x",
            "one line, pull result first: {reply}"
        );
        assert_eq!(
            store.calls,
            vec!["pull", "push:ok"],
            "pull-before-push order"
        );
    }

    #[test]
    fn sync_enqueue_quiets_no_mesh_into_a_soft_nothing_to_do() {
        // Mock pulls fail with the storage-layer no-mesh error by default.
        let mut store = MockMesh::default();
        let reply =
            handle_synaptic_mesh_command(&SynapticMeshCommands::SyncEnqueue, &ctx(), &mut store);
        assert!(
            reply.contains("no mesh joined") && reply.contains("nothing to do"),
            "hook sees a quiet success, not an error: {reply}"
        );
        assert!(
            !store.calls.contains(&"push:ok".to_string()),
            "no push attempted"
        );
    }

    #[test]
    fn sync_enqueue_renders_push_failure_as_one_blocking_free_line() {
        // An unreachable/rejecting remote: the hook STILL gets one line and
        // the CLI still exits 0 — never blocks the source push (spec §Hooks).
        let mut store = MockMesh {
            pull_ok: Some("pulled mesh x".to_string()),
            push_err: Some("git push failed: connection refused by unreachable remote".into()),
            ..Default::default()
        };
        let reply =
            handle_synaptic_mesh_command(&SynapticMeshCommands::SyncEnqueue, &ctx(), &mut store);
        assert!(
            reply.starts_with("mesh sync-enqueue failed (non-blocking"),
            "fatal shape for a hook: {reply}"
        );
        assert_eq!(reply.lines().count(), 1, "exactly one line");
        assert!(
            reply.contains("connection refused"),
            "the failure detail survives for the log: {reply}"
        );
    }

    #[test]
    fn init_command_calls_trait_and_reports_mesh_id() {
        let mut store = MockMesh {
            mesh_id: "deadbeef".to_string(),
            ..Default::default()
        };
        let cmd = SynapticMeshCommands::Init {
            name: "team".to_string(),
            namespace: vec!["Alpha".to_string(), "Beta".to_string()],
            remote: Some("git@example.com:team/mesh.git".to_string()),
        };
        let reply = handle_synaptic_mesh_command(&cmd, &ctx(), &mut store);
        assert!(
            reply.contains("deadbeef"),
            "reply must name the mesh id: {reply}"
        );
        assert_eq!(
            store.init_calls,
            vec![(
                PathBuf::from("/base"),
                "node-1".to_string(),
                "team".to_string(),
                vec!["Alpha".to_string(), "Beta".to_string()],
                Some("git@example.com:team/mesh.git".to_string()),
            )]
        );
    }

    #[test]
    fn push_pull_status_without_mesh_say_no_mesh_joined() {
        let mut store = MockMesh::default();
        for cmd in [
            SynapticMeshCommands::Push,
            SynapticMeshCommands::Pull,
            SynapticMeshCommands::Status,
        ] {
            let reply = handle_synaptic_mesh_command(&cmd, &ctx(), &mut store);
            assert_eq!(reply, no_mesh_joined(), "clean message for {cmd:?}");
        }
    }

    /// Issue 4: the storage layer words its no-mesh errors with paths; the
    /// canonical CLI wording must survive that. 3h37's MeshStorage impl
    /// depends on this collapsing rather than leaking `no mesh joined at
    /// /some/path` to the user.
    #[test]
    fn render_collapses_path_carrying_no_mesh_errors() {
        let mut store = MockMesh {
            push_err: Some("no mesh joined at /weird/path".to_string()),
            pull_err: Some("no mesh joined at /another/weird/path".to_string()),
            status_err: Some("no mesh joined: pointer file unreadable".to_string()),
            ..Default::default()
        };
        for cmd in [
            SynapticMeshCommands::Push,
            SynapticMeshCommands::Pull,
            SynapticMeshCommands::Status,
        ] {
            let reply = handle_synaptic_mesh_command(&cmd, &ctx(), &mut store);
            assert_eq!(
                reply,
                no_mesh_joined(),
                "{cmd:?} must collapse its storage wording"
            );
            assert!(!reply.contains("/weird/path"), "path leaked: {reply}");
        }
    }

    #[test]
    fn leave_removes_pointer_at_the_context_project_root() {
        let mut store = MockMesh::default();
        let reply = handle_synaptic_mesh_command(&SynapticMeshCommands::Leave, &ctx(), &mut store);
        assert_eq!(
            store.pointer_removed.as_deref(),
            Some(Path::new("/proj")),
            "remove_pointer must target ctx.project_root, not the process cwd"
        );
        assert!(
            reply.contains(
                &Path::new("/proj")
                    .join(".NeuroStrata")
                    .join("synaptic-mesh-ref")
                    .display()
                    .to_string()
            ),
            "reply: {reply}"
        );
        assert!(reply.to_lowercase().contains("left"), "reply: {reply}");
    }

    /// Issue 3: pointer-writing belongs to the storage layer (`open_mesh`
    /// adopts AND writes the pointer per storage_api.rs), so this asserts the
    /// call the CLI actually makes — and, by the `calls` log, that it makes
    /// no pointer write of its own.
    #[test]
    fn join_opens_the_mesh_at_the_given_dir() {
        let mut store = MockMesh::default();
        let reply = handle_synaptic_mesh_command(
            &SynapticMeshCommands::Join {
                dir: PathBuf::from("/tmp/mesh"),
            },
            &ctx(),
            &mut store,
        );
        assert_eq!(
            store.calls,
            vec!["open_mesh:/tmp/mesh".to_string()],
            "join delegates the whole adoption to open_mesh"
        );
        assert!(
            reply.contains("joined") || reply.contains("Join"),
            "reply: {reply}"
        );
    }

    #[test]
    fn init_failure_reports_error_without_panicking() {
        struct Failing;
        impl StorageApi for Failing {
            fn init_mesh(
                &mut self,
                _: &Path,
                _: &str,
                _: &str,
                _: &[String],
                _: Option<&str>,
            ) -> Result<String, String> {
                Err("disk full".to_string())
            }
            fn open_mesh(&mut self, _: &Path) -> Result<(), String> {
                unreachable!()
            }
            fn write_pointer(
                &mut self,
                _: &Path,
                _: &str,
                _: &Path,
                _: Option<&str>,
            ) -> Result<(), String> {
                unreachable!()
            }
            fn read_pointer(
                &self,
                _: &Path,
            ) -> Result<Option<(String, PathBuf, Option<String>)>, String> {
                unreachable!()
            }
            fn remove_pointer(&mut self, _: &Path) -> Result<(), String> {
                unreachable!()
            }
            fn status(&self) -> Result<String, String> {
                unreachable!()
            }
            fn push(&mut self) -> Result<String, String> {
                unreachable!()
            }
            fn pull(&mut self) -> Result<String, String> {
                unreachable!()
            }
        }

        let mut store = Failing;
        let reply = handle_synaptic_mesh_command(
            &SynapticMeshCommands::Init {
                name: "team".to_string(),
                namespace: vec![],
                remote: None,
            },
            &ctx(),
            &mut store,
        );
        assert!(reply.contains("disk full"), "reply: {reply}");
    }

    /// Issue 1: `MeshContext` is the ONLY channel for project root, mesh root
    /// and node id. With an explicit context in hand, the handler must not
    /// consult `$HOME` or `$NEUROSTRATA_NODE_ID` at all.
    #[test]
    fn context_alone_supplies_every_ambient_value() {
        let explicit = MeshContext {
            project_root: PathBuf::from("/explicit/project"),
            base_root: PathBuf::from("/explicit/base"),
            node_id: "explicit-node".to_string(),
        };
        let mut store = MockMesh::default();
        let _ = handle_synaptic_mesh_command(
            &SynapticMeshCommands::Init {
                name: "team".to_string(),
                namespace: vec![],
                remote: None,
            },
            &explicit,
            &mut store,
        );
        let _ = handle_synaptic_mesh_command(&SynapticMeshCommands::Leave, &explicit, &mut store);

        let (base, node, _, _, _) = &store.init_calls[0];
        assert_eq!(
            base,
            Path::new("/explicit/base"),
            "base_root comes from ctx"
        );
        assert_eq!(
            node, "explicit-node",
            "node_id comes from ctx, not $NEUROSTRATA_NODE_ID"
        );
        assert!(
            store
                .calls
                .contains(&"remove_pointer:/explicit/project".to_string()),
            "project_root comes from ctx: {:?}",
            store.calls
        );
    }

    #[test]
    fn mesh_up_and_join_token_roundtrip_and_explicit_zero_status() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx_a = MeshContext {
            project_root: tmp.path().join("proj-a"),
            base_root: tmp.path().join("base-a"),
            node_id: "node-a".into(),
        };
        let ctx_b = MeshContext {
            project_root: tmp.path().join("proj-b"),
            base_root: tmp.path().join("base-b"),
            node_id: "node-b".into(),
        };
        std::fs::create_dir_all(&ctx_a.project_root).unwrap();
        std::fs::create_dir_all(&ctx_b.project_root).unwrap();
        let mut fake = MockMesh {
            mesh_id: "swarm-1".into(),
            ..Default::default()
        };

        // Explicit-zero status before any transport is started
        let initial_peers =
            handle_synaptic_mesh_command(&SynapticMeshCommands::Peers, &ctx_b, &mut fake);
        assert!(initial_peers.contains("peers_connected: 0"));

        // Node A brings up mesh on an ephemeral local port
        let up_out = handle_synaptic_mesh_command(
            &SynapticMeshCommands::Up {
                name: "swarm".into(),
                bind: "127.0.0.1:0".into(),
            },
            &ctx_a,
            &mut fake,
        );
        assert!(
            up_out.contains("nsmesh1"),
            "mesh up must print an nsmesh1 invite token: {up_out}"
        );

        let token = up_out
            .split_whitespace()
            .find(|w| w.starts_with("nsmesh1"))
            .unwrap()
            .to_string();

        // Node B joins using Node A's invite token and connects to Node A's live listener
        let join_out = handle_synaptic_mesh_command(
            &SynapticMeshCommands::JoinToken { token },
            &ctx_b,
            &mut fake,
        );
        assert!(join_out.contains("joined neuromesh"), "got: {join_out}");

        let peers_b = handle_synaptic_mesh_command(&SynapticMeshCommands::Peers, &ctx_b, &mut fake);
        assert!(
            peers_b.contains("peers_connected: 1"),
            "Node B must report 1 connected peer: {peers_b}"
        );

        // Node A and Node B retained runtimes exchange frames end-to-end
        let node_a = get_active_mesh_node(&ctx_a.base_root).expect("Node A retained");
        let node_b = get_active_mesh_node(&ctx_b.base_root).expect("Node B retained");
        assert_eq!(node_a.mesh_id, "swarm-1");
        assert_eq!(node_a.epoch, 1);
        assert!(node_a.node_did.starts_with("did:key:"));
        assert!(node_b.outbox.max_seq_for(&node_b.node_did).unwrap() >= 1);
        run_on_mesh_runtime(async move {
            for _ in 0..20 {
                if node_a.reactor.transport_status().peers_connected >= 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(node_a.reactor.transport_status().peers_connected, 1);

            node_a
                .reactor
                .publish_payload(
                    "default",
                    vec![],
                    crate::synaptic_mesh::frame::MeshPayloadKind::TwinDelta {
                        twin_id: "cli-twin-1".into(),
                        version: 1,
                        source: "cli-test".into(),
                        rdf_patch: String::new(),
                        changed_variables: std::collections::BTreeMap::from([(
                            "temp".into(),
                            42.5,
                        )]),
                    },
                )
                .await
                .unwrap();

            for _ in 0..30 {
                if node_b.reactor.get_twin_state("cli-twin-1").is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let (ver, vars) = node_b
                .reactor
                .get_twin_state("cli-twin-1")
                .expect("Node B received TwinDelta from Node A");
            assert_eq!(ver, 1);
            assert_eq!(vars.get("temp"), Some(&42.5));
        });

        // Joining with a token pointing at an unreachable seed fails instead of reporting joined
        let (pkey, _, _) = crate::did::generate_keypair().unwrap();
        let dead_token = crate::synaptic_mesh::frame::InviteToken::issue(
            "dead-mesh",
            1,
            &[0x11u8; 32],
            vec!["127.0.0.1:1".into()],
            &pkey,
        )
        .unwrap()
        .encode()
        .unwrap();
        let dead_join = handle_synaptic_mesh_command(
            &SynapticMeshCommands::JoinToken { token: dead_token },
            &ctx_b,
            &mut fake,
        );
        assert!(
            dead_join.starts_with("error:"),
            "unreachable seed must fail join: {dead_join}"
        );
    }
}
