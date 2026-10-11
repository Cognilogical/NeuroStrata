//! The adapter that makes [`MeshStorage`] satisfy the [`StorageApi`] seam.
//!
//! Shape mismatch this file exists to absorb (checker-confirmed, slice 2a/2b
//! review): `MeshStorage` is built from associated functions returning
//! `anyhow::Result` while [`StorageApi`] is object-based, `&mut self`,
//! `String`-typed, and `String`-idiomed for mesh ids. Every `Err` here is
//! formatted with `format!("{:#}", e)` — anyhow's `to_string` drops the
//! source chain and would turn "manifest could not be read" into a message
//! without the OS error behind it.
//!
//! The ambient-resolver rule from the honest-schema review lives here too:
//! `project_root` and `node_id` arrive as arguments at the MCP call site and
//! as computed defaults at the CLI call site; nothing in the dispatch path
//! sniffs the environment by surprise.

use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use super::cli::{handle_synaptic_mesh_command, MeshContext, SynapticMeshCommands};
use super::storage::MeshStorage;
use super::storage_api::StorageApi;

/// The conventional mesh root: `~/.NeuroStrata/synaptic-mesh`
/// (spec §"Storage layout"). Computed at call sites — deliberately NOT a
/// method on `MeshContext`: an ambient-reading constructor on the context
/// struct invited handlers to reach for it (slice-2b checker landmine).
pub fn conventional_base_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".NeuroStrata")
        .join("synaptic-mesh")
}

/// Stable per-node identity: a UUID created on first run and persisted at
/// `<home>/.NeuroStrata/mesh-node-id` (spec §"Trust model": node_id ↔ git
/// author). Parameterized by home so tests never touch the real HOME.
pub fn resolve_node_id_at(home: &Path) -> String {
    let path = home.join(".NeuroStrata").join("mesh-node-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let fresh = Uuid::new_v4().to_string();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, format!("{}\n", fresh));
    fresh
}

/// The call-site wrapper: resolves the local home once, then delegates.
pub fn resolve_node_id() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    resolve_node_id_at(&home)
}

/// Does this checkout have a commit yet? A fresh `init` has no HEAD until
/// the first record lands, and `git push HEAD` on an unborn branch dies with
/// a refspec stack trace that answers nothing. Cheap probe, empty-state
/// courtesy only.
fn head_exists(root: &Path) -> bool {
    std::process::Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "rev-parse",
            "--verify",
            "-q",
            "HEAD",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Does the configured remote have refs yet? Three answers, not two:
/// `Some(false)` = reachable but EMPTY (bare repo nobody pushed to — a
/// normal "team inbox not yet filled" state); `Some(true)` = has refs;
/// `None` = could not ask (unreachable/auth) — and an unreachable remote
/// must fall through to the real fetch so the failure SURFACES, per the
/// spec's fail-soft rule ("an unreachable remote reports the failure").
fn remote_refs_present(root: &Path) -> Option<bool> {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            &root.to_string_lossy(),
            "ls-remote",
            "--refs",
            "origin",
            "HEAD",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!out.stdout.is_empty())
}

/// The mesh session for one project. `current` caches the opened repo across
/// calls within one process; a fresh process re-resolves through the project
/// pointer, which is what makes the MCP server (stateless per call) and a
/// one-shot CLI invocation behave the same.
pub struct MeshService {
    project_root: PathBuf,
    current: Option<MeshStorage>,
}

impl MeshService {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            current: None,
        }
    }

    /// The project-side pointer path (same layout `MeshStorage` writes).
    fn pointer(&self) -> PathBuf {
        self.project_root
            .join(".NeuroStrata")
            .join("synaptic-mesh-ref")
    }

    /// Resolve the joined mesh, opening it through the pointer when this
    /// process has not opened it yet. ABSENT pointer answers with the
    /// canonical no-mesh prefix (so `cli::render` collapses it); a CORRUPT
    /// pointer propagates its damage untouched — silence about a broken
    /// pointer is how the no-mesh path hides real failures (slice-2b
    /// checker finding).
    fn joined(&mut self) -> Result<&mut MeshStorage, String> {
        if self.current.is_none() {
            // Pointer access goes through the trait seam, not the concrete
            // static: the surface must have production users on both sides
            // of the dyn boundary or the seam is decorative.
            let project = self.project_root.clone();
            let pointer = StorageApi::read_pointer(self, &project)?;
            let Some((mesh_id, dir, _remote)) = pointer else {
                return Err(format!(
                    "no mesh joined — no pointer at {}",
                    self.pointer().display()
                ));
            };
            let storage = MeshStorage::open(&dir).map_err(|e| format!("{:#}", e))?;
            if storage.manifest().mesh_id.to_string() != mesh_id {
                return Err(format!(
                    "no mesh joined — pointer names mesh {} but {} holds {}",
                    mesh_id,
                    dir.display(),
                    storage.manifest().mesh_id
                ));
            }
            self.current = Some(storage);
        }
        Ok(self.current.as_mut().expect("just resolved"))
    }

    /// The one status report, shared by the trait method (which cannot take
    /// `&mut self` to populate the cache) and by nothing else.
    fn status_report(&self) -> Result<String, String> {
        let pointer = StorageApi::read_pointer(self, &self.project_root)?;
        let Some((mesh_id, dir, _remote)) = pointer else {
            return Err(format!(
                "no mesh joined — no pointer at {}",
                self.pointer().display()
            ));
        };
        let storage = MeshStorage::open(&dir).map_err(|e| format!("{:#}", e))?;
        let m = storage.manifest();
        if m.mesh_id.to_string() != mesh_id {
            return Err(format!(
                "no mesh joined — pointer names mesh {} but {} holds {}",
                mesh_id,
                dir.display(),
                m.mesh_id
            ));
        }
        Ok(format!(
            "mesh {} \"{}\" epoch {} namespaces {:?} remote {} root {}; outbox/heads/conflicts not wired until slices 3/4. DISCLOSURE: the mesh is a permanent record shared with the team — tombstones suppress content from search, but full erasure requires a coordinated history rewrite, and an uncooperative peer's clone retains the content.",
            m.mesh_id,
            m.mesh_name,
            m.epoch,
            m.namespaces,
            m.remote.as_deref().unwrap_or("<none configured>"),
            storage.root().display()
        ))
    }
}

impl StorageApi for MeshService {
    fn init_mesh(
        &mut self,
        base_root: &Path,
        node_id: &str,
        mesh_name: &str,
        namespaces: &[String],
        remote: Option<&str>,
    ) -> Result<String, String> {
        let storage = MeshStorage::init(base_root, node_id, mesh_name, namespaces, remote)
            .map_err(|e| format!("{:#}", e))?;
        let mesh_id = storage.manifest().mesh_id.to_string();
        let root = storage.root().to_path_buf();
        let project = self.project_root.clone();
        StorageApi::write_pointer(self, &project, &mesh_id, &root, remote)?;
        self.current = Some(storage);
        Ok(mesh_id)
    }

    fn open_mesh(&mut self, mesh_dir: &Path) -> Result<(), String> {
        let storage = MeshStorage::open(mesh_dir).map_err(|e| format!("{:#}", e))?;
        let (mesh_id, remote) = {
            let m = storage.manifest();
            (m.mesh_id.to_string(), m.remote.clone())
        };
        let project = self.project_root.clone();
        StorageApi::write_pointer(self, &project, &mesh_id, mesh_dir, remote.as_deref())?;
        self.current = Some(storage);
        Ok(())
    }

    fn write_pointer(
        &mut self,
        project: &Path,
        mesh_id: &str,
        mesh_dir: &Path,
        remote: Option<&str>,
    ) -> Result<(), String> {
        let parsed = Uuid::parse_str(mesh_id)
            .map_err(|e| format!("mesh id is not a uuid ({mesh_id}): {e}"))?;
        MeshStorage::write_pointer(project, parsed, mesh_dir, remote)
            .map_err(|e| format!("{:#}", e))
    }

    fn read_pointer(
        &self,
        project: &Path,
    ) -> Result<Option<(String, PathBuf, Option<String>)>, String> {
        MeshStorage::read_pointer(project)
            .map(|found| found.map(|(id, dir, remote)| (id.to_string(), dir, remote)))
            .map_err(|e| format!("{:#}", e))
    }

    fn remove_pointer(&mut self, project: &Path) -> Result<(), String> {
        MeshStorage::remove_pointer(project).map_err(|e| format!("{:#}", e))?;
        if project == self.project_root {
            self.current = None;
        }
        Ok(())
    }

    fn status(&self) -> Result<String, String> {
        self.status_report()
    }

    fn push(&mut self) -> Result<String, String> {
        let storage = self.joined()?;
        // Ordering matters: a mesh with no remote is a CONFIG error and must
        // surface as one even before its first commit. Only a configured
        // mesh gets the "nothing to publish yet" courtesy (a fresh init has
        // no commits until the mesh-worker lands a record — slice 3).
        let remote = storage.manifest().remote.clone();
        if remote.is_none() {
            return storage
                .push()
                .map(|()| "pushed".to_string())
                .map_err(|e| format!("{:#}", e));
        }
        if !head_exists(storage.root()) {
            return Ok(format!(
                "nothing to publish yet — mesh {} has no local commits",
                storage.manifest().mesh_id
            ));
        }
        storage.push().map_err(|e| format!("{:#}", e))?;
        Ok(format!(
            "pushed mesh {} to {}",
            storage.manifest().mesh_id,
            remote.unwrap_or_default()
        ))
    }

    fn pull(&mut self) -> Result<String, String> {
        let storage = self.joined()?;
        let mesh_id = storage.manifest().mesh_id;
        if storage.manifest().remote.is_some()
            && matches!(remote_refs_present(storage.root()), Some(false))
        {
            return Ok(format!(
                "pulled mesh {}: nothing on the remote yet — records land with the mesh-worker (slice 3)",
                mesh_id
            ));
        }
        storage.pull().map_err(|e| format!("{:#}", e))?;
        Ok(format!(
            "pulled mesh {} (fast-forward only; a divergence reports and preserves local work)",
            mesh_id
        ))
    }
}

/// Dispatch one MCP `tools/call` for the synaptic-mesh surface, production
/// base root (the conventional `~/.NeuroStrata/synaptic-mesh`).
pub fn handle_mesh_tool_call(name: &str, arguments: &Value) -> String {
    handle_mesh_tool_call_with_base(name, arguments, &conventional_base_root())
}

/// Test-seam variant: same dispatch, explicit base root. Tests pass a scratch
/// dir so a mesh never lands under the developer's real home.
pub fn handle_mesh_tool_call_with_base(name: &str, arguments: &Value, base_root: &Path) -> String {
    let project_root = arguments
        .get("project_root")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let node_id = arguments
        .get("node_id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(resolve_node_id);
    let ctx = MeshContext {
        project_root: project_root.clone(),
        base_root: base_root.to_path_buf(),
        node_id,
    };
    let mut svc = MeshService::new(project_root);
    let command = match name {
        "neurostrata_synaptic_mesh_init" => match arguments.get("name").and_then(|v| v.as_str()) {
            Some(mesh_name) => SynapticMeshCommands::Init {
                name: mesh_name.to_string(),
                namespace: arguments
                    .get("namespace")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                remote: arguments
                    .get("remote")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            },
            None => return "error: neurostrata_synaptic_mesh_init requires `name`".to_string(),
        },
        "neurostrata_synaptic_mesh_join" => match arguments.get("dir").and_then(|v| v.as_str()) {
            Some(dir) => SynapticMeshCommands::Join {
                dir: PathBuf::from(dir),
            },
            None => return "error: neurostrata_synaptic_mesh_join requires `dir`".to_string(),
        },
        "neurostrata_synaptic_mesh_push" => SynapticMeshCommands::Push,
        "neurostrata_synaptic_mesh_pull" => SynapticMeshCommands::Pull,
        "neurostrata_synaptic_mesh_status" => SynapticMeshCommands::Status,
        "neurostrata_synaptic_mesh_leave" => SynapticMeshCommands::Leave,
        other => return format!("error: unknown synaptic-mesh tool: {other}"),
    };
    handle_synaptic_mesh_command(&command, &ctx, &mut svc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ns-mesh-svc-{}-{}", label, Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn service_init_writes_pointer_and_status_reports_it() {
        let base = scratch("base");
        let project = scratch("proj");
        let mut svc = MeshService::new(project.clone());
        let id = svc
            .init_mesh(
                &base,
                "node-aaa",
                "platform-team",
                &["proj-ns".to_string()],
                None,
            )
            .expect("init");
        assert!(Uuid::parse_str(&id).is_ok(), "id is uuid, got {id}");
        assert!(project
            .join(".NeuroStrata")
            .join("synaptic-mesh-ref")
            .exists());
        let report = svc.status().expect("status after init");
        assert!(report.contains("platform-team"), "report: {report}");
        assert!(report.contains(&id), "report: {report}");
    }

    #[test]
    fn fresh_service_finds_mesh_through_pointer() {
        let base = scratch("base2");
        let project = scratch("proj2");
        let mut first = MeshService::new(project.clone());
        let id = first
            .init_mesh(&base, "node-b", "shared", &["ns".into()], None)
            .unwrap();
        drop(first);
        // New-process simulation: a fresh service with no cached handle.
        let mut second = MeshService::new(project);
        let report = second.status().expect("lazy resolve via pointer");
        assert!(report.contains(&id));
        // push without a remote must error honestly, not panic
        let err = second.push().expect_err("no remote configured");
        assert!(err.contains("no remote"), "err: {err}");
    }

    #[test]
    fn no_mesh_paths_collapse_to_the_canonical_reply() {
        let project = scratch("lonely");
        let mut svc = MeshService::new(project);
        let ctx = MeshContext {
            project_root: scratch("ctx-proj"),
            base_root: scratch("ctx-base"),
            node_id: "n".into(),
        };
        for cmd in [
            SynapticMeshCommands::Push,
            SynapticMeshCommands::Pull,
            SynapticMeshCommands::Status,
        ] {
            let reply = handle_synaptic_mesh_command(&cmd, &ctx, &mut svc);
            assert!(
                reply.starts_with("no mesh joined"),
                "{cmd:?} replied {reply:?} — render() must collapse the no-mesh prefix"
            );
        }
    }

    #[test]
    fn join_adopts_and_writes_the_pointer_in_one_call() {
        let base = scratch("jbase");
        let project_a = scratch("jproj-a");
        let project_b = scratch("jproj-b");
        let mut a = MeshService::new(project_a);
        let id = a
            .init_mesh(&base, "node-j", "adopted", &["ns".into()], None)
            .unwrap();
        let mesh_dir = base.join(&id);
        let mut b = MeshService::new(project_b.clone());
        b.open_mesh(&mesh_dir).expect("join");
        let read = b
            .read_pointer(&project_b)
            .unwrap()
            .expect("pointer written");
        assert_eq!(read.0, id);
        assert_eq!(read.1, mesh_dir);
        assert_eq!(read.2, None);
    }

    #[test]
    fn malformed_pointer_surfaces_the_source_chain() {
        let project = scratch("garbage");
        std::fs::create_dir_all(project.join(".NeuroStrata")).unwrap();
        std::fs::write(
            project.join(".NeuroStrata").join("synaptic-mesh-ref"),
            "junk-line",
        )
        .unwrap();
        let svc = MeshService::new(project);
        let err = svc
            .status()
            .expect_err("garbage pointer must not read as joined");
        assert!(
            err.contains("malformed"),
            "expected storage's malformed detail in the chain, got: {err}"
        );
        assert!(
            !err.starts_with("no mesh joined"),
            "a corrupt pointer is NOT 'no mesh joined' — it must render as an error, got: {err}"
        );
    }

    #[test]
    fn node_id_is_persistent_and_stable() {
        let home = scratch("home");
        let first = resolve_node_id_at(&home);
        let second = resolve_node_id_at(&home);
        assert_eq!(first, second, "stable across calls");
        assert!(Uuid::parse_str(&first).is_ok(), "it is a uuid");
        assert!(home.join(".NeuroStrata").join("mesh-node-id").exists());
    }

    #[test]
    fn tool_call_dispatch_round_trip_explicit_project_root() {
        let base = scratch("tbase");
        let project = scratch("tproj");
        let out = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_init",
            &json!({"name": "tool-mesh", "namespace": ["t"], "project_root": project, "node_id": "nt"}),
            &base,
        );
        assert!(out.contains("mesh created"), "init reply: {out}");
        let status = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_status",
            &json!({"project_root": project}),
            &base,
        );
        assert!(status.contains("tool-mesh"), "status reply: {status}");
        assert!(
            status.contains("permanent record"),
            "status must carry the spec:457 disclosure: {status}"
        );
        let leave = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_leave",
            &json!({"project_root": project}),
            &base,
        );
        assert!(leave.contains("left"), "leave reply: {leave}");
        assert!(!project
            .join(".NeuroStrata")
            .join("synaptic-mesh-ref")
            .exists());
    }

    #[test]
    fn tool_call_unknown_name_errors_loudly() {
        let out = handle_mesh_tool_call("neurostrata_synaptic_mesh_bogus", &json!({}));
        assert!(out.starts_with("error:"), "reply: {out}");
        assert!(out.contains("unknown"));
    }

    #[test]
    fn tool_call_push_pull_answer_the_fresh_mesh_honestly() {
        // init + remote configured, but no record committed yet (the
        // mesh-worker lands records in slice 3). git dies with refspec
        // stack traces on this state; the adapter must answer with the
        // empty-state truth instead.
        let remote_dir = scratch("remote");
        let bare = remote_dir.join("mesh.git");
        let out = std::process::Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&bare)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git init --bare");
        assert!(out.status.success());
        let base = scratch("dbase");
        let project = scratch("dproj");
        let init = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_init",
            &json!({"name":"div", "namespace":["d"], "remote": bare.display().to_string(), "project_root": project, "node_id":"dn"}),
            &base,
        );
        assert!(init.contains("mesh created"), "init reply: {init}");
        let push = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_push",
            &json!({"project_root": project}),
            &base,
        );
        assert!(
            push.contains("nothing to publish yet"),
            "fresh mesh push must answer the empty state, got: {push}"
        );
        let pull = handle_mesh_tool_call_with_base(
            "neurostrata_synaptic_mesh_pull",
            &json!({"project_root": project}),
            &base,
        );
        assert!(
            pull.contains("nothing on the remote yet"),
            "empty-remote pull must answer the empty state, got: {pull}"
        );
    }
}
