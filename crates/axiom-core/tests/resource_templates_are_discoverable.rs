//! A client that follows the spec must be able to discover the URI templates.
//!
//! The server serves four parameterised resources, `axiom://symbols/{...}` and
//! friends. It listed them under `resourceTemplates` in the `resources/list`
//! reply, which a tolerant client ignores and a strict one never looks at,
//! because templates are discovered through `resources/templates/list`. That
//! method was not dispatched at all, so the probe came back
//! `-32601 Method not found`: the templates were unreachable and the client had
//! an error to explain rather than a list to read.
//!
//! The list is derived from the server's own reply on both sides here, never
//! from a copy kept in this file, so a template added later is covered without
//! anyone editing this test.

use axiom_core::{AxiomMcpServer, mcp::JsonRpcRequest};
use serde_json::json;

async fn call(server: &AxiomMcpServer, method: &str) -> serde_json::Value {
    let resp = server
        .handle_request(JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: method.into(),
            params: Some(json!({})),
        })
        .await;
    assert!(
        resp.error.is_none(),
        "{method} answered with an error: {:?}",
        resp.error
    );
    resp.result
        .unwrap_or_else(|| panic!("{method} returned no result"))
}

fn template_uris(value: &serde_json::Value) -> Vec<String> {
    value["resourceTemplates"]
        .as_array()
        .unwrap_or_else(|| panic!("no resourceTemplates array in {value}"))
        .iter()
        .map(|t| {
            t["uriTemplate"]
                .as_str()
                .expect("every template names its uriTemplate")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn resources_templates_list_returns_the_same_templates_as_resources_list() {
    let server = AxiomMcpServer::with_index(None).expect("server");

    let listed = template_uris(&call(&server, "resources/list").await);
    let templates = template_uris(&call(&server, "resources/templates/list").await);

    assert!(
        !templates.is_empty(),
        "the server serves parameterised resources, so the template list cannot be empty"
    );
    assert_eq!(
        templates, listed,
        "the two methods must read one list; they disagree"
    );
}

/// Every template has to be usable: a placeholder and a resolvable scheme.
#[tokio::test]
async fn every_template_is_a_parameterised_axiom_uri() {
    let server = AxiomMcpServer::with_index(None).expect("server");
    for uri in template_uris(&call(&server, "resources/templates/list").await) {
        assert!(
            uri.starts_with("axiom://"),
            "{uri} is not an axiom resource URI"
        );
        assert!(
            uri.contains('{') && uri.contains('}'),
            "{uri} has no placeholder, so it belongs in resources, not in templates"
        );
    }
}
