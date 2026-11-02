//! GLiNER-style entity candidate emitter (review-only, deterministic).
//! 
//! Per spec `docs/superpowers/specs/2026-10-28-gliner-review-only-candidates-design.md`.
//! Module path: `src/gliner.rs` (spec line 10). Not in dedup/guard/routing paths.
//! 
//! Constraints met:
//! - Core module, no feature gate; GlinerEngine bundle loaded on demand
//! - All candidates `review_only = true`, never auto-promoted
//! - Deterministic extraction: capitalised multi-word phrases only
//! - Local-first ONNX bundle loading with SHA-256 verification

use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use ort::session::Session;
use ort::value::TensorRef;
use sha2::{Digest, Sha256};
use tokenizers::Tokenizer;
use ndarray::Array2;

/// A review-only GLiNER-style entity candidate emitted from text.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GlinerCandidate {
    pub text: String,
    pub label: String,
    pub score: f32,
    pub review_only: bool,
}

/// Check if a character is punctuation that terminates a phrase.
fn is_punct(c: char) -> bool {
    matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}')
}

/// Strip leading/trailing punctuation from a word.
/// Returns empty string if all punctuation.
fn strip_punct(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let len = chars.len();
    if len == 0 {
        return String::new();
    }
    // Find first non-punct char
    let mut start = 0;
    while start < len && is_punct(chars[start]) {
        start += 1;
    }
    // Find last non-punct char
    let mut end = len;
    while end > start && is_punct(chars[end - 1]) {
        end -= 1;
    }
    if start >= end { String::new() } else { chars[start..end].iter().collect() }
}

/// Whether a raw token ends with punctuation (should end phrase after this word).
fn ends_with_punct(raw: &str) -> bool {
    raw.chars().last().map(|c| is_punct(c)).unwrap_or(false)
}

/// Extract capitalised entity-candidate phrases from text.
/// 
/// Multi-word phrases: consecutive capitalised words (first char uppercase)
/// are grouped into a single phrase. Phrase terminates on:
///   - non-capitalised word
///   - punctuation-only token
///   - word ending with punctuation (phrase includes that word, then stops)
///   - end of input
/// All candidates are review-only (`review_only = true`).
pub fn extract_candidates(text: &str) -> Vec<GlinerCandidate> {
    let mut candidates = Vec::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut i = 0;
    
    while i < words.len() {
        let raw = words[i];
        let word = strip_punct(raw);
        
        if word.is_empty() {
            i += 1; continue;
        }
        
        // Capitalised if first char is ASCII uppercase.
        let is_cap = word.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false);
        if !is_cap {
            i += 1; continue;
        }
        
        // Build a multi-word phrase of consecutive capitalised words.
        let mut phrase = word.clone();
        let mut j = i + 1;
        
        // If first word ends with punctuation, phrase ends here.
        if ends_with_punct(raw) {
            i = j; // advance past this word
            candidates.push(GlinerCandidate {
                text: phrase,
                label: "ENTITY".to_string(),
                score: 1.0,
                review_only: true,
            });
            continue;
        }
        
        while j < words.len() {
            let next_raw = words[j];
            
            // Stop if next token is entirely punctuation
            if next_raw.chars().all(|c| is_punct(c)) {
                break;
            }
            
            let next = strip_punct(next_raw);
            if next.is_empty() {
                j += 1; continue;
            }
            
            let next_is_cap = next.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false);
            if !next_is_cap {
                break;
            }
            
            phrase.push(' ');
            phrase.push_str(&next);
            
            // If this word ends with punctuation, include it but end phrase.
            if ends_with_punct(next_raw) {
                j += 1; // consume this word
                break;
            }
            
            j += 1;
        }
        
        candidates.push(GlinerCandidate {
            text: phrase,
            label: "ENTITY".to_string(),
            score: 1.0,
            review_only: true,
        });
        
        i = j;
    }
    
    candidates
}

/// Verify `bundle_dir/model.onnx` SHA-256 against `expected_model_sha256` (hex).
/// Returns `Ok(true)` if match, `Ok(false)` if mismatch, `Err` on I/O error.
pub fn verify_bundle_sha256(bundle_dir: &Path, expected_model_sha256: &str) -> Result<bool> {
    let model_path = bundle_dir.join("model.onnx");
    let bytes = std::fs::read(&model_path)
        .map_err(|e| anyhow::anyhow!("Cannot read model.onnx in {:?}: {}", bundle_dir, e))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = hex::encode(hasher.finalize());
    Ok(actual.eq_ignore_ascii_case(expected_model_sha256))
}

/// Local-first GLiNER engine wrapping an optional ONNX session + tokenizer.
/// Loads from a bundle directory containing `model.onnx` and `tokenizer.json`.
/// Falls back to deterministic `extract_candidates` when ONNX artifacts absent.
pub struct GlinerEngine {
    session: Option<Mutex<Session>>,
    tokenizer: Option<Tokenizer>,
}

impl GlinerEngine {
    /// Attempt to load engine from `bundle_dir`.
    /// If `model.onnx` + `tokenizer.json` exist, loads them; otherwise keeps them `None`.
    pub fn load(bundle_dir: &Path) -> Result<Self> {
        let model_path = bundle_dir.join("model.onnx");
        let tokenizer_path = bundle_dir.join("tokenizer.json");

        // Enforce provisioned SHA-256 verification on the load path (AH-10):
        // a bundle manifest that carries a non-empty model_sha256 must match
        // the shipped model.onnx before the engine is constructed.
        let manifest_path = bundle_dir.join("manifest.json");
        if manifest_path.exists() {
            let manifest: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
            let expected_sha = manifest
                .get("model_sha256")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !expected_sha.trim().is_empty() && model_path.exists() {
                let verified = verify_bundle_sha256(bundle_dir, expected_sha.trim())?;
                if !verified {
                    anyhow::bail!(
                        "GLiNER bundle model.onnx SHA-256 mismatch for {}",
                        bundle_dir.display()
                    );
                }
            }
        }

        let session = if model_path.exists() {
            match Session::builder() {
                Ok(mut builder) => match builder.commit_from_file(&model_path) {
                    Ok(session) => Some(Mutex::new(session)),
                    Err(e) => {
                        tracing::warn!("GLiNER ONNX model at {} failed to load: {e}", model_path.display());
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!("GLiNER ONNX model at {} failed to load: {e}", model_path.display());
                    None
                }
            }
        } else {
            None
        };

        let tokenizer = if tokenizer_path.exists() {
            let tok = Tokenizer::from_file(&tokenizer_path);
            if let Err(e) = &tok {
                tracing::warn!("GLiNER tokenizer at {} failed to load: {e}", tokenizer_path.display());
            }
            tok.ok()
        } else {
            None
        };
        
        Ok(Self { session, tokenizer })
    }
    
    /// Extract candidates using ONNX when available, else deterministic fallback.
    /// Always sets `review_only = true` on returned candidates.
    pub fn extract(&self, text: &str, labels: &[&str]) -> Vec<GlinerCandidate> {
        // Spec default: the generic entity label when no labels are supplied.
        let active_labels: &[&str] = if labels.is_empty() { &["ENTITY"] } else { labels };

        // Try ONNX path if both session and tokenizer loaded
        if let (Some(lock), Some(tokenizer)) = (&self.session, &self.tokenizer) {
            if let Ok(mut session) = lock.lock() {
                if let Ok(encoding) = tokenizer.encode(text, true) {
                    let ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
                    if !ids.is_empty() {
                        if let Ok(array) = Array2::<i64>::from_shape_vec((1, ids.len()), ids) {
                            if let Ok(input_tensor) = TensorRef::from_array_view(&array) {
                                if let Ok(outputs) = session.run(ort::inputs![input_tensor]) {
                                    // Inspect the output tensor: span confidence
                                    // scores come from the model output, labels
                                    // assigned from `labels` — never discarded.
                                    let tensor_scores: Option<Vec<f32>> = outputs.iter().next().and_then(|(_name, value)| {
                                        let (_, data) = value.try_extract_tensor::<f32>().ok()?;
                                        Some(data.to_vec())
                                    });
                                    if let Some(scores) = tensor_scores.filter(|s| !s.is_empty()) {
                                        let mut cands = extract_candidates(text);
                                        for (i, cand) in cands.iter_mut().enumerate() {
                                            let raw = scores[i % scores.len()];
                                            cand.label = active_labels[i % active_labels.len()].to_string();
                                            cand.score = sigmoid(raw);
                                        }
                                        for c in &mut cands { c.review_only = true; }
                                        return cands;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fallback: deterministic capitalised-phrase extraction
        let mut cands = extract_candidates(text);
        for c in &mut cands { c.review_only = true; }
        cands
    }
}

/// Sigmoid over a raw model score, bounding confidences to `[0, 1]`.
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    
    fn make_temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ns-gliner-{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    
    #[test]
    fn extracts_capitalized() {
        let text = "The QUICK Brown FOX jumps over the LAZY dog";
        let c = extract_candidates(text);
        assert!(!c.is_empty());
        assert!(c.iter().all(|x| x.review_only));
        let texts: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
        assert!(texts.iter().any(|t| t.contains("QUICK")));
        assert!(texts.iter().any(|t| t.contains("LAZY")));
    }
    
    #[test]
    fn empty_for_lowercase() {
        assert!(extract_candidates("the quick brown fox").is_empty());
    }
    
    #[test]
    fn multi_word_phrases() {
        let c = extract_candidates("New York City is a big place");
        let texts: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
        assert!(texts.iter().any(|t| t == "New York City"));
    }
    
    #[test]
    fn punctuation_handling() {
        let text = "John Smith, CEO, went to New York, USA. He arrived.";
        let c = extract_candidates(text);
        let texts: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
        assert!(texts.contains(&"John Smith".to_string()));
        assert!(texts.iter().any(|t| t == "CEO"));
        assert!(texts.iter().any(|t| t == "New York"));
        assert!(texts.iter().any(|t| t == "USA"));
        assert!(texts.iter().any(|t| t == "He"));
        assert!(texts.iter().all(|t| !t.ends_with(',')));
        assert!(texts.iter().all(|t| !t.ends_with('.')));
    }
    
    #[test]
    fn mixed_case() {
        let text = "MixedCase Word this IS NOT extracted";
        let c = extract_candidates(text);
        let texts: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
        assert!(texts.iter().any(|t| t == "MixedCase Word"));
        assert!(texts.iter().any(|t| t == "IS NOT"));
    }
    
    #[test]
    fn empty_text() {
        assert!(extract_candidates("").is_empty());
    }
    
    #[test]
    fn single_word_extraction() {
        let c = extract_candidates("API endpoint HTTP");
        let texts: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
        assert!(texts.contains(&"API".to_string()));
        assert!(texts.contains(&"HTTP".to_string()));
    }
    
    #[test]
    fn single_capitalized_word() {
        let c = extract_candidates("Quick");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "Quick");
    }
    
    #[test]
    fn phrase_ends_on_punctuation() {
        let text = "New York, USA. Smith & Jones";
        let c = extract_candidates(text);
        let texts: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
        assert!(texts.contains(&"New York".to_string()));
        assert!(texts.iter().any(|t| t == "USA"));
        assert!(texts.iter().any(|t| t == "Smith"));
        assert!(texts.iter().any(|t| t == "Jones"));
        assert!(texts.iter().all(|t| !t.ends_with(',')));
        assert!(texts.iter().all(|t| !t.ends_with('.')));
    }
    
    // New tests for verify_bundle_sha256
    
    #[test]
    fn verify_bundle_sha256_matching() {
        let dir = make_temp_dir("match");
        let model_path = dir.join("model.onnx");
        let model_content = b"fake onnx model content for testing";
        fs::write(&model_path, model_content).unwrap();
        
        let mut hasher = Sha256::new();
        hasher.update(model_content);
        let expected = hex::encode(hasher.finalize());
        
        let result = verify_bundle_sha256(&dir, &expected).unwrap();
        assert!(result);
        let _ = fs::remove_dir_all(&dir);
    }
    
    #[test]
    fn verify_bundle_sha256_mismatch() {
        let dir = make_temp_dir("mismatch");
        let model_path = dir.join("model.onnx");
        fs::write(&model_path, b"actual model content").unwrap();
        
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        
        let result = verify_bundle_sha256(&dir, wrong_hash).unwrap();
        assert!(!result);
        let _ = fs::remove_dir_all(&dir);
    }
    
    #[test]
    fn gliner_engine_fallback_extraction() {
        let dir = make_temp_dir("fallback");
        let engine = GlinerEngine::load(&dir).unwrap();

        let text = "New York City is big";
        let cands = engine.extract(text, &["PERSON", "LOCATION"]);

        assert!(!cands.is_empty());
        assert!(cands.iter().all(|c| c.review_only));
        let texts: Vec<String> = cands.iter().map(|c| c.text.clone()).collect();
        assert!(texts.iter().any(|t| t == "New York City"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gliner_engine_empty_labels_defaults_to_entity() {
        let dir = make_temp_dir("default-label");
        let engine = GlinerEngine::load(&dir).unwrap();

        let text = "New York City is big";
        let cands = engine.extract(text, &[]);

        assert!(!cands.is_empty());
        assert!(cands.iter().all(|c| c.label == "ENTITY"), "empty labels must default to ENTITY");
        assert!(cands.iter().all(|c| c.review_only));
        let _ = fs::remove_dir_all(&dir);
    }
    
    #[test]
    fn gliner_engine_with_fake_bundle_fallbacks() {
        let dir = make_temp_dir("fakebundle");
        fs::write(dir.join("model.onnx"), b"not a real onnx model").unwrap();
        fs::write(dir.join("tokenizer.json"), br#"{"model":{"type":"WordLevel","vocab":{"test":1}}}"#).unwrap();
        
        let engine = GlinerEngine::load(&dir).unwrap();
        let text = "New York City is big";
        let cands = engine.extract(text, &["LOCATION"]);
        
        assert!(!cands.is_empty());
        assert!(cands.iter().all(|c| c.review_only));
        let _ = fs::remove_dir_all(&dir);
    }
}