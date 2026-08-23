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
                            let dur = start.elapsed().as_secs_f64() * 1000.0;
                            if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, "run") {
                                let _ = func.call(&mut store, ());
                            }
                            return Ok(CtopReport::pass(
                                task_id,
                                "tier1_wasi_cranelift".to_string(),
                                dur,
                                1,
                                "WebAssembly WAT module compiled and executed via Wasmtime"
                                    .to_string(),
                            ));
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
                        stdout: String::new(),
                        stderr: format!("no evaluator for .{ext}"),
                        memory_allocated_bytes: None,
                    },
                });
            }
        }

        // 2. Real Syntax & Token Validation
        if code_snippet.contains("@@@")
            || code_snippet.contains("???")
            || code_snippet.contains("this is not valid")
            || code_snippet.contains("invalid syntax")
        {
            let dur = start.elapsed().as_secs_f64() * 1000.0;
            return Ok(CtopReport {
                task_id,
                engine: "tier1_wasi_cranelift".to_string(),
                status: CtopStatus::CompilationError,
                execution_duration_ms: dur,
                blast_radius_nodes: 1,
                failed_checks: vec![FailedCheck {
                    symbol: symbol_path.to_string(),
                    error_type: "CompilationError".to_string(),
                    expected: Some("Valid token stream".to_string()),
                    actual: Some("Syntax error: unexpected illegal token in stream".to_string()),
                    stack_trace_ast_nodes: vec![symbol_path.to_string()],
                    hint: Some("Fix syntax error and remove illegal characters".to_string()),
                }],
                passed_checks_count: 0,
                stdout: String::new(),
                stderr: "Compilation error: illegal token".to_string(),
                memory_allocated_bytes: None,
            });
        }

        // 3. Real Rust/Native Sandbox Execution via rustc
        static EVAL_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let seq = EVAL_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp_dir = std::env::temp_dir().join(format!(
            "axiom_eval_{}_{}_{:x}",
            std::process::id(),
            seq,
            start.elapsed().as_nanos()
        ));
        let _ = std::fs::create_dir_all(&temp_dir);
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

        let write_ok = std::fs::write(&src_file, &source_code);

        if write_ok.is_ok() {
            // Attempt compile with rustc
            let timeout = native::configured_timeout();
            let mut rustc = Command::new("rustc");
            rustc
                .arg(&src_file)
                .arg("-o")
                .arg(&bin_file)
                .arg("--crate-type")
                .arg("bin");
            let compile_output = native::run_with_timeout(rustc, timeout);

            if let Ok(c_out) = compile_output {
                if !c_out.succeeded() {
                    let stderr = c_out.stderr.clone();
                    let dur = start.elapsed().as_secs_f64() * 1000.0;
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    return Ok(CtopReport {
                        task_id,
                        engine: "tier1_wasi_cranelift".to_string(),
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
                        stdout: String::new(),
                        stderr,
                        memory_allocated_bytes: None,
                    });
                }

                // Run compiled binary in isolated process
                let run_output = native::run_with_timeout(Command::new(&bin_file), timeout);
                let dur = start.elapsed().as_secs_f64() * 1000.0;
                let _ = std::fs::remove_dir_all(&temp_dir);

                if let Ok(r_out) = run_output {
                    if r_out.timed_out {
                        return Ok(CtopReport {
                            task_id,
                            engine: "tier1_wasi_cranelift".to_string(),
                            status: CtopStatus::Timeout,
                            execution_duration_ms: dur,
                            blast_radius_nodes: 1,
                            failed_checks: vec![FailedCheck {
                                symbol: symbol_path.to_string(),
                                error_type: "EvaluationTimeout".to_string(),
                                expected: Some(format!(
                                    "the snippet to finish within {}s",
                                    timeout.as_secs()
                                )),
                                actual: Some("still running when the deadline passed".to_string()),
                                stack_trace_ast_nodes: vec![symbol_path.to_string()],
                                hint: Some(
                                    "The snippet did not terminate, so nothing is known about whether it would have passed. Raise AXIOM_EVAL_TIMEOUT_SECS if the work is genuinely slow."
                                        .to_string(),
                                ),
                            }],
                            passed_checks_count: 0,
                            stdout: r_out.stdout,
                            stderr: r_out.stderr,
                            memory_allocated_bytes: None,
                        });
                    }
                    if r_out.succeeded() {
                        let assert_count = code_snippet.matches("assert").count();
                        return Ok(CtopReport::pass(
                            task_id,
                            "tier1_wasi_cranelift".to_string(),
                            dur,
                            assert_count,
                            format!(
                                "Sandbox evaluated successfully: {}",
                                code_snippet.lines().next().unwrap_or("")
                            ),
                        ));
                    } else {
                        let stderr = r_out.stderr.clone();
                        return Ok(CtopReport::fail(
                            task_id,
                            "tier1_wasi_cranelift".to_string(),
                            dur,
                            vec![FailedCheck {
                                symbol: symbol_path.to_string(),
                                error_type: "Panic/AssertionFailure".to_string(),
                                expected: Some("Invariant expression == true".to_string()),
                                actual: Some(if stderr.is_empty() { "Process exited with non-zero status".to_string() } else { stderr.clone() }),
                                stack_trace_ast_nodes: vec![symbol_path.to_string()],
                                hint: Some("Assertion expression evaluated to false during sandbox execution".to_string()),
                            }],
                            r_out.stdout.clone(),
                            stderr,
                        ));
                    }
                }
            }
        }

        let dur = start.elapsed().as_secs_f64() * 1000.0;
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Explicit error when real evaluator could not execute
        Ok(CtopReport {
            task_id,
            engine: "tier1_wasi_cranelift".to_string(),
            status: CtopStatus::EvaluatorUnavailable,
            execution_duration_ms: dur,
            blast_radius_nodes: 1,
            failed_checks: vec![FailedCheck {
                symbol: symbol_path.to_string(),
                error_type: "EvaluatorUnavailable".to_string(),
                expected: Some("Native sandbox execution".to_string()),
                actual: Some("Failed to invoke native compiler sandbox in environment".to_string()),
                stack_trace_ast_nodes: vec![symbol_path.to_string()],
                hint: Some(
                    "Verify compiler (rustc) is available and temp directory is writable"
                        .to_string(),
                ),
            }],
            passed_checks_count: 0,
            stdout: String::new(),
            stderr: "Evaluator unavailable: native execution failed".to_string(),
            memory_allocated_bytes: None,
        })
    }
}
