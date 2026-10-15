use crate::common;
use std::fs;

#[test]
fn wasm_emission_preserves_explicit_target_identity_and_rejects_other_architectures() {
    let root = common::temp_dir("arandu-cli-wasm-target-identity").expect("reserve fixture");
    let source = root.join("main.aru");
    for (target, os) in [
        ("wasm32-unknown-unknown", "unknown"),
        ("wasm32-wasip1", "wasi"),
    ] {
        fs::write(&source, format!(
            "func main(): int {{ comptime if @targetOS() == \"{os}\" && @targetArch() == \"wasm32\" && @targetPointerWidth() == 32 {{ return 0 }} else {{ return unavailable_for_this_target }} }}"
        )).expect("write target consumer");
        let output = common::cli_command()
            .args(["emit-wasm", "--target", target])
            .arg(&source)
            .output()
            .expect("emit target consumer");
        assert!(
            output.status.success(),
            "{target}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.starts_with(b"\0asm"));
    }
    for target in ["wasm64-wasip1", "x86_64-unknown-linux-gnu"] {
        let output = common::cli_command()
            .args(["emit-wasm", "--target", target])
            .arg(&source)
            .output()
            .expect("reject incompatible target");
        assert!(!output.status.success(), "{target}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("requires a wasm32 target"),
            "{target}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(root).expect("remove target fixture");
}

#[test]
fn array_repetition_runs_once_and_supports_static_lengths_and_ctfe() {
    let root = common::temp_dir("arandu-cli-array-repeat").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(
        &source,
        r#"
func flags<comptime N: uint>(): [N]bool { return [false; N] }
func repeated<T, comptime N: uint>(value: T): [N]T { return [value; N] }
enum Packet { Data([64]u8), Empty }
func packet(): Packet { return .Data([42; 64]) }
struct Counter { value: int }
func next(counter: mut ref Counter): int { counter.value += 1; return counter.value }
func main(): int {
    let mut calls = Counter { value: 0 }
    let numbers = [next(calls); 3]
    let empty = [next(calls); 0]
    if calls.value != 2 || numbers[0] != 1 || numbers[2] != 1 { return 1 }
    let flags = flags<256>()
    let generic = repeated<int, 3>(42)
    let computed = [7; comptime (2 + 2)]
    let frozen = comptime { let mut table = [false; 256]; table[32] = true; table }
    let nested = comptime [[7; 2]; 3]
    if flags[255] || !frozen[32] || frozen[33] { return 2 }
    if generic[2] != 42 || computed[3] != 7 || nested[2][1] != 7 { return 3 }
    if @sizeOf([0]int) != 0 { return 4 }
    match packet() {
        Packet.Data(bytes) => { if bytes[63] != 42 { return 5 } }
        Packet.Empty => { return 6 }
    }
    return 0
}
"#,
    )
    .expect("write repeat consumer");
    for command in ["check", "run", "emit-c"] {
        let output = common::cli_command()
            .arg(command)
            .arg(&source)
            .output()
            .expect("run repeat consumer");
        assert!(output.status.success(), "{command}: {output:?}");
        if command == "emit-c"
            && std::process::Command::new("clang")
                .arg("--version")
                .output()
                .is_ok_and(|version| version.status.success())
        {
            let c_file = root.join("repeat.c");
            let binary = root.join(if cfg!(windows) {
                "repeat.exe"
            } else {
                "repeat"
            });
            fs::write(&c_file, &output.stdout).expect("write repeat C");
            let compiled = std::process::Command::new("clang")
                .arg("-O2")
                .arg(c_file)
                .arg("-o")
                .arg(&binary)
                .output()
                .expect("compile repeat C");
            assert!(compiled.status.success(), "{compiled:?}");
            let executed = std::process::Command::new(binary)
                .output()
                .expect("execute repeat C");
            assert!(executed.status.success(), "{executed:?}");
        }
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn array_repetition_rejects_ownership_duplication_and_excessive_lengths() {
    let root = common::temp_dir("arandu-cli-array-repeat-invalid").expect("reserve fixture");
    let source = root.join("main.aru");
    for program in [
        "func main(): void { let mut x = 0; let refs = [mut ref x; 2] }",
        "func main(): void { let x = [false; 65537] }",
        "func main(): void { let x = [false; 18446744073709551615] }",
        "func main(): void { let x = [false; bool] }",
        "func main(): void { let x = [[false; 65536]; 65536] }",
        "func repeat<T, comptime N: uint>(x: T): [N]T { return [x; N] }\nfunc main(): void { let mut x = 0; let refs = repeat<mut ref int, 2>(mut ref x) }",
    ] {
        fs::write(&source, program).expect("write invalid repetition");
        let output = common::cli_command()
            .arg("check")
            .arg(&source)
            .output()
            .expect("check invalid repetition");
        assert!(!output.status.success(), "{program}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("T048"),
            "{program}: {output:?}"
        );
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn zero_and_one_repetitions_drop_owners_exactly_once() {
    let root = common::temp_dir("arandu-cli-array-repeat-drop").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(
        &source,
        r#"
import std.io as io
struct Owner { id: int }
@Destructor
func Owner.destroy(own self: Owner): void { io.println("drop ${self.id}") }
func make(id: int): Owner { return Owner { id: id } }
func main(): int {
    let empty = [make(10); 0]
    io.println("after zero")
    let single = [make(20); 1]
    if single[0].id != 20 { return 1 }
    return 0
}
"#,
    )
    .expect("write owning repetition");
    let stdlib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../stdlib");
    let output = common::cli_command()
        .arg("run")
        .arg(&source)
        .arg("--stdlib-path")
        .arg(stdlib)
        .output()
        .expect("run owning repetition");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "drop 10\nafter zero\ndrop 20\n"
    );
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn stdlib_byte_sets_freeze_imported_aggregate_helpers_and_match_all_bytes() {
    let root = common::temp_dir("arandu-cli-byte-set").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(&source, r#"
import std.core.ascii as ascii
func main(): int {
    let horizontal = comptime ascii.ByteSet.from(" \t\r")
    let identifier = comptime ascii.ByteSet.from("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_")
    let empty = comptime ascii.ByteSet.new()
    let horizontalTable = comptime ascii.byteTable(" \t\r")
    let edgeTable = comptime ascii.byteTable("\0?@Àÿ??\0")
    let edge = comptime ascii.ByteSet.from("\0?@Àÿ??\0")
    let runtime = ascii.ByteSet.from(" \t\r")
    if @sizeOf(ascii.ByteSet) != 32 { return 1 }
    let mut value: uint = 0
    while value < 256 {
        let b = value as u8
        let expectedSpace = b == 32 || b == 9 || b == 13
        let expectedWord = (b >= 97 && b <= 122) || (b >= 65 && b <= 90)
            || (b >= 48 && b <= 57) || b == 95
        let expectedEdge = b == 0 || b == 63 || b == 64 || b == 195 || b == 128 || b == 191
        if horizontal.contains(b) != expectedSpace { return 2 }
        if identifier.contains(b) != expectedWord { return 3 }
        if empty.contains(b) { return 4 }
        if edge.contains(b) != expectedEdge { return 5 }
        if runtime.contains(b) != horizontal.contains(b) { return 6 }
        if horizontalTable[value as usize] != expectedSpace { return 7 }
        if edgeTable[value as usize] != expectedEdge { return 8 }
        value = value + 1
    }
    return 0
}
"#).expect("write byte-set consumer");
    let stdlib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../stdlib");
    for command in ["check", "run", "amir", "emit-c"] {
        let output = common::cli_command()
            .arg(command)
            .arg(&source)
            .arg("--stdlib-path")
            .arg(&stdlib)
            .output()
            .expect("execute byte-set consumer");
        assert!(output.status.success(), "{command}: {output:?}");
        if command == "amir" {
            let amir = std::str::from_utf8(&output.stdout).expect("AMIR output");
            assert_eq!(
                amir.matches("call fn@ByteSet.from(").count(),
                1,
                "only the explicitly runtime builder may remain:\n{amir}"
            );
        }
        if command == "emit-c"
            && std::process::Command::new("clang")
                .arg("--version")
                .output()
                .is_ok_and(|version| version.status.success())
        {
            let c_file = root.join("byte_set.c");
            let binary = root.join(if cfg!(windows) {
                "byte_set.exe"
            } else {
                "byte_set"
            });
            fs::write(&c_file, output.stdout).expect("write emitted C");
            let compile = std::process::Command::new("clang")
                .arg("-O2")
                .arg(&c_file)
                .arg("-o")
                .arg(&binary)
                .output()
                .expect("compile byte-set consumer");
            assert!(compile.status.success(), "{compile:?}");
            let executed = std::process::Command::new(&binary)
                .output()
                .expect("run byte-set C consumer");
            assert!(executed.status.success(), "{executed:?}");
        }
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn static_loops_execute_with_fresh_locals_and_structured_exits() {
    let root = common::temp_dir("arandu-cli-static-for").expect("reserve fixture");
    let source = root.join("main.aru");
    for (index, program) in [
        "func main(): int { let mut sum = 0\ncomptime for i in 0..4 { let item = i + 1\nsum += item }\nreturn sum - 10 }",
        "func main(): int { let mut sum = 0\ncomptime for i in 0..4 { if i == 1 { continue } if i == 3 { break } sum += i }\nreturn sum - 2 }",
        "func main(): int { let mut sum = 0\ncomptime for i in 0..3 { comptime for j in 0..2 { sum += i + j } }\nreturn sum - 9 }",
        "func first(): int { comptime for i in 0..4 { return i + 7 } return 99 }\nfunc main(): int { return first() - 7 }",
        "func main(): int { return comptime { let mut sum = 0\ncomptime for i in 0..4 { sum += i }\nsum } - 6 }",
    ].iter().enumerate() {
        fs::write(&source, program).expect("write static loop source");
        for command in ["check", "run", "emit-c"] {
            let output = common::cli_command().arg(command).arg(&source).output()
                .expect("execute static loop source");
            assert!(output.status.success(), "case {index}, {command}: {output:?}");
            if command == "emit-c" {
                assert!(!String::from_utf8_lossy(&output.stdout).contains("comptime for"));
            }
        }
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn public_layout_expressions_execute_and_emit_c_without_runtime_intrinsics() {
    let root = common::temp_dir("arandu-cli-layout").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(&source, "func size<T>(): usize { return @sizeOf(T) }\nfunc main(): int { let n = comptime (@sizeOf([3]u16)); if n == 6 && @alignOf(i64) == 8 && size<int>() == 4 { return 0 } return 1 }")
        .expect("write layout source");
    for command in ["check", "run", "emit-c"] {
        let output = common::cli_command()
            .arg(command)
            .arg(&source)
            .output()
            .expect("run layout expressions");
        assert!(output.status.success(), "{command}: {output:?}");
        if command == "emit-c" {
            let emitted = String::from_utf8(output.stdout).expect("C output");
            assert!(!emitted.contains("@sizeOf"));
            assert!(!emitted.contains("@alignOf"));
        }
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn public_comptime_runs_from_a_standalone_source() {
    let root = common::temp_dir("arandu-cli-comptime").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(
        &source,
        "func add(a: int, b: int): int { return a + b }\n\
         func main(): int {\n\
         let answer = comptime { return add(20, 22) }\n\
         comptime if true { if answer == 42 { return 0 } } else { unavailable() }\nreturn 1\n}\n",
    )
    .expect("write source");
    for command in ["check", "run"] {
        let output = common::cli_command()
            .arg(command)
            .arg(&source)
            .output()
            .expect("execute public syntax");
        assert!(output.status.success(), "{command}: {output:?}");
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn public_comptime_capture_reports_context_instead_of_running() {
    let root = common::temp_dir("arandu-cli-comptime-capture").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(
        &source,
        "func main(): int { let outside = 42\nreturn comptime outside }\n",
    )
    .expect("write source");
    let output = common::cli_command()
        .args(["check", "--color=never"])
        .arg(&source)
        .output()
        .expect("check forbidden capture");
    assert!(!output.status.success(), "{output:?}");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("T043"), "{error}");
    assert!(error.contains("runtime value 'outside'"), "{error}");
    assert!(error.contains("runtime value declared here"), "{error}");
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn public_comptime_formats_multiple_sources_without_changing_their_results() {
    let root = common::temp_dir("arandu-cli-comptime-fmt").expect("reserve fixture");
    let first = root.join("first.aru");
    let second = root.join("second.aru");
    fs::write(
        &first,
        "func main():int{let x=comptime{return 42}\nreturn x-42}",
    )
    .expect("write block");
    fs::write(&second, "func main():int{return comptime(20+22)-42}").expect("write expression");
    let third = root.join("third.aru");
    fs::write(&third, "func count<comptime N:uint>():uint{return N}\nfunc main():int{return(count<comptime(20+22)>() as int)-42}")
        .expect("write computed argument");
    for check in [false, true] {
        let mut command = common::cli_command();
        command.arg("fmt");
        if check {
            command.arg("--check");
        }
        let output = command
            .args([&first, &second, &third])
            .output()
            .expect("format sources");
        assert!(output.status.success(), "{output:?}");
    }
    for source in [&first, &second, &third] {
        let output = common::cli_command()
            .arg("run")
            .arg(source)
            .output()
            .expect("run formatted source");
        assert!(output.status.success(), "{source:?}: {output:?}");
    }
    fs::remove_dir_all(root).expect("remove fixtures");
}

#[test]
fn frozen_core_values_run_on_native_c_and_emit_for_the_wasm_target() {
    let root = common::temp_dir("arandu-cli-frozen-core").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(
        &source,
        r#"
import std.target as target
struct Table { entries: [comptime (1 + 1)]int }
enum Packet { Data([64]u8), Empty }
func packet(): Packet { return Packet.Data([42; 64]) }
func table(): Table { return Table { entries: [20, 22] } }
func answer(): Result<int, int> {
    let value: Result<int, int> = comptime { return Result.Ok(comptime (20 + 22)) }
    return value
}
func policy<comptime ENABLED: bool, comptime DELTA: i64>(): int {
    comptime if ENABLED { return (43 + DELTA) as int } else { return 0 }
}
const PACKET = comptime packet()
const TABLE = comptime table()
const ANSWER = comptime answer()
func main(): int {
    if TABLE.entries[0] + TABLE.entries[1] != 42 { return 1 }
    match PACKET {
        Packet.Data(bytes) => { if bytes[63] != 42 { return 2 } }
        Packet.Empty => { return 3 }
    }
    match ANSWER {
        Result.Ok(value) => { if value != 42 { return 4 } }
        Result.Err(error) => { return 5 }
    }
    if comptime policy<comptime (true), comptime (-1)>() != 42 { return 6 }
    comptime if target.arch == "wasm32" {
        if target.pointer_width != 32 || target.os != "wasi" { return 7 }
    } else {
        if target.pointer_width != @targetPointerWidth() { return 8 }
    }
    return 0
}
"#,
    )
    .expect("write core consumer");
    let stdlib = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../stdlib");
    for command in ["check", "run", "emit-c", "emit-wasm"] {
        let output = common::cli_command()
            .arg(command)
            .arg(&source)
            .arg("--stdlib-path")
            .arg(&stdlib)
            .output()
            .expect("run core consumer");
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if command == "emit-wasm" {
            assert!(arandu_backend_wasm::is_core_module(&output.stdout));
        }
        if command == "emit-c"
            && std::process::Command::new("clang")
                .arg("--version")
                .output()
                .is_ok_and(|version| version.status.success())
        {
            let c_file = root.join("core.c");
            let binary = root.join(if cfg!(windows) { "core.exe" } else { "core" });
            fs::write(&c_file, &output.stdout).expect("write core C");
            let compiled = std::process::Command::new("clang")
                .args(["-O2", "-fstrict-aliasing"])
                .arg(&c_file)
                .arg("-o")
                .arg(&binary)
                .output()
                .expect("compile core C");
            assert!(
                compiled.status.success(),
                "{}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            let executed = std::process::Command::new(&binary)
                .output()
                .expect("execute core C");
            assert!(executed.status.success(), "{executed:?}");
        }
    }
    fs::remove_dir_all(root).expect("remove fixtures");
}

#[test]
fn global_table_sources_are_shared_without_aliasing_mutable_copies() {
    let root = common::temp_dir("arandu-cli-shared-table").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(&source, "const TABLE = comptime [3 as u8; 64]\nfunc modify(): int { let mut table = TABLE; table[0] = 9; return table[0] as int }\nfunc read(): int { return TABLE[0] as int }\nfunc main(): int { if modify() != 9 || read() != 3 { return 1 } return 0 }").expect("write sharing consumer");
    for command in ["run", "emit-c", "emit-wasm"] {
        let output = common::cli_command()
            .arg(command)
            .arg(&source)
            .output()
            .expect("run sharing consumer");
        assert!(
            output.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if command == "emit-wasm" {
            assert_eq!(
                output
                    .stdout
                    .windows(64)
                    .filter(|window| window.iter().all(|byte| *byte == 3))
                    .count(),
                1
            );
        }
        if command == "emit-c" {
            let generated = String::from_utf8_lossy(&output.stdout);
            assert_eq!(
                generated
                    .matches("static const uint8_t __ar_static_")
                    .count(),
                1
            );
            if std::process::Command::new("clang")
                .arg("--version")
                .output()
                .is_ok_and(|version| version.status.success())
            {
                let c_file = root.join("shared.c");
                fs::write(&c_file, &output.stdout).expect("write shared C");
                for level in ["-O0", "-O2"] {
                    let binary = root.join(if cfg!(windows) {
                        "shared.exe"
                    } else {
                        "shared"
                    });
                    let compiled = std::process::Command::new("clang")
                        .args([level, "-fstrict-aliasing"])
                        .arg(&c_file)
                        .arg("-o")
                        .arg(&binary)
                        .output()
                        .expect("compile shared C");
                    assert!(
                        compiled.status.success(),
                        "{}",
                        String::from_utf8_lossy(&compiled.stderr)
                    );
                    assert!(
                        std::process::Command::new(&binary)
                            .status()
                            .expect("execute shared C")
                            .success()
                    );
                }
            }
        }
    }
    fs::remove_dir_all(root).expect("remove sharing fixtures");
}

#[test]
fn concrete_declaration_headers_share_types_and_layouts_across_backends() {
    let root = common::temp_dir("arandu-concrete-headers").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(
        &source,
        r#"
struct Matrix<comptime R: uint, comptime C: uint> { data: [comptime (R * C)]int }
enum Packet<comptime N: uint> { Data([comptime (N * 2)]int), Empty }
func pair<comptime N: uint>(): [comptime (N * 2)]int { return [20, 22] }
func forward<comptime N: uint>(): [comptime (N * 2)]int { return pair<N>() }
func bytes<T>(x: T): [comptime (@sizeOf(T))]u8 { return [1; comptime (@sizeOf(T))] }
func packet(): Packet<1> { return .Data([20, 22]) }
func frozen(): Matrix<1, 2> { return Matrix<1, 2> { data: [20, 22] } }
func main(): int {
    let a = Matrix<1, 2> { data: [20, 22] }
    let b = Matrix<2, 2> { data: [1, 2, 3, 4] }
    let p: [2]int = forward<1>()
    let x: u32 = 42
    let small: [4]u8 = bytes(x)
    let big: [8]u8 = bytes<u64>(42)
    if @sizeOf(Matrix<1, 2>) != 8 || @sizeOf(Matrix<2, 2>) != 16 { return 1 }
    if a.data[0] + a.data[1] != 42 || b.data[3] != 4 { return 2 }
    if p[0] + p[1] != 42 || small[3] != 1 || big[7] != 1 { return 3 }
    let c = comptime frozen()
    if c.data[0] + c.data[1] != 42 { return 4 }
    match packet() { Packet.Data(data) => { if data[0] + data[1] != 42 { return 5 } }
                    Packet.Empty => { return 6 } }
    return 0
}
"#,
    )
    .expect("write concrete headers");
    for command in ["check", "run", "emit-c", "emit-wasm"] {
        let output = common::cli_command()
            .arg(command)
            .arg(&source)
            .output()
            .expect("compile concrete headers");
        assert!(output.status.success(), "{command}: {output:?}");
        if command == "emit-c"
            && std::process::Command::new("clang")
                .arg("--version")
                .output()
                .is_ok_and(|version| version.status.success())
        {
            let c = root.join("headers.c");
            let binary = root.join(if cfg!(windows) {
                "headers.exe"
            } else {
                "headers"
            });
            fs::write(&c, &output.stdout).expect("write generated C");
            let compiled = std::process::Command::new("clang")
                .arg("-O2")
                .arg(&c)
                .arg("-o")
                .arg(&binary)
                .output()
                .expect("compile generated C");
            assert!(compiled.status.success(), "{compiled:?}");
            let executed = std::process::Command::new(binary)
                .output()
                .expect("run generated C");
            assert!(executed.status.success(), "{executed:?}");
        }
        if command == "emit-wasm" {
            assert!(output.stdout.starts_with(b"\0asm"));
        }
    }
    fs::remove_dir_all(root).expect("remove concrete fixture");
}

#[test]
fn public_ctfe_limits_are_applied_and_invalid_options_are_usage_errors() {
    let root = common::temp_dir("arandu-cli-ctfe-limits").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(&source, "func main(): int { return comptime (20 + 22) }").expect("write source");
    for args in [
        vec!["--ctfe-fuel=1"],
        vec!["--ctfe-values", "1"],
        vec!["--ctfe-frames=1", "--ctfe-fuel=1"],
    ] {
        let output = common::cli_command()
            .arg("check")
            .arg(&source)
            .args(args)
            .output()
            .expect("check limited source");
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("T045"),
            "{output:?}"
        );
    }
    for value in ["0", "-1", "text", "18446744073709551616"] {
        let output = common::cli_command()
            .arg("check")
            .arg(&source)
            .arg(format!("--ctfe-fuel={value}"))
            .output()
            .expect("reject option");
        assert_eq!(output.status.code(), Some(2), "{output:?}");
    }
    let output = common::cli_command()
        .arg("check")
        .arg(&source)
        .output()
        .expect("default envelope");
    assert!(output.status.success(), "{output:?}");
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn static_array_iteration_and_header_arguments_run_on_each_backend() {
    let root = common::temp_dir("arandu-cli-static-array-iteration").expect("reserve fixture");
    let source = root.join("main.aru");
    fs::write(&source, r#"
struct Entry { value: int }
enum Mode { First, Second }
const TABLE [2]int = comptime [20, 22]
func count<comptime N: uint>(): uint { return N }
func delta<comptime Enabled: bool>(): int { comptime if Enabled { return 20 } else { return 22 } }
func produce(): [2][2]int { return [[20, 0], [21, 1]] }
func first(): uint { for i in 0..count<comptime (2)>() { return i }; return 99 }
func c_first(): int { for let mut i = 0; i < 2; i = i + 1 { return i }; return 99 }
func main(): int {
    let mut sum = 0
    comptime for value in TABLE { sum += value }
    if sum != 42 { return 1 }
    sum = 0
    comptime for row in produce() { comptime for value in row { sum += value } }
    if sum != 42 { return 2 }
    sum = 0
    comptime for row in [[true, false], [false, true]] { sum += delta<comptime (row[0])>() }
    if sum != 42 { return 3 }
    sum = 0
    comptime for entry in [Entry { value: 20 }, Entry { value: 22 }] { let value = comptime entry.value; sum += value }
    if sum != 42 { return 4 }
    sum = 0
    comptime for mode in [Mode.First, Mode.Second] {
        let value = comptime { match mode { Mode.First => { return 20 } Mode.Second => { return 22 } } }
        sum += value
    }
    if sum != 42 { return 5 }
    sum = 0
    comptime for label in ["a", "b"] { comptime if label == "a" { sum += 20 } else { sum += 22 } }
    if sum != 42 { return 6 }
    if first() != 0 || c_first() != 0 { return 7 }
    let mut visits = 0
    let max: u64 = 18446744073709551615
    for value in max..=max { if value != max { return 9 }; visits += 1 }
    if visits != 1 { return 10 }
    for value in 2..1 { return 11 }
    visits = 0
    for value in 1..=3 { if value == 2 { continue }; visits += value }
    if visits != 4 { return 12 }
    sum = 0
    for value in [20, 22] { sum += value }
    if sum != 42 { return 13 }
    sum = 0
    for label in ["a", "b"] { if label == "a" { sum += 20 } else { sum += 22 } }
    if sum != 42 { return 14 }
    if Option.Some(count<comptime (42)>()) is Option.Some(value) && value == 42 { return 0 }
    return 8
}
"#).expect("write consumer");
    for optimized in [false, true] {
        for command in ["run", "emit-c", "emit-wasm"] {
            let mut invocation = common::cli_command();
            invocation.arg(command).arg(&source);
            if optimized {
                invocation.arg("--opt");
            }
            let output = invocation.output().expect("compile and run consumer");
            assert!(
                output.status.success(),
                "{command}, optimized={optimized}: {output:?}"
            );
            if command == "emit-wasm" {
                assert!(output.stdout.starts_with(b"\0asm"));
            }
            if command == "emit-c"
                && std::process::Command::new("clang")
                    .arg("--version")
                    .output()
                    .is_ok_and(|version| version.status.success())
            {
                let c_file = root.join("iteration.c");
                let binary = root.join(if cfg!(windows) {
                    "iteration.exe"
                } else {
                    "iteration"
                });
                fs::write(&c_file, &output.stdout).expect("write C");
                let compiled = std::process::Command::new("clang")
                    .arg(if optimized { "-O2" } else { "-O0" })
                    .arg(&c_file)
                    .arg("-o")
                    .arg(&binary)
                    .output()
                    .expect("compile C");
                assert!(compiled.status.success(), "{compiled:?}");
                let executed = std::process::Command::new(&binary)
                    .output()
                    .expect("execute C");
                assert!(executed.status.success(), "{executed:?}");
            }
        }
    }
    fs::remove_dir_all(root).expect("remove fixture");
}
