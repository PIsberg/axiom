//! Finding out what really breaks when a symbol changes.
//!
//! Everything the cache audit reports compares two readings of one dependency
//! graph. When they agree, that is not evidence about the code: a call the
//! parsers never recorded is invisible to the forward walk and the reverse walk
//! alike, so both are confidently wrong together and the audit reports a zero.
//!
//! The only way past that is to stop asking the graph. Change a symbol, run the
//! project's own tests, and see which ones fail. A test that fails while its key
//! did not move is a proven hole, not a suspected one, and a test that fails
//! while the blast radius did not select it is a hole in the feature that
//! already ships.
//!
//! This edits files in place and restores them. `Restore` does it from `Drop`,
//! so a panic between writing and restoring still puts the tree back.

use std::path::{Path, PathBuf};

/// A source file to put back the way it was found.
pub struct Restore {
    path: PathBuf,
    original: String,
    done: bool,
}

impl Restore {
    pub fn write(path: &Path, mutated: &str) -> std::io::Result<Self> {
        let original = std::fs::read_to_string(path)?;
        std::fs::write(path, mutated)?;
        Ok(Self {
            path: path.to_path_buf(),
            original,
            done: false,
        })
    }

    /// Put the file back and say whether it worked.
    ///
    /// Restoring is the one step that must not fail quietly: leaving a mutation
    /// behind corrupts the working tree of whoever ran this.
    pub fn restore(mut self) -> std::io::Result<()> {
        self.done = true;
        std::fs::write(&self.path, &self.original)
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        if !self.done {
            if let Err(e) = std::fs::write(&self.path, &self.original) {
                eprintln!(
                    "FAILED TO RESTORE {}: {e}\nThe file still holds a mutation. \
                     Restore it from version control before doing anything else.",
                    self.path.display()
                );
            }
        }
    }
}

/// A source edit that changes behaviour without changing whether it compiles.
///
/// The distinction matters. A mutation that does not compile fails every test at
/// once, which says nothing about any dependency; the caller has to be able to
/// tell that case apart and throw it away. These are chosen to keep the shape of
/// the code and flip its meaning.
const SWAPS: &[(&str, &str)] = &[
    (" >= ", " <= "),
    (" <= ", " >= "),
    (" > ", " < "),
    (" < ", " > "),
    (" == ", " != "),
    (" != ", " == "),
    (" && ", " || "),
    (" || ", " && "),
    ("true", "false"),
    ("false", "true"),
    ("True", "False"),
    ("False", "True"),
];

/// Where a symbol's own lines are, found in the file rather than taken from
/// `AstNode::source_range`.
///
/// `source_range` brackets the declaration, not the body, so it cannot say
/// where a symbol ends; and it is a position in the file as it was when it was
/// scanned, which is not necessarily the file being mutated now. Reading it as
/// a body range is what produced the bug this module exists because of: it
/// used to hold `(0, content.len())`, the length of a signature, so the mutator
/// edited from line 0 to line `signature.len()`, which on a short file is the
/// whole thing. A mutation attributed to `unrelated` actually broke `is_open`,
/// and the run reported a dependency hole that did not exist. Attribution is
/// the entire value of this tool, so it locates the symbol itself.
///
/// The declaration line is matched by shape, then the body is taken by brace
/// balance, or by indentation where the declaration opens no brace, which is
/// how Python and expression-bodied Kotlin and Scala look.
fn symbol_lines(lines: &[&str], short_name: &str) -> Option<(usize, usize)> {
    if short_name.is_empty() {
        return None;
    }

    let decl_keywords = [
        format!("fn {short_name}"),
        format!("def {short_name}"),
        format!("func {short_name}"),
        format!("fun {short_name}"),
        format!("function {short_name}"),
        format!("class {short_name}"),
        format!("struct {short_name}"),
        format!("object {short_name}"),
        format!("trait {short_name}"),
        format!("interface {short_name}"),
        format!("enum {short_name}"),
    ];
    let start = lines.iter().position(|l| {
        let t = l.trim_start();
        if t.starts_with("//") || t.starts_with('#') || t.starts_with('*') {
            return false;
        }
        if decl_keywords.iter().any(|kw| t.contains(kw)) {
            return true;
        }
        // Method or constructor declaration fallback (e.g. Java "public int name(" or "ClassName(")
        if t.contains(&format!("{short_name}(")) || t.contains(&format!("{short_name}<")) {
            let is_call_site = t.starts_with("let ")
                || t.starts_with("return ")
                || t.starts_with("if ")
                || t.starts_with("while ")
                || t.starts_with("match ")
                || t.starts_with("for ")
                || t.starts_with("assert")
                || t.contains(&format!(".{short_name}("));
            return !is_call_site;
        }
        false
    })?;

    let decl = lines[start];
    let indent = decl.len() - decl.trim_start().len();
    let opens = decl.matches('{').count();
    let closes = decl.matches('}').count();

    if opens > closes {
        let mut depth = opens - closes;
        for (offset, line) in lines.iter().enumerate().skip(start + 1) {
            depth += line.matches('{').count();
            depth = depth.saturating_sub(line.matches('}').count());
            if depth == 0 {
                return Some((start, offset + 1));
            }
        }
        return Some((start, lines.len()));
    }

    // No brace opened here: the body is whatever follows at a deeper
    // indentation, which covers Python and `fun f() = expr`.
    let mut end = start + 1;
    while end < lines.len() {
        let line = lines[end];
        if !line.trim().is_empty() {
            let this_indent = line.len() - line.trim_start().len();
            if this_indent <= indent {
                break;
            }
        }
        end += 1;
    }
    Some((start, end.max(start + 1)))
}

/// Apply the first swap that matches, within the symbol's own lines.
///
/// Restricted to the symbol's own body so the failure can be attributed to it.
/// Editing anywhere else in the file would mean the tests that broke might have
/// broken because of a different symbol entirely, which is exactly the false
/// accusation that `source_range` produced.
pub fn mutate_lines(content: &str, short_name: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = content.lines().collect();
    let (start, end) = symbol_lines(&lines, short_name)?;

    for (from, to) in SWAPS {
        for i in start..end {
            let line = lines[i];
            // Comment lines change nothing when edited, so a mutation there is
            // an equivalent mutant by construction and wastes a whole test run.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with('*') {
                continue;
            }
            if !line.contains(from) {
                continue;
            }
            let mut out = lines.clone();
            let replaced = line.replacen(from, to, 1);
            out[i] = &replaced;
            let joined = out.join("\n");
            let description = format!("line {}: {from:?} -> {to:?}", i + 1);
            return Some((joined + "\n", description));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::mutate_lines;

    const SOURCE: &str = "pub fn is_open(depth: i32) -> bool {\n    depth > 0\n}\n\npub fn unrelated() -> i32 {\n    7\n}\n";

    /// The bug this module exists because of.
    ///
    /// `unrelated` has nothing to swap in its own body, so the honest answer is
    /// that it cannot be mutated. Locating it by `source_range` back when that
    /// field held `(0, signature length)` edited from line 0 to line 27
    /// instead, hit `depth > 0` inside `is_open`, and produced a run blaming
    /// `unrelated` for breaking a test it does not touch.
    #[test]
    fn a_mutation_never_escapes_the_symbol_it_is_attributed_to() {
        let mutated = mutate_lines(SOURCE, "unrelated");
        assert!(
            mutated.is_none(),
            "there is nothing swappable inside unrelated, so no mutation may be \
             claimed for it: {mutated:?}"
        );
    }

    #[test]
    fn a_symbol_with_a_swappable_line_is_mutated_in_place() {
        let (mutated, description) = mutate_lines(SOURCE, "is_open").expect("is_open has one");

        assert!(mutated.contains("depth < 0"), "{mutated}");
        assert!(
            mutated.contains("pub fn unrelated() -> i32 {") && mutated.contains("    7"),
            "the other symbol must be untouched: {mutated}"
        );
        assert!(description.contains("line 2"), "{description}");
    }

    /// Python and expression-bodied Kotlin and Scala open no brace, so the body
    /// has to be found by indentation instead.
    #[test]
    fn a_body_with_no_brace_is_bounded_by_indentation() {
        let source =
            "def compute(items):\n    return len(items) > 0\n\ndef other():\n    return True\n";
        let (mutated, _) = mutate_lines(source, "compute").expect("compute has one");

        assert!(mutated.contains("return len(items) < 0"), "{mutated}");
        assert!(
            mutated.contains("return True"),
            "the next function must be untouched: {mutated}"
        );
    }

    #[test]
    fn a_generic_symbol_is_found_and_call_site_is_ignored() {
        let source = "fn caller() {\n    let x = is_open(10);\n}\n\npub fn is_open<T: Ord>(val: T) -> bool {\n    val > 0\n}\n";
        let (mutated, _) = mutate_lines(source, "is_open").expect("is_open has one");
        assert!(mutated.contains("val < 0"), "{mutated}");
        assert!(mutated.contains("let x = is_open(10);"), "{mutated}");
    }

    #[test]
    fn reverse_comparisons_and_python_booleans_are_swapped() {
        let rs_source = "pub fn check(x: i32) -> bool {\n    x <= 10 && false\n}\n";
        let (mutated_rs, _) = mutate_lines(rs_source, "check").expect("check has one");
        assert!(mutated_rs.contains("x >= 10"), "{mutated_rs}");

        let py_source = "def is_ready():\n    return True\n";
        let (mutated_py, _) = mutate_lines(py_source, "is_ready").expect("is_ready has one");
        assert!(mutated_py.contains("return False"), "{mutated_py}");
    }
}

#[cfg(test)]
mod output_tests {
    /// Cargo's summary line begins with "test " and contains FAILED, exactly as
    /// a failing test does. Counting it invented a failing test called
    /// "result:", which accused nobody, since no symbol answers to that name,
    /// and inflated the number a reader uses to judge how much ground truth a
    /// run produced.
    #[test]
    fn the_summary_line_is_not_a_failing_test() {
        let output = "\
test test_totally_unrelated ... ok
test test_gate_opens ... FAILED
test result: FAILED. 1 passed; 1 failed; 0 ignored
";
        let names = crate::failing_test_names(output);
        assert_eq!(names, vec!["test_gate_opens".to_string()], "{names:?}");
    }

    #[test]
    fn a_passing_run_reports_nothing() {
        let output = "test a ... ok\ntest b ... ok\ntest result: ok. 2 passed; 0 failed\n";
        assert!(crate::failing_test_names(output).is_empty());
    }
}
