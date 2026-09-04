//! Bookkeeping around the evaluators: warm-up, recycling policy, telemetry.
//!
//! What "warm" means here, precisely, because an earlier version of this
//! comment promised pre-warmed execution sandboxes that did not exist. A
//! worker holds no resident process; every evaluation still spawns the
//! toolchain. What `warmup` does prime is real and bounded: the per-process
//! toolchain probe and version caches (one spawn per language instead of one
//! on the first evaluation's clock) and the artifact-cache root. The heavy
//! saving comes from the content-addressed artifact cache in
//! [`crate::artifact_cache`], which the pool surfaces per language as
//! `cache_hits`. A resident-process tier, a JVM that compiles in-process, is
//! design rather than code, and nothing here claims it.
//!
//! The pool also owns eviction policy: idle workers are dropped after
//! `idle_evict_duration`, and `evict_idle` prunes the artifact cache to its
//! size cap at the same time.

use crate::artifact_cache;
use crate::native::{DEFAULT_TIMEOUT, NativeLanguage, evaluate, language_for, prime};
use axiom_proto::CtopReport;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Configuration for the Daemon Worker Pool.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Maximum concurrent workers per language.
    pub max_workers_per_lang: usize,
    /// Maximum number of evaluations a single worker can perform before recycling.
    pub max_evals_before_recycle: u32,
    /// Default execution timeout.
    pub default_timeout: Duration,
    /// Keep-alive idle duration before eviction.
    pub idle_evict_duration: Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            max_workers_per_lang: 4,
            max_evals_before_recycle: 100,
            default_timeout: DEFAULT_TIMEOUT,
            idle_evict_duration: Duration::from_secs(300),
        }
    }
}

/// Statistics for a daemon worker.
#[derive(Debug, Clone, Default)]
pub struct WorkerStats {
    pub total_evaluations: u64,
    pub successful_evaluations: u64,
    pub total_duration_ms: f64,
    pub recycled_count: u64,
    /// Evaluations whose compile step was served by the artifact cache. The
    /// verdicts still came from real runs; only the compile was skipped.
    pub cache_hits: u64,
}

/// An individual managed worker instance.
#[allow(dead_code)]
struct WorkerInstance {
    id: String,
    language: String,
    created_at: Instant,
    last_used_at: Instant,
    eval_count: u32,
    work_dir: Option<PathBuf>,
}

impl WorkerInstance {
    fn new(language: &str) -> Self {
        // A counter rather than `Instant::now().elapsed()`, which measures
        // the time since the same statement and is always near zero, so every
        // worker got the same id.
        static IDS: AtomicU64 = AtomicU64::new(1);
        let id = format!(
            "worker_{}_{}",
            language,
            IDS.fetch_add(1, Ordering::Relaxed)
        );
        Self {
            id,
            language: language.to_string(),
            created_at: Instant::now(),
            last_used_at: Instant::now(),
            eval_count: 0,
            work_dir: None,
        }
    }

    fn should_recycle(&self, max_evals: u32) -> bool {
        self.eval_count >= max_evals
    }
}

/// Daemon Worker Pool managing warm language evaluation contexts.
pub struct DaemonPool {
    config: DaemonConfig,
    workers: Mutex<HashMap<String, VecDeque<WorkerInstance>>>,
    stats: Mutex<HashMap<String, WorkerStats>>,
}

impl DaemonPool {
    /// Create a new Daemon Pool with custom configuration.
    pub fn new(config: DaemonConfig) -> Self {
        Self {
            config,
            workers: Mutex::new(HashMap::new()),
            stats: Mutex::new(HashMap::new()),
        }
    }

    /// Access the global shared Daemon Pool singleton.
    pub fn global() -> &'static DaemonPool {
        static GLOBAL: OnceLock<DaemonPool> = OnceLock::new();
        GLOBAL.get_or_init(|| DaemonPool::new(DaemonConfig::default()))
    }

    /// Prime what can be primed before the first evaluation asks for it.
    ///
    /// Concretely: the toolchain probe and version caches (each is one
    /// process spawn, memoized for the life of this process, that would
    /// otherwise land on the first evaluation's clock), the artifact-cache
    /// root, and the worker queue the recycling policy runs on. No resident
    /// process is started, and nothing here makes a cold toolchain fast.
    pub fn warmup(&self, languages: &[&str]) {
        if artifact_cache::enabled() {
            let _ = std::fs::create_dir_all(artifact_cache::cache_root());
        }
        let mut map = self.workers.lock().unwrap();
        for &lang in languages {
            if let Some(known) = language_for(lang) {
                prime(known);
            }
            let queue = map.entry(lang.to_string()).or_default();
            while queue.len() < self.config.max_workers_per_lang.min(2) {
                queue.push_back(WorkerInstance::new(lang));
            }
        }
    }

    /// Evaluate a snippet using a warm worker from the daemon pool.
    pub fn evaluate(
        &self,
        language: &NativeLanguage,
        snippet: &str,
        symbol_path: &str,
        timeout: Option<Duration>,
    ) -> CtopReport {
        let lang_key = language.extension.to_string();
        let timeout_dur = timeout.unwrap_or(self.config.default_timeout);

        // 1. Acquire an idle worker or instantiate one
        let mut worker = {
            let mut map = self.workers.lock().unwrap();
            let queue = map.entry(lang_key.clone()).or_default();
            queue
                .pop_front()
                .unwrap_or_else(|| WorkerInstance::new(&lang_key))
        };

        // 2. Perform execution
        let start = Instant::now();
        let report = evaluate(language, symbol_path, snippet, timeout_dur);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        // 3. Update worker and pool statistics
        worker.eval_count += 1;
        worker.last_used_at = Instant::now();

        let is_pass = matches!(report.status, axiom_proto::CtopStatus::Passed);
        let mut recycled = false;

        {
            let mut stats_map = self.stats.lock().unwrap();
            let s = stats_map.entry(lang_key.clone()).or_default();
            s.total_evaluations += 1;
            if is_pass {
                s.successful_evaluations += 1;
            }
            if report.compile_cache.as_deref() == Some("hit") {
                s.cache_hits += 1;
            }
            s.total_duration_ms += elapsed_ms;

            if worker.should_recycle(self.config.max_evals_before_recycle) {
                s.recycled_count += 1;
                recycled = true;
            }
        }

        // 4. Return worker to pool if still healthy, or replace
        if !recycled {
            let mut map = self.workers.lock().unwrap();
            let queue = map.entry(lang_key).or_default();
            if queue.len() < self.config.max_workers_per_lang {
                queue.push_back(worker);
            }
        } else {
            // Fresh replacement
            let mut map = self.workers.lock().unwrap();
            let queue = map.entry(lang_key.clone()).or_default();
            if queue.len() < self.config.max_workers_per_lang {
                queue.push_back(WorkerInstance::new(&lang_key));
            }
        }

        report
    }

    /// Retrieve telemetry and evaluation metrics per language.
    pub fn get_stats(&self) -> HashMap<String, WorkerStats> {
        self.stats.lock().unwrap().clone()
    }

    /// Count active warm workers in the pool for a given language.
    pub fn warm_worker_count(&self, language: &str) -> usize {
        self.workers
            .lock()
            .unwrap()
            .get(language)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    /// Evict idle workers that have exceeded idle_evict_duration, and prune
    /// the artifact cache to its size cap while at it.
    pub fn evict_idle(&self) -> usize {
        artifact_cache::evict_cache_to_cap();
        let mut evicted = 0;
        let now = Instant::now();
        let mut map = self.workers.lock().unwrap();
        for queue in map.values_mut() {
            let before = queue.len();
            queue.retain(|w| now.duration_since(w.last_used_at) < self.config.idle_evict_duration);
            evicted += before - queue.len();
        }
        evicted
    }
}
