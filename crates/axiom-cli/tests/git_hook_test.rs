use axiom_proto::ProvenanceAttestation;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "axiom-githook-{}-{}-{}",
            tag,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn test_export_slsa_command() {
    let out = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .arg("export-slsa")
        .output()
        .expect("run export-slsa");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json output");
    assert!(parsed.is_array());
}

#[test]
fn test_git_hook_install_and_verify() {
    let dir = TempDir::new("hook_test");

    // Test install in tempdir
    let out_install = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(dir.path())
        .args(["git-hook", "--install"])
        .output()
        .expect("run git-hook --install");

    assert!(out_install.status.success());
    let hook_file = dir.path().join(".git").join("hooks").join("pre-commit");
    assert!(hook_file.exists());
    let hook_content = std::fs::read_to_string(&hook_file).unwrap();
    assert!(hook_content.contains("axiom git-hook --verify"));

    // Test verify lenient mode (empty ledger succeeds with warning)
    let out_verify = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(dir.path())
        .args(["git-hook", "--verify"])
        .output()
        .expect("run git-hook --verify");

    assert!(out_verify.status.success());
    let stdout = String::from_utf8_lossy(&out_verify.stdout);
    assert!(stdout.contains("Git / CI provenance verification passed"));
}

#[test]
fn test_git_hook_strict_fails_on_empty_ledger() {
    let dir = TempDir::new("strict_empty_test");

    let out_strict = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(dir.path())
        .args(["git-hook", "--verify", "--strict"])
        .output()
        .expect("run git-hook --verify --strict");

    assert!(
        !out_strict.status.success(),
        "Strict verification on empty ledger must fail"
    );
    let stderr = String::from_utf8_lossy(&out_strict.stderr);
    assert!(
        stderr.contains("Strict verification failed"),
        "Expected strict failure in stderr: {}",
        stderr
    );
}

#[test]
fn test_git_hook_verify_intact_chain_and_slsa_export() {
    let dir = TempDir::new("intact_chain_test");
    let axiom_dir = dir.path().join(".axiom");
    std::fs::create_dir_all(&axiom_dir).expect("create .axiom dir");

    // Create 2 chained records
    let r1 = ProvenanceAttestation::generate(axiom_proto::NewAttestation {
        parent_merkle_root: "root_parent_0",
        commit_merkle_root: "root_commit_1",
        agent_identity: "agent_alpha",
        prompt: "prompt 1",
        symbol_path: "auth::validate",
        ctop_task_id: "task_1",
        verified_by: "sandbox",
        verification_detail: "assert passed",
        previous_seal: "",
    });

    let r2 = ProvenanceAttestation::generate(axiom_proto::NewAttestation {
        parent_merkle_root: "root_commit_1",
        commit_merkle_root: "root_commit_2",
        agent_identity: "agent_beta",
        prompt: "prompt 2",
        symbol_path: "auth::token",
        ctop_task_id: "task_2",
        verified_by: "sandbox",
        verification_detail: "assert passed",
        previous_seal: &r1.seal,
    });

    let records = vec![r1, r2];
    let ledger_json = serde_json::to_string_pretty(&records).unwrap();
    std::fs::write(axiom_dir.join("attestations.json"), ledger_json).unwrap();

    let slsa_out = dir.path().join("slsa_gate_output.json");

    let out_verify = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(dir.path())
        .args([
            "git-hook",
            "--verify",
            "--strict",
            "--slsa",
            slsa_out.to_str().unwrap(),
        ])
        .output()
        .expect("run git-hook --verify");

    assert!(
        out_verify.status.success(),
        "Verification must succeed on intact chain: {}",
        String::from_utf8_lossy(&out_verify.stderr)
    );
    let stdout = String::from_utf8_lossy(&out_verify.stdout);
    assert!(
        stdout.contains("Verified 2 cryptographic attestation seal(s) in unbroken ledger chain")
    );
    assert!(stdout.contains("Exported 2 SLSA provenance statement(s)"));

    assert!(slsa_out.exists(), "SLSA provenance file must be created");
    let slsa_content = std::fs::read_to_string(&slsa_out).unwrap();
    let slsa_parsed: serde_json::Value = serde_json::from_str(&slsa_content).unwrap();
    assert_eq!(slsa_parsed.as_array().unwrap().len(), 2);
}

#[test]
fn test_git_hook_verify_tampered_ledger_fails() {
    let dir = TempDir::new("tampered_ledger_test");
    let axiom_dir = dir.path().join(".axiom");
    std::fs::create_dir_all(&axiom_dir).expect("create .axiom dir");

    // Create 2 records with broken link
    let r1 = ProvenanceAttestation::generate(axiom_proto::NewAttestation {
        parent_merkle_root: "root_parent_0",
        commit_merkle_root: "root_commit_1",
        agent_identity: "agent_alpha",
        prompt: "prompt 1",
        symbol_path: "auth::validate",
        ctop_task_id: "task_1",
        verified_by: "sandbox",
        verification_detail: "assert passed",
        previous_seal: "",
    });

    let mut r2 = ProvenanceAttestation::generate(axiom_proto::NewAttestation {
        parent_merkle_root: "root_commit_1",
        commit_merkle_root: "root_commit_2",
        agent_identity: "agent_beta",
        prompt: "prompt 2",
        symbol_path: "auth::token",
        ctop_task_id: "task_2",
        verified_by: "sandbox",
        verification_detail: "assert passed",
        previous_seal: &r1.seal,
    });
    // Deliberately corrupt the predecessor link
    r2.previous_seal = "blake3:tampered_predecessor_hash_invalid".to_string();

    let records = vec![r1, r2];
    let ledger_json = serde_json::to_string_pretty(&records).unwrap();
    std::fs::write(axiom_dir.join("attestations.json"), ledger_json).unwrap();

    let out_verify = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(dir.path())
        .args(["git-hook", "--verify"])
        .output()
        .expect("run git-hook --verify");

    assert!(
        !out_verify.status.success(),
        "Tampered chain must be rejected with failure"
    );
    let stderr = String::from_utf8_lossy(&out_verify.stderr);
    assert!(
        stderr.contains("Attestation ledger chain verification failed"),
        "Expected chain verification error: {}",
        stderr
    );
}

#[test]
fn test_git_hook_verify_trusted_key_validation() {
    let dir = TempDir::new("trusted_key_gate_test");
    let key_file = dir.path().join("test_signer.sec");
    let pub_file = dir.path().join("test_signer.pub");

    // 1. Generate keypair using axiom keygen
    let out_keygen = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .args(["keygen", "--out", key_file.to_str().unwrap()])
        .output()
        .expect("run keygen");
    assert!(out_keygen.status.success());
    assert!(pub_file.exists());

    let priv_hex = std::fs::read_to_string(&key_file).unwrap();
    let pub_hex = std::fs::read_to_string(&pub_file).unwrap();

    let axiom_dir = dir.path().join(".axiom");
    std::fs::create_dir_all(&axiom_dir).expect("create .axiom dir");

    let mut r1 = ProvenanceAttestation::generate(axiom_proto::NewAttestation {
        parent_merkle_root: "root_0",
        commit_merkle_root: "root_1",
        agent_identity: "agent_alpha",
        prompt: "prompt 1",
        symbol_path: "auth::validate",
        ctop_task_id: "task_1",
        verified_by: "sandbox",
        verification_detail: "assert passed",
        previous_seal: "",
    });

    r1.sign_with("auth::validate", "prompt 1", &priv_hex)
        .unwrap();

    let records = vec![r1];
    let ledger_json = serde_json::to_string_pretty(&records).unwrap();
    std::fs::write(axiom_dir.join("attestations.json"), ledger_json).unwrap();

    // Verify with matching trusted key -> SUCCESS
    let out_valid = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(dir.path())
        .args([
            "git-hook",
            "--verify",
            "--strict",
            "--trusted-key",
            pub_hex.trim(),
        ])
        .output()
        .expect("run git-hook --verify with trusted key");

    assert!(
        out_valid.status.success(),
        "Must pass when trusted key matches: {}",
        String::from_utf8_lossy(&out_valid.stderr)
    );

    // Verify with mismatched trusted key -> FAILURE
    let fake_key = "0000000000000000000000000000000000000000000000000000000000000000";
    let out_mismatch = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(dir.path())
        .args([
            "git-hook",
            "--verify",
            "--strict",
            "--trusted-key",
            fake_key,
        ])
        .output()
        .expect("run git-hook --verify with mismatched key");

    assert!(
        !out_mismatch.status.success(),
        "Must fail when trusted key does not match"
    );
    let stderr = String::from_utf8_lossy(&out_mismatch.stderr);
    assert!(
        stderr.contains("signer mismatch"),
        "Expected signer mismatch: {}",
        stderr
    );
}
