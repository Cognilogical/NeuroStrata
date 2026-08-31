use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

use lbug::{Connection, Database, SystemConfig};

use crate::traits::{MemoryPayload, SearchResult, VectorStore};

pub struct LadybugStore {
    #[allow(dead_code)]
    local_path: PathBuf,
    dimensions: usize,
    db: Arc<Database>,
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
        })
    }

    /// Gets a short-lived connection
    fn get_conn(&self) -> Result<Connection<'_>> {
        Ok(Connection::new(&self.db)?)
    }
}

/// One edge a memory asks for, read off its metadata.
#[derive(Debug, PartialEq)]
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

#[async_trait]
impl VectorStore for LadybugStore {
    async fn init(&self, _namespace: &str) -> Result<()> {
        let conn = self.get_conn()?;

        let create_node_table = format!(
            "CREATE NODE TABLE Memory (id STRING, namespace STRING, content STRING, user_id STRING, memory_type STRING, agent_name STRING, location STRING, location_lines STRING, metadata STRING, embedding FLOAT[{}], PRIMARY KEY (id))",
            self.dimensions
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
    }

    async fn upsert(
        &self,
        namespace: &str,
        id: &str,
        vector: Vec<f32>,
        payload: MemoryPayload,
    ) -> Result<()> {
        let conn = self.get_conn()?;

        let safe_id = escape_kuzu_string(id);
        let safe_ns = escape_kuzu_string(namespace);
        let safe_content = escape_kuzu_string(&payload.content);
        let safe_user_id = escape_kuzu_string(&payload.user_id);
        let safe_memory_type = escape_kuzu_string(&payload.memory_type);
        let safe_agent_name = escape_kuzu_string(&payload.agent_name.unwrap_or_else(|| "unknown".to_string()));
        let safe_location = escape_kuzu_string(&payload.location);
        let safe_location_lines = escape_kuzu_string(&payload.location_lines);
        let safe_metadata = escape_kuzu_string(&serde_json::to_string(&payload.metadata)?);
        
        let vec_str = format!("[{}]", vector.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(","));

        let insert_query = format!(
            "MERGE (m:Memory {{id: '{}'}})
             ON CREATE SET m.namespace = '{}', m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}
             ON MATCH SET m.namespace = '{}', m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}",
            safe_id, safe_ns, safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str,
            safe_ns, safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str
        );

        conn.query(&insert_query)?;
        
        // Materialise the edges this memory declares. A target that does not exist
        // yet simply produces no edge: MATCH finds nothing and MERGE never runs.
        // That is deliberate -- a rule may name a file the ingester has not reached.
        for edge in edge_specs(&payload.metadata) {
            let target_safe = escape_kuzu_string(&edge.target_id);
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
    }

    async fn search(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let conn = self.get_conn()?;
        let safe_ns = escape_kuzu_string(namespace);
        let vec_str = format!("[{}]", vector.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(","));

        // Step 1: Base vector search.
        //
        // Structural nodes are stored with an all-zero embedding, and the distance
        // from an all-zero row is the query vector's own magnitude -- the same value
        // for every one of them, whatever was asked. Left in, they tie with each
        // other and can fill the limit with path stubs. They stay reachable through
        // the graph expansion in step 2, which is where they are actually useful.
        let structural_types = STRUCTURAL_MEMORY_TYPES
            .iter()
            .map(|t| format!("'{}'", t))
            .collect::<Vec<_>>()
            .join(", ");
        let search_query = format!(
            "MATCH (m:Memory) WHERE m.namespace = '{}' AND NOT m.memory_type IN [{}] RETURN m.id, array_distance(m.embedding, {}) AS dist, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata ORDER BY dist ASC LIMIT {}",
            safe_ns, structural_types, vec_str, limit
        );

        let result = conn.query(&search_query)?;
        let mut results = Vec::new();
        let mut primary_ids = Vec::new();

        for row in result {
            let id: String = format!("{}", row[0]);
            let distance: f32 = match &row[1] {
                lbug::Value::Float(f) => *f,
                lbug::Value::Double(d) => *d as f32,
                _ => 0.0,
            };
            
            let content: String = format!("{}", row[2]);
            let user_id: String = format!("{}", row[3]);
            let memory_type: String = format!("{}", row[4]);
            let agent_name: String = format!("{}", row[5]);
            let location: String = format!("{}", row[6]);
            let location_lines: String = format!("{}", row[7]);
            let metadata_str: String = format!("{}", row[8]);

            let metadata_val: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Null);

            // Temporal filtering
            if let Some(valid_to) = metadata_val.get("valid_to") {
                if !valid_to.is_null() {
                    let now = chrono::Utc::now().timestamp();
                    if valid_to.as_i64().unwrap_or(0) <= now {
                        continue;
                    }
                }
            }

            let access_count = metadata_val.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
            let gain = if access_count > 0 { (access_count as f32).ln() * 0.05 } else { 0.0 };
            let boosted_distance = distance - gain;

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
        }

        // Step 2: Hybrid GraphRAG Neighborhood Fetch
        // We fetch 1-hop neighbors (CONTAINS, GOVERNS, RELATES_TO) to provide blast radius context
        if !primary_ids.is_empty() {
            let id_list = primary_ids.iter()
                .map(|id| format!("'{}'", escape_kuzu_string(id)))
                .collect::<Vec<_>>()
                .join(", ");

            // Anything one hop from a primary match.
            let neighbor_query = format!(
                "MATCH (a:Memory)-[]-(b:Memory) WHERE a.id IN [{}] AND b.namespace = '{}' AND NOT b.id IN [{}] RETURN DISTINCT b.id, b.content, b.user_id, b.memory_type, b.agent_name, b.location, b.location_lines, b.metadata LIMIT {}",
                id_list, safe_ns, id_list, limit
            );

            // And one step further, but only along GOVERNS. A match is usually a
            // symbol, whose file is one hop away, which puts the rule governing
            // that file two hops out -- exactly the thing worth surfacing, and
            // unreachable from the query above.
            let governing_query = format!(
                "MATCH (a:Memory)-[]-(f:Memory)<-[:GOVERNS]-(r:Memory) WHERE a.id IN [{}] AND r.namespace = '{}' AND NOT r.id IN [{}] RETURN DISTINCT r.id, r.content, r.user_id, r.memory_type, r.agent_name, r.location, r.location_lines, r.metadata LIMIT {}",
                id_list, safe_ns, id_list, limit
            );

            for expansion in [neighbor_query, governing_query] {
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

                        // Neighbors get a synthesized lower score (worse distance) so they appear after primary matches,
                        // but still within the context window.
                        results.push(SearchResult {
                            id,
                            score: 10.0, // High distance (low relevance score) ensures they rank below direct matches
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
                    }
                }
            }

            // Both expansions can return the same memory; keep the first of each.
            let mut seen = std::collections::HashSet::new();
            results.retain(|r| seen.insert(r.id.clone()));
        }

        results.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
        // We can truncate to a slightly larger limit to include some neighbors, or keep it strict
        results.truncate(limit * 2);

        Ok(results)
    }

    async fn delete(&self, namespace: &str, id: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let safe_ns = escape_kuzu_string(namespace);
        let safe_id = escape_kuzu_string(id);
        
        let query = format!("MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' DETACH DELETE m", safe_id, safe_ns);
        conn.query(&query)?;
        Ok(())
    }

    async fn export_database(&self, dir: &str) -> Result<()> {
        let conn = self.get_conn()?;
        // Checkpoint first so the export cannot miss writes still in the WAL --
        // which, since replay does not restore rows, would otherwise be lost.
        conn.query("CHECKPOINT")?;
        let safe_dir = escape_kuzu_string(dir);
        conn.query(&format!("EXPORT DATABASE '{}' (format='parquet')", safe_dir))?;
        Ok(())
    }

    async fn import_database(&self, dir: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let safe_dir = escape_kuzu_string(dir);
        conn.query(&format!("IMPORT DATABASE '{}'", safe_dir))?;
        conn.query("CHECKPOINT")?;
        Ok(())
    }

    async fn checkpoint(&self) -> Result<()> {
        let conn = self.get_conn()?;
        conn.query("CHECKPOINT")?;
        Ok(())
    }

    async fn clear_ingested(&self, namespace: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let safe_ns = escape_kuzu_string(namespace);

        // Every row the ingester owns: the AST symbols and the directory/file
        // nodes too, since ingestion rebuilds the whole structure for a namespace
        let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = 'auto-ingestor' DETACH DELETE m", safe_ns);
        conn.query(&query)?;
        Ok(())
    }

    async fn list(&self, namespace: &str, user_id: Option<&str>) -> Result<Vec<SearchResult>> {
        let conn = self.get_conn()?;
        let safe_ns = escape_kuzu_string(namespace);
        
        let query = if let Some(uid) = user_id {
            format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", safe_ns, escape_kuzu_string(uid))
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
    }

    async fn get(&self, namespace: &str, id: &str) -> Result<Option<(Vec<f32>, MemoryPayload)>> {
        let conn = self.get_conn()?;
        let safe_ns = escape_kuzu_string(namespace);
        let safe_id = escape_kuzu_string(id);

        let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.embedding, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", safe_ns, safe_id);
        
        let mut result = conn.query(&query)?;
        
        if let Some(row) = result.next() {
            let mut vec: Vec<f32> = Vec::new();
            if let lbug::Value::List(_, list_vals) = &row[0] {
                for v in list_vals {
                    if let lbug::Value::Float(f) = v {
                        vec.push(*f);
                    } else if let lbug::Value::Double(d) = v {
                        vec.push(*d as f32);
                    }
                }
            }
            
            let mut content: String = format!("{}", row[1]);
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
    }

    async fn list_namespaces(&self) -> Result<Vec<String>> {
        let conn = Connection::new(&self.db)?;
        let query = "MATCH (m:Memory) RETURN DISTINCT m.namespace AS ns;";
        let mut result = conn.query(query)?;
        
        let mut namespaces = Vec::new();
        while let Some(row) = result.next() {
            if let lbug::Value::String(ns) = row[0].clone() {
                namespaces.push(ns);
            }
        }
        
        Ok(namespaces)
    }

    async fn export_graph(&self) -> Result<serde_json::Value> {
        let conn = Connection::new(&self.db)?;
        
        // 1. Fetch all nodes
        let mut nodes = Vec::new();
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

            if let lbug::Value::String(metadata_str) = &row[5] {
                if let Ok(metadata_val) = serde_json::from_str::<serde_json::Value>(metadata_str) {
                    if let Some(abs_path) = metadata_val.get("absolute_path").and_then(|v| v.as_str()) {
                        absolute_path = abs_path.to_string();
                    }
                    if let Some(d) = metadata_val.get("domain").and_then(|v| v.as_str()) {
                        domain = Some(d.to_string());
                    }
                }
            }

            // If absolute_path wasn't in metadata, try to compute it from location
            if absolute_path.is_empty() && !location.is_empty() {
                let p = std::path::Path::new(&location);
                if p.is_absolute() {
                    absolute_path = location.clone();
                } else if let Ok(cwd) = std::env::current_dir() {
                    absolute_path = cwd.join(p).canonicalize().unwrap_or_default().to_string_lossy().to_string();
                }
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
        
        // 2. Fetch all edges
        let mut links = Vec::new();
        
        // RELATES_TO
        let query_relates = "MATCH (a:Memory)-[r:RELATES_TO]->(b:Memory) RETURN a.id, b.id;";
        let mut res_relates = conn.query(query_relates)?;
        while let Some(row) = res_relates.next() {
            let source = if let lbug::Value::String(s) = &row[0] { s.clone() } else { continue };
            let target = if let lbug::Value::String(s) = &row[1] { s.clone() } else { continue };
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
    }

    async fn increment_access_count(&self, namespace: &str, id: &str) -> Result<()> {
        if let Some((vector, mut payload)) = self.get(namespace, id).await? {
            let count = payload.metadata.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
            if let Some(obj) = payload.metadata.as_object_mut() {
                obj.insert("access_count".to_string(), serde_json::json!(count + 1));
            } else {
                let mut obj = serde_json::Map::new();
                obj.insert("access_count".to_string(), serde_json::json!(count + 1));
                payload.metadata = Value::Object(obj);
            }
            self.upsert(namespace, id, vector, payload).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
