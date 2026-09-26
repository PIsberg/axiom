//! Two agents editing one function both get their change into the file, or
//! the second is told it cannot and the file is left alone.
//!
//! `axiom_apply_mutation` used to record new content in the index and the
//! Tree-CRDT op log and nowhere else: no source file changed, and the
//! statement-level 3-way merge the CRDT crate carries was called only by its
//! own tests. So two agents working on one symbol had nothing to merge; the
//! second overwrote the first in the index and the code never saw either.
//!
//! With `write_source`, the caller sends the symbol's text as it read it
//! (`base_content`, which `axiom_query_symbol` returns as `source_text`) with
//! its new text. Axiom finds the symbol in the file as it reads now, merges the
//! caller's change with whatever changed there since, and writes the result, or
//! refuses with both versions when the two changes touch the same lines. What
//! must never happen is the silent kind of loss: a write that discards a change
//! another agent already made.

use axiom_ast::AstIndex;
use axiom_core::AxiomMcpServer;
use axiom_core::mcp::JsonRpcRequest;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Workspace(PathBuf);

impl Workspace {
    /// A workspace holding one source file, scanned, with its index saved
    /// where a server discovers it.
    fn with(name: &str, content: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("axiom-write-source-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create the workspace");
        std::fs::write(root.join("src").join(name), content).expect("write the source file");
        let index = AstIndex::new();
        index.scan_directory(&root).expect("scan");
        index
            .save_to_disk(&root.join(".axiom").join("index.json"))
            .expect("save the index");
        Self(root)
    }

    /// A fresh server over this workspace: one per agent, as separate
    /// processes would be.
    fn agent(&self) -> AxiomMcpServer {
        AxiomMcpServer::with_index(Some(&self.0.join(".axiom").join("index.json")))
            .expect("start a server")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join("src").join(name)
    }

    fn read(&self, name: &str) -> String {
        String::from_utf8(std::fs::read(self.file(name)).expect("read the source file"))
            .expect("utf-8")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Call a tool and return its payload with whether it was flagged as an error.
fn call(server: &AxiomMcpServer, name: &str, args: Value) -> (Value, bool) {
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": name, "arguments": args })),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(server.handle_request(req)).result.unwrap();
    let text = result["content"][0]["text"].as_str().unwrap().to_string();
    let payload = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (payload, result["isError"] == json!(true))
}

fn source_text(server: &AxiomMcpServer, symbol: &str) -> String {
    let (reply, is_error) = call(
        server,
        "axiom_query_symbol",
        json!({ "symbol_path": symbol }),
    );
    assert!(!is_error, "{reply}");
    reply["source_text"]
        .as_str()
        .unwrap_or_else(|| panic!("a query must return the symbol's source_text: {reply}"))
        .to_string()
}

fn write(server: &AxiomMcpServer, symbol: &str, base: &str, content: &str) -> (Value, bool) {
    call(
        server,
        "axiom_apply_mutation",
        json!({
            "symbol_path": symbol,
            "write_source": true,
            "base_content": base,
            "content": content,
        }),
    )
}

const LIB: &str = "fn before() -> u32 {\n    1\n}\n\nfn target(x: u32) -> u32 {\n    let a = x + 1;\n    let b = a * 2;\n    let c = b - 3;\n    c\n}\n\nfn after() -> u32 {\n    2\n}\n";

#[test]
fn a_query_returns_the_symbols_source_as_the_file_holds_it() {
    let ws = Workspace::with("lib.rs", LIB);
    assert_eq!(
        source_text(&ws.agent(), "target"),
        "fn target(x: u32) -> u32 {\n    let a = x + 1;\n    let b = a * 2;\n    let c = b - 3;\n    c\n}"
    );
}

#[test]
fn a_write_replaces_the_symbol_and_nothing_else() {
    let ws = Workspace::with("lib.rs", LIB);
    let agent = ws.agent();
    let base = source_text(&agent, "target");
    let changed = base.replace("x + 1", "x + 10");

    let (reply, is_error) = write(&agent, "target", &base, &changed);

    assert!(!is_error, "{reply}");
    assert_eq!(reply["status"], "WRITTEN", "{reply}");
    assert_eq!(ws.read("lib.rs"), LIB.replace("x + 1", "x + 10"));
}

#[test]
fn two_agents_changing_different_lines_of_one_function_both_land() {
    let ws = Workspace::with("lib.rs", LIB);
    let (first, second) = (ws.agent(), ws.agent());
    // Both read the function before either writes.
    let base = source_text(&first, "target");
    assert_eq!(base, source_text(&second, "target"));

    let (one, err_one) = write(&first, "target", &base, &base.replace("x + 1", "x + 10"));
    assert!(!err_one, "{one}");

    // The second agent's base is now stale. Writing its text as sent would
    // discard the first agent's change.
    let (two, err_two) = write(&second, "target", &base, &base.replace("b - 3", "b - 30"));
    assert!(!err_two, "{two}");
    assert_eq!(two["status"], "WRITTEN", "{two}");
    assert_eq!(two["merged_with_changes_since_read"], true, "{two}");

    let file = ws.read("lib.rs");
    assert!(
        file.contains("x + 10"),
        "the first agent's change was lost:\n{file}"
    );
    assert!(
        file.contains("b - 30"),
        "the second agent's change was lost:\n{file}"
    );
    assert_eq!(
        file,
        LIB.replace("x + 1", "x + 10").replace("b - 3", "b - 30")
    );
}

#[test]
fn two_agents_changing_the_same_line_is_refused_and_the_file_kept() {
    let ws = Workspace::with("lib.rs", LIB);
    let (first, second) = (ws.agent(), ws.agent());
    let base = source_text(&first, "target");

    let (one, err_one) = write(&first, "target", &base, &base.replace("x + 1", "x + 10"));
    assert!(!err_one, "{one}");
    let after_first = ws.read("lib.rs");

    let (two, err_two) = write(&second, "target", &base, &base.replace("x + 1", "x + 20"));

    assert!(
        err_two,
        "a conflict must reach the agent as an error: {two}"
    );
    assert_eq!(two["status"], "CONFLICT", "{two}");
    assert_eq!(
        ws.read("lib.rs"),
        after_first,
        "a refused write must not touch the file"
    );
    let current = two["current_content"].as_str().expect("current_content");
    assert!(current.contains("x + 10"), "{two}");
    let preview = two["conflict_preview"].as_str().expect("conflict_preview");
    assert!(
        preview.contains("x + 10") && preview.contains("x + 20"),
        "{two}"
    );

    // Resolved against what the file holds now, the same change goes through.
    let (retry, err_retry) = write(
        &second,
        "target",
        current,
        &current.replace("x + 10", "x + 20"),
    );
    assert!(!err_retry, "{retry}");
    assert!(ws.read("lib.rs").contains("x + 20"));
}

#[test]
fn a_symbol_moved_by_an_edit_since_the_scan_is_still_the_one_written() {
    let ws = Workspace::with("lib.rs", LIB);
    let agent = ws.agent();
    let base = source_text(&agent, "target");

    // Someone adds a function at the top after the index was built, so every
    // line number the index holds for `target` is now wrong.
    let shifted = format!("fn added_since() -> u32 {{\n    0\n}}\n\n{LIB}");
    std::fs::write(ws.file("lib.rs"), &shifted).unwrap();

    let (reply, is_error) = write(&agent, "target", &base, &base.replace("x + 1", "x + 10"));

    assert!(!is_error, "{reply}");
    assert_eq!(ws.read("lib.rs"), shifted.replace("x + 1", "x + 10"));
}

#[test]
fn a_crlf_file_keeps_its_line_endings() {
    let crlf = LIB.replace('\n', "\r\n");
    let ws = Workspace::with("lib.rs", &crlf);
    let agent = ws.agent();
    let base = source_text(&agent, "target");

    let (reply, is_error) = write(&agent, "target", &base, &base.replace("x + 1", "x + 10"));

    assert!(!is_error, "{reply}");
    assert_eq!(ws.read("lib.rs"), crlf.replace("x + 1", "x + 10"));
}

#[test]
fn a_write_without_a_base_is_refused() {
    let ws = Workspace::with("lib.rs", LIB);
    let (reply, is_error) = call(
        &ws.agent(),
        "axiom_apply_mutation",
        json!({ "symbol_path": "target", "write_source": true, "content": "fn target() {}" }),
    );
    assert!(is_error, "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .unwrap_or("")
            .contains("base_content"),
        "{reply}"
    );
    assert_eq!(ws.read("lib.rs"), LIB);
}

#[test]
fn a_write_cannot_also_be_speculative() {
    let ws = Workspace::with("lib.rs", LIB);
    let agent = ws.agent();
    let base = source_text(&agent, "target");
    let (reply, is_error) = call(
        &agent,
        "axiom_apply_mutation",
        json!({
            "symbol_path": "target",
            "write_source": true,
            "speculative": true,
            "base_content": base,
            "content": base.replace("x + 1", "x + 10"),
        }),
    );
    assert!(is_error, "{reply}");
    assert_eq!(ws.read("lib.rs"), LIB);
}

#[test]
fn many_agents_editing_one_function_at_once_lose_nothing() {
    // One line per agent, each changed by exactly one of them, all sent at
    // once from the same stale base. Every change must be in the file after.
    //
    // A lost update is a race, and a race is lost only sometimes: with the
    // workspace lock removed, one round of this caught it once in five runs.
    // Repeating the round is what makes it a test.
    const AGENTS: usize = 8;
    for round in 0..10 {
        let body: String = (0..AGENTS)
            .map(|i| format!("    let v{i} = {i};\n"))
            .collect();
        let src = format!("fn target() {{\n{body}}}\n");
        let ws = Workspace::with("lib.rs", &src);
        let base = source_text(&ws.agent(), "target");

        // Starting a server, or even a runtime, takes far longer than a write,
        // so everything is built first and only the writes are released
        // together. Otherwise the agents write one after another and never race.
        let agents: Vec<AxiomMcpServer> = (0..AGENTS).map(|_| ws.agent()).collect();
        let go = std::sync::Barrier::new(AGENTS);
        std::thread::scope(|s| {
            for (i, agent) in agents.iter().enumerate() {
                let (base, go) = (&base, &go);
                s.spawn(move || {
                    let req = JsonRpcRequest {
                        jsonrpc: "2.0".into(),
                        id: Some(json!(i)),
                        method: "tools/call".into(),
                        params: Some(json!({ "name": "axiom_apply_mutation", "arguments": {
                            "symbol_path": "target",
                            "write_source": true,
                            "base_content": base,
                            "content": base.replace(&format!("= {i};"), &format!("= {};", 100 + i)),
                        }})),
                    };
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    go.wait();
                    let result = rt.block_on(agent.handle_request(req)).result.unwrap();
                    assert_ne!(result["isError"], json!(true), "agent {i}: {result}");
                });
            }
        });

        let file = ws.read("lib.rs");
        for i in 0..AGENTS {
            assert!(
                file.contains(&format!("let v{i} = {};", 100 + i)),
                "round {round}: agent {i}'s change was lost:\n{file}"
            );
        }
    }
}

#[test]
fn the_file_named_is_the_one_written() {
    let ws = Workspace::with("lib.rs", LIB);
    let agent = ws.agent();
    let base = source_text(&agent, "target");
    let (reply, _) = write(&agent, "target", &base, &base.replace("x + 1", "x + 10"));
    let named = reply["file"].as_str().expect("file");
    assert_eq!(
        Path::new(named).canonicalize().unwrap(),
        ws.file("lib.rs").canonicalize().unwrap()
    );
}

fn write_as(server: &AxiomMcpServer, agent: &str, base: &str, content: &str) -> (Value, bool) {
    call(
        server,
        "axiom_apply_mutation",
        json!({
            "symbol_path": "target",
            "write_source": true,
            "base_content": base,
            "content": content,
            "agent_identity": agent,
        }),
    )
}

fn logged(ws: &Workspace) -> Vec<Value> {
    let path = ws.0.join(".axiom").join("source_writes.jsonl");
    std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each log line is one JSON record"))
        .collect()
}

/// Who wrote what, and where two agents collided, is only visible afterwards if
/// the outcome is recorded: a refused write leaves no trace in the file, the
/// index or the op log, so without this a conflict is seen by the one agent
/// that received it and by nobody else.
#[test]
fn every_write_and_every_conflict_is_logged_with_its_agent() {
    let ws = Workspace::with("lib.rs", LIB);
    let (alice, bob, carol) = (ws.agent(), ws.agent(), ws.agent());
    let base = source_text(&alice, "target");

    let (a, _) = write_as(&alice, "alice", &base, &base.replace("x + 1", "x + 10"));
    assert_eq!(a["status"], "WRITTEN", "{a}");
    let (b, _) = write_as(&bob, "bob", &base, &base.replace("x + 1", "x + 20"));
    assert_eq!(b["status"], "CONFLICT", "{b}");
    let (c, _) = write_as(&carol, "carol", &base, &base.replace("b - 3", "b - 30"));
    assert_eq!(c["status"], "WRITTEN", "{c}");

    let log = logged(&ws);
    let seen: Vec<(&str, &str)> = log
        .iter()
        .map(|r| (r["agent"].as_str().unwrap(), r["outcome"].as_str().unwrap()))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("alice", "written"),
            ("bob", "conflict"),
            ("carol", "merged")
        ],
        "{log:?}"
    );
    assert!(
        log.iter().all(|r| r["symbol"] == "lib.rs::target"
            || r["symbol"].as_str().unwrap().ends_with("::target")),
        "{log:?}"
    );
}

#[test]
fn an_agent_name_that_could_forge_a_log_line_is_refused() {
    let ws = Workspace::with("lib.rs", LIB);
    let agent = ws.agent();
    let base = source_text(&agent, "target");
    let (reply, is_error) = write_as(
        &agent,
        "alice\n● bob  merged",
        &base,
        &base.replace("x + 1", "x + 10"),
    );
    assert!(is_error, "{reply}");
    assert_eq!(
        ws.read("lib.rs"),
        LIB,
        "a refused write must not touch the file"
    );
    assert!(logged(&ws).is_empty());
}
