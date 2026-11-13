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
        write!(
            f,
            "ERROR [SECURITY]: Rejected {} ({}). Do not store secrets.",
            self.location, self.category
        )
    }
}

/// The compiled scanner: a set of pre-compiled regexes and a set of provider
/// form recognisers. Built once and reused across every request.
pub(crate) struct SecretScanner {
    provider_re: Regex,
    kv_re: Regex,
    jwt_re: Regex,
    he_re: Regex,
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
/// Overlap between scan chunks so a token that straddles a chunk boundary is
/// still seen whole. Provider tokens are far shorter than this.
const SCAN_CHUNK_OVERLAP: usize = 64 * 1024;
/// Carry a little context across a line boundary so a key on one line and its
/// value on the next are still scanned together. Tokens themselves never span
/// a newline, so this is small; the 64 KB overlap is only for long single
/// lines with no boundary to use.
const SCAN_CONTEXT_CARRY: usize = 4 * 1024;

/// A value captured next to a secret-like key is a source expression, not a
/// credential literal, when it is unquoted and shaped like code. Skipping
/// these avoids treating `let token = read_token();` as a password while
/// quoted literals and plain opaque values still go through the password
/// check.
fn looks_like_code_expression(value: &str) -> bool {
    // Strong source markers: path/call syntax and code-only roots. A config
    // value such as `password=correct.horse` does not qualify; member chains
    // and index expressions need a source-statement prefix, below.
    value.contains("::")
        || value.contains("->")
        || value.contains("=>")
        || (value.contains('(') && value.contains(')'))
        || value.starts_with("self.")
        || value.starts_with("&self.")
        || value.starts_with("process.env.")
}

/// Member chains and index expressions are exempt only inside a source
/// statement: `let token = response.token` yes, `password=correct.horse` no.
fn looks_like_source_reference(value: &str) -> bool {
    (!value.starts_with("eyJ") && looks_like_member_chain(value))
        || (value.contains('[') && !value.starts_with('[') && !value.contains(']'))
}

/// True when the key begins a source statement/comment, so code-shaped values
/// can be exempted without exempting the same shapes in a `.env` file.
fn source_statement_prefix(text: &str, key_start: usize) -> bool {
    let line_start = text[..key_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let prefix = text[line_start..key_start].trim_start();
    [
        "let ", "const ", "var ", "return ", "if ", "match ", "for ", "while ",
        "pub ", "fn ", "struct ", "enum ", "impl ", "type ",
    ]
    .iter()
    .any(|p| prefix.starts_with(p))
}

/// `self.token`, `response.token`, `process.env.SECRET` are member chains; a
/// password such as `p@ss.w0rd` is not because a segment contains `@`.
fn looks_like_member_chain(value: &str) -> bool {
    if !value.contains('.') {
        return false;
    }
    let mut saw_alpha = false;
    for segment in value.split('.') {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return false;
        }
        if let Some(first) = segment.chars().next() {
            saw_alpha |= first.is_ascii_alphabetic();
        }
    }
    saw_alpha
}

/// A value captured next to a secret-like key may be a Rust type annotation
/// (`api_key: String`, `token: Option<PathBuf>`) rather than a credential.
fn is_known_type_name(value: &str) -> bool {
    const TYPES: &[&str] = &[
        "String", "str", "bool", "usize", "u64", "u32", "u16", "u8", "i64", "i32",
        "i16", "i8", "f64", "f32", "PathBuf", "Path", "Option", "Result", "Vec",
        "Arc", "Rc", "Box", "Mutex", "RwLock", "SecretString",
    ];
    TYPES.iter().any(|ty| {
        value == *ty
            || value
                .strip_prefix(ty)
                .map(|rest| rest.starts_with('<') || rest.starts_with("::"))
                .unwrap_or(false)
    })
}

fn looks_like_type_annotation(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    if v.contains('<') && v.contains('>') {
        return true;
    }
    if let Some(rest) = v.strip_prefix('&') {
        let mut rest = rest.trim_start();
        if let Some(after_quote) = rest.strip_prefix('\'') {
            rest = after_quote
                .split_once(' ')
                .map(|(_, r)| r.trim_start())
                .unwrap_or(after_quote);
        }
        rest = rest.strip_prefix("mut ").unwrap_or(rest);
        if let Some(inner) = rest.strip_prefix('[') {
            return inner
                .split([';', ']'])
                .next()
                .map(|t| is_known_type_name(t.trim()))
                .unwrap_or(false);
        }
        if let Some(dynamic) = rest.strip_prefix("dyn ") {
            return dynamic
                .chars()
                .next()
                .map(|c| c.is_ascii_uppercase())
                .unwrap_or(false);
        }
        return is_known_type_name(rest);
    }
    if let Some(inner) = v.strip_prefix('[') {
        return inner
            .split([';', ']'])
            .next()
            .map(|t| is_known_type_name(t.trim()))
            .unwrap_or(false);
    }
    if let Some(dynamic) = v.strip_prefix("dyn ") {
        return dynamic
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false);
    }
    is_known_type_name(v)
}

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
        let jwt_re = Regex::new(r"eyJ[A-Za-z0-9_\-]+\.eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+")?;

        // High-entropy assignment check: secret-like keys (secret, password,
        // passwd, api_key, apikey, access_key, client_secret, private_key)
        // followed by '=' or ':' and a long opaque value (no whitespace, no
        // path separators). Entropy/class-length validation happens in Rust.
        let he_re = Regex::new(
            r#"(?i)(?:secret|password|passwd|api[_-]?key|access[_-]?key|client[_-]?secret|private[_-]?key)["']?\s*[=:]\s*["']?([^"'\s\\/]{24,})"#,
        )?;

        Ok(Self {
            provider_re,
            kv_re,
            jwt_re,
            he_re,
        })
    }

    fn instance() -> &'static SecretScanner {
        static SCANNER: OnceLock<SecretScanner> = OnceLock::new();
        // Fail closed: if a built-in regex fails to compile this must panic at
        // first use, never silently disable the scanner.
        SCANNER.get_or_init(|| {
            SecretScanner::build().expect("built-in secret scanner regexes must compile")
        })
    }

    /// Scan a string for secrets. Returns `None` if clean or the scanner is
    /// disabled, `Some(Rejection)` with category + location on detection.
    pub fn scan_str(text: &str, location: &str) -> Option<Rejection> {
        let scanner = Self::instance();
        scanner.scan_chunked(text, location)
    }

    /// Scan arbitrarily large text in overlapping chunks. A whole-file
    /// rejection for size would silently discard large clean source files
    /// (this repository's `src/server.rs` is ~684 KB); secrets are found
    /// where they are, and a clean large file is admitted.
    fn scan_chunked(&self, text: &str, location: &str) -> Option<Rejection> {
        if text.len() <= MAX_SCAN_BYTES {
            return self.scan_inner(text, location);
        }
        let mut start = 0usize;
        while start < text.len() {
            let mut end = (start + MAX_SCAN_BYTES).min(text.len());
            while end > start && !text.is_char_boundary(end) {
                end -= 1;
            }
            if end <= start {
                break;
            }
            // Prefer a line boundary: a JWT or provider token never spans a
            // newline, so ending the chunk at one keeps the whole candidate in
            // the next chunk regardless of its length.
            let mut used_newline = false;
            if end < text.len() {
                if let Some(nl) = text[start..end].rfind('\n') {
                    let candidate = start + nl + 1;
                    if candidate - start > MAX_SCAN_BYTES / 2 {
                        end = candidate;
                        used_newline = true;
                    }
                }
            }
            if let Some(r) = self.scan_inner(&text[start..end], location) {
                return Some(r);
            }
            if end == text.len() {
                break;
            }
            if used_newline {
                // Keep a little context so `key =` at the end of one chunk and
                // its value on the next line still form a kv_re match.
                let next = end.saturating_sub(SCAN_CONTEXT_CARRY);
                start = if next > start { next } else { end };
            } else {
                let next = end.saturating_sub(SCAN_CHUNK_OVERLAP);
                start = if next > start { next } else { end };
            }
            while start < text.len() && !text.is_char_boundary(start) {
                start += 1;
            }
        }
        None
    }

    /// Scan a JSON value recursively (metadata leaves), returning the first
    /// rejection found.
    pub fn scan_value(value: &Value, location: &str) -> Option<Rejection> {
        let scanner = Self::instance();
        scanner.scan_value_inner(value, location, 0)
    }

    fn scan_inner(&self, text: &str, location: &str) -> Option<Rejection> {
        if text.len() > MAX_SCAN_BYTES {
            return Some(Rejection {
                location: location.to_string(),
                category: "oversized_input",
            });
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

        // --- High-entropy assignment-style secrets ---
        // Runs BEFORE the key=value phase so genuinely high-entropy opaque
        // values next to secret-like keys get the precise
        // `high_entropy_secret` detector; shorter/low-entropy values still
        // fall through to the weak-password check below.
        for cap in self.he_re.captures_iter(text) {
            let token = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if is_placeholder(&token.to_ascii_lowercase()) {
                continue;
            }
            if meets_high_entropy(token) {
                return Some(Rejection {
                    location: location.to_string(),
                    category: "high_entropy_secret",
                });
            }
        }

        // --- Key=value extraction ---
        for cap in self.kv_re.captures_iter(text) {
            let value = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            let value_lower = value.to_ascii_lowercase();

            if is_placeholder(&value_lower) {
                continue;
            }

            // The capture stops at `;`, so a quoted value containing `;` has no
            // closing quote at `m.end()`; the opening quote is the reliable
            // marker that this is a literal.
            let quoted = cap.get(2).map(|m| {
                matches!(text[..m.start()].chars().last(), Some('"') | Some('\''))
            }).unwrap_or(false);
            // Type annotations use `:`. A config value after `=` (for example
            // `password=&hunter2!`) is never a type annotation, even if it
            // starts with a reference sigil.
            let type_context = cap
                .get(1)
                .zip(cap.get(2))
                .map(|(key, val)| {
                    let sep = text[key.end()..val.start()].trim();
                    sep.starts_with(':') && !sep.starts_with("::")
                })
                .unwrap_or(false);
            let source_context = cap
                .get(1)
                .map(|key| source_statement_prefix(text, key.start()))
                .unwrap_or(false);
            let literal_keyword = matches!(value, "None" | "true" | "false")
                || value.starts_with("&self.");
            if !quoted
                && (literal_keyword
                    || looks_like_code_expression(value)
                    || (source_context && looks_like_source_reference(value))
                    || (type_context && looks_like_type_annotation(value)))
            {
                continue;
            }

            // Weak password detection: next to password=, even low-entropy
            // values like "hunter2" must be caught. Check if this value looks
            // like a real password (>=4 chars, not all same char).
            if value.len() >= 4
                && !value
                    .chars()
                    .all(|c| c == value.chars().next().unwrap_or('x'))
            {
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
            // Fail closed: a deep structure we cannot fully scan is rejected,
            // not silently passed.
            return Some(Rejection {
                location: location.to_string(),
                category: "max_depth_exceeded",
            });
        }
        match value {
            // A metadata leaf over the scanner's window cannot be scanned as
            // one string; say so distinctly instead of reporting a generic
            // oversized input, so the caller can fix the field it names.
            Value::String(s) if s.len() > MAX_SCAN_BYTES => Some(Rejection {
                location: location.to_string(),
                category: "oversized_metadata",
            }),
            Value::String(s) => self.scan_inner(s, location),
            Value::Array(arr) => {
                for (i, item) in arr.iter().enumerate() {
                    if let Some(r) =
                        self.scan_value_inner(item, &format!("{}.{}", location, i), depth + 1)
                    {
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
                    // A key is user-controlled too: `{"ghp_…": "x"}` is a
                    // credential the value scan would never see. The location
                    // names the field's position without echoing the key: a
                    // rejection is logged and returned to the caller, so it
                    // must not repeat the credential.
                    let key_loc = format!("{location}.<key>");
                    if let Some(r) = self.scan_inner(key, &key_loc) {
                        return Some(r);
                    }
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
                return Some(
                    if lower.starts_with("ghp_")
                        || lower.starts_with("gho_")
                        || lower.starts_with("ghs_")
                        || lower.starts_with("ghr_")
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
                    },
                );
            }
        }
        None
    }
}

/// Whether a candidate value token looks like a real high-entropy secret:
/// length >= 24 chars, mixed character classes (digits combined with
/// uppercase/lowercase letters), and Shannon entropy >= 4.2 bits/char.
fn meets_high_entropy(token: &str) -> bool {
    if token.chars().count() < 24 {
        return false;
    }
    let mut has_digit = false;
    let mut has_letter = false;
    for c in token.chars() {
        if c.is_ascii_digit() {
            has_digit = true;
        } else if c.is_uppercase() || c.is_lowercase() {
            has_letter = true;
        }
    }
    if !(has_digit && has_letter) {
        return false;
    }
    shannon_entropy_bits_per_char(token).map_or(false, |h| h >= 4.2)
}

/// Shannon entropy of `s` in bits per character.
fn shannon_entropy_bits_per_char(s: &str) -> Option<f64> {
    let len = s.chars().count();
    if len == 0 {
        return None;
    }
    let mut counts: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0) += 1;
    }
    let len = len as f64;
    Some(
        counts
            .values()
            .map(|c| {
                let p = *c as f64 / len;
                -p * p.log2()
            })
            .sum(),
    )
}

/// Whether the (lowercased) text matches known placeholder/documentation
/// fixture patterns and should not be treated as a secret.
fn is_placeholder(lower: &str) -> bool {
    let trimmed = lower.trim();
    // Empty or near-empty.
    if trimmed.is_empty() {
        return true;
    }
    // Variable references and angle-bracket placeholders. Only single,
    // short, one-line placeholder tokens are excluded: a document that
    // merely begins with `<` (e.g. `<config>password = ...</config>`) or
    // `${` must still reach the real detectors, not bypass the scanner.
    if trimmed.starts_with('<')
        && trimmed.ends_with('>')
        && trimmed.len() <= 64
        && !trimmed[1..trimmed.len() - 1].contains('<')
        && !trimmed[1..trimmed.len() - 1].contains('>')
        && !trimmed.contains('\n')
    {
        return true;
    }
    if trimmed.starts_with("${")
        && trimmed.ends_with('}')
        && trimmed.len() <= 64
        && !trimmed[2..trimmed.len() - 1].contains('}')
        && !trimmed.contains('\n')
    {
        return true;
    }
    // Repeated asterisks or dots masking a value.
    if trimmed.chars().all(|c| c == '*' || c == '.') {
        return true;
    }
    // Repeated same character (e.g. "xxxx", "aaaa", "***").
    if trimmed.len() >= 3
        && trimmed
            .chars()
            .all(|c| c == trimmed.chars().next().unwrap_or('x'))
    {
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
    let mut parts = s.split('.');
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
    if let Some(r) = SecretScanner::scan_value(metadata, &format!("{}.metadata", entry_point_name))
    {
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
        let token = format!(
            "sk-{}",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".repeat(1)
        );
        let r = SecretScanner::scan_str(&token, "content");
        // This is 4+62 = 66 chars, well over min_len=40
        assert!(r.is_some(), "sk- key should be detected");
    }

    #[test]
    fn openai_project_key_detected() {
        let token = format!(
            "sk-proj-{}",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".repeat(2)
        );
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

    // ── High-entropy assignment-style secrets ───────────────────────────

    #[test]
    fn high_entropy_assignment_detected() {
        // 30 unique mixed-class chars: ~4.9 bits/char.
        let token = "aB3xZ9kQ7wE2rT8uI1oP6pM4nC5vJ0";
        let text = format!("api_key = {}", token);
        let r = SecretScanner::scan_str(&text, "content").unwrap();
        assert_eq!(r.category, "high_entropy_secret");
    }

    #[test]
    fn high_entropy_low_entropy_value_falls_back_to_password() {
        // >=24 chars but (almost) all the same character → entropy below 4.2.
        let r = SecretScanner::scan_str("api_key = aaaaaaaaaaaaaaaaaaaaaaaaaab1", "content")
            .unwrap();
        assert_eq!(r.category, "password");
    }

    #[test]
    fn short_value_not_high_entropy_detected() {
        // 20 chars, below the 24-char threshold.
        assert!(SecretScanner::scan_str("api_key = aB3xZ9kQ7wE2rT8uI1oP", "content").is_none()
            || SecretScanner::scan_str("api_key = aB3xZ9kQ7wE2rT8uI1oP", "content")
                .map(|r| r.category == "password")
                .unwrap_or(false));
    }

    #[test]
    fn quoted_high_entropy_value_detected() {
        let token = "aB3xZ9kQ7wE2rT8uI1oP6pM4nC5vJ0";
        let text = format!(r#"{{"client_secret": "{}"}}"#, token);
        let r = SecretScanner::scan_str(&text, "content").unwrap();
        assert_eq!(r.category, "high_entropy_secret");
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

    /// A credential can be the key, not only a value.
    #[test]
    fn metadata_key_carrying_a_secret_is_detected() {
        let mut map = serde_json::Map::new();
        map.insert(
            "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef123456".to_string(),
            json!("benign"),
        );
        let r = SecretScanner::scan_value(&Value::Object(map), "metadata").unwrap();
        assert_eq!(r.category, "github_pat");
        assert!(r.location.contains("<key>"), "the location names the key: {}", r.location);
        assert!(
            !r.to_string().contains("ghp_"),
            "the rejection must not echo the credential-bearing key: {r}"
        );
    }

    /// A metadata leaf over the scan window has its own reason, so the caller
    /// can fix the named field instead of seeing a generic oversized input.
    #[test]
    fn oversized_metadata_leaf_has_a_distinct_reason() {
        let meta = json!({ "note": "a".repeat(MAX_SCAN_BYTES + 1) });
        let r = SecretScanner::scan_value(&meta, "metadata").unwrap();
        assert_eq!(r.category, "oversized_metadata");
        assert_eq!(r.location, "metadata.note");
    }

    /// A long single line has no newline to chunk on; the overlap must still
    /// contain a token that straddles the scan boundary whole. A JWT needs all
    /// three segments, so a split at the boundary cannot be rescued by any
    /// other detector.
    #[test]
    fn single_line_token_straddling_a_chunk_boundary_is_detected() {
        let header = base64_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = base64_encode(br#"{"sub":"1234567890","name":"John"}"#);
        let jwt = format!("{}.{}.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c", header, payload);
        let mut text = "a".repeat(MAX_SCAN_BYTES - 10);
        text.push_str(&jwt);
        assert!(
            text.len() > MAX_SCAN_BYTES,
            "the fixture crosses the chunk window"
        );
        let r = SecretScanner::scan_str(&text, "content").unwrap();
        assert_eq!(r.category, "jwt");
    }

    #[test]
    fn overly_deep_metadata_rejected() {
        let mut v = json!({ "leaf": "fine" });
        for _ in 0..12 {
            v = json!({ "nested": v });
        }
        let r = SecretScanner::scan_value(&v, "metadata");
        assert_eq!(r.map(|r| r.category), Some("max_depth_exceeded"));
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
            assert!(
                scan_entry_point(clean, &meta, entry).is_none(),
                "{} should pass",
                entry
            );
        }
    }

    // ── Scanner bypass regression (AN-S1) ───────────────────────────────

    #[test]
    fn document_starting_with_angle_bracket_not_bypassing_scanner() {
        // A full XML/HTML/Markdown document that happens to start with `<`
        // must not disable the scanner; the embedded secret is rejected.
        // A document that starts with `${` likewise.
        let xml = r#"<config>api_key = "sk-live-1234567890abcdefghijklmnop"</config>"#;
        let r = SecretScanner::scan_str(xml, "content");
        assert!(r.is_some(), "secret inside <config> document must be rejected");
        assert!(
            matches!(
                r.as_ref().map(|r| r.category),
                Some("high_entropy_secret") | Some("password") | Some("openai_key")
            ),
            "unexpected category: {:?}",
            r.map(|r| r.category)
        );

        // While single placeholder tokens are still allowed.
        assert!(SecretScanner::scan_str("<your-api-key>", "content").is_none());
        assert!(SecretScanner::scan_str("${API_KEY}", "content").is_none());
    }

    // ── Large inputs ────────────────────────────────────────────────────

    #[test]
    fn large_clean_text_is_scanned_not_rejected_for_size() {
        let text = "a".repeat(MAX_SCAN_BYTES + 10_000);
        assert!(
            SecretScanner::scan_str(&text, "content").is_none(),
            "a clean file larger than the chunk size must not be rejected as oversized"
        );
    }

    #[test]
    fn large_text_with_a_secret_across_a_chunk_boundary_is_detected() {
        let mut text = "a".repeat(MAX_SCAN_BYTES - 10);
        // Built at runtime so no secret-shaped literal sits in the source.
        text.push_str(&format!(" ghp_{}", "Ab1".repeat(12)));
        text.push_str(&"b".repeat(10_000));
        let rejection = SecretScanner::scan_str(&text, "content").expect("secret detected");
        assert_eq!(rejection.category, "github_pat");
    }

    #[test]
    fn source_assignments_from_functions_are_not_secrets() {
        let text = "let token = read_token();\n\
                    let password = generate_password();\n\
                    let key = crate::daemon::read_token_file(path).await?;";
        assert!(
            SecretScanner::scan_str(text, "content").is_none(),
            "source expressions are not credential literals"
        );
    }

    #[test]
    fn source_type_annotations_are_not_probed_as_passwords() {
        let text = "struct Config {\n\
                    api_key: String,\n\
                    token: String,\n\
                    path: Option<PathBuf>,\n\
                    secret: &'a str,\n\
                    bytes: [u8; 32],\n\
                    }";
        assert!(
            SecretScanner::scan_str(text, "content").is_none(),
            "type annotations are not credential values"
        );
    }

    #[test]
    fn quoted_password_literal_is_still_detected() {
        let text = "let password = \"s3cr3t-value\";";
        let rejection = SecretScanner::scan_str(text, "content").expect("quoted literal");
        assert_eq!(rejection.category, "password");
    }

    #[test]
    fn long_jwt_across_a_line_boundary_is_detected() {
        let header = base64_encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        // The payload is far longer than the old 4 KB overlap; a line-aware
        // chunk boundary must keep the whole token in one scan.
        let payload = format!("eyJ{}", "A".repeat(200_000));
        let sig = "B".repeat(200);
        let jwt = format!("{header}.{payload}.{sig}");
        let mut text = "x".repeat(MAX_SCAN_BYTES - 100);
        text.push('\n');
        text.push_str(&jwt);
        text.push('\n');
        text.push_str(&"y".repeat(1_000));
        let rejection = SecretScanner::scan_str(&text, "content").expect("long JWT detected");
        assert_eq!(rejection.category, "jwt");
    }

    #[test]
    fn plaintext_config_values_with_punctuation_are_still_detected() {
        for text in [
            "password=hunter2!",
            "password = \"hunter2!;\"",
            "api_key=abc(def",
            "password=&hunter2!",
            "password=[hunter2]",
            "password: &hunter2!",
        ] {
            let rejection = SecretScanner::scan_str(text, "content")
                .unwrap_or_else(|| panic!("plaintext credential must be detected: {text}"));
            assert!(
                rejection.category == "password" || rejection.category == "high_entropy_secret",
                "unexpected category for {text}: {}",
                rejection.category
            );
        }
    }

    #[test]
    fn multiline_assignment_across_a_chunk_boundary_is_detected() {
        // The first chunk ends on the newline after `password =`; the value is
        // on the next line and must still be associated with its key.
        let mut text = "x".repeat(MAX_SCAN_BYTES - 100);
        text.push_str("let password =\n");
        text.push_str(&" ".repeat(200));
        text.push_str("\"hunter2\";\n");
        text.push_str(&" ".repeat(1_000));
        let rejection =
            SecretScanner::scan_str(&text, "content").expect("multiline assignment detected");
        assert_eq!(rejection.category, "password");
    }

    #[test]
    fn secret_source_expression_matrix() {
        for clean in [
            "let token = self.token;",
            "token=None",
            "const secret = process.env.SECRET;",
            "// token = None",
            "let value = parts[0];",
            "let token = response.token;",
            "struct Cfg { api_key: &self.api_key }",
            "    token: normalize_query_token(&stem),",
            "    secret: process.env.SECRET,",
        ] {
            assert!(
                SecretScanner::scan_str(clean, "content").is_none(),
                "clean source shape was rejected: {clean}"
            );
        }
        for literal in [
            "let password = \"hunter2!\";",
            "// password = \"hunter2!\"",
            "PASSWORD=hunter2!",
            "password=correct.horse",
            "PASSWORD=hunter2[abc",
            "// password=correct.horse",
            "# password=hunter2[abc",
        ] {
            assert!(
                SecretScanner::scan_str(literal, "content").is_some(),
                "credential literal was accepted: {literal}"
            );
        }
    }

    #[test]
    fn placeholder_wrapped_token_rejected() {
        let token = format!("ghp_{}", "Ab1".repeat(12));
        for wrapped in [format!("<{token}>"), format!("${{{token}}}")] {
            let rejection = SecretScanner::scan_str(&wrapped, "content")
                .unwrap_or_else(|| panic!("wrapped token accepted: {wrapped}"));
            assert_eq!(rejection.category, "github_pat");
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
