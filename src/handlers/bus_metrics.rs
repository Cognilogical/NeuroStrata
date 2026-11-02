//! The bus's own accounting, serialized once for every surface that shows it.
//!
//! Lives here, and not inline in the daemon, so `POST /bus/metrics` and
//! `neurostrata-mcp bus-metrics` cannot drift into two different answers about
//! the same bus.

use crate::events::ThalamicBus;
use std::sync::Arc;

/// The bus as it stands at this instant, as JSON.
///
/// Deliberately inert: it reads the counters the bus already keeps and takes
/// the two short locks `ThalamicBus::metrics` takes, and never touches the
/// store -- so a daemon wedged in the database can still answer it. Compact
/// rather than pretty, because this string is also an HTTP response body and an
/// operator `jq`ing it does not want to scroll.
///
/// The serialization cannot fail in practice -- `BusMetrics` is nine integers
/// -- so an `expect` here is a statement about that, not a swallowed error.
pub async fn handle_bus_metrics(bus: Arc<ThalamicBus>) -> String {
    let mut value = serde_json::to_value(bus.metrics())
        .expect("BusMetrics is nine integers and always serializes");
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "domain_filter".to_string(),
            crate::recall_metrics::domain_filter_snapshot(),
        );
        obj.insert("recall".to_string(), crate::recall_metrics::recall_snapshot());
    }
    serde_json::to_string(&value).expect("the composite metrics body always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{RecursionToken, ThalamicPulse};

    /// The endpoint is a *read* of the bus's own counters, so it has to move
    /// when the bus does: two emitted pulses must read back as two, with the
    /// per-field JSON an operator `jq`s actually present.
    #[tokio::test]
    async fn the_reported_snapshot_is_the_buss_own_counting() {
        let bus = Arc::new(ThalamicBus::new(64));
        for id in ["m1", "m2"] {
            bus.emit(
                ThalamicPulse::Archived {
                    id: id.to_string(),
                    namespace: "bus-metrics-test".to_string(),
                },
                &RecursionToken::root(),
            );
        }

        let body = handle_bus_metrics(bus.clone()).await;
        let snapshot: serde_json::Value = serde_json::from_str(&body)
            .expect("the body is the metrics object, not prose about it");
        assert_eq!(
            snapshot["events_emitted"], 2,
            "two pulses went in, two must be counted: {body}"
        );
        assert_eq!(
            snapshot["subscribers"], 0,
            "a bus nobody registered on reports none: {body}"
        );
        assert_eq!(
            snapshot["drops_oldest"], 0,
            "nothing was dropped from a 64-slot queue: {body}"
        );

        for field in [
            "events_emitted",
            "dispatcher_handled",
            "subscribers",
            "queue_depth",
            "drops_oldest",
            "subscriber_panics",
            "subscriber_timeouts",
            "recursion_blocks",
        ] {
            assert!(
                body.contains(&format!("\"{field}\"")),
                "{field} must be in the body an operator reads: {body}"
            );
        }
    }

    #[tokio::test]
    async fn the_metrics_body_carries_the_bus_counters_and_the_new_sections() {
        let _guard = crate::recall_metrics::acquire_test_lock();
        let bus = Arc::new(ThalamicBus::new(64));
        let body = handle_bus_metrics(bus.clone()).await;
        let snapshot: serde_json::Value = serde_json::from_str(&body)
            .expect("the body is the metrics object, not prose about it");

        for field in [
            "events_emitted",
            "dispatcher_handled",
            "subscribers",
            "queue_depth",
            "drops_oldest",
            "subscriber_panics",
            "subscriber_timeouts",
            "recursion_blocks",
        ] {
            assert!(
                body.contains(&format!("\"{}\" ", field)) || body.contains(&format!("\"{}\"", field)),
                "{field} must be in the body an operator reads: {body}"
            );
        }

        let df = snapshot.get("domain_filter").expect("domain_filter section present");
        for key in [
            "domain_queries_total",
            "domain_narrowed_hits",
            "domain_fallback_vector",
            "single_domain_bypasses",
        ] {
            assert!(df.get(key).unwrap().is_number(), "{key} must be a number: {body}");
        }

        let rc = snapshot.get("recall").expect("recall section present");
        for key in ["recall_responses", "recall_promotions"] {
            assert!(rc.get(key).unwrap().is_number(), "{key} must be a number: {body}");
        }
    }

}
