use axiom_vmm::daemon::{DaemonConfig, DaemonPool};
use axiom_vmm::native::language_for;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn daemon_pool_warmup_and_worker_tracking() {
    let config = DaemonConfig {
        max_workers_per_lang: 3,
        max_evals_before_recycle: 5,
        default_timeout: Duration::from_secs(5),
        idle_evict_duration: Duration::from_millis(50),
    };
    let pool = DaemonPool::new(config);

    assert_eq!(pool.warm_worker_count("py"), 0);
    pool.warmup(&["py", "js"]);
    assert_eq!(pool.warm_worker_count("py"), 2);
    assert_eq!(pool.warm_worker_count("js"), 2);
}

#[test]
fn daemon_pool_evaluates_and_tracks_stats() {
    let config = DaemonConfig {
        max_workers_per_lang: 2,
        max_evals_before_recycle: 2,
        default_timeout: Duration::from_secs(5),
        idle_evict_duration: Duration::from_secs(300),
    };
    let pool = DaemonPool::new(config);

    let lang = match language_for("py") {
        Some(l) => l,
        None => return,
    };

    let report1 = pool.evaluate(lang, "x = 40 + 2\nassert x == 42", "test.py::check", None);
    let _report2 = pool.evaluate(lang, "x = 100\nassert x == 100", "test.py::check", None);

    let stats = pool.get_stats();
    if let Some(py_stats) = stats.get("py") {
        assert_eq!(py_stats.total_evaluations, 2);
        if matches!(report1.status, axiom_proto::CtopStatus::Passed) {
            assert!(py_stats.successful_evaluations >= 1);
        }
        // Worker was recycled after 2 evaluations
        assert_eq!(py_stats.recycled_count, 1);
    }
}

#[test]
fn daemon_pool_idle_worker_eviction() {
    let config = DaemonConfig {
        max_workers_per_lang: 2,
        max_evals_before_recycle: 10,
        default_timeout: Duration::from_secs(5),
        idle_evict_duration: Duration::from_millis(50),
    };
    let pool = DaemonPool::new(config);
    pool.warmup(&["py"]);
    assert_eq!(pool.warm_worker_count("py"), 2);

    // Sleep past eviction timeout
    thread::sleep(Duration::from_millis(100));
    let evicted = pool.evict_idle();
    assert_eq!(evicted, 2);
    assert_eq!(pool.warm_worker_count("py"), 0);
}

#[test]
fn daemon_pool_concurrent_access() {
    let config = DaemonConfig {
        max_workers_per_lang: 4,
        max_evals_before_recycle: 10,
        default_timeout: Duration::from_secs(5),
        idle_evict_duration: Duration::from_secs(300),
    };
    let pool = Arc::new(DaemonPool::new(config));
    pool.warmup(&["py"]);

    let lang = match language_for("py") {
        Some(l) => l,
        None => return,
    };

    let mut handles = Vec::new();
    for i in 0..4 {
        let p = Arc::clone(&pool);
        handles.push(thread::spawn(move || {
            let snippet = format!("val = {}\nassert val == {}", i, i);
            p.evaluate(lang, &snippet, "test.py::calc", None)
        }));
    }

    for h in handles {
        let _ = h.join().expect("thread join failed");
    }

    let stats = pool.get_stats();
    if let Some(py_stats) = stats.get("py") {
        assert_eq!(py_stats.total_evaluations, 4);
    }
}
