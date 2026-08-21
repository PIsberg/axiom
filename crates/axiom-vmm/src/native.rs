//! Tier 2: evaluating a snippet with the language's own toolchain.
//!
//! Tier 1 compiles Rust. The indexer reads Java, Kotlin, Scala, Python,
//! TypeScript, JavaScript and Go as well, so on most codebases the sandbox step
//! of the agent loop had nothing to run and returned `UnsupportedLanguage`.
//!
//! What this tier does not claim: it is not a sandbox. It writes the snippet to
//! a temp directory and invokes the real compiler or interpreter with the
//! privileges the axiom process already has, exactly as the `rustc` tier has
//! always done. That is why the engine name says `native` rather than `wasi`,
//! and why `AXIOM_EVAL_NATIVE=off` turns the tier off for anyone who does not
//! want agent-authored code run on the host.
//!
//! Two rules keep a wrong verdict off the wire. A toolchain that is not usable
//! produces `EVALUATOR_UNAVAILABLE` naming the programs that were looked for,
//! never `PASSED`. And a command that outlives `AXIOM_EVAL_TIMEOUT_SECS` is
//! killed and reported as a timeout, because one runaway loop would otherwise
//! hang the stdio pipe an agent is waiting on.

use axiom_proto::{CtopReport, CtopStatus, FailedCheck};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a single compile or run may take before it is killed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A program and its arguments, built once the paths are known.
type CommandSpec = (String, Vec<String>);

/// Turns the source path and the work directory into a command to run.
type Step = fn(&Path, &Path) -> CommandSpec;

/// One way to evaluate a snippet: an optional build step, then a run step.
struct Recipe {
    /// The program that has to work for this recipe to be usable.
    probe: &'static str,
    /// Arguments that make `probe` do nothing and exit zero.
    ///
    /// This is deliberately a real invocation rather than a PATH lookup.
    /// Windows ships a `python3` shim that resolves, exits zero for
    /// `--version`, and then refuses to run a file: probing with `-c pass`
    /// exposes it (exit 49) instead of letting it produce a failed verdict for
    /// code that never ran.
    probe_args: &'static [&'static str],
    file_name: &'static str,
    /// Built from the source path and the work directory, once both are known.
    build: Option<Step>,
    run: Step,
}

/// A language this tier can drive, and how to shape a bare snippet into
/// something its toolchain will accept.
pub struct NativeLanguage {
    /// The extension as `language_of_symbol` reports it.
    pub extension: &'static str,
    /// Goes into `CtopReport::engine`, so a reader can tell which tier answered.
    pub engine: &'static str,
    /// Substrings counted towards `passed_checks_count` for this language.
    assertion_tokens: &'static [&'static str],
    /// Whether the snippet already brings everything the toolchain needs:
    /// an entry point where the language requires one, and the assertion
    /// helper where one would otherwise be injected.
    is_self_contained: fn(&str) -> bool,
    /// Supplies whatever the snippet is missing.
    wrap: fn(&str) -> String,
    recipes: &'static [Recipe],
}

fn javascript_wrap(snippet: &str) -> String {
    // Prepended rather than wrapped in a function: `assert` is a module in
    // Node rather than a keyword, and a snippet that never gets it fails
    // with ReferenceError, which reads as a failed check.
    format!("const assert = require('node:assert');\n{snippet}\n")
}

fn go_wrap(snippet: &str) -> String {
    format!("package main\n\nfunc main() {{\n{snippet}\n}}\n")
}

fn java_wrap(snippet: &str) -> String {
    format!(
        "public class AxiomEval {{\n    public static void main(String[] args) throws Exception {{\n{snippet}\n    }}\n}}\n"
    )
}

static PYTHON: NativeLanguage = NativeLanguage {
    extension: "py",
    engine: "tier2_native_python",
    assertion_tokens: &["assert"],
    // Python needs no entry point and no injected helper: `assert` is a
    // statement, so the snippet runs exactly as written.
    is_self_contained: |_| true,
    wrap: |s| s.to_string(),
    recipes: &[
        Recipe {
            probe: "python3",
            probe_args: &["-c", "pass"],
            file_name: "axiom_eval.py",
            build: None,
            run: |src, _| ("python3".to_string(), vec![src.display().to_string()]),
        },
        Recipe {
            probe: "python",
            probe_args: &["-c", "pass"],
            file_name: "axiom_eval.py",
            build: None,
            run: |src, _| ("python".to_string(), vec![src.display().to_string()]),
        },
    ],
};

static JAVASCRIPT: NativeLanguage = NativeLanguage {
    extension: "js",
    engine: "tier2_native_node",
    assertion_tokens: &["assert", "expect("],
    // Requiring `node:assert` twice is a redeclaration error, so the
    // prelude is skipped when the snippet already asked for it.
    is_self_contained: |s| s.contains("node:assert"),
    wrap: javascript_wrap,
    recipes: &[Recipe {
        probe: "node",
        probe_args: &["-e", ""],
        file_name: "axiom_eval.js",
        build: None,
        run: |src, _| ("node".to_string(), vec![src.display().to_string()]),
    }],
};

static TYPESCRIPT: NativeLanguage = NativeLanguage {
    extension: "ts",
    engine: "tier2_native_typescript",
    assertion_tokens: &["assert", "expect("],
    // Nothing is injected: deno and tsc-then-node disagree about how a
    // module is reached, and a prelude that works under one breaks under
    // the other. A TypeScript snippet brings its own assertions.
    is_self_contained: |_| true,
    wrap: |s| s.to_string(),
    recipes: &[
        // Deno runs TypeScript directly and grants no permissions unless asked,
        // so a snippet reaching for the network or the filesystem fails rather
        // than succeeding quietly.
        Recipe {
            probe: "deno",
            probe_args: &["--version"],
            file_name: "axiom_eval.ts",
            build: None,
            run: |src, _| {
                (
                    "deno".to_string(),
                    vec![
                        "run".to_string(),
                        "--quiet".to_string(),
                        "--no-prompt".to_string(),
                        src.display().to_string(),
                    ],
                )
            },
        },
        // tsc emits the .js beside the source; node then runs that.
        //
        // Recent Node strips types on its own, which would make a third recipe
        // possible, but an older Node given TypeScript reports a syntax error,
        // and that would be filed against the snippet rather than against the
        // toolchain. Refusing is the better answer.
        Recipe {
            probe: "tsc",
            probe_args: &["--version"],
            file_name: "axiom_eval.ts",
            build: Some(|src, _| {
                (
                    "tsc".to_string(),
                    vec![
                        "--target".to_string(),
                        "es2020".to_string(),
                        "--module".to_string(),
                        "commonjs".to_string(),
                        src.display().to_string(),
                    ],
                )
            }),
            run: |src, _| {
                (
                    "node".to_string(),
                    vec![src.with_extension("js").display().to_string()],
                )
            },
        },
    ],
};

static GO: NativeLanguage = NativeLanguage {
    extension: "go",
    engine: "tier2_native_go",
    assertion_tokens: &["panic(", "t.Error", "t.Fatal"],
    is_self_contained: |s| s.contains("func main("),
    wrap: go_wrap,
    recipes: &[Recipe {
        probe: "go",
        // `go --version` is not a thing; the subcommand is `go version`.
        probe_args: &["version"],
        file_name: "axiom_eval.go",
        build: None,
        run: |src, _| {
            (
                "go".to_string(),
                vec!["run".to_string(), src.display().to_string()],
            )
        },
    }],
};

static JAVA: NativeLanguage = NativeLanguage {
    extension: "java",
    engine: "tier2_native_java",
    assertion_tokens: &["assert", "Assert.", "assertThat"],
    is_self_contained: |s| s.contains("static void main("),
    wrap: java_wrap,
    recipes: &[Recipe {
        probe: "javac",
        probe_args: &["-version"],
        file_name: "AxiomEval.java",
        build: Some(|src, dir| {
            (
                "javac".to_string(),
                vec![
                    "-d".to_string(),
                    dir.display().to_string(),
                    src.display().to_string(),
                ],
            )
        }),
        // `assert` is a no-op in Java unless assertions are enabled at run time,
        // which would turn a false assertion into a pass. -ea is not optional.
        run: |_, dir| {
            (
                "java".to_string(),
                vec![
                    "-ea".to_string(),
                    "-cp".to_string(),
                    dir.display().to_string(),
                    "AxiomEval".to_string(),
                ],
            )
        },
    }],
};

static LANGUAGES: &[&NativeLanguage] = &[&PYTHON, &JAVASCRIPT, &TYPESCRIPT, &GO, &JAVA];

/// The driver for a file extension, if this tier has one.
///
/// `mjs`, `cjs` and `jsx` are Node's; `tsx` is TypeScript's. Kotlin and Scala
/// are indexed by the Java parser but have no recipe here, so they stay an
/// honest `UnsupportedLanguage` instead of being handed to `javac`.
pub fn language_for(extension: &str) -> Option<&'static NativeLanguage> {
    let normalised = match extension {
        "mjs" | "cjs" | "jsx" => "js",
        "tsx" => "ts",
        other => other,
    };
    LANGUAGES
        .iter()
        .copied()
        .find(|l| l.extension == normalised)
}

/// Whether this tier is switched on. Anything other than `off`, `0` or `false`
/// leaves it on, so a typo does not silently disable evaluation.
pub fn native_eval_enabled() -> bool {
    match std::env::var("AXIOM_EVAL_NATIVE") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false"
        ),
        Err(_) => true,
    }
}

/// The configured timeout, or [`DEFAULT_TIMEOUT`] when unset or unparseable.
pub fn configured_timeout() -> Duration {
    std::env::var("AXIOM_EVAL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT)
}

/// What a child process did, once it either finished or was killed.
pub struct Finished {
    pub status: Option<std::process::ExitStatus>,
    pub stdout: String,
    pub stderr: String,
    /// True when the process was killed for running past the deadline. A killed
    /// process has no meaningful exit status, so this must be checked first.
    pub timed_out: bool,
}

impl Finished {
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.status.map(|s| s.success()).unwrap_or(false)
    }
}

/// Run a command, killing it if it outlives `timeout`.
///
/// `Command::output` waits forever, which is fine for a compiler and not fine
/// for agent-authored code: one runaway loop and the server stops answering.
pub fn run_with_timeout(mut command: Command, timeout: Duration) -> std::io::Result<Finished> {
    use std::io::Read;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();

    // Each pipe is drained on its own thread. Reading them after the wait
    // deadlocks as soon as a snippet writes more than one pipe buffer holds.
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };

    let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).to_string();
    let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).to_string();

    Ok(Finished {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

/// Can `probe` actually run something?
///
/// The answer is cached for the life of the process. Probing costs a process
/// spawn against an eval budget measured in tens of milliseconds, and a
/// toolchain does not usually appear halfway through a session. Restart the
/// server after installing one.
fn probe_usable(probe: &str, args: &[&str]) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Some(known) = cache.lock().unwrap().get(probe) {
        return *known;
    }

    let usable = Command::new(probe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    cache.lock().unwrap().insert(probe.to_string(), usable);
    usable
}

fn pick_recipe(language: &NativeLanguage) -> Option<&'static Recipe> {
    // `recipes` is a `&'static [Recipe]` on a `&'static NativeLanguage`, but the
    // signature above takes a plain reference, so the lifetime is recovered by
    // going through the table rather than through the argument.
    LANGUAGES
        .iter()
        .find(|l| l.extension == language.extension)?
        .recipes
        .iter()
        .find(|r| probe_usable(r.probe, r.probe_args))
}

/// The program this tier would use for `language`, or `None` when nothing on
/// PATH can run it.
///
/// Exposed so a caller, and a test, can tell "the toolchain is missing" from
/// "the snippet failed" without inferring it from an error string.
pub fn usable_toolchain(language: &NativeLanguage) -> Option<&'static str> {
    pick_recipe(language).map(|r| r.probe)
}

fn next_task_id(extension: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!(
        "task_native_{extension}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn unavailable(
    task_id: String,
    engine: &str,
    symbol_path: &str,
    duration_ms: f64,
    actual: String,
    hint: String,
) -> CtopReport {
    CtopReport {
        task_id,
        engine: engine.to_string(),
        status: CtopStatus::EvaluatorUnavailable,
        execution_duration_ms: duration_ms,
        blast_radius_nodes: 1,
        failed_checks: vec![FailedCheck {
            symbol: symbol_path.to_string(),
            error_type: "EvaluatorUnavailable".to_string(),
            expected: Some("a toolchain that can run this language".to_string()),
            actual: Some(actual.clone()),
            stack_trace_ast_nodes: vec![symbol_path.to_string()],
            hint: Some(hint),
        }],
        passed_checks_count: 0,
        stdout: String::new(),
        stderr: actual,
        memory_allocated_bytes: None,
    }
}

fn timed_out_report(
    task_id: String,
    engine: &str,
    symbol_path: &str,
    duration_ms: f64,
    program: &str,
    timeout: Duration,
) -> CtopReport {
    CtopReport {
        task_id,
        engine: engine.to_string(),
        status: CtopStatus::Timeout,
        execution_duration_ms: duration_ms,
        blast_radius_nodes: 1,
        failed_checks: vec![FailedCheck {
            symbol: symbol_path.to_string(),
            error_type: "EvaluationTimeout".to_string(),
            expected: Some(format!("{program} to finish within {}s", timeout.as_secs())),
            actual: Some(format!(
                "{program} was still running after {}s and was killed",
                timeout.as_secs()
            )),
            stack_trace_ast_nodes: vec![symbol_path.to_string()],
            hint: Some(
                "The snippet did not terminate, so nothing is known about whether it would have \
                 passed. Raise AXIOM_EVAL_TIMEOUT_SECS if the work is genuinely slow."
                    .to_string(),
            ),
        }],
        passed_checks_count: 0,
        stdout: String::new(),
        stderr: format!("timed out after {}s", timeout.as_secs()),
        memory_allocated_bytes: None,
    }
}

fn compilation_error(
    task_id: String,
    engine: &str,
    symbol_path: &str,
    duration_ms: f64,
    program: &str,
    done: Finished,
) -> CtopReport {
    let detail = if done.stderr.trim().is_empty() {
        done.stdout.clone()
    } else {
        done.stderr.clone()
    };
    CtopReport {
        task_id,
        engine: engine.to_string(),
        status: CtopStatus::CompilationError,
        execution_duration_ms: duration_ms,
        blast_radius_nodes: 1,
        failed_checks: vec![FailedCheck {
            symbol: symbol_path.to_string(),
            error_type: "CompilationError".to_string(),
            expected: Some("Clean compilation".to_string()),
            actual: Some(detail.clone()),
            stack_trace_ast_nodes: vec![symbol_path.to_string()],
            hint: Some(format!("Fix the error {program} reported.")),
        }],
        passed_checks_count: 0,
        stdout: done.stdout,
        stderr: detail,
        memory_allocated_bytes: None,
    }
}

fn temp_work_dir(tag: &str) -> std::io::Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("axiom_eval_{tag}_{}_{seq}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Evaluate `snippet` as `language`, allowing `timeout` for each command.
///
/// A report comes back in every case. The one thing that never comes back is
/// `PASSED` for something that did not compile and run.
pub fn evaluate(
    language: &NativeLanguage,
    symbol_path: &str,
    snippet: &str,
    timeout: Duration,
) -> CtopReport {
    let start = Instant::now();
    let task_id = next_task_id(language.extension);
    let ms = |s: &Instant| s.elapsed().as_secs_f64() * 1000.0;

    if !native_eval_enabled() {
        return unavailable(
            task_id,
            language.engine,
            symbol_path,
            ms(&start),
            "native evaluation is switched off by AXIOM_EVAL_NATIVE".to_string(),
            "Unset AXIOM_EVAL_NATIVE to let axiom run this language's toolchain, or run the \
             project's own tests and report the outcome with axiom_record_verification."
                .to_string(),
        );
    }

    let recipe = match pick_recipe(language) {
        Some(r) => r,
        None => {
            let wanted: Vec<&str> = language.recipes.iter().map(|r| r.probe).collect();
            return unavailable(
                task_id,
                language.engine,
                symbol_path,
                ms(&start),
                format!(
                    "no working toolchain for .{} on PATH; looked for {}",
                    language.extension,
                    wanted.join(", ")
                ),
                format!(
                    "Install one of {} and put it on PATH, or run the project's own tests and \
                     report the outcome with axiom_record_verification; axiom_get_blast_radius \
                     will name the tests to run.",
                    wanted.join(", ")
                ),
            );
        }
    };

    let source = if (language.is_self_contained)(snippet) {
        snippet.to_string()
    } else {
        (language.wrap)(snippet)
    };

    let work_dir = match temp_work_dir(language.extension) {
        Ok(d) => d,
        Err(e) => {
            return unavailable(
                task_id,
                language.engine,
                symbol_path,
                ms(&start),
                format!("could not create a work directory: {e}"),
                "Check that the temp directory is writable.".to_string(),
            )
        }
    };
    let src_file = work_dir.join(recipe.file_name);

    if let Err(e) = std::fs::write(&src_file, &source) {
        let elapsed = ms(&start);
        let _ = std::fs::remove_dir_all(&work_dir);
        return unavailable(
            task_id,
            language.engine,
            symbol_path,
            elapsed,
            format!("could not write the snippet to {}: {e}", src_file.display()),
            "Check that the temp directory is writable.".to_string(),
        );
    }

    if let Some(build) = recipe.build {
        let (program, args) = build(&src_file, &work_dir);
        let mut cmd = Command::new(&program);
        cmd.args(&args).current_dir(&work_dir);
        match run_with_timeout(cmd, timeout) {
            Ok(done) if done.timed_out => {
                let elapsed = ms(&start);
                let _ = std::fs::remove_dir_all(&work_dir);
                return timed_out_report(
                    task_id,
                    language.engine,
                    symbol_path,
                    elapsed,
                    &program,
                    timeout,
                );
            }
            Ok(done) if !done.succeeded() => {
                let elapsed = ms(&start);
                let _ = std::fs::remove_dir_all(&work_dir);
                return compilation_error(
                    task_id,
                    language.engine,
                    symbol_path,
                    elapsed,
                    &program,
                    done,
                );
            }
            Ok(_) => {}
            Err(e) => {
                let elapsed = ms(&start);
                let _ = std::fs::remove_dir_all(&work_dir);
                return unavailable(
                    task_id,
                    language.engine,
                    symbol_path,
                    elapsed,
                    format!("could not run {program}: {e}"),
                    format!("Check that {program} is installed and executable."),
                );
            }
        }
    }

    let (program, args) = (recipe.run)(&src_file, &work_dir);
    let mut cmd = Command::new(&program);
    cmd.args(&args).current_dir(&work_dir);
    let done = match run_with_timeout(cmd, timeout) {
        Ok(d) => d,
        Err(e) => {
            let elapsed = ms(&start);
            let _ = std::fs::remove_dir_all(&work_dir);
            return unavailable(
                task_id,
                language.engine,
                symbol_path,
                elapsed,
                format!("could not run {program}: {e}"),
                format!("Check that {program} is installed and executable."),
            );
        }
    };
    let duration = ms(&start);
    let _ = std::fs::remove_dir_all(&work_dir);

    if done.timed_out {
        return timed_out_report(
            task_id,
            language.engine,
            symbol_path,
            duration,
            &program,
            timeout,
        );
    }

    if done.succeeded() {
        let checks = language
            .assertion_tokens
            .iter()
            .map(|t| snippet.matches(t).count())
            .sum();
        let mut report = CtopReport::pass(
            task_id,
            language.engine.to_string(),
            duration,
            checks,
            format!(
                "{program} ran the snippet to completion: {}",
                snippet.lines().next().unwrap_or("")
            ),
        );
        report.stdout = done.stdout;
        report.stderr = done.stderr;
        return report;
    }

    let detail = if done.stderr.trim().is_empty() {
        done.stdout.clone()
    } else {
        done.stderr.clone()
    };
    CtopReport::fail(
        task_id,
        language.engine.to_string(),
        duration,
        vec![FailedCheck {
            symbol: symbol_path.to_string(),
            error_type: "AssertionFailure".to_string(),
            expected: Some("the snippet to run to completion".to_string()),
            actual: Some(if detail.trim().is_empty() {
                format!("{program} exited with a non-zero status")
            } else {
                detail.clone()
            }),
            stack_trace_ast_nodes: vec![symbol_path.to_string()],
            hint: Some(format!(
                "The snippet ran under {program} and failed. The output above is the toolchain's \
                 own."
            )),
        }],
        done.stdout,
        detail,
    )
}
