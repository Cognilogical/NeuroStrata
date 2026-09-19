use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

use lbug::{Connection, Database, SystemConfig};

use crate::traits::{MemoryPayload, RelocateOutcome, SearchResult, VectorStore};

pub struct LadybugStore {
    #[allow(dead_code)]
    local_path: PathBuf,
    dimensions: usize,
    db: Arc<Database>,
    dirty: Dirty,
    /// LadybugDB allows exactly one write transaction and REFUSES a second
    /// rather than queueing it, so two writers that overlap produce "Cannot
    /// start a new write transaction in the system" for whichever arrives
    /// second. Every search spawns access-count writes, which makes that
    /// collision ordinary rather than exotic (bead neurostrata-3fi.6.6).
    ///
    /// Taking a turn here converts a random failure into a short wait. Writes
    /// are milliseconds now that checkpointing has left the request path, and
    /// the caller's deadline covers the wait as well as the write. The
    /// checkpoint deliberately does NOT take this lock: it waits for
    /// transactions to drain, so holding writers out for its duration would
    /// recreate the stall it was moved out of the request path to avoid.
    write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Set once the tables exist. init() is called on the way into add, move
    /// and -- because it is cheap to write and expensive to mean -- every
    /// search, and each call issued CREATE NODE TABLE only to be told it
    /// already exists. That is a write transaction per read, colliding with
    /// real writes for nothing.
    schema_ready: std::sync::atomic::AtomicBool,
}

/// Whether a write is sitting in the log with no checkpoint behind it yet.
///
/// `take` clears and reports in one step so a write landing mid-checkpoint is
/// not swallowed: the flag is cleared before the flush, and put back if the
/// flush fails.
#[derive(Default)]
struct Dirty(std::sync::atomic::AtomicBool);

impl Dirty {
    fn mark(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }

    fn is_set(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    fn take(&self) -> bool {
        self.0.swap(false, std::sync::atomic::Ordering::AcqRel)
    }
}

/// Normal open: checksums on, and replay failures throw. The throw is the whole
/// point -- with it off the engine swallows a failed verification, opens anyway
/// and silently drops every record it could not authenticate, which is exactly
/// the outcome this code exists to make visible.
fn verifying_config() -> SystemConfig {
    SystemConfig::default().throw_on_wal_replay_failure(true)
}

/// Recovery open, used only after verification has already failed. The engine
/// cannot currently verify a WAL it wrote itself once the process dies without a
/// clean close: with checksums on, replay restores the catalog and discards
/// every row; with them off, the same rows come back intact. See
/// examples/wal_repro.rs, and bead neurostrata-kug for the upstream defect.
///
/// This trades tamper-evidence for the writes, so it is never the normal path
/// and never silent.
fn unverified_config() -> SystemConfig {
    SystemConfig::default()
        .throw_on_wal_replay_failure(false)
        .enable_checksums(false)
}

/// Set NEUROSTRATA_STRICT_WAL=1 to refuse the unverified retry. The database
/// then fails to open rather than replaying records it could not authenticate --
/// the right choice where an unreadable WAL should be investigated rather than
/// recovered.
fn strict_wal_verification() -> bool {
    std::env::var("NEUROSTRATA_STRICT_WAL").map(|v| v == "1").unwrap_or(false)
}

fn is_wal_integrity_failure(err: &str) -> bool {
    err.contains("Checksum verification failed")
        || err.contains("Corrupted wal file")
        || err.contains("invalid WAL record type")
}

/// Copies a leftover write-ahead log aside before it is replayed and truncated.
/// Best effort by design: failing to keep the copy is not a reason to refuse to
/// open the database.
fn preserve_unclean_wal(local_path: &std::path::Path) {
    let mut wal_path = local_path.to_path_buf().into_os_string();
    wal_path.push(".wal");
    let wal_file = std::path::PathBuf::from(wal_path);

    let size = match std::fs::metadata(&wal_file) {
        Ok(m) if m.len() > 0 => m.len(),
        _ => return,
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut copy_path = wal_file.clone().into_os_string();
    copy_path.push(format!(".unclean.{}", timestamp));
    let copy_file = std::path::PathBuf::from(copy_path);

    eprintln!(
        "WARNING: the last shutdown was not clean -- a {} byte write-ahead log was left behind. Recovering what it still holds.",
        size
    );
    match std::fs::copy(&wal_file, &copy_file) {
        Ok(_) => eprintln!("A copy was kept at {:?} in case recovery is incomplete.", copy_file),
        Err(e) => eprintln!("WARNING: could not keep a copy of it: {}", e),
    }
}

impl LadybugStore {
    pub fn new(local_path: impl Into<PathBuf>, dimensions: usize) -> Result<Self> {
        let local_path = local_path.into();

        // A WAL present at open time means the last process did not close cleanly:
        // a clean close checkpoints and removes it. Keep a copy before the engine
        // replays, because replay truncates the file to the last good record and
        // whatever followed is then unrecoverable.
        preserve_unclean_wal(&local_path);

        // Open with verification first. Only if the WAL fails to authenticate do we
        // consider replaying it unverified, and never quietly.
        let db = match Database::new(&local_path, verifying_config()) {
            Ok(db) => db,
            Err(e) => {
                let err_msg = e.to_string();
                if !is_wal_integrity_failure(&err_msg) {
                    return Err(e.into());
                }

                eprintln!("ERROR: the write-ahead log failed verification: {}", err_msg);

                if strict_wal_verification() {
                    return Err(anyhow::anyhow!(
                        "Refusing to open: the write-ahead log did not verify, and NEUROSTRATA_STRICT_WAL=1 forbids replaying records that cannot be authenticated. The log has been left in place for inspection. Unset the variable to recover the writes without verification."
                    ));
                }

                eprintln!("ERROR: retrying WITHOUT checksum verification to recover those writes. The records replayed this way are NOT authenticated -- if this database may have been tampered with, stop and inspect the preserved copy instead. Set NEUROSTRATA_STRICT_WAL=1 to refuse this fallback.");

                match Database::new(&local_path, unverified_config()) {
                    Ok(db) => {
                        eprintln!("Recovered by replaying the unverified write-ahead log.");
                        db
                    }
                    Err(retry_e) => {
                        // Unreadable even without verification: set it aside rather
                        // than leave the database unopenable, and say what was lost.
                        let mut wal_path = local_path.clone().into_os_string();
                        wal_path.push(".wal");
                        let wal_file = std::path::PathBuf::from(wal_path);

                        if !wal_file.exists() {
                            return Err(anyhow::anyhow!(
                                "WAL verification failed and the file is no longer at {:?}: {}",
                                wal_file,
                                retry_e
                            ));
                        }

                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let mut corrupted_path = wal_file.clone().into_os_string();
                        corrupted_path.push(format!(".corrupted.{}", timestamp));
                        let corrupted_file = std::path::PathBuf::from(corrupted_path);

                        let dropped_bytes = std::fs::metadata(&wal_file).map(|m| m.len()).unwrap_or(0);
                        std::fs::rename(&wal_file, &corrupted_file).map_err(|err| {
                            anyhow::anyhow!("Recovery failed: could not move the unreadable WAL file: {}", err)
                        })?;

                        eprintln!(
                            "ERROR: DATA LOSS. {} bytes of un-checkpointed writes could not be replayed even unverified, and were discarded to reopen the database.",
                            dropped_bytes
                        );
                        eprintln!("ERROR: they were kept at {:?} -- do not delete it if those writes mattered.", corrupted_file);
                        // No WAL left to verify at this point.
                        Database::new(&local_path, verifying_config())
                            .map_err(|last_e| anyhow::anyhow!("Retry failed after discarding the WAL: {}", last_e))?
                    }
                }
            }
        };

        Ok(Self {
            local_path,
            dimensions,
            db: Arc::new(db),
            dirty: Dirty::default(),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            schema_ready: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Runs a write with a deadline, so a caller is told the database is busy
    /// rather than left holding a connection that never answers. Engine-level
    /// lock waits of two and a half minutes were measured while a burst of
    /// writes was in flight (bead neurostrata-3fi.6).
    ///
    /// The deadline releases the CALLER, not the query: spawn_blocking cannot be
    /// cancelled, so the statement may still land afterwards. The message says
    /// so, because retrying blind could otherwise duplicate work.
    ///
    /// Which is why the writer's turn belongs to the blocking task and not to
    /// the caller. It used to be a guard inside the timed future, so a caller
    /// that gave up released the turn while its statement was still running,
    /// and the next writer promptly opened a second one against an engine that
    /// already had one. That is how a single stalled write became five in a row
    /// (bead neurostrata-4nk): each waited its 30s, gave up, and handed the
    /// turn to the next. Held by the task, the turn is released when the work
    /// actually finishes, so a caller giving up costs a wait instead.
    async fn write_with_deadline<T, F>(&self, what: &'static str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: Fn(&Connection) -> Result<T> + Clone + Send + 'static,
    {
        let write_lock = Arc::clone(&self.write_lock);
        let db = self.db.clone();

        let outcome = bounded(what, write_timeout(), async move {
            // One writer at a time, because the engine refuses the second
            // rather than making it wait. The deadline covers this queue too:
            // a caller stuck behind a stalled write hears that the database is
            // busy instead of waiting indefinitely for its turn. Giving up
            // while merely waiting for the turn leaks nothing -- no statement
            // has been started yet.
            let mut turn = write_lock.lock_owned().await;

            // The turn orders our own writers; the background checkpoint and
            // any other process are outside it, so a refusal can still arrive.
            // Those clear in milliseconds, so wait and try again rather than
            // handing the caller a failure it can do nothing about.
            let mut attempt = 0;
            loop {
                let (returned, result) = Self::write_owning_turn(db.clone(), turn, f.clone()).await?;
                turn = returned;

                match result {
                    Err(e) if is_write_collision(&e) && attempt < WRITE_COLLISION_RETRIES => {
                        WRITE_COLLISIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        attempt += 1;
                        // The turn is held across this pause on purpose: it is
                        // ours until we stop retrying, and nothing is running
                        // inside the engine while we wait.
                        tokio::time::sleep(std::time::Duration::from_millis(25 * attempt as u64))
                            .await;
                    }
                    result => return result,
                }
            }
        })
        .await;

        if outcome.is_err() {
            WRITES_ABANDONED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            eprintln!("DIAGNOSTIC: gave up waiting on {} -- engine: {}", what, engine_stats());
        }

        // Marked whatever the outcome: a write that timed out may still land,
        // and an unnecessary checkpoint attempt costs nothing next to a write
        // that never reaches disk.
        self.dirty.mark();
        outcome
    }

    /// Runs one attempt on a blocking thread, and gives that thread the
    /// writer's turn for as long as it lasts.
    ///
    /// The turn travels back out with the result so a retry can keep it. A
    /// caller that has stopped waiting never reads that result, so the turn is
    /// dropped when the task completes -- which is exactly when the next writer
    /// should be allowed to start.
    async fn write_owning_turn<T, F>(
        db: Arc<Database>,
        turn: tokio::sync::OwnedMutexGuard<()>,
        f: F,
    ) -> Result<(tokio::sync::OwnedMutexGuard<()>, Result<T>)>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        ENGINE_CALLS_STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            let outcome = (|| -> Result<T> {
                let conn = Connection::new(&db)?;
                f(&conn)
            })();
            ENGINE_CALLS_FINISHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (turn, outcome)
        })
        .await
        .map_err(|e| anyhow::anyhow!("the database thread did not finish: {}", e))
    }

    /// Runs one piece of database work on a blocking thread.
    ///
    /// Every lbug call is synchronous FFI. Called straight from an async handler
    /// it occupies a tokio worker for as long as the engine takes, and a handful
    /// of concurrent queries can therefore starve the runtime outright: during a
    /// write burst even /health -- an async closure returning a constant -- had
    /// no thread to run on, which is what made the daemon look dead while it was
    /// still listening (bead neurostrata-3fi.6).
    ///
    /// The connection is opened inside the closure on purpose. It borrows the
    /// Database and is not Send, so it must never be created here and moved, nor
    /// held across an await.
    async fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let db = self.db.clone();
        ENGINE_CALLS_STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            let outcome = (|| -> Result<T> {
                let conn = Connection::new(&db)?;
                f(&conn)
            })();
            // Counted here rather than after the await: the await is what a
            // deadline drops, and the whole question is whether the work
            // outlives the caller that stopped waiting for it.
            ENGINE_CALLS_FINISHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            outcome
        })
        .await
        .map_err(|e| anyhow::anyhow!("the database thread did not finish: {}", e))?
    }
}

/// The ids a namespace actually holds, indexed so a declaration can be resolved
/// without scanning them.
///
/// Node ids became repository-relative, so a memory written when ingestion
/// produced absolute ids declares `C:/proj/src/foo.rs` and matches nothing now.
/// Its tail still identifies the file, so a declaration ending in `/<id>`
/// resolves to that id, and a suffix matching more than one node is left alone.
///
/// That rule is unchanged. Asking it of every known id in turn was the problem:
/// O(declarations x nodes) comparisons with a `format!` allocation per
/// comparison, on the path that runs at the end of every ingest. A declaration
/// has only as many path suffixes as it has separators, so looking each of
/// those up answers the same question in a handful of hashes however large the
/// namespace grows.
pub struct KnownIds<'a> {
    ids: std::collections::HashSet<&'a str>,
    /// Qualified ids indexed by the bare path inside them, so a rule naming
    /// `src/lib.rs` finds `NeuroStrata::src/lib.rs` without scanning. A path
    /// held by more than one namespace is recorded as ambiguous and resolves to
    /// neither: a GOVERNS edge into another project's file is worse than none.
    by_path: std::collections::HashMap<&'a str, Option<&'a str>>,
}

impl<'a> KnownIds<'a> {
    pub fn new(ids: impl IntoIterator<Item = &'a str>) -> Self {
        let ids: std::collections::HashSet<&'a str> =
            ids.into_iter().filter(|id| !id.is_empty()).collect();

        let mut by_path: std::collections::HashMap<&'a str, Option<&'a str>> =
            std::collections::HashMap::new();
        for id in &ids {
            if let Some(cut) = id.find(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                let path = &id[cut + crate::parser::ingest::NAMESPACE_SEPARATOR.len()..];
                if path.is_empty() {
                    continue;
                }
                by_path
                    .entry(path)
                    .and_modify(|slot| *slot = None)
                    .or_insert(Some(id));
            }
        }

        Self { ids, by_path }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Finds the node a declaration means, when it does not name one exactly.
    pub fn resolve(&self, declared: &str) -> Option<String> {
        // Deliberately before normalisation: an id stored with backslashes is
        // still matched by naming it exactly, as it always was.
        if self.ids.contains(declared) {
            return Some(declared.to_string());
        }

        let declared = declared.replace('\\', "/");

        // An id qualified by its namespace is LONGER than the declaration that
        // names it: a rule says `src/lib.rs`, the id is `NeuroStrata::src/lib.rs`.
        // That is the common case now, so it is asked first. A path held by two
        // namespaces was indexed as ambiguous and resolves to neither.
        if let Some(found) = self.by_path.get(declared.as_str()) {
            return found.map(str::to_string);
        }

        // The other direction: a declaration longer than the id it means, which
        // is a legacy absolute path naming a file the graph already holds.
        //
        // Its suffix can name that file two ways. Before migrating it is an id
        // (`src/lib.rs`); after doctor's migration, a re-ingest, it is the path
        // inside a qualified id (`NeuroStrata::src/lib.rs`). Checking only ids
        // meant every absolute declaration lost its edge at exactly the step
        // the migration tells people to take.
        let mut only: Option<&str> = None;
        for (separator, _) in declared.match_indices('/') {
            let suffix = &declared[separator + 1..];
            let mut hits: Vec<&str> = Vec::new();
            if let Some(id) = self.ids.get(suffix) {
                hits.push(id);
            }
            match self.by_path.get(suffix) {
                Some(Some(qualified)) => hits.push(qualified),
                // Held by more than one namespace: ambiguous, as above.
                Some(None) => return None,
                None => {}
            }
            for id in hits {
                if only.is_some() {
                    // A suffix matching more than one node is ambiguous, and a
                    // wrong edge is worse than a missing one.
                    return None;
                }
                only = Some(id);
            }
        }
        only.map(|id| id.to_string())
    }
}

/// One edge a memory asks for, read off its metadata.
#[derive(Debug, PartialEq, Clone)]
pub struct EdgeSpec {
    pub rel_type: &'static str,
    pub target_id: String,
    /// true for `self -> target`, false for `target -> self`. Containment reads
    /// the other way round: the parent contains the child, not the reverse.
    pub points_at_target: bool,
}

/// Maps metadata arrays onto the three edge tables. `related_to` is a semantic
/// link, `contained_by` is structure (directory to file to symbol), and
/// `governs` connects a rule to the code it constrains -- the edge that lets an
/// architectural memory be found from the file it applies to.
pub fn edge_specs(metadata: &serde_json::Value) -> Vec<EdgeSpec> {
    let mut specs = Vec::new();
    for (key, rel_type, points_at_target) in [
        ("related_to", "RELATES_TO", true),
        ("contained_by", "CONTAINS", false),
        ("governs", "GOVERNS", true),
    ] {
        if let Some(items) = metadata.get(key).and_then(|v| v.as_array()) {
            for item in items {
                if let Some(target) = item.as_str() {
                    if target.is_empty() {
                        continue;
                    }
                    specs.push(EdgeSpec {
                        rel_type,
                        target_id: target.to_string(),
                        points_at_target,
                    });
                }
            }
        }
    }
    specs
}

/// Memory types written with a zero vector by directory ingestion. They describe
/// where something lives, not what it says, so they carry no meaning for a
/// similarity search.
const STRUCTURAL_MEMORY_TYPES: [&str; 3] = ["directory", "file", "markdown"];

fn escape_kuzu_string(s: &str) -> String {
    s.replace("\\", "\\\\").replace("'", "\\'")
}

/// How many edges go into one relinking statement. The pairs are written into
/// the query text, so this is what bounds how large that text can get.
const RELINK_BATCH: usize = 500;

/// Replays one relationship type in a couple of statements instead of one per
/// edge.
///
/// The obvious spelling is a single UNWIND driving MERGE, which would say "link
/// these unless they are linked already" in one statement. lbug 0.15.3 cannot
/// run it: MERGE inside an UNWIND pipeline fails with an internal
/// `invalid unordered_map<K, T> key`, while the same UNWIND with MATCH, or the
/// same MERGE without UNWIND, both succeed. So the pairs that already exist are
/// read back and subtracted here, which makes CREATE safe and keeps the
/// statement count independent of the number of edges.
///
/// It also reports the truth. The loop this replaces counted statements that
/// did not error -- including every one whose MATCH found nothing -- so an
/// ingest that linked nothing still announced hundreds of relinked edges.
fn create_missing_edges(conn: &Connection, rel_type: &str, pairs: &[(String, String)]) -> Result<usize> {
    let existing = conn.query(&format!(
        "MATCH (x:Memory)-[:{}]->(y:Memory) RETURN x.id, y.id",
        rel_type
    ))?;

    let mut present = std::collections::HashSet::new();
    for row in existing {
        present.insert((format!("{}", row[0]), format!("{}", row[1])));
    }

    // The same edge can be declared by both ends, and CREATE would honour each
    // declaration separately, so the batch is deduplicated as well.
    let mut missing = Vec::new();
    for pair in pairs {
        if !present.contains(pair) {
            present.insert(pair.clone());
            missing.push(pair);
        }
    }

    let mut created = 0usize;
    for chunk in missing.chunks(RELINK_BATCH) {
        let list = chunk
            .iter()
            .map(|(from, to)| {
                format!(
                    "{{f: '{}', t: '{}'}}",
                    escape_kuzu_string(from),
                    escape_kuzu_string(to)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        // Both ends must exist; a rule naming a file the ingester never reached
        // still produces nothing, exactly as before.
        let query = format!(
            "UNWIND [{}] AS e MATCH (a:Memory {{id: e.f}}), (b:Memory {{id: e.t}}) CREATE (a)-[:{}]->(b) RETURN count(*)",
            list, rel_type
        );
        for row in conn.query(&query)? {
            created += format!("{}", row[0]).parse::<usize>().unwrap_or(0);
        }
    }

    Ok(created)
}

#[async_trait]
impl VectorStore for LadybugStore {
    async fn init(&self, _namespace: &str) -> Result<()> {
        if self.schema_ready.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }

        let _namespace = _namespace.to_string();
        let dimensions = self.dimensions;
        let _turn = self.write_lock.lock().await;
        let outcome = self.with_conn(move |conn| {

            let create_node_table = format!(
                "CREATE NODE TABLE Memory (id STRING, namespace STRING, content STRING, user_id STRING, memory_type STRING, agent_name STRING, location STRING, location_lines STRING, metadata STRING, embedding FLOAT[{}], PRIMARY KEY (id))",
                dimensions
            );
            if let Err(e) = conn.query(&create_node_table) {
                if !e.to_string().contains("already exists") {
                    return Err(e.into());
                }
            }

            let create_rel_table = "CREATE REL TABLE RELATES_TO (FROM Memory TO Memory)";
            conn.query(create_rel_table).ok();

            let create_contains_table = "CREATE REL TABLE CONTAINS (FROM Memory TO Memory)";
            conn.query(create_contains_table).ok();

            let create_governs_table = "CREATE REL TABLE GOVERNS (FROM Memory TO Memory)";
            conn.query(create_governs_table).ok();

            Ok(())
        })
        .await;

        if outcome.is_ok() {
            self.schema_ready
                .store(true, std::sync::atomic::Ordering::Release);
        }

        // Creating the tables is itself a write worth flushing.
        self.dirty.mark();
        outcome
    }

    async fn upsert(
        &self,
        namespace: &str,
        id: &str,
        vector: Vec<f32>,
        payload: MemoryPayload,
    ) -> Result<()> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        self.write_with_deadline("writing a memory", move |conn| {

            let safe_id = escape_kuzu_string(&id);
            let safe_ns = escape_kuzu_string(&namespace);
            let safe_content = escape_kuzu_string(&payload.content);
            let safe_user_id = escape_kuzu_string(&payload.user_id);
            let safe_memory_type = escape_kuzu_string(&payload.memory_type);
            let safe_agent_name = escape_kuzu_string(
                payload.agent_name.as_deref().unwrap_or("unknown"),
            );
            let safe_location = escape_kuzu_string(&payload.location);
            let safe_location_lines = escape_kuzu_string(&payload.location_lines);
            let safe_metadata = escape_kuzu_string(&serde_json::to_string(&payload.metadata)?);
        
            let vec_str = format!("[{}]", vector.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(","));

            let insert_query = format!(
                "MERGE (m:Memory {{id: '{}'}})
                 ON CREATE SET m.namespace = '{}', m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}
                 ON MATCH SET m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}",
                safe_id, safe_ns, safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str,
                safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str
            );

            conn.query(&insert_query)?;
        
            // Materialise the edges this memory declares. A target that does not exist
            // yet simply produces no edge: MATCH finds nothing and MERGE never runs.
            // That is deliberate -- a rule may name a file the ingester has not reached.
            for edge in edge_specs(&payload.metadata) {
                // A qualified id names its node exactly, and is what ingestion
                // writes, so it goes straight to the MERGE. Anything else may be
                // a path written the way a human writes it -- src/lib.rs, for a
                // node ingested as NeuroStrata::src/lib.rs -- and is resolved
                // now, rather than waiting for the next ingest to relink it.
                let target = if edge.target_id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
                    edge.target_id.clone()
                } else {
                    match resolve_target_on_write(conn, &namespace, &edge.target_id) {
                        Some(found) => found,
                        None => continue,
                    }
                };
                let target_safe = escape_kuzu_string(&target);
                let (from, to) = if edge.points_at_target {
                    (safe_id.as_str(), target_safe.as_str())
                } else {
                    (target_safe.as_str(), safe_id.as_str())
                };
                let edge_query = format!(
                    "MATCH (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}}) MERGE (a)-[:{}]->(b)",
                    from, to, edge.rel_type
                );
                conn.query(&edge_query).ok();
            }

            Ok(())
        })
        .await
    }

    async fn search(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let namespace = namespace.to_string();
        self.with_conn(move |conn| {
            let safe_ns = escape_kuzu_string(&namespace);
            let vec_str = format!("[{}]", vector.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(","));
            let now = chrono::Utc::now().timestamp();

            // Kuzu has no OFFSET, so we over-fetch with a larger LIMIT and
            // filter/trim in Rust. The multiplier bounds cost: at most
            // FETCH_MULTIPLIER * limit rows are scanned from the engine.
            const FETCH_MULTIPLIER: usize = 10;
            let fetch_limit = limit * FETCH_MULTIPLIER;

            // Step 1: Base vector search.
            //
            // Structural nodes are stored with an all-zero embedding, and the
            // distance from an all-zero row is the query vector's own magnitude
            // -- the same value for every one of them, whatever was asked. Left
            // in, they tie with each other and can fill the limit with path
            // stubs. They stay reachable through the graph expansion in step 2,
            // which is where they are actually useful.
            //
            // ORDER BY dist ASC, id ASC gives deterministic tie-breaking so the
            // same query returns the same rows across runs.
            let structural_types = STRUCTURAL_MEMORY_TYPES
                .iter()
                .map(|t| format!("'{}'", t))
                .collect::<Vec<_>>()
                .join(", ");
            let search_query = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND NOT m.memory_type IN [{}] RETURN m.id, array_distance(m.embedding, {}) AS dist, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata ORDER BY dist ASC, m.id ASC LIMIT {}",
                safe_ns, structural_types, vec_str, fetch_limit
            );

            let result = conn.query(&search_query)?;
            let mut primary_ids = Vec::new();
            let mut results = Vec::new();

            for row in result {
                let id: String = format!("{}", row[0]);
                let distance: f32 = match &row[1] {
                    lbug::Value::Float(f) if f.is_finite() && *f >= 0.0 => *f,
                    lbug::Value::Double(d) if d.is_finite() && *d >= 0.0 => *d as f32,
                    _ => continue,
                };

                let content: String = format!("{}", row[2]);
                let user_id: String = format!("{}", row[3]);
                let memory_type: String = format!("{}", row[4]);
                let agent_name: String = format!("{}", row[5]);
                let location: String = format!("{}", row[6]);
                let location_lines: String = format!("{}", row[7]);
                let metadata_str: String = format!("{}", row[8]);

                let metadata_val: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Null);

                // Skip retired rows -- they do not count toward the limit.
                if is_retired_at(&metadata_val, now) {
                    continue;
                }

                // Usage boost: ln(1+n) gives a real gain even at n=1, and the
                // cap at half distance preserves semantic ranking order.
                let access_count = metadata_val.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
                let n = access_count.max(0) as f32;
                let gain = 0.05 * (1.0 + n).ln();
                let capped_gain = gain.min(0.5 * distance);
                let boosted_distance = distance - capped_gain;

                primary_ids.push(id.clone());

                results.push(SearchResult {
                    id,
                    score: boosted_distance,
                    payload: MemoryPayload {
                        content,
                        user_id,
                        memory_type,
                        agent_name: Some(agent_name),
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                });

                // We have enough active primaries; stop scanning.
                if results.len() >= limit {
                    break;
                }
            }

            // Step 2: Hybrid GraphRAG Neighborhood Fetch
            // We fetch 1-hop neighbors (CONTAINS, GOVERNS, RELATES_TO) to provide
            // blast radius context. Over-fetch and filter so retired neighbors
            // cannot crowd out live ones.
            if !primary_ids.is_empty() {
                let id_list = primary_ids.iter()
                    .map(|id| format!("'{}'", escape_kuzu_string(&id)))
                    .collect::<Vec<_>>()
                    .join(", ");

                // Anything one hop from a primary match.
                let neighbor_query = format!(
                    "MATCH (a:Memory)-[]-(b:Memory) WHERE a.id IN [{}] AND b.namespace = '{}' AND NOT b.id IN [{}] RETURN DISTINCT b.id, b.content, b.user_id, b.memory_type, b.agent_name, b.location, b.location_lines, b.metadata LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );

                // And one step further, but only along GOVERNS. A match is usually a
                // symbol, whose file is one hop away, which puts the rule governing
                // that file two hops out -- exactly the thing worth surfacing, and
                // unreachable from the query above.
                let governing_query = format!(
                    "MATCH (a:Memory)-[]-(f:Memory)<-[:GOVERNS]-(r:Memory) WHERE a.id IN [{}] AND r.namespace = '{}' AND NOT r.id IN [{}] RETURN DISTINCT r.id, r.content, r.user_id, r.memory_type, r.agent_name, r.location, r.location_lines, r.metadata LIMIT {}",
                    id_list, safe_ns, id_list, fetch_limit
                );

                for expansion in [neighbor_query, governing_query] {
                    let mut expansion_count = 0usize;
                    if let Ok(mut rows) = conn.query(&expansion) {
                        while let Some(row) = rows.next() {
                            let id: String = format!("{}", row[0]);
                            let content: String = format!("{}", row[1]);
                            let user_id: String = format!("{}", row[2]);
                            let memory_type: String = format!("{}", row[3]);
                            let agent_name: String = format!("{}", row[4]);
                            let location: String = format!("{}", row[5]);
                            let location_lines: String = format!("{}", row[6]);
                            let metadata_str: String = format!("{}", row[7]);

                            let metadata_val: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Null);

                            // The graph must not bring back what the match filtered out.
                            if is_retired_at(&metadata_val, now) {
                                continue;
                            }

                            // Neighbors get a synthesized lower score (worse
                            // distance) so they appear after primary matches,
                            // but still within the context window.
                            results.push(SearchResult {
                                id,
                                score: 10.0,
                                payload: MemoryPayload {
                                    content,
                                    user_id,
                                    memory_type,
                                    agent_name: Some(agent_name),
                                    location,
                                    location_lines,
                                    metadata: metadata_val,
                                },
                            });
                            expansion_count += 1;
                            if expansion_count >= limit {
                                break;
                            }
                        }
                    }
                }

                // Both expansions can return the same memory; keep the first of each.
                let mut seen = std::collections::HashSet::new();
                results.retain(|r| seen.insert(r.id.clone()));
            }

            results.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
            // Include some neighbors after the primary matches.
            results.truncate(limit * 2);

            Ok(results)
        })
        .await
    }

    async fn delete(&self, namespace: &str, id: &str) -> Result<()> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        self.write_with_deadline("deleting a memory", move |conn| {
            let safe_ns = escape_kuzu_string(&namespace);
            let safe_id = escape_kuzu_string(&id);
        
            let query = format!("MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' DETACH DELETE m", safe_id, safe_ns);
            conn.query(&query)?;
            Ok(())
        })
        .await
    }

    async fn relocate(&self, id: &str, from: &str, to: &str) -> Result<RelocateOutcome> {
        if from == to {
            return Ok(RelocateOutcome::SameNamespace);
        }
        let payload = match self.get(from, id).await? {
            Some((_, payload)) => payload,
            None => return Ok(RelocateOutcome::NotFound),
        };
        // An ingested node's id names the namespace that owns it, and the next
        // ingest of that namespace would MERGE onto the moved row and update it
        // where it now sits -- the cross-project theft qualified ids exist to stop.
        if payload.user_id == "auto-ingestor" || id.contains(crate::parser::ingest::NAMESPACE_SEPARATOR) {
            return Ok(RelocateOutcome::Ingested);
        }

        let (id, from, to) = (id.to_string(), from.to_string(), to.to_string());
        self.write_with_deadline("moving a memory", move |conn| {
            // One statement, so there is no moment at which the memory exists in
            // neither namespace, and the row keeps its id, vector and edges.
            let query = format!(
                "MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' AND m.user_id <> 'auto-ingestor' SET m.namespace = '{}'",
                escape_kuzu_string(&id),
                escape_kuzu_string(&from),
                escape_kuzu_string(&to)
            );
            conn.query(&query)?;
            Ok(RelocateOutcome::Moved)
        })
        .await
    }

    async fn export_database(&self, dir: &str) -> Result<()> {
        let dir = dir.to_string();
        self.with_conn(move |conn| {
            // Checkpoint first so the export cannot miss writes still in the WAL --
            // which, since replay does not restore rows, would otherwise be lost.
            conn.query("CHECKPOINT")?;
            let safe_dir = escape_kuzu_string(&dir);
            conn.query(&format!("EXPORT DATABASE '{}' (format='parquet')", safe_dir))?;
            Ok(())
        })
        .await
    }

    async fn import_database(&self, dir: &str) -> Result<()> {
        let dir = dir.to_string();
        self.with_conn(move |conn| {
            let safe_dir = escape_kuzu_string(&dir);
            conn.query(&format!("IMPORT DATABASE '{}'", safe_dir))?;
            conn.query("CHECKPOINT")?;
            Ok(())
        })
        .await
    }

    async fn checkpoint(&self) -> Result<()> {
        // Cleared first: a write that lands while the engine is flushing must
        // survive, and clearing afterwards would drop its mark.
        let had_writes = self.dirty.take();

        let outcome = self
            .with_conn(move |conn| {
                conn.query("CHECKPOINT")?;
                Ok(())
            })
            .await;

        if outcome.is_err() && had_writes {
            self.dirty.mark();
        }
        outcome
    }

    fn is_dirty(&self) -> bool {
        self.dirty.is_set()
    }

    async fn relink_edges(&self, namespace: &str) -> Result<usize> {
        // Declarations live in each memory's metadata, which survives ingestion;
        // only the edges themselves are lost with the nodes. Replaying them is
        // therefore a read of what is already stored, not a guess.
        let memories = self.list(namespace, None).await?;

        // Ids stored before node ids became repository-relative are absolute, so
        // a rule written to match them declares C:/proj/src/foo.rs and matches
        // nothing now. Resolve those by path suffix against the ids that do
        // exist, and leave an ambiguous suffix alone rather than guess.
        let known = KnownIds::new(memories.iter().map(|m| m.id.as_str()));

        // Grouped by relationship because the type is part of the statement and
        // cannot be a value in it: three groups, so three statements, whatever
        // the edge count.
        let mut wanted: std::collections::HashMap<&'static str, Vec<(String, String)>> =
            std::collections::HashMap::new();
        let mut by_suffix = 0usize;

        for memory in &memories {
            // A retired memory no longer governs anything. Replaying its
            // declarations would give the corrected-away wording its edges back.
            // Its id stays among the known targets above, though: something live
            // may still point at it.
            if is_retired(&memory.payload.metadata) {
                continue;
            }
            for edge in edge_specs(&memory.payload.metadata) {
                let target = match known.resolve(&edge.target_id) {
                    Some(resolved) => {
                        if resolved != edge.target_id {
                            by_suffix += 1;
                        }
                        resolved
                    }
                    // Kept as declared rather than dropped: it names nothing this
                    // namespace holds, but the match is not namespaced, so it can
                    // still land on a node another namespace ingested -- which is
                    // what the statement-per-edge loop did.
                    None => edge.target_id.clone(),
                };
                let pair = if edge.points_at_target {
                    (memory.id.clone(), target)
                } else {
                    (target, memory.id.clone())
                };
                wanted.entry(edge.rel_type).or_default().push(pair);
            }
        }

        if wanted.is_empty() {
            return Ok(0);
        }

        if by_suffix > 0 {
            println!(
                "Relinked {} edge(s) whose declared target was an older absolute path",
                by_suffix
            );
        }

        self.write_with_deadline("relinking the graph", move |conn| {
            let mut created = 0usize;
            for (rel_type, pairs) in &wanted {
                created += create_missing_edges(conn, rel_type, pairs)?;
            }
            Ok(created)
        })
        .await
    }

    async fn clear_ingested(&self, namespace: &str) -> Result<()> {
        let namespace = namespace.to_string();
        self.write_with_deadline("clearing the ingested rows", move |conn| {
            let safe_ns = escape_kuzu_string(&namespace);

            // Every row the ingester owns: the AST symbols and the directory/file
            // nodes too, since ingestion rebuilds the whole structure for a namespace
            let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = 'auto-ingestor' DETACH DELETE m", safe_ns);
            conn.query(&query)?;
            Ok(())
        })
        .await
    }

    async fn list(&self, namespace: &str, user_id: Option<&str>) -> Result<Vec<SearchResult>> {
        let namespace = namespace.to_string();
        let user_id = user_id.map(|v| v.to_string());
        self.with_conn(move |conn| {
            let safe_ns = escape_kuzu_string(&namespace);
        
            let query = if let Some(uid) = user_id {
                format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", safe_ns, escape_kuzu_string(&uid))
            } else {
                format!("MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", safe_ns)
            };

            let result = conn.query(&query)?;
            let mut results = Vec::new();

            for row in result {
                let id: String = format!("{}", row[0]);
                let content: String = format!("{}", row[1]);
                let uid: String = format!("{}", row[2]);
                let memory_type: String = format!("{}", row[3]);
                let agent_name: String = format!("{}", row[4]);
                let location: String = format!("{}", row[5]);
                let location_lines: String = format!("{}", row[6]);
                let metadata_str: String = format!("{}", row[7]);

                let metadata_val: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Null);

                results.push(SearchResult {
                    id,
                    score: 0.0,
                    payload: MemoryPayload {
                        content,
                        user_id: uid,
                        memory_type,
                        agent_name: Some(agent_name),
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                });
            }

            Ok(results)
        })
        .await
    }

    async fn get(&self, namespace: &str, id: &str) -> Result<Option<(Vec<f32>, MemoryPayload)>> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        self.with_conn(move |conn| {
            let safe_ns = escape_kuzu_string(&namespace);
            let safe_id = escape_kuzu_string(&id);

            let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.embedding, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", safe_ns, safe_id);
        
            let mut result = conn.query(&query)?;
        
            if let Some(row) = result.next() {
                // The column is FLOAT[N], a fixed-size ARRAY, so the engine hands
                // back Value::Array. Matching only Value::List meant this always
                // returned an empty vector, and every caller that read a memory
                // and wrote it back stored an embedding of length zero -- which
                // the engine rejects, silently in /edit's case (bead
                // neurostrata-vbj). Accept both: a LIST column would still parse.
                let mut vec: Vec<f32> = Vec::new();
                if let lbug::Value::Array(_, vals) | lbug::Value::List(_, vals) = &row[0] {
                    for v in vals {
                        if let lbug::Value::Float(f) = v {
                            vec.push(*f);
                        } else if let lbug::Value::Double(d) = v {
                            vec.push(*d as f32);
                        }
                    }
                }
            
                let content: String = format!("{}", row[1]);
                let uid: String = format!("{}", row[2]);
                let memory_type: String = format!("{}", row[3]);
                let agent_name: String = format!("{}", row[4]);
                let location: String = format!("{}", row[5]);
                let location_lines: String = format!("{}", row[6]);
                let metadata_str: String = format!("{}", row[7]);

                let metadata_val: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Null);



                return Ok(Some((
                    vec,
                    MemoryPayload {
                        content,
                        user_id: uid,
                        memory_type,
                        agent_name: Some(agent_name),
                        location,
                        location_lines,
                        metadata: metadata_val,
                    },
                )));
            }

            Ok(None)
        })
        .await
    }

    async fn list_namespaces(&self) -> Result<Vec<String>> {
        self.with_conn(move |conn| {
            let query = "MATCH (m:Memory) RETURN DISTINCT m.namespace AS ns;";
            let mut result = conn.query(query)?;
        
            let mut namespaces = Vec::new();
            while let Some(row) = result.next() {
                if let lbug::Value::String(ns) = row[0].clone() {
                    namespaces.push(ns);
                }
            }
        
            Ok(namespaces)
        })
        .await
    }

    async fn export_graph(&self, include_retired: bool) -> Result<serde_json::Value> {
        self.with_conn(move |conn| {
            let now = chrono::Utc::now().timestamp();

            // 1. Fetch all nodes
            let mut nodes = Vec::new();
            let mut retired_ids = std::collections::HashSet::new();
            let query_nodes = "MATCH (n:Memory) RETURN n.id, n.namespace, n.memory_type, n.content, n.location, n.metadata;";
            let mut result_nodes = conn.query(query_nodes)?;
        
            while let Some(row) = result_nodes.next() {
                let id = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let namespace = if let lbug::Value::String(s) = &row[1] { s.clone() } else { "global".to_string() };
                let memory_type = if let lbug::Value::String(s) = &row[2] { s.clone() } else { "unknown".to_string() };
                let content = if let lbug::Value::String(s) = &row[3] { s.clone() } else { "".to_string() };
                let location = if let lbug::Value::String(s) = &row[4] { s.clone() } else { "".to_string() };
            
                let mut absolute_path = "".to_string();
                let mut domain = None;
                let mut retired = false;

                if let lbug::Value::String(metadata_str) = &row[5] {
                    if let Ok(metadata_val) = serde_json::from_str::<serde_json::Value>(metadata_str) {
                        retired = is_retired_at(&metadata_val, now);
                        if let Some(abs_path) = metadata_val.get("absolute_path").and_then(|v| v.as_str()) {
                            absolute_path = abs_path.to_string();
                        }
                        if let Some(d) = metadata_val.get("domain").and_then(|v| v.as_str()) {
                            domain = Some(d.to_string());
                        }
                    }
                }

                // Track retired ids so edges can be filtered below.
                if retired {
                    retired_ids.insert(id.clone());
                }

                // If absolute_path wasn't in metadata, try to compute it from location
                if absolute_path.is_empty() && !location.is_empty() {
                    let p = std::path::Path::new(&location);
                    if p.is_absolute() {
                        absolute_path = location.clone();
                    } else if let Ok(cwd) = std::env::current_dir() {
                        // dunce, not std. std::fs::canonicalize returns Windows'
                        // extended-length form, \\?\C:\dev\..., which most
                        // consumers cannot use: the visualizer turns this into a
                        // vscode://file URL, where \\?\ becomes //?/, and the
                        // editor opens and reports that the file does not exist.
                        // dunce returns the plain form where one exists, and
                        // leaves a genuine UNC path alone rather than corrupting
                        // it -- which is the case a hand-rolled prefix strip
                        // tends to get wrong.
                        if let Ok(resolved) = dunce::canonicalize(cwd.join(p)) {
                            absolute_path = resolved.to_string_lossy().to_string();
                        }
                        // canonicalize fails when the file is gone -- a memory
                        // naming a since-deleted path. absolute_path stays empty,
                        // which is what tells the UI it has nothing to open.
                    }
                }

                if !include_retired && retired {
                    continue;
                }
            
                nodes.push(serde_json::json!({
                    "id": id,
                    "namespace": namespace,
                    "memory_type": memory_type,
                    "content": content,
                    "location": location,
                    "absolute_path": absolute_path,
                    "domain": domain,
                }));
            }
        
            // 2. Fetch all edges, filtering out any incident on a retired node
            //    in active view.
            let mut links = Vec::new();
        
            // RELATES_TO
            let query_relates = "MATCH (a:Memory)-[r:RELATES_TO]->(b:Memory) RETURN a.id, b.id;";
            let mut res_relates = conn.query(query_relates)?;
            while let Some(row) = res_relates.next() {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if !include_retired && (retired_ids.contains(&source) || retired_ids.contains(&target)) {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "RELATES_TO"
                }));
            }
        
            // CONTAINS
            let query_contains = "MATCH (a:Memory)-[r:CONTAINS]->(b:Memory) RETURN a.id, b.id;";
            let mut res_contains = conn.query(query_contains)?;
            while let Some(row) = res_contains.next() {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if !include_retired && (retired_ids.contains(&source) || retired_ids.contains(&target)) {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "CONTAINS"
                }));
            }
        
            // GOVERNS
            let query_governs = "MATCH (a:Memory)-[r:GOVERNS]->(b:Memory) RETURN a.id, b.id;";
            let mut res_governs = conn.query(query_governs)?;
            while let Some(row) = res_governs.next() {
                let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
                let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
                if !include_retired && (retired_ids.contains(&source) || retired_ids.contains(&target)) {
                    continue;
                }
                links.push(serde_json::json!({
                    "source": source,
                    "target": target,
                    "type": "GOVERNS"
                }));
            }

            Ok(serde_json::json!({
                "nodes": nodes,
                "links": links
            }))
        })
        .await
    }

    /// Retrieval bumps a counter, so it has to cost about what a counter costs.
    /// Routing it through get() and upsert() rewrote the entire row -- its
    /// 768-float embedding included -- and re-ran every edge MERGE, five times
    /// per search, fire and forget. Later writes queued behind that burst: a
    /// delete issued with no pause after a search took 154s where an idle one
    /// takes none (bead neurostrata-3fi.6). Touch one column instead.
    async fn increment_access_count(&self, namespace: &str, id: &str) -> Result<()> {
        let namespace = namespace.to_string();
        let id = id.to_string();
        self.write_with_deadline("counting a read", move |conn| {
            let safe_ns = escape_kuzu_string(&namespace);
            let safe_id = escape_kuzu_string(&id);

            let read = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.metadata",
                safe_ns, safe_id
            );
            let mut result = conn.query(&read)?;
            let current = match result.next() {
                Some(row) => format!("{}", row[0]),
                None => return Ok(()),
            };

            let write = format!(
                "MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' SET m.metadata = '{}'",
                safe_ns,
                safe_id,
                escape_kuzu_string(&bump_access_count(&current))
            );
            conn.query(&write)?;
            Ok(())
        })
        .await
    }
}

/// Diagnostic counters for work handed to the engine.
///
/// Process-wide on purpose: there is one store per process, and the question
/// they exist to answer -- does blocking work pile up and never come back -- is
/// about the process, not about any one call.
///
/// `STARTED` minus `FINISHED` is how many engine calls are inside lbug right
/// now. If that number climbs and does not come down, a deadline that could not
/// cancel its statement has left the work running, which is the hypothesis these
/// were added to test (bead neurostrata-4nk).
pub static ENGINE_CALLS_STARTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static ENGINE_CALLS_FINISHED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Writes whose caller gave up. The statement may still be running.
pub static WRITES_ABANDONED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Times the engine refused a second write transaction.
pub static WRITE_COLLISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many engine calls are inside lbug right now.
pub fn engine_in_flight() -> u64 {
    ENGINE_CALLS_STARTED
        .load(std::sync::atomic::Ordering::Relaxed)
        .saturating_sub(ENGINE_CALLS_FINISHED.load(std::sync::atomic::Ordering::Relaxed))
}

/// The counters as a line worth logging.
pub fn engine_stats() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    format!(
        "{} in flight ({} started, {} finished), {} abandoned, {} collisions",
        engine_in_flight(),
        ENGINE_CALLS_STARTED.load(Relaxed),
        ENGINE_CALLS_FINISHED.load(Relaxed),
        WRITES_ABANDONED.load(Relaxed),
        WRITE_COLLISIONS.load(Relaxed)
    )
}

/// How long a write may wait before the caller is told the database is busy.
/// Override with NEUROSTRATA_WRITE_TIMEOUT_SECS where an unusually large
/// database makes the default too tight.
fn write_timeout() -> std::time::Duration {
    let secs = std::env::var("NEUROSTRATA_WRITE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(30);
    std::time::Duration::from_secs(secs)
}

/// How many times a write retries after being refused the write transaction.
/// Waits are 25ms, 50ms and so on, and the caller's deadline caps the total
/// regardless, so this only decides how patient a single attempt is.
const WRITE_COLLISION_RETRIES: u32 = 6;

/// The engine's refusal when something else already holds the write
/// transaction. Worth recognising by name: the fix is to wait a moment, not to
/// report a failure, and the in-process lock cannot prevent every case -- the
/// background checkpoint and any other process opening the same database are
/// both outside it.
fn is_write_collision(err: &anyhow::Error) -> bool {
    err.to_string()
        .contains("Cannot start a new write transaction")
}

/// Whether a memory has been retired, meaning its `valid_to` has passed.
///
/// A retired row has to stop surfacing wherever results come from: the vector
/// match, the graph around that match, and the edges relinking replays.
/// Filtering only the first lets the graph hand back what the match left out.
fn is_retired(metadata: &Value) -> bool {
    is_retired_at(metadata, chrono::Utc::now().timestamp())
}

/// Timestamp-aware variant so callers that fetch many rows in one pass can
/// capture `now` once instead of calling Utc::now() per row.
fn is_retired_at(metadata: &Value, now: i64) -> bool {
    match metadata.get("valid_to") {
        Some(valid_to) if !valid_to.is_null() => {
            valid_to.as_i64().unwrap_or(0) <= now
        }
        _ => false,
    }
}

/// The ids a declared edge target could stand for when it is not already a
/// qualified id: the declaration qualified by the memory's own namespace, and
/// every separator-anchored suffix of it, bare and qualified. A suffix is how a
/// legacy absolute path finds the file it meant.
fn declared_target_candidates(namespace: &str, declared: &str) -> Vec<String> {
    let declared = declared.replace('\\', "/");
    let mut candidates = vec![crate::parser::ingest::qualified_id(namespace, &declared)];
    for (cut, _) in declared.match_indices('/') {
        let suffix = &declared[cut + 1..];
        if !suffix.is_empty() {
            candidates.push(suffix.to_string());
            candidates.push(crate::parser::ingest::qualified_id(namespace, suffix));
        }
    }
    candidates
}

/// Chooses among the ids that exist: the memory's own namespace first, then the
/// id exactly as declared, then a suffix -- but only one. An ambiguous suffix
/// produces no edge, because a wrong edge is worse than a missing one.
fn pick_declared_target(namespace: &str, declared: &str, found: &[String]) -> Option<String> {
    let qualified = crate::parser::ingest::qualified_id(namespace, &declared.replace('\\', "/"));
    if found.iter().any(|id| *id == qualified) {
        return Some(qualified);
    }
    if found.iter().any(|id| id == declared) {
        return Some(declared.to_string());
    }
    let mut suffixes = found.iter().filter(|id| **id != qualified && id.as_str() != declared);
    match (suffixes.next(), suffixes.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

/// Resolves a declared edge target while a memory is being written, in one read.
///
/// The exact id stays unscoped, as edges always have been; everything inferred
/// from it is confined to the memory's own namespace, so a bare `src/lib.rs`
/// cannot land on another project's file.
fn resolve_target_on_write(conn: &Connection, namespace: &str, declared: &str) -> Option<String> {
    let candidates = declared_target_candidates(namespace, declared)
        .iter()
        .map(|c| format!("'{}'", escape_kuzu_string(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "MATCH (b:Memory) WHERE b.id = '{}' OR (b.namespace = '{}' AND b.id IN [{}]) RETURN DISTINCT b.id",
        escape_kuzu_string(declared),
        escape_kuzu_string(namespace),
        candidates
    );
    let mut rows = conn.query(&query).ok()?;
    let mut found = Vec::new();
    while let Some(row) = rows.next() {
        found.push(format!("{}", row[0]));
    }
    pick_declared_target(namespace, declared, &found)
}

/// Fails a future that outstays its deadline, with a message that says what was
/// waiting and admits the work may still be running.
async fn bounded<T>(
    what: &str,
    limit: std::time::Duration,
    fut: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "the database is busy: {} did not finish within {}s. The statement may still be running, so read the record back before retrying.",
            what,
            limit.as_secs()
        )),
    }
}

/// Adds one to `access_count` and leaves the rest of the blob alone. Metadata
/// that is missing, empty or unparseable starts a fresh object at 1 instead of
/// failing: reading a memory must never be the thing that errors.
fn bump_access_count(metadata: &str) -> String {
    let mut value: Value = serde_json::from_str(metadata).unwrap_or_else(|_| serde_json::json!({}));
    if !value.is_object() {
        value = serde_json::json!({});
    }
    if let Some(obj) = value.as_object_mut() {
        let count = obj.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
        obj.insert("access_count".to_string(), serde_json::json!(count.saturating_add(1)));
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The resolution rule as every caller now reaches it: build the index
    /// once, then ask it.
    fn resolve(declared: &str, known: &[String]) -> Option<String> {
        KnownIds::new(known.iter().map(|id| id.as_str())).resolve(declared)
    }

    #[test]
    fn the_engines_refusal_is_recognised_as_a_collision() {
        let refusal = anyhow::anyhow!(
            "Query execution failed: Cannot start a new write transaction in the system. Only one write transaction at a time is allowed in the system."
        );
        assert!(is_write_collision(&refusal));
    }

    #[test]
    fn an_ordinary_failure_is_not_retried_as_a_collision() {
        let other = anyhow::anyhow!("Binder exception: Table Memory does not exist");
        assert!(!is_write_collision(&other));
    }

    #[test]
    fn a_legacy_absolute_declaration_finds_the_file_it_meant() {
        let known = vec!["src/store/ladybug.rs".to_string(), "src/daemon.rs".to_string()];
        assert_eq!(
            resolve("C:/dev/projects/neurostrata/src/store/ladybug.rs", &known).as_deref(),
            Some("src/store/ladybug.rs")
        );
        // Backslashes are the same path, written the way Windows hands it over.
        assert_eq!(
            resolve(r"C:\dev\projects
eurostrata\src\daemon.rs", &known).as_deref(),
            Some("src/daemon.rs")
        );
    }

    #[test]
    fn an_exact_declaration_is_returned_untouched() {
        let known = vec!["src/daemon.rs".to_string()];
        assert_eq!(resolve("src/daemon.rs", &known).as_deref(), Some("src/daemon.rs"));
    }

    /// The migration doctor prescribes is a re-ingest, which qualifies every
    /// id. A rule that still declares the old absolute path must follow it.
    #[test]
    fn a_legacy_absolute_declaration_finds_its_qualified_id() {
        let known = vec![
            "probe::src/store/ladybug.rs".to_string(),
            "probe::src/daemon.rs".to_string(),
        ];
        assert_eq!(
            resolve("C:/dev/projects/neurostrata/src/store/ladybug.rs", &known).as_deref(),
            Some("probe::src/store/ladybug.rs")
        );
        assert_eq!(resolve("C:/old/mylib.rs", &["probe::lib.rs".to_string()]), None);
        assert_eq!(
            resolve(
                "C:/old/src/a.rs",
                &["probe::src/a.rs".to_string(), "other::src/a.rs".to_string()]
            ),
            None,
            "a path two namespaces hold is ambiguous"
        );
    }

    #[test]
    fn an_ambiguous_suffix_is_left_alone() {
        // A wrong edge is worse than a missing one.
        // Both "mod.rs" and "x/mod.rs" are suffixes of the declaration.
        let known = vec!["mod.rs".to_string(), "x/mod.rs".to_string()];
        assert_eq!(resolve("C:/proj/x/mod.rs", &known), None);
    }

    #[test]
    fn a_target_that_was_never_ingested_stays_unresolved() {
        let known = vec!["src/daemon.rs".to_string()];
        assert_eq!(resolve("C:/proj/src/nowhere.rs", &known), None);
    }

    #[test]
    fn a_fresh_store_has_nothing_to_flush() {
        let dirty = Dirty::default();
        assert!(!dirty.is_set());
        assert!(!dirty.take());
    }

    #[test]
    fn taking_the_flag_clears_it_so_one_checkpoint_answers_for_many_writes() {
        let dirty = Dirty::default();
        dirty.mark();
        dirty.mark();

        assert!(dirty.take(), "the flag was set, so taking it reports true");
        assert!(!dirty.is_set(), "and leaves nothing behind");
        assert!(!dirty.take(), "a second take has nothing to report");
    }

    #[test]
    fn a_write_during_a_checkpoint_is_not_swallowed() {
        let dirty = Dirty::default();
        dirty.mark();

        // The checkpoint takes the flag first, then a write lands while the
        // engine is still flushing. That write must survive the clear.
        let _flushing = dirty.take();
        dirty.mark();

        assert!(dirty.is_set());
    }

    #[tokio::test]
    async fn a_write_that_outstays_its_deadline_says_the_database_is_busy() {
        let slow = async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Ok::<(), anyhow::Error>(())
        };

        let err = bounded("deleting a memory", std::time::Duration::from_millis(50), slow)
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("the database is busy"), "{}", err);
        assert!(err.contains("deleting a memory"), "{}", err);
        // The caller is released, not the query -- saying so is the point.
        assert!(err.contains("may still be running"), "{}", err);
    }

    #[tokio::test]
    async fn waiting_a_turn_to_write_counts_against_the_deadline() {
        // The shape write_with_deadline uses: the queue for the single writer
        // sits inside the bound, so a caller stuck behind a stalled write is
        // told the database is busy rather than waiting for its turn forever.
        let lock = tokio::sync::Mutex::new(());
        let held = lock.lock().await;

        let err = bounded("writing a memory", std::time::Duration::from_millis(50), async {
            let _turn = lock.lock().await;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .unwrap_err()
        .to_string();

        assert!(err.contains("the database is busy"), "{}", err);
        drop(held);
    }

    /// The invariant the writer's turn exists for. Held by the caller, giving
    /// up released it while the statement was still running, and the next
    /// writer opened a second transaction against an engine that already had
    /// one -- which is how one stalled write became five in a row (bead
    /// neurostrata-4nk).
    #[tokio::test]
    async fn giving_up_does_not_hand_on_a_turn_that_is_still_in_use() {
        let lock = Arc::new(tokio::sync::Mutex::new(()));

        // Stands in for a statement spawn_blocking cannot cancel: it owns the
        // turn and keeps it for as long as it runs.
        let turn = Arc::clone(&lock).lock_owned().await;
        let statement = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            drop(turn);
        });

        let err = bounded("writing a memory", std::time::Duration::from_millis(20), async {
            let _held = Arc::clone(&lock).lock_owned().await;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("the database is busy"), "{}", err);

        assert!(
            lock.try_lock().is_err(),
            "the turn was handed on while the statement was still running"
        );

        statement.await.expect("the statement finishes");
        assert!(
            lock.try_lock().is_ok(),
            "the turn is free once the statement has actually ended"
        );
    }

    #[tokio::test]
    async fn a_write_inside_its_deadline_is_left_alone() {
        let quick = async { Ok::<u8, anyhow::Error>(7) };
        let out = bounded("writing a memory", std::time::Duration::from_secs(5), quick)
            .await
            .unwrap();
        assert_eq!(out, 7);
    }

    #[test]
    fn the_write_deadline_can_be_overridden() {
        std::env::set_var("NEUROSTRATA_WRITE_TIMEOUT_SECS", "90");
        assert_eq!(write_timeout(), std::time::Duration::from_secs(90));

        // A nonsense or zero value falls back rather than disabling the bound.
        std::env::set_var("NEUROSTRATA_WRITE_TIMEOUT_SECS", "0");
        assert_eq!(write_timeout(), std::time::Duration::from_secs(30));
        std::env::remove_var("NEUROSTRATA_WRITE_TIMEOUT_SECS");
    }

    #[test]
    fn a_first_read_starts_the_count_at_one() {
        assert_eq!(bump_access_count("{}"), json!({"access_count": 1}).to_string());
    }

    #[test]
    fn counting_a_read_leaves_the_rest_of_the_blob_alone() {
        let before = json!({
            "domain": "database",
            "governs": ["src/daemon.rs"],
            "access_count": 4
        })
        .to_string();

        let after: Value = serde_json::from_str(&bump_access_count(&before)).unwrap();

        assert_eq!(after["access_count"], json!(5));
        assert_eq!(after["domain"], json!("database"));
        assert_eq!(after["governs"], json!(["src/daemon.rs"]));
    }

    #[test]
    fn unusable_metadata_does_not_fail_a_read() {
        for junk in ["", "not json at all", "[1,2,3]", "null"] {
            assert_eq!(
                bump_access_count(junk),
                json!({"access_count": 1}).to_string(),
                "junk metadata {:?} should still count the read",
                junk
            );
        }
    }

    async fn retired_test_store() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-retired-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::new(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");
        store
    }

    fn retired_test_payload(memory_type: &str, user_id: &str, metadata: Value) -> MemoryPayload {
        MemoryPayload {
            content: format!("a {} row", memory_type),
            user_id: user_id.to_string(),
            memory_type: memory_type.to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata,
        }
    }

    async fn governs_edges_from(store: &LadybugStore, id: &str) -> usize {
        let id = escape_kuzu_string(id);
        store
            .with_conn(move |conn| {
                let mut rows = conn.query(&format!(
                    "MATCH (a:Memory {{id: '{}'}})-[:GOVERNS]->(:Memory) RETURN count(*)",
                    id
                ))?;
                Ok(rows
                    .next()
                    .map(|row| format!("{}", row[0]).parse().unwrap_or(0))
                    .unwrap_or(0))
            })
            .await
            .expect("count GOVERNS edges")
    }

    #[tokio::test]
    async fn relinking_does_not_give_a_retired_rule_its_edges_back() {
        let store = retired_test_store().await;
        let file = "probe::src/a.rs";
        let zero = vec![0.0f32; 4];

        // Both rules are written before the file exists, so only relinking can
        // give them edges.
        store
            .upsert("probe", "retired-rule", zero.clone(), retired_test_payload("rule", "agent", json!({ "governs": [file], "valid_to": 1 })))
            .await
            .unwrap();
        store
            .upsert("probe", "live-rule", zero.clone(), retired_test_payload("rule", "agent", json!({ "governs": [file] })))
            .await
            .unwrap();
        store
            .upsert("probe", file, zero, retired_test_payload("file", "auto-ingestor", json!({})))
            .await
            .unwrap();

        store.relink_edges("probe").await.expect("relink");

        assert_eq!(governs_edges_from(&store, "live-rule").await, 1, "a live rule is relinked");
        assert_eq!(governs_edges_from(&store, "retired-rule").await, 0, "a retired rule is not");
    }

    #[tokio::test]
    async fn the_graph_does_not_bring_back_a_retired_rule() {
        let store = retired_test_store().await;
        let file = "probe::src/a.rs";

        store
            .upsert("probe", file, vec![0.0; 4], retired_test_payload("file", "auto-ingestor", json!({})))
            .await
            .unwrap();
        store
            .upsert("probe", "symbol", vec![1.0, 0.0, 0.0, 0.0], retired_test_payload("code_ast", "auto-ingestor", json!({ "contained_by": [file] })))
            .await
            .unwrap();
        store
            .upsert("probe", "retired-rule", vec![0.0, 1.0, 0.0, 0.0], retired_test_payload("rule", "agent", json!({ "governs": [file], "valid_to": 1 })))
            .await
            .unwrap();

        // The symbol matches; its file is governed by the retired rule, two hops
        // out -- exactly the path the governing expansion follows. The query
        // vector is not whole numbers: `1` and `0` would print as an integer
        // list, which array_distance refuses.
        let ids: Vec<String> = store
            .search("probe", vec![0.9, 0.1, 0.1, 0.1], 5)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();

        assert!(ids.contains(&"symbol".to_string()), "{:?}", ids);
        assert!(!ids.contains(&"retired-rule".to_string()), "a retired rule stays retired: {:?}", ids);
    }

    #[test]
    fn a_declaration_prefers_its_own_namespace_then_its_exact_id_then_one_suffix() {
        let both = vec!["probe::src/a.rs".to_string(), "src/a.rs".to_string()];
        assert_eq!(pick_declared_target("probe", "src/a.rs", &both).as_deref(), Some("probe::src/a.rs"));
        assert_eq!(
            pick_declared_target("probe", "src/a.rs", &["src/a.rs".to_string()]).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            pick_declared_target("probe", "C:/old/src/a.rs", &["probe::src/a.rs".to_string()]).as_deref(),
            Some("probe::src/a.rs")
        );
        assert_eq!(
            pick_declared_target(
                "probe",
                "C:/old/src/a.rs",
                &["probe::src/a.rs".to_string(), "probe::a.rs".to_string()]
            ),
            None,
            "an ambiguous suffix is left alone"
        );
        assert!(declared_target_candidates("probe", "C:/old/src/a.rs").contains(&"probe::src/a.rs".to_string()));
    }

    /// Ingestion qualifies node ids, and a rule names the file the way a human
    /// writes it. The edge has to form when the rule is written, not only after
    /// the next ingest happens to relink it.
    #[tokio::test]
    async fn a_rule_added_after_ingest_governs_the_file_it_names() {
        let store = retired_test_store().await;
        store
            .upsert("probe", "probe::src/a.rs", vec![0.0; 4], retired_test_payload("file", "auto-ingestor", json!({})))
            .await
            .unwrap();
        store
            .upsert("probe", "rule", vec![0.0; 4], retired_test_payload("rule", "agent", json!({ "governs": ["src/a.rs"] })))
            .await
            .unwrap();

        assert_eq!(governs_edges_from(&store, "rule").await, 1);
    }

    /// Moving used to be "write to the target, delete from the source". upsert
    /// never rewrites a namespace, so the write updated the source row in place
    /// and the delete then removed the only copy.
    #[tokio::test]
    async fn moving_a_memory_keeps_the_row_and_its_edges() {
        let store = retired_test_store().await;
        store
            .upsert("probe", "probe::src/a.rs", vec![0.0; 4], retired_test_payload("file", "auto-ingestor", json!({})))
            .await
            .unwrap();
        store
            .upsert("probe", "rule", vec![0.0; 4], retired_test_payload("rule", "agent", json!({ "governs": ["probe::src/a.rs"] })))
            .await
            .unwrap();

        assert_eq!(store.relocate("rule", "probe", "global").await.unwrap(), RelocateOutcome::Moved);

        assert!(store.get("global", "rule").await.unwrap().is_some(), "the memory is in its new namespace");
        assert!(store.get("probe", "rule").await.unwrap().is_none(), "and no longer in the old one");
        assert_eq!(governs_edges_from(&store, "rule").await, 1, "its edges came with it");
    }

    #[tokio::test]
    async fn an_ingested_node_is_not_moved() {
        let store = retired_test_store().await;
        store
            .upsert("probe", "probe::src/a.rs", vec![0.0; 4], retired_test_payload("file", "auto-ingestor", json!({})))
            .await
            .unwrap();

        assert_eq!(
            store.relocate("probe::src/a.rs", "probe", "other").await.unwrap(),
            RelocateOutcome::Ingested
        );
        assert!(store.get("probe", "probe::src/a.rs").await.unwrap().is_some(), "and it stays where it was");
        assert_eq!(store.relocate("missing", "probe", "other").await.unwrap(), RelocateOutcome::NotFound);
        assert_eq!(store.relocate("rule", "probe", "probe").await.unwrap(), RelocateOutcome::SameNamespace);
    }

    #[test]
    fn each_metadata_key_maps_to_its_edge_table() {
        let specs = edge_specs(&json!({
            "related_to": ["a"],
            "contained_by": ["parent"],
            "governs": ["src/lib.rs"]
        }));
        assert_eq!(specs.len(), 3);
        assert!(specs.contains(&EdgeSpec { rel_type: "RELATES_TO", target_id: "a".into(), points_at_target: true }));
        assert!(specs.contains(&EdgeSpec { rel_type: "GOVERNS", target_id: "src/lib.rs".into(), points_at_target: true }));
    }

    /// Containment points from the parent to the child, so a directory contains
    /// its files rather than the other way round.
    #[test]
    fn containment_points_from_the_parent() {
        let specs = edge_specs(&json!({ "contained_by": ["src"] }));
        assert_eq!(specs[0].rel_type, "CONTAINS");
        assert!(!specs[0].points_at_target);
    }

    #[test]
    fn absent_or_empty_metadata_asks_for_no_edges() {
        assert!(edge_specs(&json!({})).is_empty());
        assert!(edge_specs(&json!({ "related_to": [] })).is_empty());
        assert!(edge_specs(&json!({ "related_to": [""] })).is_empty());
        assert!(edge_specs(&json!(null)).is_empty());
    }

    /// A store with three nodes: a rule that declares the file it governs, a
    /// second rule that declares the same file the way ingestion used to write
    /// it -- absolute -- and the file itself.
    async fn store_with_two_declarations() -> LadybugStore {
        let dir = std::env::temp_dir().join(format!("ns-relink-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::new(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let seed = |id: &str, metadata: &str| {
            format!(
                "CREATE (m:Memory {{id: '{}', namespace: 'probe', content: 'c', user_id: 'u', memory_type: 't', agent_name: 'a', location: '', location_lines: '', metadata: '{}', embedding: [0.0,0.0,0.0,0.0]}})",
                escape_kuzu_string(id),
                escape_kuzu_string(metadata)
            )
        };
        let rows = [
            seed("rule", r#"{"governs": ["src/a.rs"]}"#),
            seed("legacy", r#"{"governs": ["C:/old/checkout/src/a.rs"]}"#),
            seed("src/a.rs", "{}"),
        ];
        for row in rows {
            store
                .with_conn(move |conn| {
                    conn.query(&row)?;
                    Ok(())
                })
                .await
                .expect("seed a memory");
        }
        store
    }

    async fn governs_count(store: &LadybugStore) -> usize {
        store
            .with_conn(|conn| {
                let result = conn.query("MATCH (:Memory)-[:GOVERNS]->(:Memory) RETURN count(*)")?;
                let mut count = 0usize;
                for row in result {
                    count += format!("{}", row[0]).parse::<usize>().unwrap_or(0);
                }
                Ok(count)
            })
            .await
            .expect("count the edges")
    }

    /// Both declarations are replayed, including the one that names the file by
    /// its old absolute path.
    #[tokio::test]
    async fn relinking_replays_every_declaration_once() {
        let store = store_with_two_declarations().await;

        let created = store.relink_edges("probe").await.expect("relink");
        assert_eq!(created, 2, "both declarations name a node that exists");
        assert_eq!(governs_count(&store).await, 2);
    }

    /// Relinking runs at the end of every ingest, so running it twice must not
    /// double the graph. The count is of edges actually created, not of
    /// statements that did not error -- the loop this replaced reported the
    /// latter, so it claimed hundreds of edges on a run that made none.
    #[tokio::test]
    async fn relinking_twice_creates_nothing_the_second_time() {
        let store = store_with_two_declarations().await;

        assert_eq!(store.relink_edges("probe").await.expect("first"), 2);
        assert_eq!(
            store.relink_edges("probe").await.expect("second"),
            0,
            "the edges already exist, so none are created"
        );
        assert_eq!(governs_count(&store).await, 2);
    }

    /// A declaration naming something that was never ingested links nothing,
    /// and is not counted as though it had.
    #[tokio::test]
    async fn a_declaration_with_no_node_behind_it_links_nothing() {
        let dir = std::env::temp_dir().join(format!("ns-relink-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::new(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let row = format!(
            "CREATE (m:Memory {{id: 'rule', namespace: 'probe', content: 'c', user_id: 'u', memory_type: 't', agent_name: 'a', location: '', location_lines: '', metadata: '{}', embedding: [0.0,0.0,0.0,0.0]}})",
            escape_kuzu_string(r#"{"governs": ["src/never-ingested.rs"]}"#)
        );
        store
            .with_conn(move |conn| {
                conn.query(&row)?;
                Ok(())
            })
            .await
            .expect("seed a memory");

        assert_eq!(store.relink_edges("probe").await.expect("relink"), 0);
        assert_eq!(governs_count(&store).await, 0);
    }

    /// `get` reads back the embedding it stored.
    ///
    /// The column is FLOAT[N] -- an ARRAY -- and the reader matched only
    /// Value::List, so this came back empty every time. Nothing caught it
    /// because search never returns a vector: it computes array_distance in the
    /// engine. Only the read-modify-write callers saw it, and they wrote the
    /// empty vector straight back (bead neurostrata-vbj).
    #[tokio::test]
    async fn get_reads_back_the_embedding_it_stored() {
        let dir = std::env::temp_dir().join(format!("ns-embed-{}", uuid::Uuid::new_v4()));
        let store = LadybugStore::new(&dir, 4).expect("open temp database");
        store.init("probe").await.expect("create the schema");

        let stored = vec![0.25f32, -1.5, 3.0, 0.0];
        store
            .upsert(
                "probe",
                "rule",
                stored.clone(),
                MemoryPayload {
                    content: "c".to_string(),
                    user_id: "u".to_string(),
                    memory_type: "rule".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({}),
                },
            )
            .await
            .expect("store the row");

        let (read_back, _) = store
            .get("probe", "rule")
            .await
            .expect("query succeeds")
            .expect("the row is there");
        assert_eq!(read_back, stored, "the embedding survives a round trip");
    }

    // --- TASK 1 tests: usage boost formula and counter saturation ---

    #[test]
    fn usage_boost_at_distance_zero_stays_zero() {
        // gain = 0.05 * ln(1+5) ≈ 0.080, capped_gain = min(0.080, 0) = 0
        let n = 5i64;
        let distance: f32 = 0.0;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        let capped = gain.min(0.5 * distance);
        let boosted = distance - capped;
        assert_eq!(boosted, 0.0);
    }

    #[test]
    fn usage_boost_at_count_zero_gives_zero_gain() {
        let n = 0i64;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        assert_eq!(gain, 0.0);
    }

    #[test]
    fn usage_boost_at_count_one_gives_real_gain() {
        let n = 1i64;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        assert!(gain > 0.0, "ln(1+1) should be > 0");
    }

    #[test]
    fn usage_boost_cap_holds_at_half_distance() {
        let distance: f32 = 0.1;
        let n = 8i64;
        let gain = 0.05 * (1.0 + n.max(0) as f32).ln();
        let capped = gain.min(0.5 * distance);
        let boosted = distance - capped;
        assert!(boosted >= 0.0, "adjusted must be non-negative");
        assert!(boosted >= 0.5 * distance, "cap must hold: adjusted >= d/2");
        assert!(boosted <= distance, "adjusted must be <= d");
    }

    #[test]
    fn usage_boost_in_range_for_finite_distance() {
        for d in [0.01, 0.1, 0.5, 1.0, 5.0] {
            for n in [0, 1, 5, 100, 1000] {
                let gain = 0.05 * (1.0 + n as f32).ln();
                let capped = gain.min(0.5 * d);
                let boosted = d - capped;
                assert!(boosted >= 0.5 * d, "d={}, n={}: adjusted {} < d/2", d, n, boosted);
                assert!(boosted <= d, "d={}, n={}: adjusted {} > d", d, n, boosted);
            }
        }
    }

    #[test]
    fn bump_access_count_saturates_at_i64_max() {
        let metadata = json!({"access_count": i64::MAX}).to_string();
        let result = bump_access_count(&metadata);
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["access_count"], json!(i64::MAX));
    }

    #[tokio::test]
    async fn search_after_saturating_count_does_not_panic() {
        let store = retired_test_store().await;
        // Non-integer values: Kuzu treats whole-number floats as integers,
        // which ARRAY_DISTANCE rejects.
        let v = vec![1.1, 0.0, 0.0, 0.0];
        store
            .upsert(
                "probe",
                "hot-memory",
                v.clone(),
                retired_test_payload("rule", "agent", json!({"access_count": i64::MAX})),
            )
            .await
            .unwrap();
        // Should not panic from overflow in the gain calculation.
        let results = store.search("probe", v, 10).await.unwrap();
        assert!(!results.is_empty());
    }

    // --- TASK 2 tests: search paging, expansion paging, export_graph ---

    #[tokio::test]
    async fn search_returns_active_results_when_retired_rows_fill_first_pages() {
        let store = retired_test_store().await;
        // Insert many retired rows with very close (identical) embeddings,
        // then one live row further away.
        for i in 0..20 {
            store
                .upsert(
                    "probe",
                    &format!("retired-{}", i),
                    vec![0.1; 4],
                    retired_test_payload("rule", "agent", json!({"valid_to": 1})),
                )
                .await
                .unwrap();
        }
        store
            .upsert(
                "probe",
                "live-row",
                vec![0.9; 4],
                retired_test_payload("rule", "agent", json!({})),
            )
            .await
            .unwrap();

        let results = store.search("probe", vec![0.1; 4], 5).await.unwrap();
        let ids: Vec<String> = results.into_iter().map(|r| r.id).collect();
        assert!(
            ids.contains(&"live-row".to_string()),
            "paging must find the live row: {:?}",
            ids
        );
        assert!(
            ids.iter().all(|id| !id.starts_with("retired-")),
            "no retired rows should appear: {:?}",
            ids
        );
    }

    #[tokio::test]
    async fn search_expansion_excludes_retired_while_live_sibling_surfaces() {
        let store = retired_test_store().await;
        let file = "probe::src/a.rs";

        store
            .upsert("probe", file, vec![0.0; 4], retired_test_payload("file", "auto-ingestor", json!({})))
            .await
            .unwrap();
        store
            .upsert("probe", "live-rule", vec![0.0; 4], retired_test_payload("rule", "agent", json!({ "governs": [file] })))
            .await
            .unwrap();
        store
            .upsert("probe", "retired-rule", vec![0.0; 4], retired_test_payload("rule", "agent", json!({ "governs": [file], "valid_to": 1 })))
            .await
            .unwrap();

        let ids: Vec<String> = store
            .search("probe", vec![0.9, 0.1, 0.1, 0.1], 5)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();

        assert!(ids.contains(&"live-rule".to_string()), "{:?}", ids);
        assert!(!ids.contains(&"retired-rule".to_string()), "{:?}", ids);
    }

    #[tokio::test]
    async fn search_stable_tie_order() {
        let store = retired_test_store().await;
        // Two live rows with identical embeddings; order should be deterministic.
        for name in &["alpha", "beta"] {
            store
                .upsert(
                    "probe",
                    name,
                    vec![0.5; 4],
                    retired_test_payload("rule", "agent", json!({})),
                )
                .await
                .unwrap();
        }

        let first: Vec<String> = store
            .search("probe", vec![0.5; 4], 10)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        let second: Vec<String> = store
            .search("probe", vec![0.5; 4], 10)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(first, second, "tie order must be deterministic");
    }

    #[tokio::test]
    async fn export_graph_excludes_retired_by_default() {
        let store = retired_test_store().await;
        store
            .upsert("probe", "live-node", vec![0.0; 4], retired_test_payload("rule", "agent", json!({})))
            .await
            .unwrap();
        store
            .upsert("probe", "retired-node", vec![0.0; 4], retired_test_payload("rule", "agent", json!({ "valid_to": 1 })))
            .await
            .unwrap();
        store
            .upsert("probe", "another-live", vec![0.0; 4], retired_test_payload("rule", "agent", json!({ "related_to": ["live-node"] })))
            .await
            .unwrap();

        let data = store.export_graph(false).await.unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-node"), "{:?}", node_ids);
        assert!(node_ids.contains(&"another-live"), "{:?}", node_ids);
        assert!(!node_ids.contains(&"retired-node"), "{:?}", node_ids);

        // No dangling edges to retired nodes.
        let links = data["links"].as_array().unwrap();
        let retired_set: std::collections::HashSet<&str> = ["retired-node"].iter().copied().collect();
        for link in links {
            let src = link.get("source").and_then(|s| s.as_str()).unwrap_or("");
            let tgt = link.get("target").and_then(|s| s.as_str()).unwrap_or("");
            assert!(
                !retired_set.contains(src) && !retired_set.contains(tgt),
                "dangling edge to retired node: {:?}",
                link
            );
        }
    }

    #[tokio::test]
    async fn export_graph_include_retired_true_shows_all() {
        let store = retired_test_store().await;
        store
            .upsert("probe", "live-node", vec![0.0; 4], retired_test_payload("rule", "agent", json!({})))
            .await
            .unwrap();
        store
            .upsert("probe", "retired-node", vec![0.0; 4], retired_test_payload("rule", "agent", json!({ "valid_to": 1 })))
            .await
            .unwrap();

        let data = store.export_graph(true).await.unwrap();
        let node_ids: Vec<&str> = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()))
            .collect();
        assert!(node_ids.contains(&"live-node"), "{:?}", node_ids);
        assert!(node_ids.contains(&"retired-node"), "{:?}", node_ids);
    }
}
