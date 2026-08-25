//! Verdicts the evaluator used to hand out without running anything.
//!
//! The rule the whole tool rests on is that `PASSED` is only ever produced by
//! a run that happened and succeeded, and that a negative verdict names
//! something the toolchain found. Three paths broke it, each found by
//! driving the server by hand on 2026-08-23.

use axiom_proto::CtopStatus;
use axiom_vmm::{SandboxEngine, WasiEngine};
use std::process::{Command, Stdio};

fn rustc_is_installed() -> bool {
    Command::new("rustc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn eval(snippet: &str) -> axiom_proto::CtopReport {
    WasiEngine::new()
        .expect("engine")
        .execute_eval_in("anonymous", snippet, None)
        .await
        .expect("a report")
}

/// A WebAssembly module whose `run` export traps was reported as PASSED with
/// one passed check, because the call's result was discarded.
#[tokio::test]
async fn a_wat_module_that_traps_is_not_passed() {
    let report = eval("(module (func (export \"run\") unreachable))").await;

    assert_eq!(report.status, CtopStatus::Failed, "{report:?}");
    assert_eq!(report.passed_checks_count, 0);
    assert!(
        report
            .failed_checks
            .iter()
            .any(|c| c.error_type == "Trap/ExecutionError"),
        "the trap must be named: {:?}",
        report.failed_checks
    );
}

/// A module with no `run` export executes nothing, and nothing executed is
/// not a pass.
#[tokio::test]
async fn a_wat_module_without_a_run_export_is_not_passed() {
    let report = eval("(module (func (export \"other\")))").await;

    assert_eq!(report.status, CtopStatus::Failed, "{report:?}");
    assert_eq!(report.passed_checks_count, 0);
    assert!(
        report
            .failed_checks
            .iter()
            .any(|c| c.error_type == "SymbolNotFound"),
        "{:?}",
        report.failed_checks
    );
}

/// The positive case still holds, so the two above are not passing because
/// the path was removed.
#[tokio::test]
async fn a_wat_module_whose_run_returns_is_passed() {
    let report = eval("(module (func (export \"run\")))").await;
    assert_eq!(report.status, CtopStatus::Passed, "{report:?}");
    assert_eq!(report.engine, "tier1_wasi_cranelift");
}

/// `println!("???")` was reported as a compilation error with the message
/// "unexpected illegal token in stream" before rustc ever saw it: a leftover
/// substring check from a demo evaluator. The only thing allowed to say a
/// snippet does not compile is the compiler.
#[tokio::test]
async fn a_string_that_looks_like_a_syntax_error_is_compiled_not_pattern_matched() {
    let report = eval("println!(\"??? @@@ this is not valid\");").await;

    if !rustc_is_installed() {
        assert_eq!(
            report.status,
            CtopStatus::EvaluatorUnavailable,
            "{report:?}"
        );
        return;
    }
    assert_eq!(
        report.status,
        CtopStatus::Passed,
        "rustc accepts this snippet, so nothing else may reject it: {report:?}"
    );
}

/// The rustc path labelled itself `tier1_wasi_cranelift`, which names an
/// engine it does not use. The label is what a reader of a provenance record
/// sees under "Checked by", so it has to say what ran.
#[tokio::test]
async fn the_rustc_tier_names_the_engine_that_ran() {
    let report = eval("assert!(1 + 1 == 2);").await;
    if !rustc_is_installed() {
        return;
    }
    assert_eq!(report.status, CtopStatus::Passed, "{report:?}");
    assert_eq!(report.engine, "tier1_native_rustc");
    assert_eq!(report.passed_checks_count, 1);
    assert!(
        !report.passed_checks_basis.is_empty(),
        "a count has to say what it counts"
    );
}

#[tokio::test]
async fn a_rust_test_function_in_snippet_is_executed_and_can_fail() {
    if !rustc_is_installed() {
        return;
    }
    let report = eval("fn test_failure() { assert!(1 + 1 == 3); }").await;
    assert_eq!(report.status, CtopStatus::Failed, "{report:?}");
    assert_eq!(report.engine, "tier1_native_rustc");

    let report_pass = eval("fn test_success() { assert!(1 + 1 == 2); }").await;
    assert_eq!(report_pass.status, CtopStatus::Passed, "{report_pass:?}");
}
