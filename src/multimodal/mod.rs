//! Multi-modal AST embeddings (in-house): placeholder for image/audio.
pub fn embed_text(s: &str) -> Vec<f32> { s.bytes().map(|b| b as f32 / 255.0).collect() }
#[cfg(test)] mod tests { use super::*; #[test] fn embeds() { assert!(!embed_text("hi").is_empty()); } }
