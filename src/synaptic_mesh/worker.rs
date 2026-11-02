//! The mesh-worker — drains the durable outbox into the mesh checkout.
//!
//! See `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`
//! §"Durable mutation → mesh handoff". The worker is the *only* thing that
//! writes mesh files or commits them; the subscriber only appends outbox
//! entries, and this module never touches the network (no push, no pull — a
//! configured-but-unreachable remote is irrelevant to a drain).
//!
//! [`drain_once`] is a **pure, synchronous** function: it reads pending outbox
//! entries in stable order, builds a [`MeshRecord`] for each, atomically
//! replaces `memories/<id>.json` / `procedures/<id>.json`, commits once per
//! drain, then marks the entries committed. Everything it needs is in the
//! outbox entry or the mesh checkout — it never reads the LadybugDB store,
//! which is why it can run without the daemon's async runtime (and why the
//! in-file tests can drive it directly).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use super::manifest::Trust;
use super::outbox::{
    is_shareable_memory_type, Outbox, OutboxEntry, OP_ARCHIVED, OP_CREATED, OP_SUPERSEDED,
};
use super::record::{
    Lifecycle, LifecycleState, MeshRecord, Relationship, RdfBlock, MESH_RECORD_SCHEMA_VERSION,
};
use crate::rdf::build_mesh_canonical_nquads;
use super::storage::MeshStorage;

/// What one drain did. Fielded so the daemon's loop can log it and the tests
/// can assert on it without parsing a sentence.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DrainReport {
    /// Entries read from the WAL this drain.
    pub pending_seen: usize,
    /// Records whose file this drain wrote (or replaced).
    pub applied: usize,
    /// Entries consumed without writing a file: foreign-origin, a
    /// non-shareable type, not eligible under policy, an unknown op, or a
    /// non-UUID id.
    pub skipped: usize,
    /// Entries skipped because their capture-time epoch predates the mesh
    /// manifest's CURRENT epoch — a pre-purge entry re-presenting into a new
    /// epoch. Unlike `skipped` (consumed as a decision), these stay PENDING:
    /// the worker neither writes their file nor marks them committed, so the
    /// operator can inspect the old outbox (spec §"Deletion & erasure").
    pub quarantined: usize,
    /// One flattened line per quarantined entry, naming the entry's epoch and
    /// the current manifest epoch — the operator-facing surface.
    pub quarantined_entries: Vec<String>,
    /// Entries whose target file already carried the same `update_id` — an
    /// idempotent retry after a crash between the file rename and the
    /// mark-committed step. Not a write, not a skip, and never a second commit.
    pub already_applied: usize,
    /// Whether `commit_all` recorded a new commit this drain.
    pub committed: bool,
}

/// The outcome of applying one entry. Skip reasons are a decision, never an
/// error; I/O failures are an error and abort the drain so the entries stay
/// pending for the next attempt. A quarantine carries the flattened reason
/// line and, unlike a skip, leaves the entry pending.
enum ApplyOutcome {
    Applied,
    AlreadyApplied,
    Skipped,
    Quarantined(String),
}

/// Drain the outbox once. One batched `git commit` per call, authored as
/// `node_id`, message `"synaptic-mesh: N record(s)"`.
///
/// An I/O error mid-batch aborts *before* the commit and before any
/// mark-committed, so nothing is half-acknowledged: the files already written
/// are idempotently re-applied on the next drain.
pub fn drain_once(
    outbox: &Outbox,
    mesh: &MeshStorage,
    node_id: &str,
) -> Result<DrainReport, String> {
    let entries = outbox.pending()?;
    let mut report = DrainReport {
        pending_seen: entries.len(),
        ..DrainReport::default()
    };
    if entries.is_empty() {
        return Ok(report);
    }

    let policy = mesh.manifest().policy.clone();
    let epoch = mesh.manifest().epoch;
    let mut committed_ids: Vec<Uuid> = Vec::with_capacity(entries.len());
    // The MeshRecords this drain actually wrote (Applied only) — the
    // compressed-bundle opt-in (NEUROSTRATA_MESH_BUNDLE_ZSTD) serializes
    // exactly these.
    let mut committed_records: Vec<MeshRecord> = Vec::with_capacity(entries.len());

    for entry in &entries {
        let (outcome, written) = match apply_one(entry, mesh, node_id, &policy, epoch) {
            Ok(pair) => pair,
            Err(e) => return Err(e),
        };
        match outcome {
            ApplyOutcome::Applied => {
                report.applied += 1;
                committed_ids.push(entry.stable_op_id);
                if let Some(record) = written {
                    committed_records.push(record);
                }
            }
            ApplyOutcome::AlreadyApplied => {
                report.already_applied += 1;
                committed_ids.push(entry.stable_op_id);
            }
            ApplyOutcome::Skipped => {
                report.skipped += 1;
                committed_ids.push(entry.stable_op_id);
            }
            // A quarantine is NOT a consume: the entry stays pending (never
            // pushed onto `committed_ids`) so the operator inspects it rather
            // than the drain silently dropping it on every tick.
            ApplyOutcome::Quarantined(line) => {
                report.quarantined += 1;
                report.quarantined_entries.push(line);
            }
        }
    }

    // ONE commit per drain, not per record. Author = node_id (the same node
    // id the manifest's created_by carries).
    if report.applied > 0 || report.already_applied > 0 {
        let n = report.applied + report.already_applied;
        let message = format!("synaptic-mesh: {} record(s)", n);
        match mesh.commit_all(&message, node_id) {
            Ok(did_commit) => report.committed = did_commit,
            Err(e) => return Err(format!("mesh commit failed: {e}")),
        }
    }

    // Compressed-bundle opt-in (spec: NEUROSTRATA_MESH_BUNDLE_ZSTD): the
    // drained records also land in `<root>/bundles/drain-<ts>.bundle.zst`.
    // Best-effort — a bundle write failure never fails the drain.
    if !committed_records.is_empty()
        && std::env::var("NEUROSTRATA_MESH_BUNDLE_ZSTD")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    {
        if let Err(e) = write_compressed_bundle(
            &mesh.root().join("bundles"),
            &format!("drain-{}", Utc::now().timestamp_millis()),
            &committed_records,
        ) {
            tracing::warn!("SynapticMesh compressed bundle write failed: {e}");
        }
    }

    // Mark committed AFTER the commit: a crash before this line leaves the
    // entries pending, and the worker's idempotent apply makes the retry a
    // no-op (AlreadyApplied), never a duplicate commit.
    outbox.mark_committed(&committed_ids)?;

    Ok(report)
}

/// Apply one entry: classify (skip) or build + atomically write the record.
/// On success returns the written record (only for `Applied` — a written
/// file); the record is `None` for skip/already-applied/quarantine
/// outcomes and is consumed by the caller's bundle tracking.
fn apply_one(
    entry: &OutboxEntry,
    mesh: &MeshStorage,
    node_id: &str,
    policy: &super::manifest::MeshPolicy,
    epoch: u32,
) -> Result<(ApplyOutcome, Option<MeshRecord>), String> {
    // ---- publish-side epoch quarantine (spec §"Deletion & erasure") ----
    // A record captured in an older epoch than the mesh's current one is from
    // a pre-purge outbox and must not be replayed into the new epoch. This is
    // the publish-side twin of `import::epoch_gate`: the entry is skipped AND
    // left pending (quarantined) rather than consumed, so the operator can
    // inspect the old outbox. It is checked FIRST — an old-epoch entry is
    // quarantined whatever its other classifications would say.
    if entry.epoch < epoch {
        return Ok((
            ApplyOutcome::Quarantined(format!(
                "{}: old-epoch entry (epoch {} < manifest epoch {}); quarantined, not replayed",
                entry.id, entry.epoch, epoch
            )),
            None,
        ));
    }
    // ---- classification: a skip is a decision, never an error ----
    // Feedback guard: a row that already carries a `mesh` provenance marker was
    // imported FROM the mesh (slice 4 writes it) and must never be re-exported.
    if entry.metadata.get("mesh").is_some() {
        return Ok((ApplyOutcome::Skipped, None));
    }
    // Unknown op: defensive — an op this worker doesn't recognise is skipped,
    // never exported as if it were a fresh `created`.
    if !matches!(entry.op.as_str(), OP_CREATED | OP_SUPERSEDED | OP_ARCHIVED) {
        return Ok((ApplyOutcome::Skipped, None));
    }
    // Defensive re-check of the type filter the subscriber already applied.
    if !is_shareable_memory_type(&entry.memory_type) {
        return Ok((ApplyOutcome::Skipped, None));
    }
    // Sharing boundary: when the policy requires it (default), only
    // `eligibility: shareable` records are published. Absent or `private`
    // reads as private.
    if policy.require_record_eligibility && !is_eligible(&entry.metadata) {
        return Ok((ApplyOutcome::Skipped, None));
    }
    // ReBAC mesh boundary: restricted memories (`visible_to` without `*` or
    // matching mesh peer principal) stay local and are never published.
    let peer_principals: Vec<String> = mesh
        .manifest()
        .signers
        .iter()
        .map(|s| s.did.clone())
        .collect();
    if !crate::rebac::can_publish_to_mesh(&entry.metadata, &peer_principals) {
        return Ok((ApplyOutcome::Skipped, None));
    }
    let id = match Uuid::parse_str(&entry.id) {
        Ok(id) => id,
        Err(_) => return Ok((ApplyOutcome::Skipped, None)),
    };
    let superseded_by = match entry.op.as_str() {
        OP_SUPERSEDED => match entry
            .superseded_by
            .as_deref()
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            Some(target) => Some(target),
            None => return Ok((ApplyOutcome::Skipped, None)),
        },
        _ => None,
    };

    // ---- build + write ----
    let mut record = build_record(entry, id, superseded_by, node_id, epoch);

    // Auto-sign if a local Ed25519 signing key exists.
    let signing_key_path = std::env::var("NEUROSTRATA_MESH_SIGNING_KEY")
        .ok()
        .filter(|p| std::path::Path::new(p).exists())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| mesh.root().join(".neurostrata-mesh").join("signing.key"));
    if signing_key_path.exists() {
        // A signing failure with the key present is a real fault (corrupt
        // or unreadable key); committing an unsigned record silently would
        // downgrade the mesh's trust surface, so fail the drain.
        if let Err(e) = sign_record(&mut record, &signing_key_path) {
            return Err(format!("SynapticMesh record signing failed: {e}"));
        }
    }

    let path = record_path(mesh, &record);

    if let Some(existing) = load_existing(&path)? {
        if existing.update_id == record.update_id {
            // Byte-identical apply already landed (crash between rename and
            // mark-committed). Idempotent: nothing to write, nothing to commit.
            return Ok((ApplyOutcome::AlreadyApplied, None));
        }
        // A lifecycle transition (superseded/archived) re-exports the SAME
        // record: preserve the origin fields that must not drift with each
        // revision.
        record.lifecycle.created_at = existing.lifecycle.created_at;
        record.lifecycle.created_by = existing.lifecycle.created_by.clone();
        record.origin_mesh_epoch = existing.origin_mesh_epoch;

        // Rebuild the canonical RDF block from the post-mutation record so
        // the exported quads reflect the new lifecycle state (spec §5.2c).
        // The existing block is the source of truth for every other field.
        if let Some(ref existing_rdf) = existing.rdf {
            if let Some(new_rdf) = rebuild_rdf_block(&record, existing_rdf) {
                record.rdf = Some(new_rdf);
            }
        }
    }

    // Oversized-record size cap: refuse records larger than 1 MiB to keep
    // mesh files bounded and reviewable.
    let serialized = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
    if serialized.len() > 1_048_576 {
        return Err(format!("mesh record exceeds 1 MiB size cap ({} bytes)", serialized.len()));
    }
    write_record(&path, &record)?;

    // RDF patch sidecar (spec: NEUROSTRATA_MESH_RDF_PATCH opt-in): rebuild a
    // Stream from the record's canonical N-Quads block and write its patch
    // serialization next to the record file. Fail-soft.
    if let Some(ref rdf_block) = record.rdf {
        if std::env::var("NEUROSTRATA_MESH_RDF_PATCH").is_ok() {
            if let Ok(stream) =
                crate::streaming::Stream::from_nquads(&rdf_block.quads, crate::streaming::PatchOp::Add)
            {
                if !stream.is_empty() {
                    let patch_path = path.with_extension("rdfpatch");
                    match std::fs::write(&patch_path, stream.to_patch()) {
                        Ok(()) => tracing::debug!(
                            "SynapticMesh RDF patch ({} quads) written to {}",
                            stream.len(),
                            patch_path.display()
                        ),
                        Err(e) => tracing::warn!(
                            "SynapticMesh RDF patch write to {} failed: {e}",
                            patch_path.display()
                        ),
                    }
                }
            }
        }
    }

    Ok((ApplyOutcome::Applied, Some(record)))
}

/// The record's eligibility gate: only a literal `eligibility: "shareable"`.
fn is_eligible(metadata: &Value) -> bool {
    metadata.get("eligibility").and_then(|v| v.as_str()) == Some("shareable")
}

/// Build the mesh record for one entry. Pure — everything comes from the entry
/// or the passed-in epoch/node id.
fn build_record(
    entry: &OutboxEntry,
    id: Uuid,
    superseded_by: Option<Uuid>,
    node_id: &str,
    epoch: u32,
) -> MeshRecord {
    let (state, archived_at) = match entry.op.as_str() {
        OP_SUPERSEDED => (LifecycleState::Superseded, None),
        OP_ARCHIVED => (LifecycleState::Archived, archived_at(entry)),
        _ => (LifecycleState::Active, None),
    };

    let content = if entry.memory_type == "procedure" {
        procedure_definition(entry)
    } else {
        Value::String(entry.content.clone())
    };

    let mut relationships: Vec<Relationship> = Vec::new();
    if let Some(supersedes) = entry.metadata.get("supersedes").and_then(|v| v.as_str()) {
        if let Ok(target) = Uuid::parse_str(supersedes) {
            relationships.push(Relationship {
                kind: "supersedes".to_string(),
                target,
            });
        }
    }

    // Build canonical RDF block (Phase 1 dual-write: always emit canonical payload)
    let created_at_str = created_at(entry).to_rfc3339();
    let archived_at_str = archived_at.as_ref().map(|dt| dt.to_rfc3339());
    // Tombstone: not set on creation; an archive operation IS the tombstoning
    // event, so an archived record carries the archive timestamp forward as
    // its tombstone time (the publish-time update stamp).
    let tombstoned_at = if matches!(state, LifecycleState::Archived) {
        archived_at.clone()
    } else {
        None
    };
    let tombstoned_at_str = tombstoned_at.as_ref().map(|dt| dt.to_rfc3339());
    let superseded_by_str = superseded_by.as_ref().map(|u| u.to_string());
    let update_id_str = entry.stable_op_id.to_string();
    let trust_val = trust_from(&entry.metadata).to_string();

    // Extract curated metadata
    let eligibility = entry.metadata.get("eligibility").and_then(|v| v.as_str());
    let source = entry.metadata.get("source").and_then(|v| v.as_str());
    let tags: Vec<String> = entry.metadata.get("tags").and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let concept_iri = entry.metadata.get("concept_iri").and_then(|v| v.as_str());

    // Extract procedure definition fields (owned Strings so they outlive any
    // further borrow; the def is dropped at the end of its if-block).
    let (trigger, initial_fires, valid_to, rehearsal_script) = if entry.memory_type == "procedure" {
        let def = procedure_definition(entry);
        let trig = def.get("trigger").and_then(|v| v.as_str()).map(String::from);
        let fires = def.get("remaining_fires_initial").and_then(|v| v.as_i64());
        let vt = def.get("valid_to").and_then(|v| v.as_str()).map(String::from);
        let rs = def.get("rehearsal_script").and_then(|v| v.as_str()).map(String::from);
        (trig, fires, vt, rs)
    } else {
        (None, None, None, None)
    };

    // Build relationships vec for RDF
    let rdf_relationships: Vec<(String, String)> = relationships.iter()
        .map(|r| (r.kind.clone(), r.target.to_string()))
        .collect();

    // Content string for RDF (stringified JSON for procedure, plain text for memory)
    let content_str = match &content {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(&content).unwrap_or_default(),
    };

    let (nquads_text, digest_str) = build_mesh_canonical_nquads(
        &id.to_string(),
        &entry.namespace,
        &entry.memory_type,
        &format!("{:?}", state).to_lowercase(),
        &created_at_str,
        &node_id,
        superseded_by_str.as_deref(),
        archived_at_str.as_deref(),
        tombstoned_at_str.as_deref(),
        &content_str,
        eligibility,
        source,
        &tags,
        concept_iri,
        &rdf_relationships,
        &update_id_str,
        epoch,
        &trust_val,
        trigger.as_deref(),
        initial_fires,
        valid_to.as_deref(),
        rehearsal_script.as_deref(),
    );

    let patch_op = if matches!(state, LifecycleState::Archived | LifecycleState::Tombstoned) {
        crate::streaming::PatchOp::Delete
    } else {
        crate::streaming::PatchOp::Add
    };
    let patch = match crate::streaming::Stream::from_nquads(&nquads_text, patch_op) {
        Ok(stream) if !stream.is_empty() => Some(stream),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("SynapticMesh RDF patch build failed for update {update_id_str}: {e}");
            None
        }
    };

    MeshRecord {
        schema_version: MESH_RECORD_SCHEMA_VERSION.to_string(),
        id,
        namespace: entry.namespace.clone(),
        memory_type: entry.memory_type.clone(),
        lifecycle: Lifecycle {
            state,
            created_at: created_at(entry),
            created_by: node_id.to_string(),
            superseded_by,
            archived_at,
            tombstoned_at,
        },
        content,
        metadata: projected_metadata(&entry.metadata),
        rdf: Some(RdfBlock {
            schema: "neurostrata-rdf-record/v1".to_string(),
            format: "n-quads".to_string(),
            canonical_hash: digest_str,
            quads: nquads_text,
        }),
        relationships,
        // The record's revision id IS the outbox entry's idempotency key, so
        // re-applying an entry produces byte-identical output.
        update_id: entry.stable_op_id,
        origin_mesh_epoch: epoch,
        trust: trust_from(&entry.metadata),
        signature: None,
        signer_did: None,
        public_key: None,
        patch,
    }
}

/// Sign a [`MeshRecord`] in place using the Ed25519 keypair at `key_path`.
pub fn sign_record(record: &mut MeshRecord, key_path: &Path) -> Result<(), String> {
    let (pkey, did, pubkey) = crate::did::load_or_generate_keypair(key_path)?;
    record.signer_did = Some(did);
    record.public_key = Some(hex::encode(&pubkey));
    record.signature = None;
    let payload = if let Some(ref rdf_block) = record.rdf {
        crate::did::signing_payload(
            &rdf_block.quads,
            &record.update_id.to_string(),
            record.origin_mesh_epoch,
        )
    } else {
        let val = serde_json::to_value(&*record).map_err(|e| e.to_string())?;
        crate::did::canonical_signing_bytes(&val)?
    };
    let sig = crate::did::sign(&payload, &pkey)?;
    record.signature = Some(crate::did::encode_base64(&sig));
    Ok(())
}

/// Compress a batch of mesh records with zstd for bundle transport.
/// Used by `write_compressed_bundle` under the drain-time
/// `NEUROSTRATA_MESH_BUNDLE_ZSTD` opt-in and the bundle round-trip tests.
pub fn compress_record_bundle(records: &[MeshRecord], level: i32) -> Result<Vec<u8>, String> {
    let json_bytes = serde_json::to_vec(records).map_err(|e| e.to_string())?;
    crate::compression::zstd::compress_mesh_bundle_with_level(&json_bytes, level)
        .map_err(|e| e.to_string())
}

/// Write a compressed bundle to `<bundles_dir>/<bundle_name>.bundle.zst`.
/// `bundles_dir` is the target directory directly (the mesh worker passes
/// `<mesh_root>/bundles`).
pub fn write_compressed_bundle(
    bundles_dir: &Path,
    bundle_name: &str,
    records: &[MeshRecord],
) -> Result<PathBuf, String> {
    let bytes = compress_record_bundle(records, 3)?;
    std::fs::create_dir_all(bundles_dir).map_err(|e| e.to_string())?;
    let path = bundles_dir.join(format!("{}.bundle.zst", bundle_name));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Rebuild the canonical RDF block from a MeshRecord after lifecycle mutation.
/// Uses the existing block only to preserve content/relationships (all come
/// from the record itself); the lifecycle fields are taken from the record.
fn rebuild_rdf_block(record: &MeshRecord, existing: &RdfBlock) -> Option<RdfBlock> {
    let id_str = record.id.to_string();
    let namespace = &record.namespace;
    let memory_type = &record.memory_type;
    let state_str = format!("{:?}", record.lifecycle.state).to_lowercase();
    let created_at_str = record.lifecycle.created_at.to_rfc3339();
    let created_by = &record.lifecycle.created_by;
    let superseded_by_str = record.lifecycle.superseded_by.as_ref().map(|u| u.to_string());
    let archived_at_str = record.lifecycle.archived_at.as_ref().map(|dt| dt.to_rfc3339());
    // Tombstone the update stamp for an archive: the archive op IS the
    // tombstoning event (the update timestamp), and an already-present
    // tombstone wins.
    let tombstoned_at = if record.lifecycle.state == LifecycleState::Archived {
        record
            .lifecycle
            .archived_at
            .clone()
            .or_else(|| record.lifecycle.tombstoned_at.clone())
    } else {
        record.lifecycle.tombstoned_at.clone()
    };
    let tombstoned_at_str = tombstoned_at.as_ref().map(|dt| dt.to_rfc3339());
    let content_str = match &record.content {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(&record.content).unwrap_or_default(),
    };
    let eligibility = record.metadata.get("eligibility").and_then(|v| v.as_str());
    let source = record.metadata.get("source").and_then(|v| v.as_str());
    let tags: Vec<String> = record.metadata.get("tags").and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let concept_iri = record.metadata.get("concept_iri").and_then(|v| v.as_str());
    let rdf_relationships: Vec<(String, String)> = record.relationships.iter()
        .map(|r| (r.kind.clone(), r.target.to_string()))
        .collect();
    let update_id_str = record.update_id.to_string();
    let epoch = record.origin_mesh_epoch;
    let trust_val = format!("{:?}", record.trust).to_lowercase();
    // Procedure fields: read from content definition if procedure
    let (trigger, initial_fires, valid_to, rehearsal_script) = if record.memory_type == "procedure" {
        if let Some(def) = record.content.as_object() {
            let trig = def.get("trigger").and_then(|v| v.as_str()).map(String::from);
            let fires = def.get("remaining_fires_initial").and_then(|v| v.as_i64());
            let vt = def.get("valid_to").and_then(|v| v.as_str()).map(String::from);
            let rs = def.get("rehearsal_script").and_then(|v| v.as_str()).map(String::from);
            (trig, fires, vt, rs)
        } else { (None, None, None, None) }
    } else { (None, None, None, None) };

    let (nquads_text, digest_str) = build_mesh_canonical_nquads(
        &id_str, namespace, memory_type, &state_str,
        &created_at_str, created_by,
        superseded_by_str.as_deref(),
        archived_at_str.as_deref(), tombstoned_at_str.as_deref(),
        &content_str, eligibility, source, &tags, concept_iri,
        &rdf_relationships, &update_id_str, epoch, &trust_val,
        trigger.as_deref(), initial_fires, valid_to.as_deref(), rehearsal_script.as_deref(),
    );
    Some(RdfBlock {
        // The rebuilt block retains the existing block's schema/format
        // identifiers — a rebuilt block is the same projection, refreshed.
        schema: existing.schema.clone(),
        format: existing.format.clone(),
        canonical_hash: digest_str,
        quads: nquads_text,
    })
}

/// The record's `metadata` object: the spec's curated subset. Eligibility is
/// always present (defaulting to `private`); `source` and `tags` are carried
/// through when the memory declares them. Everything else — runtime counters,
/// structural pointers, access counts — stays local.
fn projected_metadata(metadata: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let eligibility = metadata
        .get("eligibility")
        .and_then(|v| v.as_str())
        .unwrap_or("private");
    out.insert("eligibility".to_string(), json!(eligibility));
    if let Some(source) = metadata.get("source") {
        out.insert("source".to_string(), source.clone());
    }
    if let Some(tags) = metadata.get("tags") {
        out.insert("tags".to_string(), tags.clone());
    }
    // concept_iri is shareable (alignment across namespaces); domain/aliases stay private.
    if let Some(concept_iri) = metadata.get("concept_iri") {
        out.insert("concept_iri".to_string(), concept_iri.clone());
    }
    Value::Object(out)
}

/// The procedure record's `content` is the DEFINITION, never the runtime
/// state. The node's current `remaining_fires` is exported as the definition's
/// `remaining_fires_initial` — the budget an importing node starts fresh at.
/// Runtime counters (`remaining_fires`, `performance_count`,
/// `last_performed_at`, `last_episodic_pointer`) are NEVER written.
fn procedure_definition(entry: &OutboxEntry) -> Value {
    json!({
        "trigger": entry.metadata.get("trigger").cloned().unwrap_or(Value::Null),
        "remaining_fires_initial": entry.metadata.get("remaining_fires").cloned().unwrap_or(Value::Null),
        "valid_to": entry.metadata.get("valid_to").cloned().unwrap_or(Value::Null),
        "rehearsal_script": entry.content,
    })
}

/// The trust field for an exported record. Foreign-origin rows are skipped, so
/// every published record is locally-authored (`trusted`); an explicit
/// `metadata.trust` is honoured if present.
fn trust_from(metadata: &Value) -> Trust {
    match metadata.get("trust").and_then(|v| v.as_str()) {
        Some("untrusted") => Trust::Untrusted,
        Some("promoted") => Trust::Promoted,
        _ => Trust::Trusted,
    }
}

fn created_at(entry: &OutboxEntry) -> DateTime<Utc> {
    DateTime::from_timestamp(entry.ts, 0).unwrap_or_else(Utc::now)
}

/// The archive timestamp: the tombstone the archive handler wrote, falling
/// back to the dispatcher timestamp when it is absent.
fn archived_at(entry: &OutboxEntry) -> Option<DateTime<Utc>> {
    entry
        .metadata
        .get("archived_at")
        .and_then(|v| v.as_i64())
        .and_then(|ts| DateTime::from_timestamp(ts, 0))
        .or_else(|| DateTime::from_timestamp(entry.ts, 0))
}

/// The on-disk path for a record: `<root>/<namespace>/memories/<id>.json`, or
/// `procedures/<id>.json` for procedures.
fn record_path(mesh: &MeshStorage, record: &MeshRecord) -> PathBuf {
    let subdir = if record.memory_type == "procedure" {
        "procedures"
    } else {
        "memories"
    };
    mesh.root()
        .join(&record.namespace)
        .join(subdir)
        .join(format!("{}.json", record.id))
}

/// Atomically write a record: pretty JSON + newline to a tmp file, then
/// rename over the target. `git diff`-friendly and crash-safe.
fn write_record(path: &Path, record: &MeshRecord) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("record path {} has no parent", path.display()))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("record dir {} could not be created: {e}", dir.display()))?;
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| format!("record could not be serialised: {e}"))?;
    let tmp = dir.join(format!(".{}.tmp", record.id));
    std::fs::write(&tmp, format!("{json}\n"))
        .map_err(|e| format!("record tmp {} could not be written: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("record rename to {} failed: {e}", path.display()))?;
    Ok(())
}

/// Read + parse the existing record file, if any. `Ok(None)` for a missing or
/// corrupt file — a corrupt file is overwritten by the fresh write (recovery),
/// never left to block the drain forever.
fn load_existing(path: &Path) -> Result<Option<MeshRecord>, String> {
    // A directory where a record file belongs is not an error here: treat it
    // as absent so the apply proceeds and the tmp->rename below fails loudly
    // (the fault-injection test leans on exactly that ordering) instead of
    // blocking the drain at the read stage.
    if path.is_dir() {
        return Ok(None);
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("record {} could not be read: {e}", path.display())),
    };
    match serde_json::from_str(&text) {
        Ok(record) => Ok(Some(record)),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synaptic_mesh::outbox::outbox_dir_for;
    use std::process::Command;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-mesh-worker-{}-{}-{}",
            std::process::id(),
            tag,
            Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fresh mesh checkout + its outbox, under one scratch base.
    fn fixture(tag: &str, remote: Option<&str>) -> (MeshStorage, Outbox) {
        let base = scratch(tag);
        let mesh =
            MeshStorage::init(&base, "node-fixture", "team", &["ns".to_string()], remote).unwrap();
        let outbox = Outbox::open(&outbox_dir_for(mesh.root())).unwrap();
        (mesh, outbox)
    }

    fn git_count(root: &Path) -> usize {
        let out = Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git rev-list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// `git_count` for the fault test: counts every commit on all refs, `0`
    /// when the checkout has no commit yet (a file-step failure MUST leave
    /// the checkout exactly where the fixture started, possibly with no HEAD).
    fn git_count_or_zero(root: &Path) -> usize {
        let out = Command::new("git")
            .args(["rev-list", "--count", "--all"])
            .current_dir(root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git rev-list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// A `created` entry for a shareable `rule`.
    fn created(id: &str, metadata: serde_json::Value) -> OutboxEntry {
        OutboxEntry {
            stable_op_id: Uuid::now_v7(),
            op: OP_CREATED.to_string(),
            id: id.to_string(),
            namespace: "ns".to_string(),
            memory_type: "rule".to_string(),
            content: "the memory text".to_string(),
            metadata,
            superseded_by: None,
            ts: 1_700_000_000,
            epoch: 1,
        }
    }

    fn shareable_metadata() -> serde_json::Value {
        json!({ "eligibility": "shareable", "source": "owner 2026-10-11" })
    }

    /// Rewrite the on-disk manifest to a new `epoch`, commit it (the local
    /// half of an epoch migration), and reopen the mesh so its cached manifest
    /// reflects the new epoch. Mirrors `import.rs`'s test helper.
    fn reopen_with_epoch(mesh: &MeshStorage, epoch: u32) -> MeshStorage {
        let mut manifest = mesh.manifest().clone();
        manifest.epoch = epoch;
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        std::fs::write(
            mesh.root().join(super::super::storage::MANIFEST_FILE_NAME),
            format!("{json}\n"),
        )
        .unwrap();
        mesh.commit_all("epoch migration", "node-fixture").unwrap();
        MeshStorage::open(mesh.root()).unwrap()
    }

    #[test]
    fn a_created_mutation_writes_a_record_that_round_trips() {
        let (mesh, outbox) = fixture("roundtrip", None);
        let id = Uuid::now_v7();
        let entry = created(&id.to_string(), shareable_metadata());
        outbox.append(&entry).unwrap();

        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.skipped, 0);
        assert!(report.committed);

        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path).expect("the record file must exist");
        let record: MeshRecord =
            serde_json::from_str(&text).expect("the file parses as MeshRecord");
        assert_eq!(record.schema_version, MESH_RECORD_SCHEMA_VERSION);
        assert_eq!(record.id, id);
        assert_eq!(record.namespace, "ns");
        assert_eq!(record.memory_type, "rule");
        assert_eq!(record.lifecycle.state, LifecycleState::Active);
        assert_eq!(record.lifecycle.created_by, "node-author");
        assert_eq!(record.content, json!("the memory text"));
        assert_eq!(record.metadata["eligibility"], json!("shareable"));
        assert_eq!(record.metadata["source"], json!("owner 2026-10-11"));
        assert_eq!(record.update_id, entry.stable_op_id);
        assert_eq!(record.origin_mesh_epoch, 1);
        assert_eq!(record.trust, Trust::Trusted);
    }

    #[test]
    fn a_second_drain_is_a_no_op() {
        let (mesh, outbox) = fixture("second-drain", None);
        let id = Uuid::now_v7();
        outbox
            .append(&created(&id.to_string(), shareable_metadata()))
            .unwrap();

        drain_once(&outbox, &mesh, "node-author").unwrap();
        let second = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(second.pending_seen, 0);
        assert_eq!(second.applied, 0);
        assert!(!second.committed, "nothing to commit on an empty drain");
        assert_eq!(git_count(mesh.root()), 1, "still exactly one commit");
    }

    #[test]
    fn a_pending_entry_applies_on_the_next_drain_after_a_crash() {
        let (mesh, outbox) = fixture("crash-before-drain", None);
        let id = Uuid::now_v7();
        outbox
            .append(&created(&id.to_string(), shareable_metadata()))
            .unwrap();

        // Simulate "crash before the drain ever ran": nothing drained, and the
        // mesh file must not exist yet.
        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        assert!(!path.exists(), "the mesh has not exported yet");

        drain_once(&outbox, &mesh, "node-author").unwrap();
        assert!(
            path.exists(),
            "the next drain applies the pending entry — nothing lost"
        );
    }

    #[test]
    fn a_crash_between_rename_and_mark_committed_retries_idempotently() {
        let (mesh, outbox) = fixture("crash-rename-commit", None);
        let id = Uuid::now_v7();
        let entry = created(&id.to_string(), shareable_metadata());
        outbox.append(&entry).unwrap();

        drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(git_count(mesh.root()), 1);

        // Simulate the crash state: the file + commit landed, but the WAL was
        // never compacted (mark-committed did not run), so the entry is still
        // pending. Re-appending the same entry reconstructs that state.
        outbox.append(&entry).unwrap();
        let retry = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(retry.applied, 0);
        assert_eq!(
            retry.already_applied, 1,
            "the retry recognises the same update_id"
        );
        assert!(
            !retry.committed,
            "byte-identical output must not be a second commit"
        );
        assert_eq!(git_count(mesh.root()), 1, "no duplicate commit");
    }

    #[test]
    fn a_batch_produces_one_git_commit() {
        let (mesh, outbox) = fixture("batch", None);
        for _ in 0..3 {
            outbox
                .append(&created(&Uuid::now_v7().to_string(), shareable_metadata()))
                .unwrap();
        }
        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(report.applied, 3);
        assert!(report.committed);
        assert_eq!(git_count(mesh.root()), 1, "three records in one commit");

        let out = Command::new("git")
            .args(["log", "--format=%s"])
            .current_dir(mesh.root())
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("synaptic-mesh: 3 record(s)"));
    }

    #[test]
    fn a_foreign_origin_row_is_skipped_never_re_exported() {
        let (mesh, outbox) = fixture("foreign", None);
        let id = Uuid::now_v7();
        let mut metadata = shareable_metadata();
        metadata["mesh"] = json!({ "mesh_id": "imported-from-peer" });
        outbox.append(&created(&id.to_string(), metadata)).unwrap();

        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(report.skipped, 1);
        assert_eq!(report.applied, 0);
        assert!(!report.committed);

        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        assert!(!path.exists(), "the imported row must not be re-exported");
        assert_eq!(
            outbox.pending().unwrap().len(),
            0,
            "the skip is consumed, not retried"
        );
    }

    #[test]
    fn a_procedure_record_omits_the_runtime_counters() {
        let (mesh, outbox) = fixture("procedure", None);
        let id = Uuid::now_v7();
        let entry = OutboxEntry {
            stable_op_id: Uuid::now_v7(),
            op: OP_CREATED.to_string(),
            id: id.to_string(),
            namespace: "ns".to_string(),
            memory_type: "procedure".to_string(),
            content: "when editing, run the formatter".to_string(),
            metadata: json!({
                "eligibility": "shareable",
                "trigger": "before-edit",
                "remaining_fires": 5,
                "valid_to": 1_800_000_000_i64,
                "performance_count": 2,
                "last_performed_at": 1_700_000_000_i64,
            }),
            superseded_by: None,
            ts: 1_700_000_000,
            epoch: 1,
        };
        outbox.append(&entry).unwrap();

        drain_once(&outbox, &mesh, "node-author").unwrap();
        let path = mesh
            .root()
            .join("ns")
            .join("procedures")
            .join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path).expect("the definition file exists");
        let record: MeshRecord = serde_json::from_str(&text).unwrap();

        assert_eq!(record.memory_type, "procedure");
        let def = record.content;
        assert_eq!(def["trigger"], json!("before-edit"));
        assert_eq!(
            def["remaining_fires_initial"],
            json!(5),
            "the initial budget is the definition's"
        );
        assert_eq!(def["valid_to"], json!(1_800_000_000_i64));
        assert_eq!(
            def["rehearsal_script"],
            json!("when editing, run the formatter")
        );

        // The runtime counters never leave the node — in the definition object
        // OR anywhere in the serialised record.
        assert!(def.get("remaining_fires").is_none());
        assert!(def.get("performance_count").is_none());
        assert!(def.get("last_performed_at").is_none());
        assert!(def.get("last_episodic_pointer").is_none());
        assert!(
            !text.contains("performance_count"),
            "counters must not leak: {text}"
        );
        assert!(
            !text.contains("last_performed_at"),
            "counters must not leak: {text}"
        );
    }

    #[test]
    fn an_unreachable_remote_does_not_stop_the_drain() {
        let (mesh, outbox) = fixture(
            "unreachable",
            Some("https://unreachable.invalid/team/mesh.git"),
        );
        let id = Uuid::now_v7();
        outbox
            .append(&created(&id.to_string(), shareable_metadata()))
            .unwrap();

        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(report.applied, 1, "the worker never touches the network");
        assert!(report.committed);
        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        assert!(path.exists());
    }

    #[test]
    fn eligibility_absenting_or_private_is_not_published_shareable_is() {
        let (mesh, outbox) = fixture("eligibility", None);

        let shareable_id = Uuid::now_v7();
        outbox
            .append(&created(
                &shareable_id.to_string(),
                json!({ "eligibility": "shareable" }),
            ))
            .unwrap();
        let private_id = Uuid::now_v7();
        outbox
            .append(&created(
                &private_id.to_string(),
                json!({ "eligibility": "private" }),
            ))
            .unwrap();
        let absent_id = Uuid::now_v7();
        outbox
            .append(&created(&absent_id.to_string(), json!({})))
            .unwrap();

        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(report.applied, 1, "only the shareable record is written");
        assert_eq!(
            report.skipped, 2,
            "private and absent-eligibility are skipped"
        );
        assert!(report.committed);

        let dir = mesh.root().join("ns").join("memories");
        assert!(dir.join(format!("{shareable_id}.json")).exists());
        assert!(!dir.join(format!("{private_id}.json")).exists());
        assert!(!dir.join(format!("{absent_id}.json")).exists());
    }

    #[test]
    fn a_superseded_entry_records_the_lifecycle_and_successor() {
        let (mesh, outbox) = fixture("superseded", None);
        let old_id = Uuid::now_v7();
        let new_id = Uuid::now_v7();
        let entry = OutboxEntry {
            stable_op_id: Uuid::now_v7(),
            op: OP_SUPERSEDED.to_string(),
            id: old_id.to_string(),
            namespace: "ns".to_string(),
            memory_type: "rule".to_string(),
            content: "old text".to_string(),
            metadata: shareable_metadata(),
            superseded_by: Some(new_id.to_string()),
            ts: 1_700_000_000,
            epoch: 1,
        };
        outbox.append(&entry).unwrap();

        drain_once(&outbox, &mesh, "node-author").unwrap();
        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{old_id}.json"));
        let record: MeshRecord =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(record.lifecycle.state, LifecycleState::Superseded);
        assert_eq!(record.lifecycle.superseded_by, Some(new_id));
    }

    #[test]
    fn an_archived_entry_records_the_lifecycle_and_tombstone() {
        let (mesh, outbox) = fixture("archived", None);
        let id = Uuid::now_v7();
        let entry = OutboxEntry {
            stable_op_id: Uuid::now_v7(),
            op: OP_ARCHIVED.to_string(),
            id: id.to_string(),
            namespace: "ns".to_string(),
            memory_type: "rule".to_string(),
            content: "text".to_string(),
            metadata: json!({ "eligibility": "shareable", "archived_at": 1_700_000_000_i64 }),
            superseded_by: None,
            ts: 1_700_000_000,
            epoch: 1,
        };
        outbox.append(&entry).unwrap();

        drain_once(&outbox, &mesh, "node-author").unwrap();
        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        let record: MeshRecord =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(record.lifecycle.state, LifecycleState::Archived);
        assert!(record.lifecycle.archived_at.is_some());
    }

    #[test]
    fn a_file_rename_fault_keeps_the_entry_pending_until_the_fault_clears() {
        let (mesh, outbox) = fixture("rename-fault", None);
        let id = Uuid::now_v7();
        let entry = created(&id.to_string(), shareable_metadata());
        outbox.append(&entry).unwrap();
        let before = git_count_or_zero(mesh.root());

        // Fault: pre-create the record's target path as a DIRECTORY, so the
        // tmp->rename step of the file write fails (rename(2) cannot replace
        // a directory with a file). Everything before the rename succeeds:
        // the entry is read, classified, the tmp JSON is written.
        let dir = mesh.root().join("ns").join("memories");
        let target = dir.join(format!("{id}.json"));
        std::fs::create_dir_all(&target).unwrap();

        let err = drain_once(&outbox, &mesh, "node-author").unwrap_err();
        assert!(
            err.contains("record rename"),
            // The fixture tag "rename-fault" lives in the path, so a loose
            // `contains("rename")` would also pass a READ-stage failure;
            // only the tmp->rename step's message says "record rename".
            "the failure must be the tmp->rename step, not an earlier stage: {err}"
        );

        // Ordering — the WAL compaction must NOT have run: the entry stays
        // pending, and no git commit was authored for a batch that failed.
        assert_eq!(
            outbox.pending().unwrap().len(),
            1,
            "mark_committed must not be reached"
        );
        assert_eq!(
            git_count_or_zero(mesh.root()),
            before,
            "no commit when the file step fails"
        );
        assert!(!target.is_file(), "the record file never landed");

        // The fault clears: the next drain applies cleanly — the pending
        // entry is replayed, exactly one mesh file exists, exactly one commit.
        std::fs::remove_dir(&target).unwrap();
        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(report.applied, 1);
        assert!(report.committed);
        assert_eq!(
            outbox.pending().unwrap().len(),
            0,
            "the drain consumed the entry"
        );
        assert_eq!(
            git_count(mesh.root()),
            before + 1,
            "exactly one commit was authored"
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "exactly one file in the dir"
        );
        assert!(target.is_file());
    }

    #[test]
    fn a_lifecycle_transition_preserves_the_origin_fields() {
        let (mesh, outbox) = fixture("origin-preserved", None);
        let id = Uuid::now_v7();

        // Create, then archive the same record.
        outbox
            .append(&created(&id.to_string(), shareable_metadata()))
            .unwrap();
        drain_once(&outbox, &mesh, "node-author").unwrap();

        let archive = OutboxEntry {
            stable_op_id: Uuid::now_v7(),
            op: OP_ARCHIVED.to_string(),
            id: id.to_string(),
            namespace: "ns".to_string(),
            memory_type: "rule".to_string(),
            content: "text".to_string(),
            metadata: json!({ "eligibility": "shareable", "archived_at": 1_700_000_100_i64 }),
            superseded_by: None,
            ts: 1_700_000_100,
            epoch: 1,
        };
        outbox.append(&archive).unwrap();
        drain_once(&outbox, &mesh, "node-author").unwrap();

        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        let record: MeshRecord =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(record.lifecycle.state, LifecycleState::Archived);
        assert_eq!(
            record.lifecycle.created_at,
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            "created_at does not drift across a lifecycle transition"
        );
        assert_eq!(record.lifecycle.created_by, "node-author");
        assert_eq!(
            record.origin_mesh_epoch, 1,
            "origin epoch is preserved across transitions"
        );
    }

    // ---- slice F4: publish-side epoch quarantine ---------------------------
    //
    // Spec §"Deletion & erasure": the mesh-worker must refuse to publish a
    // record born in an older epoch than the mesh's current one — the
    // publish-side twin of `import::epoch_gate`. A stale entry is quarantined
    // (named, left pending) rather than drained into the new epoch; a
    // current-epoch entry drains exactly as it always has.

    /// An entry captured at epoch 1, drained after a migration to epoch 2, is
    /// quarantined: no file write, a named report line, and the entry STAYS
    /// pending for operator inspection (never consumed).
    #[test]
    fn a_stale_epoch_entry_is_quarantined_named_and_stays_pending() {
        let (mesh, outbox) = fixture("stale-epoch", None);
        let id = Uuid::now_v7();
        let entry = created(&id.to_string(), shareable_metadata()); // epoch 1
        outbox.append(&entry).unwrap();

        // Migrate: manifest epoch 1 -> 2 (the filter-repo purge).
        let mesh = reopen_with_epoch(&mesh, 2);

        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(report.applied, 0, "the stale entry must not publish");
        assert_eq!(report.quarantined, 1, "one entry quarantined: {report:?}");
        assert!(!report.committed, "no commit for a quarantined entry");
        assert_eq!(report.quarantined_entries.len(), 1, "one named line");
        let line = &report.quarantined_entries[0];
        assert!(
            line.contains("epoch 1") && line.contains("manifest epoch 2"),
            "names both epochs: {line}"
        );
        assert!(line.contains(&id.to_string()), "names the entry: {line}");

        // The file was never written.
        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        assert!(!path.exists(), "no record file for a quarantined entry");

        // The entry stays pending — NOT consumed — for operator inspection.
        assert_eq!(
            outbox.pending().unwrap().len(),
            1,
            "the quarantined entry stays pending"
        );
    }

    /// An entry captured at the CURRENT epoch drains after a migration — the
    /// publish gate only quarantines OLDER epochs, never current ones.
    #[test]
    fn a_current_epoch_entry_drains_after_an_epoch_migration() {
        let (mesh, outbox) = fixture("current-epoch", None);
        let mesh = reopen_with_epoch(&mesh, 2); // migrate FIRST
        let id = Uuid::now_v7();
        let mut entry = created(&id.to_string(), shareable_metadata());
        entry.epoch = 2; // born after the migration
        outbox.append(&entry).unwrap();

        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(report.applied, 1, "current-epoch entry drains: {report:?}");
        assert_eq!(report.quarantined, 0, "nothing quarantined: {report:?}");
        assert!(report.committed);
        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        let record: MeshRecord =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            record.origin_mesh_epoch, 2,
            "stamped with the current epoch"
        );
    }

    /// A mixed batch drains the current-epoch entry (committing it) while the
    /// stale entry is quarantined and LEFT PENDING — the commit step must
    /// never consume a quarantined entry.
    #[test]
    fn a_mixed_batch_quarantines_stale_and_drains_current() {
        let (mesh, outbox) = fixture("mixed-epoch", None);
        let stale_id = Uuid::now_v7();
        let current_id = Uuid::now_v7();
        let stale = created(&stale_id.to_string(), shareable_metadata()); // epoch 1
        outbox.append(&stale).unwrap();
        let mut current = created(&current_id.to_string(), shareable_metadata());
        current.epoch = 2; // born post-migration
        outbox.append(&current).unwrap();

        let mesh = reopen_with_epoch(&mesh, 2);
        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(
            report.applied, 1,
            "only the current entry drains: {report:?}"
        );
        assert_eq!(report.quarantined, 1, "the stale entry is quarantined");
        assert!(report.committed, "the current entry commits");

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1, "only the stale entry remains pending");
        assert_eq!(
            pending[0].stable_op_id, stale.stable_op_id,
            "the quarantined entry is the one left pending"
        );
    }

    /// The mesh's PUBLISH path reduces a record's metadata to the curated
    /// envelope — `eligibility`, `source`, `tags` (`projected_metadata`,
    /// above). So a `fact` carrying a declared field such as
    /// `metadata.topology` crosses the mesh as a `fact` RECORD — the type
    /// closure holds and the content travels — while the declared field
    /// itself stays node-local: the wire format is unchanged.
    #[test]
    fn a_published_fact_keeps_the_curated_metadata_envelope() {
        let (mesh, outbox) = fixture("curated-envelope", None);
        let id = Uuid::now_v7();
        let topology = json!({
            "component": "api-gateway",
            "target_system": "ledger-service",
            "channel": "mTLS",
            "credential_boundary": "eu-west-1 VPC",
            "reason": "the ledger accepts traffic only from inside the VPC",
        });
        let entry = OutboxEntry {
            memory_type: "fact".to_string(),
            ..created(
                &id.to_string(),
                json!({
                    "eligibility": "shareable",
                    "source": "owner 2026-10-21",
                    "tags": ["access-topology"],
                    "topology": topology,
                }),
            )
        };
        outbox.append(&entry).unwrap();

        let report = drain_once(&outbox, &mesh, "node-author").unwrap();
        assert_eq!(report.applied, 1, "the fact is published: {report:?}");
        assert_eq!(report.skipped, 0, "the fact is not skipped: {report:?}");

        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path).expect("the record file must exist");
        let record: MeshRecord =
            serde_json::from_str(&text).expect("the file parses as MeshRecord");

        // The type closure holds: the record is still a `fact`.
        assert_eq!(record.memory_type, "fact");
        // The definition travels.
        assert_eq!(
            record.content,
            json!("the memory text"),
            "the fact's content crosses the mesh"
        );
        // The curated envelope survives...
        assert_eq!(record.metadata["eligibility"], json!("shareable"));
        assert_eq!(record.metadata["source"], json!("owner 2026-10-21"));
        assert_eq!(record.metadata["tags"], json!(["access-topology"]));
        // ...and nothing else does: a declared field is NOT published.
        assert!(
            record.metadata.get("topology").is_none(),
            "the published metadata is the curated envelope: {}",
            record.metadata
        );
    }

    #[test]
    fn apply_one_signs_record_when_signing_key_exists_and_bundle_roundtrip() {
        let (mesh, outbox) = fixture("sign-bundle", None);
        let id = Uuid::now_v7();
        // Pre-create a valid Ed25519 signing key inside the mesh.
        let key_dir = mesh.root().join(".neurostrata-mesh");
        std::fs::create_dir_all(&key_dir).unwrap();
        let key_path = key_dir.join("signing.key");
        let _ = crate::did::load_or_generate_keypair(&key_path).unwrap();

        let entry = created(&id.to_string(), shareable_metadata());
        outbox.append(&entry).unwrap();
        drain_once(&outbox, &mesh, "node-author").unwrap();

        let path = mesh
            .root()
            .join("ns")
            .join("memories")
            .join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path).expect("record exists");
        let record: MeshRecord = serde_json::from_str(&text).unwrap();

        // (a) DID-signed when local signing key exists.
        assert!(
            record.signature.as_ref().map_or(false, |s| !s.is_empty()),
            "record must carry a non-empty signature"
        );
        let did = record.signer_did.as_ref().expect("signer_did must be set");
        assert!(
            did.starts_with("did:key:z6Mk"),
            "signer_did should start with did:key:z6Mk, got: {}",
            did
        );

        // (b) Compressed bundle round-trips losslessly.
        let bundle_path = write_compressed_bundle(
            &mesh.root().join("bundles"),
            "test-bundle",
            &[record.clone()],
        )
        .unwrap();
        let roundtrip = crate::synaptic_mesh::import::read_compressed_bundle(&bundle_path).unwrap();
        assert_eq!(roundtrip.len(), 1);
        assert_eq!(roundtrip[0], record);

        let loaded = &roundtrip[0];
        assert!(loaded.patch.as_ref().map(|p| !p.is_empty()).unwrap_or(false));
    }
}
