//! Bounded time-window reasoning for graph reads.
//!
//! The store keeps an INT64 validity window on every edge (`0` means
//! unbounded) and the ingest layer may stamp ISO-8601 or epoch timestamps into
//! a row's payload. This module owns the pure predicate that decides whether a
//! relationship was active for a caller-supplied time window, so the store and
//! the `related` server handler cannot drift on the boundary rules.
//!
//! The module is deliberately serde + chrono only: it is evaluated at query
//! time, never materialised, and the spec requires deterministic
//! ("same window -> same result") answers.

use serde::{Deserialize, Serialize};

/// A caller-supplied temporal window. All fields are optional: a missing
/// field is unbounded, matching the spec's backward-compatibility rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalFilter {
    /// Evaluate validity at one instant (`valid_from <= as_of <= valid_to`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<i64>,
    /// Start of the requested window (epoch seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<i64>,
    /// End of the requested window (epoch seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<i64>,
}

impl TemporalFilter {
    /// True when no bound was requested, so callers can skip filtering.
    pub fn is_unbounded(&self) -> bool {
        self.as_of.is_none() && self.valid_from.is_none() && self.valid_to.is_none()
    }
}

/// Parse an ISO-8601 / RFC-3339 timestamp or a plain epoch-seconds string.
/// Anything unparseable (or empty) is treated as absent, i.e. unbounded --
/// the spec is warn-first, and a pure predicate cannot fail the query.
pub fn parse_temporal(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(epoch) = raw.parse::<i64>() {
        return Some(epoch);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.timestamp());
    }
    // Date-only ISO-8601 ("2026-10-27") is the other shape ingest writes.
    if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return date
            .and_hms_opt(0, 0, 0)
            .map(|dt| dt.and_utc().timestamp());
    }
    tracing::warn!("unparseable temporal timestamp '{}'; treating bound as absent", raw);
    None
}

/// Extract the earliest ISO-8601 date or RFC-3339 timestamp from `query` text.
/// Tokens are split on whitespace and surrounding punctuation trimmed. Returns
/// the first candidate parsed into epoch seconds; ordinary words are ignored.
pub fn extract_query_timestamp(query: &str) -> Option<i64> {
    for tok_raw in query.split_whitespace() {
        let tok = tok_raw.trim_matches(|c: char| ". ,;!?()[]\"'".contains(c));
        if tok.len() == 10 && tok.as_bytes()[4] == b'-' && tok.as_bytes()[7] == b'-' {
            if let Ok(date) = chrono::NaiveDate::parse_from_str(tok, "%Y-%m-%d") {
                return Some(
                    date.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp()).unwrap_or(0),
                );
            }
        } else if tok.contains('T') && tok.len() >= 19 {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(tok) {
                return Some(dt.timestamp());
            }
        }
    }
    None
}

/// Compute a time-proximity boost applied to a search result score.
///
/// Looks up the result's timestamp metadata field (`valid_from`,
/// `created_at`, `timestamp`, or `date`) and derives days elapsed since the
/// query timestamp. The closer the boost happens, the lower the penalty.
/// Missing timestamps yield `0.0`, which skips the rerank pass entirely.
pub fn temporal_proximity_boost(query_ts: i64, metadata: &serde_json::Value) -> f32 {
    let ts = if let Some(v) = metadata.get("valid_from").and_then(|v| v.as_str()) {
        parse_temporal(v).filter(|&t| t > 0)
    } else if let Some(v) = metadata.get("valid_from").and_then(|v| v.as_i64()).filter(|&n| n > 0) {
        Some(v)
    } else if let Some(v) = metadata.get("created_at").and_then(|v| v.as_str()) {
        parse_temporal(v).filter(|&t| t > 0)
    } else if let Some(v) = metadata.get("created_at").and_then(|v| v.as_i64()).filter(|&n| n > 0) {
        Some(v)
    } else if let Some(v) = metadata.get("timestamp").and_then(|v| v.as_str()) {
        parse_temporal(v).filter(|&t| t > 0)
    } else if let Some(v) = metadata.get("timestamp").and_then(|v| v.as_i64()).filter(|&n| n > 0) {
        Some(v)
    } else if let Some(v) = metadata.get("date").and_then(|v| v.as_str()) {
        parse_temporal(v).filter(|&t| t > 0)
    } else if let Some(v) = metadata.get("date").and_then(|v| v.as_i64()).filter(|&n| n > 0) {
        Some(v)
    } else {
        None
    };
    match ts {
        Some(t) => {
            let days_diff = ((t - query_ts).abs() as f32) / 86_400.0;
            0.15_f32 / (1.0_f32 + days_diff / 30.0_f32)
        }
        None => 0.0,
    }
}

/// The effective `[lo, hi]` validity interval of one relationship. `0` on an
/// INT64 edge column and a missing/unparseable payload stamp both mean
/// "unbounded"; intersecting the two sources can make the interval empty.
fn item_window(
    edge_valid_from: i64,
    edge_valid_to: i64,
    payload_valid_from: Option<&str>,
    payload_valid_to: Option<&str>,
) -> (i64, i64) {
    let mut lo = i64::MIN;
    let mut hi = i64::MAX;

    if edge_valid_from > 0 {
        lo = lo.max(edge_valid_from);
    }
    if edge_valid_to > 0 {
        hi = hi.min(edge_valid_to);
    }
    if let Some(from) = payload_valid_from.and_then(parse_temporal).filter(|&v| v > 0) {
        lo = lo.max(from);
    }
    if let Some(to) = payload_valid_to.and_then(parse_temporal).filter(|&v| v > 0) {
        hi = hi.min(to);
    }

    (lo, hi)
}

/// Whether a relationship was active for `filter`.
///
/// - `as_of` requires the instant to fall inside the item's validity window.
/// - `valid_from`/`valid_to` require the requested window to overlap the
///   item's window (interval intersection, not containment).
/// - An inverted requested window (`valid_from > valid_to`) is never active.
pub fn is_temporally_active(
    edge_valid_from: i64,
    edge_valid_to: i64,
    payload_valid_from: Option<&str>,
    payload_valid_to: Option<&str>,
    filter: &TemporalFilter,
) -> bool {
    let (lo, hi) = item_window(
        edge_valid_from,
        edge_valid_to,
        payload_valid_from,
        payload_valid_to,
    );
    if lo > hi {
        return false;
    }

    if let Some(at) = filter.as_of {
        if at < lo || at > hi {
            return false;
        }
    }

    let requested_lo = filter.valid_from.unwrap_or(i64::MIN);
    let requested_hi = filter.valid_to.unwrap_or(i64::MAX);
    if requested_lo > requested_hi {
        return false;
    }

    lo <= requested_hi && hi >= requested_lo
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(as_of: Option<i64>, from: Option<i64>, to: Option<i64>) -> TemporalFilter {
        TemporalFilter {
            as_of,
            valid_from: from,
            valid_to: to,
        }
    }

    #[test]
    fn zero_edge_bounds_are_unbounded() {
        assert!(is_temporally_active(0, 0, None, None, &filter(Some(1), None, None)));
        assert!(is_temporally_active(0, 0, None, None, &filter(None, Some(-10), Some(10))));
    }

    #[test]
    fn as_of_checks_the_edge_window() {
        let f = filter(Some(50), None, None);
        assert!(is_temporally_active(10, 100, None, None, &f));
        assert!(!is_temporally_active(60, 100, None, None, &f));
        assert!(!is_temporally_active(10, 40, None, None, &f));
    }

    #[test]
    fn requested_window_requires_overlap() {
        let f = filter(None, Some(100), Some(200));
        assert!(is_temporally_active(150, 250, None, None, &f));
        assert!(is_temporally_active(50, 120, None, None, &f));
        assert!(!is_temporally_active(10, 90, None, None, &f));
        assert!(!is_temporally_active(210, 300, None, None, &f));
    }

    #[test]
    fn payload_iso_and_epoch_timestamps_restrict_the_window() {
        // ISO-8601 payload bounds intersect with the edge bounds.
        assert!(is_temporally_active(
            0,
            0,
            Some("2026-01-01T00:00:00Z"),
            Some("2026-12-31T23:59:59Z"),
            &filter(Some(1_780_000_000), None, None),
        ));
        // Epoch seconds are accepted too.
        assert!(!is_temporally_active(
            0,
            0,
            Some("1000"),
            Some("2000"),
            &filter(Some(3000), None, None),
        ));
    }

    #[test]
    fn inverted_windows_and_contradictions_are_inactive() {
        assert!(!is_temporally_active(
            0,
            0,
            None,
            None,
            &filter(None, Some(10), Some(5)),
        ));
        // Edge lower bound after the upper bound is an empty interval.
        assert!(!is_temporally_active(100, 50, None, None, &filter(Some(75), None, None)));
    }

    #[test]
    fn unbounded_filter_accepts_any_consistent_window() {
        assert!(is_temporally_active(1, 2, None, None, &TemporalFilter::default()));
        assert!(TemporalFilter::default().is_unbounded());
        assert!(!filter(Some(1), None, None).is_unbounded());
    }

    #[test]
    fn parses_epoch_and_iso_shapes() {
        assert_eq!(parse_temporal("1700000000"), Some(1_700_000_000));
        assert_eq!(parse_temporal(" 2026-10-27 "), Some(1_793_059_200));
        assert_eq!(parse_temporal("not-a-date"), None);
        assert_eq!(parse_temporal(""), None);
    }

    #[test]
    fn extract_query_timestamp_finds_iso_date() {
        assert_eq!(extract_query_timestamp("hello world 2026-10-27 test"), Some(1_793_059_200));
    }

    #[test]
    fn extract_query_timestamp_finds_rfc3339() {
        assert_eq!(extract_query_timestamp("query 2026-10-27T12:00:00Z more text"), Some(1_793_102_400));
    }

    #[test]
    fn extract_query_timestamp_skips_ordinary_words() {
        assert_eq!(extract_query_timestamp("the quick brown fox jumps over the lazy dog"), None);
    }

    #[test]
    fn extract_query_timestamp_ignores_short_words() {
        assert_eq!(extract_query_timestamp("2026-10 test end"), None);
    }

    #[test]
    fn extract_query_timestamp_handles_punctuation_around_dates() {
        assert_eq!(extract_query_timestamp("(\"2026-10-27\") and [2026-11-15] data"), Some(1_793_059_200));
    }

    #[test]
    fn temporal_proximity_boost_closer_result_higher_boost() {
        let today = chrono::Utc::now().timestamp();
        // Very recent: boost is high (~0.15).
        let recent_meta = serde_json::json!({"valid_from": today});
        assert!(temporal_proximity_boost(today, &recent_meta) > 0.0);
        // Older result: lower boost.
        let older_meta = serde_json::json!({"valid_from": (today - 365 * 86400)});
        let close_boost = temporal_proximity_boost(today, &recent_meta);
        let old_boost = temporal_proximity_boost(today, &older_meta);
        assert!(old_boost < close_boost);
    }

    #[test]
    fn temporal_proximity_boost_missing_metadata_returns_zero() {
        assert_eq!(temporal_proximity_boost(1_000_000, &serde_json::Value::Null), 0.0);
    }

    #[test]
    fn temporal_proximity_boost_no_valid_timestamp_returns_zero() {
        // No positive timestamp field present.
        let meta = serde_json::json!({"deleted_at": -1});
        assert_eq!(temporal_proximity_boost(1_000_000, &meta), 0.0);
    }
}