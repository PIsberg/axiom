use axiom_ast::AstIndex;

#[test]
fn test_causal_paths_in_blast_radius() {
    let index = AstIndex::new();

    // A -> B -> Test
    index.index_node("core::engine::compute", "function", "pub fn compute() {}", vec![]);
    index.index_node(
        "service::handler::process",
        "function",
        "pub fn process() { core::engine::compute(); }",
        vec!["core::engine::compute".to_string()],
    );
    index.index_node(
        "tests::test_service",
        "test",
        "#[test] fn test_service() { service::handler::process(); }",
        vec!["service::handler::process".to_string()],
    );

    let radius = index
        .compute_blast_radius("core::engine::compute", 3)
        .expect("blast radius computed");

    assert!(radius.impacted_tests.contains(&"tests::test_service".to_string()));
    assert!(radius.causal_paths.contains_key("tests::test_service"));

    let path = &radius.causal_paths["tests::test_service"];
    assert_eq!(
        path,
        &vec![
            "core::engine::compute".to_string(),
            "service::handler::process".to_string(),
            "tests::test_service".to_string(),
        ]
    );
}

#[test]
fn test_adaptive_symbol_context_slice() {
    let index = AstIndex::new();

    index.index_node(
        "auth::token::verify",
        "function",
        "pub fn verify(token: &str) -> bool { true }",
        vec!["crypto::jwt::decode".to_string()],
    );
    index.index_node(
        "crypto::jwt::decode",
        "function",
        "pub fn decode(raw: &str) -> Claims { Claims::new() }",
        vec![],
    );
    index.index_node(
        "api::auth_middleware",
        "function",
        "pub fn auth_middleware(req: Request) { auth::token::verify(&req.token); }",
        vec!["auth::token::verify".to_string()],
    );

    // Test without tight budget
    let slice = index
        .get_symbol_slice("auth::token::verify", None)
        .expect("slice exists");

    assert_eq!(slice.symbol, "auth::token::verify");
    assert!(slice.callers.contains(&"api::auth_middleware".to_string()));
    assert!(slice.callees.contains(&"crypto::jwt::decode".to_string()));
    assert!(!slice.truncated);
    assert!(slice.rendered_slice.contains("api::auth_middleware"));
    assert!(slice.rendered_slice.contains("crypto::jwt::decode"));

    // Test with very small token budget to trigger truncation
    let tight_slice = index
        .get_symbol_slice("auth::token::verify", Some(5))
        .expect("tight slice exists");

    assert!(tight_slice.truncated);
    assert!(tight_slice.rendered_slice.contains("truncated for token budget"));
}
