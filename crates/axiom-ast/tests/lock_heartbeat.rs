//! A held lock stays fresh, so a write that legitimately runs longer than the
//! staleness window is not judged dead and taken over mid-write.
//!
//! Staleness is judged by the lock file's mtime: a lock untouched for longer
//! than the window is taken to belong to a crashed holder. A large index save
//! under contention can exceed that window, and nothing refreshed the lock
//! while it was held, so a live writer looked dead. A heartbeat touches the
//! lock file while it is held, so an aged mtime now means the holder is really
//! gone.

use axiom_ast::IndexLock;
use std::time::Duration;

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_hb_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

#[test]
fn a_lock_held_past_the_staleness_window_stays_fresh() {
    let dir = tmp("held");
    let index = dir.join("index.json");
    let lock_file = index.with_extension("lock");

    let held = IndexLock::acquire(&index).expect("acquire");

    // Hold it well past the two-second staleness window without doing anything
    // that touches the lock file directly.
    std::thread::sleep(Duration::from_millis(2500));

    // The heartbeat must have refreshed the lock's mtime, so its age is far
    // under the staleness window. Without the heartbeat this age is ~2.5s and
    // another agent would have taken the lock over by now.
    let age = std::fs::metadata(&lock_file)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().unwrap_or_default())
        .expect("lock file exists while held");
    assert!(
        age < Duration::from_millis(1500),
        "a held lock must be kept fresh; its mtime is {age:?} old, which reads as stale"
    );

    drop(held);
    assert!(
        !lock_file.exists(),
        "the lock and its heartbeat must be gone once dropped"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_heartbeat_stops_and_does_not_recreate_the_lock_after_release() {
    let dir = tmp("release");
    let index = dir.join("index.json");
    let lock_file = index.with_extension("lock");

    let held = IndexLock::acquire(&index).expect("acquire");
    drop(held);
    assert!(!lock_file.exists(), "released immediately");

    // If a heartbeat outlived the drop it would recreate the file within an
    // interval. It must not.
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        !lock_file.exists(),
        "a heartbeat recreated the lock after it was released"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
