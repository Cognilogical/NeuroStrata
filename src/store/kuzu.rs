use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

use kuzu::{Connection, Database, SystemConfig};

use crate::traits::{MemoryPayload, SearchResult, VectorStore};

pub struct KuzuStore {
    local_path: PathBuf,
    dimensions: usize,
    db: Arc<Database>,
}

impl KuzuStore {
    pub fn new(local_path: impl Into<PathBuf>, dimensions: usize) -> Result<Self> {
        let local_path = local_path.into();

        // We initialize the embedded Kuzu database once, and keep it in Arc to spawn connections from it
        let db = Database::new(&local_path, SystemConfig::default())?;

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

#[async_trait]
impl VectorStore for KuzuStore {
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

        let safe_id = id.replace("'", "\\'");
        let safe_ns = namespace.replace("'", "\\'");
        let safe_content = payload.content.replace("'", "\\'");
        let safe_user_id = payload.user_id.replace("'", "\\'");
        let safe_memory_type = payload.memory_type.replace("'", "\\'");
        let safe_agent_name = payload.agent_name.unwrap_or_else(|| "unknown".to_string()).replace("'", "\\'");
        let safe_location = payload.location.replace("'", "\\'");
        let safe_location_lines = payload.location_lines.replace("'", "\\'");
        let safe_metadata = serde_json::to_string(&payload.metadata)?.replace("'", "\\'");
        
        let vec_str = format!("[{}]", vector.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(","));

        let insert_query = format!(
            "MERGE (m:Memory {{id: '{}'}})
             ON CREATE SET m.namespace = '{}', m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}
             ON MATCH SET m.namespace = '{}', m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}",
            safe_id, safe_ns, safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str,
            safe_ns, safe_content, safe_user_id, safe_memory_type, safe_agent_name, safe_location, safe_location_lines, safe_metadata, vec_str
        );

        conn.query(&insert_query)?;
        
        // Edge linking logic (if related_to is present)
        if let Some(related) = payload.metadata.get("related_to").and_then(|r| r.as_array()) {
            for rel in related {
                if let Some(rel_id) = rel.as_str() {
                    let rel_id_safe = rel_id.replace("'", "\\'");
                    let edge_query = format!(
                        "MATCH (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}}) MERGE (a)-[:RELATES_TO]->(b)",
                        safe_id, rel_id_safe
                    );
                    conn.query(&edge_query).ok();
                }
            }
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
        let safe_ns = namespace.replace("'", "\\'");
        let vec_str = format!("[{}]", vector.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(","));

        // Step 1: Base vector search
        let search_query = format!(
            "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, array_distance(m.embedding, {}) AS dist, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata ORDER BY dist ASC LIMIT {}",
            safe_ns, vec_str, limit
        );

        let result = conn.query(&search_query)?;
        let mut results = Vec::new();
        let mut primary_ids = Vec::new();

        for row in result {
            let id: String = format!("{}", row[0]);
            let distance: f32 = match &row[1] {
                kuzu::Value::Float(f) => *f,
                kuzu::Value::Double(d) => *d as f32,
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
            if metadata_val.get("valid_to").is_some() && !metadata_val["valid_to"].is_null() {
                continue;
            }

            let access_count = metadata_val.get("access_count").and_then(|v| v.as_i64()).unwrap_or(0);
            let boosted_distance = distance - (access_count as f32 * 0.05);

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
                .map(|id| format!("'{}'", id.replace("'", "\\'")))
                .collect::<Vec<_>>()
                .join(", ");

            // Query any neighbors connected to our primary matches
            let neighbor_query = format!(
                "MATCH (a:Memory)-[]-(b:Memory) WHERE a.id IN [{}] AND b.namespace = '{}' AND NOT b.id IN [{}] RETURN DISTINCT b.id, b.content, b.user_id, b.memory_type, b.agent_name, b.location, b.location_lines, b.metadata LIMIT {}",
                id_list, safe_ns, id_list, limit
            );

            if let Ok(mut neighbor_result) = conn.query(&neighbor_query) {
                while let Some(row) = neighbor_result.next() {
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

        results.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
        // We can truncate to a slightly larger limit to include some neighbors, or keep it strict
        results.truncate(limit * 2);

        Ok(results)
    }

    async fn delete(&self, namespace: &str, id: &str) -> Result<()> {
        let conn = self.get_conn()?;
        let safe_ns = namespace.replace("'", "\\'");
        let safe_id = id.replace("'", "\\'");
        
        let query = format!("MATCH (m:Memory) WHERE m.id = '{}' AND m.namespace = '{}' DETACH DELETE m", safe_id, safe_ns);
        conn.query(&query)?;
        Ok(())
    }

    async fn list(&self, namespace: &str, user_id: Option<&str>) -> Result<Vec<SearchResult>> {
        let conn = self.get_conn()?;
        let safe_ns = namespace.replace("'", "\\'");
        
        let query = if let Some(uid) = user_id {
            format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.user_id = '{}' RETURN m.id, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", safe_ns, uid.replace("'", "\\'"))
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
        let safe_ns = namespace.replace("'", "\\'");
        let safe_id = id.replace("'", "\\'");

        let query = format!("MATCH (m:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN m.embedding, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata", safe_ns, safe_id);
        
        let mut result = conn.query(&query)?;
        
        if let Some(row) = result.next() {
            let mut vec: Vec<f32> = Vec::new();
            if let kuzu::Value::List(_, list_vals) = &row[0] {
                for v in list_vals {
                    if let kuzu::Value::Float(f) = v {
                        vec.push(*f);
                    } else if let kuzu::Value::Double(d) = v {
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

            // Fetch graph neighborhood (AST upward inheritance, blast radius)
            let neighbor_query = format!(
                "MATCH (m:Memory)-[r]-(n:Memory) WHERE m.namespace = '{}' AND m.id = '{}' RETURN type(r), n.content LIMIT 5",
                safe_ns, safe_id
            );

            if let Ok(mut neighbor_res) = conn.query(&neighbor_query) {
                let mut context_added = false;
                while let Some(n_row) = neighbor_res.next() {
                    if !context_added {
                        content.push_str("\n\n--- Graph Context (Neighborhood) ---\n");
                        context_added = true;
                    }
                    let rel_type = format!("{}", n_row[0]);
                    let n_content = format!("{}", n_row[1]);
                    content.push_str(&format!("\n[{}] Neighbor:\n{}\n", rel_type, n_content));
                }
            }

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
            if let kuzu::Value::String(ns) = row[0].clone() {
                namespaces.push(ns);
            }
        }
        
        Ok(namespaces)
    }

    async fn export_graph(&self) -> Result<serde_json::Value> {
        let conn = Connection::new(&self.db)?;
        
        // 1. Fetch all nodes
        let mut nodes = Vec::new();
        let query_nodes = "MATCH (n:Memory) RETURN n.id, n.namespace, n.memory_type, n.content, n.domain;";
        let mut result_nodes = conn.query(query_nodes)?;
        
        while let Some(row) = result_nodes.next() {
            let id = if let kuzu::Value::String(s) = &row[0] { s.clone() } else { continue };
            let namespace = if let kuzu::Value::String(s) = &row[1] { s.clone() } else { "global".to_string() };
            let memory_type = if let kuzu::Value::String(s) = &row[2] { s.clone() } else { "unknown".to_string() };
            let content = if let kuzu::Value::String(s) = &row[3] { s.clone() } else { "".to_string() };
            let domain = if let kuzu::Value::String(s) = &row[4] { Some(s.clone()) } else { None };
            
            nodes.push(serde_json::json!({
                "id": id,
                "namespace": namespace,
                "memory_type": memory_type,
                "content": content,
                "domain": domain,
            }));
        }
        
        // 2. Fetch all edges
        let mut links = Vec::new();
        
        // RELATES_TO
        let query_relates = "MATCH (a:Memory)-[r:RELATES_TO]->(b:Memory) RETURN a.id, b.id;";
        let mut res_relates = conn.query(query_relates)?;
        while let Some(row) = res_relates.next() {
            let source = if let kuzu::Value::String(s) = &row[0] { s.clone() } else { continue };
            let target = if let kuzu::Value::String(s) = &row[1] { s.clone() } else { continue };
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
            let source = if let kuzu::Value::String(s) = &row[0] { s.clone() } else { continue };
            let target = if let kuzu::Value::String(s) = &row[1] { s.clone() } else { continue };
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
            let source = if let kuzu::Value::String(s) = &row[0] { s.clone() } else { continue };
            let target = if let kuzu::Value::String(s) = &row[1] { s.clone() } else { continue };
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
}
