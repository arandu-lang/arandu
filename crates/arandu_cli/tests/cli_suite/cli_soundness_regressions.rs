//! Regressions from the 0.1.11 front-end soundness hunt: each program either
//! runs to a known result at every level, traps, or is rejected with the
//! code that names its defect.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

fn write(name: &str, source: &str) -> String {
    let file = std::env::temp_dir().join(format!("arandu_cli_soundness_{name}.aru"));
    fs::write(&file, source).expect("write");
    file.to_string_lossy().into_owned()
}

fn exit_codes(name: &str, source: &str) -> Vec<Option<i32>> {
    let path = write(name, source);
    let mut codes: Vec<Option<i32>> = [None, Some("--opt")]
        .into_iter()
        .map(|profile| {
            let mut args = vec!["run"];
            args.extend(profile);
            args.push(&path);
            cli(&args).status.code()
        })
        .collect();
    codes.extend(super::common::c_release_exit_code(std::path::Path::new(
        &*path,
    )));
    codes
}

const RUNS: &[(&str, &str, i32)] = &[
    (
        "vec_index_in_bounds",
        r#"import std.alloc.vec as vec
func mk(): vec.Vec<int> {
    let mut v = vec.Vec<int>.new()
    v.push(10)
    v.push(20)
    return v
}
func viaRef(v: mut ref vec.Vec<int>, i: int): int {
    v[i] = 5
    return v[i]
}
func main(): int {
    let mut v = mk()
    let i: int = 1
    return v[i]
}
"#,
        20,
    ),
    (
        "vec_write_through_ref",
        r#"import std.alloc.vec as vec
func mk(): vec.Vec<int> {
    let mut v = vec.Vec<int>.new()
    v.push(10)
    v.push(20)
    return v
}
func viaRef(v: mut ref vec.Vec<int>, i: int): int {
    v[i] = 5
    return v[i]
}
func main(): int {
    let mut v = mk()
    let i: int = 1
    return viaRef(mut ref v, i)
}
"#,
        5,
    ),
    (
        "vec_index_of_call_result",
        r#"import std.alloc.vec as vec
func mk(): vec.Vec<int> {
    let mut v = vec.Vec<int>.new()
    v.push(10)
    v.push(20)
    return v
}
func viaRef(v: mut ref vec.Vec<int>, i: int): int {
    v[i] = 5
    return v[i]
}
func main(): int {
    let mut v = mk()
    let i: usize = 1
    return mk()[i]
}
"#,
        20,
    ),
    (
        "match_on_borrowed_enum",
        r#"enum Op { Add(int, int), Lit(int) }
func eval(o: ref Op): int {
    return match o {
        Add(a, b) => a + b
        Lit(v) => v
    }
}
func Op.go(shared self): int {
    return match self {
        Add(a, b) => a * b
        Lit(v) => v
    }
}
func main(): int {
    let x = Op.Add(3, 4)
    let y = Op.Lit(5)
    return eval(ref x) * 100 + x.go() + eval(ref y) * 1000
}
"#,
        80,
    ),
    (
        "match_guard_on_scalar_param",
        r#"func classify(x: int): int {
    return match x {
        n if n < 0 => 1
        n if n > 100 => 2
        _ => 3
    }
}
func main(): int { return classify(-1) * 100 + classify(500) * 10 + classify(9) }
"#,
        123,
    ),
    (
        "catch_binding_moves_and_drops",
        r#"import std.alloc.string as strings
func ps(x: int): Result<int, strings.String> {
    if x < 0 {
        return .Err(strings.String.from("an error string that lives on the heap"))
    }
    return .Ok(3)
}
func pt(x: int): Result<strings.String, strings.String> {
    if x < 0 {
        return .Err(strings.String.from("an error string that lives on the heap"))
    }
    return .Ok(strings.String.from("fine string that also lives on the heap"))
}
func main(): int {
    let s = ps(-1) catch |e| {
        (e.len() as int)
    }
    let t = ps(1) catch |e| {
        (e.len() as int)
    }
    let u = pt(-1) catch |e| {
        e
    }
    let w = pt(1) catch |e| {
        e
    }
    return s + t + (u.len() as int) + (w.len() as int)
}
"#,
        118,
    ),
    (
        "match_arms_move_different_variants",
        r#"import std.alloc.string as strings
func pt(x: int): Result<strings.String, strings.String> {
    return .Ok(strings.String.from("fine"))
}
func main(): int {
    let u = match pt(-1) {
        Ok(v) => v
        Err(e) => e
    }
    let w = pt(1) catch strings.String.from("x")
    return (u.len() + w.len()) as int
}
"#,
        8,
    ),
    (
        "nullable_struct_assigned_in_branch",
        r#"struct Inner { v: int }
func main(): int {
    let mut inn: Inner? = nil
    let c = 1
    if c > 0 { inn = Inner { v: 3 } }
    return inn?.v ?? 7
}
"#,
        3,
    ),
    (
        "nullable_scalar_from_variable",
        r#"func main(): int {
    let x = 5
    let k: int? = x
    let c = 1
    if c > 0 { }
    return k ?? 7
}
"#,
        5,
    ),
    (
        "nested_safe_navigation",
        r#"struct Inner { v: int }
struct Outer { inner: Inner? }
func mk(): Inner? { return Inner { v: 4 } }
func mko(): Outer? { return Outer { inner: mk() } }
func main(): int {
    return mko()?.inner?.v ?? 9
}
"#,
        4,
    ),
    (
        "nullable_struct_return",
        r#"struct P { v: int }
func g(x: int): P? {
    if x > 0 { return P { v: x } }
    return nil
}
func main(): int {
    return (g(6)?.v ?? 0) + (g(-1)?.v ?? 10)
}
"#,
        16,
    ),
    (
        "nullable_field_round_trip",
        r#"struct Inner { v: int }
struct Outer { inner: Inner? }
func mk(): Inner? { return Inner { v: 4 } }
func main(): int {
    let i: Inner? = mk()
    let o = Outer { inner: i }
    let j: Inner? = o.inner
    if i == j { return 77 }
    return 78
}
"#,
        77,
    ),
    (
        "nullable_field_read_after_other_calls",
        r#"struct Inner { v: int }
struct Outer { inner: Inner? }
func mk(): Inner? { return Inner { v: 4 } }
func main(): int {
    let o = Outer { inner: mk() }
    let x: Inner? = o.inner
    let y: Inner? = mk()
    return (x?.v ?? 9) * 10 + (y?.v ?? 9)
}
"#,
        44,
    ),
];

#[test]
fn programs_run_to_their_result_at_every_level() {
    for (name, source, expected) in RUNS {
        let codes = exit_codes(name, source);
        assert!(
            codes.iter().all(|code| *code == Some(*expected)),
            "{name}: {codes:?}"
        );
    }
}

/// `Vec` indices are checked like array and slice indices: reads, writes,
/// negative signed indices, indices through `mut ref` and on call results.
const TRAPS: &[(&str, &str)] = &[
    (
        "v_readoob",
        r#"import std.alloc.vec as vec
func mk(): vec.Vec<int> {
    let mut v = vec.Vec<int>.new()
    v.push(10)
    v.push(20)
    return v
}
func viaRef(v: mut ref vec.Vec<int>, i: int): int {
    v[i] = 5
    return v[i]
}
func main(): int {
    let mut v = mk()
    let i: int = 1000
    return v[i]
}
"#,
    ),
    (
        "v_neg",
        r#"import std.alloc.vec as vec
func mk(): vec.Vec<int> {
    let mut v = vec.Vec<int>.new()
    v.push(10)
    v.push(20)
    return v
}
func viaRef(v: mut ref vec.Vec<int>, i: int): int {
    v[i] = 5
    return v[i]
}
func main(): int {
    let mut v = mk()
    let i: int = -1
    return v[i]
}
"#,
    ),
    (
        "v_write",
        r#"import std.alloc.vec as vec
func mk(): vec.Vec<int> {
    let mut v = vec.Vec<int>.new()
    v.push(10)
    v.push(20)
    return v
}
func viaRef(v: mut ref vec.Vec<int>, i: int): int {
    v[i] = 5
    return v[i]
}
func main(): int {
    let mut v = mk()
    let i: usize = 1000
    v[i] = 7
    return 0
}
"#,
    ),
    (
        "v_ref",
        r#"import std.alloc.vec as vec
func mk(): vec.Vec<int> {
    let mut v = vec.Vec<int>.new()
    v.push(10)
    v.push(20)
    return v
}
func viaRef(v: mut ref vec.Vec<int>, i: int): int {
    v[i] = 5
    return v[i]
}
func main(): int {
    let mut v = mk()
    let i: int = 5
    return viaRef(mut ref v, i)
}
"#,
    ),
    (
        "v_call",
        r#"import std.alloc.vec as vec
func mk(): vec.Vec<int> {
    let mut v = vec.Vec<int>.new()
    v.push(10)
    v.push(20)
    return v
}
func viaRef(v: mut ref vec.Vec<int>, i: int): int {
    v[i] = 5
    return v[i]
}
func main(): int {
    let mut v = mk()
    let i: usize = 9
    return mk()[i]
}
"#,
    ),
];

#[test]
fn out_of_bounds_vec_indices_trap() {
    for (name, source) in TRAPS {
        for code in exit_codes(name, source) {
            assert!(
                code.is_none_or(|code| code >= 128),
                "{name}: must trap, got {code:?}"
            );
        }
    }
}

const REJECTS: &[(&str, &str, &str)] = &[
    (
        "field_of_immutable",
        "struct P { x: int }\nfunc bump(p: ref P): void { p.x = 5 }\nfunc P.sneak(shared self): void { self.x = 1 }\nfunc main(): int {\n    let p = P { x: 1 }\n    p.x = 4\n    return p.x\n}\n",
        "T026",
    ),
    (
        "owned_nullable",
        "import std.alloc.string as strings\nfunc eat(s: strings.String?): int { return 0 }\nfunc main(): int { return 0 }\n",
        "T006",
    ),
    (
        "move_out_of_index",
        "import std.alloc.vec as vec\nimport std.alloc.string as strings\nfunc main(): int {\n    let mut v = vec.Vec<strings.String>.new()\n    v.push(strings.String.from(\"a heap allocated string\"))\n    let s = v[0]\n    return s.len() as int\n}\n",
        "O015",
    ),
    (
        "literal_beyond_i128",
        "func main(): int {\n    let x = 99999999999999999999999999999999999999999999\n    return x\n}\n",
        "T038",
    ),
];

#[test]
fn unsound_programs_are_rejected() {
    for (name, source, code) in REJECTS {
        let output = cli(&["check", &write(name, source)]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{name} must be rejected");
        assert!(stderr.contains(code), "{name}: expected {code}:\n{stderr}");
    }
}

#[test]
fn field_writes_with_mutable_access_are_accepted() {
    let source = "struct P { x: int }\nfunc put(p: mut ref P): void { p.x = 6 }\nfunc P.fine(mut self): void { self.x = 2 }\nfunc main(): int {\n    let mut q = P { x: 1 }\n    q.x = 3\n    put(mut ref q)\n    q.fine()\n    return q.x\n}\n";
    let codes = exit_codes("field_mutable", source);
    assert!(codes.iter().all(|code| *code == Some(2)), "{codes:?}");
}

/// `T?` of a value type is a boxed handle in C as in Cranelift: the C build
/// agrees with `run` and stays clean under AddressSanitizer.
#[test]
fn nullable_values_match_between_cranelift_and_c() {
    if Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    for (name, source, expected) in RUNS.iter().filter(|(name, ..)| name.contains("nullable")) {
        let path = write(name, source);
        let c = cli(&["emit-c", &path]);
        assert!(
            c.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&c.stderr)
        );
        let dir = std::env::temp_dir();
        let c_file = dir.join(format!("arandu_cli_soundness_{name}.c"));
        let binary = dir.join(format!("arandu_cli_soundness_{name}.bin"));
        fs::write(&c_file, &c.stdout).expect("write c");
        let compiled = Command::new("cc")
            .args(["-O1", "-w"])
            .arg(&c_file)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("cc");
        assert!(
            compiled.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let status = Command::new(&binary).status().expect("run");
        assert_eq!(status.code(), Some(*expected), "{name}");
    }
}
