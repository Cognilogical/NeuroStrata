use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

use lancedb::query::{ExecutableQuery, QueryBase};
use arrow::datatypes::{DataType, Field, Schema as ArrowSchema};
use arrow::array::{
    RecordBatch, StringArray, FixedSizeListArray, Float32Array, RecordBatchReader,
};
use arrow::record_batch::RecordBatchIterator;
use futures::StreamExt;

use crate::traits::{VectorStore, MemoryPayload, SearchResult};

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
        
        Ok(Self { local_path, dimensions })
    }

    fn schema(&self) -> Arc<ArrowSchema> {
        Arc::new(ArrowSchema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("vector", DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                self.dimensions as i32
            ), false),
            Field::new("content", DataType::Utf8, false),
            Field::new("user_id", DataType::Utf8, false),
            Field::new("agent_name", DataType::Utf8, false),
            Field::new("metadata", DataType::Utf8, false),
        ]))
    }
}

#[async_trait]
impl VectorStore for LanceDBStore {
    async fn init(&self, namespace: &str) -> Result<()> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap()).execute().await?;
        
        // Check if table exists, if not create empty table
        let table_names = conn.table_names().execute().await?;
        if !table_names.contains(&namespace.to_string()) {
            let schema = self.schema();
            let empty_batch = RecordBatch::new_empty(schema.clone());
            let batches = vec![empty_batch];
            let batches_iter = Box::new(RecordBatchIterator::new(batches.into_iter().map(Ok), schema.clone())) as Box<dyn RecordBatchReader + Send>;
            conn.create_table(namespace, batches_iter).execute().await?;
        }
        Ok(())
    }
    
    async fn upsert(&self, namespace: &str, id: &str, vector: Vec<f32>, payload: MemoryPayload) -> Result<()> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap()).execute().await?;
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
        let agent_name_str = payload.agent_name.unwrap_or_else(|| "unknown".to_string());
        let agent_name_array = Arc::new(StringArray::from(vec![agent_name_str.as_str()]));
        let metadata_str = serde_json::to_string(&payload.metadata)?;
        let metadata_array = Arc::new(StringArray::from(vec![metadata_str.as_str()]));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                id_array,
                vector_list,
                content_array,
                user_id_array,
                agent_name_array,
                metadata_array,
            ],
        )?;

        let batches_iter = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema.clone())) as Box<dyn RecordBatchReader + Send>;
        
        table.delete(format!("id = '{}'", id).as_str()).await.ok();
        table.add(batches_iter).execute().await?;
        
        Ok(())
    }
    
    async fn search(&self, namespace: &str, vector: Vec<f32>, limit: usize) -> Result<Vec<SearchResult>> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap()).execute().await?;
        let table = conn.open_table(namespace).execute().await?;

        let mut stream = table.vector_search(vector)?
            .limit(limit)
            .execute()
            .await?;

        let mut results = Vec::new();
        
        while let Some(batch) = stream.next().await {
            let batch: RecordBatch = batch?;
            let ids = batch.column_by_name("id").ok_or_else(|| anyhow!("Missing id column"))?.as_any().downcast_ref::<StringArray>().unwrap();
            let distances = batch.column_by_name("_distance").ok_or_else(|| anyhow!("Missing _distance column"))?.as_any().downcast_ref::<Float32Array>().unwrap();
            let contents = batch.column_by_name("content").ok_or_else(|| anyhow!("Missing content column"))?.as_any().downcast_ref::<StringArray>().unwrap();
            let user_ids = batch.column_by_name("user_id").ok_or_else(|| anyhow!("Missing user_id column"))?.as_any().downcast_ref::<StringArray>().unwrap();
            let agent_names = batch.column_by_name("agent_name").ok_or_else(|| anyhow!("Missing agent_name column"))?.as_any().downcast_ref::<StringArray>().unwrap();
            let metadatas = batch.column_by_name("metadata").ok_or_else(|| anyhow!("Missing metadata column"))?.as_any().downcast_ref::<StringArray>().unwrap();

            for i in 0..batch.num_rows() {
                let metadata_val: Value = serde_json::from_str(metadatas.value(i)).unwrap_or(Value::Null);
                results.push(SearchResult {
                    id: ids.value(i).to_string(),
                    score: distances.value(i),
                    payload: MemoryPayload {
                        content: contents.value(i).to_string(),
                        user_id: user_ids.value(i).to_string(),
                        agent_name: Some(agent_names.value(i).to_string()),
                        metadata: metadata_val,
                    }
                });
            }
        }

        Ok(results)
    }
    
    async fn delete(&self, namespace: &str, id: &str) -> Result<()> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap()).execute().await?;
        let table = conn.open_table(namespace).execute().await?;
        table.delete(format!("id = '{}'", id).as_str()).await?;
        Ok(())
    }

    async fn list(&self, namespace: &str, user_id: Option<&str>) -> Result<Vec<SearchResult>> {
        let conn = lancedb::connect(self.local_path.to_str().unwrap()).execute().await?;
        let table = conn.open_table(namespace).execute().await?;

        let mut query = table.query();
        if let Some(uid) = user_id {
            query = query.only_if(format!("user_id = '{}'", uid));
        }

        let mut stream = query.execute().await?;
        let mut results = Vec::new();
        
        while let Some(batch) = stream.next().await {
            let batch: RecordBatch = batch?;
            let ids = batch.column_by_name("id").ok_or_else(|| anyhow!("Missing id column"))?.as_any().downcast_ref::<StringArray>().unwrap();
            let contents = batch.column_by_name("content").ok_or_else(|| anyhow!("Missing content column"))?.as_any().downcast_ref::<StringArray>().unwrap();
            let user_ids = batch.column_by_name("user_id").ok_or_else(|| anyhow!("Missing user_id column"))?.as_any().downcast_ref::<StringArray>().unwrap();
            let agent_names = batch.column_by_name("agent_name").ok_or_else(|| anyhow!("Missing agent_name column"))?.as_any().downcast_ref::<StringArray>().unwrap();
            let metadatas = batch.column_by_name("metadata").ok_or_else(|| anyhow!("Missing metadata column"))?.as_any().downcast_ref::<StringArray>().unwrap();

            for i in 0..batch.num_rows() {
                let metadata_val: Value = serde_json::from_str(metadatas.value(i)).unwrap_or(Value::Null);
                results.push(SearchResult {
                    id: ids.value(i).to_string(),
                    score: 0.0, // Score not applicable for list
                    payload: MemoryPayload {
                        content: contents.value(i).to_string(),
                        user_id: user_ids.value(i).to_string(),
                        agent_name: Some(agent_names.value(i).to_string()),
                        metadata: metadata_val,
                    }
                });
            }
        }

        Ok(results)
    }
}
