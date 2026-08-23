//! A timeout has to end everything the snippet started, not only the process
//! the evaluator spawned.
//!
//! `go run` builds a binary and runs it as a child; the `kotlin` launcher
//! starts a JVM; a Python snippet can `Popen` whatever it likes. Killing the
//! direct child alone left all of those running past the deadline, with the
//! report saying TIMEOUT as if the matter were closed. Unix puts the child in
//! its own process group and signals the group; Windows asks taskkill to take
//! the tree.

use axiom_proto::CtopStatus;
use axiom_vmm::native;
use std::time::Duration;

#[test]
fn a_grandchild_does_not_outlive_the_deadline() {
    let python = native::language_for("py").expect("python is a known language");
    if native::usable_toolchain(python).is_none() {
        let report = native::evaluate(python, "gate.py::is_open", "pass", Duration::from_secs(2));
        assert_eq!(report.status, CtopStatus::EvaluatorUnavailable);
        return;
    }

    let heartbeat = std::env::temp_dir().join(format!(
        "axiom_heartbeat_{}_{}.txt",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_file(&heartbeat);
    let path_literal = heartbeat
        .display()
        .to_string()
        .replace(char::from(92u8), "/");

    // The grandchild appends to the heartbeat file ten times a second for a
    // minute. The child itself then sleeps past the deadline so the evaluator
    // has to kill it.
    // Python reads the grandchild's program from a string, and that string
    // needs newlines spelled as escapes: `chr(10)` keeps the escaping out of
    // this file.
    let grandchild = format!(
        "import time\nfor _ in range(600):\n    open('{path_literal}', 'a').write('x')\n    time.sleep(0.1)\n"
    );
    let snippet = format!(
        "import subprocess, sys, time\nprogram = {grandchild:?}\nsubprocess.Popen([sys.executable, '-c', program])\ntime.sleep(60)\n"
    );

    let started = std::time::Instant::now();
    let report = native::evaluate(python, "gate.py::is_open", &snippet, Duration::from_secs(2));
    let elapsed = started.elapsed();
    assert_eq!(report.status, CtopStatus::Timeout, "{report:?}");

    // The grandchild inherited the stdout pipe. Draining that pipe to EOF
    // after killing the child waited for the grandchild instead, which turned
    // a two-second deadline into a sixty-second one. The deadline bounds the
    // whole call, not only the child.
    assert!(
        elapsed < Duration::from_secs(8),
        "evaluate must return near its deadline, not when the grandchild feels like exiting: {elapsed:?}"
    );

    // Give a surviving grandchild time to show itself, then watch for growth.
    std::thread::sleep(Duration::from_millis(800));
    let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let before = size(&heartbeat);
    std::thread::sleep(Duration::from_millis(1200));
    let after = size(&heartbeat);
    let _ = std::fs::remove_file(&heartbeat);

    assert!(
        before > 0,
        "the grandchild never started, so this test established nothing"
    );
    assert_eq!(
        before, after,
        "the grandchild was still writing after the evaluator reported TIMEOUT"
    );
}
