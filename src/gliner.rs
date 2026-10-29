//! GLiNER-style entity candidate emitter (review-only, deterministic).
//! 
//! Per spec `docs/superpowers/specs/2026-10-28-gliner-review-only-candidates-design.md`.
//! Module path: `src/gliner.rs` (spec line 10). Not in dedup/guard/routing paths.
//! 
//! Constraints met:
//! - No ONNX inference in default build (`gliner` feature gates session)
//! - All candidates `review_only = true`, never auto-promoted
//! - Deterministic extraction: capitalised multi-word phrases only
//! - Full ONNX sidecar feature-gated; SHA-256 bundle verifier deferred to future work

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
    matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '"' | '\'')
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

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn extracts_capitalized() {
        let text = "The QUICK Brown FOX jumps over the LAZY dog";
        let c = extract_candidates(text);
        assert!(!c.is_empty());
        assert!(c.iter().all(|x| x.review_only));
        let texts: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
        // Should contain "QUICK" (from "The QUICK Brown FOX") and "LAZY"
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
        // Comma/period terminates phrase after the word it attaches to
        let text = "John Smith, CEO, went to New York, USA. He arrived.";
        let c = extract_candidates(text);
        let texts: Vec<String> = c.iter().map(|x| x.text.clone()).collect();
        // "John Smith" (comma after Smith ends phrase)
        assert!(texts.contains(&"John Smith".to_string()));
        // "CEO" (comma after CEO ends phrase)
        assert!(texts.iter().any(|t| t == "CEO"));
        // "New York" (comma after York ends phrase)
        assert!(texts.iter().any(|t| t == "New York"));
        // "USA" (period after USA ends phrase)
        assert!(texts.iter().any(|t| t == "USA"));
        // "He" (period after He ends phrase, but He is capitalized alone)
        assert!(texts.iter().any(|t| t == "He"));
        // No punctuation bleed
        assert!(texts.iter().all(|t| !t.ends_with(',')));
        assert!(texts.iter().all(|t| !t.ends_with('.')));
    }
    
    #[test]
    fn mixed_case() {
        // "MixedCase Word" -> phrase, "this" stops, "IS NOT" -> phrase, "extracted" stops
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
        // Punctuation terminates phrase after the word it attaches to
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
}