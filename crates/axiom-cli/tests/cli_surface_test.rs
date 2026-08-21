//! Tests for the command-line surface itself, by running the real binary.
//!
//! The rest of the suite drives the server in-process, which means nothing ever
//! looked at what a user sees when they type `axiom --help`. That let a doc
//! comment attach to the wrong subcommand: `keygen` inherited the description of
//! `watch`, and `watch` was listed with no description at all.

use std::process::Command;

fn run(args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .args(args)
        .output()
        .expect("failed to run the axiom binary");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// Every subcommand listed in `--help` must carry its own description.
///
/// A blank description means a doc comment went missing; a description that
/// belongs to a different subcommand means one was captured by its neighbour.
#[test]
fn test_cli_every_subcommand_has_its_own_description() {
    let (stdout, _, ok) = run(&["--help"]);
    assert!(ok, "`axiom --help` should exit successfully");

    let commands = parse_command_table(&stdout);
    assert!(
        commands.len() > 10,
        "expected the full subcommand table, parsed {} entries from:\n{}",
        commands.len(),
        stdout
    );

    for (name, description) in &commands {
        assert!(
            !description.trim().is_empty(),
            "subcommand `{name}` is listed with no description; \
             its doc comment was probably captured by the subcommand above it"
        );
    }

    // The specific collision that motivated this test: `watch`'s description
    // ended up on `keygen`, because `Keygen` was inserted between the doc
    // comment and the `Watch` variant it belonged to.
    let keygen = lookup(&commands, "keygen");
    assert!(
        !keygen.to_lowercase().contains("watch"),
        "`keygen` is described as a watch command, so it has absorbed \
         `watch`'s doc comment: {keygen}"
    );
    let watch = lookup(&commands, "watch");
    assert!(
        watch.to_lowercase().contains("watch") || watch.to_lowercase().contains("re-index"),
        "`watch` should describe watching the filesystem, got: {watch}"
    );
}

/// `axiom --version` must report a version, the way every other CLI does.
#[test]
fn test_cli_reports_its_version() {
    let (stdout, stderr, ok) = run(&["--version"]);
    assert!(
        ok,
        "`axiom --version` should exit successfully, stderr was:\n{stderr}"
    );
    let reported = stdout.trim();
    assert!(
        reported.contains(env!("CARGO_PKG_VERSION")),
        "expected `--version` to report {}, got {reported:?}",
        env!("CARGO_PKG_VERSION")
    );
}

/// Parse clap's `Commands:` table into (name, description) pairs.
///
/// Entries start with exactly two spaces; clap indents wrapped continuation
/// lines further, so those are folded into the preceding description.
fn parse_command_table(help: &str) -> Vec<(String, String)> {
    let mut commands: Vec<(String, String)> = Vec::new();
    let mut inside = false;

    for line in help.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("Commands:") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if trimmed.trim().is_empty() {
            continue;
        }
        // A new section header (`Options:`) ends the table.
        if !trimmed.starts_with("  ") {
            break;
        }

        let indent = trimmed.len() - trimmed.trim_start().len();
        let body = trimmed.trim_start();
        if indent > 2 && !commands.is_empty() {
            // Continuation of the previous description.
            let last = commands.last_mut().expect("checked non-empty");
            if last.1.is_empty() {
                last.1 = body.to_string();
            } else {
                last.1.push(' ');
                last.1.push_str(body);
            }
            continue;
        }

        let (name, rest) = match body.split_once(char::is_whitespace) {
            Some((n, r)) => (n, r.trim()),
            None => (body, ""),
        };
        commands.push((name.to_string(), rest.to_string()));
    }

    commands
}

fn lookup<'a>(commands: &'a [(String, String)], name: &str) -> &'a str {
    commands
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, d)| d.as_str())
        .unwrap_or_else(|| panic!("`{name}` is missing from the subcommand table"))
}
