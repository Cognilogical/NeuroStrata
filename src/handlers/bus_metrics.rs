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
/// The serialization cannot fail in practice -- `BusMetrics` is eight integers
/// -- so an `expect` here is a statement about that, not a swallowed error.
pub async fn handle_bus_metrics(bus: Arc<ThalamicBus>) -> String {
    serde_json::to_string(&bus.metrics()).expect("BusMetrics is eight integers and always serializes")
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
        let snapshot: serde_json::Value =
            serde_json::from_str(&body).expect("the body is the metrics object, not prose about it");
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
}
