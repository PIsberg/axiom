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

    // Test verify
    let out_verify = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .current_dir(dir.path())
        .args(["git-hook", "--verify"])
        .output()
        .expect("run git-hook --verify");

    assert!(out_verify.status.success());
    let stdout = String::from_utf8_lossy(&out_verify.stdout);
    assert!(stdout.contains("Git pre-commit verification passed"));
}
