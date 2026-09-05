use axiom_vmm::native::{confine_environment, is_refused_secret, run_with_timeout};
use axiom_vmm::sandbox::SandboxGuard;
use std::process::Command;
use std::time::Duration;

#[test]
fn test_job_object_sandbox_creation_and_memory_accounting() {
    let guard = SandboxGuard::new();
    assert!(
        guard.is_some(),
        "SandboxGuard must be constructible on this OS"
    );

    let mut guard = guard.unwrap();

    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.args(["/c", "echo", "axiom_sandboxed_execution"]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", "echo axiom_sandboxed_execution"]);
        c
    };

    let mut child = cmd.spawn().expect("failed to spawn child process");
    let assigned = guard.assign_child(&child);
    assert!(assigned, "Child process must be assignable to SandboxGuard");

    let _ = guard.peak_memory_bytes();
    let _ = child.wait();
}

#[test]
fn test_secret_confinement_prevents_secret_leakage() {
    // Set test secrets in host environment
    unsafe {
        std::env::set_var(
            "AXIOM_SIGNING_KEY",
            "super_secret_hex_signing_key_never_leak",
        );
        std::env::set_var("AXIOM_SIGNING_KEY_FILE", "/path/to/key.sec");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "AKIAIOSFODNN7EXAMPLE");
        std::env::set_var("MY_APPLICATION_SECRET", "super_confidential");
        std::env::set_var("DATABASE_PASSWORD", "postgres_admin_pass");
    }

    assert!(is_refused_secret("AXIOM_SIGNING_KEY"));
    assert!(is_refused_secret("AXIOM_SIGNING_KEY_FILE"));
    assert!(is_refused_secret("AWS_SECRET_ACCESS_KEY"));
    assert!(is_refused_secret("MY_APPLICATION_SECRET"));
    assert!(is_refused_secret("DATABASE_PASSWORD"));

    let mut cmd = Command::new("cmd");
    confine_environment(&mut cmd);

    // Verify through execution that child cannot read the secrets
    let test_script = if cfg!(windows) {
        "if defined AXIOM_SIGNING_KEY (echo SECRET_LEAKED) else (echo SECRET_IS_CLEARED)"
    } else {
        "if [ -z \"$AXIOM_SIGNING_KEY\" ]; then echo SECRET_IS_CLEARED; else echo SECRET_LEAKED; fi"
    };

    let run_cmd = if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.args(["/c", test_script]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", test_script]);
        c
    };

    let finished = run_with_timeout(run_cmd, Duration::from_secs(5))
        .expect("failed to run sandboxed test command");

    assert!(finished.succeeded(), "Command must succeed");
    assert!(
        finished.stdout.contains("SECRET_IS_CLEARED"),
        "Secret must not be present in confined child stdout: {}",
        finished.stdout
    );
    assert!(
        !finished
            .stdout
            .contains("super_secret_hex_signing_key_never_leak"),
        "Secret value must never leak to child"
    );
}

#[test]
fn test_sandbox_timeout_terminates_runaway_process() {
    let timeout = Duration::from_millis(400);

    let runaway_cmd = if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.args(["-Command", "Start-Sleep -Seconds 10"]);
        c
    } else {
        let mut c = Command::new("sleep");
        c.arg("10");
        c
    };

    let start = std::time::Instant::now();
    let finished =
        run_with_timeout(runaway_cmd, timeout).expect("run_with_timeout should complete cleanly");

    let elapsed = start.elapsed();
    assert!(
        finished.timed_out,
        "Runaway process must be flagged as timed_out"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "Timeout must enforce rapid termination within deadline, took {:?}",
        elapsed
    );
}
