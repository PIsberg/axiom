//! Where a symbol's source sits in a file as it reads now.
//!
//! `axiom_apply_mutation` writes a symbol's new text over its old lines, so the
//! extent decides which lines of somebody's file are replaced. An extent that
//! stops short leaves the old body under the new one; one that runs long eats
//! the next function. So it is found in the text being written, not taken from
//! the index, whose line numbers date from the last scan and move as soon as
//! anyone edits the file, and it refuses rather than guesses when the shape is
//! one it cannot close.
//!
//! `body_span`, which the index hashes, is not reused: it follows indentation
//! when the declaration line opens no brace, which is right for Python and
//! wrong for a rustfmt-style signature wrapped over several lines, where it
//! stops at the parameters and leaves out the body.

use axiom_ast::AstIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("axiom-extent-{}-{}-{}", tag, std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test directory");
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

/// Index `content` as `name`, then locate `symbol` in `now`, which is the same
/// file as somebody may have changed it since, and return the lines found.
fn located(name: &str, content: &str, symbol: &str, now: &str) -> Result<String, String> {
    let dir = TempDir::new("locate");
    std::fs::write(dir.path().join(name), content).expect("write the fixture");
    let index = AstIndex::new();
    index.scan_directory(dir.path()).expect("scan");
    let (start, end) = index.locate_source(symbol, now)?;
    Ok(now.lines().collect::<Vec<_>>()[start..end].join("\n"))
}

#[test]
fn a_rust_function_runs_from_its_declaration_to_its_closing_brace() {
    let src = "fn before() {}\n\n/// Doc stays outside.\n#[inline]\nfn target(x: u32) -> u32 {\n    let s = \"}\";\n    if x > 1 {\n        x\n    } else {\n        0\n    }\n}\n\nfn after() {}\n";
    let got = located("lib.rs", src, "target", src).unwrap();
    assert_eq!(
        got,
        "fn target(x: u32) -> u32 {\n    let s = \"}\";\n    if x > 1 {\n        x\n    } else {\n        0\n    }\n}",
        "a brace inside a string literal must not close the body"
    );
}

#[test]
fn a_signature_wrapped_over_several_lines_keeps_its_body() {
    let src = "fn target(\n    first: u32,\n    second: u32,\n) -> u32 {\n    first + second\n}\n\nfn after() {}\n";
    let got = located("lib.rs", src, "target", src).unwrap();
    assert_eq!(
        got, "fn target(\n    first: u32,\n    second: u32,\n) -> u32 {\n    first + second\n}",
        "stopping at the parameters would leave the old body under the new one"
    );
}

#[test]
fn the_extent_is_found_in_the_file_as_it_reads_now() {
    let scanned = "fn target() {\n    one();\n}\n";
    let now = "fn added_since() {\n    x();\n}\n\nfn target() {\n    one();\n    two();\n}\n";
    let got = located("lib.rs", scanned, "target", now).unwrap();
    assert_eq!(got, "fn target() {\n    one();\n    two();\n}");
}

#[test]
fn a_python_function_runs_while_the_indentation_is_deeper() {
    let src = "@decorator\ndef target(a,\n           b):\n    x = {\n        'k': 1,\n    }\n    return x\n\n\ndef after():\n    pass\n";
    let got = located("mod.py", src, "target", src).unwrap();
    assert_eq!(
        got, "def target(a,\n           b):\n    x = {\n        'k': 1,\n    }\n    return x",
        "trailing blank lines belong to the gap, not the function"
    );
}

#[test]
fn a_kotlin_expression_body_ends_where_its_expression_does() {
    let src = "package p\n\nclass Gate {\n    fun isOpen(depth: Int): Boolean = listOf(\n        depth,\n    ).isNotEmpty()\n\n    fun after() {}\n}\n";
    let got = located("Gate.kt", src, "isOpen", src).unwrap();
    assert_eq!(
        got,
        "    fun isOpen(depth: Int): Boolean = listOf(\n        depth,\n    ).isNotEmpty()"
    );
}

#[test]
fn a_java_method_with_its_brace_on_the_next_line_is_found_whole() {
    let src = "package p;\n\npublic class A {\n    public int target(int x)\n    {\n        return x;\n    }\n\n    public void after() {}\n}\n";
    let got = located("A.java", src, "target", src).unwrap();
    assert_eq!(
        got,
        "    public int target(int x)\n    {\n        return x;\n    }"
    );
}

#[test]
fn a_symbol_gone_from_the_file_is_refused() {
    let scanned = "fn target() {}\n";
    let now = "fn renamed() {}\n";
    let err = located("lib.rs", scanned, "target", now).unwrap_err();
    assert!(err.contains("not declared"), "{err}");
}

#[test]
fn a_symbol_declared_twice_is_refused_rather_than_picked() {
    let src =
        "#[cfg(unix)]\nfn target() {\n    a();\n}\n\n#[cfg(windows)]\nfn target() {\n    b();\n}\n";
    let err = located("lib.rs", src, "target", src).unwrap_err();
    assert!(err.contains("declared 2 times"), "{err}");
}

#[test]
fn a_body_that_never_closes_is_refused() {
    let scanned = "fn target() {\n    a();\n}\n";
    let now = "fn target() {\n    a();\n";
    let err = located("lib.rs", scanned, "target", now).unwrap_err();
    assert!(err.contains("where"), "{err}");
}
