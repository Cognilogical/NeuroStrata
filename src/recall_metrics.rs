//! Process-global counters for the recall closure's two open questions:
//! whether a domain filter pays, and whether the `append_log` seat is
//! sufficient. The counters are atomics in the daemon process (they reset on
//! restart, exactly like the bus's own). The join between a recall and its
//! later promotion is a bounded, per-namespace set of recently-recalled ids.

use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use std::sync::MutexGuard;

// Test-only: a process-wide lock that serializes all counter access so that
// tests asserting on global counter state do not race with concurrent
// increments from other tests. Re-entrant on the holding thread so that
// production code called from a test that already holds the lock does not
// deadlock.
#[cfg(test)]
thread_local! {
    static HOLDING: std::cell::Cell<bool> = std::cell::Cell::new(false);
}

#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub struct TestLockGuard {
    _guard: Option<MutexGuard<'static, ()>>,
}

#[cfg(test)]
impl Drop for TestLockGuard {
    fn drop(&mut self) {
        // Only the guard that actually acquired the lock resets the
        // re-entrancy flag; a no-op (re-entrant) guard must not clobber
        // the flag held by an outer guard.
        if self._guard.is_some() {
            HOLDING.set(false);
        }
    }
}

#[cfg(test)]
pub fn acquire_test_lock() -> TestLockGuard {
    if HOLDING.get() {
        return TestLockGuard { _guard: None };
    }
    let guard = TEST_LOCK.lock().expect("test lock poisoned");
    HOLDING.set(true);
    TestLockGuard { _guard: Some(guard) }
}

#[cfg(test)]
fn maybe_acquire() -> Option<MutexGuard<'static, ()>> {
    if HOLDING.get() {
        None
    } else {
        Some(TEST_LOCK.lock().expect("test lock poisoned"))
    }
}

/// Cap on the number of recently-recalled ids retained per namespace.
pub const RECENTLY_RECALLED_CAP: usize = 32;

static DOMAIN_QUERIES_TOTAL: AtomicU64 = AtomicU64::new(0);
static DOMAIN_NARROWED_HITS: AtomicU64 = AtomicU64::new(0);
static DOMAIN_FALLBACK_VECTOR: AtomicU64 = AtomicU64::new(0);
static SINGLE_DOMAIN_BYPASSES: AtomicU64 = AtomicU64::new(0);
static RECALL_RESPONSES: AtomicU64 = AtomicU64::new(0);
static RECALL_PROMOTIONS: AtomicU64 = AtomicU64::new(0);

static RECENTLY_RECALLED: Mutex<Option<HashMap<String, VecDeque<String>>>> =
    Mutex::new(None);

pub fn domain_filter_snapshot() -> serde_json::Value {
    #[cfg(test)]
    let _lock = maybe_acquire();
    json!({
        "domain_queries_total": DOMAIN_QUERIES_TOTAL.load(Ordering::Relaxed),
        "domain_narrowed_hits": DOMAIN_NARROWED_HITS.load(Ordering::Relaxed),
        "domain_fallback_vector": DOMAIN_FALLBACK_VECTOR.load(Ordering::Relaxed),
        "single_domain_bypasses": SINGLE_DOMAIN_BYPASSES.load(Ordering::Relaxed),
    })
}

pub fn recall_snapshot() -> serde_json::Value {
    #[cfg(test)]
    let _lock = maybe_acquire();
    json!({
        "recall_responses": RECALL_RESPONSES.load(Ordering::Relaxed),
        "recall_promotions": RECALL_PROMOTIONS.load(Ordering::Relaxed),
    })
}

pub fn record_domain_query(narrowed: bool, fell_back: bool, single_domain: bool) {
    #[cfg(test)]
    let _lock = maybe_acquire();
    DOMAIN_QUERIES_TOTAL.fetch_add(1, Ordering::Relaxed);
    if narrowed {
        DOMAIN_NARROWED_HITS.fetch_add(1, Ordering::Relaxed);
    }
    if fell_back {
        DOMAIN_FALLBACK_VECTOR.fetch_add(1, Ordering::Relaxed);
    }
    if single_domain {
        SINGLE_DOMAIN_BYPASSES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_recall(namespace: &str, ids: &[String]) {
    #[cfg(test)]
    let _lock = maybe_acquire();
    if ids.is_empty() {
        // An empty recall response does not count toward the rate.
        return;
    }
    let mut guard = RECENTLY_RECALLED.lock().expect("recently-recalled lock poisoned");
    let map = guard.get_or_insert_with(HashMap::new);
    let deque = map.entry(namespace.to_string()).or_default();
    for id in ids {
        deque.push_back(id.clone());
        while deque.len() > RECENTLY_RECALLED_CAP {
            deque.pop_front();
        }
    }
    drop(guard);
    RECALL_RESPONSES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_promotion(namespace: &str, id: &str) -> bool {
    #[cfg(test)]
    let _lock = maybe_acquire();
    let mut guard = RECENTLY_RECALLED.lock().expect("recently-recalled lock poisoned");
    let found = if let Some(map) = guard.as_mut() {
        if let Some(deque) = map.get_mut(namespace) {
            let before = deque.len();
            deque.retain(|x| x != id);
            before != deque.len()
        } else {
            false
        }
    } else {
        false
    };
    drop(guard);
    if found {
        RECALL_PROMOTIONS.fetch_add(1, Ordering::Relaxed);
    }
    found
}


#[cfg(test)]
pub fn reset_counters_for_tests() {
    let _lock = acquire_test_lock();
    DOMAIN_QUERIES_TOTAL.store(0, Ordering::Relaxed);
    DOMAIN_NARROWED_HITS.store(0, Ordering::Relaxed);
    DOMAIN_FALLBACK_VECTOR.store(0, Ordering::Relaxed);
    SINGLE_DOMAIN_BYPASSES.store(0, Ordering::Relaxed);
    RECALL_RESPONSES.store(0, Ordering::Relaxed);
    RECALL_PROMOTIONS.store(0, Ordering::Relaxed);
    if let Ok(mut g) = RECENTLY_RECALLED.lock() {
        *g = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_counters_start_at_zero() {
        let _guard = acquire_test_lock();
        reset_counters_for_tests();
        let df = domain_filter_snapshot();
        assert_eq!(df["domain_queries_total"], 0);
        assert_eq!(df["domain_narrowed_hits"], 0);
        assert_eq!(df["domain_fallback_vector"], 0);
        assert_eq!(df["single_domain_bypasses"], 0);
        let rc = recall_snapshot();
        assert_eq!(rc["recall_responses"], 0);
        assert_eq!(rc["recall_promotions"], 0);
    }

    #[test]
    fn a_recalled_id_is_counted_once_when_it_is_promoted() {
        let _guard = acquire_test_lock();
        reset_counters_for_tests();
        let ns = "recall-promo-test";
        let id = "recalled-id";
        record_recall(ns, &[id.to_string()]);
        let before = recall_snapshot()["recall_promotions"].as_u64().unwrap();
        assert!(record_promotion(ns, id));
        let after = recall_snapshot()["recall_promotions"].as_u64().unwrap();
        assert_eq!(after, before + 1);
        assert!(!record_promotion(ns, id));
        let after2 = recall_snapshot()["recall_promotions"].as_u64().unwrap();
        assert_eq!(after2, after);
    }

    #[test]
    fn the_recalled_id_set_is_bounded() {
        let _guard = acquire_test_lock();
        reset_counters_for_tests();
        let ns = "bounded-recall-test";
        for i in 0..(RECENTLY_RECALLED_CAP + 5) {
            record_recall(ns, &[format!("bounded-id-{}", i)]);
        }
        // The oldest id has aged out.
        assert!(!record_promotion(ns, "bounded-id-0"));
        // The newest id is still present.
        assert!(record_promotion(
            ns,
            &format!("bounded-id-{}", RECENTLY_RECALLED_CAP + 4)
        ));
    }
}
