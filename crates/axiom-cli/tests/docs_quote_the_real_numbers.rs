//! Every measured number in the docs that can be re-derived, re-derived (#69).
//!
//! PR #67 guarded the parts of the docs that are lists: the tool names, the
//! subcommand table. It guarded no number, and numbers are how this set went
//! stale before: "138 tests", "189 tests", "459 files, 2,219 tests", "154
//! non-test symbols against 49 tests". Every one was true when it was written.
//! The README was quoting "236 tests across 53 test binaries" when this file was
//! added, against a tree that had 278 and 54.
//!
//! Code is the source and the docs are checked against it, never the reverse,
//! which is the same rule `docs_name_every_tool` and `readme_lists_every_
//! subcommand` follow.
//!
//! What is deliberately not pinned here: the derived statistics in
//! `docs/USAGE_GUIDE.md`, the means, medians and Jaccard overlap that
//! `.github/scripts/blast_radius_stats.py` produces. Re-deriving those costs one
//! subprocess per symbol, several minutes, and the script already exists to
//! produce them on demand. What is pinned instead is the population they were
//! measured over, the symbol and test counts, because when that moves the
//! statistics are stale and the sweep needs re-running. A stale population is
//! the signal; the numbers themselves are the sweep's job.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/axiom-cli.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the workspace root is two levels above this crate")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Every `.rs` file in the workspace, sources and tests alike.
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

/// Count the test attributes a file really declares.
///
/// Comments and string literals are stripped first, with axiom's own stripper,
/// because this repository writes Rust fixtures inside string literals
/// constantly: 28 of the 306 raw matches are fixtures a parser test feeds to the
/// indexer, or prose in a doc comment naming the attribute. Counting the raw
/// text would have reported 306 and pinned a number that is not the suite.
///
/// This is the same mistake the indexer made and was fixed for, one layer up,
/// which is why the fix is to call the same function rather than to write
/// another approximation of it.
fn declared_tests(source: &str) -> usize {
    let clean = AstIndex::strip_comments_and_strings(source, false);
    clean
        .lines()
        .filter(|line| {
            let t = line.trim();
            t == "#[test]" || t == "#[tokio::test]"
        })
        .count()
}

struct Suite {
    test_functions: usize,
    integration_files: usize,
    e2e_tests: usize,
}

fn measure() -> Suite {
    let root = repo_root();
    let mut files = Vec::new();
    rust_sources(&root.join("crates"), &mut files);

    let test_functions = files.iter().map(|f| declared_tests(&read_abs(f))).sum();

    let integration_files = files
        .iter()
        .filter(|f| {
            f.components().any(|c| c.as_os_str() == "tests") && declared_tests(&read_abs(f)) > 0
        })
        .count();

    let e2e_tests = declared_tests(&read("crates/axiom-cli/tests/e2e_test.rs"));

    Suite {
        test_functions,
        integration_files,
        e2e_tests,
    }
}

fn read_abs(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// The README's "Running Tests" paragraph quotes three numbers.
#[test]
fn the_readme_quotes_the_real_suite_size() {
    let suite = measure();
    let readme = read("README.md");

    // The section, not the whole file. Searching the file for "277" would pass
    // on any stray occurrence of those digits, which is a guard that cannot go
    // red for the reason it exists.
    let heading = "## Running Tests";
    let start = readme
        .find(heading)
        .expect("README.md has a Running Tests section");
    let rest = &readme[start + heading.len()..];
    let section = match rest.find(
        "
## ",
    ) {
        Some(end) => &rest[..end],
        None => rest,
    };

    for (number, what) in [
        (suite.test_functions, "test functions in the workspace"),
        (suite.integration_files, "integration test files"),
        (
            suite.e2e_tests,
            "tests in crates/axiom-cli/tests/e2e_test.rs",
        ),
    ] {
        assert!(
            section.contains(&number.to_string()),
            "the README's Running Tests paragraph does not quote {number}, the \
             number of {what}. Counted from the source tree: {} test functions \
             across {} integration test files, {} of them end-to-end. Update the \
             paragraph; this number has gone stale three times.",
            suite.test_functions,
            suite.integration_files,
            suite.e2e_tests
        );
    }
}

/// The population the blast-radius statistics were measured over.
///
/// `docs/USAGE_GUIDE.md` quotes "N non-test symbols against M tests" beside the
/// means and medians derived from them. The statistics cannot be re-derived
/// cheaply; the population can, from the same index the sweep would have used.
/// When it moves, the numbers beside it are describing a tree that no longer
/// exists.
#[test]
fn the_usage_guide_quotes_the_population_the_statistics_came_from() {
    let root = repo_root();
    let index = AstIndex::new();
    index
        .scan_directory(&root)
        .expect("scanning this repository");

    let tests = index.total_tests_count();
    let non_tests = index.total_symbols_count() - tests;

    let guide = read("docs/USAGE_GUIDE.md");
    assert!(
        guide.contains(&format!(
            "{non_tests} non-test symbols against {tests} tests"
        )),
        "docs/USAGE_GUIDE.md quotes a blast-radius population that this tree no \
         longer has. Scanned now: {non_tests} non-test symbols against {tests} \
         tests. The statistics beside that phrase were derived from the old \
         population, so re-run\n  \
         axiom scan --path . && python .github/scripts/blast_radius_stats.py\n  \
         and quote what it prints, rather than editing the population alone."
    );
}
