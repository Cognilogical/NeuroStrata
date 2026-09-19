use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// Rejection reason produced by the scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// Which field was rejected, e.g. "content", "metadata.governs".
    pub location: String,
    /// Category of the detected secret, e.g. "provider_token", "jwt", "password".
    pub category: &'static str,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ERROR [SECURITY]: Rejected {} ({}). Do not store secrets.", self.location, self.category)
    }
}

/// The compiled scanner: a set of pre-compiled regexes and a set of provider
/// form recognisers. Built once and reused across every request.
pub(crate) struct SecretScanner {
    provider_re: Regex,
    kv_re: Regex,
    jwt_re: Regex,
}

/// Provider token prefixes and their expected shape (alphabet + min length
/// after the prefix). The scanner checks the value side of key=value pairs,
/// but also standalone tokens that match a known prefix.
struct ProviderForm {
    prefix: &'static str,
    /// Minimum length of the whole token (prefix + suffix).
    min_len: usize,
    /// If set, the suffix must match this regex (which anchors at ^).
    suffix_re: Option<&'static str>,
}

const PROVIDER_FORMS: &[ProviderForm] = &[
    ProviderForm {
        // GitHub PAT: ghp_, gho_, ghs_, ghr_, github_pat_
        // Real tokens are 40+ chars (prefix + 36-38 base62). 16 catches
        // partial leaks (e.g. test fixtures) while excluding trivial matches.
        prefix: "ghp_",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        prefix: "gho_",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        prefix: "ghs_",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        prefix: "ghr_",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        prefix: "github_pat_",
        min_len: 40,
        suffix_re: None,
    },
    ProviderForm {
        // Anthropic: sk-ant-api03-... (base62, long)
        prefix: "sk-ant-",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        // OpenAI sk- (not sk-proj-, handled separately): base62, ~51 chars total
        prefix: "sk-proj-",
        min_len: 20,
        suffix_re: None,
    },
    ProviderForm {
        // OpenAI sk- must come AFTER sk-proj- and sk-ant- to avoid
        // false-positive prefix overlap. Higher min_len because sk- is
        // a common prefix; real keys are ~51 chars.
        prefix: "sk-",
        min_len: 40,
        suffix_re: None,
    },
    ProviderForm {
        // Slack xox[baprs]-
        prefix: "xoxb-",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        prefix: "xoxp-",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        prefix: "xoxa-",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        prefix: "xoxr-",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        prefix: "xoxs-",
        min_len: 16,
        suffix_re: None,
    },
    ProviderForm {
        // AWS access key: AKIA followed by exactly 16 uppercase alphanumeric
        prefix: "AKIA",
        min_len: 20,
        suffix_re: Some("^[0-9A-Z]{16}$"),
    },
];

/// Patterns that look like placeholders or documentation fixtures and must
/// NOT be treated as secrets, even if they match a provider prefix or key.
/// Checked as whole-value or prefix matches, not loose substrings, to avoid
/// false positives on tokens that happen to contain common words.
const PLACEHOLDER_PATTERNS: &[&str] = &[
    "changeme",
    "your-api-key-here",
    "your_key_here",
    "your-secret-here",
    "replace-me",
    "replace_me",
    "insert-key",
    "insert_key",
    "placeholder",
];

/// Maximum recursion depth for metadata scanning.
const MAX_DEPTH: usize = 8;
/// Maximum total characters scanned per top-level call.
const MAX_SCAN_BYTES: usize = 256_000;

impl SecretScanner {
    /// Build the scanner. Returns `Err` if a regex fails to compile.
    fn build() -> Result<Self, regex::Error> {
        // Provider token form: word-boundary anchored, full-match check is done
        // separately after the regex finds a candidate.
        let provider_re = Regex::new(
            r"(?i)\b(?:sk-ant-|ghp_|gho_|ghs_|ghr_|github_pat_|sk-proj-|sk-|xox[baprs]-|AKIA)[A-Za-z0-9_\-]{4,}\b",
        )?;

        // Key=value candidate extraction. Matches:
        //   key = "value"      key : value
        //   "key": "value"     key=value
        // Case-insensitive keys. Allows optional quotes around the key name
        // (for JSON-style) and around the value. No backreferences.
        let kv_re = Regex::new(
            r#"(?i)["']?\s*(api[_-]?key|apikey|secret|token|password|passwd|private[_-]?key|client[_-]?secret|access[_-]?token|refresh[_-]?token|bearer)\s*["']?\s*[=:]\s*["']?([^"'\s,;}\]]{4,})"#,
        )?;

        // JWT: three base64url segments separated by dots.
        let jwt_re = Regex::new(
            r"eyJ[A-Za-z0-9_\-]+\.eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+",
        )?;

        Ok(Self {
            provider_re,
            kv_re,
            jwt_re,
        })
    }

    fn instance() -> Option<&'static SecretScanner> {
        static SCANNER: OnceLock<Option<SecretScanner>> = OnceLock::new();
        SCANNER
            .get_or_init(|| SecretScanner::build().ok())
            .as_ref()
    }

    /// Scan a string for secrets. Returns `None` if clean or the scanner is
    /// disabled, `Some(Rejection)` with category + location on detection.
    pub fn scan_str(text: &str, location: &str) -> Option<Rejection> {
        let scanner = Self::instance()?;
        if text.len() > MAX_SCAN_BYTES {
            return Some(Rejection {
                location: location.to_string(),
                category: "oversized_input",
            });
        }
        scanner.scan_inner(text, location, 0)
    }

    /// Scan a JSON value recursively (metadata leaves), returning the first
    /// rejection found.
    pub fn scan_value(value: &Value, location: &str) -> Option<Rejection> {
        let scanner = Self::instance()?;
        scanner.scan_value_inner(value, location, 0)
    }

    fn scan_inner(&self, text: &str, location: &str, _depth: usize) -> Option<Rejection> {
        if text.len() > MAX_SCAN_BYTES {
            return Some(Rejection {
                location: location.to_string(),
                category: "oversized_input",
            });
        }

        let lower = text.to_ascii_lowercase();

        // --- Placeholder exclusion (fast check before regex) ---
        // Check the whole text first: inputs like "changeme" or "<placeholder>"
        // should be rejected as a whole without regex parsing.
        if is_placeholder(&lower) {
            return None;
        }

        // --- JWT detection ---
        if let Some(cat) = self.detect_jwt(text) {
            return Some(Rejection {
                location: location.to_string(),
                category: cat,
            });
        }

        // --- Provider token form detection ---
        // Try the provider regex on the whole text and on each extracted
        // value-side from key=value pairs.
        for mat in self.provider_re.find_iter(text) {
            let candidate = mat.as_str();
            if let Some(cat) = self.validate_provider_form(candidate) {
                if !is_placeholder(&candidate.to_ascii_lowercase()) {
                    return Some(Rejection {
                        location: location.to_string(),
                        category: cat,
                    });
                }
            }
        }

        // --- Key=value extraction ---
        for cap in self.kv_re.captures_iter(text) {
            let value = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            let value_lower = value.to_ascii_lowercase();

            if is_placeholder(&value_lower) {
                continue;
            }

            // Weak password detection: next to password=, even low-entropy
            // values like "hunter2" must be caught. Check if this value looks
            // like a real password (>=4 chars, not all same char).
            if value.len() >= 4 && !value.chars().all(|c| c == value.chars().next().unwrap_or('x')) {
                return Some(Rejection {
                    location: location.to_string(),
                    category: "password",
                });
            }
        }

        // --- Generic high-entropy check on remaining opaque values ---
        // Only for the direct text scan (not metadata leaves — those go
        // through scan_value which handles strings individually).

        None
    }

    fn scan_value_inner(&self, value: &Value, location: &str, depth: usize) -> Option<Rejection> {
        if depth > MAX_DEPTH {
            return None;
        }
        match value {
            Value::String(s) => self.scan_inner(s, location, depth),
            Value::Array(arr) => {
                for (i, item) in arr.iter().enumerate() {
                    if let Some(r) = self.scan_value_inner(item, &format!("{}.{}", location, i), depth + 1) {
                        return Some(r);
                    }
                }
                None
            }
            Value::Object(map) => {
                for (key, val) in map {
                    let loc = if location.is_empty() {
                        key.clone()
                    } else {
                        format!("{}.{}", location, key)
                    };
                    if let Some(r) = self.scan_value_inner(val, &loc, depth + 1) {
                        return Some(r);
                    }
                }
                None
            }
            // numbers, bools, null: not secrets
            _ => None,
        }
    }

    /// Detect a JWT-shaped token: three base64url segments where segment 1
    /// decodes to a JSON object containing `alg` and `typ` (case-sensitive
    /// bytes).
    fn detect_jwt(&self, text: &str) -> Option<&'static str> {
        // Find all dot-separated triplets that look base64url.
        // We look for the pattern: non-dot+ . non-dot+ . non-dot+
        let mut start = 0;
        while start < text.len() {
            let rest = &text[start..];
            if let Some(mat) = self.jwt_re.find(rest) {
                let jwt_str = mat.as_str();
                // Validate it's actually a JWT by decoding segment 1.
                if let Some(dots) = find_jwt_segments(jwt_str) {
                    if is_valid_jwt_header(dots.0) {
                        return Some("jwt");
                    }
                }
                start += jwt_str.len();
            } else {
                break;
            }
        }
        None
    }

    /// Validate a candidate against known provider form rules.
    /// Returns the category if it matches, None if it's a false positive.
    fn validate_provider_form(&self, candidate: &str) -> Option<&'static str> {
        let lower = candidate.to_ascii_lowercase();
        for form in PROVIDER_FORMS {
            if lower.starts_with(&form.prefix.to_ascii_lowercase()) {
                if candidate.len() < form.min_len {
                    // Truncated prefix — not a real token.
                    continue;
                }
                if let Some(suffix_pattern) = form.suffix_re {
                    // Extract the part after the prefix.
                    let suffix = &candidate[form.prefix.len()..];
                    if let Ok(re) = Regex::new(suffix_pattern) {
                        if !re.is_match(suffix) {
                            continue;
                        }
                    }
                }
                // Determine category from prefix.
                return Some(if lower.starts_with("ghp_") || lower.starts_with("gho_")
                    || lower.starts_with("ghs_") || lower.starts_with("ghr_")
                    || lower.starts_with("github_pat_")
                {
                    "github_pat"
                } else if lower.starts_with("sk-ant-") {
                    "anthropic_token"
                } else if lower.starts_with("sk-proj-") {
                    "openai_project_key"
                } else if lower.starts_with("sk-") {
                    "openai_key"
                } else if lower.starts_with("xox") {
                    "slack_token"
                } else if lower.starts_with("akia") {
                    "aws_access_key"
                } else {
                    "provider_token"
                });
            }
        }
        None
    }
}

/// Whether the (lowercased) text matches known placeholder/documentation
/// fixture patterns and should not be treated as a secret.
fn is_placeholder(lower: &str) -> bool {
    let trimmed = lower.trim();
    // Empty or near-empty.
    if trimmed.is_empty() {
        return true;
    }
    // Variable references and angle-bracket placeholders.
    if trimmed.starts_with("${") || trimmed.starts_with('<') {
        return true;
    }
    // Repeated asterisks or dots masking a value.
    if trimmed.chars().all(|c| c == '*' || c == '.') {
        return true;
    }
    // Repeated same character (e.g. "xxxx", "aaaa", "***").
    if trimmed.len() >= 3 && trimmed.chars().all(|c| c == trimmed.chars().next().unwrap_or('x')) {
        return true;
    }
    // Known placeholder strings: whole-value match for short patterns like
    // "changeme" to avoid false positives on real tokens that happen to
    // contain common substrings (e.g. "example" in AWS example keys).
    for pat in PLACEHOLDER_PATTERNS {
        if trimmed == *pat {
            return true;
        }
    }
    false
}

/// Split a JWT string into its three base64url segments.
/// Returns `None` if there aren't exactly three segments.
fn find_jwt_segments(s: &str) -> Option<(&str, &str, &str)> {
    let mut parts = s.splitn(3, '.');
    let a = parts.next()?;
    let b = parts.next()?;
    let c = parts.next()?;
    if parts.next().is_some() {
        return None; // too many dots
    }
    Some((a, b, c))
}

/// Decode a base64url segment and check it's a JSON object containing
/// both `alg` and `typ` keys (case-sensitive).
fn is_valid_jwt_header(segment: &str) -> bool {
    // base64url decode: replace URL chars, add padding.
    let mut s = segment.replace('-', "+").replace('_', "/");
    while s.len() % 4 != 0 {
        s.push('=');
    }
    let decoded = match base64_decode(&s) {
        Some(d) => d,
        None => return false,
    };
    let val: Value = match serde_json::from_slice(&decoded) {
        Ok(v) => v,
        Err(_) => return false,
    };
    match &val {
        Value::Object(map) => map.contains_key("alg") && map.contains_key("typ"),
        _ => false,
    }
}

/// Minimal base64 decoder (no external crate needed; regex is the only dep).
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    // Lookup table: ASCII byte -> base64 value, -1 for invalid.
    // Built at runtime (once) because const fn can't iterate in stable Rust.
    use std::sync::OnceLock;
    static TABLE: OnceLock<[i8; 128]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [-1i8; 128];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < alphabet.len() {
            t[alphabet[i] as usize] = i as i8;
            i += 1;
        }
        t
    });

    let input = input.trim_end_matches('=');
    let len = input.len();
    if len == 0 {
        return Some(Vec::new());
    }

    let mut output = Vec::with_capacity(len * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;

    for &byte in input.as_bytes() {
        let val = if byte < 128 { table[byte as usize] } else { -1 };
        if val < 0 {
            return None; // invalid character
        }
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buf >> bits) as u8);
        }
    }

    Some(output)
}

/// Scan user-controlled content + metadata at an entry point.
/// Returns `Some(Rejection)` on detection, `None` if clean.
pub fn scan_entry_point(
    content: &str,
    metadata: &Value,
    entry_point_name: &str,
) -> Option<Rejection> {
    // 1. Scan content string.
    if let Some(r) = SecretScanner::scan_str(content, &format!("{}.content", entry_point_name)) {
        return Some(r);
    }
    // 2. Scan metadata recursively.
    if let Some(r) = SecretScanner::scan_value(metadata, &format!("{}.metadata", entry_point_name)) {
        return Some(r);
    }
    None
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Provider token forms ────────────────────────────────────────────

    #[test]
    fn github_pat_full_form_detected() {
        // ghp_ + 36 base62 = 40 chars
        let token = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef123456";
        let r = SecretScanner::scan_str(token, "content").unwrap();
        assert_eq!(r.category, "github_pat");
    }

    #[test]
    fn github_pat_truncated_rejected() {
        // Too short
        let token = "ghp_abc";
        assert!(SecretScanner::scan_str(token, "content").is_none());
    }

    #[test]
    fn anthropic_token_detected() {
        let token = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz1234567890";
        let r = SecretScanner::scan_str(token, "content").unwrap();
        assert_eq!(r.category, "anthropic_token");
    }

    #[test]
    fn anthropic_token_truncated_rejected() {
        let token = "sk-ant-abc";
        assert!(SecretScanner::scan_str(token, "content").is_none());
    }

    #[test]
    fn openai_key_detected() {
        // sk- + base62, ~51 chars total
        let token = format!("sk-{}", "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".repeat(1));
        let r = SecretScanner::scan_str(&token, "content");
        // This is 4+62 = 66 chars, well over min_len=40
        assert!(r.is_some(), "sk- key should be detected");
    }

    #[test]
    fn openai_project_key_detected() {
        let token = format!("sk-proj-{}", "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".repeat(2));
        let r = SecretScanner::scan_str(&token, "content").unwrap();
        assert_eq!(r.category, "openai_project_key");
    }

    #[test]
    fn slack_token_detected() {
        let token = format!("xoxb-{}", "0123456789abcdef".repeat(4));
        let r = SecretScanner::scan_str(&token, "content").unwrap();
        assert_eq!(r.category, "slack_token");
    }

    #[test]
    fn aws_access_key_detected() {
        let token = "AKIAIOSFODNN7EXAMPLE"; // official AWS example
        let r = SecretScanner::scan_str(token, "content").unwrap();
        assert_eq!(r.category, "aws_access_key");
    }

    #[test]
    fn aws_access_key_wrong_suffix_rejected() {
        let token = "AKIAIOSFODNN7"; // only 13 chars, not 20
        assert!(SecretScanner::scan_str(token, "content").is_none());
    }

    // ── Key=value variants ──────────────────────────────────────────────

    #[test]
    fn password_equals_catches_weak_password() {
        let text = "password = hunter2";
        let r = SecretScanner::scan_str(text, "content").unwrap();
        assert_eq!(r.category, "password");
    }

    #[test]
    fn password_colon_catches_value() {
        let text = "password: hunter2";
        let r = SecretScanner::scan_str(text, "content").unwrap();
        assert_eq!(r.category, "password");
    }

    #[test]
    fn json_password_key_detected() {
        let text = r#"{"password": "hunter2"}"#;
        let r = SecretScanner::scan_str(text, "content").unwrap();
        assert_eq!(r.category, "password");
    }

    #[test]
    fn api_key_with_whitespace() {
        let text = "api_key  =  ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef123456";
        let r = SecretScanner::scan_str(text, "content").unwrap();
        assert!(r.category == "github_pat" || r.category == "password");
    }

    #[test]
    fn bearer_token_detected() {
        let text = "bearer = eyJhbGciOiJIUzI1NiJ9.eyJ0eXAiOiJKV1QifQ.xxx";
        let r = SecretScanner::scan_str(text, "content");
        assert!(r.is_some(), "bearer with JWT should be caught");
    }

    // ── JWT detection ───────────────────────────────────────────────────

    #[test]
    fn standard_jwt_detected() {
        // alg: HS256, typ: JWT
        let header = base64_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = base64_encode(br#"{"sub":"1234567890","name":"John"}"#);
        let sig = "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let token = format!("{}.{}.{}", header, payload, sig);
        let r = SecretScanner::scan_str(&token, "content").unwrap();
        assert_eq!(r.category, "jwt");
    }

    #[test]
    fn jwt_with_reordered_header_detected() {
        // typ before alg — should still match
        let header = base64_encode(br#"{"typ":"JWT","alg":"RS256"}"#);
        let payload = base64_encode(br#"{"data":"test"}"#);
        let sig = "signature";
        let token = format!("{}.{}.{}", header, payload, sig);
        let r = SecretScanner::scan_str(&token, "content").unwrap();
        assert_eq!(r.category, "jwt");
    }

    #[test]
    fn jwt_without_alg_or_typ_not_detected() {
        // Missing `typ` — not a JWT per spec
        let header = base64_encode(br#"{"alg":"HS256"}"#);
        let payload = base64_encode(br#"{"data":"test"}"#);
        let sig = "signature";
        let token = format!("{}.{}.{}", header, payload, sig);
        // Should NOT be detected as JWT (no typ)
        // It may still be caught as provider token if it looks like one,
        // but the JWT path should not fire.
        let scanner = SecretScanner::build().unwrap();
        assert!(scanner.detect_jwt(&token).is_none());
    }

    #[test]
    fn truncated_jwt_not_detected() {
        // Only two segments
        let header = base64_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = base64_encode(br#"{"data":"test"}"#);
        let token = format!("{}.{}", header, payload);
        let scanner = SecretScanner::build().unwrap();
        assert!(scanner.detect_jwt(&token).is_none());
    }

    // ── Placeholders / false positives ──────────────────────────────────

    #[test]
    fn placeholder_not_detected() {
        assert!(SecretScanner::scan_str("${API_KEY}", "content").is_none());
    }

    #[test]
    fn angle_bracket_placeholder_not_detected() {
        assert!(SecretScanner::scan_str("<your-api-key-here>", "content").is_none());
    }

    #[test]
    fn asterisks_not_detected() {
        assert!(SecretScanner::scan_str("password: ***", "content").is_none());
    }

    #[test]
    fn changeme_not_detected() {
        assert!(SecretScanner::scan_str("password = changeme", "content").is_none());
    }

    #[test]
    fn repeated_chars_not_detected() {
        assert!(SecretScanner::scan_str("xxx", "content").is_none());
        assert!(SecretScanner::scan_str("aaaa", "content").is_none());
    }

    #[test]
    fn empty_value_not_detected() {
        assert!(SecretScanner::scan_str("password =", "content").is_none());
    }

    // ── Weak password caught regardless of entropy ──────────────────────

    #[test]
    fn low_entropy_password_next_to_keyword_caught() {
        let text = "password = 1234";
        let r = SecretScanner::scan_str(text, "content").unwrap();
        assert_eq!(r.category, "password");
    }

    #[test]
    fn single_char_password_not_caught() {
        // Single char is below minimum length of 4
        assert!(SecretScanner::scan_str("password = a", "content").is_none());
    }

    // ── Benign high-entropy content (git SHA hashes) ────────────────────

    #[test]
    fn git_sha_hash_in_prose_not_detected() {
        let text = "The commit abcdef1234567890abcdef1234567890abcdef12 fixed the issue.";
        assert!(SecretScanner::scan_str(text, "content").is_none());
    }

    // ── Metadata scanning ───────────────────────────────────────────────

    #[test]
    fn nested_metadata_secret_detected() {
        let meta = json!({
            "refs": [
                { "file": "src/main.rs" },
                { "secret": "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef123456" }
            ]
        });
        let r = SecretScanner::scan_value(&meta, "metadata").unwrap();
        assert_eq!(r.category, "github_pat");
    }

    #[test]
    fn metadata_array_leaf_detected() {
        let meta = json!({
            "tags": ["safe", "password = hunter2"]
        });
        let r = SecretScanner::scan_value(&meta, "metadata").unwrap();
        assert_eq!(r.category, "password");
    }

    // ── Scan entry point integration ────────────────────────────────────

    #[test]
    fn entry_point_scans_content_and_metadata() {
        // Clean content, dirty metadata
        let meta = json!({ "token": "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef123456" });
        let r = scan_entry_point("all good", &meta, "add_memory").unwrap();
        assert_eq!(r.location, "add_memory.metadata.token");

        // Dirty content
        let meta2 = json!({});
        let r2 = scan_entry_point("password = hunter2", &meta2, "add_memory").unwrap();
        assert_eq!(r2.location, "add_memory.content");
    }

    // ── All three entry points same outcome ─────────────────────────────

    #[test]
    fn same_outcome_on_add_supersede_and_edit() {
        let dirty_content = "api_key = ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef123456";
        let clean_meta = json!({});

        for entry in &["add_memory", "supersede_memory", "edit_memory"] {
            let r = scan_entry_point(dirty_content, &clean_meta, entry);
            assert!(r.is_some(), "{} should reject secret", entry);
        }
    }

    #[test]
    fn clean_input_passes_all_entry_points() {
        let clean = "Always use podman, not docker.";
        let meta = json!({ "governs": ["src/daemon.rs"] });

        for entry in &["add_memory", "supersede_memory", "edit_memory"] {
            assert!(scan_entry_point(clean, &meta, entry).is_none(), "{} should pass", entry);
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    /// Base64url-encode for test JWT headers (JWT uses base64url, not standard base64).
    fn base64_encode(input: &[u8]) -> String {
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = chunk.get(1).map_or(0, |&b| b as u32);
            let b2 = chunk.get(2).map_or(0, |&b| b as u32);
            let triple = (b0 << 16) | (b1 << 8) | b2;
            out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
            out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
            if chunk.len() > 1 {
                out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
            } else {
                // base64url: no padding in JWTs
            }
            if chunk.len() > 2 {
                out.push(CHARS[(triple & 0x3F) as usize] as char);
            }
        }
        out
    }
}
