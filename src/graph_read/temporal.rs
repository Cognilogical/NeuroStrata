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
    None
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
    if let Some(from) = payload_valid_from.and_then(parse_temporal).filter(|v| *v > 0) {
        lo = lo.max(from);
    }
    if let Some(to) = payload_valid_to.and_then(parse_temporal).filter(|v| *v > 0) {
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
}
