use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::traits::Embedder;

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchSuite {
    All,
    Ingest,
    Embed,
    Lexical,
    Sparql,
    Gate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchConfig {
    pub suite: BenchSuite,
    pub iterations: usize,
    pub output: Option<PathBuf>,
    pub baseline: Option<PathBuf>,
    pub max_regression_pct: f64,
    pub fail_on_regression: bool,
    pub record_metrics: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteReceipt {
    pub name: String,
    pub iterations: usize,
    pub items_per_iter: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub mean_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub ops_per_sec: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteDiff {
    pub name: String,
    pub baseline_p50_ms: f64,
    pub current_p50_ms: f64,
    pub p50_delta_pct: f64,
    pub baseline_p95_ms: f64,
    pub current_p95_ms: f64,
    pub p95_delta_pct: f64,
    pub regressed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineComparison {
    pub baseline_git_sha: String,
    pub baseline_pkg_version: String,
    pub max_regression_pct: f64,
    pub passed: bool,
    pub suites: Vec<SuiteDiff>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerfReceipt {
    pub schema_version: u32,
    pub timestamp: String,
    pub git_sha: String,
    pub git_branch: String,
    pub pkg_version: String,
    pub os: String,
    pub arch: String,
    pub cpu_parallelism: usize,
    pub rss_before_mb: f64,
    pub rss_after_mb: f64,
    pub peak_rss_mb: f64,
    pub rss_delta_mb: f64,
    pub suites: Vec<SuiteReceipt>,
    pub comparison: Option<BaselineComparison>,
}

pub struct BenchEmbedder {
    dims: usize,
}

impl BenchEmbedder {
    pub fn new(dims: usize) -> Self {
        Self { dims: dims.max(1) }
    }
}

#[async_trait]
impl Embedder for BenchEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(self.embed_sync(text))
    }

    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.embed_sync(t)).collect())
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn embedder_id(&self) -> &str {
        "bench-cpu-hash-trigram"
    }
}

impl BenchEmbedder {
    fn embed_sync(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dims];
        let bytes = text.as_bytes();
        for w in bytes.windows(3) {
            let mut h: usize = 0;
            for b in w {
                h = h.wrapping_mul(31).wrapping_add(*b as usize);
            }
            v[h % self.dims] += 1.0;
        }
        // also char-level contribution for short texts
        for (i, c) in text.chars().enumerate() {
            v[(i * 7 + (c as usize)) % self.dims] += 0.5;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64) * p).ceil() as usize;
    let idx = if idx == 0 { 1 } else { idx } - 1;
    sorted[idx.min(sorted.len() - 1)]
}

pub fn compute_suite_receipt(
    name: &str,
    iterations: usize,
    items_per_iter: usize,
    durations_ms: &[f64],
) -> SuiteReceipt {
    let mut sorted: Vec<f64> = durations_ms
        .iter()
        .copied()
        .filter(|x| x.is_finite())
        .collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mean_ms = if sorted.is_empty() {
        0.0
    } else {
        sorted.iter().sum::<f64>() / sorted.len() as f64
    };
    let min_ms = sorted.first().copied().unwrap_or(0.0);
    let max_ms = sorted.last().copied().unwrap_or(0.0);
    SuiteReceipt {
        name: name.to_string(),
        iterations,
        items_per_iter,
        p50_ms: percentile(&sorted, 0.5),
        p95_ms: percentile(&sorted, 0.95),
        p99_ms: percentile(&sorted, 0.99),
        mean_ms,
        min_ms,
        max_ms,
        ops_per_sec: if mean_ms > 0.0 {
            items_per_iter as f64 / (mean_ms / 1000.0)
        } else {
            0.0
        },
    }
}

pub fn compare_receipts(
    current: &PerfReceipt,
    baseline: &PerfReceipt,
    max_regression_pct: f64,
) -> BaselineComparison {
    let mut suites = Vec::new();
    let mut passed = !current.suites.is_empty();
    for cur in &current.suites {
        if let Some(base) = baseline.suites.iter().find(|b| b.name == cur.name) {
            let p50_delta_pct = if base.p50_ms > 0.0 {
                (cur.p50_ms - base.p50_ms) / base.p50_ms * 100.0
            } else {
                0.0
            };
            let p95_delta_pct = if base.p95_ms > 0.0 {
                (cur.p95_ms - base.p95_ms) / base.p95_ms * 100.0
            } else {
                0.0
            };
            let regressed = p50_delta_pct > max_regression_pct || p95_delta_pct > max_regression_pct;
            if regressed {
                passed = false;
            }
            suites.push(SuiteDiff {
                name: cur.name.clone(),
                baseline_p50_ms: base.p50_ms,
                current_p50_ms: cur.p50_ms,
                p50_delta_pct,
                baseline_p95_ms: base.p95_ms,
                current_p95_ms: cur.p95_ms,
                p95_delta_pct,
                regressed,
            });
        } else {
            passed = false;
            suites.push(SuiteDiff {
                name: cur.name.clone(),
                baseline_p50_ms: 0.0,
                current_p50_ms: cur.p50_ms,
                p50_delta_pct: 100.0,
                baseline_p95_ms: 0.0,
                current_p95_ms: cur.p95_ms,
                p95_delta_pct: 100.0,
                regressed: true,
            });
        }
    }
    BaselineComparison {
        baseline_git_sha: baseline.git_sha.clone(),
        baseline_pkg_version: baseline.pkg_version.clone(),
        max_regression_pct,
        passed,
        suites,
    }
}

pub fn record_receipt_metrics(
    receipt: &PerfReceipt,
    store: &crate::time_series::TimeSeriesStore,
    namespace: &str,
) {
    let ts = Utc::now().timestamp();
    for suite in &receipt.suites {
        store.record(
            namespace,
            crate::time_series::MetricKind::EnrichmentLatencyMs,
            &suite.name,
            ts,
            suite.p50_ms,
            BTreeMap::new(),
        );
    }
}

pub async fn persist_receipt_metrics_to_store(
    receipt: &PerfReceipt,
    store: &crate::store::ladybug::LadybugStore,
    namespace: &str,
) -> anyhow::Result<usize> {
    record_receipt_metrics(receipt, store.metrics(), namespace);
    let written = store.flush_metrics().await?;
    if crate::traits::VectorStore::is_dirty(store) {
        crate::traits::VectorStore::checkpoint(store).await?;
    }
    Ok(written)
}

fn git_output(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn suites_to_run(suite: BenchSuite) -> Vec<BenchSuite> {
    match suite {
        BenchSuite::All => vec![
            BenchSuite::Ingest,
            BenchSuite::Embed,
            BenchSuite::Lexical,
            BenchSuite::Sparql,
            BenchSuite::Gate,
        ],
        other => vec![other],
    }
}

async fn run_ingest_suite(iterations: usize) -> SuiteReceipt {
    use crate::parser::ingest::ingest_directory;
    use crate::parser::schema::ParserSchema;

    let schema = ParserSchema::load(include_str!("schema.json")).expect("schema");
    let dims = 64usize;
    let mut samples = Vec::new();
    let items_per_iter = 3usize;
    for _ in 0..iterations.max(1) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("src");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "pub fn foo() { let x = 1; }\n").unwrap();
        std::fs::write(dir.join("b.rs"), "pub struct Bar { pub y: i32 }\n").unwrap();
        std::fs::write(dir.join("c.md"), "# Title\n\nSome docs.\n").unwrap();
        let store_path = tmp.path().join("store");
        let store = crate::store::ladybug::LadybugStore::for_testing(&store_path, dims)
            .expect("store");
        let store: std::sync::Arc<dyn crate::traits::VectorStore> = std::sync::Arc::new(store);
        store.init("bench").await.expect("init bench namespace");
        let embedder: std::sync::Arc<dyn crate::traits::Embedder> =
            std::sync::Arc::new(BenchEmbedder::new(dims));
        let start = Instant::now();
        ingest_directory(&dir, &schema, embedder, store, "bench", None, &[])
            .await
            .expect("ingest benchmark directory");
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        samples.push(ms);
    }
    compute_suite_receipt("ingest", iterations.max(1), items_per_iter, &samples)
}

async fn run_embed_suite(iterations: usize) -> SuiteReceipt {
    let embedder = BenchEmbedder::new(64);
    let texts: Vec<String> = (0..32).map(|i| format!("function number {} does work", i)).collect();
    let mut samples = Vec::new();
    for _ in 0..iterations.max(1) {
        let start = Instant::now();
        let _ = embedder.embed_batch(&texts).await;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        samples.push(ms);
    }
    compute_suite_receipt("embed", iterations.max(1), 32, &samples)
}

async fn run_lexical_suite(iterations: usize) -> SuiteReceipt {
    let mut samples = Vec::new();
    let items = 100usize;
    for _ in 0..iterations.max(1) {
        let start = Instant::now();
        for i in 0..items {
            let text = format!("symbol {} fn process_data value {}", i, i * 2);
            let _ = crate::lexical::tokenize_code_for_tantivy(&text);
            let _ = crate::lexical::detect_task_intent(&text);
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        samples.push(ms);
    }
    compute_suite_receipt("lexical", iterations.max(1), items, &samples)
}

async fn run_sparql_suite(iterations: usize) -> SuiteReceipt {
    use crate::rdf::sparql::{execute_sparql_query_with_config, get_sparql_config};
    use sparesults::QueryResultsFormat;

    let dims = 8usize;
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = crate::store::ladybug::LadybugStore::for_testing(tmp.path().join("store"), dims)
        .expect("store");
    crate::traits::VectorStore::init(&store, "bench")
        .await
        .expect("init sparql bench namespace");
    let config = get_sparql_config();
    let mut samples = Vec::new();
    for _ in 0..iterations.max(1) {
        let start = Instant::now();
        let _ = execute_sparql_query_with_config(
            &store,
            "bench",
            "SELECT ?s WHERE { ?s ?p ?o }",
            QueryResultsFormat::Json,
            false,
            false,
            config.clone(),
        )
        .await;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        samples.push(ms);
    }
    compute_suite_receipt("sparql", iterations.max(1), 1, &samples)
}

async fn run_gate_suite(iterations: usize) -> SuiteReceipt {
    use crate::task::gate;
    use std::sync::Arc;

    let dims = 8usize;
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = crate::store::ladybug::LadybugStore::for_testing(tmp.path().join("store"), dims)
        .expect("store");
    let store: Arc<dyn crate::traits::VectorStore> = Arc::new(store);
    let _ = store.init("bench").await;
    let mut samples = Vec::new();
    for _ in 0..iterations.max(1) {
        let start = Instant::now();
        let _ = gate::run(&store, "bench").await;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        samples.push(ms);
    }
    compute_suite_receipt("gate", iterations.max(1), 1, &samples)
}

pub async fn run_benchmarks(cfg: &BenchConfig) -> anyhow::Result<PerfReceipt> {
    let rss_before = crate::procmem::sample();
    let iterations = cfg.iterations.max(1);

    let mut suites = Vec::new();
    for s in suites_to_run(cfg.suite) {
        let sr = match s {
            BenchSuite::Ingest => run_ingest_suite(iterations).await,
            BenchSuite::Embed => run_embed_suite(iterations).await,
            BenchSuite::Lexical => run_lexical_suite(iterations).await,
            BenchSuite::Sparql => run_sparql_suite(iterations).await,
            BenchSuite::Gate => run_gate_suite(iterations).await,
            BenchSuite::All => unreachable!(),
        };
        suites.push(sr);
    }

    let rss_after = crate::procmem::sample();
    let mb = |b: Option<u64>| b.map(|x| x as f64 / (1024.0 * 1024.0)).unwrap_or(0.0);

    let mut receipt = PerfReceipt {
        schema_version: 1,
        timestamp: Utc::now().to_rfc3339(),
        git_sha: git_output(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string()),
        git_branch: git_output(&["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|| "unknown".to_string()),
        pkg_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpu_parallelism: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        rss_before_mb: mb(rss_before.rss_bytes),
        rss_after_mb: mb(rss_after.rss_bytes),
        peak_rss_mb: mb(rss_after.peak_bytes),
        rss_delta_mb: mb(rss_after.rss_bytes) - mb(rss_before.rss_bytes),
        suites,
        comparison: None,
    };

    if let Some(path) = &cfg.baseline {
        let raw = std::fs::read_to_string(path).with_context(|| {
            format!("failed to read benchmark baseline receipt from {}", path.display())
        })?;
        let base: PerfReceipt = serde_json::from_str(&raw).with_context(|| {
            format!("failed to parse benchmark baseline receipt from {}", path.display())
        })?;
        receipt.comparison = Some(compare_receipts(&receipt, &base, cfg.max_regression_pct));
    }

    if let Some(path) = &cfg.output {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create output directory {}", parent.display())
                })?;
            }
        }
        let serialized = serde_json::to_string_pretty(&receipt)
            .context("failed to serialize benchmark receipt")?;
        std::fs::write(path, serialized).with_context(|| {
            format!("failed to write benchmark receipt to {}", path.display())
        })?;
    }

    if let Some(ns) = &cfg.record_metrics {
        record_receipt_metrics(&receipt, crate::time_series::global_store(), ns);
        let db_path = if let Ok(val) = std::env::var("NEUROSTRATA_BENCH_METRICS_DB") {
            if !val.trim().is_empty() {
                PathBuf::from(val)
            } else {
                crate::config::Config::from_default_path()?.db_path
            }
        } else {
            crate::config::Config::from_default_path()?.db_path
        };
        let dims = crate::embed::configured_dimensions().unwrap_or(64).max(1);
        let store = crate::store::ladybug::LadybugStore::new(
            db_path.to_string_lossy().to_string(),
            dims,
        )
        .with_context(|| format!("failed to open metrics store at {}", db_path.display()))?;
        persist_receipt_metrics_to_store(&receipt, &store, ns).await?;
    }

    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_suite_receipt_percentiles() {
        let samples = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let r = compute_suite_receipt("t", 5, 2, &samples);
        assert_eq!(r.iterations, 5);
        assert!(r.p50_ms > 0.0);
        assert!(r.mean_ms > 0.0);
    }

    #[test]
    fn compare_receipts_detects_regression() {
        let base = PerfReceipt {
            schema_version: 1,
            timestamp: "t".into(),
            git_sha: "a".into(),
            git_branch: "main".into(),
            pkg_version: "1".into(),
            os: "linux".into(),
            arch: "x86".into(),
            cpu_parallelism: 1,
            rss_before_mb: 0.0,
            rss_after_mb: 0.0,
            peak_rss_mb: 0.0,
            rss_delta_mb: 0.0,
            suites: vec![SuiteReceipt {
                name: "ingest".into(),
                iterations: 1,
                items_per_iter: 1,
                p50_ms: 10.0,
                p95_ms: 12.0,
                p99_ms: 12.0,
                mean_ms: 10.0,
                min_ms: 10.0,
                max_ms: 10.0,
                ops_per_sec: 100.0,
            }],
            comparison: None,
        };
        let mut cur = base.clone();
        cur.suites[0].p50_ms = 100.0;
        cur.suites[0].p95_ms = 120.0;
        let cmp = compare_receipts(&cur, &base, 20.0);
        assert!(!cmp.passed);
        assert!(cmp.suites[0].regressed);
    }

    #[tokio::test]
    async fn run_benchmarks_all_suites() {
        let cfg = BenchConfig {
            suite: BenchSuite::All,
            iterations: 1,
            output: None,
            baseline: None,
            max_regression_pct: 20.0,
            fail_on_regression: false,
            record_metrics: None,
        };
        let r = run_benchmarks(&cfg).await.unwrap();
        assert_eq!(r.suites.len(), 5);
    }

    #[test]
    fn record_receipt_metrics_populates_store() {
        use crate::time_series::TimeSeriesStore;
        let cfg = BenchConfig {
            suite: BenchSuite::All,
            iterations: 1,
            output: None,
            baseline: None,
            max_regression_pct: 20.0,
            fail_on_regression: false,
            record_metrics: None,
        };
        // build a receipt by running not async? Use compute
        let receipt = PerfReceipt {
            schema_version: 1,
            timestamp: "t".into(),
            git_sha: "a".into(),
            git_branch: "main".into(),
            pkg_version: "1".into(),
            os: "linux".into(),
            arch: "x86".into(),
            cpu_parallelism: 1,
            rss_before_mb: 0.0,
            rss_after_mb: 0.0,
            peak_rss_mb: 0.0,
            rss_delta_mb: 0.0,
            suites: vec![SuiteReceipt {
                name: "ingest".into(),
                iterations: 1,
                items_per_iter: 1,
                p50_ms: 5.0,
                p95_ms: 5.0,
                p99_ms: 5.0,
                mean_ms: 5.0,
                min_ms: 5.0,
                max_ms: 5.0,
                ops_per_sec: 200.0,
            }],
            comparison: None,
        };
        let store = TimeSeriesStore::default();
        record_receipt_metrics(&receipt, &store, "ns");
        assert!(!store.list_keys("ns").is_empty());
        let _ = cfg;
    }

    #[test]
    fn compare_receipts_fails_when_requested_suite_missing_from_baseline() {
        let base = PerfReceipt {
            schema_version: 1,
            timestamp: "t".into(),
            git_sha: "a".into(),
            git_branch: "main".into(),
            pkg_version: "1".into(),
            os: "linux".into(),
            arch: "x86".into(),
            cpu_parallelism: 1,
            rss_before_mb: 0.0,
            rss_after_mb: 0.0,
            peak_rss_mb: 0.0,
            rss_delta_mb: 0.0,
            suites: vec![],
            comparison: None,
        };
        let mut cur = base.clone();
        cur.suites = vec![SuiteReceipt {
            name: "ingest".into(),
            iterations: 1,
            items_per_iter: 1,
            p50_ms: 5.0,
            p95_ms: 5.0,
            p99_ms: 5.0,
            mean_ms: 5.0,
            min_ms: 5.0,
            max_ms: 5.0,
            ops_per_sec: 200.0,
        }];
        let cmp = compare_receipts(&cur, &base, 20.0);
        assert!(!cmp.passed);
        assert_eq!(cmp.suites.len(), 1);
        assert!(cmp.suites[0].regressed);
        assert_eq!(cmp.suites[0].baseline_p50_ms, 0.0);
    }

    #[tokio::test]
    async fn run_benchmarks_errors_on_missing_or_invalid_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.json");
        let cfg = BenchConfig {
            suite: BenchSuite::All,
            iterations: 1,
            output: None,
            baseline: Some(missing.clone()),
            max_regression_pct: 20.0,
            fail_on_regression: false,
            record_metrics: None,
        };
        assert!(run_benchmarks(&cfg).await.is_err());

        let bad = tmp.path().join("bad.json");
        std::fs::write(&bad, "not json").unwrap();
        let cfg2 = BenchConfig {
            suite: BenchSuite::All,
            iterations: 1,
            output: None,
            baseline: Some(bad),
            max_regression_pct: 20.0,
            fail_on_regression: false,
            record_metrics: None,
        };
        assert!(run_benchmarks(&cfg2).await.is_err());
    }

    #[tokio::test]
    async fn run_benchmarks_errors_on_unwritable_output_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("outdir");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = BenchConfig {
            suite: BenchSuite::All,
            iterations: 1,
            output: Some(dir),
            baseline: None,
            max_regression_pct: 20.0,
            fail_on_regression: false,
            record_metrics: None,
        };
        assert!(run_benchmarks(&cfg).await.is_err());
    }

    #[tokio::test]
    async fn persist_receipt_metrics_to_store_flushes_to_ladybug() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::ladybug::LadybugStore::for_testing(tmp.path().join("store"), 8)
            .expect("store");
        let receipt = PerfReceipt {
            schema_version: 1,
            timestamp: "t".into(),
            git_sha: "a".into(),
            git_branch: "main".into(),
            pkg_version: "1".into(),
            os: "linux".into(),
            arch: "x86".into(),
            cpu_parallelism: 1,
            rss_before_mb: 0.0,
            rss_after_mb: 0.0,
            peak_rss_mb: 0.0,
            rss_delta_mb: 0.0,
            suites: vec![SuiteReceipt {
                name: "ingest".into(),
                iterations: 1,
                items_per_iter: 1,
                p50_ms: 5.0,
                p95_ms: 5.0,
                p99_ms: 5.0,
                mean_ms: 5.0,
                min_ms: 5.0,
                max_ms: 5.0,
                ops_per_sec: 200.0,
            }],
            comparison: None,
        };
        let written = persist_receipt_metrics_to_store(&receipt, &store, "ns").await.unwrap();
        assert!(written > 0);
        let samples = store
            .query_db_metrics("ns", crate::time_series::MetricKind::EnrichmentLatencyMs, "ingest", None, None, 10)
            .await
            .unwrap();
        assert!(!samples.is_empty());
    }
}
