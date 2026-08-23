//! What a snippet can see of the server's environment.
//!
//! A child process inherits its parent's environment unless told otherwise,
//! and the evaluator's parent holds the one thing a snippet must never read:
//! the signing key. Measured before the confinement existed, on 2026-08-23: a
//! Python snippet printed `AXIOM_SIGNING_KEY` and the report carried the value
//! back to the caller in `stdout`, and a Rust snippet did the same through a
//! panic message in `stderr`. That hands the party whose claims the signature
//! exists to check the means to sign anything at all.
//!
//! The key is the obvious case and not the only one. Whatever else the
//! operator's shell carries, API tokens and cloud credentials among them, is
//! one `os.environ` away, so the child gets a fresh environment holding only
//! what a toolchain needs, plus whatever `AXIOM_EVAL_ENV_PASS` names.

use axiom_proto::CtopStatus;
use axiom_vmm::native;
use axiom_vmm::{SandboxEngine, WasiEngine};
use std::sync::Mutex;
use std::time::Duration;

/// Tests in this file mutate the process environment, which every other test
/// in the binary reads. They take turns.
static ENV: Mutex<()> = Mutex::new(());

/// A failed test poisons the mutex; the next test still needs its turn.
fn take_turn() -> std::sync::MutexGuard<'static, ()> {
    ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A variable set for the duration of a test and removed afterwards.
struct Var(&'static str);

impl Var {
    fn set(name: &'static str, value: &str) -> Self {
        // SAFETY: callers hold ENV, so no other thread in this binary writes
        // the environment while this one does.
        unsafe { std::env::set_var(name, value) };
        Var(name)
    }
}

impl Drop for Var {
    fn drop(&mut self) {
        // SAFETY: as above; the lock is still held by the test that owns this.
        unsafe { std::env::remove_var(self.0) };
    }
}

const PRINT_THREE: &str = "import os\nprint(' '.join(os.environ.get(k, 'absent') for k in ('AXIOM_SIGNING_KEY', 'AXIOM_SIGNING_KEY_FILE', 'MY_TOKEN')))\n";

fn run_python(snippet: &str) -> axiom_proto::CtopReport {
    let python = native::language_for("py").expect("python is a known language");
    native::evaluate(python, "gate.py::is_open", snippet, Duration::from_secs(30))
}

#[test]
fn the_signing_key_is_not_visible_to_a_snippet() {
    let _turn = take_turn();
    let _key = Var::set("AXIOM_SIGNING_KEY", "marker-private-key-bytes");
    let _file = Var::set("AXIOM_SIGNING_KEY_FILE", "/keys/agent.key");
    let _token = Var::set("MY_TOKEN", "marker-some-other-secret");

    let report = run_python(PRINT_THREE);
    let python = native::language_for("py").unwrap();
    match native::usable_toolchain(python) {
        Some(program) => {
            assert_eq!(
                report.status,
                CtopStatus::Passed,
                "{program} should have run the snippet: {report:?}"
            );
            assert!(
                !report.stdout.contains("marker"),
                "the snippet could read a secret from the environment: {:?}",
                report.stdout
            );
            assert_eq!(
                report.stdout.trim(),
                "absent absent absent",
                "every variable outside the allowlist must be absent in the child"
            );
        }
        None => assert_eq!(report.status, CtopStatus::EvaluatorUnavailable),
    }
}

#[test]
fn a_variable_named_in_the_pass_list_reaches_the_snippet() {
    let _turn = take_turn();
    let _pass = Var::set("AXIOM_EVAL_ENV_PASS", "MY_TOKEN, UNSET_ONE");
    let _token = Var::set("MY_TOKEN", "marker-passed-on-purpose");

    let report = run_python(PRINT_THREE);
    let python = native::language_for("py").unwrap();
    if native::usable_toolchain(python).is_some() {
        assert_eq!(report.status, CtopStatus::Passed, "{report:?}");
        assert_eq!(
            report.stdout.trim(),
            "absent absent marker-passed-on-purpose",
            "a variable the operator named must arrive, and nothing else must"
        );
    }
}

#[test]
fn the_signing_key_is_refused_even_when_the_pass_list_names_it() {
    let _turn = take_turn();
    let _pass = Var::set("AXIOM_EVAL_ENV_PASS", "AXIOM_SIGNING_KEY,AXIOM_SIGNING_KEY_FILE");
    let _key = Var::set("AXIOM_SIGNING_KEY", "marker-private-key-bytes");
    let _file = Var::set("AXIOM_SIGNING_KEY_FILE", "/keys/agent.key");

    let report = run_python(PRINT_THREE);
    let python = native::language_for("py").unwrap();
    if native::usable_toolchain(python).is_some() {
        assert_eq!(report.status, CtopStatus::Passed, "{report:?}");
        assert_eq!(
            report.stdout.trim(),
            "absent absent absent",
            "no configuration may hand the signing key to a snippet"
        );
    }
}

/// The Rust tier spawns `rustc` and then the compiled binary itself, neither
/// of them through the tier 2 recipes, so it has to be checked on its own.
#[tokio::test]
async fn the_rust_tier_confines_the_environment_too() {
    let _turn = take_turn();
    let _key = Var::set("AXIOM_SIGNING_KEY", "marker-private-key-bytes");

    let engine = WasiEngine::new().expect("engine");
    let report = engine
        .execute_eval_in(
            "anonymous",
            "panic!(\"RUST_LEAK={:?}\", std::env::var(\"AXIOM_SIGNING_KEY\"));",
            None,
        )
        .await
        .expect("a report");

    if report.status == CtopStatus::EvaluatorUnavailable {
        // No rustc here. The refusal is the right answer and says nothing
        // about this property; CI has rustc and runs the branch below.
        return;
    }
    assert_eq!(report.status, CtopStatus::Failed, "{report:?}");
    assert!(
        !report.stderr.contains("marker") && !report.stdout.contains("marker"),
        "the compiled snippet could read the signing key: {:?}",
        report.stderr
    );
    assert!(
        report.stderr.contains("RUST_LEAK=Err(NotPresent)"),
        "the variable must be absent, not merely empty: {:?}",
        report.stderr
    );
}
