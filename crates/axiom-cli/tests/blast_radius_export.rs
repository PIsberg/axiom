//! `axiom blast-radius --format dot|json` hands the graph to something that
//! can draw it.
//!
//! The text output lists the tests a symbol reaches, which answers "what do I
//! run" and nothing about why. Checking whether the graph missed an edge needs
//! the paths, and a list of forty test names does not show a path. The JSON is
//! the tool's own answer, `causal_paths` included; the DOT turns those paths
//! into edges Graphviz can lay out.
//!
//! What must hold is that the picture is the answer: every test the text
//! output would list is a node, and every one of them is reachable from the
//! symbol along the drawn edges. A DOT file with a test floating unconnected
//! would look like a finding about the graph when it is a bug in the export.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_blast_export_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(d.join("src")).expect("temp dir");
    std::fs::write(
        d.join("src").join("lib.rs"),
        "pub fn helper(x: u32) -> u32 {\n    x + 1\n}\n\n\
         pub fn middle(x: u32) -> u32 {\n    helper(x) * 2\n}\n\n\
         #[cfg(test)]\nmod tests {\n    use super::*;\n\n\
         \x20   #[test]\n    fn helper_adds_one() {\n        assert_eq!(helper(1), 2);\n    }\n\n\
         \x20   #[test]\n    fn middle_doubles() {\n        assert_eq!(middle(1), 4);\n    }\n}\n",
    )
    .unwrap();
    let out = axiom(&d, &["scan", "--path", "."]);
    assert!(out.status.success(), "scan failed: {out:?}");
    d
}

fn axiom(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_axiom"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run axiom")
}

fn stdout(out: &std::process::Output) -> String {
    assert!(out.status.success(), "axiom failed: {out:?}");
    String::from_utf8(out.stdout.clone()).expect("utf-8")
}

/// Node ids and edges out of the DOT text, read the way the export writes
/// them: one statement per line, ids double-quoted.
fn parse_dot(dot: &str) -> (BTreeMap<String, String>, BTreeSet<(String, String)>) {
    let quoted = |s: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '"' {
                let mut id = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => id.push(chars.next().unwrap_or('\\')),
                        '"' => break,
                        c => id.push(c),
                    }
                }
                out.push(id);
            }
        }
        out
    };
    let mut nodes = BTreeMap::new();
    let mut edges = BTreeSet::new();
    for line in dot.lines().map(str::trim) {
        if !line.starts_with('"') {
            continue;
        }
        let ids = quoted(line);
        if line.contains("->") {
            edges.insert((ids[0].clone(), ids[1].clone()));
        } else {
            nodes.insert(ids[0].clone(), line.to_string());
        }
    }
    (nodes, edges)
}

#[test]
fn every_test_listed_is_a_node_reachable_from_the_symbol() {
    let dir = workspace();
    let answer: serde_json::Value = serde_json::from_str(&stdout(&axiom(
        &dir,
        &[
            "blast-radius",
            "--symbol",
            "helper",
            "--depth",
            "2",
            "--format",
            "json",
        ],
    )))
    .expect("--format json prints the tool's answer as JSON");
    let symbol = answer["symbol"].as_str().expect("symbol").to_string();
    let tests: Vec<String> = answer["impacted_tests"]
        .as_array()
        .expect("impacted_tests")
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    assert!(
        tests.len() >= 2,
        "the fixture has a direct and an indirect test: {answer}"
    );

    let dot = stdout(&axiom(
        &dir,
        &[
            "blast-radius",
            "--symbol",
            "helper",
            "--depth",
            "2",
            "--format",
            "dot",
        ],
    ));
    assert!(dot.trim_start().starts_with("digraph"), "{dot}");
    assert!(dot.trim_end().ends_with('}'), "{dot}");

    let (nodes, edges) = parse_dot(&dot);
    assert!(
        nodes.contains_key(&symbol),
        "the symbol is not a node:\n{dot}"
    );
    for (from, to) in &edges {
        assert!(
            nodes.contains_key(from) && nodes.contains_key(to),
            "edge {from} -> {to} names an undeclared node:\n{dot}"
        );
    }

    let mut reached = BTreeSet::from([symbol.clone()]);
    let mut queue = VecDeque::from([symbol.clone()]);
    while let Some(at) = queue.pop_front() {
        for (from, to) in &edges {
            if from == &at && reached.insert(to.clone()) {
                queue.push_back(to.clone());
            }
        }
    }
    for test in &tests {
        assert!(
            nodes.get(test).is_some_and(|n| n.contains("ellipse")),
            "{test} is not drawn as a test:\n{dot}"
        );
        assert!(
            reached.contains(test),
            "{test} is listed but no drawn path leads to it from {symbol}:\n{dot}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_format_is_refused() {
    let dir = workspace();
    let out = axiom(
        &dir,
        &["blast-radius", "--symbol", "helper", "--format", "svg"],
    );
    assert!(!out.status.success(), "svg is not a format: {out:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
