//! A snippet that does not terminate must not take the server with it.
//!
//! `Command::output` waits forever. The eval tools are reached over a stdio
//! pipe an agent is blocked on, so one `while True` used to mean the session
//! stopped answering with no way to tell why.

use axiom_proto::CtopStatus;
use axiom_vmm::native;
use std::time::{Duration, Instant};

#[test]
fn a_snippet_that_never_finishes_is_killed_and_reported() {
    let python = native::language_for("py").expect("python is a known language");

    let deadline = Duration::from_secs(2);
    let started = Instant::now();
    let report = native::evaluate(
        python,
        "gate.py::is_open",
        "while True:\n    pass",
        deadline,
    );
    let elapsed = started.elapsed();

    match native::usable_toolchain(python) {
        Some(program) => {
            assert_eq!(
                report.status,
                CtopStatus::Timeout,
                "{program} ran past the deadline and should be reported as a timeout: {report:?}"
            );
            assert_eq!(report.passed_checks_count, 0);
            assert!(
                elapsed < deadline * 8,
                "the deadline should end the run, not the snippet: waited {elapsed:?}"
            );
            assert!(report
                .failed_checks
                .iter()
                .any(|c| c.error_type == "EvaluationTimeout"));
        }
        None => {
            assert_eq!(
                report.status,
                CtopStatus::EvaluatorUnavailable,
                "no python, so nothing ran: {report:?}"
            );
        }
    }
}

#[test]
fn a_language_with_no_recipe_is_not_silently_borrowed_from_another() {
    // Kotlin and Scala are indexed by the Java parser, and each is now run by
    // its own compiler. Sharing a parser is not sharing a compiler: handing
    // either to javac would file the error against the snippet rather than
    // against the language.
    assert_eq!(
        native::language_for("kt").map(|l| l.engine),
        Some("tier2_native_kotlin"),
        ".kt must go to kotlinc, not to javac"
    );
    assert_eq!(
        native::language_for("scala").map(|l| l.engine),
        Some("tier2_native_scala"),
        ".scala must go to the Scala runner, not to javac"
    );
    assert_eq!(
        native::language_for("java").map(|l| l.engine),
        Some("tier2_native_java")
    );

    // An extension nothing here reads is still refused rather than handed to
    // whichever compiler happens to be nearest.
    assert!(native::language_for("rb").is_none());

    // Node's extensions all resolve to the one recipe rather than each needing
    // their own entry.
    for ext in ["js", "mjs", "cjs", "jsx"] {
        assert_eq!(
            native::language_for(ext).map(|l| l.engine),
            Some("tier2_native_node"),
            ".{ext} should be run by node"
        );
    }
}

/// A toolchain that never reached the snippet must not produce a verdict about
/// it.
///
/// Observed on CI: `scala` spent 134 seconds failing to fetch its own compiler
/// dependencies, exited non-zero, and `axiom_eval_patch` reported `FAILED`. No
/// user code ran. That tells an agent its change is wrong on the strength of a
/// download, which is the same class as the assertion-substring fallback removed
/// earlier: a verdict produced by something that is not a run of the code.
///
/// `AXIOM_EVAL_TIMEOUT_SECS` does not cover it. CI raises that to 300 for the
/// cold-cache case and this run was well inside it.
#[test]
fn a_resolver_failure_is_not_a_verdict_about_the_snippet() {
    // The real stderr from the CI run that prompted this, trimmed.
    let stderr = "\
Downloading https://central.sonatype.com/repository/maven-snapshots/ch/epfl/scala/bloop-frontend_2.12/2.0.19/bloop-frontend_2.12-2.0.19.pom
Failed to download https://central.sonatype.com/repository/maven-snapshots/ch/epfl/scala/bloop-frontend_2.12/2.0.19/bloop-frontend_2.12-2.0.19.pom
";
    let reason = native::toolchain_failure_reason("", stderr)
        .expect("a download failure is the toolchain not getting going");
    assert!(
        reason.contains("did not get as far as running"),
        "the reason must say what did not happen: {reason}"
    );
}

/// The other direction, which matters more: a real failure must stay a real
/// failure. Widening the markers until they swallow genuine verdicts would trade
/// one wrong answer for another.
#[test]
fn a_real_assertion_failure_is_still_a_verdict() {
    let stderr = "\
Exception in thread \"main\" java.lang.AssertionError: assertion failed
	at scala.runtime.Scala3RunTime$.assertFailed(Scala3RunTime.scala:13)
	at AxiomEval$.main(AxiomEval.scala:3)
";
    assert!(
        native::toolchain_failure_reason("", stderr).is_none(),
        "an AssertionError is the snippet failing, and must keep its verdict"
    );
    assert!(native::toolchain_failure_reason("", "").is_none());
    assert!(
        native::toolchain_failure_reason("", "error[E0425]: cannot find value `x`").is_none(),
        "a compiler diagnostic is the toolchain having read the snippet"
    );
}
