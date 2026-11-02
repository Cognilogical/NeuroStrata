use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    AccessCount,
    EnrichmentFailure,
    EnrichmentLatencyMs,
    RetrievalDistance,
    MemoryCount,
    IngestLatencyMs,
}

#[allow(non_upper_case_globals)]
pub const Latency: MetricKind = MetricKind::EnrichmentLatencyMs;

impl MetricKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MetricKind::AccessCount => "access_count",
            MetricKind::EnrichmentFailure => "enrichment_failure",
            MetricKind::EnrichmentLatencyMs => "enrichment_latency_ms",
            MetricKind::RetrievalDistance => "retrieval_distance",
            MetricKind::MemoryCount => "memory_count",
            MetricKind::IngestLatencyMs => "ingest_latency_ms",
        }
    }
}

impl std::str::FromStr for MetricKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "access_count" => Ok(MetricKind::AccessCount),
            "enrichment_failure" => Ok(MetricKind::EnrichmentFailure),
            "enrichment_latency_ms" => Ok(MetricKind::EnrichmentLatencyMs),
            "retrieval_distance" => Ok(MetricKind::RetrievalDistance),
            "memory_count" => Ok(MetricKind::MemoryCount),
            "ingest_latency_ms" => Ok(MetricKind::IngestLatencyMs),
            _ => Err(format!("unknown MetricKind: {}", s)),
        }
    }
}

pub type AnomalyKey = (String, MetricKind, String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnomalyEvent {
    pub metric: MetricKind,
    pub value: f64,
    /// The Welford running mean over all samples seen for this metric/stage
    /// (not the EWMA, which is exponentially weighted toward recent samples).
    pub mean: f64,
    pub std_dev: f64,
    /// Deviation of `value` from the EWMA, normalized by the Welford sample
    /// `std_dev` (floored at 1e-6 to keep zero-variance streams finite).
    pub z_score: f64,
    pub namespace: String,
    pub stage: String,
}

#[derive(Debug, Clone)]
pub struct WelfordAccumulator {
    pub count: u64,
    pub mean: f64,
    pub m2: f64,
    pub min: f64,
    pub max: f64,
}

impl WelfordAccumulator {
    pub fn new() -> Self {
        Self {
            count: 0,
            mean: 0.0,
            m2: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }

    pub fn add(&mut self, value: f64) {
        if self.count == 0 {
            self.count = 1;
            self.mean = value;
            self.m2 = 0.0;
            self.min = value;
            self.max = value;
            return;
        }
        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / (self.count as f64);
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
    }

    pub fn mean(&self) -> f64 {
        self.mean
    }

    pub fn std_dev(&self) -> f64 {
        if self.count < 2 {
            0.0
        } else {
            (self.m2 / ((self.count - 1) as f64)).sqrt()
        }
    }

    pub fn min(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.min
        }
    }

    pub fn max(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.max
        }
    }
}

#[derive(Debug, Clone)]
pub struct RollingStats {
    pub welford: WelfordAccumulator,
    pub ewma: f64,
    pub alpha: f64,
    pub z_threshold: f64,
    pub cooldown: Duration,
    pub last_emit: Option<Instant>,
}

impl RollingStats {
    pub fn new(alpha: f64, z_threshold: f64, cooldown: Duration) -> Self {
        Self {
            welford: WelfordAccumulator::new(),
            ewma: 0.0,
            alpha,
            z_threshold,
            cooldown,
            last_emit: None,
        }
    }

    pub fn record(
        &mut self,
        namespace: &str,
        metric: MetricKind,
        stage: &str,
        sample: f64,
        now: Instant,
    ) -> Option<AnomalyEvent> {
        self.welford.add(sample);
        if self.welford.count == 1 {
            self.ewma = sample;
            return None;
        }
        let std_dev = self.welford.std_dev();
        let span = (self.welford.max() - self.welford.min()).abs();
        let z_score = if (std_dev <= f64::EPSILON || span <= f64::EPSILON)
            && (sample - self.welford.mean()).abs() <= f64::EPSILON
        {
            0.0
        } else {
            (sample - self.ewma) / std_dev.max(1e-6)
        };
        self.ewma = self.alpha * sample + (1.0 - self.alpha) * self.ewma;
        let emit = z_score.abs() > self.z_threshold
            && self
                .last_emit
                .map(|t| now.duration_since(t) >= self.cooldown)
                .unwrap_or(true);
        if emit {
            self.last_emit = Some(now);
            Some(AnomalyEvent {
                metric,
                value: sample,
                mean: self.welford.mean(),
                std_dev,
                z_score,
                namespace: namespace.to_string(),
                stage: stage.to_string(),
            })
        } else {
            None
        }
    }
}

pub struct AnomalyDetector {
    pub stats: RwLock<HashMap<AnomalyKey, RollingStats>>,
    pub alpha: f64,
    pub z_threshold: f64,
    pub cooldown: Duration,
}

impl AnomalyDetector {
    pub fn new(alpha: f64, z_threshold: f64, cooldown: Duration) -> Self {
        Self {
            stats: RwLock::new(HashMap::new()),
            alpha,
            z_threshold,
            cooldown,
        }
    }

    pub fn observe(&self, namespace: &str, metric: MetricKind, stage: &str, sample: f64) -> Option<AnomalyEvent> {
        let key = (namespace.to_string(), metric, stage.to_string());
        let mut map = self
            .stats
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let stats = map.entry(key).or_insert_with(|| RollingStats::new(self.alpha, self.z_threshold, self.cooldown));
        stats.record(namespace, metric, stage, sample, Instant::now())
    }
}

impl Default for AnomalyDetector {
    fn default() -> Self {
        Self::new(0.2, 3.0, Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welford_mean_and_std_dev_manual() {
        let mut w = WelfordAccumulator::new();
        w.add(2.0);
        w.add(4.0);
        w.add(4.0);
        w.add(4.0);
        w.add(5.0);
        w.add(5.0);
        w.add(7.0);
        w.add(9.0);
        assert!((w.mean() - 5.0).abs() < 1e-6);
        // Sample std_dev over [2,4,4,4,5,5,7,9] = 2.138...; approximate check
        assert!(w.std_dev() > 2.0 && w.std_dev() < 2.3);
        assert_eq!(w.min(), 2.0);
        assert_eq!(w.max(), 9.0);
    }

    #[test]
    fn zero_variance_guard() {
        let mut w = WelfordAccumulator::new();
        w.add(5.0);
        w.add(5.0);
        w.add(5.0);
        assert_eq!(w.std_dev(), 0.0);
    }

    #[test]
    fn first_sample_guard_no_emit() {
        let mut s = RollingStats::new(0.2, 3.0, Duration::from_secs(60));
        let ev = s.record("ns", MetricKind::MemoryCount, "s", 42.0, Instant::now());
        assert!(ev.is_none());
        assert_eq!(s.welford.count, 1);
        assert_eq!(s.ewma, 42.0);
    }

    #[test]
    fn ewma_emits_outlier_and_cooldown() {
        let mut s = RollingStats::new(0.2, 3.0, Duration::from_secs(60));
        let now = Instant::now();
        // First sample initializes
        s.record("ns", MetricKind::MemoryCount, "s", 10.0, now);
        // Stable samples
        for _ in 0..10 {
            s.record("ns", MetricKind::MemoryCount, "s", 10.0, now + Duration::from_millis(1));
        }
        // Big outlier
        let ev = s.record("ns", MetricKind::MemoryCount, "s", 100.0, now + Duration::from_millis(100));
        assert!(ev.is_some());
        let ev = ev.unwrap();
        assert_eq!(ev.metric, MetricKind::MemoryCount);
        assert!(ev.z_score > 3.0);
        // Same instant should be blocked by cooldown
        let ev2 = s.record("ns", MetricKind::MemoryCount, "s", 100.0, now + Duration::from_millis(100));
        assert!(ev2.is_none());
    }
}
