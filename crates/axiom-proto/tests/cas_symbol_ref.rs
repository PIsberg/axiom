use axiom_proto::CasSymbolRef;

#[test]
fn test_cas_symbol_ref_parsing_and_formatting() {
    let raw = "auth::service::validate_token@3f8a42bc11223344556677889900aabbccddeeff11223344556677889900aabb";
    let cas_ref = CasSymbolRef::parse(raw).expect("must parse valid symbol@hash");
    assert_eq!(cas_ref.symbol_path, "auth::service::validate_token");
    assert_eq!(
        cas_ref.hash,
        "3f8a42bc11223344556677889900aabbccddeeff11223344556677889900aabb"
    );
    assert_eq!(cas_ref.to_pointer(), raw);
    assert_eq!(
        cas_ref.to_uri(),
        "axiom://symbols/auth::service::validate_token#3f8a42bc11223344556677889900aabbccddeeff11223344556677889900aabb"
    );
    assert_eq!(
        cas_ref.to_slice_uri(),
        "axiom://slice/auth::service::validate_token#3f8a42bc11223344556677889900aabbccddeeff11223344556677889900aabb"
    );
}

#[test]
fn test_cas_symbol_ref_parse_uris() {
    let uri1 = "axiom://symbols/order::process#abc123hash";
    let parsed1 = CasSymbolRef::parse(uri1).expect("must parse symbols URI");
    assert_eq!(parsed1.symbol_path, "order::process");
    assert_eq!(parsed1.hash, "abc123hash");

    let uri2 = "axiom://slice/order::process#abc123hash";
    let parsed2 = CasSymbolRef::parse(uri2).expect("must parse slice URI");
    assert_eq!(parsed2.symbol_path, "order::process");
    assert_eq!(parsed2.hash, "abc123hash");

    let uri3 = "cas://order::process#abc123hash";
    let parsed3 = CasSymbolRef::parse(uri3).expect("must parse cas:// URI");
    assert_eq!(parsed3.symbol_path, "order::process");
    assert_eq!(parsed3.hash, "abc123hash");
}

#[test]
fn test_cas_symbol_ref_invalid_inputs() {
    assert!(CasSymbolRef::parse("").is_none());
    assert!(CasSymbolRef::parse("   ").is_none());
    assert!(CasSymbolRef::parse("symbol_without_hash").is_none());
    assert!(CasSymbolRef::parse("@only_hash").is_none());
    assert!(CasSymbolRef::parse("only_symbol@").is_none());
    assert!(CasSymbolRef::parse("axiom://symbols/no_hash").is_none());
}
