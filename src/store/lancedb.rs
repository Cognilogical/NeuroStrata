use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

use arrow::array::{FixedSizeListArray, Float32Array, RecordBatch, RecordBatchReader, StringArray};
use arrow::datatypes::{DataType, Field, Schema as ArrowSchema};
use arrow::record_batch::RecordBatchIterator;
use futures::StreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::traits::{MemoryPayload, SearchResult, VectorStore};

pub struct LanceDBStore {
    local_path: PathBuf,
    dimensions: usize,
}
impl LanceDBStore {
    pub fn new(local_path: impl Into<PathBuf>, dimensions: usize) -> Result<Self> {
        let local_path = local_path.into();
        if !local_path.exists() {
            std::fs::create_dir_all(&local_path)?;
        }

        Ok(Self {
            local_path,
            dimensions,
        })
    }

    fn schema(&self) -> Arc<ArrowSchema> {
        Arc::new(ArrowSchema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    self.dimensions as i32,
                ),
                false,
            ),
            Field::new("content", DataType::Utf8, false),
            Field::new("user_id", DataType::Utf8, false),
            Field::new("memory_type", DataType::Utf8, false),
            Field::new("agent_name", DataType::Utf8, false),
            Field::new("location", DataType::Utf8, false),
            Field::new("location_lines", DataType::Utf8, false),
            Field::new("metadata", DataType::Utf8, false),
        ]))
    }
}
#[async_trait]
impl VectorStore for LanceDBStore {
    async fn init(&self, namespace: &str) -> Result<()> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap())
            .execute()
            .await?;

        // Check if table exists, if not create empty table
        let table_names = conn.table_names().execute().await?;
        if !table_names.contains(&namespace.to_string()) {
            let schema = self.schema();
            let empty_batch = RecordBatch::new_empty(schema.clone());
            let batches = vec![empty_batch];
            let batches_iter = Box::new(RecordBatchIterator::new(
                batches.into_iter().map(Ok),
                schema.clone(),
            )) as Box<dyn RecordBatchReader + Send>;
            conn.create_table(namespace, batches_iter).execute().await?;
        }
        Ok(())
    }

    async fn upsert(
        &self,
        namespace: &str,
        id: &str,
        vector: Vec<f32>,
        payload: MemoryPayload,
    ) -> Result<()> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap())
            .execute()
            .await?;
        let table = conn.open_table(namespace).execute().await?;

        let schema = self.schema();
        let id_array = Arc::new(StringArray::from(vec![id]));

        let vector_list = Arc::new(FixedSizeListArray::try_new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            self.dimensions as i32,
            Arc::new(Float32Array::from(vector)),
            None,
        )?);

        let content_array = Arc::new(StringArray::from(vec![payload.content.as_str()]));
        let user_id_array = Arc::new(StringArray::from(vec![payload.user_id.as_str()]));
        let memory_type_array = Arc::new(StringArray::from(vec![payload.memory_type.as_str()]));
        let agent_name_str = payload.agent_name.unwrap_or_else(|| "unknown".to_string());
        let agent_name_array = Arc::new(StringArray::from(vec![agent_name_str.as_str()]));
        let location_array = Arc::new(StringArray::from(vec![payload.location.as_str()]));
        let location_lines_array =
            Arc::new(StringArray::from(vec![payload.location_lines.as_str()]));
        let metadata_str = serde_json::to_string(&payload.metadata)?;
        let metadata_array = Arc::new(StringArray::from(vec![metadata_str.as_str()]));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                id_array,
                vector_list,
                content_array,
                user_id_array,
                memory_type_array,
                agent_name_array,
                location_array,
                location_lines_array,
                metadata_array,
            ],
        )?;

        let batches_iter = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema.clone()))
            as Box<dyn RecordBatchReader + Send>;

        table.delete(format!("id = '{}'", id).as_str()).await.ok();
        table.add(batches_iter).execute().await?;

        Ok(())
    }

    async fn search(
        &self,
        namespace: &str,
        vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap())
            .execute()
            .await?;
        let table = conn.open_table(namespace).execute().await?;

        let mut stream = table.vector_search(vector)?.limit(limit).execute().await?;

        let mut results = Vec::new();

        while let Some(batch) = stream.next().await {
            let batch: RecordBatch = batch?;
            let ids = batch
                .column_by_name("id")
                .ok_or_else(|| anyhow!("Missing id column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let distances = batch
                .column_by_name("_distance")
                .ok_or_else(|| anyhow!("Missing _distance column"))?
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap();
            let contents = batch
                .column_by_name("content")
                .ok_or_else(|| anyhow!("Missing content column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let user_ids = batch
                .column_by_name("user_id")
                .ok_or_else(|| anyhow!("Missing user_id column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let memory_types = batch
                .column_by_name("memory_type")
                .ok_or_else(|| anyhow!("Missing memory_type column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let agent_names = batch
                .column_by_name("agent_name")
                .ok_or_else(|| anyhow!("Missing agent_name column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let locations = batch
                .column_by_name("location")
                .ok_or_else(|| anyhow!("Missing location column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let location_lines = batch
                .column_by_name("location_lines")
                .ok_or_else(|| anyhow!("Missing location_lines column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let metadatas = batch
                .column_by_name("metadata")
                .ok_or_else(|| anyhow!("Missing metadata column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();

            for i in 0..batch.num_rows() {
                let metadata_val: Value =
                    serde_json::from_str(metadatas.value(i)).unwrap_or(Value::Null);

                // Temporal filtering (Bi-Temporal Memory)
                if metadata_val.get("valid_to").is_some() && !metadata_val["valid_to"].is_null() {
                    continue; // Skip soft-deleted memories
                }

                // Neural Gain Mechanism (Access-Based Synaptic Weighting)
                let access_count = metadata_val
                    .get("access_count")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let base_distance = distances.value(i);
                // LanceDB distance (L2): lower is better. We subtract a boost based on access_count.
                let boosted_distance = base_distance - (access_count as f32 * 0.05);

                results.push(SearchResult {
                    id: ids.value(i).to_string(),
                    score: boosted_distance,
                    payload: MemoryPayload {
                        content: contents.value(i).to_string(),
                        user_id: user_ids.value(i).to_string(),
                        memory_type: memory_types.value(i).to_string(),
                        agent_name: Some(agent_names.value(i).to_string()),
                        location: locations.value(i).to_string(),
                        location_lines: location_lines.value(i).to_string(),
                        metadata: metadata_val,
                    },
                });
            }
        }

        // Sort by boosted score and truncate to limit
        results.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);

        Ok(results)
    }

    async fn delete(&self, namespace: &str, id: &str) -> Result<()> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap())
            .execute()
            .await?;
        let table = conn.open_table(namespace).execute().await?;
        table.delete(format!("id = '{}'", id).as_str()).await?;
        Ok(())
    }

    async fn list(&self, namespace: &str, user_id: Option<&str>) -> Result<Vec<SearchResult>> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap())
            .execute()
            .await?;
        let table = conn.open_table(namespace).execute().await?;

        let mut query = table.query();
        if let Some(uid) = user_id {
            query = query.only_if(format!("user_id = '{}'", uid));
        }

        let mut stream = query.execute().await?;
        let mut results = Vec::new();

        while let Some(batch) = stream.next().await {
            let batch: RecordBatch = batch?;
            let ids = batch
                .column_by_name("id")
                .ok_or_else(|| anyhow!("Missing id column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let contents = batch
                .column_by_name("content")
                .ok_or_else(|| anyhow!("Missing content column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let user_ids = batch
                .column_by_name("user_id")
                .ok_or_else(|| anyhow!("Missing user_id column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let memory_types = batch
                .column_by_name("memory_type")
                .ok_or_else(|| anyhow!("Missing memory_type column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let agent_names = batch
                .column_by_name("agent_name")
                .ok_or_else(|| anyhow!("Missing agent_name column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let locations = batch
                .column_by_name("location")
                .ok_or_else(|| anyhow!("Missing location column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let location_lines = batch
                .column_by_name("location_lines")
                .ok_or_else(|| anyhow!("Missing location_lines column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let metadatas = batch
                .column_by_name("metadata")
                .ok_or_else(|| anyhow!("Missing metadata column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();

            for i in 0..batch.num_rows() {
                let metadata_val: Value =
                    serde_json::from_str(metadatas.value(i)).unwrap_or(Value::Null);
                results.push(SearchResult {
                    id: ids.value(i).to_string(),
                    score: 0.0, // Score not applicable for list
                    payload: MemoryPayload {
                        content: contents.value(i).to_string(),
                        user_id: user_ids.value(i).to_string(),
                        memory_type: memory_types.value(i).to_string(),
                        agent_name: Some(agent_names.value(i).to_string()),
                        location: locations.value(i).to_string(),
                        location_lines: location_lines.value(i).to_string(),
                        metadata: metadata_val,
                    },
                });
            }
        }

        Ok(results)
    }

    async fn get(&self, namespace: &str, id: &str) -> Result<Option<(Vec<f32>, MemoryPayload)>> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap())
            .execute()
            .await?;
        let table_names = conn.table_names().execute().await?;
        if !table_names.contains(&namespace.to_string()) {
            return Ok(None);
        }

        let table = conn.open_table(namespace).execute().await?;
        let mut query = table.query();
        query = query.only_if(format!("id = '{}'", id));
        let mut stream = query.execute().await?;

        if let Some(batch) = stream.next().await {
            let batch: RecordBatch = batch?;
            if batch.num_rows() == 0 {
                return Ok(None);
            }
            let contents = batch
                .column_by_name("content")
                .ok_or_else(|| anyhow!("Missing content column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let user_ids = batch
                .column_by_name("user_id")
                .ok_or_else(|| anyhow!("Missing user_id column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let memory_types = batch
                .column_by_name("memory_type")
                .ok_or_else(|| anyhow!("Missing memory_type column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let agent_names = batch
                .column_by_name("agent_name")
                .ok_or_else(|| anyhow!("Missing agent_name column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let locations = batch
                .column_by_name("location")
                .ok_or_else(|| anyhow!("Missing location column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let location_lines = batch
                .column_by_name("location_lines")
                .ok_or_else(|| anyhow!("Missing location_lines column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let metadatas = batch
                .column_by_name("metadata")
                .ok_or_else(|| anyhow!("Missing metadata column"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let vectors = batch
                .column_by_name("vector")
                .ok_or_else(|| anyhow!("Missing vector column"))?
                .as_any()
                .downcast_ref::<FixedSizeListArray>()
                .unwrap();

            let metadata_val: Value =
                serde_json::from_str(metadatas.value(0)).unwrap_or(Value::Null);

            let float_arr = vectors
                .value(0)
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap()
                .clone();
            let vec: Vec<f32> = float_arr.values().to_vec();

            let payload = MemoryPayload {
                content: contents.value(0).to_string(),
                user_id: user_ids.value(0).to_string(),
                memory_type: memory_types.value(0).to_string(),
                agent_name: Some(agent_names.value(0).to_string()),
                location: locations.value(0).to_string(),
                location_lines: location_lines.value(0).to_string(),
                metadata: metadata_val,
            };
            return Ok(Some((vec, payload)));
        }

        Ok(None)
    }

    async fn list_namespaces(&self) -> Result<Vec<String>> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap())
            .execute()
            .await?;
        let table_names = conn.table_names().execute().await?;
        Ok(table_names)
    }
}
