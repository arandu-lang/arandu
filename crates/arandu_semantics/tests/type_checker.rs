#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::fs;

use arandu_parser::parse;
use arandu_semantics::{TargetInfo, resolve_for_test, type_check};
use arandu_test_support::workspace_root;

/// Default 64-bit target used by integration tests (mirrors host).
const TEST_TARGET: TargetInfo = TargetInfo { pointer_width: 64 };

fn assert_diagnostic_golden(name: &str) {
    let source = arandu_test_support::read_golden_text("ui/type_checker", name, "aru");
    let program = parse(&source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, TEST_TARGET);
    arandu_test_support::assert_diagnostic_golden("ui/type_checker", name, &result.diagnostics);
}

macro_rules! assert_type_errors {
    ($source:expr, [$($code:ident),*]) => {
        let program = parse($source).expect("Failed to parse");
        let resolution = resolve_for_test(0, &program);
        let result = type_check(resolution, &program, crate::TEST_TARGET);

        let expected_codes: Vec<arandu_semantics::DiagCode> = vec![$(arandu_semantics::DiagCode::$code),*];
        // Filter to only T-series diagnostics (type checker), ignoring N-series (name resolution)
        let actual_codes: Vec<arandu_semantics::DiagCode> = result
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect();

        if expected_codes != actual_codes {
            println!("ALL DIAGNOSTICS: {:?}", result.diagnostics);
        }

        assert_eq!(
            expected_codes,
            actual_codes,
            "Expected diagnostics {:?}, but got {:?}",
            expected_codes,
            actual_codes
        );
    };
}

#[test]
fn test_mixed_operator_mismatch() {
    assert_type_errors!(
        "
        func main() {
            let x: int = 10
            let y: float = 3.14
            let z: int = x + y
        }
        ",
        [T005OperatorNotApplicable]
    );
}

#[test]
fn test_implicit_widening_error() {
    assert_type_errors!(
        "
        func main() {
            let a: int = 10
            let b: float = a
        }
        ",
        [T015ImplicitWidening]
    );
}

#[test]
fn test_result_ok() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("tests/ui/type_checker/result_ok.aru")).unwrap();
    let program = parse(&source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "expected no errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_result_err() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("tests/ui/type_checker/result_err.aru")).unwrap();
    let program = parse(&source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "expected no errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_result_propagation() {
    let root = workspace_root();
    let source =
        fs::read_to_string(root.join("tests/ui/type_checker/result_propagation.aru")).unwrap();
    let program = parse(&source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "expected no errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_method_shared() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("tests/ui/type_checker/method_shared.aru")).unwrap();
    let program = parse(&source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "expected no errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_method_mut() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("tests/ui/type_checker/method_mut.aru")).unwrap();
    let program = parse(&source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "expected no errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_method_own() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("tests/ui/type_checker/method_own.aru")).unwrap();
    let program = parse(&source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "expected no errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_option_some() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("tests/ui/type_checker/option_some.aru")).unwrap();
    let program = parse(&source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "expected no errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_option_nil() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("tests/ui/type_checker/option_nil.aru")).unwrap();
    let program = parse(&source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "expected no errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_where_ok() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join("tests/ui/type_checker/where_ok.aru")).unwrap();
    let program = parse(&source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "expected no errors: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_structural_interface_no_impl_keyword() {
    // TYP.1: satisfaction is structural (methods present) — no `impl Writer for T`.
    let source = r#"
interface Greeter {
    func greet(shared self): str
}
struct Person { n: int }
func Person.greet(shared self): str { return "hi" }
func call_it<T: Greeter>(t: T): str { return t.greet() }
func main(): int {
    let _ = call_it<Person>(Person { n: 1 })
    return 0
}
"#;
    let program = parse(source).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, arandu_semantics::Severity::Error)),
        "structural interface failed: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_result_not_handled() {
    assert_type_errors!(
        "
        func openConfig(): Result<str, Err>  {
            return Result.Ok(\"x\")
        }
        func main() {
            let config = openConfig()
        }
        ",
        [W006UnhandledResult]
    );
}

#[test]
fn test_result_try_invalid() {
    assert_type_errors!(
        "
        func main() {
            let x: int = 10
            let y = x?
        }
        ",
        [T016TryInvalid]
    );
}

#[test]
fn test_literal_absorption_ok() {
    // Literal absorption should work without implicit widening errors
    assert_type_errors!(
        "
        func main() {
            let a: float = 10
            let b: int = 10
        }
        ",
        []
    );
}

// ── Regression: promoted negative literals (Bug A) ──────────────────

#[test]
fn test_neg_promoted_min_no_t038() {
    // A promoted negative literal reaching its type's minimum must NOT
    // produce a spurious T038: the unary `-` participates in the literal
    // occurrence, so the retroactive range check sees "-128" (not "128").
    for src in [
        "func main() { let a = -128; let b: i8 = a }",
        "func main() { let a = -32768; let b: i16 = a }",
        "func main() { let a = -9223372036854775808; let b: i64 = a }",
        "func main() { let a = -0x80; let b: i8 = a }",
        "func main() { let a = -0b10000000; let b: i8 = a }",
        "func main() { let a = -0o200; let b: i8 = a }",
        "func f(x: i8) {} func main() { let a = -128; f(a) }",
        "func f(): i8 { let a = -128; return a }",
    ] {
        assert_type_errors!(src, []);
    }
}

#[test]
fn test_neg_promoted_below_min_rejected() {
    // One past the minimum is still rejected, even with the sign.
    assert_type_errors!(
        "func main() { let a = -129; let b: i8 = a }",
        [T038IntegerLiteralOutOfRange]
    );
}

// ── Regression: parenthesized negative literals (Bug B) ─────────────

#[test]
fn test_neg_paren_checked() {
    // Parenthesized negatives were never tied to the promotion group:
    // `-(129)` overflowed i8 silently. The unary arm now peels groups and
    // negates the occurrence, so out-of-range still reports T038...
    assert_type_errors!(
        "func main() { let a = -(129); let b: i8 = a }",
        [T038IntegerLiteralOutOfRange]
    );
    assert_type_errors!(
        "func main() { let a = -(1000000000000); let b: i8 = a }",
        [T038IntegerLiteralOutOfRange]
    );
    // ...and the minimum itself is still accepted, including the raw-typed
    // direct form that flows through the fast path.
    assert_type_errors!("func main() { let a = -(128); let b: i8 = a }", []);
    assert_type_errors!("func main() { let a: i8 = -(128) }", []);
}

// ── Regression: set on undefined field diagnosed once (#1) ──────────

#[test]
fn test_set_undef_field_single_diag() {
    // check_set_stmt must reuse the single synthesized place in the
    // single-place branch; before, the undefined field was diagnosed twice.
    assert_type_errors!(
        "
        struct S { x: i8 }
        func main() {
            let s = S{ x: 0 }
            set s.bad = 1
        }
        ",
        [T018UndefinedField]
    );
}

// ── Regression: receiver mismatch labels (#2) ───────────────────────

#[test]
fn test_receiver_mismatch_expected_found_swap() {
    // validate_method_receiver must constrain expected = self type, found =
    // receiver type, so flow labels line up: "type 'B' declared here" on
    // the self parameter and "value has type 'A'" on the receiver.
    let program = parse(
        "
        struct A {}
        struct B {}
        func A.m(self: B) {}
        func main() {
            let a = A{}
            a.m()
        }
        ",
    )
    .expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, TEST_TARGET);

    let t002 = result
        .diagnostics
        .iter()
        .find(|d| d.code == arandu_semantics::DiagCode::T002IncompatibleAssignment)
        .expect("expected a receiver mismatch T002");
    assert!(
        t002.message.contains("expected 'B', found 'A'"),
        "unexpected message: {}",
        t002.message
    );
    let labels: Vec<&str> = t002.labels.iter().map(|l| l.message.as_str()).collect();
    assert_eq!(labels, ["type 'B' declared here", "value has type 'A'"]);
}

// ── Regression: generic literal fields synthesized once (#3) ────────

#[test]
fn test_generic_literal_field_synth_once() {
    // infer_struct_type_args must peek at field values without side
    // effects: `1 + "x"` gets diagnosed exactly once. Before the fix the
    // field was synthesized for type inference *and* for the real check,
    // doubling the T005.
    assert_type_errors!(
        "
        struct BoxG<T> { v: T }
        func main() {
            let b = BoxG { v: 1 + \"x\" }
        }
        ",
        [T005OperatorNotApplicable]
    );
    // Literal inference itself still works (peek without diagnostics).
    assert_type_errors!(
        "
        struct BoxG<T> { v: T }
        func main() {
            let b = BoxG { v: 42 }
            let c: BoxG<int> = b
        }
        ",
        []
    );
}

// ── Regression: generic struct patterns (#4) ────────────────────────

#[test]
fn test_generic_struct_pattern() {
    // Struct patterns cannot carry generic arguments (`BoxG { v }`); the
    // pattern must match by struct symbol and bind field types from the
    // value's instantiation (`v` is `int` here, not the type parameter).
    assert_type_errors!(
        "
        struct BoxG<T> { v: T }
        func main() {
            let b = BoxG { v: 42 }
            match b {
                BoxG { v } => {
                    let x: int = v
                }
            }
        }
        ",
        []
    );
}

// ── Regression: unsigned-negation hint only for `-` (#5) ────────────

#[test]
fn test_unary_neg_hint_only_for_minus() {
    let cases: [(&str, bool); 2] = [
        ("let a: u8 = 1; let b = -a", true),
        ("let a: u8 = 1; let b = !a", false),
    ];
    for (body, want_hint) in cases {
        let source = format!("func main() {{ {body} }}");
        let program = parse(&source).expect("Failed to parse");
        let resolution = resolve_for_test(0, &program);
        let result = type_check(resolution, &program, TEST_TARGET);
        let d = &result.diagnostics[0];
        assert_eq!(
            d.code,
            arandu_semantics::DiagCode::T005OperatorNotApplicable,
            "unexpected code for `{body}`: {}",
            d.code
        );
        let has_hint = d
            .hints
            .iter()
            .any(|h| h.message.contains("cannot be negated"));
        assert_eq!(
            has_hint, want_hint,
            "hint presence mismatch for `{body}`: hints={:?}",
            d.hints
        );
    }
}

// ── Regression: call arg widening consistent with assignment (#8) ───

#[test]
fn test_call_arg_widening_reported() {
    // Non-literal numeric call args follow `let`/assignment semantics:
    // int→float is an implicit widening error (T015), matching
    // apply_assignment_constraints instead of degenerate incompatible-arg
    // mismatch. Widening int→u8 in a call is also T015.
    assert_type_errors!(
        "
        func f(x: float) {}
        func main() {
            let a: int = 1
            f(a)
        }
        ",
        [T015ImplicitWidening]
    );
    assert_type_errors!(
        "
        func f(x: u8) {}
        func main() {
            let a: int = 1
            f(a)
        }
        ",
        [T015ImplicitWidening]
    );
}

#[test]
fn test_incompatible_assignment() {
    assert_type_errors!(
        "
        func main() {
            let mut x: bool = true
            x = 10
        }
        ",
        [T002IncompatibleAssignment]
    );
}

#[test]
fn golden_implicit_widening() {
    assert_diagnostic_golden("implicit_widening");
}

#[test]
fn golden_undefined_field() {
    assert_diagnostic_golden("undefined_field");
}

#[test]
fn golden_invalid_index() {
    assert_diagnostic_golden("invalid_index");
}

#[test]
fn golden_try_invalid() {
    assert_diagnostic_golden("try_invalid");
}

#[test]
fn golden_struct_literal_errors() {
    assert_diagnostic_golden("struct_literal_errors");
}

#[test]
fn test_multi_binding_destructuring() {
    // Verify that variables defined in multi-bindings (tuple destructuring)
    // receive their correct individual types.
    assert_type_errors!(
        "
        func foo(): (int, bool)  {
            return 10, true
        }
        func main() {
            let a, b = foo()
            let x: int = a   // Ok if destructuring works
            let y: bool = b  // Ok if destructuring works
        }
        ",
        []
    );

    // Also verify mismatch is correctly identified on assignment to wrong type
    assert_type_errors!(
        "
        func foo(): (int, bool)  {
            return 10, true
        }
        func main() {
            let a, b = foo()
            let x: bool = a  // Mismatch: a is int, not bool
        }
        ",
        [T002IncompatibleAssignment]
    );
}

#[test]
fn test_multi_assignment_destructuring() {
    // Verify that assignments with 'set' to multiple variables (multi-assignment)
    // correctly validate types of individual tuple elements.
    assert_type_errors!(
        "
        func foo(): (int, bool)  {
            return 10, true
        }
        func main() {
            let mut a: int = 0
            let mut b: bool = false
            a, b = foo() // Ok if assignment destructuring works
        }
        ",
        []
    );

    assert_type_errors!(
        "
        func foo(): (int, bool)  {
            return 10, true
        }
        func main() {
            let mut a: bool = false
            let mut b: bool = false
            a, b = foo() // Mismatch: a is bool, LHS is int
        }
        ",
        [T002IncompatibleAssignment]
    );
}

#[test]
fn test_generic_type_resolution() {
    assert_type_errors!(
        "
        struct Box<T> {
            value: T
        }
        func get_box(): Box<int>  {
            return get_box()
        }
        func main() {
            let b: Box<int> = get_box()
        }
        ",
        []
    );
}

#[test]
fn test_expr_types_population() {
    let source = "
    func main() {
        let x: int = 10 + 20
    }
    ";
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);

    // Check that expr_types contains populated expression types
    assert!(!result.type_info.expr_types.is_empty());
}

#[test]
fn test_type_info_uses_interned_type_ids() {
    let source = "
    func main() {
        let a: int = 10
        let b: int = 20
    }
    ";
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);

    let mut int_ids = result.type_info.decl_types.values().filter_map(|type_id| {
        let ty = result.type_info.resolve_type_id(*type_id);
        (ty.display(&result.symbols, &result.type_info.type_interner) == "int").then_some(*type_id)
    });
    let first = int_ids.next().expect("expected at least one int type id");
    assert!(
        int_ids.any(|type_id| type_id == first),
        "expected repeated int declarations to share a TypeId"
    );
}

#[test]
fn test_forward_declarations() {
    assert_type_errors!(
        "
        func entry() {
            // Forward ref to Struct and Function:
            let s: MyStruct = MyStruct { val: 42 }
            let val: int = getVal(s)
        }

        func getVal(s: MyStruct): int  {
            return s.val
        }

        struct MyStruct {
            val: int
        }
        ",
        []
    );
}

#[test]
fn test_byte_arithmetic() {
    assert_type_errors!(
        "
        func main() {
            let a: byte = 10
            let b: byte = 20
            let c: byte = a + b
        }
        ",
        []
    );
}

#[test]
fn test_any_validation() {
    // any inside local variable declaration: should fail with T014
    assert_type_errors!(
        "
        func main() {
            let a: any = 10
        }
        ",
        [T014InvalidVariadicType]
    );

    // any inside struct field definition: should fail with T014
    assert_type_errors!(
        "
        struct S {
            a: any
        }
        ",
        [T014InvalidVariadicType]
    );

    // any inside normal func parameter: should fail with T014
    assert_type_errors!(
        "
        func foo(x: any) {}
        ",
        [T014InvalidVariadicType]
    );

    // any inside variadic parameter: should succeed
    assert_type_errors!(
        "
        func foo(x: any...) {}
        ",
        []
    );
}

#[test]
fn test_enum_variant_resolution() {
    assert_type_errors!(
        "
        enum LoadState {
            Idle,
            Loaded(str),
        }
        func main() {
            let a: LoadState = LoadState.Idle
            let b: func(str) LoadState = LoadState.Loaded
            let c: LoadState = LoadState.Loaded(\"hello\")
        }
        ",
        []
    );
}

#[test]
fn test_match_pattern_typecheck() {
    assert_type_errors!(
        "
        enum LoadState {
            Idle,
            Loaded(str),
        }
        func check_state(state: LoadState): int  {
            match state {
                LoadState.Idle => { return 0; }
                LoadState.Loaded(s) => {
                    let val: str = s
                    return 1;
                }
            }
        }
        ",
        []
    );

    // Test pattern type mismatch
    assert_type_errors!(
        "
        enum LoadState {
            Idle,
            Loaded(str),
        }
        func check_state(state: LoadState): int  {
            match state {
                LoadState.Loaded(123) => { return 1; }
            }
        }
        ",
        [T024NonExhaustiveMatch, T002IncompatibleAssignment]
    );
}

#[test]
fn test_call_validation() {
    // 1. Wrong argument count: T012
    assert_type_errors!(
        "
        func foo(x: int, y: bool) {}
        func main() {
            foo(10)
        }
        ",
        [T012WrongArgCount]
    );

    // 2. Call non-callable: T003
    assert_type_errors!(
        "
        func main() {
            let x: int = 10
            x()
        }
        ",
        [T003IncompatibleCallArg]
    );
}

#[test]
fn test_cast_validation() {
    assert_type_errors!(
        r#"
        func main() {
            let x: int ="hello" as int
        }
        "#,
        [T010InvalidCast]
    );
}

#[test]
fn test_nullability_and_safe_access() {
    // 1. Accessing field on nullable without safe operator '?.'
    assert_type_errors!(
        "
        struct User {
            age: int
        }
        func main() {
            let u: User? = nil
            let a: int = u.age
        }
        ",
        [T006NotNullable]
    );

    // 2. Safe access returning a nullable type
    assert_type_errors!(
        "
        struct User {
            age: int
        }
        func main() {
            let u: User? = nil
            let a: int? = u?.age
        }
        ",
        []
    );

    // 3. Indexing a *nullable slice* without `?[]`.
    // Grammar: `[]int?` is `[](int?)` (slice of optional ints), not a nullable slice.
    // Parenthesize: `([]int)?` for "optional slice of int".
    assert_type_errors!(
        "
        func main() {
            let arr: ([]int)? = nil
            let x: int = arr[0]
        }
        ",
        [T006NotNullable]
    );

    // 4. Safe indexing on nullable slice → element type (int?), assignable to int?
    assert_type_errors!(
        "
        func main() {
            let arr: ([]int)? = nil
            let x: int? = arr?[0]
        }
        ",
        []
    );
}

#[test]
fn test_official_ok_suite() {
    let root = workspace_root();
    let ok_dir = root
        .join("tests")
        .join("ui")
        .join("type_checker")
        .join("ok");
    assert!(ok_dir.exists(), "ok directory does not exist");

    let mut paths = Vec::new();
    for entry in fs::read_dir(ok_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("aru") {
            paths.push(path);
        }
    }
    paths.sort();

    for path in paths {
        let source = fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
        let program = parse(&source)
            .unwrap_or_else(|err| panic!("failed to parse {}: {:?}", path.display(), err));
        let resolution = resolve_for_test(0, &program);
        let result = type_check(resolution, &program, crate::TEST_TARGET);

        let errors: Vec<String> = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == arandu_semantics::Severity::Error)
            .map(|d| format!("{d}"))
            .collect();
        assert!(
            errors.is_empty(),
            "File {} failed typecheck with errors:\n{}",
            path.display(),
            errors.join("\n")
        );
    }
}

#[test]
fn test_official_invalid_suite() {
    let root = workspace_root();
    let invalid_dir = root
        .join("tests")
        .join("ui")
        .join("type_checker")
        .join("invalid");
    assert!(invalid_dir.exists(), "invalid directory does not exist");

    let mut aru_files = std::collections::HashSet::new();
    let mut diag_files = std::collections::HashSet::new();

    for entry in fs::read_dir(&invalid_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap()
            .to_string();
        if path.extension().and_then(|s| s.to_str()) == Some("aru") {
            aru_files.insert(name);
        } else if path.extension().and_then(|s| s.to_str()) == Some("diag") {
            diag_files.insert(name);
        }
    }

    // Check for orphans
    for name in &aru_files {
        assert!(
            diag_files.contains(name),
            "Orphan file: tests/ui/type_checker/invalid/{name}.aru has no corresponding .diag file"
        );
    }
    for name in &diag_files {
        assert!(
            aru_files.contains(name),
            "Orphan file: tests/ui/type_checker/invalid/{name}.diag has no corresponding .aru file"
        );
    }

    let mut sorted_names: Vec<String> = aru_files.into_iter().collect();
    sorted_names.sort();

    for name in sorted_names {
        let path = invalid_dir.join(format!("{name}.aru"));
        let diag_path = invalid_dir.join(format!("{name}.diag"));
        // Golden diagnostic spans use LF byte offsets on every platform.
        let source = fs::read_to_string(&path).unwrap().replace("\r\n", "\n");

        // Standardize relative filepath format with forward slashes:
        let rel_filepath = path
            .strip_prefix(&root)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");

        let mut actual = String::new();
        let mut registry = arandu_base::source_registry::SourceRegistry::default();
        registry.register(&rel_filepath, &source);

        match parse(&source) {
            Ok(program) => {
                let resolution = resolve_for_test(0, &program);
                let result = type_check(resolution, &program, crate::TEST_TARGET);
                for diagnostic in &result.diagnostics {
                    actual.push_str(&diagnostic.format_for_cli(&registry));
                    actual.push('\n');
                }
            }
            Err(err) => {
                actual.push_str(&err.format_for_cli(&registry));
                actual.push('\n');
            }
        }

        let update_golden = std::env::var("UPDATE_GOLDEN").is_ok();
        if update_golden {
            fs::write(&diag_path, &actual).unwrap();
        } else {
            let expected = fs::read_to_string(&diag_path).unwrap();
            let actual_lines: Vec<&str> = actual
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let expected_lines: Vec<&str> = expected
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();

            assert_eq!(
                actual_lines,
                expected_lines,
                "Mismatch in golden diagnostic test for {}.\nActual:\n{}\nExpected:\n{}",
                path.display(),
                actual,
                expected
            );
        }
    }
}

#[test]
fn test_break_continue_outside_loop() {
    assert_type_errors!(
        "
        func main() {
            break
            continue
        }
        ",
        [N011BreakContinueOutsideLoop, N011BreakContinueOutsideLoop]
    );
}

#[test]
fn test_free_requires_ptr() {
    assert_type_errors!(
        "
        func main() {
            let x: int = 10
            free(x)
        }
        ",
        [O011FreeRequiresPtr]
    );
}

#[test]
fn test_null_coalesce_type_mismatch() {
    assert_type_errors!(
        "
        func main() {
            let a: int? = nil
            let b =a ?? \"text\"
        }
        ",
        [T002IncompatibleAssignment]
    );
}

#[test]
fn test_array_literal_element_mismatch() {
    assert_type_errors!(
        "
        func main() {
            let xs =[1, 2.5, 3]
        }
        ",
        [T002IncompatibleAssignment]
    );
}

#[test]
fn test_array_literal_element_mismatch_string() {
    assert_type_errors!(
        "
        func main() {
            let xs =[1, \"dois\", 3]
        }
        ",
        [T002IncompatibleAssignment]
    );
}

#[test]
fn test_catch_result_ok_type() {
    let source = r#"
        func ok(): Result<int, Err>  {
            return Result.Ok(1)
        }
        func main() {
            let x = ok() catch 0
        }
    "#;
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    let t_errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| format!("{}", d.code).starts_with('T'))
        .collect();
    assert!(
        t_errors.is_empty(),
        "expected no type errors for valid catch, got: {:?}",
        t_errors
    );
}

#[test]
fn test_catch_requires_result() {
    assert_type_errors!(
        "
        func main() {
            let x: int = 1
            let y =x catch 0
        }
        ",
        [T005OperatorNotApplicable]
    );
}

#[test]
fn test_non_exhaustive_enum_match() {
    assert_type_errors!(
        "
        enum Color { Red, Green, Blue }
        func pick(c: Color): int  {
            return match c {
                Color.Red => 1
                Color.Green => 2
            }
        }
        ",
        [T024NonExhaustiveMatch]
    );
}

#[test]
fn test_generic_identity_call() {
    assert_type_errors!(
        "
        func identity<T>(value: T): T  {
            return value
        }
        func main() {
            let x: int = identity<int>(42)
        }
        ",
        []
    );
}

#[test]
fn test_generic_box_struct_literal() {
    assert_type_errors!(
        "
        struct Box<T> {
            value: T
        }
        func main() {
            let b: Box<int> = Box<int> { value: 42 }
        }
        ",
        []
    );
}

#[test]
fn test_result_ok_generic() {
    assert_type_errors!(
        "
        func ok(): Result<int, Err>  {
            return Result.Ok<int>(1)
        }
        func main() {
            let x = ok()
        }
        ",
        [W006UnhandledResult]
    );
}

#[test]
fn test_result_ok_custom_error_enum() {
    // Bidirectional: return type Result<T, E> pins E for Result.Ok / Result.Err.
    assert_type_errors!(
        "
        enum E { A, B }
        func ok(): Result<int, E> {
            return Result.Ok(1)
        }
        func err(): Result<int, E> {
            return Result.Err(E.A)
        }
        func main(): int {
            let x = ok()?
            return x
        }
        ",
        []
    );
}

#[test]
fn test_generic_where_interface_ok() {
    assert_type_errors!(
        "
        interface Show {
            func show(): void
        }
        struct Counter {
            n: int
        }
        func Counter.show(shared self): void  {
        }
        func emit<T: Show>(value: T): void  {
        }
        func main() {
            emit<Counter>(Counter { n: 1 })
        }
        ",
        []
    );
}

#[test]
fn test_generic_where_constraint_violation() {
    assert_type_errors!(
        "
        interface Show {
            func show(): void
        }
        struct Silent {
            n: int
        }
        func emit<T: Show>(value: T): void  {
        }
        func main() {
            emit<Silent>(Silent { n: 0 })
        }
        ",
        [T025InterfaceNotSatisfied]
    );
}

#[test]
fn test_struct_generic_param_constraint() {
    assert_type_errors!(
        "
        interface Show {
            func show(): void
        }
        struct Box<T: Show> {
            value: T
        }
        struct Silent {
            n: int
        }
        func main() {
            let b: Box<Silent> = Box<Silent> { value: Silent { n: 0 } }
        }
        ",
        [T025InterfaceNotSatisfied]
    );
}

#[test]
fn test_type_decl_generic_constraint_violation() {
    assert_type_errors!(
        "
        interface Show {
            func show(): void
        }
        struct Box<T: Show> {
            value: T
        }
        struct Silent {
            n: int
        }
        struct Container {
            b: Box<Silent>
        }
        func main() {}
        ",
        [T025InterfaceNotSatisfied]
    );
}

#[test]
fn golden_interface_not_satisfied() {
    assert_diagnostic_golden("interface_not_satisfied");
}

#[test]
fn golden_where_invalid_type_param() {
    assert_diagnostic_golden("where_invalid_type_param");
}

#[test]
fn test_type_checker_smart_suggestions() {
    let source = "
        struct Point {
            myfield: int
            y: int
        }
        func Point.get_x(self): int  {
            return self.myfield
        }
        func main() {
            let p: Point = Point { myfield: 0, y: 0 }
            // Case-insensitive match on field
            let a: int = p.myField
            // Method suggestion
            let b: int = p.get_
            // Pointer to struct suggestion
            let ptr_p: ptr[Point] = alloc(Point { myfield: 1, y: 2 })
            let c: int = ptr_p.y_
        }
    ";
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);

    let hints: Vec<String> = result
        .diagnostics
        .iter()
        .flat_map(|d| d.hints.iter().map(|h| h.message.clone()))
        .collect();

    assert!(
        hints.contains(&"did you mean 'myfield'?".to_string()),
        "got hints: {:?}",
        hints
    );
    assert!(
        hints.contains(&"did you mean 'get_x()'?".to_string()),
        "got hints: {:?}",
        hints
    );
    assert!(
        hints.contains(&"did you mean 'y'?".to_string()),
        "got hints: {:?}",
        hints
    );
}

#[test]
fn test_interface_missing_method_suggestions() {
    let source = "
        interface Writer {
            func write(): void
        }
        struct Buffer {
            data: int
        }
        // Buffer implements a typo-ed method wrte instead of write
        func Buffer.wrte(shared self): void  {
        }
        func send<T: Writer>(value: T): void  {
        }
        func main() {
            send<Buffer>(Buffer { data: 1 })
        }
    ";
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);

    // Check that we got the expected error and spelling suggestion
    let mut found = false;
    for diag in &result.diagnostics {
        if diag.code == arandu_semantics::DiagCode::T025InterfaceNotSatisfied {
            for note in &diag.notes {
                if note.contains("write (did you mean `wrte`?)") {
                    found = true;
                }
            }
        }
    }
    assert!(
        found,
        "Expected note suggesting 'wrte' for missing method 'write', but got diagnostics: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_async_block_and_await_typecheck() {
    let source = "
        func main(): void  {
            let x: Coroutine<int> = async { 42; };
            let y: int = await x;
        }
    ";
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result.diagnostics.is_empty(),
        "Expected no type errors, but got {:?}",
        result.diagnostics
    );
}

/// A3.6 / typeck: builtin `Poll<T>` + `Poll.Ready` / `Poll.Pending`.
#[test]
fn test_poll_type_and_ctors() {
    let source = r#"
        func ready_one(): Poll<int> {
            return Poll.Ready(1)
        }
        func pend(): Poll<int> {
            return Poll.Pending()
        }
        func main(): void {
            let a: Poll<int> = ready_one()
            let b: Poll<str> = Poll.Pending()
        }
    "#;
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result.diagnostics.is_empty(),
        "Expected no type errors, got {:?}",
        result.diagnostics
    );
}

/// A3: `async func f(): T` is type-sugar for `func f(): Coroutine[T]`.
#[test]
fn test_async_func_return_is_coroutine() {
    let source = "
        async func answer(): int {
            return 42
        }
        func main(): int {
            let c: Coroutine<int> = answer()
            return await c
        }
    ";
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result.diagnostics.is_empty(),
        "Expected no type errors, but got {:?}",
        result.diagnostics
    );
}

#[test]
fn test_await_invalid_type() {
    let source = "
        func main(): void  {
            let x: int = await 42;
        }
    ";
    assert_type_errors!(source, [T032AwaitInvalid]);
}

#[test]
fn test_variant_sugar_user_enum_simple() {
    let source = "
        enum Color {
            Red,
            Green,
            Blue,
        }
        func id(c: Color): Color {
            return c
        }
        func main() {
            let val: Color = .Red
            let got = id(.Green)
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn test_variant_sugar_user_enum_with_payload() {
    let source = "
        enum Payload {
            Val(int),
            Empty,
        }
        func id(p: Payload): Payload {
            return p
        }
        func main() {
            let val: Payload = .Val(42)
            let got = id(.Empty)
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn test_variant_sugar_user_enum_generic() {
    let source = "
        enum MyGeneric<T> {
            Data(T),
            None,
        }
        func main() {
            let val: MyGeneric<int> = .Data(42)
            let got: MyGeneric<int> = .None
        }
    ";
    assert_type_errors!(source, []);

    // Error case: type mismatch
    let err_source = "
        enum MyGeneric<T> {
            Data(T),
            None,
        }
        func main() {
            let val: MyGeneric<int> = .Data(\"hello\")
        }
    ";
    assert_type_errors!(err_source, [T003IncompatibleCallArg]);
}

#[test]
fn test_dot_variant_sugar_shadowing() {
    let source = "
        enum MeuTipo {
            Ok(int),
            Err,
        }
        func main() {
            let x: MeuTipo = .Ok(42)
            let y: Result<int, str> = .Ok(100)
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn test_recursive_struct_infinite_size() {
    let source = "
        struct InfiniteNode {
            val: int
            next: InfiniteNode
        }
        func main() {}
    ";
    assert_type_errors!(source, [T029RecursiveStructInfiniteSize]);
}

#[test]
fn test_recursive_struct_nullable_ok() {
    let source = "
        struct Node {
            val: int
            next: Node?
        }
        func main() {}
    ";
    assert_type_errors!(source, []);
}

#[test]
fn test_mixed_layout_typecheck() {
    let source = "
        struct MixedLayout {
            a: byte
            b: int
            c: bool
            d: i32
        }
        func main() {
            let m = MixedLayout { a: 42 as byte, b: 999999, c: true, d: 123456 as i32 }
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn contextual_integer_literals_accept_boundaries() {
    let source = "
        func take(value: u8): u8 { return value }
        func main() {
            let low: u8 = 0
            let high: u8 = 255
            let passed = take(255)
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn contextual_integer_literals_reject_values_outside_fixed_width() {
    let source = "
        func take(value: u8): u8 { return value }
        func main() {
            let high: u8 = 256
            let passed = take(300)
        }
    ";
    assert_type_errors!(
        source,
        [T038IntegerLiteralOutOfRange, T038IntegerLiteralOutOfRange]
    );
}

#[test]
fn impl_blocks_preserve_associated_functions_and_instance_methods() {
    let source = "
        struct Point {
            x: float
            y: float
        }
        impl Point {
            public func new(x: float, y: float): Point { return Point { x, y } }
            public func getX(self: ref): float { return self.x }
            public func setX(self: mut ref, x: float) { self.x = x }
        }
        func main(): int {
            let mut point = Point.new(1.0, 2.0)
            point.setX(3.0)
            if point.getX() == 3.0 { return 0 }
            return 1
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn interface_method_self_parameter_and_return_substitution() {
    let source = "
        enum Ordering {
            Less,
            Equal,
            Greater,
        }
        interface Ord {
            func cmp(self: ref Self, other: ref Self): Ordering
        }
        struct Score {
            val: int
        }
        func Score.cmp(self: ref Score, other: ref Score): Ordering {
            if self.val < other.val { return Ordering.Less }
            if self.val > other.val { return Ordering.Greater }
            return Ordering.Equal
        }
        func min_by<T: Ord>(a: T, b: T): T {
            let ord = a.cmp(ref b)
            match ord {
                Ordering.Less => { return a }
                _ => { return b }
            }
        }
        func main(): int {
            let s1 = Score { val: 10 }
            let s2 = Score { val: 20 }
            let m = min_by<Score>(s1, s2)
            return m.val
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn parameterized_interface_constraint_satisfied_and_methods_called() {
    let source = "
        interface Iterator<Item> {
            func next(self: mut ref Self): Option<Item>
        }
        struct Counter {
            curr: int
        }
        func Counter.next(self: mut ref Counter): Option<int> {
            let val = self.curr
            self.curr = self.curr + 1
            return Option.Some(val)
        }
        func step<I: Iterator<int>>(mut iter: I): Option<int> {
            return iter.next()
        }
        func main(): int {
            let mut c = Counter { curr: 0 }
            let res = step<Counter>(c)
            match res {
                Option.Some(val) => { return val }
                Option.None => { return -1 }
            }
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn parameterized_interface_constraint_generic_adapter_take() {
    let source = "
        interface Iterator<Item> {
            func next(self: mut ref Self): Option<Item>
        }
        struct Take<I: Iterator<Item>, Item> {
            iter: I
            remaining: int
        }
        func Take.next<I: Iterator<Item>, Item>(self: mut ref Take<I, Item>): Option<Item> {
            if self.remaining <= 0 {
                return Option.None
            }
            self.remaining = self.remaining - 1
            return self.iter.next()
        }
        struct RangeIter {
            val: int
        }
        func RangeIter.next(self: mut ref RangeIter): Option<int> {
            let v = self.val
            self.val = self.val + 1
            return Option.Some(v)
        }
        func take<I: Iterator<Item>, Item>(iter: I, n: int): Take<I, Item> {
            return Take<I, Item> { iter, remaining: n }
        }
        func main(): int {
            let r = RangeIter { val: 10 }
            let mut t = take(r, 5)
            let item = t.next()
            match item {
                Option.Some(val) => { return val }
                Option.None => { return 0 }
            }
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn parameterized_interface_constraint_type_argument_mismatch() {
    let source = "
        interface Iterator<Item> {
            func next(self: mut ref Self): Option<Item>
        }
        struct Counter {
            curr: int
        }
        func Counter.next(self: mut ref Counter): Option<int> {
            return Option.Some(self.curr)
        }
        func consume<I: Iterator<str>>(mut iter: I): Option<str> {
            return iter.next()
        }
        func main() {
            let c = Counter { curr: 0 }
            let _ = consume<Counter>(c)
        }
    ";
    assert_type_errors!(source, [T025InterfaceNotSatisfied]);
}

#[test]
fn where_clause_with_parameterized_interface_constraint() {
    let source = "
        interface Iterator<Item> {
            func next(self: mut ref Self): Option<Item>
        }
        struct Range {
            current: int
        }
        func Range.next(self: mut ref Range): Option<int> {
            return Option.Some(self.current)
        }
        func first<I, Item>(mut iter: I): Option<Item> where I: Iterator<Item> {
            return iter.next()
        }
        func main(): int {
            let r = Range { current: 42 }
            let res = first(r)
            match res {
                Option.Some(v) => { return v }
                Option.None => { return 0 }
            }
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn test_ptr_nil_comparison() {
    let source = "
        func check_ptr(p: ptr[u8]): bool {
            if p == nil {
                return false
            }
            if p != nil {
                return true
            }
            return false
        }
    ";
    assert_type_errors!(source, []);
}

// ── TYP.3.2: Propagação em Operadores Binários e Coleções ──

#[test]
fn typ32_binary_ops_with_expected_type() {
    let source = "
        func main() {
            let x: u64 = 1 + 2
            let y: u64 = 1 + 2 + 3
            let z: u8 = 100 + 155
            let w: i16 = 1000 - 500
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ32_binary_ops_heterogeneous_propagation() {
    let source = "
        func test_hetero(x: u64): u64 {
            let a = x + 1
            let b = 1 + x
            let c = (1 + 2) + x
            let d = x + (1 + 2)
            return a + b + c + d
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ32_binary_comparisons() {
    let source = "
        func test_cmp(x: u8): bool {
            let a = x > 1 + 2
            let b = 1 + 2 < x
            let c = x == 1 + 2
            let d = 1 + 2 == x
            return a && b && c && d
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ32_binary_ops_reject_out_of_range() {
    let source = "
        func main() {
            let x: u8 = 100 + 300
        }
    ";
    assert_type_errors!(source, [T038IntegerLiteralOutOfRange]);
}

#[test]
fn typ32_negative_literal_range_checking() {
    let valid_source = "
        func main() {
            let a: i8 = -128
            let b: i16 = -32768
        }
    ";
    assert_type_errors!(valid_source, []);

    let invalid_source = "
        func main() {
            let a: i8 = -129
        }
    ";
    assert_type_errors!(invalid_source, [T038IntegerLiteralOutOfRange]);
}

#[test]
fn typ32_array_literal_with_expected_type() {
    let valid_source = "
        func main() {
            let a: [3]u8 = [1, 2, 3]
            let b: [2]u64 = [100, 200]
        }
    ";
    assert_type_errors!(valid_source, []);

    let invalid_source = "
        func main() {
            let a: [3]u8 = [1, 300, 3]
        }
    ";
    assert_type_errors!(invalid_source, [T038IntegerLiteralOutOfRange]);
}

#[test]
fn typ32_array_literal_concrete_element_inference() {
    let valid_source = "
        func main() {
            let x: u8 = 42
            let a = [1, 2, x]
            let b = [x, 1, 2]
            let c = [1, x, 2]
        }
    ";
    assert_type_errors!(valid_source, []);

    let invalid_source_trailing = "
        func main() {
            let x: u8 = 42
            let a = [1, 300, x]
        }
    ";
    assert_type_errors!(invalid_source_trailing, [T038IntegerLiteralOutOfRange]);

    let invalid_source_leading = "
        func main() {
            let x: u8 = 42
            let a = [x, 300, 1]
        }
    ";
    assert_type_errors!(invalid_source_leading, [T038IntegerLiteralOutOfRange]);
}

#[test]
fn typ32_array_literal_pure_literals() {
    let source = "
        func main() {
            let a = [1, 2, 3]
            let b = [1.0, 2.0, 3.0]
        }
    ";
    assert_type_errors!(source, []);

    let mismatch = "
        func main() {
            let a = [1, 2.5]
        }
    ";
    assert_type_errors!(mismatch, [T002IncompatibleAssignment]);
}

#[test]
fn typ32_binary_ops_in_call_args_and_return() {
    let source = "
        func take_u64(x: u64): u64 {
            return x
        }
        func calc(): u64 {
            return 10 + 20
        }
        func main() {
            let res = take_u64(1 + 2)
            let res2 = take_u64(calc() + 5)
        }
    ";
    assert_type_errors!(source, []);

    let overflow_arg = "
        func take_u8(x: u8): u8 {
            return x
        }
        func main() {
            let res = take_u8(100 + 300)
        }
    ";
    assert_type_errors!(overflow_arg, [T038IntegerLiteralOutOfRange]);
}

#[test]
fn typ32_array_expected_type_in_args_and_let() {
    let source = "
        func take_array(a: [3]u8): int {
            return 0
        }
        func main() {
            let a: [3]u8 = [1, 2, 3]
            let res = take_array([10, 20, 30])
        }
    ";
    assert_type_errors!(source, []);

    let overflow_source = "
        func main() {
            let a: [3]u8 = [1, 300, 3]
        }
    ";
    assert_type_errors!(overflow_source, [T038IntegerLiteralOutOfRange]);

    let overflow_call = "
        func take_array(a: [3]u8): int {
            return 0
        }
        func main() {
            let res = take_array([10, 300, 30])
        }
    ";
    assert_type_errors!(overflow_call, [T038IntegerLiteralOutOfRange]);
}

#[test]
fn typ33_unify_literal_vars_in_binary_ops() {
    let source = "
        func take_u64(x: u64): u64 {
            return x
        }
        func main() {
            let a = 10
            let b = a + 20
            let res = take_u64(b)
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ33_unify_literal_vars_retroactive_overflow_t038() {
    let source = "
        func take_u8(x: u8): u8 {
            return x
        }
        func main() {
            let a = 300
            let b = a + 1
            let res = take_u8(b)
        }
    ";
    assert_type_errors!(source, [T038IntegerLiteralOutOfRange]);
}

#[test]
fn typ33_unconstrained_literal_defaults_to_native_int() {
    let source = "
        func main() {
            let a = 10
            let b = a + 20
        }
    ";
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        result.diagnostics
    );

    for (&sym_id, &type_id) in &result.type_info.decl_types {
        let sym = result.symbols.get(sym_id);
        if sym.name == "a" || sym.name == "b" {
            let ty = result.type_info.resolve_type_id(type_id);
            let display = ty.display(&result.symbols, &result.type_info.type_interner);
            assert_eq!(
                display, "int",
                "decl {} should default to int, got {}",
                sym.name, display
            );
        }
    }
}

#[test]
fn typ33_unconstrained_float_defaults_to_native_float() {
    let source = "
        func main() {
            let x = 1.5
            let y = x + 2.0
        }
    ";
    let program = parse(source).expect("Failed to parse");
    let resolution = resolve_for_test(0, &program);
    let result = type_check(resolution, &program, crate::TEST_TARGET);
    assert!(
        result.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        result.diagnostics
    );

    for (&sym_id, &type_id) in &result.type_info.decl_types {
        let sym = result.symbols.get(sym_id);
        if sym.name == "x" || sym.name == "y" {
            let ty = result.type_info.resolve_type_id(type_id);
            let display = ty.display(&result.symbols, &result.type_info.type_interner);
            assert_eq!(
                display, "float",
                "decl {} should default to float, got {}",
                sym.name, display
            );
        }
    }
}

#[test]
fn typ33_promoted_literal_call_arg_widening() {
    // `let a = 10` is a promoted literal: `take_u8(a)` absorbs the u8
    // retarget silently, but the subsequent `u8 -> u64` call argument is an
    // implicit widening, reported as T015 — matching `let`/assignment
    // semantics instead of the generic incompatible-argument T003.
    let source = "
        func take_u8(x: u8) {}
        func take_u64(x: u64) {}
        func main() {
            let a = 10
            take_u8(a)
            take_u64(a)
        }
    ";
    assert_type_errors!(source, [T015ImplicitWidening]);
}

#[test]
fn typ33_promoted_literal_no_implicit_widening_let() {
    let source = "
        func take_int(x: int) {}
        func main() {
            let a = 10
            take_int(a)
            let b: float = a
        }
    ";
    assert_type_errors!(source, [T015ImplicitWidening]);
}

#[test]
fn typ33_literal_incompatible_type() {
    let source = "
        func take_bool(b: bool) {}
        func main() {
            let a = 10
            take_bool(a)
        }
    ";
    assert_type_errors!(source, [T003IncompatibleCallArg]);
}

#[test]
fn typ33_struct_field_init_promotes_literal_var() {
    let source = "
        struct Point {
            x: u16,
            y: u16,
        }
        func main() {
            let a = 10
            let b = 20
            let p = Point { x: a, y: b }
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ33_return_stmt_promotes_literal_var() {
    let source = "
        func make_val(): i16 {
            let a = 42
            return a
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ33_tail_expr_promotes_literal_var() {
    let source = "
        func make_val(): i16 {
            let a = 42
            a
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ33_comparison_promotes_literal_vars() {
    let source = "
        func take_u32(x: u32) {}
        func main() {
            let a = 10
            let b = 20
            let c = a == b
            take_u32(a)
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ33_unary_neg_promotes_literal_var() {
    let source = "
        func take_i16(x: i16) {}
        func main() {
            let a = 10
            let b = -a
            take_i16(b)
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ33_promoted_literal_no_implicit_widening_unpinned_let() {
    // Regression: an unannotated literal that is promoted to a variable must
    // not silently widen to float just because no earlier constraint pinned it.
    let source = "
        func main() {
            let a = 10
            let b: float = a
        }
    ";
    assert_type_errors!(source, [T015ImplicitWidening]);
}

#[test]
fn typ33_promoted_literal_no_implicit_widening_call_arg() {
    let source = "
        func take_float(x: float) {}
        func main() {
            let a = 10
            take_float(a)
        }
    ";
    assert_type_errors!(source, [T015ImplicitWidening]);
}

#[test]
fn typ33_promoted_literal_no_implicit_widening_return() {
    let source = "
        func make_val(): float {
            let a = 10
            return a
        }
    ";
    assert_type_errors!(source, [T015ImplicitWidening]);
}

#[test]
fn typ33_promoted_literal_no_implicit_widening_binary_merge() {
    // Merging a promoted int literal with a promoted float literal must not
    // silently turn the int binding into a float.
    let source = "
        func main() {
            let a = 10
            let b = 2.5
            let c = a + b
        }
    ";
    assert_type_errors!(source, [T015ImplicitWidening]);
}

#[test]
fn typ33_raw_literal_to_float_is_allowed() {
    // Direct literals still coerce with the surrounding float context; only
    // promoted (variable-bound) literals are blocked from widening.
    let source = "
        func take_float(x: float) {}
        func main() {
            let x: float = 1
            take_float(2)
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ33_promoted_integer_literal_still_retypes() {
    let source = "
        func take_u64(x: u64) {}
        func main() {
            let a = 10
            take_u64(a)
        }
    ";
    assert_type_errors!(source, []);
}

#[test]
fn typ33_promoted_literal_no_implicit_widening_array() {
    // A promoted int literal mixed with a float literal in an array is the
    // same implicit widening as elsewhere, not a plain element mismatch.
    let source = "
        func main() {
            let a = 10
            let arr = [a, 2.5]
        }
    ";
    assert_type_errors!(source, [T015ImplicitWidening]);
}

#[test]
fn typ32_array_raw_int_float_literals_still_mismatch() {
    // Raw literals are not promoted, so this is a genuine array element
    // mismatch (T002), not an implicit widening.
    let source = "
        func main() {
            let arr = [1, 2.5]
        }
    ";
    assert_type_errors!(source, [T002IncompatibleAssignment]);
}
