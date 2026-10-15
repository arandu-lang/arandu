#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use arandu_semantics::{
    DiagCode, Diagnostic, Severity, lower_to_amir, lower_to_amir_with_interfaces, lower_to_hir,
    resolve_for_test, type_check,
};
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelLoanKind {
    Shared,
    Exclusive,
}

impl ModelLoanKind {
    const ALL: [Self; 2] = [Self::Shared, Self::Exclusive];

    const fn conflicts_with(self, other: Self) -> bool {
        matches!(self, Self::Exclusive) || matches!(other, Self::Exclusive)
    }

    const fn type_syntax(self) -> &'static str {
        match self {
            Self::Shared => "ref int",
            Self::Exclusive => "mut ref int",
        }
    }

    const fn expression_syntax(self) -> &'static str {
        match self {
            Self::Shared => "&value",
            Self::Exclusive => "&mut value",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ModelCfgShape {
    Linear,
    Diamond,
}

impl ModelCfgShape {
    const ALL: [Self; 2] = [Self::Linear, Self::Diamond];
}

fn emit_two_loan_program(
    first: ModelLoanKind,
    second: ModelLoanKind,
    shape: ModelCfgShape,
) -> String {
    let mut source = String::with_capacity(512);
    writeln!(
        source,
        "func consume(first: {}, second: {}): int {{ return *first + *second }}",
        first.type_syntax(),
        second.type_syntax()
    )
    .unwrap();
    writeln!(source, "func main(): int {{").unwrap();
    writeln!(source, "    let mut value = 21").unwrap();
    writeln!(source, "    let first = {}", first.expression_syntax()).unwrap();
    match shape {
        ModelCfgShape::Linear => {
            writeln!(source, "    let second = {}", second.expression_syntax()).unwrap();
            writeln!(source, "    return consume(first, second)").unwrap();
        }
        ModelCfgShape::Diamond => {
            writeln!(source, "    if value > 0 {{").unwrap();
            writeln!(
                source,
                "        let second = {}",
                second.expression_syntax()
            )
            .unwrap();
            writeln!(source, "        return consume(first, second)").unwrap();
            writeln!(source, "    }} else {{").unwrap();
            writeln!(
                source,
                "        let second = {}",
                second.expression_syntax()
            )
            .unwrap();
            writeln!(source, "        return consume(first, second)").unwrap();
            writeln!(source, "    }}").unwrap();
        }
    }
    writeln!(source, "}}").unwrap();
    source
}

fn ownership_diagnostics(src: &str) -> Vec<Diagnostic> {
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let type_errors: Vec<_> = tc
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect();
    assert!(type_errors.is_empty(), "typeck errors: {type_errors:?}");
    let hir = lower_to_hir(&mut tc, &program).expect("hir");
    match lower_to_amir_with_interfaces(&mut tc, &hir, 8) {
        Ok((_, diagnostics)) | Err(diagnostics) => diagnostics,
    }
}

fn ownership_codes(src: &str) -> Vec<DiagCode> {
    let mut codes: Vec<_> = ownership_diagnostics(src)
        .into_iter()
        .filter_map(|diagnostic| match diagnostic.code {
            code @ (DiagCode::O001UseAfterMove
            | DiagCode::O002MoveWhileBorrowed
            | DiagCode::O003MutableBorrowConflict
            | DiagCode::O004GenerationalFallback
            | DiagCode::O005DoubleFree
            | DiagCode::O006DestroyWhileBorrowed
            | DiagCode::O007InconsistentMoveBetweenBranches
            | DiagCode::O008UseBeforeInit
            | DiagCode::O010EscapeOfBorrowedValue) => Some(code),
            _ => None,
        })
        .collect();
    codes.sort_by_key(|code| code.as_str());
    codes.dedup();
    codes
}

fn assert_deterministic(case: &str, source: &str, expected: &[DiagCode]) {
    let first = ownership_codes(source);
    assert_eq!(first, ownership_codes(source), "{case}: result changed");
    assert_eq!(
        first, expected,
        "{case}: unexpected ownership classification"
    );
}

fn assert_cfg_equivalent(case: &str, left: &str, right: &str, expected: &[DiagCode]) {
    let first = ownership_codes(left);
    let repeated = ownership_codes(left);
    let transformed = ownership_codes(right);
    assert_eq!(first, repeated, "{case}: result was not deterministic");
    assert_eq!(
        first, transformed,
        "{case}: equivalent CFG shapes disagreed"
    );
    assert_eq!(
        first, expected,
        "{case}: unexpected ownership classification"
    );
}

#[test]
fn two_loan_model_matches_all_kinds_and_cfg_shapes() {
    for first in ModelLoanKind::ALL {
        for second in ModelLoanKind::ALL {
            let expected: &[_] = if first.conflicts_with(second) {
                &[DiagCode::O003MutableBorrowConflict]
            } else {
                &[]
            };
            let linear = emit_two_loan_program(first, second, ModelCfgShape::Linear);
            let diamond = emit_two_loan_program(first, second, ModelCfgShape::Diamond);
            assert_cfg_equivalent(
                &format!("{first:?}/{second:?}"),
                &linear,
                &diamond,
                expected,
            );
        }
    }

    for shape in ModelCfgShape::ALL {
        let source = emit_two_loan_program(ModelLoanKind::Shared, ModelLoanKind::Shared, shape);
        assert_deterministic("shared/shared", &source, &[]);
    }
}

#[test]
fn borrow_of_one_field_does_not_block_a_disjoint_field() {
    let source = r#"
struct Pair { left: int right: int }
func main(): int {
    let mut pair = Pair { left: 20, right: 21 }
    let left = &pair.left
    set pair.right = 22
    return *left + pair.right
}
"#;
    assert_deterministic("disjoint struct fields", source, &[]);
}

#[test]
fn nested_field_paths_follow_prefix_and_sibling_alias_rules() {
    let disjoint_nested = r#"
struct Pair { left: int right: int }
struct Outer { first: Pair second: Pair }
func sum(left: mut ref int, right: mut ref int): int { return *left + *right }
func main(): int {
    let first = Pair { left: 20, right: 21 }
    let second = Pair { left: 1, right: 2 }
    let mut outer = Outer { first, second }
    let left = &mut outer.first.left
    let right = &mut outer.first.right
    set outer.second.left = 3
    return sum(left, right) + outer.second.left
}
"#;
    assert_deterministic("nested sibling fields", disjoint_nested, &[]);

    let parent_and_child = r#"
struct Pair { left: int right: int }
func sum(pair: ref Pair, left: mut ref int): int { return pair.right + *left }
func main(): int {
    let mut pair = Pair { left: 20, right: 22 }
    let whole = &pair
    let left = &mut pair.left
    return sum(whole, left)
}
"#;
    assert_deterministic(
        "parent and child paths",
        parent_and_child,
        &[DiagCode::O003MutableBorrowConflict],
    );
}

#[test]
fn moving_a_field_checks_only_overlapping_loans() {
    let disjoint = r#"
struct Resource { handle: ptr[u8] }
struct Pair { left: Resource right: Resource }
func consume(own text: Resource): int { return 1 }
func main(): int {
    let pair = Pair { left: Resource { handle: nil }, right: Resource { handle: nil } }
    let loan = &pair.left
    let result = consume(pair.right)
    let keepAlive = loan.handle
    return result
}
"#;
    assert_deterministic("move disjoint field", disjoint, &[]);

    let overlapping = r#"
struct Resource { handle: ptr[u8] }
struct Pair { left: Resource right: Resource }
func consume(own text: Resource): int { return 1 }
func main(): int {
    let pair = Pair { left: Resource { handle: nil }, right: Resource { handle: nil } }
    let loan = &pair.left
    let result = consume(pair.left)
    let keepAlive = loan.handle
    return result
}
"#;
    assert_deterministic(
        "move borrowed field",
        overlapping,
        &[DiagCode::O002MoveWhileBorrowed],
    );

    let reuse_same_field_after_move = r#"
struct Resource { handle: ptr[u8] }
struct Pair { left: Resource right: Resource }
func consume(own text: Resource): int { return 1 }
func main(): int {
    let pair = Pair { left: Resource { handle: nil }, right: Resource { handle: nil } }
    let result = consume(pair.left)
    return consume(pair.left) + result
}
"#;
    assert_deterministic(
        "reuse same field after move",
        reuse_same_field_after_move,
        &[DiagCode::O001UseAfterMove],
    );
    assert_eq!(
        ownership_diagnostics(reuse_same_field_after_move)
            .iter()
            .filter(|diagnostic| diagnostic.code == DiagCode::O001UseAfterMove)
            .count(),
        1,
        "one invalid source move must produce one O001 diagnostic"
    );

    let use_sibling_after_field_move = r#"
struct Resource { handle: ptr[u8] }
struct Pair { left: Resource right: Resource }
func consume(own text: Resource): int { return 1 }
func main(): int {
    let pair = Pair { left: Resource { handle: nil }, right: Resource { handle: nil } }
    let result = consume(pair.left)
    return consume(pair.right) + result
}
"#;
    assert_deterministic(
        "use sibling after field move",
        use_sibling_after_field_move,
        &[],
    );
}

#[test]
fn partial_field_moves_join_and_reinitialize_by_path() {
    let sibling_after_partial_branch = r#"
struct Resource { handle: ptr[u8] }
struct Pair { left: Resource right: Resource }
func consume(own text: Resource): int { return 1 }
func main(): int {
    let pair = Pair { left: Resource { handle: nil }, right: Resource { handle: nil } }
    let mut result = 0
    if result == 0 { result = consume(pair.left) } else { result = 2 }
    return consume(pair.right) + result
}
"#;
    assert_deterministic(
        "sibling after partial branch move",
        sibling_after_partial_branch,
        &[],
    );

    let same_field_after_partial_branch = r#"
struct Resource { handle: ptr[u8] }
struct Pair { left: Resource right: Resource }
func consume(own text: Resource): int { return 1 }
func main(): int {
    let pair = Pair { left: Resource { handle: nil }, right: Resource { handle: nil } }
    let mut result = 0
    if result == 0 { result = consume(pair.left) } else { result = 2 }
    return consume(pair.left) + result
}
"#;
    assert_deterministic(
        "same field after partial branch move",
        same_field_after_partial_branch,
        &[DiagCode::O007InconsistentMoveBetweenBranches],
    );

    let reinitialized_field = r#"
struct Resource { handle: ptr[u8] }
struct Pair { left: Resource right: Resource }
func consume(own text: Resource): int { return 1 }
func main(): int {
    let mut pair = Pair { left: Resource { handle: nil }, right: Resource { handle: nil } }
    let first = consume(pair.left)
    set pair.left = Resource { handle: nil }
    return consume(pair.left) + first
}
"#;
    assert_deterministic("reinitialized moved field", reinitialized_field, &[]);
}

#[test]
fn o003_conflicting_borrows_end_to_end() {
    let src = r#"
func use_both(a: &mut int, b: &int): int {
    return *a
}
func main(): int {
    let n = 1
    let a = &mut n
    let b = &n
    return use_both(a, b)
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let errors: Vec<_> = tc
        .diagnostics
        .iter()
        .filter(|d| d.severity == arandu_semantics::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "typeck errors: {errors:?}");
    let hir = lower_to_hir(&mut tc, &program).expect("hir");
    let result = lower_to_amir(&tc, &hir, 8);
    match result {
        Ok(_) => panic!("expected O003 from lower_to_amir"),
        Err(diags) => {
            assert!(
                diags
                    .iter()
                    .any(|d| d.code == DiagCode::O003MutableBorrowConflict),
                "expected O003, got {diags:?}"
            );
        }
    }
}

#[test]
fn return_ref_to_local_is_o010() {
    let src = r#"
func bad(): &int {
    let x = 1
    return &x
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let errors: Vec<_> = tc
        .diagnostics
        .iter()
        .filter(|d| d.severity == arandu_semantics::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "typeck errors: {errors:?}");
    let hir = lower_to_hir(&mut tc, &program).expect("hir");
    let result = lower_to_amir(&tc, &hir, 8);
    match result {
        Ok(_) => panic!("expected O010 from lower_to_amir"),
        Err(diags) => {
            assert!(
                diags
                    .iter()
                    .any(|d| d.code == DiagCode::O010EscapeOfBorrowedValue),
                "expected O010, got {diags:?}"
            );
            assert!(
                diags
                    .iter()
                    .any(|d| d.code == DiagCode::O004GenerationalFallback),
                "expected O004 note, got {diags:?}"
            );
        }
    }
}

#[test]
fn sequential_borrows_ok() {
    let src = r#"
func main(): int {
    let n = 5
    let a = &n
    let x = *a
    let b = &mut n
    let y = *b
    return x
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        !tc.diagnostics
            .iter()
            .any(|d| d.severity == arandu_semantics::Severity::Error),
        "typeck: {:?}",
        tc.diagnostics
    );
    let hir = lower_to_hir(&mut tc, &program).expect("hir");
    let amir = lower_to_amir(&tc, &hir, 8).expect("sequential borrows should lower");
    assert!(!amir.funcs.is_empty());
}

#[test]
fn ownership_classification_is_stable_across_equivalent_cfg_shapes() {
    let conflict_linear = r#"
func useBoth(a: mut ref int, b: ref int): int { return *a + *b }
func main(): int {
    let mut value = 1
    let exclusive = &mut value
    let loan = &value
    return useBoth(exclusive, loan)
}
"#;
    let conflict_diamond = r#"
func useBoth(a: mut ref int, b: ref int): int { return *a + *b }
func main(): int {
    let mut value = 1
    let exclusive = &mut value
    if value > 0 {
        let loan = &value
        return useBoth(exclusive, loan)
    } else {
        let loan = &value
        return useBoth(exclusive, loan)
    }
}
"#;
    assert_cfg_equivalent(
        "exclusive/shared diamond",
        conflict_linear,
        conflict_diamond,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let valid_linear = r#"
func main(): int {
    let value = 7
    let loan = &value
    return *loan
}
"#;
    let valid_diamond = r#"
func main(): int {
    let value = 7
    let loan = &value
    let mut result = 0
    if value > 0 { result = *loan } else { result = *loan }
    return result
}
"#;
    assert_cfg_equivalent("valid shared diamond", valid_linear, valid_diamond, &[]);

    let mutation_linear = r#"
func main(): int {
    let mut value = 7
    let loan = &value
    value = 8
    return *loan
}
"#;
    let mutation_diamond = r#"
func main(): int {
    let mut value = 7
    let loan = &value
    if value > 0 { value = 8 } else { value = 8 }
    return *loan
}
"#;
    assert_cfg_equivalent(
        "mutation diamond",
        mutation_linear,
        mutation_diamond,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let mutation_loop = r#"
func main(): int {
    let mut value = 7
    let loan = &value
    let mut once = true
    while once {
        value = 8
        once = false
    }
    return *loan
}
"#;
    assert_cfg_equivalent(
        "mutation loop",
        mutation_linear,
        mutation_loop,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let mutation_match = r#"
func main(): int {
    let mut value = 7
    let loan = &value
    match value {
        7 => { value = 8 }
        _ => { value = 8 }
    }
    return *loan
}
"#;
    assert_cfg_equivalent(
        "mutation match",
        mutation_linear,
        mutation_match,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let mutation_struct_carrier = r#"
struct Holder { item: ref int }
func main(): int {
    let mut value = 7
    let holder = Holder { item: &value }
    value = 8
    return *holder.item
}
"#;
    assert_deterministic(
        "struct carrier",
        mutation_struct_carrier,
        &[DiagCode::O004GenerationalFallback],
    );

    let mutation_call_result = r#"
func borrowValue(value: ref int): ref int { return value }
func main(): int {
    let mut value = 7
    let loan = borrowValue(value)
    value = 8
    return *loan
}
"#;
    assert_cfg_equivalent(
        "call result",
        mutation_linear,
        mutation_call_result,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let partial_move_diamond = r#"
struct Box { handle: ptr[u8] }
func consume(own item: Box): int { return 7 }
func main(): int {
    let item = Box { handle: nil }
    let mut result = 0
    if result == 0 { result = consume(item) } else { result = 1 }
    return consume(item) + result
}
"#;
    let partial_move_match = r#"
struct Box { handle: ptr[u8] }
func consume(own item: Box): int { return 7 }
func main(): int {
    let item = Box { handle: nil }
    let mut result = 0
    match result {
        0 => { result = consume(item) }
        _ => { result = 1 }
    }
    return consume(item) + result
}
"#;
    assert_cfg_equivalent(
        "partial move join",
        partial_move_diamond,
        partial_move_match,
        &[DiagCode::O007InconsistentMoveBetweenBranches],
    );
}
