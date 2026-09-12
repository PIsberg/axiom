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

    // The grandchild writes once the moment it is alive, then keeps appending
    // ten times a second for a minute. That first write is separate from the
    // loop because the whole test rests on it: `before > 0` below is what
    // separates "the tree was killed" from "nothing ever ran".
    //
    // The child waits for that write before sleeping past the deadline. It
    // does not make the grandchild start any sooner, but it keeps the child
    // alive for exactly as long as the grandchild has not started, so the two
    // are killed as one tree rather than the child having already moved on.
    //
    // Python reads the grandchild's program from a string, and that string
    // needs newlines spelled as escapes: the `{:?}` on `grandchild` keeps the
    // escaping out of this file.
    let grandchild = format!(
        "import time\nopen('{path_literal}', 'a').write('x')\nfor _ in range(600):\n    open('{path_literal}', 'a').write('x')\n    time.sleep(0.1)\n"
    );
    let snippet = format!(
        "import os, subprocess, sys, time\nprogram = {grandchild:?}\nsubprocess.Popen([sys.executable, '-c', program])\nwhile not os.path.exists('{path_literal}'):\n    time.sleep(0.02)\ntime.sleep(60)\n"
    );

    // The deadline has to outlast a cold interpreter start, because the
    // grandchild is a fresh `sys.executable` that must reach its first write
    // before the evaluator kills the tree. At the two seconds this used to
    // allow, a loaded windows runner could spend the whole budget getting
    // Python up, leaving an empty heartbeat file and a failure that said
    // nothing about process trees: seen on CI on 2026-09-11, green on a rerun
    // of the same commit. An idle interpreter start measures 30 to 72 ms here,
    // so the two seconds were not tight in themselves; what exhausts them is a
    // runner already compiling several snippets in parallel. Ten seconds is
    // two orders of magnitude above the idle figure and still a sixth of the
    // sixty the grandchild would otherwise live for, which is the gap every
    // assertion below rests on.
    const DEADLINE: Duration = Duration::from_secs(10);

    let started = std::time::Instant::now();
    let report = native::evaluate(python, "gate.py::is_open", &snippet, DEADLINE);
    let elapsed = started.elapsed();
    assert_eq!(report.status, CtopStatus::Timeout, "{report:?}");

    // The grandchild inherited the stdout pipe. Draining that pipe to EOF
    // after killing the child waited for the grandchild instead, which turned
    // the deadline into a sixty-second one. The deadline bounds the whole
    // call, not only the child, which is why the bound here is written
    // relative to DEADLINE rather than as its own number.
    assert!(
        elapsed < DEADLINE + Duration::from_secs(15),
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
        "the grandchild never started in {DEADLINE:?}, so this test established nothing; raise the deadline rather than this assertion"
    );
    assert_eq!(
        before, after,
        "the grandchild was still writing after the evaluator reported TIMEOUT"
    );
}
