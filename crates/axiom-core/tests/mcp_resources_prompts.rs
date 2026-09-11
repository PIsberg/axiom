use axiom_core::mcp::{AxiomMcpServer, JsonRpcRequest};
use serde_json::json;

#[tokio::test]
async fn test_mcp_initialize_advertises_resources_and_prompts() {
    let server = AxiomMcpServer::with_index(None).unwrap();
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "initialize".to_string(),
        params: None,
    };

    let res = server.handle_request(req).await;
    assert!(res.error.is_none());
    let result = res.result.expect("result present");
    assert!(result["capabilities"]["resources"].is_object());
    assert!(result["capabilities"]["prompts"].is_object());
    assert!(result["capabilities"]["tools"].is_object());
}

#[tokio::test]
async fn test_mcp_resources_list_and_read() {
    let server = AxiomMcpServer::with_index(None).unwrap();
    server.seed_demo_workspace();

    // 1. resources/list
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "resources/list".to_string(),
        params: None,
    };
    let res = server.handle_request(req).await;
    assert!(res.error.is_none());
    let result = res.result.unwrap();
    let resources = result["resources"].as_array().expect("resources array");
    assert!(resources.iter().any(|r| r["uri"] == "axiom://symbols"));
    assert!(resources.iter().any(|r| r["uri"] == "axiom://ledger"));

    // 2. resources/read axiom://symbols
    let read_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://symbols" })),
    };
    let read_res = server.handle_request(read_req).await;
    assert!(read_res.error.is_none());
    let read_result = read_res.result.unwrap();
    let contents = read_result["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["uri"], "axiom://symbols");

    // 3. resources/read specific symbol
    let sym_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://symbols/auth::service::validate_token" })),
    };
    let sym_res = server.handle_request(sym_req).await;
    assert!(sym_res.error.is_none());
    let sym_result = sym_res.result.unwrap();
    let sym_contents = sym_result["contents"].as_array().unwrap();
    let text = sym_contents[0]["text"].as_str().unwrap();
    assert!(text.contains("validate_token"));

    // 4. resources/read blast radius
    let blast_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(5)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://blast-radius/auth::service::validate_token" })),
    };
    let blast_res = server.handle_request(blast_req).await;
    assert!(blast_res.error.is_none());

    // 5. resources/read short symbol candidate resolution
    let short_sym_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(6)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://symbols/validate_token" })),
    };
    let short_sym_res = server.handle_request(short_sym_req).await;
    assert!(short_sym_res.error.is_none());
    let short_sym_result = short_sym_res.result.unwrap();
    let short_contents = short_sym_result["contents"].as_array().unwrap();
    assert!(
        short_contents[0]["text"]
            .as_str()
            .unwrap()
            .contains("validate_token")
    );

    // 6. resources/read blast-radius with depth query param
    let depth_blast_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(7)),
        method: "resources/read".to_string(),
        params: Some(json!({ "uri": "axiom://blast-radius/validate_token?depth=2" })),
    };
    let depth_blast_res = server.handle_request(depth_blast_req).await;
    assert!(depth_blast_res.error.is_none());
}

#[tokio::test]
async fn test_mcp_prompts_list_and_get() {
    let server = AxiomMcpServer::with_index(None).unwrap();

    // 1. prompts/list
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(10)),
        method: "prompts/list".to_string(),
        params: None,
    };
    let res = server.handle_request(req).await;
    assert!(res.error.is_none());
    let result = res.result.unwrap();
    let prompts = result["prompts"].as_array().expect("prompts array");
    assert!(prompts.iter().any(|p| p["name"] == "axiom_review_patch"));
    assert!(
        prompts
            .iter()
            .any(|p| p["name"] == "axiom_targeted_refactor")
    );
    assert!(prompts.iter().any(|p| p["name"] == "axiom_attest_task"));

    // 2. prompts/get axiom_targeted_refactor
    let get_req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(11)),
        method: "prompts/get".to_string(),
        params: Some(json!({
            "name": "axiom_targeted_refactor",
            "arguments": {
                "target_symbol": "auth::service::validate_token",
                "goal": "Optimize regex and add expiry check"
            }
        })),
    };
    let get_res = server.handle_request(get_req).await;
    assert!(get_res.error.is_none());
    let get_result = get_res.result.unwrap();
    let messages = get_result["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    let prompt_text = messages[0]["content"]["text"].as_str().unwrap();
    assert!(prompt_text.contains("auth::service::validate_token"));
    assert!(prompt_text.contains("Optimize regex and add expiry check"));
    assert!(prompt_text.contains("axiom_get_blast_radius"));
}

/// Every declared prompt answers a `prompts/get`.
///
/// `prompts/list` and the dispatch below it are the same pair as `tools/list` and
/// its `match`, and carry the same failure: a surface declared and not dispatched
/// fails at call time, not at startup. `declared_tools_are_dispatched` has pinned
/// that for tools since the tool set was seven. Nothing pinned it for prompts, and
/// the test that existed named the three by hand and only ever `get`-ed one of
/// them, so the other two were declared-and-unexercised.
#[tokio::test]
async fn every_declared_prompt_answers_a_get() {
    let server = AxiomMcpServer::with_index(None).unwrap();

    let listed = server
        .handle_request(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(20)),
            method: "prompts/list".to_string(),
            params: None,
        })
        .await
        .result
        .expect("prompts/list must answer");

    let prompts = listed["prompts"].as_array().expect("prompts array").clone();
    assert!(
        !prompts.is_empty(),
        "a server that advertises no prompt is not what this pins"
    );

    for prompt in prompts {
        let name = prompt["name"].as_str().expect("every prompt has a name");

        // Supply every declared argument, so what is under test is the dispatch
        // and not the required-argument check the next test covers.
        let mut arguments = serde_json::Map::new();
        for argument in prompt["arguments"].as_array().into_iter().flatten() {
            let argument_name = argument["name"]
                .as_str()
                .expect("every argument has a name");
            arguments.insert(argument_name.to_string(), json!("validate_token"));
        }

        let got = server
            .handle_request(JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                id: Some(json!(21)),
                method: "prompts/get".to_string(),
                params: Some(json!({ "name": name, "arguments": arguments })),
            })
            .await;

        assert!(
            got.error.is_none(),
            "prompt '{name}' is advertised by prompts/list and the dispatch does not \
             answer it: {:?}",
            got.error
        );
        let messages = got.result.expect("a dispatched prompt returns a result");
        assert!(
            !messages["messages"]
                .as_array()
                .expect("messages array")
                .is_empty(),
            "prompt '{name}' answered with no messages"
        );
    }
}

/// An argument the declaration marks required is refused when it is absent.
///
/// Each handler reads its arguments with a default of the empty string. Without a
/// check, a `prompts/get` that omits `target_symbol` renders "Refactor symbol ''"
/// and hands an agent an instruction naming nothing, which it has no way to tell
/// from a real one. This is the tool-schema defect wearing the prompt surface:
/// the declaration says required, the dispatch accepted absence.
#[tokio::test]
async fn a_required_prompt_argument_is_refused_when_absent() {
    let server = AxiomMcpServer::with_index(None).unwrap();

    let listed = server
        .handle_request(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(30)),
            method: "prompts/list".to_string(),
            params: None,
        })
        .await
        .result
        .expect("prompts/list must answer");

    for prompt in listed["prompts"].as_array().expect("prompts array") {
        let name = prompt["name"].as_str().expect("every prompt has a name");

        for required in prompt["arguments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["required"] == json!(true))
        {
            let omitted = required["name"]
                .as_str()
                .expect("every argument has a name");

            // Every other declared argument is supplied, so the only thing the
            // refusal can be about is the one left out.
            let mut arguments = serde_json::Map::new();
            for argument in prompt["arguments"].as_array().into_iter().flatten() {
                let argument_name = argument["name"].as_str().unwrap();
                if argument_name != omitted {
                    arguments.insert(argument_name.to_string(), json!("validate_token"));
                }
            }

            let got = server
                .handle_request(JsonRpcRequest {
                    jsonrpc: "2.0".to_string(),
                    id: Some(json!(31)),
                    method: "prompts/get".to_string(),
                    params: Some(json!({ "name": name, "arguments": arguments })),
                })
                .await;

            assert!(
                got.error.is_some(),
                "prompt '{name}' declares '{omitted}' required and answered without it; \
                 the rendered prompt would carry an empty name an agent cannot detect"
            );
        }
    }
}

/// Every declared resource answers a `resources/read`.
///
/// Same pair, third surface. A URI in `resources/list` that the read arm does not
/// match is a dead link an agent finds by following it.
#[tokio::test]
async fn every_declared_resource_answers_a_read() {
    let server = AxiomMcpServer::with_index(None).unwrap();

    let listed = server
        .handle_request(JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(40)),
            method: "resources/list".to_string(),
            params: None,
        })
        .await
        .result
        .expect("resources/list must answer");

    let resources = listed["resources"]
        .as_array()
        .expect("resources array")
        .clone();
    assert!(
        !resources.is_empty(),
        "a server that advertises no resource is not what this pins"
    );

    for resource in resources {
        let uri = resource["uri"].as_str().expect("every resource has a uri");

        let got = server
            .handle_request(JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                id: Some(json!(41)),
                method: "resources/read".to_string(),
                params: Some(json!({ "uri": uri })),
            })
            .await;

        assert!(
            got.error.is_none(),
            "resource '{uri}' is advertised by resources/list and the read arm does not \
             answer it: {:?}",
            got.error
        );
    }
}
