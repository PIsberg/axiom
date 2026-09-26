//! `axiom dashboard` shows what agents did to the workspace, read from the
//! workspace: who wrote which symbol and where they collided, whether the
//! provenance ledger still links up, and why a symbol's tests were selected.
//!
//! The dashboard it replaces once printed constants as measurements (see
//! `test_e2e_dashboard_counts_come_from_the_index`), so each panel here is
//! checked against something done to the workspace first: three agents write
//! one function, a record is cut out of the ledger, a symbol with an indirect
//! test is asked about. In a pipe it prints one frame without colour, which is
//! also how an agent or a test reads it.

use axiom_core::AxiomMcpServer;
use axiom_core::mcp::JsonRpcRequest;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "pub fn helper(x: u32) -> u32 {\n    x + 1\n}\n\n\
pub fn middle(x: u32) -> u32 {\n    helper(x) * 2\n}\n\n\
pub fn target(x: u32) -> u32 {\n    let a = x + 1;\n    let b = a * 2;\n    let c = b - 3;\n    c\n}\n\n\
#[cfg(test)]\nmod tests {\n    use super::*;\n\n\
    #[test]\n    fn helper_adds_one() {\n        assert_eq!(helper(1), 2);\n    }\n\n\
    #[test]\n    fn middle_doubles() {\n        assert_eq!(middle(1), 4);\n    }\n}\n";

fn workspace(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "axiom_dashboard_{tag}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(d.join("src")).expect("temp dir");
    std::fs::write(d.join("src").join("lib.rs"), LIB).unwrap();
    let out = axiom(&d, &["scan", "--path", "."]);
    assert!(out.status.success(), "scan failed: {out:?}");
    d
}

fn axiom(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_axiom"))
        .args(args)
        .current_dir(dir)
        .env_remove("NO_COLOR")
        .output()
        .expect("run axiom")
}

fn frame(dir: &Path, args: &[&str]) -> String {
    let mut all = vec!["dashboard"];
    all.extend_from_slice(args);
    let out = axiom(dir, &all);
    assert!(out.status.success(), "dashboard failed: {out:?}");
    String::from_utf8(out.stdout).expect("utf-8")
}

fn agent(dir: &Path) -> AxiomMcpServer {
    AxiomMcpServer::with_index(Some(&dir.join(".axiom").join("index.json"))).expect("server")
}

fn call(server: &AxiomMcpServer, name: &str, args: Value) -> Value {
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": args })),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(server.handle_request(req)).result.unwrap();
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// The line of the frame that mentions `needle`, or a failure showing the frame.
fn line_with<'a>(frame: &'a str, needle: &str) -> &'a str {
    frame
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line mentions {needle:?}:\n{frame}"))
}

#[test]
fn the_writes_panel_names_the_agents_and_where_they_collided() {
    let dir = workspace("writes");
    let (alice, bob, carol) = (agent(&dir), agent(&dir), agent(&dir));
    let base = call(
        &alice,
        "axiom_query_symbol",
        json!({ "symbol_path": "target" }),
    )["source_text"]
        .as_str()
        .expect("source_text")
        .to_string();
    let write = |who: &AxiomMcpServer, name: &str, from: &str, to: &str| {
        call(
            who,
            "axiom_apply_mutation",
            json!({ "symbol_path": "target", "write_source": true, "agent_identity": name,
                    "base_content": base, "content": base.replace(from, to) }),
        )
    };
    assert_eq!(
        write(&alice, "alice", "x + 1", "x + 10")["status"],
        "WRITTEN"
    );
    assert_eq!(write(&bob, "bob", "x + 1", "x + 20")["status"], "CONFLICT");
    assert_eq!(
        write(&carol, "carol", "b - 3", "b - 30")["status"],
        "WRITTEN"
    );

    let shown = frame(&dir, &[]);
    assert!(
        !shown.contains('\x1b'),
        "a pipe gets no colour codes:\n{shown}"
    );
    let row = line_with(&shown, "::target");
    for expected in ["alice, bob, carol", "1 merged", "1 conflict"] {
        assert!(
            row.contains(expected),
            "{expected:?} missing from the row: {row}\n{shown}"
        );
    }
    // Carol's write landed after Bob's refusal, but Bob's change never did,
    // and a row that only looked at the latest write would call it settled.
    assert!(
        row.contains("not landed: bob"),
        "bob's refused change is still outstanding: {row}\n{shown}"
    );
    assert!(
        line_with(&shown, "WRITES").contains("1 change not landed"),
        "{shown}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_ledger_panel_shows_where_the_chain_breaks() {
    let dir = workspace("ledger");
    let server = agent(&dir);
    for task in ["t1", "t2", "t3"] {
        call(
            &server,
            "axiom_record_verification",
            json!({ "task_id": task, "passed": true, "command": "cargo test" }),
        );
        let attested = call(
            &server,
            "axiom_attest_commit",
            json!({ "prompt": format!("work {task}"), "symbol_path": "helper", "ctop_task_id": task }),
        );
        assert!(attested.get("error").is_none(), "{attested}");
    }

    let intact = frame(&dir, &[]);
    assert!(
        line_with(&intact, "LEDGER").contains("chain intact"),
        "{intact}"
    );
    assert!(
        line_with(&intact, "LEDGER").contains("3 records"),
        "{intact}"
    );

    // Cut the middle record out, as someone hiding it would.
    let ledger = dir.join(".axiom").join("attestations.json");
    let text = std::fs::read_to_string(&ledger).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, l)| l)
        .collect();
    std::fs::write(&ledger, kept.join("\n") + "\n").unwrap();

    let broken = frame(&dir, &[]);
    let header = line_with(&broken, "LEDGER");
    assert!(header.contains("chain breaks at #2"), "{broken}");
    assert!(!header.contains("chain intact"), "{broken}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_blast_panel_draws_the_path_to_each_test() {
    let dir = workspace("blast");
    let shown = frame(&dir, &["--symbol", "helper", "--depth", "2"]);

    assert!(
        line_with(&shown, "BLAST").contains("2 of 2 tests"),
        "{shown}"
    );
    let lines: Vec<&str> = shown.lines().collect();
    // Where a symbol is drawn: the line whose first word past the tree glyphs
    // is its name, bare or qualified by the module it sits in.
    let at = |name: &str| {
        lines
            .iter()
            .position(|l| {
                let text = l.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '_');
                let word = text.split_whitespace().next().unwrap_or("");
                word == name || word.ends_with(&format!("::{name}"))
            })
            .unwrap_or_else(|| panic!("{name} not drawn:\n{shown}"))
    };
    let indent = |i: usize| lines[i].find(|c: char| c.is_alphanumeric()).unwrap_or(0);
    let (middle, through_middle) = (at("middle"), at("middle_doubles"));
    assert!(
        through_middle > middle && indent(through_middle) > indent(middle),
        "the indirect test must hang under the symbol it goes through:\n{shown}"
    );
    at("helper_adds_one");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_untouched_workspace_says_so_rather_than_showing_nothing() {
    let dir = workspace("empty");
    let shown = frame(&dir, &[]);
    assert!(
        line_with(&shown, "WRITES").contains("no source writes yet"),
        "{shown}"
    );
    assert!(
        line_with(&shown, "LEDGER").contains("no records yet"),
        "{shown}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
