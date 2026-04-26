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

        // Use array_distance for distance metric
        let search_query = format!(
            "MATCH (m:Memory) WHERE m.namespace = '{}' RETURN m.id, array_distance(m.embedding, {}) AS dist, m.content, m.user_id, m.memory_type, m.agent_name, m.location, m.location_lines, m.metadata ORDER BY dist ASC LIMIT {}",
            safe_ns, vec_str, limit
        );

        let result = conn.query(&search_query)?;
        let mut results = Vec::new();

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

        results.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

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
            
            let content: String = format!("{}", row[1]);
            let uid: String = format!("{}", row[2]);
            let memory_type: String = format!("{}", row[3]);
            let agent_name: String = format!("{}", row[4]);
            let location: String = format!("{}", row[5]);
            let location_lines: String = format!("{}", row[6]);
            let metadata_str: String = format!("{}", row[7]);

            let metadata_val: Value = serde_json::from_str(&metadata_str).unwrap_or(Value::Null);

            let payload = MemoryPayload {
                content,
                user_id: uid,
                memory_type,
                agent_name: Some(agent_name),
                location,
                location_lines,
                metadata: metadata_val,
            };

            return Ok(Some((vec, payload)));
        }

        Ok(None)
    }

    async fn list_namespaces(&self) -> Result<Vec<String>> {
        let conn = self.get_conn()?;
        let query = "MATCH (m:Memory) RETURN DISTINCT m.namespace";
        
        let result = conn.query(query)?;
        let mut namespaces = Vec::new();

        for row in result {
            let ns: String = format!("{}", row[0]);
            namespaces.push(ns);
        }

        Ok(namespaces)
    }
}
