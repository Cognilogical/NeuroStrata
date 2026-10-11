//! MeshStorage — the daemon-owned mesh checkout, one standalone git repo per
//! mesh at `~/.NeuroStrata/synaptic-mesh/<mesh_id>/` (see spec §"Storage
//! layout", Approach D). The project tree holds nothing but the tiny pointer
//! file `<project>/.NeuroStrata/synaptic-mesh-ref`.
//!
//! The mesh repo is a peer of the project repo, never nested in it: git's
//! content-addressable tree *is* the snapshot, `git log` *is* the event log,
//! and per-record files keep concurrent writers conflict-free.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use super::manifest::{MeshManifest, MeshPolicy, MESH_MANIFEST_SCHEMA_VERSION};

/// The manifest's name inside every mesh repo.
pub const MANIFEST_FILE_NAME: &str = "manifest.json";
/// Directory under the project root that holds the mesh pointer.
pub const POINTER_DIR_NAME: &str = ".NeuroStrata";
/// The pointer file itself: `<mesh_id>\t<mesh_dir>\t<remote or ->`.
pub const POINTER_FILE_NAME: &str = "synaptic-mesh-ref";

/// A handle to one mesh checkout: the directory, plus the manifest as read at
/// open time.
#[derive(Debug)]
pub struct MeshStorage {
    root: PathBuf,
    manifest: MeshManifest,
}

impl MeshStorage {
    /// Create a NEW mesh: `<base_root>/<new uuid v4>/`, `git init`, write
    /// `manifest.json` (schema `synaptic-mesh/v1`, epoch 1, default policy).
    /// `base_root` is normally `~/.NeuroStrata/synaptic-mesh`; tests pass a
    /// scratch dir. `remote` only records the URL and configures `origin` —
    /// no network happens here.
    pub fn init(
        base_root: &Path,
        node_id: &str,
        mesh_name: &str,
        namespaces: &[String],
        remote: Option<&str>,
    ) -> Result<MeshStorage> {
        let mesh_id = Uuid::new_v4();
        let root = base_root.join(mesh_id.to_string());
        std::fs::create_dir_all(&root).with_context(|| {
            format!(
                "mesh checkout dir could not be created at {}",
                root.display()
            )
        })?;

        run_ok(&root, &["init"])?;
        if let Some(url) = remote {
            run_ok(&root, &["remote", "add", "origin", url])?;
        }

        let manifest = MeshManifest {
            schema_version: MESH_MANIFEST_SCHEMA_VERSION.to_string(),
            mesh_id,
            mesh_name: mesh_name.to_string(),
            created_at: chrono::Utc::now(),
            created_by: node_id.to_string(),
            epoch: 1,
            namespaces: namespaces.to_vec(),
            policy: MeshPolicy::default(),
            remote: remote.map(str::to_string),
        };
        write_manifest(&root, &manifest)?;

        Ok(MeshStorage { root, manifest })
    }

    /// Open an EXISTING mesh checkout. Refuses any manifest whose
    /// `schema_version` is not `synaptic-mesh/v1`, naming the offending
    /// version, so an old daemon cannot silently misread a newer mesh.
    pub fn open(mesh_dir: &Path) -> Result<MeshStorage> {
        let path = mesh_dir.join(MANIFEST_FILE_NAME);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("mesh manifest could not be read at {}", path.display()))?;
        let manifest: MeshManifest = serde_json::from_str(&text).with_context(|| {
            format!(
                "mesh manifest at {} is not valid {}",
                path.display(),
                MESH_MANIFEST_SCHEMA_VERSION
            )
        })?;
        if manifest.schema_version != MESH_MANIFEST_SCHEMA_VERSION {
            bail!(
                "unsupported mesh manifest schema_version {:?} at {} (expected {})",
                manifest.schema_version,
                path.display(),
                MESH_MANIFEST_SCHEMA_VERSION
            );
        }

        Ok(MeshStorage {
            root: mesh_dir.to_path_buf(),
            manifest,
        })
    }

    /// The project-side pointer: `<project>/.NeuroStrata/synaptic-mesh-ref`,
    /// one line, `<mesh_id>\t<mesh_dir>\t<remote or ->`. Created by `join`,
    /// deleted by `leave`.
    pub fn write_pointer(
        project: &Path,
        mesh_id: Uuid,
        mesh_dir: &Path,
        remote: Option<&str>,
    ) -> Result<()> {
        let path = pointer_path(project);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("pointer dir could not be created at {}", parent.display())
            })?;
        }
        let line = format!(
            "{}\t{}\t{}\n",
            mesh_id,
            mesh_dir.display(),
            remote.unwrap_or("-")
        );
        std::fs::write(&path, line)
            .with_context(|| format!("mesh pointer could not be written to {}", path.display()))?;
        Ok(())
    }

    /// Read the project pointer. `Ok(None)` when the project has no mesh.
    pub fn read_pointer(project: &Path) -> Result<Option<(Uuid, PathBuf, Option<String>)>> {
        let path = pointer_path(project);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("mesh pointer could not be read at {}", path.display())
                })
            }
        };

        let fields: Vec<&str> = text.trim_end().split('\t').collect();
        if fields.len() != 3 {
            bail!(
                "mesh pointer at {} is malformed: expected `<mesh_id>\\t<mesh_dir>\\t<remote or ->`, got {:?}",
                path.display(),
                text
            );
        }
        let mesh_id = Uuid::parse_str(fields[0]).with_context(|| {
            format!(
                "mesh pointer at {} names an invalid mesh id {:?}",
                path.display(),
                fields[0]
            )
        })?;
        let mesh_dir = PathBuf::from(fields[1]);
        // The wire format writes `-` for "no remote" (a bare tab-separated
        // empty field would be ambiguous with a typo).
        let remote = match fields[2] {
            "-" | "" => None,
            url => Some(url.to_string()),
        };
        Ok(Some((mesh_id, mesh_dir, remote)))
    }

    /// `leave()`: drop the project's link to the mesh. Idempotent — a project
    /// with no pointer is already left.
    pub fn remove_pointer(project: &Path) -> Result<()> {
        let path = pointer_path(project);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| {
                format!("mesh pointer could not be removed at {}", path.display())
            }),
        }
    }

    pub fn manifest(&self) -> &MeshManifest {
        &self.manifest
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stage + commit everything in the mesh repo as `node_id`. Returns
    /// `Ok(false)` on a no-op (nothing changed) — an empty commit is a bug
    /// in the caller, never a fallback.
    pub fn commit_all(&self, message: &str, node_id: &str) -> Result<bool> {
        run_ok(&self.root, &["add", "-A"])?;
        let staged = run(&self.root, &["status", "--porcelain"])?;
        if String::from_utf8_lossy(&staged.stdout).trim().is_empty() {
            return Ok(false);
        }

        let user_name = format!("user.name={}", node_id);
        let user_email = format!("user.email={}@synaptic-mesh.local", node_id);
        let args = [
            "-c",
            user_name.as_str(),
            "-c",
            user_email.as_str(),
            "commit",
            "-m",
            message,
        ];
        run_ok(&self.root, &args)?;
        Ok(true)
    }

    /// `git fetch` + fast-forward merge from the configured remote. Err when
    /// no remote is set, or when history has genuinely diverged (operator
    /// resolves; never force-overwrite).
    pub fn pull(&self) -> Result<()> {
        self.require_remote()?;
        // `origin HEAD` = the remote's default branch, written as the single
        // line of FETCH_HEAD (a bare `git fetch origin` lists every branch and
        // the first line is not necessarily the one to merge).
        run_ok(&self.root, &["fetch", "origin", "HEAD"])?;
        // NOT bare `FETCH_HEAD`: git special-cases that literal name and skips
        // FETCH_HEAD lines marked `not-for-merge` (which is what a fetch that
        // also updated a remote-tracking ref writes), silently reporting
        // "Already up to date" without merging. `FETCH_HEAD^{commit}` peels to
        // the commit and bypasses the special case. --ff-only: divergent
        // history is an operator decision, never a silent merge commit.
        run_ok(&self.root, &["merge", "--ff-only", "FETCH_HEAD^{commit}"])?;
        Ok(())
    }

    /// `git push` the current checkout to the configured remote. Err when no
    /// remote is set.
    pub fn push(&self) -> Result<()> {
        self.require_remote()?;
        run_ok(&self.root, &["push", "origin", "HEAD"])?;
        Ok(())
    }

    fn require_remote(&self) -> Result<()> {
        match &self.manifest.remote {
            Some(url) if !url.trim().is_empty() => Ok(()),
            _ => bail!(
                "mesh {} has no remote configured; join a mesh with a remote before pull/push",
                self.manifest.mesh_id
            ),
        }
    }
}

fn pointer_path(project: &Path) -> PathBuf {
    project.join(POINTER_DIR_NAME).join(POINTER_FILE_NAME)
}

fn write_manifest(root: &Path, manifest: &MeshManifest) -> Result<()> {
    let path = root.join(MANIFEST_FILE_NAME);
    // Pretty JSON, one field per line, matching the spec's record format and
    // keeping `git diff` on the manifest reviewable.
    let json =
        serde_json::to_string_pretty(manifest).context("manifest could not be serialized")?;
    std::fs::write(&path, format!("{}\n", json))
        .with_context(|| format!("manifest could not be written to {}", path.display()))?;
    Ok(())
}

/// Run one git subcommand non-interactively (`GIT_TERMINAL_PROMPT=0`: a
/// credential prompt would wedge the daemon) inside the mesh checkout.
fn run(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| format!("git {} could not be executed", args.join(" ")))
}

fn run_ok(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    let out = run(root, args)?;
    if out.status.success() {
        return Ok(out);
    }
    bail!(
        "git {} failed ({}): {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Scratch dir keyed by pid + tag + uuid: parallel tests never collide,
    /// and a leftover from a crashed run is cleared first.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-mesh-storage-{}-{}-{}",
            std::process::id(),
            tag,
            Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn init_under(base: &Path) -> MeshStorage {
        MeshStorage::init(
            base,
            "node-abc123",
            "stripe-api-team",
            &["stripe-api".to_string()],
            None,
        )
        .unwrap()
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn init_creates_dir_repo_and_manifest() {
        let base = scratch("init");
        let mesh = init_under(&base);

        let root = mesh.root();
        assert!(root.is_dir(), "mesh dir must exist at {}", root.display());
        assert!(root.join(".git").is_dir(), "`git init` must create .git");
        assert!(
            root.join(MANIFEST_FILE_NAME).is_file(),
            "manifest.json must be written at init"
        );

        let on_disk: MeshManifest =
            serde_json::from_str(&std::fs::read_to_string(root.join(MANIFEST_FILE_NAME)).unwrap())
                .expect("manifest.json parses as MeshManifest");
        assert_eq!(on_disk.schema_version, MESH_MANIFEST_SCHEMA_VERSION);
        assert_eq!(on_disk.mesh_name, "stripe-api-team");
        assert_eq!(on_disk.namespaces, vec!["stripe-api".to_string()]);
        assert_eq!(on_disk.epoch, 1);
        assert_eq!(on_disk.created_by, "node-abc123");
        assert_eq!(on_disk.policy, MeshPolicy::default());
        assert!(on_disk.remote.is_none());

        // The checkout dir is keyed by the manifest's mesh id (uuid v4).
        let dir_id = root.file_name().unwrap().to_str().unwrap();
        assert_eq!(dir_id, on_disk.mesh_id.to_string());
        let parsed = Uuid::parse_str(dir_id).expect("mesh dir name is a uuid");
        assert_eq!(parsed.get_version(), Some(uuid::Version::Random));
    }

    #[test]
    fn manifest_round_trips_to_disk() {
        let base = scratch("roundtrip");
        let mesh = init_under(&base);
        let original = mesh.manifest().clone();

        let reopened = MeshStorage::open(mesh.root()).expect("open freshly inited mesh");
        assert_eq!(reopened.manifest(), &original);
        assert_eq!(reopened.root(), mesh.root());
    }

    #[test]
    fn open_refuses_unknown_schema_version() {
        let base = scratch("schema-mismatch");
        let mesh = init_under(&base);
        let path = mesh.root().join(MANIFEST_FILE_NAME);

        let mut doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["schema_version"] = serde_json::json!("synaptic-mesh/v99");
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let err = MeshStorage::open(mesh.root())
            .expect_err("a future schema_version must be refused")
            .to_string();
        assert!(
            err.contains("synaptic-mesh/v99"),
            "error must name the offending version, got: {err}"
        );
    }

    #[test]
    fn pointer_round_trips() {
        let project = scratch("pointer");
        let mesh_dir = PathBuf::from("/opt/neurostrata/synaptic-mesh/abc");

        assert!(
            MeshStorage::read_pointer(&project).unwrap().is_none(),
            "a project with no pointer file is not in a mesh"
        );

        let id = Uuid::new_v4();
        let url = "https://example.com/team/mesh.git";
        MeshStorage::write_pointer(&project, id, &mesh_dir, Some(url)).unwrap();
        assert_eq!(
            MeshStorage::read_pointer(&project).unwrap(),
            Some((id, mesh_dir.clone(), Some(url.to_string())))
        );

        // A mesh with no remote writes `-`, and reads back as None.
        let id2 = Uuid::new_v4();
        MeshStorage::write_pointer(&project, id2, &mesh_dir, None).unwrap();
        assert_eq!(
            MeshStorage::read_pointer(&project).unwrap(),
            Some((id2, mesh_dir.clone(), None))
        );

        MeshStorage::remove_pointer(&project).unwrap();
        assert!(MeshStorage::read_pointer(&project).unwrap().is_none());
        // leave() is idempotent.
        MeshStorage::remove_pointer(&project).unwrap();
    }

    #[test]
    fn commit_all_records_a_commit_and_author() {
        let base = scratch("commit");
        let mesh = init_under(&base);

        // The slice-3 mesh-worker's shape: one file per record.
        let memories = mesh.root().join("stripe-api").join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(memories.join("m1.json"), r#"{"id":"m1"}"#).unwrap();

        assert!(
            mesh.commit_all("mesh slice 2a: first record", "node-author-1")
                .unwrap(),
            "a dirty tree must commit"
        );
        assert_eq!(
            git_out(mesh.root(), &["log", "--format=%an"]),
            "node-author-1"
        );
        assert_eq!(git_out(mesh.root(), &["rev-list", "--count", "HEAD"]), "1");

        assert!(
            !mesh.commit_all("nothing changed", "node-author-1").unwrap(),
            "a clean tree is Ok(false), never an empty commit"
        );
    }

    #[test]
    fn push_and_pull_between_two_checkouts() {
        let base = scratch("sync");
        let remote = base.join("remote.git");
        let out = Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let a_dir = base.join("checkout-a");
        let a = MeshStorage::init(
            &a_dir,
            "node-a",
            "stripe-api-team",
            &["stripe-api".to_string()],
            Some(remote.to_str().unwrap()),
        )
        .unwrap();
        assert!(a.commit_all("bootstrap", "node-a").unwrap());
        a.push().unwrap();

        // Point the bare repo's HEAD at the branch A actually pushed, so the
        // clone checks it out (git init's default HEAD may not match).
        let branch = git_out(a.root(), &["branch", "--show-current"]);
        let out = Command::new("git")
            .args(["--git-dir"])
            .arg(&remote)
            .args([
                "symbolic-ref",
                "HEAD",
                format!("refs/heads/{}", branch).as_str(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let b_dir = base.join("checkout-b");
        let out = Command::new("git")
            .arg("clone")
            .arg(&remote)
            .arg(&b_dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let b = MeshStorage::open(&b_dir).unwrap();
        assert_eq!(b.manifest().mesh_id, a.manifest().mesh_id);
        assert_eq!(b.manifest(), a.manifest(), "clone must carry the manifest");

        let memories = b.root().join("stripe-api").join("memories");
        std::fs::create_dir_all(&memories).unwrap();
        std::fs::write(memories.join("m2.json"), r#"{"id":"m2"}"#).unwrap();
        assert!(b.commit_all("node-b record", "node-b").unwrap());
        b.push().unwrap();
        assert_eq!(git_out(b.root(), &["log", "--format=%an", "-1"]), "node-b");

        a.pull().unwrap();
        assert!(
            a.root()
                .join("stripe-api")
                .join("memories")
                .join("m2.json")
                .is_file(),
            "pull must fast-forward the record in"
        );
    }

    #[test]
    fn push_without_remote_errors() {
        let base = scratch("no-remote");
        let mesh = init_under(&base);
        assert!(mesh.manifest().remote.is_none());

        let pull_err = mesh.pull().expect_err("pull with no remote must error");
        assert!(
            pull_err.to_string().contains("no remote"),
            "got: {pull_err}"
        );
        let push_err = mesh.push().expect_err("push with no remote must error");
        assert!(
            push_err.to_string().contains("no remote"),
            "got: {push_err}"
        );
    }
}
