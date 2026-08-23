//! What `axiom serve` puts on the wire, driven as a real stdio process.
//!
//! JSON-RPC 2.0 draws one line the server was on the wrong side of: a
//! notification is a message with no `id`, and it must draw no response.
//! `notifications/initialized` is the first thing an MCP client sends after
//! `initialize`, and the server answered it with an `id: null` "method not
//! found" error, which a strict client treats as a protocol violation.
//!
//! And a line that is not JSON at all was dropped in silence, so a client that
//! sent one and waited for its answer waited forever. The spec's answer is a
//! parse error with a null id.

use std::io::Write;
use std::process::{Command, Stdio};

/// Feed the given lines to `axiom serve` and return the response lines it
/// wrote to stdout.
fn serve(lines: &[&str]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axiom"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn axiom serve");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        for line in lines {
            writeln!(stdin, "{line}").expect("write request");
        }
        // Dropping stdin closes it, so the read loop reaches EOF and the
        // process exits rather than blocking this test.
    }

    let out = child.wait_with_output().expect("wait for serve");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("non-JSON line {l:?}: {e}")))
        .collect()
}

#[test]
fn a_notification_draws_no_response() {
    let responses = serve(&[
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    ]);

    // initialize and tools/list each answer; the notification does not. Three
    // messages in, two responses out.
    assert_eq!(
        responses.len(),
        2,
        "the notification must not be answered: {responses:#?}"
    );
    let ids: Vec<_> = responses.iter().map(|r| r["id"].clone()).collect();
    assert_eq!(ids, vec![serde_json::json!(1), serde_json::json!(2)]);
}

#[test]
fn a_line_that_is_not_json_is_a_parse_error_with_a_null_id() {
    let responses = serve(&[
        "this is not json at all",
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#,
    ]);

    assert_eq!(responses.len(), 2, "{responses:#?}");
    let parse_error = &responses[0];
    assert_eq!(parse_error["id"], serde_json::Value::Null);
    assert_eq!(
        parse_error["error"]["code"], -32700,
        "a parse error is -32700 by the spec: {parse_error}"
    );
    // The valid request that followed is still answered, so one bad line does
    // not derail the session.
    assert_eq!(responses[1]["id"], serde_json::json!(7));
}

#[test]
fn a_tool_level_error_is_flagged_as_an_error_result() {
    // A tool that ran and reported a problem is a result with isError set, not
    // a JSON-RPC error: the agent is meant to read the message and react, and
    // an MCP client only surfaces it to the model when it is flagged.
    let responses = serve(&[
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"axiom_query_symbol","arguments":{"symbol_path":"does::not::exist"}}}"#,
    ]);

    let call = responses
        .iter()
        .find(|r| r["id"] == serde_json::json!(2))
        .expect("a response");
    assert_eq!(
        call["result"]["isError"],
        serde_json::json!(true),
        "a not-found result must be flagged as an error: {call}"
    );
}
