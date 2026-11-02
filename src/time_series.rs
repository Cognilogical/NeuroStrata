use std::collections::{BTreeMap, VecDeque};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub use crate::telemetry::anomaly::MetricKind;

pub type SeriesKey = (String, MetricKind, String);

/// Every metric kind the store tracks. A cross-stage query without an explicit
/// `kind` sweeps all of these rather than guessing one.
pub const ALL_METRIC_KINDS: [MetricKind; 6] = [
    MetricKind::AccessCount,
    MetricKind::EnrichmentFailure,
    MetricKind::EnrichmentLatencyMs,
    MetricKind::RetrievalDistance,
    MetricKind::MemoryCount,
    MetricKind::IngestLatencyMs,
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub id: String,
    pub seq: u64,
    pub ts: i64,
    pub value: f64,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SeriesConfig {
    pub capacity: usize,
    /// Seconds between persisted flushes of the in-memory ring.
    pub flush_interval_secs: u64,
    /// EWMA smoothing factor for the rolling aggregate (clamped to 0.01..=1.0).
    pub ewma_alpha: f64,
}

impl Default for SeriesConfig {
    fn default() -> Self {
        default_series_config_from_app_config()
    }
}

/// Derive the in-memory series defaults from the app config (metrics_* keys).
/// Falls back to the hard-coded serde defaults when the app config cannot be
/// loaded, so metric collection never blocks on config resolution.
pub fn default_series_config_from_app_config() -> SeriesConfig {
    match crate::config::Config::load() {
        Ok(cfg) => SeriesConfig {
            capacity: cfg.metrics_ring_capacity.max(1),
            flush_interval_secs: cfg.metrics_flush_interval_secs.max(1),
            ewma_alpha: cfg.metrics_ewma_alpha.clamp(0.01, 1.0),
        },
        Err(e) => {
            tracing::warn!("time_series Config::from_default_path failed, using fallback: {e}");
            bad_app_config_fallback()
        }
    }
}

fn bad_app_config_fallback() -> SeriesConfig {
    SeriesConfig {
        capacity: 1024,
        flush_interval_secs: 60,
        ewma_alpha: 0.2,
    }
}

/// Split a persisted metric sample id into its `(namespace, metric, stage)`
/// segments. Canonical ids are `namespace::metric::stage::ts::seq`; legacy
/// rows use the same layout with single colons (`ns:metric:stage:ts:seq`).
/// An unprefixed legacy id starts with the metric segment itself
/// (`metric::stage::ts::seq`), recognized by its first field parsing as a
/// `MetricKind`; those return `None` for the namespace.
fn split_metric_sample_id(id: &str) -> (Option<&str>, Option<&str>, Option<&str>) {
    let sep = if id.contains("::") { "::" } else { ":" };
    let mut fields = id.split(sep);
    let first = fields.next();
    let second = fields.next();
    let third = fields.next();
    if let (Some(f0), Some(f1)) = (first, second) {
        if MetricKind::from_str(f0).is_ok() {
            // Legacy unprefixed id: metric :: stage :: ts :: seq
            return (None, Some(f0), Some(f1));
        }
    }
    (first, second, third)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RollingAgg {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub ewma: f64,
    pub alpha: f64,
    pub next_seq: u64,
    pub flushed_seq: u64,
}

impl RollingAgg {
    pub fn new(alpha: f64) -> Self {
        Self {
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            ewma: 0.0,
            alpha,
            next_seq: 1,
            flushed_seq: 0,
        }
    }

    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum / self.count as f64
        }
    }

    pub fn to_json_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "count": self.count,
            "sum": self.sum,
            "mean": self.mean(),
            "min": if self.count == 0 || !self.min.is_finite() { 0.0 } else { self.min },
            "max": if self.count == 0 || !self.max.is_finite() { 0.0 } else { self.max },
            "ewma": self.ewma,
        })
    }

    /// Combine per-series rolling aggregates for one stage across metric kinds
    /// into a single virtual series aggregate. `count` and `sum` add, `min` and
    /// `max` take the extremes, `ewma` blends each aggregate's value weighted
    /// by its count, and the smoothing constant carries the shared alpha.
    /// Empty aggregates (count == 0) contribute nothing; an all-empty input
    /// yields `None` so the caller can report "no data" rather than zeros.
    pub fn combine_aggs(aggs: &[RollingAgg]) -> Option<RollingAgg> {
        let mut combined: Option<RollingAgg> = None;
        for agg in aggs {
            if agg.count == 0 {
                continue;
            }
            match combined.as_mut() {
                None => combined = Some(agg.clone()),
                Some(c) => {
                    c.count += agg.count;
                    c.sum += agg.sum;
                    c.min = c.min.min(agg.min);
                    c.max = c.max.max(agg.max);
                    c.next_seq = c.next_seq.max(agg.next_seq);
                    c.flushed_seq = c.flushed_seq.max(agg.flushed_seq);
                }
            }
        }
        let mut c = combined?;
        let total: u64 = aggs.iter().filter(|a| a.count > 0).map(|a| a.count).sum();
        if total > 0 {
            c.ewma = aggs
                .iter()
                .filter(|a| a.count > 0)
                .map(|a| a.ewma * a.count as f64)
                .sum::<f64>()
                / total as f64;
        }
        Some(c)
    }
}

pub struct Series {
    pub key: SeriesKey,
    pub samples: VecDeque<Sample>,
    pub agg: RollingAgg,
    pub capacity: usize,
    pub last_flushed: Instant,
}

pub struct TimeSeriesStore {
    pub series: RwLock<BTreeMap<SeriesKey, Series>>,
    pub config: SeriesConfig,
    pub anomaly_cb: Option<Arc<dyn Fn(&SeriesKey, &Sample) + Send + Sync>>,
    pub detector: crate::telemetry::AnomalyDetector,
    pub anomalies: RwLock<Vec<crate::telemetry::AnomalyEvent>>,
}

impl TimeSeriesStore {
    pub fn new(config: SeriesConfig) -> Self {
        Self {
            series: RwLock::new(BTreeMap::new()),
            config,
            anomaly_cb: None,
            detector: crate::telemetry::AnomalyDetector::default(),
            anomalies: RwLock::new(Vec::new()),
        }
    }

    pub fn with_anomaly_callback(mut self, cb: Arc<dyn Fn(&SeriesKey, &Sample) + Send + Sync>) -> Self {
        self.anomaly_cb = Some(cb);
        self
    }

    pub fn push(&self, key: SeriesKey, mut sample: Sample) {
        if !sample.value.is_finite() {
            tracing::warn!(
                "time_series: dropping non-finite sample value {} for {:?}",
                sample.value,
                key
            );
            return;
        }

        let mut guard = self
            .series
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let capacity = self.config.capacity.max(1);

        let series = guard.entry(key.clone()).or_insert_with(|| Series {
            key: key.clone(),
            samples: VecDeque::with_capacity(capacity),
            agg: RollingAgg::new(self.config.ewma_alpha),
            capacity,
            last_flushed: Instant::now(),
        });

        series.capacity = series.capacity.max(1);

        if series.samples.len() >= series.capacity {
            if let Some(evicted) = series.samples.pop_front() {
                series.agg.sum -= evicted.value;
                series.agg.count = series.agg.count.saturating_sub(1);
            }
            // Recompute min/max over remaining ring buffer + incoming value
            let mut new_min = f64::INFINITY;
            let mut new_max = f64::NEG_INFINITY;
            for s in series.samples.iter() {
                new_min = new_min.min(s.value);
                new_max = new_max.max(s.value);
            }
            new_min = new_min.min(sample.value);
            new_max = new_max.max(sample.value);
            series.agg.min = new_min;
            series.agg.max = new_max;
        }

        sample.seq = series.agg.next_seq;
        series.agg.next_seq += 1;
        sample.id = format!(
            "{}::{}::{}::{}::{}",
            series.key.0,
            series.key.1.as_str(),
            series.key.2,
            sample.ts,
            sample.seq
        );

        series.agg.count += 1;
        series.agg.sum += sample.value;
        series.agg.min = series.agg.min.min(sample.value);
        series.agg.max = series.agg.max.max(sample.value);
        series.agg.ewma = if series.agg.count == 1 {
            sample.value
        } else {
            series.agg.alpha * sample.value + (1.0 - series.agg.alpha) * series.agg.ewma
        };

        series.samples.push_back(sample.clone());
        drop(guard);

        if let Some(ev) = self.detector.observe(&key.0, key.1, &key.2, sample.value) {
            let mut a = self.anomalies.write().unwrap_or_else(|e| e.into_inner());
            if a.len() >= 128 {
                a.remove(0);
            }
            a.push(ev);
        }

        if let Some(ref cb) = self.anomaly_cb {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cb(&key, &sample);
            }));
            if result.is_err() {
                // Callbacks must never block push; the panic is swallowed, but
                // surfaced at warn level so a broken callback is visible.
                tracing::warn!("time_series: anomaly callback panicked for {:?}", key);
            }
        }
    }

    pub fn recent_anomalies(&self, namespace: &str) -> Vec<crate::telemetry::AnomalyEvent> {
        let guard = self
            .anomalies
            .read()
            .unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .filter(|ev| namespace.is_empty() || ev.namespace == namespace)
            .cloned()
            .collect()
    }

    pub fn record(&self, namespace: &str, metric: MetricKind, stage: &str, ts: i64, value: f64, labels: BTreeMap<String, String>) {
        let sample = Sample {
            id: String::new(),
            seq: 0,
            ts,
            value,
            labels,
        };
        self.push((namespace.to_string(), metric, stage.to_string()), sample);
    }

    pub fn range(&self, key: &SeriesKey, from: i64, to: i64, db_samples: &[Sample]) -> Vec<Sample> {
        let guard = self.series.read().unwrap_or_else(|e| e.into_inner());
        let in_mem: Vec<Sample> = guard
            .get(key)
            .map(|s| {
                s.samples
                    .iter()
                    .filter(|samp| samp.ts >= from && samp.ts <= to)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        drop(guard);

        let mut merged: BTreeMap<String, Sample> = BTreeMap::new();
        for s in in_mem {
            merged.insert(s.id.clone(), s);
        }
        for s in db_samples {
            if s.ts >= from && s.ts <= to {
                merged.insert(s.id.clone(), s.clone());
            }
        }

        let mut result: Vec<Sample> = merged.into_values().collect();
        result.sort_by(|a, b| {
            (a.ts, a.seq, a.id.clone()).cmp(&(b.ts, b.seq, b.id.clone()))
        });
        result
    }

    pub fn aggregate(&self, key: &SeriesKey) -> Option<RollingAgg> {
        let guard = self.series.read().unwrap_or_else(|e| e.into_inner());
        guard.get(key).map(|series| {
            let mut agg = series.agg.clone();
            if agg.count == 0 {
                agg.min = 0.0;
                agg.max = 0.0;
            }
            agg
        })
    }

    pub fn list_keys(&self, namespace: &str) -> Vec<(SeriesKey, u64)> {
        let guard = self.series.read().unwrap_or_else(|e| e.into_inner());
        let mut result: Vec<(SeriesKey, u64)> = guard
            .iter()
            .filter(|(k, _)| k.0 == namespace)
            .map(|(k, s)| (k.clone(), s.agg.count))
            .collect();
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    pub fn samples_for_anomaly(&self, key: &SeriesKey) -> Vec<f64> {
        let guard = self.series.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(key)
            .map(|s| s.samples.iter().map(|samp| samp.value).collect())
            .unwrap_or_default()
    }

    pub fn take_unflushed(&self, namespace: Option<&str>) -> Vec<(SeriesKey, Vec<Sample>, u64)> {
        let guard = self.series.read().unwrap_or_else(|e| e.into_inner());
        let mut result: Vec<(SeriesKey, Vec<Sample>, u64)> = Vec::new();
        for (key, series) in guard.iter() {
            if let Some(ns) = namespace {
                if key.0 != ns {
                    continue;
                }
            }
            let samples: Vec<Sample> = series
                .samples
                .iter()
                .filter(|s| s.seq > series.agg.flushed_seq)
                .cloned()
                .collect();
            if !samples.is_empty() {
                let max_seq = samples.iter().map(|s| s.seq).max().unwrap_or(0);
                result.push((key.clone(), samples, max_seq));
            }
        }
        drop(guard);
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    pub fn mark_flushed(&self, key: &SeriesKey, flushed_seq: u64) {
        let mut guard = self.series.write().unwrap_or_else(|e| e.into_inner());
        if let Some(series) = guard.get_mut(key) {
            series.agg.flushed_seq = flushed_seq;
            series.last_flushed = Instant::now();
        }
    }

    pub fn query_metrics(
        &self,
        namespace: &str,
        metric: MetricKind,
        stage: Option<&str>,
        from: Option<i64>,
        to: Option<i64>,
        db_samples_ref: &[(String, Sample)],
    ) -> serde_json::Value {
        let db_map: BTreeMap<String, Sample> = db_samples_ref
            .iter()
            .map(|(id, samp)| (id.clone(), samp.clone()))
            .collect();
        let mut all_samples: Vec<Sample> = Vec::new();

        let guard = self.series.read().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = stage {
            let key = (namespace.to_string(), metric, st.to_string());
            if let Some(series) = guard.get(&key) {
                for s in series.samples.iter() {
                    if from.map_or(true, |f| s.ts >= f) && to.map_or(true, |t| s.ts <= t) {
                        all_samples.push(s.clone());
                    }
                }
            }
        } else {
            for (key, series) in guard.iter() {
                if key.0 == namespace && key.1 == metric {
                    for s in series.samples.iter() {
                        if from.map_or(true, |f| s.ts >= f) && to.map_or(true, |t| s.ts <= t) {
                            let samp = s.clone();
                            // Ensure stage field reflects stage from series key
                            // (already embedded via id, but keep as-is)
                            all_samples.push(samp);
                        }
                    }
                }
            }
        }
        drop(guard);

        // Deduplicate with DB samples
        let mut seen: BTreeMap<String, Sample> = BTreeMap::new();
        for s in all_samples {
            seen.insert(s.id.clone(), s);
        }
        for (_, samp) in db_map.iter() {
            // DB sample ids embed their `namespace`, `metric` and `stage`
            // segments (`namespace::metric::stage::ts::seq`; legacy rows use
            // single colons). Every segment must match the query: a substring
            // namespace hit (e.g. "a" inside "beta::memory_count::...") or a
            // row persisted for a different metric or stage must not merge
            // into this result.
            let (id_ns, id_metric, id_stage) = split_metric_sample_id(&samp.id);
            if !namespace.is_empty() && id_ns != Some(namespace) {
                continue;
            }
            if id_metric != Some(metric.as_str()) {
                continue;
            }
            if let Some(req_stage) = stage {
                if id_stage != Some(req_stage) {
                    continue;
                }
            }
            // Include DB sample if it matches range
            if from.map_or(true, |f| samp.ts >= f) && to.map_or(true, |t| samp.ts <= t) {
                seen.insert(samp.id.clone(), samp.clone());
            }
        }

        let mut result_samples: Vec<Sample> = seen.into_values().collect();
        result_samples.sort_by(|a, b| {
            (a.ts, a.seq, a.id.clone()).cmp(&(b.ts, b.seq, b.id.clone()))
        });

        // Compute aggregate
        let count = result_samples.len() as u64;
        let sum: f64 = result_samples.iter().map(|s| s.value).sum();
        let mean = if count == 0 { 0.0 } else { sum / count as f64 };
        let min_val = if result_samples.is_empty() {
            0.0
        } else {
            result_samples.iter().map(|s| s.value).fold(f64::INFINITY, f64::min)
        };
        let max_val = if result_samples.is_empty() {
            0.0
        } else {
            result_samples.iter().map(|s| s.value).fold(f64::NEG_INFINITY, f64::max)
        };
        // Compute EWMA sequentially over sorted values with the configured alpha
        let alpha = self.config.ewma_alpha;
        let mut ewma_val = 0.0;
        let mut first_ewma = true;
        for s in &result_samples {
            if first_ewma {
                ewma_val = s.value;
                first_ewma = false;
            } else {
                ewma_val = alpha * s.value + (1.0 - alpha) * ewma_val;
            }
        }
        let ewma_final = if result_samples.is_empty() { 0.0 } else { ewma_val };

        let samples_json: Vec<serde_json::Value> = result_samples
            .iter()
            .map(|s| {
                let stage_str = stage
                    .map(|st| st.to_string())
                    .unwrap_or_else(|| {
                        // Derive stage from sample id: last segment before ts/seq is stage
                        // id format: namespace::metric::stage::ts::seq
                        s.id.split("::").nth(2).unwrap_or("").to_string()
                    });
                serde_json::json!({
                    "id": s.id,
                    "seq": s.seq,
                    "ts": s.ts,
                    "value": s.value,
                    "labels": s.labels,
                    "stage": stage_str,
                })
            })
            .collect();

        serde_json::json!({
            "samples": samples_json,
            "aggregate": {
                "count": count,
                "sum": sum,
                "mean": mean,
                "min": if count == 0 { 0.0 } else { min_val },
                "max": if count == 0 { 0.0 } else { max_val },
                "ewma": ewma_final,
            }
        })
    }
}

impl Default for TimeSeriesStore {
    fn default() -> Self {
        Self::new(SeriesConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_eviction_at_capacity() {
        let store = TimeSeriesStore::new(SeriesConfig {
            capacity: 3,
            ..SeriesConfig::default()
        });
        for i in 0..5 {
            store.push(
                ("test".to_string(), MetricKind::AccessCount, "search".to_string()),
                Sample {
                    id: String::new(),
                    seq: 0,
                    ts: i as i64,
                    value: (i + 1) as f64,
                    labels: BTreeMap::new(),
                },
            );
        }
        let agg = store.aggregate(&("test".to_string(), MetricKind::AccessCount, "search".to_string())).unwrap();
        assert_eq!(agg.count, 3);
        assert_eq!(agg.sum, 3.0 + 4.0 + 5.0);
        assert!((agg.min - 3.0).abs() < 1e-6);
        assert!((agg.max - 5.0).abs() < 1e-6);
    }

    #[test]
    fn rolling_aggregate_correctness() {
        let store = TimeSeriesStore::default();
        let key = ("ns".to_string(), MetricKind::EnrichmentLatencyMs, "embed".to_string());
        store.push(
            key.clone(),
            Sample {
                id: String::new(),
                seq: 0,
                ts: 1,
                value: 10.0,
                labels: BTreeMap::new(),
            },
        );
        store.push(
            key.clone(),
            Sample {
                id: String::new(),
                seq: 0,
                ts: 2,
                value: 20.0,
                labels: BTreeMap::new(),
            },
        );
        let agg = store.aggregate(&key).unwrap();
        assert_eq!(agg.count, 2);
        assert!((agg.sum - 30.0).abs() < 1e-6);
        assert!((agg.mean() - 15.0).abs() < 1e-6);
        assert!((agg.min - 10.0).abs() < 1e-6);
        assert!((agg.max - 20.0).abs() < 1e-6);
    }

    #[test]
    fn range_filter_and_dedup() {
        let store = TimeSeriesStore::default();
        let key = ("r".to_string(), MetricKind::RetrievalDistance, "search".to_string());
        store.push(
            key.clone(),
            Sample {
                id: "a::retrieval_distance::search::100::1".to_string(),
                seq: 1,
                ts: 100,
                value: 1.5,
                labels: BTreeMap::new(),
            },
        );
        store.push(
            key.clone(),
            Sample {
                id: "a::retrieval_distance::search::200::2".to_string(),
                seq: 2,
                ts: 200,
                value: 2.5,
                labels: BTreeMap::new(),
            },
        );
        let db = vec![
            Sample {
                id: "a::retrieval_distance::search::100::1".to_string(),
                seq: 1,
                ts: 100,
                value: 1.5,
                labels: BTreeMap::new(),
            },
        ];
        let samples = store.range(&key, 150, 250, &db);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].ts, 200);
    }

    #[test]
    fn anomaly_callback_invoked() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let called = Arc::new(AtomicUsize::new(0));
        let called_clone = Arc::clone(&called);
        let cb = Arc::new(move |_k: &SeriesKey, _s: &Sample| {
            called_clone.fetch_add(1, Ordering::SeqCst);
        });
        let store = TimeSeriesStore::new(SeriesConfig::default()).with_anomaly_callback(cb);
        store.push(
            ("cb".to_string(), MetricKind::MemoryCount, "housekeeping".to_string()),
            Sample {
                id: String::new(),
                seq: 0,
                ts: 1,
                value: 42.0,
                labels: BTreeMap::new(),
            },
        );
        assert_eq!(called.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn push_records_anomaly_event_in_recent_anomalies() {
        let store = TimeSeriesStore::default();
        let key = ("test".to_string(), MetricKind::MemoryCount, "s".to_string());
        // Baseline samples
        for _ in 0..11 {
            store.push(
                key.clone(),
                Sample {
                    id: String::new(),
                    seq: 0,
                    ts: 1,
                    value: 10.0,
                    labels: BTreeMap::new(),
                },
            );
        }
        // Spike
        store.push(
            key.clone(),
            Sample {
                id: String::new(),
                seq: 0,
                ts: 2,
                value: 100.0,
                labels: BTreeMap::new(),
            },
        );
        let anomalies = store.recent_anomalies("test");
        assert!(!anomalies.is_empty(), "expected at least one anomaly event");
    }

    #[test]
    fn determinism_identical_samples() {
        let s1 = TimeSeriesStore::default();
        let s2 = TimeSeriesStore::default();
        let key = ("d".to_string(), MetricKind::IngestLatencyMs, "upsert".to_string());
        for _ in 0..3 {
            s1.push(
                key.clone(),
                Sample {
                    id: String::new(),
                    seq: 0,
                    ts: 10,
                    value: 5.0,
                    labels: BTreeMap::new(),
                },
            );
            s2.push(
                key.clone(),
                Sample {
                    id: String::new(),
                    seq: 0,
                    ts: 10,
                    value: 5.0,
                    labels: BTreeMap::new(),
                },
            );
        }
        let agg1 = s1.aggregate(&key).unwrap();
        let agg2 = s2.aggregate(&key).unwrap();
        assert_eq!(agg1, agg2);
        let q1 = s1.query_metrics("d", MetricKind::IngestLatencyMs, Some("upsert"), Some(0), Some(100), &[]);
        let q2 = s2.query_metrics("d", MetricKind::IngestLatencyMs, Some("upsert"), Some(0), Some(100), &[]);
        assert_eq!(q1, q2);
    }

    #[test]
    fn combine_aggs_folds_series_into_one_virtual_aggregate() {
        let mut a = RollingAgg::new(0.3);
        a.count = 3;
        a.sum = 6.0;
        a.min = 1.0;
        a.max = 3.0;
        a.ewma = 2.0;
        let mut b = RollingAgg::new(0.3);
        b.count = 1;
        b.sum = 4.0;
        b.min = 4.0;
        b.max = 4.0;
        b.ewma = 4.0;
        // An empty series contributes nothing and must not zero the fold.
        let empty = RollingAgg::new(0.3);

        let combined = RollingAgg::combine_aggs(&[a, b, empty]).expect("non-empty fold");
        assert_eq!(combined.count, 4);
        assert_eq!(combined.sum, 10.0);
        assert_eq!(combined.min, 1.0);
        assert_eq!(combined.max, 4.0);
        // Ewma blends by count: (2.0*3 + 4.0*1) / 4 = 2.5.
        assert!((combined.ewma - 2.5).abs() < 1e-9, "ewma weighted by count");
        // An all-empty input reports "no data" instead of a zero-series lie.
        let empty_only = [RollingAgg::new(0.3)];
        assert!(RollingAgg::combine_aggs(&empty_only).is_none());
    }

    #[test]
    fn series_config_fallback_matches_documented_defaults() {
        assert_eq!(
            bad_app_config_fallback(),
            SeriesConfig {
                capacity: 1024,
                flush_interval_secs: 60,
                ewma_alpha: 0.2,
            }
        );
    }

    #[test]
    fn non_finite_samples_are_dropped_with_warn() {
        let store = TimeSeriesStore::new(SeriesConfig {
            capacity: 8,
            ..SeriesConfig::default()
        });
        let key = ("nf".to_string(), MetricKind::MemoryCount, "s".to_string());
        store.push(
            key.clone(),
            Sample {
                id: String::new(),
                seq: 0,
                ts: 1,
                value: f64::NAN,
                labels: BTreeMap::new(),
            },
        );
        store.push(
            key.clone(),
            Sample {
                id: String::new(),
                seq: 0,
                ts: 2,
                value: f64::INFINITY,
                labels: BTreeMap::new(),
            },
        );
        store.push(
            key.clone(),
            Sample {
                id: String::new(),
                seq: 0,
                ts: 3,
                value: 7.0,
                labels: BTreeMap::new(),
            },
        );
        let agg = store.aggregate(&key).expect("series exists");
        assert_eq!(agg.count, 1, "only the finite sample is stored");
        assert_eq!(agg.sum, 7.0);
    }

    #[test]
    fn query_metrics_excludes_foreign_namespace_db_samples() {
        let store = TimeSeriesStore::new(SeriesConfig {
            capacity: 8,
            ..SeriesConfig::default()
        });
        let db = vec![
            (
                "other::memory_count::s::50::1".to_string(),
                Sample {
                    id: "other::memory_count::s::50::1".to_string(),
                    seq: 1,
                    ts: 50,
                    value: 100.0,
                    labels: BTreeMap::new(),
                },
            ),
            (
                "alpha::memory_count::s::50::2".to_string(),
                Sample {
                    id: "alpha::memory_count::s::50::2".to_string(),
                    seq: 2,
                    ts: 50,
                    value: 2.0,
                    labels: BTreeMap::new(),
                },
            ),
        ];
        let out = store.query_metrics("alpha", MetricKind::MemoryCount, Some("s"), None, None, &db);
        let samples = out["samples"].as_array().expect("samples array");
        assert_eq!(samples.len(), 1, "foreign-namespace DB sample must be filtered out");
        assert_eq!(samples[0]["id"], "alpha::memory_count::s::50::2");
        assert!(out["aggregate"]["sum"].as_f64().unwrap() - 2.0 < 1e-6);
    }

    #[test]
    fn query_metrics_drops_db_rows_for_other_metric_stage_and_ns_lookalikes() {
        let store = TimeSeriesStore::new(SeriesConfig {
            capacity: 8,
            ..SeriesConfig::default()
        });
        let db = vec![
            // Different metric, same namespace and stage.
            (
                "alpha::access_count::s::50::3".to_string(),
                Sample {
                    id: "alpha::access_count::s::50::3".to_string(),
                    seq: 3,
                    ts: 50,
                    value: 11.0,
                    labels: BTreeMap::new(),
                },
            ),
            // Same metric and namespace, different stage.
            (
                "alpha::memory_count::embed::51::4".to_string(),
                Sample {
                    id: "alpha::memory_count::embed::51::4".to_string(),
                    seq: 4,
                    ts: 51,
                    value: 12.0,
                    labels: BTreeMap::new(),
                },
            ),
            // Substring namespace look-alike: "a" matches inside "beta::...".
            (
                "beta::memory_count::s::52::5".to_string(),
                Sample {
                    id: "beta::memory_count::s::52::5".to_string(),
                    seq: 5,
                    ts: 52,
                    value: 13.0,
                    labels: BTreeMap::new(),
                },
            ),
            // The matching row that must survive the filter.
            (
                "alpha::memory_count::s::53::6".to_string(),
                Sample {
                    id: "alpha::memory_count::s::53::6".to_string(),
                    seq: 6,
                    ts: 53,
                    value: 2.0,
                    labels: BTreeMap::new(),
                },
            ),
        ];
        // The matching row used by the exact-namespace assertions below.
        // 1) Substring-namespace look-alike: querying "a" must not match
        //    ids merely containing "a" ("alpha::...", "beta::...").
        let out = store.query_metrics("a", MetricKind::MemoryCount, Some("s"), None, None, &db);
        let samples = out["samples"].as_array().expect("samples array");
        assert!(
            samples.is_empty(),
            "only rows whose namespace segment is exactly 'a' may survive; got {samples:?}"
        );
        assert_eq!(out["aggregate"]["sum"].as_f64().unwrap(), 0.0);

        // 2) Exact namespace, explicit stage: the foreign metric, the
        //    foreign stage and the "beta" look-alike are all dropped.
        let out = store.query_metrics("alpha", MetricKind::MemoryCount, Some("s"), None, None, &db);
        let samples = out["samples"].as_array().expect("samples array");
        assert_eq!(
            samples.len(),
            1,
            "foreign-metric, foreign-stage and namespace-lookalike DB rows must be filtered out"
        );
        assert_eq!(samples[0]["id"], "alpha::memory_count::s::53::6");

        // 3) A cross-stage sweep (no stage filter) admits the same metric's
        // other stage but still drops the foreign metric and the look-alike.
        let sweep = store.query_metrics("alpha", MetricKind::MemoryCount, None, None, None, &db);
        let swept = sweep["samples"].as_array().expect("samples array");
        let swept_ids: Vec<&str> = swept
            .iter()
            .filter_map(|s| s["id"].as_str())
            .collect();
        assert_eq!(
            swept_ids,
            vec![
                "alpha::memory_count::embed::51::4",
                "alpha::memory_count::s::53::6"
            ]
        );
    }
}
