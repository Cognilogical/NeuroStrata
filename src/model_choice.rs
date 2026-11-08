//! Embedding-model shortlist for setup-time model choice.
//!
//! Static data only: the candidates a first-run setup may offer, in
//! presentation order. The setup surface slices a prefix of
//! [`candidates()`] at the shortlist breadth fixed outside this module
//! and derives each candidate's context class from `max_tokens`
//! (8,192 = full-context class; 512 = limited-context class).

/// One selectable embedding model in the setup shortlist.
pub struct ModelChoice {
    /// fastembed `EmbeddingModel` variant name; parses via
    /// `EmbeddingModel::from_str`.
    pub model_name: &'static str,
    /// Embedding width; matches the resolver's model info for
    /// `model_name`.
    pub dimensions: usize,
    /// Byte size of the model's ONNX file on HuggingFace, byte-exact as
    /// re-verified against the live `resolve` URLs when this table was
    /// written.
    pub file_bytes: u64,
    /// Maximum number of input tokens the model embeds before
    /// truncation.
    pub max_tokens: usize,
    /// Structural facts only — no benchmark figures.
    pub note: &'static str,
}

/// The shortlist, in presentation order: the ranked top three first,
/// then the non-quantized small model, then the base quantized model.
pub fn candidates() -> &'static [ModelChoice] {
    &[
        ModelChoice {
            model_name: "NomicEmbedTextV15Q",
            dimensions: 768,
            file_bytes: 137296292,
            max_tokens: 8192,
            note: "Same model as the standard default, int8-quantized. Dimensions and context unchanged.",
        },
        ModelChoice {
            model_name: "BGESmallENV15Q",
            dimensions: 384,
            file_bytes: 66465124,
            max_tokens: 512,
            note: "Smallest download. 384 dimensions; 512-token context. Already the low profile's model.",
        },
        ModelChoice {
            model_name: "SnowflakeArcticEmbedMQ",
            dimensions: 768,
            file_bytes: 110084023,
            max_tokens: 512,
            note: "Keeps 768 dimensions; 512-token context.",
        },
        ModelChoice {
            model_name: "BGESmallENV15",
            dimensions: 384,
            file_bytes: 133093490,
            max_tokens: 512,
            note: "The non-quantized small model. 384 dimensions; 512-token context.",
        },
        ModelChoice {
            model_name: "BGEBaseENV15Q",
            dimensions: 768,
            file_bytes: 217824172,
            max_tokens: 512,
            note: "768 dimensions; least memory saved of this list; 512-token context.",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastembed::{EmbeddingModel, TextEmbedding};
    use std::str::FromStr;

    #[test]
    fn candidates_are_complete_in_presentation_order() {
        let names: Vec<&str> = candidates().iter().map(|c| c.model_name).collect();
        assert_eq!(
            names,
            vec![
                "NomicEmbedTextV15Q",
                "BGESmallENV15Q",
                "SnowflakeArcticEmbedMQ",
                "BGESmallENV15",
                "BGEBaseENV15Q",
            ]
        );
    }

    #[test]
    fn candidate_dimensions_match_the_resolver() {
        for c in candidates() {
            let parsed = EmbeddingModel::from_str(c.model_name)
                .unwrap_or_else(|_| panic!("{} does not parse as an EmbeddingModel", c.model_name));
            let info = TextEmbedding::get_model_info(&parsed)
                .unwrap_or_else(|_| panic!("no model info for {}", c.model_name));
            assert_eq!(
                info.dim, c.dimensions,
                "dimension drift for {}",
                c.model_name
            );
        }
    }

    #[test]
    fn candidate_file_bytes_are_the_validated_values() {
        let expected: [(&str, u64); 5] = [
            ("NomicEmbedTextV15Q", 137296292),
            ("BGESmallENV15Q", 66465124),
            ("SnowflakeArcticEmbedMQ", 110084023),
            ("BGESmallENV15", 133093490),
            ("BGEBaseENV15Q", 217824172),
        ];
        assert_eq!(candidates().len(), expected.len());
        for (c, (name, bytes)) in candidates().iter().zip(expected.iter()) {
            assert_eq!(c.model_name, *name);
            assert_eq!(c.file_bytes, *bytes, "file_bytes drift for {}", name);
        }
    }
}
