use super::ThalamicPulse;
use crate::traits::VectorStore;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::atomic::{AtomicU8, Ordering};

/// How deep a subscriber-induced emit chain may go. One is deliberate: a
/// subscriber that reacts to its own write is a feedback loop, and the bus
/// never carries one.
#[cfg(test)]
const MAX_DEPTH: u8 = 1;

/// What a subscriber reports when it could not do its work.
///
/// The dispatcher only logs a failure and counts it -- a subscriber's problem
/// never reaches the caller that made the storage write -- so there is nothing
/// here to match on. Every fallible call in this codebase already produces an
/// `anyhow::Error`, so a subscriber hands one straight back rather than
/// wrapping it in a taxonomy nothing reads.
pub type SubscriberError = anyhow::Error;

/// The capability a subscriber is handed for emitting a follow-up event.
///
/// It is a capability rather than a flag because the bus never lends one
/// implicitly: a subscriber learns it may emit by asking, and at the default
/// depth the answer is always no. Depth 0 -- outside any subscriber invocation
/// -- is the only state cleared to emit, so the guard is closed by default, and
/// raising `MAX_DEPTH` is what would ever open a chain.
///
/// Two constructors, one per side of the boundary: [`RecursionToken::root`] for
/// a caller standing at the top of the recursion chain (a request handler, which
/// is not inside a subscriber and is therefore cleared to emit), and
/// [`RecursionToken::new`] for the dispatcher's hand-off into a subscriber
/// (which is inside one, and is therefore blocked).
pub struct RecursionToken {
    depth: AtomicU8,
}

impl RecursionToken {
    /// A depth-0 token for a caller at the top of the recursion chain -- a
    /// request handler making the original write, not a subscriber reacting to
    /// one. Cleared to emit.
    pub fn root() -> Self {
        Self { depth: AtomicU8::new(0) }
    }

    /// The token the dispatcher hands to a subscriber it is about to invoke:
    /// depth 1, because emitting from inside a subscriber is the recursion this
    /// token exists to refuse.
    pub fn new() -> Self {
        Self { depth: AtomicU8::new(1) }
    }

    /// Whether a token at this depth is cleared to emit.
    pub fn allows_emit(&self) -> bool {
        self.depth.load(Ordering::Acquire) == 0
    }

    /// A token one level deeper, or `None` once the chain already sits at
    /// `MAX_DEPTH`.
    ///
    /// Single-use against the receiver: the reservation is a CAS rather than a
    /// move, and it works by *mutating the parent's own depth* -- the deeper
    /// slot is claimed with `compare_exchange_weak`, and a lost race retries
    /// against the observed value. A load-then-construct would let two
    /// concurrent upgrades each read `MAX_DEPTH - 1` and both succeed, which is
    /// exactly the runaway chain the depth guard exists to stop.
    ///
    /// The mutation is observable and permanent: after a successful
    /// `upgrade()`, the parent's `allows_emit()` has flipped from `true` to
    /// `false`, and a second `upgrade()` on the same parent returns `None`.
    /// Hoisting one [`RecursionToken::root`] into a field and reusing it across
    /// emits is therefore a misuse -- the first upgrade silently revokes the
    /// parent's permission, and every later emit under it is dropped at the bus
    /// boundary with `recursion_blocks` incremented. Construct a fresh `root()`
    /// per emit. There is no decrement path that would undo the reservation.
    #[cfg(test)]
    pub fn upgrade(&self) -> Option<Self> {
        let mut current = self.depth.load(Ordering::Acquire);
        loop {
            if current >= MAX_DEPTH {
                return None;
            }
            match self.depth.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self { depth: AtomicU8::new(current + 1) }),
                Err(observed) => current = observed,
            }
        }
    }
}

/// Everything a subscriber is allowed to reach that the pulse itself does not
/// carry: the store to read siblings from, the namespace the pulse belongs to,
/// and the timestamp the bus stamped once, so every subscriber agrees on "now"
/// instead of each calling the clock.
pub struct SubscriberContext<'a> {
    pub store: &'a dyn VectorStore,
    pub namespace: &'a str,
    pub ts: i64,
}

/// A consumer of thalamic pulses.
///
/// A subscriber opts in by matching on the pulse and returning `Ok(())` for the
/// variants it does not care about -- an unmatched variant is a no-op, not an
/// error. It never holds the bus: a follow-up event needs the explicit
/// capability of [`RecursionToken`], which at the default depth it will not be
/// given.
#[async_trait]
pub trait MemorySubscriber: Send + Sync {
    /// Stable identifier, used in logs and in [`BackpressureEvent`] so an
    /// operator can tell which subscriber dropped, timed out, or panicked.
    fn name(&self) -> &'static str;

    async fn handle(
        &self,
        event: &ThalamicPulse,
        token: &RecursionToken,
        ctx: &SubscriberContext<'_>,
    ) -> Result<(), SubscriberError>;
}

/// What the bus lost or had to interrupt, published on the backpressure watch
/// channel for the daemon to surface at `POST /bus/metrics`.
///
/// These are notifications, not failures of the write that caused them: a
/// dropped pulse means bookkeeping did not happen, never that a memory was not
/// stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackpressureEvent {
    /// Nothing has happened yet. This is the value the watch channel is
    /// seeded with, so a consumer that reads before the first event never
    /// mistakes a `Dropped { count: 0 }` -- a semantically false "the queue
    /// was full" signal -- for real backpressure.
    Idle,
    /// The bounded queue was full, so its oldest pulses were dropped to make
    /// room. `count` is how many have been dropped in total.
    ///
    /// Queue-full is the *only* cause this variant reports; a refused recursion
    /// is [`RecursionRefused`], so a consumer never has to correlate counters
    /// to tell the two apart.
    Dropped { count: u64 },
    /// The recursion guard refused an emit: the caller's [`RecursionToken`]
    /// was held at depth ≥ 1, so the pulse never entered the queue and
    /// `BusMetrics::recursion_blocks` was incremented. `token_allows_emit`
    /// records what the token reported at the moment of refusal -- the value
    /// the bus checked, so a consumer can assert rather than infer it.
    RecursionRefused { token_allows_emit: bool },
    /// A subscriber ran past the dispatcher's per-subscriber timeout and was
    /// abandoned mid-flight.
    SubscriberTimeout { name: &'static str },
    /// A subscriber panicked. The dispatcher caught it and moved on.
    ///
    /// `payload` is a *redacted snippet*, not the raw panic: first line only,
    /// control characters stripped, capped at 200 characters. Panic text is
    /// attacker- or content-influenced and this channel is what
    /// `POST /bus/metrics` surfaces, so it gets the same treatment the spec
    /// gives `GuardEventLog`'s `payload_hash`. The full text stays in the
    /// local WARN log.
    SubscriberPanic { name: &'static str, payload: String },
}

/// A read-only snapshot of what the bus has done, for the metrics endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct BusMetrics {
    /// Pulses accepted by the bus since it was constructed: everything that
    /// entered the queue, whether or not it was later dropped to make room.
    pub events_emitted: u64,
    /// Pulses the dispatcher has finished running through its subscribers.
    /// Together with `queue_depth` and `drops_oldest` this closes the books on
    /// `events_emitted`: every accepted pulse is in exactly one of those three
    /// terms while the dispatcher is idle between pulses.
    pub dispatcher_handled: u64,
    /// Subscribers currently registered.
    pub subscribers: usize,
    /// Pulses waiting for the dispatcher right now.
    pub queue_depth: usize,
    /// Pulses dropped from a full queue: the oldest pulse, dropped to make room.
    pub drops_oldest: u64,
    /// Subscriber panics caught by the dispatcher.
    pub subscriber_panics: u64,
    /// Subscriber invocations abandoned for exceeding their timeout.
    pub subscriber_timeouts: u64,
    /// Emits refused because they were attempted under a held [`RecursionToken`].
    pub recursion_blocks: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recursion_token_disallows_emit_at_depth_one() {
        let t = RecursionToken::new();
        assert!(!t.allows_emit(), "fresh token must block emit");
        let upgraded = t.upgrade();
        assert!(upgraded.is_none(), "depth-1 token cannot be upgraded further");
    }

    #[test]
    fn recursion_token_root_allows_emit() {
        let root = RecursionToken::root();
        assert!(root.allows_emit(), "depth-0 token must permit emit");
        let t = root.upgrade().expect("depth-0 token can be upgraded to depth 1");
        assert!(!t.allows_emit(), "upgraded token sits at depth 1 and blocks emit");
        assert!(t.upgrade().is_none(), "depth-1 token cannot be upgraded further");
    }
}
