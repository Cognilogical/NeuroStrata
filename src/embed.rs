use anyhow::Result;
use async_trait::async_trait;
use fastembed::{TextEmbedding, InitOptions, EmbeddingModel};
use crate::traits::Embedder;

pub struct FastEmbedder {
    model: TextEmbedding,
}

impl FastEmbedder {
    pub fn new() -> Result<Self> {
        let model = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::NomicEmbedTextV15).with_show_download_progress(true))?;
        Ok(Self { model })
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut embeddings = self.model.embed(vec![text], None)?;
        Ok(embeddings.pop().unwrap_or_default())
    }

    fn dimensions(&self) -> usize {
        768
    }
}
