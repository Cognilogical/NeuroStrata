//! SynapticMesh acceptance evidence — the CROSS-NODE scenarios the module
//! unit tests do not already pin (spec §"Acceptance evidence", openai-arch
//! §5). Every test here starts real git: a bare remote, genuine clones, and
//! the actual `MeshStorage` push/pull plumbing — the same way two teammates'
//! daemons would meet.
//!
//! Test-only module: production code never links it (`#[cfg(test)]` in
//! `mod.rs`); it exists to DEMONSTRATE the spec's acceptance claims, not to
//! add surface.
//!
//! # Scenario coverage map
//!
//! | Spec scenario (§"Acceptance evidence") | Where it is pinned |
//! |---|---|
//! | 1. two offline nodes, disjoint adds converge, no conflicts | HERE (`two_offline_nodes_disjoint_adds_converge`) |
//! | 1. two offline nodes, same-id lifecycle conflicts — variants preserved, never silently dropped | HERE (`same_id_conflict_across_two_nodes_preserves_variants`) + unit-level merge table in `import.rs::{merge_table_row_*, conflict_is_never_silently_dropped}` |
//! | 2. lifecycle change crosses nodes within one sync cycle | HERE (`archived_on_a_gone_from_b_within_one_sync_cycle` — worker→path→commit→push→clone→pull→import→store) + single-node `import.rs::tombstone_never_resurrects_end_to_end`, `worker.rs::{an_archived_entry_records_the_lifecycle_and_tombstone, a_superseded_entry_records_the_lifecycle_and_successor}` |
//! | 3. malformed/forged inputs fail safely | ALREADY PINNED in `import.rs::traversal_symlink_executable_and_garbage_are_ignored_and_store_untouched` (traversal names, symlinks, exec bits, garbage JSON — store untouched), `import.rs::unknown_schema_version_is_ignored_and_named`, `storage.rs::open_refuses_unknown_schema_version` (manifest schema), `import.rs::retired_id_reactivation_is_refused` |
//! | 4. old-epoch offline peers cannot resurrect purged identities | HERE (`old_epoch_peer_records_refused_on_pull_and_stale_outbox_quarantined`) + import-side `import.rs::{epoch_older_refused_named, epoch_equal_applies, epoch_future_refused}` + publish-side `worker.rs::{a_stale_epoch_entry_is_quarantined_named_and_stays_pending, a_current_epoch_entry_drains_after_an_epoch_migration, a_mixed_batch_quarantines_stale_and_drains_current}` |
//! | 5. full-tree recovery preserves private rows + procedure consumption state | HERE (`full_tree_recovery_preserves_private_rows_and_procedure_state_in_one_pass`) + `import.rs::private_row_with_same_id_is_never_stomped`, `import.rs::reapply_advances_zero_and_crash_replay_never_duplicates`, `worker.rs::a_procedure_record_omits_the_runtime_counters` |
//! | 6. missing hooks / credential failure / dirty checkout / rejected push / daemon restart | ALREADY PINNED: `mesh_service.rs::{sync_enqueue_on_an_unreachable_remote_reports_soft_and_singly, status_pauses_on_a_dirty_checkout_and_names_the_paths, recover_*}`, `import.rs::dirty_checkout_refuses_full_tree_pass`, `worker.rs::an_unreachable_remote_does_not_stop_the_drain`, `storage.rs::push_without_remote_errors`, `worker.rs::{a_file_rename_fault_keeps_the_entry_pending_until_the_fault_clears, a_crash_between_rename_and_mark_committed_retries_idempotently, a_foreign_origin_row_is_skipped_never_re_exported}` |
//! | 7/8. year-scale data + latency thresholds | out of scope for this suite (no enforcement exists to test against; recorded durably in the spec) |
//!
//! # Former open gaps, now enforced
//!
//! Two gaps the map above used to carry are now closed:
//!
//! * **Old-epoch quarantine** — enforced on BOTH sides now. Import side
//!   (`import::epoch_gate`, F3): an old-epoch record is refused and named in
//!   the ignore report, never replayed into the new epoch. Publish side (F4):
//!   the mesh-worker compares each outbox entry's capture-time `epoch` against
//!   the manifest's current `epoch` and quarantines an older entry (skips the
//!   file write AND leaves it pending) rather than draining it into the new
//!   epoch.
//! * **Oversized JSON cap** — `MAX_RECORD_BYTES` (1 MiB) is enforced in
//!   `validate_record` BEFORE parse (F3); an oversized record is ignored with
//!   a named reason, never applied.
//!
//! In-file map purpose: so the NEXT acceptance pass knows exactly what is
//! new here versus cross-referenced, and treats any remaining gap as a gap —
//! not as a license to invent enforcement inside a test suite.

#[cfg(test)]
mod tests {
    use super::super::import::{apply_changed, contains_counter_key, cursor_file};
    use super::super::outbox::{outbox_dir_for, Outbox, OutboxEntry, OP_ARCHIVED, OP_CREATED};
    use super::super::storage::MeshStorage;
    use crate::store::LadybugStore;
    use crate::traits::{Embedder, MemoryPayload, VectorStore};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use uuid::Uuid;

    // ---- fixtures -----------------------------------------------------------

    const NS: &str = "probe";

    /// Fractional-offset stub (1.8.0 lesson): the per-element `(i + 1) * 0.001`
    /// base keeps the vector FLOAT[] for LadybugDB's ARRAY_DISTANCE binder,
    /// and the same per-text byte offsets keep equality assertions together.
    struct StubEmbedder;

    #[async_trait::async_trait]
    impl Embedder for StubEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v: Vec<f32> = (0..4).map(|i| (i as f32 + 1.0) * 0.001).collect();
            for (i, b) in text.bytes().enumerate() {
                v[i % 4] += b as f32 * 0.001;
            }
            Ok(v)
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    fn emb() -> Arc<dyn Embedder> {
        Arc::new(StubEmbedder)
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ns-mesh-acc-{tag}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    async fn row_count(store: &Arc<dyn VectorStore>, ns: &str) -> usize {
        store.init(ns).await.unwrap();
        store.list(ns, None, None).await.unwrap().len()
    }

    async fn fresh_store(label: &str) -> Arc<dyn VectorStore> {
        let db = std::env::temp_dir().join(format!(
            "ns-mesh-acc-db-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        Arc::new(LadybugStore::for_testing(&db, 4).expect("open temp store"))
    }

    /// One bare remote, ready to be pushed to. Callers mirror its HEAD onto
    /// the branch node A actually pushed (B's `git clone` then checks the
    /// right branch out).
    fn bare_remote(base: &Path, tag: &str) -> PathBuf {
        let remote = base.join(format!("remote-{tag}.git"));
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
        remote
    }

    /// Point the bare repo's default HEAD at the branch A actually pushed
    /// (git's init default HEAD may disagree with A's checkout branch).
    fn aim_remote_head(remote: &Path, branch: &str) {
        let out = Command::new("git")
            .args(["--git-dir"])
            .arg(remote)
            .args(["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn shareable_meta(source: &str) -> Value {
        json!({ "eligibility": "shareable", "source": source })
    }

    fn entry(op: &str, id: &str, content: &str, metadata: Value, memory_type: &str) -> OutboxEntry {
        OutboxEntry {
            stable_op_id: Uuid::new_v4(),
            op: op.to_string(),
            id: id.to_string(),
            namespace: NS.to_string(),
            memory_type: memory_type.to_string(),
            content: content.to_string(),
            metadata,
            superseded_by: None,
            ts: 1_700_000_000,
            epoch: 1,
        }
    }

    async fn apply_pass(
        mesh: &MeshStorage,
        store: &Arc<dyn VectorStore>,
    ) -> super::super::import::ApplyReport {
        apply_changed(mesh, &[NS.to_string()], store.clone(), emb())
            .await
            .unwrap()
    }

    /// `mesh init → bootstrap commit → push → clone into <base>/<tag>-b`
    /// (the two teammates' first handshake, exactly as storage.rs does it).
    fn joined_pair(base: &Path, tag: &str) -> (MeshStorage, MeshStorage) {
        let remote = bare_remote(base, tag);
        let a_dir = base.join(format!("{tag}-a"));
        let a = MeshStorage::init(
            &a_dir,
            "node-a",
            "team",
            &[NS.to_string()],
            Some(remote.to_str().unwrap()),
        )
        .unwrap();
        a.commit_all("bootstrap", "node-a").unwrap();
        a.push().unwrap();
        let branch = git_out(a.root(), &["branch", "--show-current"]);
        aim_remote_head(&remote, &branch);

        let b_dir = base.join(format!("{tag}-b"));
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
        (a, b)
    }

    /// Drain one WAL (drain_once itself authors the single commit), then
    /// push — the worker's publish cycle.
    fn publish(mesh: &MeshStorage, outbox: &Outbox, node: &str, pending: OutboxEntry) {
        outbox.append(&pending).unwrap();
        let report = super::super::worker::drain_once(outbox, mesh, node).unwrap();
        assert_eq!(report.applied, 1, "drain must publish: {report:?}");
        assert!(
            report.committed,
            "drain_once must author the commit itself: {report:?}"
        );
        mesh.push().unwrap();
    }

    // ---- scenario 1: two offline nodes, disjoint adds -----------------------

    /// Spec §"Acceptance evidence" 1. Both nodes queue disjoint adds in
    /// their WALs while offline. A drains + pushes first; B pulls
    /// (fast-forward — pull BEFORE its own drain commit keeps the history
    /// linear, which is what ff-only requires), drains, pushes; A pulls
    /// again. Both stores converge on BOTH records through real git with
    /// zero conflicts signalled.
    #[tokio::test]
    async fn two_offline_nodes_disjoint_adds_converge() {
        let base = scratch("acc-disjoint");
        let (a, b) = joined_pair(&base, "disjoint");
        let a_out = Outbox::open(&outbox_dir_for(a.root())).unwrap();
        let b_out = Outbox::open(&outbox_dir_for(b.root())).unwrap();
        let m1 = Uuid::new_v4();
        let m2 = Uuid::new_v4();

        // A publishes first, and applies its own row to its local store
        // (the daemon's view lives in the sameLadybug store the real
        // daemon would carry).
        let a_store = fresh_store("acc-disjoint-a").await;
        let first = entry(
            OP_CREATED,
            &m1.to_string(),
            "always use podman",
            shareable_meta("node-a 2026-10-11"),
            "rule",
        );
        publish(&a, &a_out, "node-a", first);
        let a0 = apply_pass(&a, &a_store).await;
        assert_eq!(a0.applied, 1, "A's own row applies locally: {a0:?}");

        // B comes online: pull before draining, then apply A's add.
        b.pull().unwrap();
        let b_store = fresh_store("acc-disjoint-b").await;
        let b1 = apply_pass(&b, &b_store).await;
        assert_eq!(b1.applied, 1, "A's add applies on B: {b1:?}");
        assert!(b1.full_tree, "B's first pass is full-tree");

        // B publishes its own add (disjoint from A's — different id). In
        // production the daemon wrote that row locally when the memory was
        // mutated; in this harness the import pass is the local writer, so
        // B applies its own row too.
        let second = entry(
            OP_CREATED,
            &m2.to_string(),
            "never use docker",
            shareable_meta("node-b 2026-10-11"),
            "rule",
        );
        publish(&b, &b_out, "node-b", second);
        let b2 = apply_pass(&b, &b_store).await;
        assert_eq!(b2.applied, 1, "B's own row applies locally: {b2:?}");

        // A pulls B's add back and applies it.
        a.pull().unwrap();
        let a2 = apply_pass(&a, &a_store).await;
        assert_eq!(a2.applied, 1, "B's add applies on A: {a2:?}");
        assert_eq!(
            row_count(&a_store, NS).await,
            2,
            "A's store converged on both records"
        );

        // Both stores hold both records with the original content. NOTE:
        // the worker publishes non-procedure content as a bare JSON string,
        // and `import.rs::content_text` stores its JSON serialization for
        // such records — the row content therefore reads `"\"…\""` with the
        // quotes intact. Documented wart (worker/import are sibling scope);
        // asserted here AS-STORED, never papered over.
        let quoted1 = json!("always use podman").to_string();
        let quoted2 = json!("never use docker").to_string();
        for (store, label) in [(&b_store, "B"), (&a_store, "A")] {
            let (_, row1) = store.get(NS, &m1.to_string()).await.unwrap().unwrap();
            let (_, row2) = store.get(NS, &m2.to_string()).await.unwrap().unwrap();
            assert_eq!(row1.content, quoted1, "{label}");
            assert_eq!(row2.content, quoted2, "{label}");
        }
        // And no conflicts were signalled anywhere.
        assert_eq!(
            super::super::conflict::load_overlay(a.root()).unresolved_count(),
            0,
            "no unresolved conflicts at A"
        );
        assert_eq!(
            super::super::conflict::load_overlay(b.root()).unresolved_count(),
            0,
            "no unresolved conflicts at B"
        );
    }

    // ---- scenario 1 (lifecycle table): same-id conflict over the wire -------

    /// Spec §"Acceptance evidence" 1/2 — the same-id conflict exercised
    /// WHERE it actually happens: B accepted A's record; B's import cursor
    /// is lost (crash); A pushes an incompatible edit; B's next pass is the
    /// full-tree reconciliation, which the design pins to `concurrent =
    /// true`, and the merge table hands back "incompatible content edits".
    /// The candidate variant MUST be preserved byte-parseably, the accepted
    /// row MUST keep A's earlier content (no silent drop, no overwrite),
    /// the overlay MUST name it, and the cursor MUST NOT advance until a
    /// team-side resolution lands.
    #[tokio::test]
    async fn same_id_conflict_across_two_nodes_preserves_variants() {
        let base = scratch("acc-conflict");
        let (a, b) = joined_pair(&base, "conflict");
        let a_out = Outbox::open(&outbox_dir_for(a.root())).unwrap();
        let id = Uuid::new_v4();

        // A publishes the original record.
        let first = entry(
            OP_CREATED,
            &id.to_string(),
            "version from A",
            shareable_meta("node-a 2026-10-11"),
            "rule",
        );
        let update_a1 = first.stable_op_id;
        publish(&a, &a_out, "node-a", first);

        // B accepts it.
        b.pull().unwrap();
        let b_store = fresh_store("acc-conflict-b").await;
        let b1 = apply_pass(&b, &b_store).await;
        assert_eq!(b1.applied, 1, "{b1:?}");

        // A pushes a DIFFERENT edit of the SAME memory (same id, new
        // update_id, different content) while B is effectively offline.
        let second = entry(
            OP_CREATED,
            &id.to_string(),
            "edited from A in the other direction",
            shareable_meta("node-a 2026-10-11, later"),
            "rule",
        );
        let update_a2 = second.stable_op_id;
        publish(&a, &a_out, "node-a", second);

        // B's crash: the accepted-head cursor is gone, so B's next replay is
        // the full-tree path — the designed concurrency trigger.
        let cursor = cursor_file(b.root());
        std::fs::remove_file(&cursor).unwrap();

        b.pull().unwrap();
        let b2 = apply_pass(&b, &b_store).await;
        assert!(b2.full_tree, "crash forces the full-tree pass");
        assert_eq!(b2.conflicted, 1, "{b2:?}");
        assert!(b2.conflicted_ids.contains(&id.to_string()), "{b2:?}");
        assert_eq!(b2.applied, 0, "the conflict is suppressed, not applied");

        // The accepted row still carries A's FIRST content, annotated (the
        // losing variant is never merged through the door it did not ask
        // for). Content is the JSON-serialized form of the bare-string
        // record content (import.rs::content_text fallback — documented
        // wart, sibling scope).
        let (_, row) = b_store.get(NS, &id.to_string()).await.unwrap().unwrap();
        assert_eq!(
            row.content,
            json!("version from A").to_string(),
            "accepted content untouched"
        );
        assert_eq!(
            row.metadata["mesh"]["update_id"],
            json!(update_a1.to_string()),
            "accepted update_id untouched"
        );
        assert_eq!(
            row.metadata["mesh"]["conflict"],
            json!(true),
            "the row carries the suppression marker"
        );

        // The candidate variant is preserved as a real file, byte-parseable,
        // with the SECOND update_id.
        let variant_path = super::super::conflict::conflicts_dir_for(b.root())
            .join(NS)
            .join(format!("{id}.json"));
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&variant_path).unwrap()).unwrap();
        assert_eq!(
            saved["update_id"],
            json!(update_a2.to_string()),
            "the preserved variant is A's later edit, exactly as committed"
        );

        // The overlay names it, status surfaces it, integration pauses.
        assert_eq!(
            super::super::conflict::load_overlay(b.root()).unresolved_count(),
            1
        );
        let lines = super::super::conflict::status_lines(b.root());
        assert!(
            lines.iter().any(|l| l.contains("1 unresolved")),
            "status must name the conflict: {lines:?}"
        );
        assert!(
            !cursor.exists(),
            "the cursor must NOT advance past an unresolved conflict"
        );
    }

    // ---- scenario 2: lifecycle change crosses nodes in one sync cycle -------

    /// Spec §"Acceptance evidence" 2 — the WHOLE publish side of a lifecycle
    /// change is the real one (worker drain → record path → git commit →
    /// push) and the whole receive side too (clone → pull → import replay →
    /// store row). Archived on A, gone from search-facing effective view on
    /// B after one sync cycle; and a stale resurrection pushed afterwards
    /// never re-serves the row at B.
    #[tokio::test]
    async fn archived_on_a_gone_from_b_within_one_sync_cycle() {
        let base = scratch("acc-archived");
        let (a, b) = joined_pair(&base, "archived");
        let a_out = Outbox::open(&outbox_dir_for(a.root())).unwrap();
        let id = Uuid::new_v4();
        let text = "the staged rollout rule";

        // A: create → push; B pulls and accepts.
        let created = entry(
            OP_CREATED,
            &id.to_string(),
            text,
            shareable_meta("node-a 2026-10-11"),
            "rule",
        );
        publish(&a, &a_out, "node-a", created);
        b.pull().unwrap();
        let b_store = fresh_store("acc-archived-b").await;
        assert_eq!(apply_pass(&b, &b_store).await.applied, 1);
        let (_, live) = b_store.get(NS, &id.to_string()).await.unwrap().unwrap();
        assert_eq!(
            live.content,
            json!(text).to_string(),
            "the record lives on B as shared (JSON-serialized bare-string content — documented wart)"
        );

        // The archive (worker-side: OP_ARCHIVED + `archived_at`, exactly as
        // the real capture writes it).
        let archived = entry(
            OP_ARCHIVED,
            &id.to_string(),
            text,
            json!({ "eligibility": "shareable", "archived_at": 1_700_000_050_i64 }),
            "rule",
        );
        publish(&a, &a_out, "node-a", archived);

        // B's pull + replay: within one sync cycle the archive crosses.
        b.pull().unwrap();
        let upd = apply_pass(&b, &b_store).await;
        assert_eq!(upd.applied, 1, "{upd:?}");
        let (_, gone) = b_store.get(NS, &id.to_string()).await.unwrap().unwrap();
        assert_eq!(
            gone.metadata["archived"],
            json!(true),
            "B's row reached the tombstone within one cycle"
        );
        assert!(
            gone.metadata["archived_at"].is_i64(),
            "archived_at crossed too: {:?}",
            gone.metadata
        );
        assert_eq!(
            gone.metadata["mesh"]["lifecycle_state"],
            json!("archived"),
            "provenance names the final state"
        );

        // A later OP_CREATED at the same id with CHANGED content is, by the
        // merge table, the `archive + edit` combination — the edited content
        // is kept but the record STAYS archived, never re-activated. (The
        // never-resurrect guarantee proper belongs to tombstones, which are
        // terminal — pinned by import.rs::merge_table_row_tombstone_always_
        // wins and import.rs::tombstone_never_resurrects_end_to_end; the
        // worker has no tombstone op, so the worker cycle can only lead the
        // record to Archived.) Assert the combination as designed.
        let revival = entry(
            OP_CREATED,
            &id.to_string(),
            "post-archive rewrite text",
            shareable_meta("node-a 2026-10-11, again"),
            "rule",
        );
        publish(&a, &a_out, "node-a", revival);
        b.pull().unwrap();
        let third = apply_pass(&b, &b_store).await;
        assert_eq!(
            third.applied, 1,
            "the combination applies and is reported: {third:?}"
        );
        assert!(
            third
                .combinations
                .iter()
                .any(|c| c.contains("archive + edit")),
            "the combination must be named in the report: {third:?}"
        );
        let (_, kept) = b_store.get(NS, &id.to_string()).await.unwrap().unwrap();
        assert_eq!(
            kept.metadata["archived"],
            json!(true),
            "still archived after the edit-crosses-nodes combination"
        );
        assert_eq!(
            kept.metadata["mesh"]["lifecycle_state"],
            json!("archived"),
            "never back to active"
        );
        assert_eq!(
            kept.content,
            json!("post-archive rewrite text").to_string(),
            "the edited content was kept — as designed, one combination for the whole cycle"
        );
    }

    // ---- scenario 5: full-tree recovery in one pass --------------------------

    /// Spec §"Acceptance evidence" 7 — full-tree recovery after a lost
    /// cursor must, in ONE pass whose diff covers every tracked file:
    /// apply the genuinely new record, skip the already-accepted procedure
    /// by its update_id WITHOUT disturbing its row (procedure consumption
    /// state is strictly local — no runtime counter key may even appear),
    /// and never stomp a private row.
    #[tokio::test]
    async fn full_tree_recovery_preserves_private_rows_and_procedure_state_in_one_pass() {
        let base = scratch("acc-recovery");
        let (a, b) = joined_pair(&base, "recovery");
        let a_out = Outbox::open(&outbox_dir_for(a.root())).unwrap();
        let rule_id = Uuid::new_v4();
        let proc_id = Uuid::new_v4();
        let private_id = Uuid::new_v4();

        // A publishes a rule AND a procedure definition (no counters travel).
        let rule = entry(
            OP_CREATED,
            &rule_id.to_string(),
            "check the manifest before pushing",
            shareable_meta("node-a 2026-10-11"),
            "rule",
        );
        publish(&a, &a_out, "node-a", rule);
        let proc = entry(
            OP_CREATED,
            &proc_id.to_string(),
            r#"{"trigger":"session-start","remaining_fires_initial":5,"valid_to":null,"rehearsal_script":"Read the mesh spec once."}"#,
            json!({
                "eligibility": "shareable",
                "trust": "untrusted",
                "trigger": "session-start",
                "remaining_fires_initial": 5,
                "valid_to": Value::Null,
                "rehearsal_script": "Read the mesh spec once."
            }),
            "procedure",
        );
        publish(&a, &a_out, "node-a", proc);

        // B accepts both, and holds one PRIVATE row (no provenance —
        // someone's local note).
        b.pull().unwrap();
        let b_store = fresh_store("acc-recovery-b").await;
        let first = apply_pass(&b, &b_store).await;
        assert_eq!(first.applied, 2, "both of A's records apply: {first:?}");
        b_store.init(NS).await.unwrap();
        b_store
            .upsert(
                NS,
                &private_id.to_string(),
                emb().embed("my private note").await.unwrap(),
                MemoryPayload {
                    content: "my private note".to_string(),
                    user_id: "me".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({ "source": "hand-written" }),
                },
            )
            .await
            .unwrap();
        assert_eq!(row_count(&b_store, NS).await, 3);

        // Crash: B loses the accepted-head cursor → the next pass is the
        // full-tree reconciliation against every tracked file.
        std::fs::remove_file(cursor_file(b.root())).unwrap();

        // Meanwhile A publishes a NEW record (a fresh rule) and pushes. An
        // in-place content edit under the full-tree path is per-design a
        // concurrent conflict (the merge table pins that; import.rs owns
        // those rows) — the recovery pass here proves the survival side,
        // not the conflict side.
        let fresh_id = Uuid::new_v4();
        let added = entry(
            OP_CREATED,
            &fresh_id.to_string(),
            "added while B was recovering",
            shareable_meta("node-a 2026-10-11, second"),
            "rule",
        );
        publish(&a, &a_out, "node-a", added);

        b.pull().unwrap();
        let recovery = apply_pass(&b, &b_store).await;
        assert!(recovery.full_tree, "crash → full-tree pass");
        assert_eq!(
            recovery.applied, 1,
            "the new record applies through the full-tree pass: {recovery:?}"
        );
        assert_eq!(
            recovery.skipped_update_id, 2,
            "rule AND procedure replay but skip by their own update_ids: {recovery:?}"
        );
        assert_eq!(row_count(&b_store, NS).await, 4, "no duplicates anywhere");
        let (_, fresh) = b_store
            .get(NS, &fresh_id.to_string())
            .await
            .unwrap()
            .expect("the new record landed");
        assert_eq!(
            fresh.content,
            json!("added while B was recovering").to_string()
        );

        // The accepted rule row: unchanged by recovery, still mesh-owned.
        let (_, rule_row) = b_store
            .get(NS, &rule_id.to_string())
            .await
            .unwrap()
            .expect("rule row still there");
        assert_eq!(
            rule_row.content,
            json!("check the manifest before pushing").to_string()
        );
        assert!(rule_row.metadata.get("mesh").is_some());

        // The private row: survived byte-for-byte, no provenance injected.
        let (_, private) = b_store
            .get(NS, &private_id.to_string())
            .await
            .unwrap()
            .expect("private row still there");
        assert_eq!(private.content, "my private note");
        assert!(
            private.metadata.get("mesh").is_none(),
            "no provenance was injected into a private row"
        );

        // The procedure row: untouched by recovery — same content, and its
        // metadata carries not one runtime counter key (consumption state is
        // strictly local, and recovery must not fake it or reset it).
        let (_, proc_row) = b_store
            .get(NS, &proc_id.to_string())
            .await
            .unwrap()
            .expect("procedure row still there");
        assert!(
            proc_row.content.contains("rehearsal_script"),
            "the definition stands: {:?}",
            proc_row.content
        );
        assert!(
            !contains_counter_key(&proc_row.metadata),
            "recovery must not introduce runtime counters: {:?}",
            proc_row.metadata
        );
    }

    // ---- scenario 4: old-epoch quarantine, both sides ------------------------

    /// Rewrite the on-disk manifest to a new `epoch`, commit it (the local
    /// half of an epoch migration), and reopen the mesh so its cached manifest
    /// reflects the new epoch. The manifest-edit trick this suite uses for
    /// epoch-bump fixtures (no real `filter-repo` history rewrite needed to
    /// exercise the gate).
    fn reopen_with_epoch(mesh: &MeshStorage, epoch: u32) -> MeshStorage {
        let mut manifest = mesh.manifest().clone();
        manifest.epoch = epoch;
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        std::fs::write(
            mesh.root().join(super::super::storage::MANIFEST_FILE_NAME),
            format!("{json}\n"),
        )
        .unwrap();
        mesh.commit_all("epoch migration", "node-a").unwrap();
        MeshStorage::open(mesh.root()).unwrap()
    }

    /// Spec §"Acceptance evidence" 4 — an old-epoch peer's records are refused
    /// on import (named, store untouched) AND its stale outbox entries are
    /// quarantined on publish. Two halves, one epoch migration:
    ///
    /// 1. B publishes record S at epoch 1 and pushes; A pulls it (ff).
    /// 2. B captures a second record T into its outbox at epoch 1 but does
    ///    NOT drain it (the WAL keeps it pending).
    /// 3. A migrates to epoch 2 (manifest edit + commit + push); B pulls the
    ///    migration (ff).
    /// 4. A's apply refuses S — born epoch 1, manifest epoch 2 — named in the
    ///    ignore report, A's store untouched.
    /// 5. B's drain quarantines T — born epoch 1, manifest epoch 2 — skipped,
    ///    named, and left pending (not consumed).
    #[tokio::test]
    async fn old_epoch_peer_records_refused_on_pull_and_stale_outbox_quarantined() {
        let base = scratch("acc-old-epoch");
        let (a, b) = joined_pair(&base, "old-epoch");
        let b_out = Outbox::open(&outbox_dir_for(b.root())).unwrap();
        let s_id = Uuid::new_v4();
        let t_id = Uuid::new_v4();

        // 1. B publishes S at epoch 1 and pushes; A pulls it.
        let s = entry(
            OP_CREATED,
            &s_id.to_string(),
            "pre-purge identity",
            shareable_meta("node-b 2026-10-11"),
            "rule",
        );
        publish(&b, &b_out, "node-b", s);
        a.pull().unwrap();

        // 2. B captures T at epoch 1 but leaves it pending (no drain).
        let t = entry(
            OP_CREATED,
            &t_id.to_string(),
            "stale outbox record",
            shareable_meta("node-b 2026-10-11"),
            "rule",
        );
        b_out.append(&t).unwrap();

        // 3. A migrates to epoch 2 (manifest edit + commit + push); B pulls.
        let a = reopen_with_epoch(&a, 2);
        a.push().unwrap();
        b.pull().unwrap();
        let b = MeshStorage::open(b.root()).unwrap(); // reflect the migrated manifest

        // 4. A's import refuses S (epoch 1 < epoch 2): named, store untouched.
        let a_store = fresh_store("acc-old-epoch-a").await;
        let report = apply_pass(&a, &a_store).await;
        assert_eq!(report.applied, 0, "old-epoch record refused: {report:?}");
        assert_eq!(row_count(&a_store, NS).await, 0, "A's store untouched");
        let joined = report.ignored.join(" | ");
        assert!(
            joined.contains("old-epoch"),
            "the refusal names the old epoch: {joined}"
        );
        assert!(
            joined.contains("origin_mesh_epoch 1") && joined.contains("manifest epoch 2"),
            "both epochs are named: {joined}"
        );

        // 5. B's drain quarantines T (epoch 1 < epoch 2): skipped, named, pending.
        let drain = super::super::worker::drain_once(&b_out, &b, "node-b").unwrap();
        assert_eq!(
            drain.applied, 0,
            "the stale entry must not publish: {drain:?}"
        );
        assert_eq!(drain.quarantined, 1, "one entry quarantined: {drain:?}");
        assert!(!drain.committed, "no commit for a quarantined entry");
        let line = &drain.quarantined_entries[0];
        assert!(
            line.contains("epoch 1") && line.contains("manifest epoch 2"),
            "the publish skip names both epochs: {line}"
        );
        assert!(
            line.contains(&t_id.to_string()),
            "the publish skip names the entry: {line}"
        );
        assert_eq!(
            b_out.pending().unwrap().len(),
            1,
            "the stale entry stays pending for operator inspection"
        );
    }
}
