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

/// Detect task-shaped statements in `text`. Case-insensitive substring match
/// on clause boundaries. Returns one signal per distinct marker matched (in
/// table order), with the trimmed clause (capped at 160 chars) as `excerpt`.
pub fn detect_task_intents(text: &str) -> Vec<TaskIntentSignal> {
    let lower = text.to_lowercase();
    let mut out: Vec<TaskIntentSignal> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for &(match_str, canonical) in MARKERS {
        // find first occurrence (case-insensitive) of match_str
        let idx = match lower.find(match_str) {
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
        let mut excerpt: String = rest[..end].trim_end().to_string();
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
}