use anyhow::Result;
use async_trait::async_trait;
use fastembed::FastEmbed;
use crate::traits::Embedder;

pub struct FastEmbedder {
    model: FastEmbed,
}

impl FastEmbedder {
    pub fn new() -> Result<Self> {
        let model = FastEmbed::new("nomic-embed-text-v1.5.onnx")?;
        Ok(Self { model })
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let embedding = self.model.embed(text).await?;
        Ok(embedding)
    }

    fn dimensions(&self) -> usize {
        self.model.dimensions()
    }
}