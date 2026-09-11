//! Running a binary that is still held open for writing.
//!
//! Every tier with a build step writes an executable and then runs it: the
//! rustc path, the C and C++ recipes, and any restore from the artifact cache.
//! On Linux `execve` fails with `ETXTBSY` while any process holds that file open
//! for writing, and the holder is usually a fork of this one: `fs::write` opens
//! the file, another thread spawns a compiler in the window before the close,
//! the forked child inherits the descriptor, and the file stays busy until that
//! child reaches its own `execve`.
//!
//! It surfaced as a one-in-four failure of `artifact_cache`'s
//! `a_one_byte_change_misses_even_when_a_neighbour_entry_exists` on the CI
//! ubuntu runner and never on a developer machine, which is the usual shape: the
//! suite evaluates many snippets in parallel there. The symptom is the worst
//! kind available here, an `EvaluatorUnavailable` blaming a missing rustc for a
//! compiler that had just run successfully.
//!
//! Windows raises a sharing violation for the same situation and never
//! `ETXTBSY`, so there is nothing to pin there and this file is Unix-only. That
//! means it does not run on the machine most of this repository is developed on;
//! the ubuntu CI job is where it earns its place.

#![cfg(unix)]

use axiom_vmm::native::run_with_timeout;
use std::io::Write;
use std::process::Command;
use std::time::Duration;

/// A real ELF binary to run, copied so this test owns the file it holds open.
fn executable_copy(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let source = ["/bin/true", "/usr/bin/true"]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists())
        .expect("a POSIX system has /bin/true or /usr/bin/true");

    let dest = dir.join("axiom_spawn_retry_target");
    std::fs::copy(&source, &dest).expect("copying the target binary");
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
        .expect("making the copy executable");
    dest
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "axiom_spawn_retry_{}_{}_{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("creating the test directory");
    dir
}

/// The condition this is all about is real and reproducible.
///
/// Without this, the test below could pass because nothing was ever busy, which
/// would make the retry look proven while testing nothing. A plain `spawn` while
/// a write handle is open must fail, and must fail with `ETXTBSY` specifically,
/// rather than with some other error that happens to be non-zero.
#[test]
fn a_plain_spawn_of_a_file_held_open_for_writing_fails_with_etxtbsy() {
    let dir = temp_dir("plain");
    let binary = executable_copy(&dir);

    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(&binary)
        .expect("opening the binary for writing");

    let error = Command::new(&binary)
        .spawn()
        .expect_err("a binary held open for writing must not exec");

    assert_eq!(
        error.raw_os_error(),
        Some(26),
        "the condition under test is ETXTBSY, not {error:?}"
    );

    drop(held);
    std::fs::remove_dir_all(&dir).ok();
}

/// `run_with_timeout` waits for the writer to let go instead of reporting.
///
/// The handle is released after a delay, which is what the real race does when
/// the forked child reaches its `execve`. Before the retry existed this returned
/// the `ETXTBSY` straight to the caller, and the evaluator turned that into
/// `EvaluatorUnavailable` with a hint to install a compiler that was already
/// installed.
#[test]
fn run_with_timeout_waits_for_the_writer_to_let_go() {
    let dir = temp_dir("retry");
    let binary = executable_copy(&dir);

    let mut held = std::fs::OpenOptions::new()
        .write(true)
        .open(&binary)
        .expect("opening the binary for writing");

    // Long enough that the first spawn attempt certainly fails, short enough to
    // be well inside the retry deadline.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        let _ = held.flush();
        drop(held);
    });

    let finished = run_with_timeout(Command::new(&binary), Duration::from_secs(10))
        .expect("the spawn must be retried until the writer closes, not reported");

    assert!(
        !finished.timed_out,
        "the target exits immediately; a timeout means it never ran"
    );
    assert!(
        finished.succeeded(),
        "the copied binary exits zero: {finished:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// An error that cannot clear is still reported at once.
///
/// The retry has to stay narrow. Retrying a missing binary would turn an
/// immediate, accurate answer into a delayed one, which is the same rule
/// `worth_retrying` in axiom-ast encodes for its own platform-specific set.
#[test]
fn a_missing_binary_is_reported_rather_than_retried() {
    let dir = temp_dir("missing");
    let absent = dir.join("no_such_program");

    let started = std::time::Instant::now();
    let result = run_with_timeout(Command::new(&absent), Duration::from_secs(10));
    let elapsed = started.elapsed();

    assert!(result.is_err(), "a missing binary must not appear to run");
    assert!(
        elapsed < Duration::from_millis(400),
        "a missing binary cannot become present; reporting it took {elapsed:?}, \
         which means it was retried"
    );

    std::fs::remove_dir_all(&dir).ok();
}
