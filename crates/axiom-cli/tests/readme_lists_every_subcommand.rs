//! Every subcommand the binary offers must appear in the README's command table.
//!
//! `cache-validate` shipped and the table kept listing `cache-audit` alone, so
//! the one command that checks the blast radius against a real test run, rather
//! than against the graph's opinion of itself, was undiscoverable. The table
//! also carried `axiom demo` with no hint that it answers from a seeded fixture.
//!
//! The direction matters: clap is the source of truth and the README is checked
//! against it. A README that lists a command the binary does not have is the
//! other failure, and the second assertion below catches that one.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

/// Subcommands that are deliberately absent from the user-facing table.
const NOT_IN_THE_TABLE: &[&str] = &["help"];

fn readme() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the crate must sit two levels below the repository root")
        .join("README.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn subcommands() -> BTreeSet<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .arg("--help")
        .output()
        .expect("failed to run the axiom binary");
    let help = String::from_utf8_lossy(&out.stdout).into_owned();

    let mut names = BTreeSet::new();
    let mut inside = false;
    for line in help.lines() {
        if line.trim_start().starts_with("Commands:") {
            inside = true;
            continue;
        }
        if !inside || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with("  ") {
            break;
        }
        let indent = line.len() - line.trim_start().len();
        if indent > 2 {
            continue; // a wrapped description line
        }
        let name = line.split_whitespace().next().unwrap_or("");
        if !name.is_empty() && !NOT_IN_THE_TABLE.contains(&name) {
            names.insert(name.to_string());
        }
    }
    assert!(
        names.len() > 10,
        "expected the full subcommand table, parsed {names:?}"
    );
    names
}

/// The table entries, as the command word each row starts with.
fn documented() -> BTreeSet<String> {
    readme()
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("| `axiom ")?;
            let word = rest.split([' ', '`', '|']).next()?;
            (!word.is_empty()).then(|| word.to_string())
        })
        .collect()
}

#[test]
fn the_readme_lists_every_subcommand() {
    let shipped = subcommands();
    let documented = documented();

    let missing: Vec<&String> = shipped
        .iter()
        .filter(|c| !documented.contains(*c))
        .collect();
    assert!(
        missing.is_empty(),
        "the README command table is missing these subcommands: {missing:?}. \
         A command nobody can find is a command nobody runs."
    );
}

#[test]
fn the_readme_lists_no_subcommand_that_does_not_exist() {
    let shipped = subcommands();
    let documented = documented();

    let invented: Vec<&String> = documented
        .iter()
        .filter(|c| !shipped.contains(*c))
        .collect();
    assert!(
        invented.is_empty(),
        "the README command table lists subcommands the binary does not have: \
         {invented:?}. Documenting a command that was renamed or removed sends \
         a reader to a non-zero exit."
    );
}
