//! Process-global counters for the recall closure's two open questions:
//! whether a domain filter pays, and whether the `append_log` seat is
//! sufficient. The counters are atomics in the daemon process (they reset on
//! restart, exactly like the bus's own). The join between a recall and its
//! later promotion is a bounded, per-namespace set of recently-recalled ids.

use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

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
    json!({
        "domain_queries_total": DOMAIN_QUERIES_TOTAL.load(Ordering::Relaxed),
        "domain_narrowed_hits": DOMAIN_NARROWED_HITS.load(Ordering::Relaxed),
        "domain_fallback_vector": DOMAIN_FALLBACK_VECTOR.load(Ordering::Relaxed),
        "single_domain_bypasses": SINGLE_DOMAIN_BYPASSES.load(Ordering::Relaxed),
    })
}

pub fn recall_snapshot() -> serde_json::Value {
    json!({
        "recall_responses": RECALL_RESPONSES.load(Ordering::Relaxed),
        "recall_promotions": RECALL_PROMOTIONS.load(Ordering::Relaxed),
    })
}

pub fn record_domain_query(narrowed: bool, fell_back: bool, single_domain: bool) {
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
    let mut guard = RECENTLY_RECALLED.lock().expect("recently-recalled lock poisoned");
    let found = if let Some(map) = guard.as_mut() {
        if let Some(deque) = map.get_mut(namespace) {
            if let Some(pos) = deque.iter().position(|x| x == id) {
                deque.remove(pos);
                true
            } else {
                false
            }
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