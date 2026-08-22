//! What `axiom verify` says about who issued a record.
//!
//! The record carries an `agent_identity`, and until #11 it was the constant
//! `agent_axiom_v1` on every record ever written. Now a caller sets it, which
//! makes it caller-controlled text that this command prints. Two things have to
//! hold: the value must actually be shown, or accepting the argument changed
//! nothing a reader can see; and the output must say what the value is worth,
//! because a self-declared name sitting under a heading called "Agent" reads as
//! established when nothing established it.

use axiom_proto::{NewAttestation, ProvenanceAttestation};
use std::process::Command;

/// Run the binary in a directory holding just the ledger we wrote, so the test
/// does not read whatever the working tree happens to have attested.
fn verify_in(dir: &std::path::Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run the axiom binary");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

fn ledger_with(identity: &str, dir: &std::path::Path) {
    let record = ProvenanceAttestation::generate(NewAttestation {
        parent_merkle_root: "merkle_root_prev_77a1",
        commit_merkle_root: "merkle_root_aaaaaaaa",
        agent_identity: identity,
        prompt: "Tighten the guard",
        symbol_path: "auth::service::validate_token",
        ctop_task_id: "eval_0",
        verified_by: "reported",
        verification_detail: "cargo test",
        previous_seal: "",
    });
    std::fs::create_dir_all(dir.join(".axiom")).expect("create .axiom");
    std::fs::write(
        dir.join(".axiom").join("attestations.json"),
        serde_json::to_string_pretty(&vec![record]).expect("encode ledger"),
    )
    .expect("write ledger");
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_verify_agent_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("create temp dir");
    d
}

#[test]
fn verify_shows_the_agent_a_record_names() {
    let dir = temp_dir("named");
    ledger_with("claude-code-selftest", &dir);

    let (stdout, ok) = verify_in(
        &dir,
        &[
            "verify",
            "--symbol",
            "auth::service::validate_token",
            "--prompt",
            "Tighten the guard",
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        ok,
        "verify should succeed on a well-formed ledger:\n{stdout}"
    );
    assert!(
        stdout.contains("claude-code-selftest"),
        "the record names an agent and verify must show it:\n{stdout}"
    );
}

/// An unsigned record's agent name is a claim. Saying so is the whole point of
/// taking the argument rather than leaving the misleading constant in place.
#[test]
fn verify_says_an_unsigned_agent_name_is_only_a_claim() {
    let dir = temp_dir("claim");
    ledger_with("someone-elses-name", &dir);

    let (stdout, ok) = verify_in(
        &dir,
        &[
            "verify",
            "--symbol",
            "auth::service::validate_token",
            "--prompt",
            "Tighten the guard",
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);

    assert!(ok, "verify should succeed:\n{stdout}");
    assert!(
        stdout.contains("self-declared"),
        "an unsigned agent name must not be presented as established:\n{stdout}"
    );
}
