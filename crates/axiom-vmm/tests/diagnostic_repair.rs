use axiom_vmm::parse_compiler_diagnostics;

#[test]
fn test_parse_rustc_diagnostics() {
    let stderr = r#"
error[E0308]: mismatched types
 --> src/main.rs:14:20
    |
 14 |     let x: u32 = "hello";
    |            ---   ^^^^^^^ expected `u32`, found `&str`
    |            |
    |            expected due to this
"#;
    let diags = parse_compiler_diagnostics(stderr, "");
    assert!(!diags.is_empty(), "expected parsed diagnostics");
    let d = &diags[0];
    assert_eq!(d.file.as_deref(), Some("src/main.rs"));
    assert_eq!(d.line, Some(14));
    assert_eq!(d.column, Some(20));
    assert_eq!(d.severity, "error");
    assert!(d.message.contains("mismatched types"));
}

#[test]
fn test_parse_javac_diagnostics() {
    let stderr = r#"
App.java:12: error: cannot find symbol
        System.out.println(unknownVar);
                           ^
  symbol:   variable unknownVar
  location: class App
1 error
"#;
    let diags = parse_compiler_diagnostics(stderr, "");
    assert!(!diags.is_empty(), "expected javac diagnostics");
    let d = &diags[0];
    assert_eq!(d.file.as_deref(), Some("App.java"));
    assert_eq!(d.line, Some(12));
    assert_eq!(d.severity, "error");
    assert!(d.message.contains("cannot find symbol"));
}

#[test]
fn test_parse_python_traceback() {
    let stderr = r#"
Traceback (most recent call last):
  File "calculator.py", line 42, in evaluate
    result = 10 / 0
ZeroDivisionError: division by zero
"#;
    let diags = parse_compiler_diagnostics(stderr, "");
    assert!(!diags.is_empty(), "expected python diagnostics");
    let d = &diags[0];
    assert_eq!(d.file.as_deref(), Some("calculator.py"));
    assert_eq!(d.line, Some(42));
    assert_eq!(d.severity, "error");
    assert!(d.message.contains("division by zero"));
}
