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
//!
//! Two more keep the host's secrets out of the snippet's reach. The child gets
//! an environment built from an allowlist rather than inherited, see
//! [`confine_environment`], and the deadline ends everything the snippet
//! started rather than only the process this tier spawned, see
//! [`run_with_timeout`].

use crate::artifact_cache;
use axiom_proto::{CtopReport, CtopStatus, DiagnosticSpan, FailedCheck};
use std::collections::HashMap;
use std::path::Path;
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
    /// Arguments that make `probe` print what version it is.
    ///
    /// Separate from `probe_args` because those are chosen to produce no output:
    /// `python -c pass` and `node -e ""` are silent by design, which is right for
    /// a probe and useless for a fingerprint. Reusing them gave `node=` and
    /// `python=` in the environment key, so upgrading either would not have
    /// invalidated a single cached verdict.
    version_args: &'static [&'static str],
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
    let mut imports = Vec::new();
    let mut body = Vec::new();

    for line in snippet.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") {
            imports.push(line);
        } else {
            body.push(line);
        }
    }

    let imports_str = if imports.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", imports.join("\n"))
    };
    let body_str = body.join("\n");

    format!("package main\n\n{imports_str}func main() {{\n{body_str}\n}}\n")
}

fn kotlin_wrap(snippet: &str) -> String {
    // A top-level main, so the compiled class is AxiomEvalKt and the launcher
    // has something to call.
    format!(
        "fun main() {{
{snippet}
}}
"
    )
}

fn scala_wrap(snippet: &str) -> String {
    let mut imports = Vec::new();
    let mut body = Vec::new();

    for line in snippet.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") || trimmed.starts_with("package ") {
            imports.push(line);
        } else {
            body.push(line);
        }
    }

    let imports_str = if imports.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", imports.join("\n"))
    };
    let body_str = body.join("\n");

    format!(
        "{imports_str}object AxiomEval {{\n  def main(args: Array[String]): Unit = {{\n{body_str}\n  }}\n}}\n"
    )
}

fn java_wrap(snippet: &str) -> String {
    let mut imports = Vec::new();
    let mut body = Vec::new();

    for line in snippet.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") || trimmed.starts_with("package ") {
            imports.push(line);
        } else {
            body.push(line);
        }
    }

    let imports_str = if imports.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", imports.join("\n"))
    };
    let body_str = body.join("\n");

    format!(
        "{imports_str}public class AxiomEval {{\n    public static void main(String[] args) throws Exception {{\n{body_str}\n    }}\n}}\n"
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
            version_args: &["--version"],
            file_name: "axiom_eval.py",
            build: None,
            run: |src, _| ("python3".to_string(), vec![src.display().to_string()]),
        },
        Recipe {
            probe: "python",
            probe_args: &["-c", "pass"],
            version_args: &["--version"],
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
        version_args: &["--version"],
        file_name: "axiom_eval.js",
        build: None,
        run: |src, _| ("node".to_string(), vec![src.display().to_string()]),
    }],
};

static TYPESCRIPT: NativeLanguage = NativeLanguage {
    extension: "ts",
    engine: "tier2_native_typescript",
    // `throw` is here because it is the assertion style the two recipes agree
    // on. Without it a snippet written the documented way reports
    // passed_checks_count 0 next to PASSED, which reads as a pass nothing
    // checked.
    assertion_tokens: &["assert", "expect(", "throw "],
    // Nothing is injected, and the two recipes do not offer the same
    // environment, so a snippet has to bring assertions that need neither an
    // import nor an ambient type declaration.
    //
    // Measured rather than assumed, after #9. Under deno,
    // `import assert from "node:assert"` works. Under tsc it is TS2591,
    // "cannot find name", because @types/node is not installed, so the same
    // snippet passes on one machine and comes back CompilationError on
    // another. A bare `if (!cond) throw new Error(...)` needs nothing from
    // either and produces the same verdict under both, which is why the guide
    // tells a caller to write that.
    is_self_contained: |_| true,
    wrap: |s| s.to_string(),
    recipes: &[
        // Deno runs TypeScript directly and grants no permissions unless asked,
        // so a snippet reaching for the network or the filesystem fails rather
        // than succeeding quietly. A thrown error exits non-zero, which is what
        // the report shape assumes; measured, not reasoned.
        Recipe {
            probe: "deno",
            probe_args: &["--version"],
            version_args: &["--version"],
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
            version_args: &["--version"],
            file_name: "axiom_eval.ts",
            build: Some(|src, _| {
                (
                    "tsc".to_string(),
                    vec![
                        "--target".to_string(),
                        "es2020".to_string(),
                        "--module".to_string(),
                        "commonjs".to_string(),
                        // tsc emits the .js anyway when type checking fails,
                        // which would leave a file for the run step to execute
                        // after the build step reported an error. The work
                        // directory is fresh per evaluation so nothing stale
                        // can be picked up today, but a verdict must not depend
                        // on that staying true.
                        "--noEmitOnError".to_string(),
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
        version_args: &["version"],
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
        version_args: &["-version"],
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

static KOTLIN: NativeLanguage = NativeLanguage {
    extension: "kt",
    engine: "tier2_native_kotlin",
    // `check` and `require` throw whatever the JVM's assertion flag says, unlike
    // `assert`, so they count too.
    assertion_tokens: &["assert", "check(", "require("],
    is_self_contained: |s| s.contains("fun main("),
    wrap: kotlin_wrap,
    recipes: &[Recipe {
        probe: "kotlinc",
        probe_args: &["-version"],
        version_args: &["-version"],
        file_name: "AxiomEval.kt",
        build: Some(|src, dir| {
            (
                "kotlinc".to_string(),
                vec![
                    src.display().to_string(),
                    "-d".to_string(),
                    dir.display().to_string(),
                ],
            )
        }),
        // Kotlin's `assert` compiles to a check of the JVM's assertion status
        // for the enclosing class, exactly as Java's does, so without -ea a
        // false assertion is a no-op and the snippet exits zero. Measured, not
        // assumed: `assert(1 + 1 == 3)` printed the line after it and returned
        // success until -J-ea was passed. The run goes through the `kotlin`
        // launcher rather than `java` because the launcher knows where the
        // Kotlin standard library lives, and that path is per-installation.
        //
        // A top-level `fun main` in AxiomEval.kt compiles to `AxiomEvalKt`.
        run: |_, dir| {
            (
                "kotlin".to_string(),
                vec![
                    "-J-ea".to_string(),
                    "-cp".to_string(),
                    dir.display().to_string(),
                    "AxiomEvalKt".to_string(),
                ],
            )
        },
    }],
};

static SCALA: NativeLanguage = NativeLanguage {
    extension: "scala",
    engine: "tier2_native_scala",
    assertion_tokens: &["assert", "require("],
    is_self_contained: |s| s.contains("def main(") || s.contains("@main"),
    wrap: scala_wrap,
    recipes: &[Recipe {
        probe: "scala",
        probe_args: &["version"],
        version_args: &["version"],
        file_name: "AxiomEval.scala",
        // No build step: the Scala 3 runner compiles and runs in one command.
        build: None,
        // No -ea either, and that difference is the point. Scala's `assert` is
        // `Predef.assert`, which throws unconditionally rather than compiling to
        // a JVM assertion check, so a false assertion fails whatever the flag
        // says. Measured the same way Kotlin's was.
        run: |src, _| ("scala".to_string(), vec![src.display().to_string()]),
    }],
};

static LANGUAGES: &[&NativeLanguage] = &[
    &PYTHON,
    &JAVASCRIPT,
    &TYPESCRIPT,
    &GO,
    &JAVA,
    &KOTLIN,
    &SCALA,
];

/// The driver for a file extension, if this tier has one.
///
/// `mjs`, `cjs` and `jsx` are Node's; `tsx` is TypeScript's; `kts` is a Kotlin
/// script, which the same compiler reads.
///
/// Kotlin and Scala are indexed by the Java parser and now have recipes of their
/// own. They are not handed to `javac`: neither compiles as Java, and the error
/// would be filed against the snippet rather than against the language.
pub fn language_for(extension: &str) -> Option<&'static NativeLanguage> {
    let normalised = match extension {
        "mjs" | "cjs" | "jsx" => "js",
        "tsx" => "ts",
        "kts" => "kt",
        "sc" => "scala",
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

/// Variables a snippet's process receives from this one, compared without
/// regard to case because Windows does not distinguish it.
///
/// Each is here because a toolchain reads it: the search path and its Windows
/// companions, a home and a temp directory, locale, and every language's own
/// configuration. Nothing here is a credential, and nothing with a credential
/// in it is listed under a prefix. `GO*` would have admitted
/// `GOOGLE_APPLICATION_CREDENTIALS`, and `CARGO_*` the registry token, so the
/// Go and Rust names are spelled out.
const PASSED_NAMES: &[&str] = &[
    // Process and platform.
    "PATH",
    "PATHEXT",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "TEMP",
    "TMP",
    "TMPDIR",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "LANG",
    "LANGUAGE",
    "TZ",
    "USER",
    "USERNAME",
    "LOGNAME",
    "SHELL",
    "TERM",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROCESSOR_LEVEL",
    "PROCESSOR_REVISION",
    "OS",
    // Go.
    "GOPATH",
    "GOROOT",
    "GOCACHE",
    "GOMODCACHE",
    "GOFLAGS",
    "GOPROXY",
    "GOSUMDB",
    "GONOSUMDB",
    "GOPRIVATE",
    "GOTOOLCHAIN",
    "GOTOOLDIR",
    "GOTMPDIR",
    "GOENV",
    "GOWORK",
    "GOOS",
    "GOARCH",
    "GO111MODULE",
    // Rust and MSVC toolchains.
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "RUST_BACKTRACE",
    "LIB",
    "LIBPATH",
    "INCLUDE",
    "VCINSTALLDIR",
    "VCTOOLSVERSION",
    "VCTOOLSINSTALLDIR",
    "VCTOOLSREDISTDIR",
    "WINDOWSSDKDIR",
    "WINDOWSSDKVERSION",
    "WINDOWSSDKBINPATH",
    "WINDOWSLIBPATH",
    "UNIVERSALCRTSDKDIR",
    "UCRTVERSION",
    "VSINSTALLDIR",
    // Node and Deno.
    "NODE_PATH",
    "NODE_OPTIONS",
    "NODE_EXTRA_CA_CERTS",
    "DENO_DIR",
    "DENO_INSTALL",
    "DENO_INSTALL_ROOT",
    "DENO_NO_UPDATE_CHECK",
    "DENO_NO_PROMPT",
    // Python.
    "PYTHONPATH",
    "PYTHONHOME",
    "PYTHONIOENCODING",
    "PYTHONUTF8",
    "PYTHONUNBUFFERED",
    "PYTHONDONTWRITEBYTECODE",
    "VIRTUAL_ENV",
];

/// Prefixes whose whole namespace belongs to a toolchain: the JVM launchers,
/// Kotlin, Scala and the coursier resolver behind it, the JVM build tools, and
/// the locale and XDG directory conventions.
const PASSED_PREFIXES: &[&str] = &[
    "JAVA_",
    "JDK_",
    "JRE_",
    "KOTLIN_",
    "SCALA_",
    "COURSIER_",
    "GRADLE_",
    "SBT_",
    "MAVEN_",
    "M2_",
    "LC_",
    "XDG_",
    "VC_",
    "VS_",
    "VSCMD_",
    "VISUALSTUDIO_",
    "WINDOWSSDK",
];

/// Never passed, whatever `AXIOM_EVAL_ENV_PASS` says.
const REFUSED_NAMES: &[&str] = &[
    "AXIOM_SIGNING_KEY",
    "AXIOM_SIGNING_KEY_FILE",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "GITLAB_TOKEN",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
];

pub fn is_refused_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if REFUSED_NAMES.contains(&upper.as_str()) {
        return true;
    }
    if upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("PRIVATE_KEY")
        || upper.contains("SIGNING_KEY")
    {
        return !PASSED_NAMES.contains(&upper.as_str());
    }
    false
}

/// Give `command` an environment a snippet may see.
///
/// A child inherits its parent's environment unless told otherwise, and this
/// parent holds the signing key. Measured before this existed: a Python
/// snippet printed `AXIOM_SIGNING_KEY` and the report carried the value back
/// to the caller in `stdout`. That hands the party whose claims the signature
/// exists to check the means to sign anything, and it is the same shape for
/// every other secret an operator's shell carries.
///
/// So the child starts from nothing and receives [`PASSED_NAMES`], anything
/// under [`PASSED_PREFIXES`], and whatever `AXIOM_EVAL_ENV_PASS` names as a
/// comma-separated list, for a toolchain that reads something not listed.
/// The two signing-key variables are refused even there.
///
/// The probe that decides whether a toolchain is usable runs under the same
/// environment as the snippet, so a toolchain that needs a variable the
/// confinement drops is reported as unavailable rather than failing the
/// snippet.
pub fn confine_environment(command: &mut Command) -> &mut Command {
    let extra: Vec<String> = std::env::var("AXIOM_EVAL_ENV_PASS")
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_ascii_uppercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    command.env_clear();
    for (name, value) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if is_refused_secret(&upper) {
            continue;
        }
        let passed = PASSED_NAMES.contains(&upper.as_str())
            || PASSED_PREFIXES.iter().any(|p| upper.starts_with(p))
            || extra.contains(&upper);
        if passed {
            command.env(name, value);
        }
    }
    command
}

/// What a child process did, once it either finished or was killed.
pub struct Finished {
    pub status: Option<std::process::ExitStatus>,
    pub stdout: String,
    pub stderr: String,
    /// True when the process was killed for running past the deadline. A killed
    /// process has no meaningful exit status, so this must be checked first.
    pub timed_out: bool,
    /// False when the pipes were still open at the deadline, so `stdout` and
    /// `stderr` may be missing a tail. Something the snippet started was
    /// still holding them.
    pub drained: bool,
    /// Peak memory consumed by the sandboxed process tree in bytes, if measured.
    pub peak_memory_bytes: Option<u64>,
}

impl Finished {
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.status.map(|s| s.success()).unwrap_or(false)
    }
}

/// Read a pipe on its own thread, handing chunks over as they arrive.
///
/// Chunks rather than one buffer at EOF, because EOF is not guaranteed: the
/// pipe stays open for as long as any process holding it lives, and the child
/// can hand it to processes of its own. A reader that only reports at EOF
/// reports nothing when the collector gives up waiting.
fn drain_in_background(
    pipe: Option<impl std::io::Read + Send + 'static>,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(chunk[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// Collect what a reader has sent, until EOF or `until`, whichever is first.
/// The flag says which.
fn collect_until(rx: &std::sync::mpsc::Receiver<Vec<u8>>, until: Instant) -> (Vec<u8>, bool) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut buf = Vec::new();
    loop {
        let now = Instant::now();
        if now >= until {
            return (buf, false);
        }
        match rx.recv_timeout(until - now) {
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(RecvTimeoutError::Timeout) => return (buf, false),
            Err(RecvTimeoutError::Disconnected) => return (buf, true),
        }
    }
}

/// End the child and everything it started.
///
/// The child was made the leader of its own process group when it was
/// spawned, so signalling the negative pid reaches every process it started
/// that has not called setsid itself.
#[cfg(unix)]
fn kill_tree(child: &mut std::process::Child) {
    let pid = child.id() as i32;
    // SAFETY: kill(2) with a pid this process spawned and has not yet reaped,
    // so the id cannot have been reused.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// End the child and everything it started.
///
/// `taskkill /T` follows the parent links down from the child. A process that
/// has reparented itself is out of its reach; a Job Object would close that
/// gap, at the cost of FFI this crate does not otherwise carry, and the
/// bounded drain in [`run_with_timeout`] keeps even that case from holding
/// the call open.
#[cfg(windows)]
fn kill_tree(child: &mut std::process::Child) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// Run a command, killing it and everything it started if it outlives
/// `timeout`.
///
/// `Command::output` waits forever, which is fine for a compiler and not fine
/// for agent-authored code: one runaway loop and the server stops answering.
///
/// The deadline bounds the whole call, not only the child. Measured before it
/// did: a Python snippet that started a second process and slept was killed
/// at its two-second deadline, and the call still returned after sixty,
/// because the second process had inherited the stdout pipe and draining it
/// to EOF meant waiting for that process to finish on its own. So the child
/// is spawned as a process group of its own on Unix, the whole tree is ended
/// on timeout, and the pipes are read for a bounded time after the child is
/// gone rather than to EOF.
///
/// The environment is confined here too, so no caller can forget it.
pub fn run_with_timeout(mut command: Command, timeout: Duration) -> std::io::Result<Finished> {
    confine_environment(&mut command);
    crate::sandbox::prepare_command(&mut command);

    let mut sandbox = crate::sandbox::SandboxGuard::new();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(sb) = sandbox.as_mut() {
        let _ = sb.assign_child(&child);
    }

    // Each pipe is drained on its own thread. Reading them after the wait
    // deadlocks as soon as a snippet writes more than one pipe buffer holds.
    let out_rx = drain_in_background(child.stdout.take());
    let err_rx = drain_in_background(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if Instant::now() >= deadline {
                    if let Some(sb) = sandbox.as_ref() {
                        sb.terminate();
                    }
                    kill_tree(&mut child);
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };

    let peak_memory_bytes = sandbox.as_ref().and_then(|sb| sb.peak_memory_bytes());

    // After the child exits its pipes close at once in the ordinary case. A
    // process it started and left behind can hold them open, and that process
    // is not the one being judged, so the wait for EOF is bounded: by the
    // deadline when the child finished early, by a short grace when the
    // deadline is what ended it.
    let grace = Duration::from_secs(1);
    let until = if timed_out {
        Instant::now() + grace
    } else {
        deadline.max(Instant::now() + grace)
    };
    let (out, out_done) = collect_until(&out_rx, until);
    let (err, err_done) = collect_until(&err_rx, until);

    Ok(Finished {
        status,
        stdout: String::from_utf8_lossy(&out).to_string(),
        stderr: String::from_utf8_lossy(&err).to_string(),
        timed_out,
        drained: out_done && err_done,
        peak_memory_bytes,
    })
}

/// Where a program name actually lives on this machine.
///
/// Windows resolves a bare name through `CreateProcess`, which appends `.exe`
/// and nothing else. npm installs its global binaries as shims: `tsc` and
/// `deno` arrive as `tsc.cmd` and `deno.cmd`, with no `.exe` anywhere. So a
/// toolchain the user can run from their own shell was invisible here, and the
/// evaluator answered `EVALUATOR_UNAVAILABLE` naming a program that was
/// installed. The refusal was safe, and it was also wrong.
///
/// Found while verifying #9: with both `tsc` and `deno` on PATH, the TypeScript
/// test still took the refusal branch, which is the same way the recipe reached
/// main unrun in the first place.
///
/// PATHEXT is honoured rather than hardcoded, since that is what the shell
/// itself searches. A name that already carries a path is returned untouched.
#[cfg(windows)]
fn resolve_program(program: &str) -> std::ffi::OsString {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    match std::env::var_os("PATH") {
        Some(path) => resolve_in(program, &path, &exts),
        None => std::ffi::OsString::from(program),
    }
}

/// The search itself, with the environment passed in.
///
/// Split out so it can be tested without mutating `PATH` for the whole process,
/// which the rest of the suite is running in at the same time.
#[cfg(windows)]
fn resolve_in(program: &str, path: &std::ffi::OsStr, exts: &str) -> std::ffi::OsString {
    use std::ffi::OsString;

    if program.contains('/') || program.contains('\\') {
        return OsString::from(program);
    }

    // PATHEXT candidates come first, and the bare name only when it already
    // carries an extension. npm drops three files for one tool: `deno.cmd`,
    // `deno.ps1`, and an extension-less `deno` holding a POSIX shell script for
    // Git Bash. Windows cannot execute that third one, so matching the bare
    // name first found it, handed it to CreateProcess, and failed, which looks
    // exactly like the tool not being installed.
    let named_extension = std::path::Path::new(program).extension().is_some();
    for dir in std::env::split_paths(path) {
        for ext in exts.split(';').filter(|e| !e.is_empty()) {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return candidate.into_os_string();
            }
        }
        if named_extension {
            let bare = dir.join(program);
            if bare.is_file() {
                return bare.into_os_string();
            }
        }
    }

    // Nothing found. Hand back the bare name so the spawn fails the way it
    // always did, rather than inventing a path that does not exist.
    OsString::from(program)
}

/// Unix resolves a bare name through PATH itself, and an npm shim there is an
/// executable script with a shebang, so there is nothing to add.
#[cfg(not(windows))]
fn resolve_program(program: &str) -> std::ffi::OsString {
    std::ffi::OsString::from(program)
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

    // Under the snippet's environment, so that a toolchain which needs a
    // variable the confinement drops is reported as missing rather than
    // failing the snippet.
    let mut command = Command::new(resolve_program(probe));
    confine_environment(&mut command);
    let usable = command
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

/// The version string `program` reports for `args`, memoized per process.
///
/// One helper feeds both the environment fingerprints and the artifact-cache
/// key, so the two can never disagree about which toolchain produced an
/// artifact. Runs under the snippet's confined environment for the same
/// reason the probe does. An empty answer is reported as such rather than as
/// an empty string, because a fingerprint of `node=` is one an upgrade never
/// moves.
pub(crate) fn program_version(program: &str, args: &[&str]) -> String {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Some(known) = cache.lock().unwrap().get(program) {
        return known.clone();
    }

    let mut command = Command::new(resolve_program(program));
    confine_environment(&mut command);
    let version = command
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| {
            // Some report on stdout, some on stderr, and javac has moved
            // between the two across releases. Both are read rather than
            // picking one and getting an empty string on the wrong version.
            let mut combined = String::from_utf8_lossy(&o.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&o.stderr));
            combined.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .unwrap_or_default();
    let version = if version.trim().is_empty() {
        "<reported no version>".to_string()
    } else {
        version
    };

    cache
        .lock()
        .unwrap()
        .insert(program.to_string(), version.clone());
    version
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

/// Fill the per-process probe and version caches for `language`, so the
/// spawns they memoize land here rather than on the first evaluation's clock.
pub(crate) fn prime(language: &NativeLanguage) {
    if let Some(recipe) = pick_recipe(language) {
        let _ = program_version(recipe.probe, recipe.version_args);
    }
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
            diagnostics: Vec::new(),
        }],
        passed_checks_count: 0,
        passed_checks_basis: String::new(),
        stdout: String::new(),
        stderr: actual,
        memory_allocated_bytes: None,
        compile_cache: None,
        diagnostics: Vec::new(),
        suggested_fixes: Vec::new(),
    }
}

fn timed_out_report(
    task_id: String,
    engine: &str,
    symbol_path: &str,
    duration_ms: f64,
    program: &str,
    timeout: Duration,
    peak_memory_bytes: Option<u64>,
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
            diagnostics: Vec::new(),
        }],
        passed_checks_count: 0,
        passed_checks_basis: String::new(),
        stdout: String::new(),
        stderr: format!("timed out after {}s", timeout.as_secs()),
        memory_allocated_bytes: peak_memory_bytes,
        compile_cache: None,
        diagnostics: Vec::new(),
        suggested_fixes: Vec::new(),
    }
}

/// Parse multi-language compiler errors/warnings into structured DiagnosticSpan items
pub fn parse_compiler_diagnostics(stderr: &str, stdout: &str) -> Vec<DiagnosticSpan> {
    let mut diags = Vec::new();
    let text = if !stderr.trim().is_empty() {
        stderr
    } else {
        stdout
    };

    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();

        // 1. Rustc format: error[E0425]: ... / warning: ...
        if trimmed.starts_with("error") || trimmed.starts_with("warning") {
            let is_warning = trimmed.starts_with("warning");
            let severity = if is_warning {
                "warning".to_string()
            } else {
                "error".to_string()
            };
            let message = trimmed.to_string();

            let mut file = None;
            let mut line_num = None;
            let mut col_num = None;
            let mut suggestion = None;

            while let Some(&next_l) = lines.peek() {
                let next_t = next_l.trim();
                if next_t.starts_with("-->") {
                    let loc = next_t.trim_start_matches("-->").trim();
                    let parts: Vec<&str> = loc.split(':').collect();
                    if parts.len() >= 3 {
                        file = Some(parts[0].to_string());
                        line_num = parts[1].parse::<usize>().ok();
                        col_num = parts[2].parse::<usize>().ok();
                    } else if parts.len() == 2 {
                        file = Some(parts[0].to_string());
                        line_num = parts[1].parse::<usize>().ok();
                    }
                    lines.next();
                } else if next_t.starts_with("help:") {
                    suggestion = Some(next_t.trim_start_matches("help:").trim().to_string());
                    lines.next();
                } else if next_t.starts_with("error") || next_t.starts_with("warning") {
                    break;
                } else {
                    lines.next();
                }
            }

            diags.push(DiagnosticSpan {
                file,
                line: line_num,
                column: col_num,
                message,
                severity,
                suggested_replacement: suggestion,
            });
            continue;
        }

        // 2. Python traceback: File "...", line X
        if trimmed.starts_with("File \"") {
            if let Some(rest) = trimmed.strip_prefix("File \"") {
                if let Some(idx) = rest.find('"') {
                    let f = &rest[..idx];
                    let rem = &rest[idx + 1..];
                    let line_num = if let Some(l_idx) = rem.find("line ") {
                        rem[l_idx + 5..]
                            .split(|c: char| !c.is_ascii_digit())
                            .next()
                            .and_then(|s| s.parse::<usize>().ok())
                    } else {
                        None
                    };
                    let mut msg = String::new();
                    for nxt in lines.by_ref() {
                        let tn = nxt.trim();
                        if !tn.is_empty() && !tn.starts_with("File \"") {
                            msg = tn.to_string();
                        }
                    }
                    if !msg.is_empty() {
                        diags.push(DiagnosticSpan {
                            file: Some(f.to_string()),
                            line: line_num,
                            column: None,
                            message: msg,
                            severity: "error".to_string(),
                            suggested_replacement: None,
                        });
                        break;
                    }
                }
            }
        }

        // 3. Javac / Clang / GCC: file:line:col: error: ...
        let parts: Vec<&str> = trimmed.splitn(4, ':').collect();
        if parts.len() >= 4 {
            let f = parts[0].trim();
            if let Ok(l) = parts[1].trim().parse::<usize>() {
                if let Ok(c) = parts[2].trim().parse::<usize>() {
                    let rest = parts[3].trim();
                    let sev = if rest.starts_with("warning") {
                        "warning"
                    } else {
                        "error"
                    };
                    diags.push(DiagnosticSpan {
                        file: Some(f.to_string()),
                        line: Some(l),
                        column: Some(c),
                        message: rest.to_string(),
                        severity: sev.to_string(),
                        suggested_replacement: None,
                    });
                    continue;
                } else {
                    let rest = format!("{}: {}", parts[2].trim(), parts[3].trim());
                    let sev = if rest.contains("warning") {
                        "warning"
                    } else {
                        "error"
                    };
                    diags.push(DiagnosticSpan {
                        file: Some(f.to_string()),
                        line: Some(l),
                        column: None,
                        message: rest,
                        severity: sev.to_string(),
                        suggested_replacement: None,
                    });
                    continue;
                }
            }
        }
    }

    diags
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
    let diags = parse_compiler_diagnostics(&done.stderr, &done.stdout);
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
            diagnostics: diags.clone(),
        }],
        passed_checks_count: 0,
        passed_checks_basis: String::new(),
        stdout: done.stdout,
        stderr: detail,
        memory_allocated_bytes: done.peak_memory_bytes,
        compile_cache: None,
        diagnostics: diags,
        suggested_fixes: Vec::new(),
    }
}

/// A fresh, unpredictably named directory for one evaluation, removed when the
/// handle is dropped.
///
/// It used to be `axiom_eval_<tag>_<pid>_<seq>` under the shared temp
/// directory, created with `create_dir_all`, which succeeds on a directory
/// that already exists: anyone else on the machine who could guess the name
/// could create it first and own what the snippet was written into.
pub fn temp_work_dir(tag: &str) -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(&format!("axiom_eval_{tag}_"))
        .tempdir()
}

/// Evaluate `snippet` as `language`, allowing `timeout` for each command.
///
/// A report comes back in every case. The one thing that never comes back is
/// `PASSED` for something that did not compile and run.
/// Did the toolchain fail before it ever reached the snippet?
///
/// A non-zero exit is not by itself a verdict about the code. `scala` and the
/// JVM launchers fetch their own compiler and dependencies on first use, and
/// when that fetch fails they exit non-zero having executed nothing the caller
/// wrote. Reporting `FAILED` there tells an agent its code is wrong on the
/// strength of a download, which is the same class as the assertion-substring
/// fallback removed earlier: a verdict produced by something that is not a run
/// of the code.
///
/// Observed on CI, where this returned FAILED after 134s:
///
/// ```text
/// Failed to download https://.../bloop-frontend_2.12-2.0.19.pom
/// ```
///
/// The markers are deliberately narrow, and all of them belong to a resolver or
/// a downloader rather than to a compiler or a program. A snippet that prints
/// one of these itself is misread as an unusable toolchain, which costs a
/// refusal where a verdict was possible. That is the safe direction: refusing
/// says nothing was established, which is true either way.
pub fn toolchain_failure_reason(stdout: &str, stderr: &str) -> Option<String> {
    const RESOLVER_FAILURES: &[&str] = &[
        "Failed to download",
        "Error downloading",
        "failed to resolve",
        "Could not resolve",
        "unresolved dependency",
        "not found in any repository",
        "Server returned HTTP response code",
        "UnknownHostException",
        "Connection refused",
        "Connection timed out",
        "Network is unreachable",
        "Could not find or load main class",
    ];

    for stream in [stderr, stdout] {
        for line in stream.lines() {
            if RESOLVER_FAILURES.iter().any(|m| line.contains(m)) {
                return Some(format!(
                    "the toolchain did not get as far as running the snippet: {}",
                    line.trim().chars().take(200).collect::<String>()
                ));
            }
        }
    }
    None
}

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

    let work = match temp_work_dir(language.extension) {
        Ok(d) => d,
        Err(e) => {
            return unavailable(
                task_id,
                language.engine,
                symbol_path,
                ms(&start),
                format!("could not create a work directory: {e}"),
                "Check that the temp directory is writable.".to_string(),
            );
        }
    };
    // Removed when `work` drops, on every path out of this function.
    let work_dir = work.path().to_path_buf();
    let src_file = work_dir.join(recipe.file_name);

    if let Err(e) = std::fs::write(&src_file, &source) {
        let elapsed = ms(&start);
        return unavailable(
            task_id,
            language.engine,
            symbol_path,
            elapsed,
            format!("could not write the snippet to {}: {e}", src_file.display()),
            "Check that the temp directory is writable.".to_string(),
        );
    }

    // Everything that could change what the build step produces goes into
    // the key; the run step's flags do not, because they do not shape the
    // artifact. The build spec is rendered against placeholder paths so the
    // key does not move with the temp directory's name.
    let cache_key = if artifact_cache::enabled() {
        recipe.build.map(|build| {
            let (build_program, build_args) = build(Path::new("<src>"), Path::new("<work>"));
            artifact_cache::key_of(&[
                "native",
                language.extension,
                recipe.probe,
                recipe.file_name,
                &build_program,
                &build_args.join("\u{1f}"),
                &program_version(recipe.probe, recipe.version_args),
                std::env::consts::OS,
                std::env::consts::ARCH,
                &source,
            ])
        })
    } else {
        None
    };
    let mut compile_cache: Option<String> = None;
    let mut skip_build = false;
    if let Some(key) = cache_key.as_deref() {
        if artifact_cache::restore(key, &work_dir) {
            skip_build = true;
            compile_cache = Some("hit".to_string());
        } else {
            compile_cache = Some("miss".to_string());
        }
    }

    if !skip_build {
        if let Some(build) = recipe.build {
            let (program, args) = build(&src_file, &work_dir);
            let mut cmd = Command::new(resolve_program(&program));
            cmd.args(&args).current_dir(&work_dir);
            match run_with_timeout(cmd, timeout) {
                Ok(done) if done.timed_out => {
                    let elapsed = ms(&start);
                    return timed_out_report(
                        task_id,
                        language.engine,
                        symbol_path,
                        elapsed,
                        &program,
                        timeout,
                        done.peak_memory_bytes,
                    );
                }
                Ok(done) if !done.succeeded() => {
                    let elapsed = ms(&start);
                    // A build that could not fetch its own dependencies has not
                    // found anything wrong with the snippet, so it is not a
                    // compilation error any more than it is a failure.
                    if let Some(reason) = toolchain_failure_reason(&done.stdout, &done.stderr) {
                        return unavailable(
                            task_id,
                            language.engine,
                            symbol_path,
                            elapsed,
                            reason,
                            format!(
                                "{program} exited non-zero without compiling the snippet, so                              nothing is known about it. Retry once its dependencies can be                              fetched, or run the project's own tests and report the outcome                              with axiom_record_verification."
                            ),
                        );
                    }
                    let mut report = compilation_error(
                        task_id,
                        language.engine,
                        symbol_path,
                        elapsed,
                        &program,
                        done,
                    );
                    report.compile_cache = compile_cache;
                    return report;
                }
                Ok(_) => {
                    // The artifact was just built from `source` by the exact
                    // toolchain the key names, so it is safe to reuse for the
                    // same key. The source file itself is excluded: it is
                    // rewritten fresh on every evaluation.
                    if let Some(key) = cache_key.as_deref() {
                        artifact_cache::store(key, &work_dir, &[recipe.file_name]);
                    }
                }
                Err(e) => {
                    let elapsed = ms(&start);
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
    }

    let (program, args) = (recipe.run)(&src_file, &work_dir);
    let mut cmd = Command::new(resolve_program(&program));
    cmd.args(&args).current_dir(&work_dir);
    let done = match run_with_timeout(cmd, timeout) {
        Ok(d) => d,
        Err(e) => {
            let elapsed = ms(&start);
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
    drop(work);

    if done.timed_out {
        let mut report = timed_out_report(
            task_id,
            language.engine,
            symbol_path,
            duration,
            &program,
            timeout,
            done.peak_memory_bytes,
        );
        report.compile_cache = compile_cache;
        return report;
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
        report.memory_allocated_bytes = done.peak_memory_bytes;
        report.compile_cache = compile_cache;
        return report;
    }

    if let Some(reason) = toolchain_failure_reason(&done.stdout, &done.stderr) {
        return unavailable(
            task_id,
            language.engine,
            symbol_path,
            duration,
            reason,
            format!(
                "{program} exited non-zero without running the snippet, so nothing is                  known about it. Retry once its dependencies can be fetched, or run                  the project's own tests and report the outcome with                  axiom_record_verification."
            ),
        );
    }

    let detail = if done.stderr.trim().is_empty() {
        done.stdout.clone()
    } else {
        done.stderr.clone()
    };
    let diags = parse_compiler_diagnostics(&done.stderr, &done.stdout);
    let mut report = CtopReport::fail(
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
            diagnostics: diags.clone(),
        }],
        done.stdout,
        detail,
    );
    report.diagnostics = diags;
    report.memory_allocated_bytes = done.peak_memory_bytes;
    report.compile_cache = compile_cache;
    report
}

/// Every language this tier knows how to drive.
///
/// Exposed so a test can ask whether each one is actually runnable here, rather
/// than each test discovering that for itself and quietly asserting nothing.
pub fn languages() -> &'static [&'static NativeLanguage] {
    LANGUAGES
}

/// A version string for every toolchain this tier can currently drive.
///
/// Feeds `EnvironmentKey`, which is what makes it safe for a closure to treat an
/// out-of-tree name as covered rather than as a gap. `anyhow::Result` does not
/// resolve to an indexed symbol and never will, and what it means is fixed by
/// the compiler and the lock file; if either moves, every cached verdict has to
/// go with it.
///
/// A toolchain that is not installed contributes nothing rather than an empty
/// string, so installing one changes the key and invalidates verdicts reached
/// without it. That is the right way round: a snippet that was refused for want
/// of a compiler must not stay refused once the compiler arrives.
///
/// The probe is the same cached one the evaluator uses, so this costs at most
/// one process spawn per language per process.
pub fn toolchain_fingerprints() -> Vec<String> {
    let mut out = Vec::new();
    for language in LANGUAGES {
        let Some(recipe) = pick_recipe(language) else {
            continue;
        };
        // An empty answer must not read as a version. A fingerprint of
        // `node=` is one that never changes, so an upgrade would leave every
        // cached verdict standing; `program_version` reports the emptiness
        // instead, which is the difference between a key that covers node and
        // one that only looks like it does.
        let version = program_version(recipe.probe, recipe.version_args);
        out.push(format!("{}={}", recipe.probe, version));
    }
    out.sort();
    out
}

#[cfg(all(test, windows))]
mod resolve_tests {
    use super::resolve_in;

    /// The npm global layout, which is what broke the TypeScript recipe: three
    /// files for one tool, and the only one Windows can execute is the `.cmd`.
    /// Matching the extension-less POSIX shim first found a file, spawned it,
    /// and failed, which reads as "the toolchain is not installed" for a
    /// toolchain the user can run from their own shell.
    #[test]
    fn a_cmd_shim_wins_over_the_extension_less_posix_shim() {
        let dir =
            std::env::temp_dir().join(format!("axiom_resolve_{}_{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("deno"), "#!/bin/sh\nexec node deno.js\n").expect("posix shim");
        std::fs::write(dir.join("deno.cmd"), "@node deno.js %*\n").expect("cmd shim");

        let path = std::ffi::OsString::from(dir.display().to_string());
        let resolved = resolve_in("deno", &path, ".COM;.EXE;.BAT;.CMD");

        // Compared case-insensitively: the extension comes from PATHEXT, which
        // is conventionally upper case, while the file on disk is lower case.
        // Windows does not distinguish them, and the point of the assertion is
        // which file was chosen, not how it was spelled.
        assert_eq!(
            resolved.to_string_lossy().to_lowercase(),
            dir.join("deno.cmd").display().to_string().to_lowercase(),
            "the executable shim must win over the one Windows cannot run"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Nothing on PATH means the bare name is handed back, so the spawn fails
    /// the way it always did. Inventing a path would turn a missing toolchain
    /// into a confusing error about a file that was never there.
    #[test]
    fn an_absent_program_is_returned_unchanged() {
        let dir =
            std::env::temp_dir().join(format!("axiom_resolve_{}_{}", std::process::id(), line!()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let path = std::ffi::OsString::from(dir.display().to_string());
        let resolved = resolve_in("definitely_not_installed", &path, ".COM;.EXE;.BAT;.CMD");

        assert_eq!(
            resolved,
            std::ffi::OsString::from("definitely_not_installed")
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
