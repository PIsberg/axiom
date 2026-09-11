//! A green local gate run does not type-check a file gated to another platform.
//!
//! `cargo test --all-targets` and `cargo clippy --all-targets` both compile only
//! what the host target selects, so a test file carrying a file-level
//! `#![cfg(unix)]` is invisible on Windows: not run, and not type-checked
//! either. Cfg-gated code is skipped before type checking, so moving the
//! attribute onto the individual functions would not help.
//!
//! That is not hypothetical. #83 was pushed after all three gates came back
//! green on Windows, and CI failed the ubuntu test job and the lint job on
//!
//! ```text
//! error[E0277]: `Finished` doesn't implement `Debug`
//!   --> crates/axiom-vmm/tests/spawn_retry.rs:125:40
//! ```
//!
//! CI catches it, because the ubuntu job compiles the file. What was missing is
//! that the person running the gates locally had no way to know their green was
//! partial. So CLAUDE.md says which files that applies to, and this pins the
//! list against the source tree rather than against prose, because a list
//! copied into a document drifts silently and stays green.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/axiom-cli.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name != "target" && !name.starts_with('.') {
                rust_sources(&path, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Files whose whole contents are gated to a platform, as repo-relative paths
/// with forward slashes, so the comparison reads the same on every host.
fn platform_gated_files() -> Vec<String> {
    let root = repo_root();
    let mut files = Vec::new();
    rust_sources(&root.join("crates"), &mut files);

    let mut gated: Vec<String> = files
        .iter()
        .filter(|path| {
            let Ok(body) = std::fs::read_to_string(path) else {
                return false;
            };
            // A file-level inner attribute, which gates the entire file. An
            // outer `#[cfg(...)]` on one item is a different thing and is not
            // what makes a whole file invisible.
            body.lines().any(|line| {
                let line = line.trim_start();
                line.starts_with("#![cfg(")
                    && ["unix", "windows", "target_os", "target_family"]
                        .iter()
                        .any(|p| line.contains(p))
            })
        })
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    gated.sort();
    gated
}

#[test]
fn claude_md_names_every_platform_gated_test_file() {
    let gated = platform_gated_files();
    assert!(
        !gated.is_empty(),
        "no platform-gated file was found; if the last one was removed, drop this \
         guard and the paragraph in CLAUDE.md it pins, rather than leaving both \
         passing vacuously"
    );

    let claude_md = std::fs::read_to_string(repo_root().join("CLAUDE.md")).expect("CLAUDE.md");
    let missing: Vec<&String> = gated.iter().filter(|f| !claude_md.contains(*f)).collect();

    assert!(
        missing.is_empty(),
        "CLAUDE.md does not name these platform-gated files: {missing:?}. A local \
         gate run on another host compiles none of them, so anyone trusting a \
         green run needs to be told which files it did not cover."
    );
}

/// The warning itself has to be there, not just the path.
///
/// The path alone could appear in an unrelated sentence and satisfy the check
/// above while telling a reader nothing.
#[test]
fn claude_md_says_what_a_platform_gated_file_costs() {
    let claude_md = std::fs::read_to_string(repo_root().join("CLAUDE.md")).expect("CLAUDE.md");
    assert!(
        claude_md.contains("does not type-check"),
        "CLAUDE.md's Gates section has to say that a host gate run does not \
         type-check the platform-gated files, not merely mention them"
    );
}
