//! Cascade Torture Test Suite for Arandu compiler and runtime.
//!
//! Stages:
//! - Stage 1: Extreme Arithmetic & Numbers
//! - Stage 2: Strings, Text & Hostile Unicode
//! - Stage 3: Parser, Syntax & Deep Nesting
//! - Stage 4: Types, Scope Resolution & Collections
//! - Stage 5: Ownership, Borrowing & Drop Glue
//! - Stage 6: Structured Parallelism & Concurrency
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn temp_fixture(name: &str, source: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let directory =
        std::env::temp_dir().join(format!("arandu-torture-{}-{id}", std::process::id()));
    fs::create_dir_all(&directory).expect("create isolated test directory");
    let path = directory.join(name);
    fs::write(&path, source).expect("write test source");
    path
}

fn check_fixture(path: &Path) -> Output {
    common::cli_command()
        .args(["check", path.to_str().expect("valid path")])
        .output()
        .expect("run arandu check")
}

fn run_fixture(path: &Path) -> Output {
    common::cli_command()
        .args(["run", path.to_str().expect("valid path")])
        .output()
        .expect("run arandu run")
}

fn cleanup_fixture(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir_all(parent);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// ETAPA 1: NÚMEROS E ARITMÉTICA
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn stage1_const_eval_division_by_zero_int_is_rejected_with_t040() {
    let fixture = temp_fixture(
        "div_zero_int.aru",
        r#"module div_zero_int
func main(): int {
    let x = 10 / 0
    return x
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "division by zero literal must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("T040"),
        "expected T040DivisionByZero, got: {stderr}"
    );
}

#[test]
fn stage1_const_eval_division_by_zero_float_is_rejected_with_t040() {
    let fixture = temp_fixture(
        "div_zero_float.aru",
        r#"module div_zero_float
func main(): int {
    let x: float = 1.0 / 0.0
    return 0
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "float division by 0.0 literal must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("T040"),
        "expected T040DivisionByZero, got: {stderr}"
    );
}

#[test]
fn stage1_runtime_float_division_produces_ieee754_special_values() {
    let fixture = temp_fixture(
        "runtime_float.aru",
        r#"module runtime_float
func main(): int {
    let zero: float = 0.0
    let pinf = 1.0 / zero
    let ninf = -1.0 / zero
    let nan = 0.0 / zero

    // IEEE 754: pinf != ninf
    if pinf == ninf {
        return 1
    }
    // IEEE 754: nan == nan must be false!
    if nan == nan {
        return 2
    }
    // IEEE 754: nan != nan must be true!
    if !(nan != nan) {
        return 3
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "runtime float IEEE-754 failed: {stderr}"
    );
}

#[test]
fn stage1_float_precision_01_plus_02() {
    let fixture = temp_fixture(
        "float_prec.aru",
        r#"module float_prec
func main(): int {
    let a: float = 0.1
    let b: float = 0.2
    let sum = a + b
    let target: float = 0.3
    // In IEEE-754 double precision, 0.1 + 0.2 != 0.3
    if sum == target {
        return 1
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "float precision 0.1 + 0.2 failed: {stderr}"
    );
}

#[test]
fn stage1_negative_zero_comparisons() {
    let fixture = temp_fixture(
        "neg_zero.aru",
        r#"module neg_zero
func main(): int {
    let pz: float = 0.0
    let nz: float = -0.0
    // In IEEE-754, -0.0 == +0.0 is true
    if !(pz == nz) {
        return 1
    }
    // But 1.0 / +0.0 is +inf and 1.0 / -0.0 is -inf
    let pinf = 1.0 / pz
    let ninf = 1.0 / nz
    if pinf == ninf {
        return 2
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "negative zero comparison failed: {stderr}"
    );
}

#[test]
fn stage1_integer_literal_overflow_i8_and_u8_rejected_with_t038() {
    for (val, ty) in [
        ("128", "i8"),
        ("-129", "i8"),
        ("256", "u8"),
        ("32768", "i16"),
        ("65536", "u16"),
        ("2147483648", "i32"),
    ] {
        let fixture = temp_fixture(
            "overflow.aru",
            &format!(
                r#"module overflow
func main(): int {{
    let x: {ty} = {val}
    return 0
}}
"#
            ),
        );
        let output = check_fixture(&fixture);
        cleanup_fixture(&fixture);

        assert!(
            !output.status.success(),
            "literal {val} for type {ty} must be rejected"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("T038"),
            "expected T038IntegerLiteralOutOfRange for {val}:{ty}, got: {stderr}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// ETAPA 2: STRINGS, TEXTO & UNICODE HOSTIL
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn stage2_empty_string_and_whitespace_operations() {
    let fixture = temp_fixture(
        "empty_str.aru",
        r#"module empty_str

import std.core.str as s

func main(): int {
    let empty = ""
    if !s.isEmpty(empty) {
        return 1
    }
    let ws = "   \t\r\n   "
    let concat = "${empty}${ws}${empty}"
    if s.lenBytes(concat) != s.lenBytes(ws) {
        return 2
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "empty string operations failed: {stderr}"
    );
}

#[test]
fn stage2_null_byte_inside_string_is_preserved() {
    let fixture = temp_fixture(
        "null_byte.aru",
        r#"module null_byte

import std.core.str as s

func main(): int {
    let str_val = "abc\0def"
    // Must NOT truncate at \0 like C strlen
    if s.lenBytes(str_val) == 3 {
        return 1
    }
    if s.lenBytes(str_val) != 8 {
        return 2
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "string with null byte failed: {stderr}"
    );
}

#[test]
fn stage2_complex_unicode_emojis_cjk_combining() {
    let fixture = temp_fixture(
        "unicode.aru",
        r#"module unicode

import std.core.str as s

func main(): int {
    let emojis = "🦀🔥🚀"
    let cjk = "日本語・中文・한국어"
    let combining = "e\u{0301}"
    let combined = "${emojis} ${cjk} ${combining}"
    if s.lenBytes(combined) == 0 {
        return 1
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "complex unicode failed: {stderr}"
    );
}

#[test]
fn stage2_trojan_source_bidi_character_is_rejected_with_lx004() {
    // U+202E is Right-to-Left Override (Trojan Source, CWE-1307)
    let fixture = temp_fixture(
        "bidi_trojan.aru",
        "module bidi\nfunc main(): int {\n    let s = \"abc\u{202E}def\"\n    return 0\n}\n",
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "Trojan source bidi character must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LX004"),
        "expected LX004BidiTrojanSource, got: {stderr}"
    );
}

#[test]
fn stage2_unterminated_string_literal_rejected_with_lx001() {
    let fixture = temp_fixture(
        "unterminated.aru",
        "module unterminated\nfunc main(): int {\n    let s = \"hello world\n    return 0\n}\n",
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "unterminated string literal must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LX001") || stderr.contains("P001"),
        "expected LX001 or P001, got: {stderr}"
    );
}

#[test]
fn stage2_invalid_unicode_escape_rejected_with_lx002() {
    // Without braces \u0301 is invalid in Arandu
    let fixture = temp_fixture(
        "invalid_escape.aru",
        "module invalid_escape\nfunc main(): int {\n    let s = \"\\u0301\"\n    return 0\n}\n",
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(!output.status.success(), "\\u without braces must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LX002"),
        "expected LX002InvalidUnicodeChar/Escape, got: {stderr}"
    );
}

#[test]
fn stage2_out_of_range_unicode_scalar_rejected_with_lx002() {
    // > 0x10FFFF is invalid Unicode scalar
    let fixture = temp_fixture(
        "out_of_range_escape.aru",
        "module out_of_range_escape\nfunc main(): int {\n    let s = \"\\u{110000}\"\n    return 0\n}\n",
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(!output.status.success(), "scalar > 0x10FFFF must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LX002"),
        "expected LX002 for > 0x10FFFF, got: {stderr}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// ETAPA 3: PARSER, SINTAXE & ANINHAMENTO PROFUNDO
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn stage3_deeply_nested_parentheses_does_not_overflow_stack() {
    let mut expr = String::from("42");
    for _ in 0..100 {
        expr = format!("({expr})");
    }
    let fixture = temp_fixture(
        "nested_paren.aru",
        &format!(
            r#"module nested_paren
func main(): int {{
    return {expr} - 42
}}
"#
        ),
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "100 levels of parentheses failed: {stderr}"
    );
}

#[test]
fn stage3_deeply_nested_conditional_blocks_does_not_overflow_stack() {
    let mut body = String::from("return 0");
    for _ in 0..60 {
        body = format!("if true {{\n{body}\n}}");
    }
    let fixture = temp_fixture(
        "nested_blocks.aru",
        &format!(
            r#"module nested_blocks
func main(): int {{
{body}
    return 1
}}
"#
        ),
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "60 levels of nested blocks failed: {stderr}"
    );
}

#[test]
fn stage3_operator_precedence_without_spaces() {
    let fixture = temp_fixture(
        "precedence.aru",
        r#"module precedence
func main(): int {
    let val = 1+2*3-8/4
    // 1 + 6 - 2 = 5
    if val != 5 {
        return 1
    }
    let b = !false && true
    if !b {
        return 2
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "operator precedence without spaces failed: {stderr}"
    );
}

#[test]
fn stage3_keyword_as_identifier_rejected_cleanly() {
    let fixture = temp_fixture(
        "keyword_ident.aru",
        r#"module keyword_ident
func main(): int {
    let if = 10
    return if
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(!output.status.success(), "keyword as variable must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("P004") || stderr.contains("P001"),
        "expected syntax error for keyword identifier, got: {stderr}"
    );
}

#[test]
fn stage3_unclosed_block_rejected_cleanly_at_eof() {
    let fixture = temp_fixture(
        "unclosed.aru",
        "module unclosed\nfunc main(): int {\n    let x = 1\n",
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(!output.status.success(), "unclosed block at EOF must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("P002") || stderr.contains("P001"),
        "expected P002UnclosedBlock or P001, got: {stderr}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// ETAPA 4: TIPOS, RESOLUÇÃO DE ESCOPO & CONTROLE DE FLUXO
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn stage4_recursive_struct_infinite_size_rejected_with_t029() {
    let fixture = temp_fixture(
        "rec_struct.aru",
        r#"module rec_struct
struct Node {
    val: int,
    next: Node
}
func main(): int { return 0 }
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "infinite recursive struct must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("T029"),
        "expected T029RecursiveStructInfiniteSize, got: {stderr}"
    );
}

#[test]
fn stage4_duplicate_field_decl_rejected_with_t030() {
    let fixture = temp_fixture(
        "dup_field.aru",
        r#"module dup_field
struct Point {
    x: int,
    x: int
}
func main(): int { return 0 }
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(!output.status.success(), "duplicate field decl must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("T030"),
        "expected T030DuplicateFieldDecl, got: {stderr}"
    );
}

#[test]
fn stage4_break_outside_loop_rejected_with_n011() {
    let fixture = temp_fixture(
        "bad_break.aru",
        r#"module bad_break
func main(): int {
    break
    return 0
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(!output.status.success(), "break outside loop must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("N011"),
        "expected N011BreakContinueOutsideLoop, got: {stderr}"
    );
}

#[test]
fn stage4_short_circuit_does_not_evaluate_right_side() {
    let fixture = temp_fixture(
        "short_circuit.aru",
        r#"module short_circuit

func fail_test(): bool {
    let zero = 0
    let _ = 1 / zero
    return false
}

func main(): int {
    let a = false
    // If short circuit works, fail_test() is NEVER called
    if a && fail_test() {
        return 1
    }
    let b = true
    if b || fail_test() {
        return 0
    }
    return 2
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "short circuit evaluation failed: {stderr}"
    );
}

#[test]
fn stage4_deep_variable_shadowing_preserves_bindings() {
    let fixture = temp_fixture(
        "shadowing.aru",
        r#"module shadowing
func main(): int {
    let x = 10
    let mut acc = 0
    if true {
        let x = 20
        if true {
            let x = 30
            acc = acc + x
        }
        acc = acc + x
    }
    acc = acc + x
    // 30 + 20 + 10 = 60
    if acc != 60 {
        return 1
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "variable shadowing failed: {stderr}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// ETAPA 5: OWNERSHIP, BORROWING & DROP GLUE (AMIR)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn stage5_use_after_move_rejected_with_o001() {
    let fixture = temp_fixture(
        "uam.aru",
        r#"module uam

struct Resource {
    handle: int
}

@Destructor
func Resource.destroy(own self): void {}

func consume(r: Resource): int {
    return r.handle
}

func main(): int {
    let r = Resource { handle: 1 }
    let _ = consume(r)
    let x = r.handle
    return x
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "use after move of linear resource must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("O001"),
        "expected O001UseAfterMove, got: {stderr}"
    );
}

#[test]
fn stage5_move_while_borrowed_rejected_with_o002_or_o003() {
    let fixture = temp_fixture(
        "mwb.aru",
        r#"module mwb

struct Resource {
    handle: int
}

@Destructor
func Resource.destroy(own self): void {}

func consume(r: Resource): int {
    return r.handle
}

func main(): int {
    let r = Resource { handle: 1 }
    let b = ref r
    let _ = consume(r)
    return b.handle
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(!output.status.success(), "move while borrowed must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("O002") || stderr.contains("O003"),
        "expected O002 or O003, got: {stderr}"
    );
}

#[test]
fn stage5_conflicting_mut_borrow_rejected_with_o003() {
    let fixture = temp_fixture(
        "conf_borrow.aru",
        r#"module conf_borrow
func main(): int {
    let mut x = 42
    let r = ref x
    x = 100
    return *r
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "mutation during active shared borrow must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("O003") || stderr.contains("O002"),
        "expected O003MutableBorrowConflict, got: {stderr}"
    );
}

#[test]
fn stage5_dangling_local_reference_escape_rejected_with_o010() {
    let fixture = temp_fixture(
        "dangling_escape.aru",
        r#"module dangling_escape
func bad(): ref int {
    let x = 42
    return ref x
}
func main(): int { return 0 }
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "dangling reference escape must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("O010"),
        "expected O010EscapeOfBorrowedValue, got: {stderr}"
    );
}

#[test]
fn stage5_syntax_prevents_uninitialized_variables_at_grammar_level() {
    let fixture = temp_fixture(
        "uninit.aru",
        r#"module uninit
func main(): int {
    let x: int;
    let y = x + 1;
    return y;
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "uninitialized let must fail parse"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("P001"),
        "expected P001 syntax error requiring initializer, got: {stderr}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// ETAPA 6: PARALELISMO ESTRUTURADO & ASYNC RUNTIME
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn stage6_async_coroutine_spawn_and_join_lifecycle() {
    let fixture = temp_fixture(
        "async_lifecycle.aru",
        r#"module async_lifecycle

import std.runtime.executor as rt

async func compute(val: int): int {
    return val * 2
}

func main(): int {
    let ex = rt.newSyncExecutor()
    let h = rt.spawn(ex, compute(21))
    let result = rt.join(ex, h)
    if result != 42 {
        return 1
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "async spawn and join lifecycle failed: {stderr}"
    );
}

#[test]
fn stage6_structured_parallel_fold_deterministic_scaling() {
    let fixture = temp_fixture(
        "parallel_det.aru",
        r#"module parallel_det

import std.alloc.vec as vec
import std.core.parallel as parallel

struct Sum { value: int }
struct Item { val: int }
struct SumJob {}

func SumJob.run(self: ref SumJob, item: ref Item, state: mut ref Sum): void {
    state.value = state.value + item.val
}

struct SumCombine {}

func SumCombine.combine(self: ref SumCombine, dest: mut ref Sum, partial: ref Sum): void {
    dest.value = dest.value + partial.value
}

func runFold(items: []Item, workers: uint): int {
    let identity = Sum { value: 0 }
    match parallel.parallelFold<Item, Sum, SumJob, SumCombine>(
        items,
        Sum { value: 0 },
        ref identity,
        SumJob {},
        SumCombine {},
        workers
    ) {
        Ok(res) => { return res.value }
        Err(_) => { return -1 }
    }
}

func main(): int {
    let mut values = vec.new<Item>()
    let mut i = 0
    while i < 100 {
        vec.push<Item>(values, Item { val: 1 })
        i = i + 1
    }
    let items = vec.asSlice<Item>(values)
    let res1 = runFold(items, 1)
    let res4 = runFold(items, 4)
    vec.destroy<Item>(values)

    if res1 != 100 { return 1 }
    if res4 != 100 { return 2 }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "structured parallel fold determinism failed: {stderr}"
    );
}

#[test]
fn stage6_stack_borrow_across_await_is_rejected_with_o010() {
    let fixture = temp_fixture(
        "suspend_escape.aru",
        r#"module suspend_escape

async func tick(): int {
    return 1
}

func inspect(r: ref int): int {
    return *r
}

async func bad(): int {
    let local = 42
    let r = ref local
    let _ = await tick()
    return inspect(r)
}

func main(): int {
    return 0
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "stack borrow escaping via call arg across await suspension must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("O010") || stderr.contains("O004"),
        "expected O010 or O004 across await, got: {stderr}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// CASOS ADVERSARIAIS AVANÇADOS ADICIONAIS
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn stage1_const_eval_modulo_by_zero_rejected_with_t040() {
    let fixture = temp_fixture(
        "mod_zero.aru",
        r#"module mod_zero
func main(): int {
    let x = 10 % 0
    return x
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(!output.status.success(), "modulo by zero must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("T040"),
        "expected T040DivisionByZero for modulo, got: {stderr}"
    );
}

#[test]
fn stage1_nan_comparisons_comprehensive() {
    let fixture = temp_fixture(
        "nan_comp.aru",
        r#"module nan_comp
func main(): int {
    let zero: float = 0.0
    let nan = 0.0 / zero
    // IEEE 754: all ordered comparisons against NaN must be false
    if nan < 5.0 { return 1 }
    if nan > 5.0 { return 2 }
    if nan <= 5.0 { return 3 }
    if nan >= 5.0 { return 4 }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "ordered NaN comparisons failed: {stderr}"
    );
}

#[test]
fn stage3_comment_at_eof_without_trailing_newline() {
    let fixture = temp_fixture(
        "eof_comment.aru",
        "module eof_comment\nfunc main(): int {\n    return 0\n}\n// trailing comment without newline",
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        output.status.success(),
        "comment at EOF without newline must compile cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stage4_continue_outside_loop_rejected_with_n011() {
    let fixture = temp_fixture(
        "bad_continue.aru",
        r#"module bad_continue
func main(): int {
    continue
    return 0
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(!output.status.success(), "continue outside loop must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("N011"),
        "expected N011BreakContinueOutsideLoop, got: {stderr}"
    );
}

#[test]
fn stage4_mutual_recursion_deep_call_stack() {
    let fixture = temp_fixture(
        "recursion.aru",
        r#"module recursion

func ping(n: int): int {
    if n <= 0 { return 0 }
    return pong(n - 1)
}

func pong(n: int): int {
    if n <= 0 { return 0 }
    return ping(n - 1)
}

func main(): int {
    return ping(500)
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "deep mutual recursion failed: {stderr}"
    );
}

#[test]
fn stage5_aliased_mut_ref_arguments_rejected_with_o003() {
    let fixture = temp_fixture(
        "alias_arg.aru",
        r#"module alias_arg

func mutate_both(a: mut ref int, b: mut ref int): void {
}

func main(): int {
    let mut x = 42
    mutate_both(mut ref x, mut ref x)
    return x
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "two simultaneous mut refs of the same variable to a call must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("O003"),
        "expected O003MutableBorrowConflict for aliased call args, got: {stderr}"
    );
}

#[test]
fn stage5_inconsistent_move_across_branches_rejected_with_o007() {
    let fixture = temp_fixture(
        "branch_move.aru",
        r#"module branch_move

struct Resource {
    handle: int
}

@Destructor
func Resource.destroy(own self): void {}

func consume(r: Resource): void {}

func test(flag: bool): int {
    let r = Resource { handle: 42 }
    if flag {
        consume(r)
    }
    let x = r.handle
    return x
}

func main(): int {
    return 0
}
"#,
    );
    let output = check_fixture(&fixture);
    cleanup_fixture(&fixture);

    assert!(
        !output.status.success(),
        "accessing resource moved conditionally on one branch must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("O007") || stderr.contains("O001"),
        "expected O007InconsistentMoveBetweenBranches or O001, got: {stderr}"
    );
}

#[test]
fn stage6_structured_parallel_fold_empty_collection() {
    let fixture = temp_fixture(
        "parallel_empty.aru",
        r#"module parallel_empty

import std.alloc.vec as vec
import std.core.parallel as parallel

struct Sum { value: int }
struct Item { val: int }
struct SumJob {}

func SumJob.run(self: ref SumJob, item: ref Item, state: mut ref Sum): void {
    state.value = state.value + item.val
}

struct SumCombine {}

func SumCombine.combine(self: ref SumCombine, dest: mut ref Sum, partial: ref Sum): void {
    dest.value = dest.value + partial.value
}

func main(): int {
    let values = vec.new<Item>()
    let items = vec.asSlice<Item>(values)
    let identity = Sum { value: 0 }
    let seed = Sum { value: 42 }

    let result = match parallel.parallelFold<Item, Sum, SumJob, SumCombine>(
        items,
        seed,
        ref identity,
        SumJob {},
        SumCombine {},
        4
    ) {
        Ok(res) => { res.value }
        Err(_) => { -1 }
    }
    vec.destroy<Item>(values)

    if result != 42 {
        return 1
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "parallel fold over empty slice failed: {stderr}"
    );
}

#[test]
fn stage6_structured_parallel_fold_single_element() {
    let fixture = temp_fixture(
        "parallel_single.aru",
        r#"module parallel_single

import std.alloc.vec as vec
import std.core.parallel as parallel

struct Sum { value: int }
struct Item { val: int }
struct SumJob {}

func SumJob.run(self: ref SumJob, item: ref Item, state: mut ref Sum): void {
    state.value = state.value + item.val
}

struct SumCombine {}

func SumCombine.combine(self: ref SumCombine, dest: mut ref Sum, partial: ref Sum): void {
    dest.value = dest.value + partial.value
}

func main(): int {
    let mut values = vec.new<Item>()
    vec.push<Item>(values, Item { val: 58 })
    let items = vec.asSlice<Item>(values)
    let identity = Sum { value: 0 }
    let seed = Sum { value: 42 }

    let result = match parallel.parallelFold<Item, Sum, SumJob, SumCombine>(
        items,
        seed,
        ref identity,
        SumJob {},
        SumCombine {},
        4
    ) {
        Ok(res) => { res.value }
        Err(_) => { -1 }
    }
    vec.destroy<Item>(values)

    // 42 + 58 = 100
    if result != 100 {
        return 1
    }
    return 0
}
"#,
    );
    let output = run_fixture(&fixture);
    let stderr = String::from_utf8_lossy(&output.stderr);
    cleanup_fixture(&fixture);

    assert_eq!(
        output.status.code(),
        Some(0),
        "parallel fold over single element failed: {stderr}"
    );
}
