//! Tests for std.text.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

use crate::common;

const TEXT_ARU: &str = include_str!("../../../../stdlib/std/text.aru");

#[test]
fn stdlib_text_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_stdlib_file("stdlib/std/text.aru".to_string(), TEXT_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("text.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "StringBuilder",
        "StringBuilder.new",
        "StringBuilder.withCapacity",
        "StringBuilder.append",
        "StringBuilder.appendLine",
        "StringBuilder.appendChar",
        "StringBuilder.appendInt",
        "StringBuilder.clear",
        "StringBuilder.intoString",
        "trimStart",
        "trimEnd",
        "trim",
        "toUpper",
        "toLower",
        "repeat",
        "padLeft",
        "padRight",
        "center",
    ];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_text_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let mut text_file = None;
    for (path, source) in common::STDLIB_MODULES {
        let file = db.new_stdlib_file((*path).to_string(), (*source).to_string());
        if *path == "stdlib/std/text.aru" {
            text_file = Some(file);
        }
    }
    let text_file = text_file.expect("text.aru must be registered");

    let main_src = r#"
import std.text as text

func testTextOperations(): bool {
    let mut sb = text.StringBuilder.new()
    sb.append("hello")
    sb.appendChar(' ')
    sb.appendInt(42 as i64)
    sb.appendLine("!")
    let s = sb.intoString()

    let trimmed = text.trim("   hello   ")
    let upper = text.toUpper("abc")
    let lower = text.toLower("ABC")
    let rep = text.repeat("x", 5 as usize)
    let padded = text.padLeft("7", 4 as usize, '0')
    return true
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags = file_ide_diagnostics(&db, text_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_text: Vec<_> = diags.iter().filter(|d| d.severity == 0).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 0).collect();

    assert!(
        error_diags_text.is_empty(),
        "unexpected errors in text.aru: {error_diags_text:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru using text.aru: {error_diags_main:?}"
    );
}

#[test]
fn onda6_stdlib_and_vec_range_indexing() {
    let mut db = DatabaseImpl::default();
    for (path, source) in common::STDLIB_MODULES {
        db.new_stdlib_file((*path).to_string(), (*source).to_string());
    }
    let main_src = r#"
import std.core.option as option
import std.core.result as result
import std.core.hash as hash
import std.core.intrinsics as intrinsics
import std.alloc.vec as vec
import std.alloc.hash_map as hash_map
import std.alloc.gen_arena as gen_arena

struct Key {
    id: int
}

func Key.eq(self: ref Key, other: ref Key): bool {
    return self.id == other.id
}

func Key.hash<H: hash.Hasher>(self: ref Key, state: mut ref H): void {
    state.writeInt(self.id)
}

func ref_vec_range(v: ref vec.Vec<int>): []int {
    let r = 0..2
    return v[r]
}

func diverge_check(cond: bool): int {
    return if cond { unsafe { intrinsics.abort() } } else { 7 }
}

public func exercise(): int {
    let mut opt: option.Option<int> = option.Option.Some(10)
    match opt.asMut() {
        option.Option.Some(p) => { *p = 20 }
        option.Option.None => {}
    }

    let mut res: result.Result<int, int> = result.Result.Ok(5)
    let _r_ref = res.asRef()
    let _r_mut = res.asMut()

    let mut map = hash_map.HashMap<Key, int>.new()
    map.put(Key { id: 1 }, 100)
    match map.getMut(Key { id: 1 }) {
        option.Option.Some(v) => { *v = 200 }
        option.Option.None => {}
    }

    let mut arena = gen_arena.GenArena<int>.new()
    let handle = arena.insert(9)
    match arena.tryGetMut(handle) {
        option.Option.Some(v) => { *v = 11 }
        option.Option.None => {}
    }
    let _ro = arena.tryGet(handle)
    let _gm = arena.getMut(handle)

    let mut v = vec.Vec<int>.new()
    v.push(1)
    v.push(2)
    v.push(3)
    let r = 0..2
    let s1 = v[0..2]
    let s2 = v[r]
    let s3 = ref_vec_range(ref v)
    let mut acc = 0
    for j in r {
        acc = acc + j
    }
    return s1.len() as int + s2.len() as int + s3.len() as int + diverge_check(false) + acc
}
"#;
    let main_file = db.new_file("onda6_main.aru".to_string(), main_src.to_string());
    let diags = file_ide_diagnostics(&db, main_file);
    let errors: Vec<_> = diags.iter().filter(|d| d.severity == 0).collect();
    assert!(errors.is_empty(), "unexpected Onda 6 errors: {errors:?}");
}
