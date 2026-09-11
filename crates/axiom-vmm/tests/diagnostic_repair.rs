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

// The fixtures above all use a relative POSIX path, which is not what the
// evaluator ever sees. It writes the snippet to a temp directory and hands the
// compiler an absolute path, so on Windows every real diagnostic location
// begins with a drive letter, and a drive letter contains the same colon the
// location separator uses. Splitting from the left then reads the drive as the
// file and the line number as the column. Observed on 2026-09-11 by evaluating
// a snippet that does not compile: the tool reported `file: "C"`, no line, and
// `column: 8` for an error on line 8, column 31.

/// rustc's `-->` location, as the evaluator actually receives it on Windows.
#[test]
fn a_rustc_location_on_a_windows_path_keeps_its_drive_line_and_column() {
    let stderr = concat!(
        "error: mismatched closing delimiter: `}`\n",
        r" --> C:\Users\dev\AppData\Local\Temp\axiom_eval_rs_xemxWb\eval_main.rs:8:31",
        "\n"
    );
    let diags = parse_compiler_diagnostics(stderr, "");
    assert!(!diags.is_empty(), "expected a diagnostic");
    let d = &diags[0];
    assert_eq!(
        d.file.as_deref(),
        Some(r"C:\Users\dev\AppData\Local\Temp\axiom_eval_rs_xemxWb\eval_main.rs"),
        "the drive letter belongs to the path, not to the location separator"
    );
    assert_eq!(d.line, Some(8), "line 8 must be reported as the line");
    assert_eq!(d.column, Some(31));
}

/// The same for the javac/clang/gcc `file:line:col: message` form, which the C,
/// C++ and Java recipes produce. Splitting from the left failed to parse the
/// drive's remainder as a line number, so the diagnostic was dropped entirely
/// and the caller saw an empty `diagnostics` array for a compile that failed.
#[test]
fn a_gcc_location_on_a_windows_path_is_not_dropped() {
    let stderr = concat!(
        r"C:\Temp\axiom_eval_c_QQ\eval_main.c:12:5: error: 'x' undeclared here",
        "\n"
    );
    let diags = parse_compiler_diagnostics(stderr, "");
    assert!(
        !diags.is_empty(),
        "a compile error on a Windows path must still be reported"
    );
    let d = &diags[0];
    assert_eq!(
        d.file.as_deref(),
        Some(r"C:\Temp\axiom_eval_c_QQ\eval_main.c")
    );
    assert_eq!(d.line, Some(12));
    assert_eq!(d.column, Some(5));
    assert_eq!(d.severity, "error");
    assert!(d.message.contains("undeclared"));
}

/// An absolute POSIX path, which is what the ubuntu CI runner produces, has to
/// keep working. Splitting from the right is what makes both hold at once.
#[test]
fn an_absolute_posix_location_keeps_its_line_and_column() {
    let stderr =
        "error[E0425]: cannot find value `x`\n --> /tmp/axiom_eval_rs_ab/eval_main.rs:3:13\n";
    let diags = parse_compiler_diagnostics(stderr, "");
    assert!(!diags.is_empty(), "expected a diagnostic");
    let d = &diags[0];
    assert_eq!(
        d.file.as_deref(),
        Some("/tmp/axiom_eval_rs_ab/eval_main.rs")
    );
    assert_eq!(d.line, Some(3));
    assert_eq!(d.column, Some(13));
}
