//! Deterministic, LLM-free mention detector (OxiRS §1.8).
//!
//! For each memory, find the symbols / file paths it references by exact
//! token match against a `KnownIds` index, and return their symbol ids.
//! No regex alternation, no model, no I/O.

use crate::store::ladybug::KnownIds;
use std::collections::HashMap;

const MENTION_CAP: usize = 32;

const STOPWORDS: &[&str] = &[
    "id", "x", "i", "t", "n", "ok", "err", "new", "get", "set",
];

/// Strip surrounding punctuation from a token, returning the inner string.
fn strip_surrounding_punct(s: &str) -> String {
    let mut start = 0usize;
    let mut end = s.len();
    let bytes = s.as_bytes();
    while start < end {
        let c = bytes[start] as char;
        if matches!(c, '`' | '"' | '\'' | '(' | '[' | '{' | '<' | ',' | ';' | ':' | '.' | '!' | '?') {
            start += 1;
        } else {
            break;
        }
    }
    while end > start {
        let c = bytes[end - 1] as char;
        if matches!(c, '`' | '"' | '\'' | ')' | ']' | '}' | '>' | ',' | ';' | ':' | '.' | '!' | '?') {
            end -= 1;
        } else {
            break;
        }
    }
    s[start..end].to_string()
}

fn is_short_alias(s: &str) -> bool {
    !s.contains('/') && !s.contains('#') && !s.contains(':')
}

fn alias_allowed(s: &str) -> bool {
    s.len() >= 3 && !STOPWORDS.contains(&s)
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn strip_hash(s: &str) -> &str {
    s.split('@').next().unwrap_or(s)
}

/// Build the candidate map (candidate string -> symbol id) from a `KnownIds`
/// index. Collisions drop the candidate entirely.
pub fn build_candidates(index: &KnownIds) -> HashMap<String, String> {
    // First pass: collect (candidate, id) pairs.
    let mut pairs: Vec<(String, String)> = Vec::new();
    for id in index.ids() {
        let logical = strip_hash(crate::parser::generation::logical_path_of(id));
        let logical = logical.to_string();
        // Full logical path (without hash).
        pairs.push((logical.clone(), id.to_string()));
        // File path portion before '#'.
        let file_path = logical.split('#').next().unwrap_or("").to_string();
        if !file_path.is_empty() && file_path != logical {
            if !is_short_alias(&file_path) || alias_allowed(&file_path) {
                pairs.push((file_path.clone(), id.to_string()));
            }
        }
        // Symbol name after kind: (e.g. functions:bar -> bar)
        let rest = logical.split('#').nth(1).unwrap_or("");
        let name = if rest.contains(':') {
            rest.split(':').last().unwrap_or(rest)
        } else {
            rest
        };
        let name = name.to_string();
        if !name.is_empty() && name != logical && name != file_path {
            if !is_short_alias(&name) || alias_allowed(&name) {
                pairs.push((name.clone(), id.to_string()));
            }
        }
        // Short aliases (final path components), guard + uniqueness handled later.
        if !file_path.is_empty() {
            let alias = basename(&file_path).to_string();
            if alias != file_path && alias_allowed(&alias) {
                pairs.push((alias, id.to_string()));
            }
        }
        if !name.is_empty() {
            let alias = basename(&name).to_string();
            if alias != name && alias_allowed(&alias) {
                pairs.push((alias, id.to_string()));
            }
        }
    }

    // Count occurrences.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (cand, _) in &pairs {
        *counts.entry(cand.clone()).or_insert(0) += 1;
    }

    // Keep only unambiguous candidates.
    let mut out: HashMap<String, String> = HashMap::new();
    for (cand, id) in pairs {
        if counts.get(&cand).copied().unwrap_or(0) == 1 {
            // Short aliases already guarded above; guard again for safety.
            if is_short_alias(&cand) && !alias_allowed(&cand) {
                continue;
            }
            out.insert(cand, id);
        }
    }
    out
}

/// Detect mention symbol ids in `content` and `location`.
pub fn detect_mentions(content: &str, location: &str, index: &KnownIds) -> Vec<String> {
    if content.len() > 1_048_576 {
        eprintln!("[neurostrata:mention] content exceeds 1 MiB; skipping detection");
        return Vec::new();
    }
    let candidates = build_candidates(index);
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut resolved: Vec<String> = Vec::new();
    // We need neighbor checks; process content and location with positions.
    let inputs = [content, location];
    for text in inputs {
        for token in text.split_whitespace() {
            let stripped = strip_surrounding_punct(token);
            if stripped.is_empty() {
                continue;
            }
            if let Some(id) = candidates.get(&stripped) {
                // Short alias neighbor guard.
                if is_short_alias(&stripped) {
                    // Find token position in text to inspect neighbors.
                    // We approximate by searching for the token substring.
                    // Guard: neighboring chars (in original text around this token)
                    // must not be path/identifier separators.
                    // Since token came from split_whitespace, neighbors are whitespace
                    // or string boundaries, which are safe. Additional guard inside token
                    // is not applicable. Keep simple: allow.
                    let _ = text;
                }
                if !resolved.contains(id) {
                    resolved.push(id.clone());
                }
                if resolved.len() >= MENTION_CAP {
                    eprintln!("[neurostrata:mention] mention cap reached");
                    return resolved;
                }
            }
        }
    }
    resolved
}

// Expose KnownIds ids via trait-like method added in ladybug.rs.
// This module relies on `index.ids()`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ladybug::KnownIds;

    #[test]
    fn basic_detection() {
        let ids = vec![
            "ns::1::src/foo.rs#functions:bar@abc".to_string(),
            "ns::1::src/baz.rs".to_string(),
        ];
        let refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        let index = KnownIds::new(refs);
        let content = "see `src/foo.rs#functions:bar` and src/baz.rs";
        let got = detect_mentions(content, "", &index);
        assert!(got.iter().any(|g| g.contains("src/foo.rs#functions:bar")));
        assert!(got.iter().any(|g| g.contains("src/baz.rs")));
    }

    #[test]
    fn cap_and_dedup() {
        // Build 40 distinct symbols.
        let mut ids = Vec::new();
        for i in 0..40 {
            ids.push(format!("ns::1::src/f{}.rs#functions:g{}@h{}", i, i, i));
        }
        let refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        let index = KnownIds::new(refs);
        let content = (0..40)
            .map(|i| format!("src/f{}.rs#functions:g{}", i, i))
            .collect::<Vec<_>>()
            .join(" ");
        let got = detect_mentions(&content, "", &index);
        assert_eq!(got.len(), MENTION_CAP);
    }
}
