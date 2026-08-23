use anyhow::Result;
use async_trait::async_trait;
use axiom_proto::{CtopReport, CtopStatus, FailedCheck};
use std::process::Command;
use std::time::Instant;
use wasmtime::*;
use wasmtime_wasi::WasiCtxBuilder;
// wasmtime-wasi renamed `preview1` to `p1` between 20 and 48. Same functions,
// same signatures; the alias keeps the call sites below reading as they did.
use wasmtime_wasi::p1::{self as preview1, WasiP1Ctx};

pub mod native;

/// Sandboxed execution backend trait
#[async_trait]
pub trait SandboxEngine: Send + Sync {
    async fn execute_wasi(&self, wasm_binary: &[u8], entrypoint: &str) -> Result<CtopReport>;

    /// Evaluate a snippet written in the language of the file `symbol_path`
    /// lives in, named by its extension.
    ///
    /// The extension is not a hint the engine may ignore. Handing a Java symbol
    /// to `rustc` produces a syntax error that reads as though the caller wrote
    /// bad code, when the real answer is that a different toolchain was needed.
    async fn execute_eval_in(
        &self,
        symbol_path: &str,
        code_snippet: &str,
        language: Option<&str>,
    ) -> Result<CtopReport>;

    /// Evaluate a snippet as Rust, for callers that already know it is Rust.
    async fn execute_eval(&self, symbol_path: &str, code_snippet: &str) -> Result<CtopReport> {
        self.execute_eval_in(symbol_path, code_snippet, None).await
    }
}

/// Tier 1: In-Process WASI Engine with real Cranelift JIT compiler & native sandbox executor
pub struct WasiEngine {
    engine: Engine,
}

impl WasiEngine {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        // config.async_support(false) used to sit here. wasmtime 48 deprecates it
        // as having no effect: whether a call is sync or async is decided by which
        // API is used, and this engine uses add_to_linker_sync and TypedFunc::call
        // throughout.
        config.cranelift_opt_level(OptLevel::Speed);
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;
        Ok(Self { engine })
    }

    /// Fast pre-compilation check
    pub fn compile(&self, wasm_bytes: &[u8]) -> Result<Module> {
        // wasmtime 48 returns its own Error type rather than re-exporting
        // anyhow's, so this no longer coerces on the way out.
        Module::from_binary(&self.engine, wasm_bytes).map_err(|e| anyhow::anyhow!(e))
    }
}

struct HostState {
    wasi: WasiP1Ctx,
}

/// The report for a WAT module's `run` export: `Ok` when it returned,
/// `Err(Some(trap))` when it trapped, `Err(None)` when there was no such
/// export and so nothing was executed.
fn wat_verdict(
    outcome: Result<(), Option<wasmtime::Error>>,
    task_id: String,
    symbol_path: &str,
    duration_ms: f64,
) -> CtopReport {
    let engine = "tier1_wasi_cranelift".to_string();
    match outcome {
        Ok(()) => {
            let mut report = CtopReport::pass(
                task_id,
                engine,
                duration_ms,
                1,
                "WebAssembly WAT module compiled and its `run` export returned".to_string(),
            );
            report.passed_checks_basis =
                "the exported `run` function returned without trapping".to_string();
            report
        }
        Err(Some(trap)) => CtopReport::fail(
            task_id,
            engine,
            duration_ms,
            vec![FailedCheck {
                symbol: symbol_path.to_string(),
                error_type: "Trap/ExecutionError".to_string(),
                expected: Some("`run` to return".to_string()),
                actual: Some(trap.to_string()),
                stack_trace_ast_nodes: vec![symbol_path.to_string()],
                hint: Some(
                    "The module trapped: check memory bounds, unreachable instructions and fuel"
                        .to_string(),
                ),
            }],
            String::new(),
            trap.to_string(),
        ),
        Err(None) => CtopReport::fail(
            task_id,
            engine,
            duration_ms,
            vec![FailedCheck {
                symbol: symbol_path.to_string(),
                error_type: "SymbolNotFound".to_string(),
                expected: Some(
                    "an exported function named `run` taking and returning nothing".to_string(),
                ),
                actual: Some("no such export, so nothing was executed".to_string()),
                stack_trace_ast_nodes: vec![],
                hint: Some("Export the entry point as `run`".to_string()),
            }],
            String::new(),
            "the module has no `run` export".to_string(),
        ),
    }
}

/// The rustc tier could not run anything: no compiler, or no writable work
/// directory. Never a verdict about the snippet.
fn unavailable_rustc(
    task_id: String,
    symbol_path: &str,
    duration_ms: f64,
    actual: String,
) -> CtopReport {
    CtopReport {
        task_id,
        engine: RUSTC_ENGINE.to_string(),
        status: CtopStatus::EvaluatorUnavailable,
        execution_duration_ms: duration_ms,
        blast_radius_nodes: 1,
        failed_checks: vec![FailedCheck {
            symbol: symbol_path.to_string(),
            error_type: "EvaluatorUnavailable".to_string(),
            expected: Some("rustc on PATH and a writable temp directory".to_string()),
            actual: Some(actual.clone()),
            stack_trace_ast_nodes: vec![symbol_path.to_string()],
            hint: Some("Install rustc and put it on PATH, or run the project's own tests and report the outcome with axiom_record_verification".to_string()),
        }],
        passed_checks_count: 0,
        passed_checks_basis: String::new(),
        stdout: String::new(),
        stderr: actual,
        memory_allocated_bytes: None,
    }
}

/// What the Rust path calls itself. It used to say `tier1_wasi_cranelift`,
/// naming an engine it does not use; a reader of a provenance record sees
/// this under "Checked by", so it has to say what ran.
const RUSTC_ENGINE: &str = "tier1_native_rustc";

#[async_trait]
impl SandboxEngine for WasiEngine {
    async fn execute_wasi(&self, wasm_binary: &[u8], entrypoint: &str) -> Result<CtopReport> {
        let start = Instant::now();
        let task_id = format!("task_wasi_{:x}", start.elapsed().as_nanos());

        let module = match Module::from_binary(&self.engine, wasm_binary) {
            Ok(m) => m,
            Err(e) => {
                let dur = start.elapsed().as_secs_f64() * 1000.0;
                return Ok(CtopReport {
                    task_id,
                    engine: "tier1_wasi_cranelift".to_string(),
                    status: CtopStatus::CompilationError,
                    execution_duration_ms: dur,
                    blast_radius_nodes: 1,
                    failed_checks: vec![FailedCheck {
                        symbol: entrypoint.to_string(),
                        error_type: "WasmCompileError".to_string(),
                        expected: Some("Valid WASM bytecode".to_string()),
                        actual: Some(e.to_string()),
                        stack_trace_ast_nodes: vec![],
                        hint: Some("Verify WASM bytecode structure and magic header".to_string()),
                    }],
                    passed_checks_count: 0,
                    passed_checks_basis: String::new(),
                    stdout: String::new(),
                    stderr: e.to_string(),
                    memory_allocated_bytes: None,
                });
            }
        };

        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        preview1::add_to_linker_sync(&mut linker, |s| &mut s.wasi)?;

        let wasi = WasiCtxBuilder::new()
            .inherit_stdout()
            .inherit_stderr()
            .build_p1();

        let mut store = Store::new(&self.engine, HostState { wasi });
        store.set_fuel(10_000_000)?;

        let instance = match linker.instantiate(&mut store, &module) {
            Ok(inst) => inst,
            Err(e) => {
                let dur = start.elapsed().as_secs_f64() * 1000.0;
                return Ok(CtopReport::fail(
                    task_id,
                    "tier1_wasi_cranelift".to_string(),
                    dur,
                    vec![FailedCheck {
                        symbol: entrypoint.to_string(),
                        error_type: "InstantiationError".to_string(),
                        expected: Some("Clean Instance".to_string()),
                        actual: Some(e.to_string()),
                        stack_trace_ast_nodes: vec![],
                        hint: Some("Check WASI import resolution".to_string()),
                    }],
                    String::new(),
                    e.to_string(),
                ));
            }
        };

        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

        // Written as a match rather than `if let ... else` because edition 2024
        // drops the scrutinee's temporaries before the else block runs. Nothing
        // here depends on that, but the explicit form has one reading under both
        // editions rather than two.
        match instance.get_typed_func::<(), ()>(&mut store, entrypoint) {
            Ok(func) => match func.call(&mut store, ()) {
                Ok(_) => {
                    let total_ms = start.elapsed().as_secs_f64() * 1000.0;
                    Ok(CtopReport::pass(
                        task_id,
                        "tier1_wasi_cranelift".to_string(),
                        total_ms,
                        1,
                        format!("WASI function '{}' executed successfully", entrypoint),
                    ))
                }
                Err(e) => {
                    let total_ms = start.elapsed().as_secs_f64() * 1000.0;
                    Ok(CtopReport::fail(
                        task_id,
                        "tier1_wasi_cranelift".to_string(),
                        total_ms,
                        vec![FailedCheck {
                            symbol: entrypoint.to_string(),
                            error_type: "Trap/ExecutionError".to_string(),
                            expected: Some("Clean Return".to_string()),
                            actual: Some(e.to_string()),
                            stack_trace_ast_nodes: vec![entrypoint.to_string()],
                            hint: Some("Check memory bounds or fuel limits".to_string()),
                        }],
                        String::new(),
                        e.to_string(),
                    ))
                }
            },
            Err(_) => Ok(CtopReport::fail(
                task_id,
                "tier1_wasi_cranelift".to_string(),
                duration_ms,
                vec![FailedCheck {
                    symbol: entrypoint.to_string(),
                    error_type: "SymbolNotFound".to_string(),
                    expected: Some(format!("Exported function '{}'", entrypoint)),
                    actual: Some("Missing Export".to_string()),
                    stack_trace_ast_nodes: vec![],
                    hint: Some(
                        "Ensure function is marked #[no_mangle] or exported in WASM module"
                            .to_string(),
                    ),
                }],
                String::new(),
                format!("Symbol '{}' not found in WASI module", entrypoint),
            )),
        }
    }

    async fn execute_eval_in(
        &self,
        symbol_path: &str,
        code_snippet: &str,
        language: Option<&str>,
    ) -> Result<CtopReport> {
        let start = Instant::now();
        let task_id = format!("eval_{:x}", start.elapsed().as_nanos());

        // 1. If WAT format is provided, compile directly with Wasmtime Cranelift JIT
        let trimmed = code_snippet.trim();
        if trimmed.starts_with("(module") || trimmed.starts_with("(func") {
            match Module::new(&self.engine, code_snippet) {
                Ok(module) => {
                    let mut linker: Linker<HostState> = Linker::new(&self.engine);
                    let _ = preview1::add_to_linker_sync(&mut linker, |s| &mut s.wasi);
                    let wasi = WasiCtxBuilder::new()
                        .inherit_stdout()
                        .inherit_stderr()
                        .build_p1();
                    let mut store = Store::new(&self.engine, HostState { wasi });
                    let _ = store.set_fuel(10_000_000);

                    match linker.instantiate(&mut store, &module) {
                        Ok(instance) => {
                            // The call's result used to be discarded, so a
                            // `run` that trapped, and a module with no `run`
                            // at all, both came back PASSED with one passed
                            // check. Nothing that did not return cleanly is a
                            // pass.
                            let outcome = instance
                                .get_typed_func::<(), ()>(&mut store, "run")
                                .map_err(|_| None)
                                .and_then(|func| func.call(&mut store, ()).map_err(Some));
                            let dur = start.elapsed().as_secs_f64() * 1000.0;
                            return Ok(wat_verdict(outcome, task_id, symbol_path, dur));
                        }
                        Err(e) => {
                            let dur = start.elapsed().as_secs_f64() * 1000.0;
                            return Ok(CtopReport::fail(
                                task_id,
                                "tier1_wasi_cranelift".to_string(),
                                dur,
                                vec![FailedCheck {
                                    symbol: symbol_path.to_string(),
                                    error_type: "InstantiationError".to_string(),
                                    expected: Some("Clean Instance".to_string()),
                                    actual: Some(e.to_string()),
                                    stack_trace_ast_nodes: vec![],
                                    hint: Some("Check WASI import resolution".to_string()),
                                }],
                                String::new(),
                                e.to_string(),
                            ));
                        }
                    }
                }
                Err(e) => {
                    let dur = start.elapsed().as_secs_f64() * 1000.0;
                    return Ok(CtopReport {
                        task_id,
                        engine: "tier1_wasi_cranelift".to_string(),
                        status: CtopStatus::CompilationError,
                        execution_duration_ms: dur,
                        blast_radius_nodes: 1,
                        failed_checks: vec![FailedCheck {
                            symbol: symbol_path.to_string(),
                            error_type: "WatCompileError".to_string(),
                            expected: Some("Valid WebAssembly syntax".to_string()),
                            actual: Some(e.to_string()),
                            stack_trace_ast_nodes: vec![],
                            hint: Some("Fix WebAssembly text format syntax".to_string()),
                        }],
                        passed_checks_count: 0,
                        passed_checks_basis: String::new(),
                        stdout: String::new(),
                        stderr: e.to_string(),
                        memory_allocated_bytes: None,
                    });
                }
            }
        }

        // 2. Anything that is not Rust goes to its own toolchain.
        //
        // WAT is checked first above, because a WebAssembly module is
        // recognisable from its own text and does not belong to the file the
        // symbol was indexed from.
        if let Some(ext) = language {
            let ext = ext.to_ascii_lowercase();
            if ext != "rs" {
                return Ok(match native::language_for(&ext) {
                    Some(lang) => native::evaluate(
                        lang,
                        symbol_path,
                        code_snippet,
                        native::configured_timeout(),
                    ),
                    // Kotlin and Scala reach here: the indexer reads them with
                    // the Java parser, which does not make javac able to run
                    // them.
                    None => CtopReport {
                        task_id,
                        engine: "tier2_native".to_string(),
                        status: CtopStatus::EvaluatorUnavailable,
                        execution_duration_ms: start.elapsed().as_secs_f64() * 1000.0,
                        blast_radius_nodes: 1,
                        failed_checks: vec![FailedCheck {
                            symbol: symbol_path.to_string(),
                            error_type: "UnsupportedLanguage".to_string(),
                            expected: Some("a language axiom can evaluate".to_string()),
                            actual: Some(format!(
                                "{symbol_path:?} is defined in a .{ext} file, and axiom has no evaluator for it"
                            )),
                            stack_trace_ast_nodes: vec![symbol_path.to_string()],
                            hint: Some(
                                "Run this symbol's own test suite instead and report the outcome                                  with axiom_record_verification; axiom_get_blast_radius will name                                  the tests to run."
                                    .to_string(),
                            ),
                        }],
                        passed_checks_count: 0,
                        passed_checks_basis: String::new(),
                        stdout: String::new(),
                        stderr: format!("no evaluator for .{ext}"),
                        memory_allocated_bytes: None,
                    },
                });
            }
        }

        // 3. Rust, compiled by rustc and run as a process of its own.
        //
        // A substring check used to sit here and return CompilationError for
        // any snippet containing `???`, `@@@`, "invalid syntax" or "this is
        // not valid", before rustc saw it. `println!("???")` compiles, and the
        // only thing allowed to say otherwise is the compiler.
        //
        // Not a sandbox, whatever the old engine label said: this is the same
        // arrangement as tier 2, a temp directory and the real toolchain with
        // this process's privileges, and the label now says so. The work
        // directory is removed when `work` drops, on every path out.
        let work = match native::temp_work_dir("rs") {
            Ok(d) => d,
            Err(e) => {
                return Ok(unavailable_rustc(
                    task_id,
                    symbol_path,
                    start.elapsed().as_secs_f64() * 1000.0,
                    format!("could not create a work directory: {e}"),
                ));
            }
        };
        let temp_dir = work.path().to_path_buf();
        let src_file = temp_dir.join("eval_main.rs");
        let bin_file = temp_dir.join(if cfg!(windows) {
            "eval_main.exe"
        } else {
            "eval_main"
        });

        // Format code into runnable harness
        let source_code = if code_snippet.contains("fn main") {
            code_snippet.to_string()
        } else {
            format!(
                r#"
#![allow(unused_variables, unused_mut, dead_code, unused_imports)]
fn validate_token(token: &str) -> bool {{
    token.len() > 10
}}

fn main() {{
    {}
}}
"#,
                code_snippet
            )
        };

        if let Err(e) = std::fs::write(&src_file, &source_code) {
            return Ok(unavailable_rustc(
                task_id,
                symbol_path,
                start.elapsed().as_secs_f64() * 1000.0,
                format!("could not write the snippet to {}: {e}", src_file.display()),
            ));
        }

        // Compile. run_with_timeout confines the environment and ends the
        // whole process tree on timeout, for rustc and for the binary alike.
        let timeout = native::configured_timeout();
        let mut rustc = Command::new("rustc");
        rustc
            .arg(&src_file)
            .arg("-o")
            .arg(&bin_file)
            .arg("--crate-type")
            .arg("bin");
        let c_out = match native::run_with_timeout(rustc, timeout) {
            Ok(c) => c,
            Err(e) => {
                return Ok(unavailable_rustc(
                    task_id,
                    symbol_path,
                    start.elapsed().as_secs_f64() * 1000.0,
                    format!("could not run rustc: {e}"),
                ));
            }
        };
        if c_out.timed_out {
            let dur = start.elapsed().as_secs_f64() * 1000.0;
            return Ok(rustc_timeout(
                task_id,
                symbol_path,
                dur,
                timeout,
                "rustc",
                c_out,
            ));
        }
        if !c_out.succeeded() {
            let stderr = c_out.stderr.clone();
            let dur = start.elapsed().as_secs_f64() * 1000.0;
            return Ok(CtopReport {
                task_id,
                engine: RUSTC_ENGINE.to_string(),
                status: CtopStatus::CompilationError,
                execution_duration_ms: dur,
                blast_radius_nodes: 1,
                failed_checks: vec![FailedCheck {
                    symbol: symbol_path.to_string(),
                    error_type: "RustcCompilationError".to_string(),
                    expected: Some("Clean compilation".to_string()),
                    actual: Some(stderr.clone()),
                    stack_trace_ast_nodes: vec![symbol_path.to_string()],
                    hint: Some("Fix syntax or type error reported by compiler".to_string()),
                }],
                passed_checks_count: 0,
                passed_checks_basis: String::new(),
                stdout: c_out.stdout,
                stderr,
                memory_allocated_bytes: None,
            });
        }

        // Run the binary as a process of its own.
        let r_out = match native::run_with_timeout(Command::new(&bin_file), timeout) {
            Ok(r) => r,
            Err(e) => {
                return Ok(unavailable_rustc(
                    task_id,
                    symbol_path,
                    start.elapsed().as_secs_f64() * 1000.0,
                    format!("could not run the compiled snippet: {e}"),
                ));
            }
        };
        let dur = start.elapsed().as_secs_f64() * 1000.0;
        drop(work);

        if r_out.timed_out {
            return Ok(rustc_timeout(
                task_id,
                symbol_path,
                dur,
                timeout,
                "the compiled snippet",
                r_out,
            ));
        }
        if r_out.succeeded() {
            let mut report = CtopReport::pass(
                task_id,
                RUSTC_ENGINE.to_string(),
                dur,
                code_snippet.matches("assert").count(),
                String::new(),
            );
            report.stdout = r_out.stdout;
            report.stderr = r_out.stderr;
            return Ok(report);
        }
        let stderr = r_out.stderr.clone();
        Ok(CtopReport::fail(
            task_id,
            RUSTC_ENGINE.to_string(),
            dur,
            vec![FailedCheck {
                symbol: symbol_path.to_string(),
                error_type: "Panic/AssertionFailure".to_string(),
                expected: Some("the snippet to run to completion".to_string()),
                actual: Some(if stderr.is_empty() {
                    "Process exited with non-zero status".to_string()
                } else {
                    stderr.clone()
                }),
                stack_trace_ast_nodes: vec![symbol_path.to_string()],
                hint: Some("The snippet ran under rustc's output and failed; the output above is the program's own".to_string()),
            }],
            r_out.stdout,
            stderr,
        ))
    }
}

/// A rustc-tier command ran past the deadline. `what` names it.
fn rustc_timeout(
    task_id: String,
    symbol_path: &str,
    duration_ms: f64,
    timeout: std::time::Duration,
    what: &str,
    done: native::Finished,
) -> CtopReport {
    CtopReport {
        task_id,
        engine: RUSTC_ENGINE.to_string(),
        status: CtopStatus::Timeout,
        execution_duration_ms: duration_ms,
        blast_radius_nodes: 1,
        failed_checks: vec![FailedCheck {
            symbol: symbol_path.to_string(),
            error_type: "EvaluationTimeout".to_string(),
            expected: Some(format!("{what} to finish within {}s", timeout.as_secs())),
            actual: Some(format!(
                "{what} was still running after {}s and was killed{}",
                timeout.as_secs(),
                if done.drained {
                    ""
                } else {
                    "; something it started was still holding its output open, so the output below may be incomplete"
                }
            )),
            stack_trace_ast_nodes: vec![symbol_path.to_string()],
            hint: Some(
                "The snippet did not terminate, so nothing is known about whether it would have passed. Raise AXIOM_EVAL_TIMEOUT_SECS if the work is genuinely slow."
                    .to_string(),
            ),
        }],
        passed_checks_count: 0,
        passed_checks_basis: String::new(),
        stdout: done.stdout,
        stderr: done.stderr,
        memory_allocated_bytes: None,
    }
}
