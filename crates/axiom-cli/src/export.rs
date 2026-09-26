//! The blast radius as a graph something else can draw.
//!
//! `axiom blast-radius` prints the tests a change reaches. Whether the graph
//! missed an edge is a question about the paths, not the list, so this writes
//! the tool's `causal_paths` out as Graphviz DOT: the symbol, every symbol a
//! path passes through, and every test, joined in the direction a change
//! travels.
//!
//! Only tests within the requested depth are drawn, the same set the text
//! output lists, so the picture and the list answer one question.

use serde_json::Value;
use std::collections::BTreeSet;

/// The path from the symbol to each test within the requested depth, in the
/// order the answer lists the tests, each running from the symbol to the test.
///
/// A test whose path is missing or does not start at the symbol is joined to
/// it directly, so nothing listed is ever drawn unconnected: a floating node
/// would read as a hole in the graph when it is only a gap in the path
/// bookkeeping. `axiom blast-radius --format dot` and the dashboard's tree
/// both draw from this, so the two pictures cannot disagree.
pub fn paths_to_tests(answer: &Value) -> Vec<Vec<String>> {
    let symbol = answer["symbol"].as_str().unwrap_or("?").to_string();
    let tests: Vec<&str> = answer["impacted_tests"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    tests
        .into_iter()
        .map(|test| {
            let mut path: Vec<String> = answer["causal_paths"][test]
                .as_array()
                .map(|p| {
                    p.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            if path.first() != Some(&symbol) {
                path.insert(0, symbol.clone());
            }
            if path.last().map(String::as_str) != Some(test) {
                path.push(test.to_string());
            }
            path.dedup();
            path
        })
        .collect()
}

/// The answer of `axiom_get_blast_radius` as a DOT digraph.
pub fn blast_radius_dot(answer: &Value) -> String {
    let symbol = answer["symbol"].as_str().unwrap_or("?").to_string();
    let paths = paths_to_tests(answer);
    let tests: Vec<&str> = paths
        .iter()
        .filter_map(|p| p.last().map(String::as_str))
        .collect();

    let mut between: BTreeSet<String> = BTreeSet::new();
    let mut edges: BTreeSet<(String, String)> = BTreeSet::new();
    for path in &paths {
        for pair in path.windows(2) {
            edges.insert((pair[0].clone(), pair[1].clone()));
        }
        // A test's own blast radius includes itself, and its path is then one
        // element long, so this is a skip and a take rather than a slice.
        for step in path.iter().skip(1).take(path.len().saturating_sub(2)) {
            between.insert(step.clone());
        }
    }
    let tests: BTreeSet<&str> = tests.into_iter().collect();

    let mut dot = String::from("digraph blast_radius {\n  rankdir=LR;\n");
    dot.push_str(&format!(
        "  label={};\n",
        quote(&format!(
            "{symbol}: {} of {} tests",
            tests.len(),
            answer["total_tests_in_repo"].as_u64().unwrap_or(0)
        ))
    ));
    dot.push_str(&format!("  {} [shape=box, style=bold];\n", quote(&symbol)));
    for step in between
        .iter()
        .filter(|s| **s != symbol && !tests.contains(s.as_str()))
    {
        dot.push_str(&format!("  {} [shape=box];\n", quote(step)));
    }
    for test in &tests {
        dot.push_str(&format!("  {} [shape=ellipse];\n", quote(test)));
    }
    for (from, to) in &edges {
        dot.push_str(&format!("  {} -> {};\n", quote(from), quote(to)));
    }
    dot.push_str("}\n");
    dot
}

/// A DOT double-quoted id. Symbol paths carry `::`, `/` and `.`, which an
/// unquoted id cannot, and a Windows path or a quote in a name would end the
/// string early without the escapes.
fn quote(id: &str) -> String {
    format!("\"{}\"", id.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quote_or_backslash_in_a_name_stays_inside_its_id() {
        let answer = serde_json::json!({
            "symbol": "C:\\src\\lib.rs::say",
            "impacted_tests": ["tests::says \"hi\""],
            "causal_paths": {},
            "total_tests_in_repo": 1
        });
        let dot = blast_radius_dot(&answer);
        assert!(
            dot.contains(r#""C:\\src\\lib.rs::say" -> "tests::says \"hi\"";"#),
            "{dot}"
        );
    }
}
