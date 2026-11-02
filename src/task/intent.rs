//! Deterministic lexical task-intent detector (Mechanism 2).
//!
//! Zero-latency, 100% deterministic. Scans text for high-signal task phrases
//! and emits an advisory `TaskIntentSignal` per distinct marker matched. Never
//! blocks the caller; the nudge is advisory only.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskIntentSignal {
    pub marker: &'static str,
    pub excerpt: String,
}

/// Closed table of high-signal task phrases (lowercase). Order is the
/// emission order.
// (match_str, canonical_marker). match_str may carry a trailing space for
// word-boundary safety; canonical_marker is what gets emitted (trimmed).
const MARKERS: &[(&str, &str)] = &[
    ("todo:", "todo:"),
    ("fixme:", "fixme:"),
    ("follow-up:", "follow-up:"),
    ("followup:", "followup:"),
    ("next step:", "next step:"),
    ("action item:", "action item:"),
    ("remaining work:", "remaining work:"),
    ("we should also ", "we should also"),
    ("later we need to ", "later we need to"),
    ("need to follow up", "need to follow up"),
    ("still need to ", "still need to"),
    ("must also fix ", "must also fix"),
    ("filed follow-up", "filed follow-up"),
    ("defer ", "defer"),
    ("deferred ", "deferred"),
];

const NEGATION_PREFIXES: &[&str] = &["never ", "do not ", "don't ", "do not, "];

/// Largest byte offset `i <= max_bytes.min(s.len())` such that
/// `s.is_char_boundary(i)` — never slices a multi-byte UTF-8 codepoint.
fn safe_prefix(s: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Case-insensitive substring search using ASCII case folding only, so the
/// returned byte offset is always a char boundary of the ORIGINAL haystack
/// (no `to_lowercase()` re-encoding that can shift byte offsets under
/// multi-byte characters like "→", "İ", or emoji). All markers are ASCII,
/// so ASCII folding matches everything a Unicode fold would for them.
fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    for start in 0..=hb.len().saturating_sub(nb.len()) {
        if !haystack.is_char_boundary(start) {
            continue;
        }
        if (0..nb.len()).all(|j| {
            hb[start + j].to_ascii_lowercase() == nb[j].to_ascii_lowercase()
        }) {
            return Some(start);
        }
    }
    None
}

/// Detect task-shaped statements in `text`. Case-insensitive substring match
/// on clause boundaries. Returns one signal per distinct marker matched (in
/// table order), with the trimmed clause (capped at 160 chars) as `excerpt`.
pub fn detect_task_intents(text: &str) -> Vec<TaskIntentSignal> {
    let mut out: Vec<TaskIntentSignal> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for &(match_str, canonical) in MARKERS {
        // find first occurrence (case-insensitive, char-boundary aligned) of
        // match_str directly in the original text
        let idx = match find_ascii_ci(text, match_str) {
            Some(i) => i,
            None => continue,
        };
        if seen.contains(canonical) {
            continue;
        }
        // negation guard: skip if the clause containing the marker starts with
        // a negation prefix (normative "never"/"do not" rule statements).
        let clause_start = text[..idx]
            .rfind('\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        // also consider sentence boundary ". "
        let mut start = clause_start;
        if let Some(pos) = text[..idx].rfind(". ") {
            if pos + 2 > start {
                start = pos + 2;
            }
        }
        let clause_prefix = text[start..idx].trim_start().to_lowercase();
        let negated = NEGATION_PREFIXES
            .iter()
            .any(|p| clause_prefix.starts_with(p));
        if negated {
            continue;
        }

        // excerpt: from marker to end of clause, capped 160
        let from = idx;
        let rest = &text[from..];
        let end = rest
            .find('\n')
            .unwrap_or(rest.len())
            .min(160);
        let mut excerpt: String = safe_prefix(rest, end).trim_end().to_string();
        if excerpt.chars().count() > 160 {
            excerpt = excerpt.chars().take(160).collect();
        }
        // also trim trailing sentence if ". " appears within cap
        // keep simple: already capped.

        seen.insert(canonical);
        out.push(TaskIntentSignal {
            marker: canonical,
            excerpt,
        });
    }
    // stable order = MARKERS order (already iterated in order)
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_later_we_need_to() {
        let sigs = detect_task_intents("Fixed auth; later we need to rotate the signing key");
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].marker, "later we need to");
    }

    #[test]
    fn no_markers_returns_empty() {
        let sigs = detect_task_intents("Refactored parser");
        assert!(sigs.is_empty());
    }

    #[test]
    fn detects_todo() {
        let sigs = detect_task_intents("TODO: add rate limiting to /mcp");
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].marker, "todo:");
    }

    #[test]
    fn negated_rule_skipped() {
        let sigs = detect_task_intents("Never TODO: do this");
        assert!(sigs.is_empty());
    }

    #[test]
    fn short_inputs_never_panic() {
        assert!(detect_task_intents("").is_empty());
        assert!(detect_task_intents("a").is_empty());
        assert!(detect_task_intents("TODO").is_empty());
        assert_eq!(
            detect_task_intents("TODO:").len(),
            1,
            "haystack exactly needle-length must match"
        );
        assert!(detect_task_intents("→ later").is_empty());
    }

    #[test]
    fn multibyte_excerpt_does_not_panic() {
        // "TODO: " occupies 6 bytes; 85 x ("İ" + "→") pushes total length far
        // past byte 160, and byte 160 lands mid-codepoint — previously this
        // sliced `text[..160]` on a non-boundary and panicked.
        let filler = "İ→".repeat(85);
        assert!(filler.len() > 160);
        let text = format!("TODO: {filler}");
        let sigs = detect_task_intents(&text);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].marker, "todo:");
        assert!(sigs[0].excerpt.starts_with("TODO:"));
        assert!(sigs[0].excerpt.chars().count() <= 160);
    }
}