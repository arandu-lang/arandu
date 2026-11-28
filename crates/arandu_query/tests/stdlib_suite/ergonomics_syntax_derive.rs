//! End-to-end tests for Package B ergonomics:
//! default parameter values, struct shorthand/update, if/while/guard let,
//! expression-body functions, and built-in `@Derive(...)`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::STDLIB_MODULES;
use arandu_middle::{DiagCode, Severity};
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{lower_amir, parse, type_check};

fn db_with_stdlib() -> DatabaseImpl {
    let mut db = DatabaseImpl::default();
    for &(path, src) in STDLIB_MODULES {
        db.new_stdlib_file(path.to_string(), src.to_string());
    }
    db
}

fn check_clean_amir(db: &DatabaseImpl, file: arandu_query::SourceFile) {
    if let Err(err) = parse(db, file).as_ref() {
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
    let prepared = arandu_query::passes::prepare_hir(db, file);
    assert!(
        prepared.hir.is_some(),
        "expected HIR preparation to succeed: {:#?}",
        prepared.diagnostics
    );
    let amir = lower_amir(db, file);
    let amir_diags: Vec<_> =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(db, file)
            .into_iter()
            .map(|d| d.0.clone())
            .filter(|d| matches!(d.severity, Severity::Error))
            .collect();
    assert!(
        !amir.amir.funcs.is_empty() && amir_diags.is_empty(),
        "expected clean AMIR lowering, got: {amir_diags:#?}"
    );
}

fn typeck_errors(
    db: &DatabaseImpl,
    file: arandu_query::SourceFile,
) -> Vec<arandu_middle::Diagnostic> {
    if let Err(err) = parse(db, file).as_ref() {
        panic!("expected parse to succeed, got: {err}");
    }
    type_check(db, file)
        .diagnostics
        .iter()
        .filter(|d| matches!(d.severity, Severity::Error))
        .cloned()
        .collect()
}

// ── B.1 Default Parameters & B.4 Expression-Body Functions ───────────

#[test]
fn default_params_and_expr_body_funcs_lower_cleanly() {
    let mut db = db_with_stdlib();
    let helper_src = r#"
module app.netcfg

public func connect(host: str, port: int = 8080, tls: bool = true): int {
    return if tls { port } else { -port }
}
"#;
    let main_src = r#"
module app.main

import app.netcfg as netcfg

struct Point {
    x: int,
    y: int,
}

func Point.shift(self: ref Point, dx: int = 1, dy: int = -2): Point = Point { x: self.x + dx, y: self.y + dy }

func double(x: int): int = x * 2

func wrapTag<T>(val: T, scale: int = 3): int {
    return scale
}

func optFallback(v: int? = nil): int {
    return v ?? 42
}

func main(): int {
    let a = netcfg.connect("localhost")
    let b = netcfg.connect("localhost", 9090)
    let c = netcfg.connect("localhost", 443, false)
    let p = Point { x: 10, y: 20 }
    let p1 = p.shift()
    let p2 = p.shift(5)
    let d = double(p1.x + p2.y)
    let w = wrapTag(p)
    let o = optFallback()
    return a + b + c + d + w + o
}
"#;
    let _helper = db.new_file("app/netcfg.aru".to_string(), helper_src.to_string());
    let main_file = db.new_file("app/main.aru".to_string(), main_src.to_string());
    check_clean_amir(&db, main_file);
}

#[test]
fn non_default_param_after_default_param_is_rejected() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.bad_defaults

func broken(a: int = 1, b: int): int {
    return a + b
}
"#;
    let file = db.new_file("app/bad_defaults.aru".to_string(), src.to_string());
    let errs = typeck_errors(&db, file);
    assert!(
        errs.iter().any(|d| d.code == DiagCode::T012WrongArgCount),
        "expected T012WrongArgCount for non-default after default param, got: {errs:#?}"
    );
}

// ── B.2 Struct Shorthand/Update & B.3 if/while/guard let ─────────────

#[test]
fn struct_shorthand_update_and_if_while_guard_let_lower_cleanly() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.syntax_ergo

struct Config {
    host: str,
    port: int,
    retries: int,
}

func nextOpt(counter: mut ref int): int? {
    if *counter <= 0 {
        return nil
    }
    *counter = *counter - 1
    return Option.Some(*counter)
}

func extractOrZero(v: int?): int {
    let Option.Some(inner) = v else {
        return 0
    }
    return inner
}

func main(): int {
    let host = "127.0.0.1"
    let port = 8080
    let retries = 3
    let base = Config { host, port, retries }
    let updated = Config { port: 9000, ..base }

    let mut total = extractOrZero(Option.Some(updated.port))
    let mut count = 4
    while let Option.Some(n) = nextOpt(count) {
        total = total + n
    }
    if let Option.Some(k) = Option.Some(5) {
        total = total + k
    }
    return total
}
"#;
    let file = db.new_file("app/syntax_ergo.aru".to_string(), src.to_string());
    check_clean_amir(&db, file);
}

#[test]
fn struct_update_rejects_private_fields_across_modules() {
    let mut db = db_with_stdlib();
    let dep_src = r#"
module app.secret

public struct Token {
    public id: int,
    private secret: int,
}

public func makeToken(): Token {
    return Token { id: 1, secret: 99 }
}
"#;
    let main_src = r#"
module app.caller

import app.secret as secret

func main(): int {
    let base = secret.makeToken()
    let forged = secret.Token { id: 2, ..base }
    return forged.id
}
"#;
    let _dep = db.new_file("app/secret.aru".to_string(), dep_src.to_string());
    let caller = db.new_file("app/caller.aru".to_string(), main_src.to_string());
    let errs = typeck_errors(&db, caller);
    assert!(
        !errs.is_empty(),
        "expected privacy error when updating struct with private fields across modules"
    );
}

// ── B.5 Built-in @Derive ──────────────────────────────────────────────

#[test]
fn derive_all_traits_on_struct_and_enum_lower_cleanly() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.derive_demo

import std.core.cmp as cmp
import std.core.hash as hash
import std.core.fmt as fmt

@Derive(PartialEq, Eq, Ord, Clone, Hash, Debug)
struct Point {
    x: int,
    y: int,
}

@Derive(Eq, Clone, Hash, Debug)
struct User {
    id: int,
    name: String,
}

@Derive(Eq, Clone, Hash, Debug)
enum Message {
    Quit,
    Text(String),
    Pair(int, int),
}

@Derive(Eq, Ord, Clone, Hash, Debug)
enum Priority {
    Low,
    Medium,
    Custom(int, int),
}

func checkOrd<T: cmp.Ord>(a: ref T, b: ref T): bool {
    return *a < *b
}

func checkHash<T: hash.Hash>(v: ref T): u64 {
    let mut h = hash.FnvHasher.new()
    v.hash(h)
    return h.finish()
}

func main(): int {
    let p1 = Point { x: 1, y: 2 }
    let p2 = p1.clone()
    let eq_ok = p1 == p2 && p1.eq(ref p2)
    let _ = p1.cmp(ref p2)
    let pr1 = Priority.Low
    let pr2 = Priority.Custom(1, 2)
    let _ = checkOrd(ref pr1, ref pr2)
    let u1 = User { id: 1, name: "Ana".toString() }
    let u2 = u1.clone()
    let _ = u1 == u2
    let m1 = Message.Text("hi".toString())
    let m2 = m1.clone()
    let _ = m1 == m2 && checkOrd(ref p1, ref p2)
    let digest = checkHash(ref p1) + checkHash(ref m2) + checkHash(ref pr2)

    let mut storage = Vec<u8>.new()
    let mut i: int = 0
    while i < 128 {
        storage.push(0 as u8)
        i = i + 1
    }
    let mut f = fmt.Formatter.new(storage.asMutSlice())
    let _ = p1.fmtDebug(f)
    let _ = m1.fmtDebug(f)
    let _ = u2.fmtDebug(f)
    let _ = pr2.fmtDebug(f)
    if eq_ok && digest > 0 as u64 {
        return f.len() as int
    }
    return 0
}
"#;
    let file = db.new_file("app/derive_demo.aru".to_string(), src.to_string());
    check_clean_amir(&db, file);
}

#[test]
fn derive_partial_eq_allows_floats_while_eq_ord_hash_reject_them() {
    let mut db = db_with_stdlib();
    let ok_src = r#"
module app.float_partial_eq

@Derive(PartialEq, Debug)
struct Vec2f {
    x: f64,
    y: f64,
}

func main(): int {
    let a = Vec2f { x: 1.5, y: 2.5 }
    let b = Vec2f { x: 1.5, y: 2.5 }
    return if a == b { 1 } else { 0 }
}
"#;
    let ok_file = db.new_file("app/float_partial_eq.aru".to_string(), ok_src.to_string());
    check_clean_amir(&db, ok_file);

    for trait_name in ["Eq", "Ord", "Hash"] {
        let bad_src = format!(
            "module app.bad_float_{trait_name}\n@Derive({trait_name})\nstruct BadFloat {{ x: f64 }}\n"
        );
        let bad_file = db.new_file(format!("app/bad_float_{trait_name}.aru"), bad_src);
        let errs = typeck_errors(&db, bad_file);
        assert!(
            errs.iter()
                .any(|d| d.code == DiagCode::T025InterfaceNotSatisfied),
            "expected T025 for @Derive({trait_name}) with float field, got: {errs:#?}"
        );
    }
}

#[test]
fn derive_phantom_data_does_not_require_type_param_bounds() {
    let mut db = db_with_stdlib();
    let src = r#"
module app.phantom_derive

import std.core.marker as marker
import std.core.fmt as fmt

@NoCopy
struct Opaque {
    token: int,
}

@Derive(Eq, Clone, Hash, Debug)
struct Tagged<T> {
    id: int,
    marker: marker.PhantomData<T>,
}

@Derive(Clone)
struct Boxed<T> {
    value: T,
}

func requireClone<T: Clone>(x: ref T): T {
    return x.clone()
}

func main(): int {
    let t1: Tagged<Opaque> = Tagged { id: 7, marker: marker.phantom<Opaque>() }
    let t2 = requireClone(ref t1)
    let b = Boxed { value: Opaque { token: 9 } }
    let mut storage = Vec<u8>.new()
    let mut i: int = 0
    while i < 64 {
        storage.push(0 as u8)
        i = i + 1
    }
    let mut f = fmt.Formatter.new(storage.asMutSlice())
    let _ = t2.fmtDebug(f)
    if t1 == t2 && b.value.token == 9 {
        return 1
    }
    return 0
}
"#;
    let file = db.new_file("app/phantom_derive.aru".to_string(), src.to_string());
    check_clean_amir(&db, file);

    // Calling .clone() on Boxed<Opaque> (where T IS stored in non-PhantomData field) must fail!
    let bad_src = r#"
module app.boxed_noclone

@NoCopy
struct Opaque {
    token: int,
}

@Derive(Clone)
struct Boxed<T> {
    value: T,
}

func main(): int {
    let b = Boxed { value: Opaque { token: 1 } }
    let c = b.clone()
    return c.value.token
}
"#;
    let bad_file = db.new_file("app/boxed_noclone.aru".to_string(), bad_src.to_string());
    let errs = typeck_errors(&db, bad_file);
    assert!(
        !errs.is_empty(),
        "expected generic constraint error when cloning Boxed<NoClone>"
    );
}

#[test]
fn derive_rejects_structural_capabilities_display_and_unknown_with_friendly_diagnostics() {
    let mut db = db_with_stdlib();
    let cases = [
        ("Copy", "@NoCopy"),
        ("Send", "structurally"),
        ("Sync", "structurally"),
        ("Display", "Debug"),
        ("Magic", "PartialEq"),
    ];
    for (arg, expected_hint_substr) in cases {
        let src =
            format!("module app.bad_derive_{arg}\n@Derive({arg})\nstruct Item {{ id: int }}\n");
        let file = db.new_file(format!("app/bad_derive_{arg}.aru"), src);
        let errs = typeck_errors(&db, file);
        let matching = errs
            .iter()
            .find(|d| d.code == DiagCode::N014InvalidAnnotationArguments);
        assert!(
            matching.is_some(),
            "expected N014 for @Derive({arg}), got: {errs:#?}"
        );
        let diag = matching.unwrap();
        let combined = format!(
            "{} {}",
            diag.message,
            diag.hints
                .iter()
                .map(|h| h.message.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        assert!(
            combined.contains(expected_hint_substr),
            "expected diagnostic mentioning `{expected_hint_substr}` for @Derive({arg}), got: {diag:#?}"
        );
    }
}
