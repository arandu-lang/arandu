//! End-to-end regressions for layered prelude, `str.toString()`,
//! `std.collections` facade, and grouped imports (`import std.{env, fs}`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::STDLIB_MODULES;
use arandu_middle::Severity;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{lower_amir, type_check};

fn db_with_stdlib() -> DatabaseImpl {
    let mut db = DatabaseImpl::default();
    for &(path, src) in STDLIB_MODULES {
        db.new_stdlib_file(path.to_string(), src.to_string());
    }
    db
}

fn check_no_errors(db: &DatabaseImpl, file: arandu_query::SourceFile) {
    let parsed = arandu_query::passes::parse(db, file);
    if let Err(err) = parsed.as_ref() {
        panic!("expected parse to succeed, got: {err}");
    }
    let tc = type_check(db, file);
    let errors: Vec<_> = tc
        .diagnostics
        .iter()
        .filter(|d| matches!(d.severity, Severity::Error))
        .collect();
    assert!(
        errors.is_empty(),
        "expected no typeck errors, got: {errors:#?}"
    );
}

#[test]
fn layered_prelude_supports_vec_string_box_tostring_and_println_without_imports() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.prelude_demo

func main(): int {
    let mut names = Vec<String>.new()
    names.push("Ana".toString())
    names.push("Bruno".toString())
    let boxed = Box.new(names.len() as int)
    if !names.isEmpty() {
        println("Total: ${names.len()}")
        io.println("Checked via io")
    }
    return *boxed.get()
}
"#;
    let file = db.new_file("app/prelude_demo.aru".to_string(), src.to_string());
    check_no_errors(&db, file);
    let prepared = arandu_query::passes::prepare_hir(&db, file);
    assert!(
        prepared.hir.is_some(),
        "expected HIR preparation to succeed, got diagnostics: {:#?}",
        prepared.diagnostics
    );
    let amir = lower_amir(&db, file);
    let amir_diags: Vec<_> =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, file)
            .into_iter()
            .map(|d| d.0.clone())
            .collect();
    assert!(
        !amir.amir.funcs.is_empty(),
        "expected AMIR lowering to emit functions, diagnostics: {amir_diags:#?}"
    );
}

#[test]
fn local_declarations_cleanly_shadow_prelude_names() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.shadow_demo

struct Vec { count: int }
func println(x: int): int { return x + 1 }

func main(): int {
    let v = Vec { count: 41 }
    return println(v.count)
}
"#;
    let file = db.new_file("app/shadow_demo.aru".to_string(), src.to_string());
    check_no_errors(&db, file);
}

#[test]
fn std_collections_facade_reexports_common_collections() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.collections_demo

from std.collections import { HashMap, VecDeque, BitSet }

func main(): int {
    let mut map = HashMap<String, int>.new()
    let _ = map.put("a".toString(), 10)
    let mut q = VecDeque<int>.new()
    q.pushBack(2)
    let mut bits = BitSet.new()
    let _ = bits.insert(0 as usize)
    return (map.len() as int) + (q.len() as int) + (bits.countOnes() as int)
}
"#;
    let file = db.new_file("app/collections_demo.aru".to_string(), src.to_string());
    check_no_errors(&db, file);
}

#[test]
fn grouped_imports_resolve_modules_and_aliases() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.grouped_demo

import std.{env, fs as files}

@Effects(FileRead, Foreign)
func main(): int {
    let n = env.argsLen()
    if files.fileExists("Cargo.toml") {
        return n as int
    }
    return 0
}
"#;
    let file = db.new_file("app/grouped_demo.aru".to_string(), src.to_string());
    check_no_errors(&db, file);
}

#[test]
fn ambient_option_and_result_methods_work_without_imports() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.option_result_ambient

func main(): int {
    let opt: Option<int> = Option.Some(20)
    let res: Result<int, str> = Result.Ok(22)
    if opt.isSome() && res.isOk() {
        let a = opt.map((x) => x + 1).unwrapOr(0)
        let b = res.ok().unwrapOr(0)
        return a + b
    }
    return 0
}
"#;
    let file = db.new_file("app/option_result_ambient.aru".to_string(), src.to_string());
    check_no_errors(&db, file);
    let prepared = arandu_query::passes::prepare_hir(&db, file);
    assert!(
        prepared.hir.is_some(),
        "expected HIR preparation to succeed: {:#?}",
        prepared.diagnostics
    );
    let amir = lower_amir(&db, file);
    let amir_diags: Vec<_> =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, file)
            .into_iter()
            .map(|d| d.0.clone())
            .collect();
    assert!(
        !amir.amir.funcs.is_empty(),
        "expected AMIR lowering to emit functions, diagnostics: {amir_diags:#?}"
    );
}

#[test]
fn string_and_vec_view_coercions_work_without_as_str_or_as_slice() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.view_coercions

func sumSlice(xs: []int): int {
    let mut total = 0
    for x in xs {
        total += x
    }
    return total
}

func firstOrZero<T: Copy>(xs: []T, fallback: T): T {
    if xs.len == 0 {
        return fallback
    }
    return xs[0]
}

func main(): int {
    let owned_text = "Cargo.toml".toString()
    println(owned_text)

    let mut nums: Vec<int> = Vec.new()
    nums.push(10)
    nums.push(20)
    nums.push(12)
    let total = sumSlice(nums)
    let first = firstOrZero(nums, 0)
    return total + first
}
"#;
    let file = db.new_file("app/view_coercions.aru".to_string(), src.to_string());
    check_no_errors(&db, file);
    let prepared = arandu_query::passes::prepare_hir(&db, file);
    assert!(
        prepared.hir.is_some(),
        "expected HIR preparation to succeed: {:#?}",
        prepared.diagnostics
    );
    let amir = lower_amir(&db, file);
    let amir_diags: Vec<_> =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, file)
            .into_iter()
            .map(|d| d.0.clone())
            .collect();
    assert!(
        !amir.amir.funcs.is_empty() && amir_diags.is_empty(),
        "expected clean AMIR lowering, got: {amir_diags:#?}"
    );
}

#[test]
fn vec_from_array_literal_and_combinators_work() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.vec_combinators

func main(): int {
    let nums = Vec.from([1, 2, 3, 4])
    let doubled = nums.map((x) => *x * 2)
    let has_six = doubled.anyMatch((x) => *x == 6)
    let all_pos = doubled.allMatch((x) => *x > 0)
    let found = doubled.find((x) => *x > 4)
    if has_six && all_pos && found.isSome() {
        return doubled.fold(0, (acc, x) => acc + *x)
    }
    return 0
}
"#;
    let file = db.new_file("app/vec_combinators.aru".to_string(), src.to_string());
    check_no_errors(&db, file);
    let prepared = arandu_query::passes::prepare_hir(&db, file);
    assert!(
        prepared.hir.is_some(),
        "expected HIR preparation to succeed: {:#?}",
        prepared.diagnostics
    );
    let amir = lower_amir(&db, file);
    let amir_diags: Vec<_> =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, file)
            .into_iter()
            .map(|d| d.0.clone())
            .collect();
    assert!(
        !amir.amir.funcs.is_empty(),
        "expected AMIR lowering to emit functions, diagnostics: {amir_diags:#?}"
    );
}
