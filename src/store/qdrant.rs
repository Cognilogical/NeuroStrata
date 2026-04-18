use anyhow::Result;
use async_trait::async_trait;
use qdrant_client::qdrant::{
    CreateCollection, Distance, PointId, PointStruct,
    SearchPoints, VectorParams, VectorsConfig, Value as QdrantValue,
};
use qdrant_client::qdrant::DeletePointsBuilder;
use qdrant_client::qdrant::UpsertPointsBuilder;
use qdrant_client::Qdrant;
use std::collections::HashMap;

use crate::traits::{MemoryPayload, SearchResult, VectorStore};

pub struct RemoteQdrantStore {
    client: Qdrant,
    collection_name: String,
    dimensions: usize,
}

impl RemoteQdrantStore {
    pub fn new(url: &str, dimensions: usize) -> Result<Self> {
        let client = Qdrant::from_url(url).build()?;
        Ok(Self {
            client,
            collection_name: "memories".to_string(),
            dimensions,
        })
    }
}

#[async_trait]
impl VectorStore for RemoteQdrantStore {
    async fn init(&self) -> Result<()> {
        if !self.client.collection_exists(&self.collection_name).await? {
            self.client
                .create_collection(CreateCollection {
                    collection_name: self.collection_name.clone(),
                    vectors_config: Some(VectorsConfig {
                        config: Some(qdrant_client::qdrant::vectors_config::Config::Params(
                            VectorParams {
                                size: self.dimensions as u64,
                                distance: Distance::Cosine as i32,
                                ..Default::default()
                            },
                        )),
                    }),
                    ..Default::default()
                })
                .await?;
        }
        Ok(())
    }

    async fn upsert(&self, id: &str, vector: Vec<f32>, payload: MemoryPayload) -> Result<()> {
        // Convert MemoryPayload to Qdrant Payload map
        let mut qdrant_payload: HashMap<String, QdrantValue> = HashMap::new();
        qdrant_payload.insert("content".to_string(), payload.content.into());
        qdrant_payload.insert("user_id".to_string(), payload.user_id.into());
        qdrant_payload.insert("metadata".to_string(), serde_json::to_string(&payload.metadata)?.into());

        let point = PointStruct::new(
            id.to_string(),
            vector,
            qdrant_payload,
        );

        let request = UpsertPointsBuilder::new(&self.collection_name, vec![point]);

        self.client.upsert_points(request).await?;

        Ok(())
    }

    async fn search(&self, vector: Vec<f32>, limit: usize) -> Result<Vec<SearchResult>> {
        let search_result = self
            .client
            .search_points(SearchPoints {
                collection_name: self.collection_name.clone(),
                vector,
                limit: limit as u64,
                with_payload: Some(true.into()),
                ..Default::default()
            })
            .await?;

        let mut results = Vec::new();
        for point in search_result.result {
            let id = match point.id.and_then(|i| i.point_id_options) {
                Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => u,
                Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => n.to_string(),
                None => continue,
            };

            let payload_map = point.payload;
            
            let content = payload_map.get("content")
                .and_then(|v| v.as_str())
                .map_or("".to_string(), |s| s.to_string());
                
            let user_id = payload_map.get("user_id")
                .and_then(|v| v.as_str())
                .map_or("".to_string(), |s| s.to_string());
                
            let metadata_str = payload_map.get("metadata")
                .and_then(|v| v.as_str())
                .map_or("{}".to_string(), |s| s.to_string());
                
            let metadata = serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null);

            results.push(SearchResult {
                id,
                score: point.score,
                payload: MemoryPayload {
                    content,
                    user_id,
                    metadata,
                },
            });
        }

        Ok(results)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let point_id = PointId {
            point_id_options: Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(id.to_string())),
        };

        let request = DeletePointsBuilder::new(&self.collection_name)
            .points(vec![point_id]);

        self.client.delete_points(request).await?;
            
        Ok(())
    }
}
