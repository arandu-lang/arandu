#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Tests for operator and arithmetic semantics edge cases:
//! - Unsigned negation rejection (T005)
//! - Signed and float negation acceptance
//! - Division and modulo by literal zero rejection (T040)
//! - Runtime division and modulo acceptance
//! - Float modulo rejection (T005)
//! - Relational ordering on non-orderable types rejection (bool, str, struct, tuple) (T005)
//! - Relational ordering on orderable types acceptance (integers, floats, char)
//! - Shift count literal out of bounds rejection (T038)
//! - Shift within bounds acceptance
//! - Struct and tuple direct equality rejection (T005)
//! - Scalar equality acceptance (int, uint, bool, float, char, str)
//! - AMIR lowering preservation for compliant operations

use arandu_diagnostics::DiagCode;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{lower_amir, type_check};
use std::sync::atomic::{AtomicUsize, Ordering};

static FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn assert_ok(src: &str) {
    let mut db = DatabaseImpl::new();
    let idx = FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file = db.new_file(format!("edge_case_{idx}.aru"), src.into());
    let tc = type_check(&db, file);
    assert!(
        tc.diagnostics.is_empty(),
        "expected zero diagnostics for:\n{src}\nbut got:\n{:#?}",
        tc.diagnostics
    );
}

fn assert_has_diag(src: &str, expected_code: DiagCode) {
    let mut db = DatabaseImpl::new();
    let idx = FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file = db.new_file(format!("edge_case_{idx}.aru"), src.into());
    let parse_diags = arandu_query::passes::parse_diagnostics(&db, file);
    let tc = type_check(&db, file);
    let found = parse_diags.value.iter().any(|d| d.code == expected_code)
        || tc.diagnostics.iter().any(|d| d.code == expected_code);
    assert!(
        found,
        "expected diagnostic {:?} for:\n{src}\nbut got:\nparse: {:#?}\ntypeck: {:#?}",
        expected_code, parse_diags.value, tc.diagnostics
    );
}

fn assert_has_diag_with_hint(src: &str, expected_code: DiagCode, hint_contains: &str) {
    let mut db = DatabaseImpl::new();
    let idx = FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file = db.new_file(format!("edge_case_{idx}.aru"), src.into());
    let tc = type_check(&db, file);
    let found = tc.diagnostics.iter().find(|d| d.code == expected_code);
    assert!(
        found.is_some(),
        "expected diagnostic {:?} for:\n{src}\nbut got:\n{:#?}",
        expected_code,
        tc.diagnostics
    );
    let diag = found.unwrap();
    let has_hint = diag.hints.iter().any(|h| {
        h.message
            .to_lowercase()
            .contains(&hint_contains.to_lowercase())
    });
    assert!(
        has_hint,
        "diagnostic {:?} found, but none of its hints contained '{}':\n{:#?}",
        expected_code, hint_contains, diag.hints
    );
}

// ── 1. Unsigned Negation Rejection ──────────────────────────────────

#[test]
fn unsigned_negation_u8_rejected() {
    assert_has_diag_with_hint(
        "func f(x: u8): u8 { return -x }",
        DiagCode::T005OperatorNotApplicable,
        "unsigned",
    );
}

#[test]
fn unsigned_negation_u16_rejected() {
    assert_has_diag_with_hint(
        "func f(x: u16): u16 { return -x }",
        DiagCode::T005OperatorNotApplicable,
        "unsigned",
    );
}

#[test]
fn unsigned_negation_u32_rejected() {
    assert_has_diag_with_hint(
        "func f(x: u32): u32 { return -x }",
        DiagCode::T005OperatorNotApplicable,
        "unsigned",
    );
}

#[test]
fn unsigned_negation_u64_rejected() {
    assert_has_diag_with_hint(
        "func f(x: u64): u64 { return -x }",
        DiagCode::T005OperatorNotApplicable,
        "unsigned",
    );
}

#[test]
fn unsigned_negation_uint_rejected() {
    assert_has_diag_with_hint(
        "func f(x: uint): uint { return -x }",
        DiagCode::T005OperatorNotApplicable,
        "unsigned",
    );
}

#[test]
fn unsigned_negation_byte_rejected() {
    assert_has_diag_with_hint(
        "func f(x: byte): byte { return -x }",
        DiagCode::T005OperatorNotApplicable,
        "unsigned",
    );
}

// ── 2. Signed and Float Negation Accepted ───────────────────────────

#[test]
fn signed_negation_accepted() {
    assert_ok("func f(x: i8): i8 { return -x }");
    assert_ok("func f(x: i16): i16 { return -x }");
    assert_ok("func f(x: i32): i32 { return -x }");
    assert_ok("func f(x: i64): i64 { return -x }");
    assert_ok("func f(x: int): int { return -x }");
}

#[test]
fn float_negation_accepted() {
    assert_ok("func f(x: float): float { return -x }");
    assert_ok("func f(x: f32): f32 { return -x }");
    assert_ok("func f(x: f64): f64 { return -x }");
}

// ── 3. Literal Division / Modulo by Zero Rejection (T040) ───────────

#[test]
fn literal_int_div_by_zero_rejected() {
    assert_has_diag(
        "func f(): int { return 42 / 0 }",
        DiagCode::T040DivisionByZero,
    );
}

#[test]
fn literal_int_mod_by_zero_rejected() {
    assert_has_diag(
        "func f(): int { return 42 % 0 }",
        DiagCode::T040DivisionByZero,
    );
}

#[test]
fn literal_float_div_by_zero_preserves_ieee754_semantics() {
    assert_ok("func f(): float { return 42.0 / 0.0 }");
}

#[test]
fn literal_float_mod_by_zero_rejected() {
    assert_has_diag(
        "func f(): float { return 42.0 % 0.0 }",
        DiagCode::T005OperatorNotApplicable,
    );
}

// ── 4. Runtime Division and Modulo Accepted ─────────────────────────

#[test]
fn runtime_int_div_mod_accepted() {
    assert_ok("func f(a: int, b: int): int { return a / b }");
    assert_ok("func f(a: int, b: int): int { return a % b }");
    assert_ok("func f(): int { return 42 / 2 }");
    assert_ok("func f(): int { return 42 % 5 }");
}

#[test]
fn runtime_float_div_accepted() {
    assert_ok("func f(a: float, b: float): float { return a / b }");
    assert_ok("func f(): float { return 42.0 / 2.0 }");
}

// ── 5. Float Modulo Rejection (T005) ────────────────────────────────

#[test]
fn float_modulo_runtime_rejected() {
    assert_has_diag_with_hint(
        "func f(a: float, b: float): float { return a % b }",
        DiagCode::T005OperatorNotApplicable,
        "integer",
    );
}

#[test]
fn f32_modulo_rejected() {
    assert_has_diag_with_hint(
        "func f(a: f32, b: f32): f32 { return a % b }",
        DiagCode::T005OperatorNotApplicable,
        "integer",
    );
}

#[test]
fn f64_modulo_rejected() {
    assert_has_diag_with_hint(
        "func f(a: f64, b: f64): f64 { return a % b }",
        DiagCode::T005OperatorNotApplicable,
        "integer",
    );
}

#[test]
fn float_modulo_literal_non_zero_rejected() {
    assert_has_diag_with_hint(
        "func f(): float { return 5.5 % 2.0 }",
        DiagCode::T005OperatorNotApplicable,
        "integer",
    );
}

// ── 6. Relational Comparisons on Bool Rejected (T005) ───────────────

#[test]
fn bool_relational_lt_rejected() {
    assert_has_diag_with_hint(
        "func f(a: bool, b: bool): bool { return a < b }",
        DiagCode::T005OperatorNotApplicable,
        "bool",
    );
}

#[test]
fn bool_relational_le_rejected() {
    assert_has_diag_with_hint(
        "func f(a: bool, b: bool): bool { return a <= b }",
        DiagCode::T005OperatorNotApplicable,
        "bool",
    );
}

#[test]
fn bool_relational_gt_rejected() {
    assert_has_diag_with_hint(
        "func f(a: bool, b: bool): bool { return a > b }",
        DiagCode::T005OperatorNotApplicable,
        "bool",
    );
}

#[test]
fn bool_relational_ge_rejected() {
    assert_has_diag_with_hint(
        "func f(a: bool, b: bool): bool { return a >= b }",
        DiagCode::T005OperatorNotApplicable,
        "bool",
    );
}

// ── 7. Relational Comparisons on String Rejected (T005) ─────────────

#[test]
fn str_relational_lt_rejected() {
    assert_has_diag_with_hint(
        "func f(a: str, b: str): bool { return a < b }",
        DiagCode::T005OperatorNotApplicable,
        "string",
    );
}

#[test]
fn str_relational_le_rejected() {
    assert_has_diag_with_hint(
        "func f(a: str, b: str): bool { return a <= b }",
        DiagCode::T005OperatorNotApplicable,
        "string",
    );
}

#[test]
fn str_relational_gt_rejected() {
    assert_has_diag_with_hint(
        "func f(a: str, b: str): bool { return a > b }",
        DiagCode::T005OperatorNotApplicable,
        "string",
    );
}

#[test]
fn str_relational_ge_rejected() {
    assert_has_diag_with_hint(
        "func f(a: str, b: str): bool { return a >= b }",
        DiagCode::T005OperatorNotApplicable,
        "string",
    );
}

// ── 8. Relational Comparisons on Orderable Types Accepted ───────────

#[test]
fn integer_relational_comparisons_accepted() {
    assert_ok("func f(a: int, b: int): bool { return a < b }");
    assert_ok("func f(a: uint, b: uint): bool { return a <= b }");
    assert_ok("func f(a: i8, b: i8): bool { return a > b }");
    assert_ok("func f(a: u8, b: u8): bool { return a >= b }");
    assert_ok("func f(a: i16, b: i16): bool { return a < b }");
    assert_ok("func f(a: u16, b: u16): bool { return a <= b }");
    assert_ok("func f(a: i32, b: i32): bool { return a > b }");
    assert_ok("func f(a: u32, b: u32): bool { return a >= b }");
    assert_ok("func f(a: i64, b: i64): bool { return a < b }");
    assert_ok("func f(a: u64, b: u64): bool { return a <= b }");
    assert_ok("func f(a: byte, b: byte): bool { return a > b }");
}

#[test]
fn float_relational_comparisons_accepted() {
    assert_ok("func f(a: float, b: float): bool { return a < b }");
    assert_ok("func f(a: f32, b: f32): bool { return a <= b }");
    assert_ok("func f(a: f64, b: f64): bool { return a > b }");
}

#[test]
fn char_relational_comparisons_accepted() {
    assert_ok("func f(a: char, b: char): bool { return a < b }");
    assert_ok("func f(a: char, b: char): bool { return a <= b }");
    assert_ok("func f(a: char, b: char): bool { return a > b }");
    assert_ok("func f(a: char, b: char): bool { return a >= b }");
}

// ── 9. Literal Shift Out of Bounds Rejected (T038) ──────────────────

#[test]
fn shift_u8_out_of_bounds_rejected() {
    assert_has_diag(
        "func f(x: u8): u8 { return x << 8 }",
        DiagCode::T038IntegerLiteralOutOfRange,
    );
    assert_has_diag(
        "func f(x: u8): u8 { return x >> 8 }",
        DiagCode::T038IntegerLiteralOutOfRange,
    );
}

#[test]
fn shift_u16_out_of_bounds_rejected() {
    assert_has_diag(
        "func f(x: u16): u16 { return x << 16 }",
        DiagCode::T038IntegerLiteralOutOfRange,
    );
}

#[test]
fn shift_u32_out_of_bounds_rejected() {
    assert_has_diag(
        "func f(x: u32): u32 { return x << 32 }",
        DiagCode::T038IntegerLiteralOutOfRange,
    );
}

#[test]
fn shift_u64_out_of_bounds_rejected() {
    assert_has_diag(
        "func f(x: u64): u64 { return x << 64 }",
        DiagCode::T038IntegerLiteralOutOfRange,
    );
}

#[test]
fn shift_i32_out_of_bounds_rejected() {
    assert_has_diag(
        "func f(x: i32): i32 { return x << 32 }",
        DiagCode::T038IntegerLiteralOutOfRange,
    );
    assert_has_diag(
        "func f(x: i32): i32 { return x >> 100 }",
        DiagCode::T038IntegerLiteralOutOfRange,
    );
}

// ── 10. Valid Shift Operations Accepted ─────────────────────────────

#[test]
fn valid_shifts_accepted() {
    assert_ok("func f(x: u8): u8 { return x << 7 }");
    assert_ok("func f(x: u16): u16 { return x << 15 }");
    assert_ok("func f(x: u32): u32 { return x << 31 }");
    assert_ok("func f(x: u64): u64 { return x << 63 }");
    assert_ok("func f(x: int, y: int): int { return x << y }");
    assert_ok("func f(x: int, y: int): int { return x >> y }");
    assert_ok("func f(x: i32): i32 { return x << 0 }");
}

// ── 11. Composite Direct Equality Rejection (T005) ──────────────────

#[test]
fn struct_direct_equality_rejected() {
    assert_has_diag_with_hint(
        "struct Point { x: int, y: int }\nfunc f(p1: Point, p2: Point): bool { return p1 == p2 }",
        DiagCode::T005OperatorNotApplicable,
        "composite",
    );
    assert_has_diag_with_hint(
        "struct Point { x: int, y: int }\nfunc f(p1: Point, p2: Point): bool { return p1 != p2 }",
        DiagCode::T005OperatorNotApplicable,
        "composite",
    );
}

#[test]
fn struct_direct_inequality_rejected() {
    assert_has_diag_with_hint(
        "struct Box { val: int }\nfunc f(b1: Box, b2: Box): bool { return b1 != b2 }",
        DiagCode::T005OperatorNotApplicable,
        "composite",
    );
}

// ── 12. Scalar Equality Accepted ────────────────────────────────────

#[test]
fn scalar_equality_accepted() {
    assert_ok("func f(a: int, b: int): bool { return a == b }");
    assert_ok("func f(a: uint, b: uint): bool { return a != b }");
    assert_ok("func f(a: bool, b: bool): bool { return a == b }");
    assert_ok("func f(a: float, b: float): bool { return a != b }");
    assert_ok("func f(a: char, b: char): bool { return a == b }");
    assert_ok("func f(a: str, b: str): bool { return a == b }");
}

// ── 13. AMIR Lowering Preservation for Valid Arithmetic ─────────────

#[test]
fn amir_lowering_preserves_arithmetic_ops() {
    let src = r#"
func arith(a: int, b: int): int {
    let sum: int = a + b
    let diff: int = a - b
    let prod: int = a * b
    let quot: int = a / b
    let rem: int = a % b
    return sum + diff + prod + quot + rem
}
"#;
    let mut db = DatabaseImpl::new();
    let idx = FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file = db.new_file(format!("edge_case_{idx}.aru"), src.into());
    let tc = type_check(&db, file);
    assert!(tc.diagnostics.is_empty(), "{:?}", tc.diagnostics);
    let artifacts = lower_amir(&db, file);
    assert_eq!(artifacts.amir.funcs.len(), 1);
    let func = &artifacts.amir.funcs[0];
    assert!(!func.stmts.is_empty());
}

// ── 14. Negative Shift Count Literal Rejected (T038) ────────────────

#[test]
fn negative_shift_rejected() {
    assert_has_diag(
        "func f(x: int): int { return x << -1 }",
        DiagCode::T038IntegerLiteralOutOfRange,
    );
    assert_has_diag(
        "func f(x: i32): i32 { return x >> -5 }",
        DiagCode::T038IntegerLiteralOutOfRange,
    );
}

// ── 15. Bitwise Operations on Integers Accepted ─────────────────────

#[test]
fn bitwise_ops_on_integers_accepted() {
    assert_ok("func f(a: int, b: int): int { return a & b }");
    assert_ok("func f(a: int, b: int): int { return a | b }");
    assert_ok("func f(a: int, b: int): int { return a ^ b }");
    assert_ok("func f(a: int): int { return ~a }");
    assert_ok("func f(a: u32, b: u32): u32 { return a & b }");
    assert_ok("func f(a: u32, b: u32): u32 { return a | b }");
    assert_ok("func f(a: u32, b: u32): u32 { return a ^ b }");
    assert_ok("func f(a: u32): u32 { return ~a }");
}

// ── 16. Bitwise Operations on Non-Integers Rejected (T005) ──────────

#[test]
fn bitwise_and_on_floats_rejected() {
    assert_has_diag(
        "func f(a: float, b: float): float { return a & b }",
        DiagCode::T005OperatorNotApplicable,
    );
}

#[test]
fn bitwise_or_on_floats_rejected() {
    assert_has_diag(
        "func f(a: float, b: float): float { return a | b }",
        DiagCode::T005OperatorNotApplicable,
    );
}

#[test]
fn bitwise_xor_on_bools_rejected() {
    assert_has_diag(
        "func f(a: bool, b: bool): bool { return a ^ b }",
        DiagCode::T005OperatorNotApplicable,
    );
}

#[test]
fn bitwise_not_on_float_rejected() {
    assert_has_diag(
        "func f(a: float): float { return ~a }",
        DiagCode::T005OperatorNotApplicable,
    );
}

// ── 17. Explicit Numeric and Float Casts Accepted ───────────────────

#[test]
fn explicit_numeric_casts_accepted() {
    assert_ok("func f(x: int): float { return x as float }");
    assert_ok("func f(x: float): int { return x as int }");
    assert_ok("func f(x: f32): f64 { return x as f64 }");
    assert_ok("func f(x: f64): f32 { return x as f32 }");
    assert_ok("func f(x: u32): i64 { return x as i64 }");
    assert_ok("func f(x: i64): u32 { return x as u32 }");
}

// ── 18. Pointer Comparison Semantics Accepted ───────────────────────

#[test]
fn pointer_comparisons_accepted() {
    assert_ok("func f(p1: ptr[u8], p2: ptr[u8]): bool { return p1 == p2 }");
    assert_ok("func f(p1: ptr[u8], p2: ptr[u8]): bool { return p1 != p2 }");
    assert_has_diag(
        "func f(p1: ptr[u8], p2: ptr[u8]): bool { return p1 < p2 }",
        DiagCode::T005OperatorNotApplicable,
    );
    assert_has_diag(
        "func f(p1: ptr[u8], p2: ptr[u8]): bool { return p1 <= p2 }",
        DiagCode::T005OperatorNotApplicable,
    );
    assert_has_diag(
        "func f(p1: ptr[u8], p2: ptr[u8]): bool { return p1 > p2 }",
        DiagCode::T005OperatorNotApplicable,
    );
    assert_has_diag(
        "func f(p1: ptr[u8], p2: ptr[u8]): bool { return p1 >= p2 }",
        DiagCode::T005OperatorNotApplicable,
    );
}

// ── 19. Trojan Source (BiDi Unicode) Rejection (CWE-1307) ───────────

#[test]
fn trojan_source_bidi_rejected() {
    assert_has_diag(
        "// test \u{202E} override\nfunc f(): int { return 0 }",
        DiagCode::LX004BidiTrojanSource,
    );
    assert_has_diag(
        "/* block \u{2066} isolate */\nfunc f(): int { return 0 }",
        DiagCode::LX004BidiTrojanSource,
    );
    assert_has_diag(
        "func f(): str { return \"hello \u{202E} world\" }",
        DiagCode::LX004BidiTrojanSource,
    );
}
