#![cfg(target_pointer_width = "64")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use arandu_backend_cranelift::CraneliftBackend;
use arandu_middle::amir::{AmirConstant, AmirOperand, AmirProgram, AmirRvalue, AmirStmt};
use arandu_middle::layout::DataLayout;
use arandu_middle::ops::BinaryOp;
use arandu_semantics::{
    CodegenBackend, OptLevel, TypeCheckResult, lower_to_amir_with_interfaces, lower_to_hir,
    optimize_amir_checked_with_level, resolve_for_test, type_check,
};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::process::Command;

fn c_compiler(cc: &str) -> Command {
    let mut command = Command::new(cc);
    command.arg("-fwrapv");
    if env::var_os("ARANDU_C_SANITIZERS").is_some() {
        command.args([
            "-O1",
            "-g",
            "-fno-omit-frame-pointer",
            "-fsanitize=address,undefined",
        ]);
    }
    command
}

#[test]
fn parity_bit_intrinsics_cover_every_bit_position_and_zero() {
    test_zero_result_all_opt_levels("bit_intrinsics", include_str!("bit_intrinsics.aru"));
}

#[test]
fn atomic_intrinsics_survive_real_native_thread_contention() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let (amir, tc) = compile_src(include_str!("../../../tests/corpus/atomic_contention.aru"));
    let module = CraneliftBackend::try_new()
        .unwrap()
        .compile(&amir, &tc.symbols, &tc.type_info)
        .unwrap();
    for name in ["increment", "incrementCas"] {
        let symbol = tc
            .symbols
            .iter()
            .find(|symbol| symbol.name.rsplit('.').next() == Some(name))
            .unwrap();
        let host_name = tc.symbols.host_func_name(symbol);
        // SAFETY: Both exports have exactly the declared pointer/i32 -> void
        // ABI. The module stays alive until every scoped worker has joined.
        let increment = unsafe {
            module
                .get_fn::<unsafe extern "C" fn(*mut u32, i32)>(host_name)
                .unwrap()
        };
        let counter = AtomicU32::new(0);
        std::thread::scope(|scope| {
            for _ in 0..6 {
                let shared = &counter;
                scope.spawn(move || {
                    // SAFETY: AtomicU32 provides aligned, live storage; every
                    // concurrent access in the generated function is atomic.
                    unsafe { increment(shared.as_ptr(), 50_000) };
                });
            }
        });
        assert_eq!(counter.load(Ordering::SeqCst), 300_000, "{name}");
    }
}

#[test]
#[cfg(unix)]
fn generated_c_atomic_intrinsics_survive_pthread_contention() {
    let (amir, tc) = compile_src(include_str!("../../../tests/corpus/atomic_contention.aru"));
    let source = format!(
        "#define main arandu_unused_main\n{}\n#undef main\n{}",
        emit_c(&amir, &tc),
        r#"
#include <pthread.h>
static uint32_t counter = 0;
static int use_cas = 0;
static void *worker(void *unused) {
    (void)unused;
    if (use_cas) std__core__atomicContention__incrementCas(&counter, 50000);
    else std__core__atomicContention__increment(&counter, 50000);
    return NULL;
}
int main(void) {
    for (use_cas = 0; use_cas < 2; ++use_cas) {
        __atomic_store_n(&counter, 0, __ATOMIC_SEQ_CST);
        pthread_t workers[6];
        for (int i = 0; i < 6; ++i) if (pthread_create(&workers[i], NULL, worker, NULL)) return 1;
        for (int i = 0; i < 6; ++i) if (pthread_join(workers[i], NULL)) return 2;
        if (__atomic_load_n(&counter, __ATOMIC_SEQ_CST) != 300000) return 3;
    }
    return 0;
}
"#
    );
    let directory = std::env::temp_dir().join(format!("arandu-atomic-c-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("atomic.c");
    fs::write(&path, source).unwrap();
    for optimization in ["-O0", "-O2"] {
        let executable = directory.join("atomic");
        let output = Command::new("cc")
            .args([
                optimization,
                "-pthread",
                "-fsanitize=undefined",
                "-fno-sanitize-recover=all",
            ])
            .arg(&path)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(executable).output().unwrap();
        assert!(
            output.status.success(),
            "{:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(unix)]
fn generated_c_udp_socket_table_handles_concurrent_bind_send_close() {
    let (amir, tc) = compile_src(
        r#"
extern "C" {
    func ar_net_udp_bind(host: ptr[u8]?, host_len: usize, port: int): int
}
func main(): int {
    return unsafe { ar_net_udp_bind(nil, 0 as usize, 0) }
}
"#,
    );
    let source = format!(
        "#define main arandu_unused_main\n{}\n#undef main\n{}",
        emit_c(&amir, &tc),
        r#"
#include <pthread.h>
#include <stdatomic.h>
#include <sched.h>

enum { UDP_WORKERS = 8 };
static atomic_int start_workers = 0;
static atomic_int senders_ready = 0;
static atomic_int start_senders = 0;
static int socket_ids[UDP_WORKERS];
static const uint8_t loopback[] = "127.0.0.1";
static const uint8_t payload = 42;

static void *bind_udp_socket(void *argument) {
    intptr_t index = (intptr_t)argument;
    while (!atomic_load_explicit(&start_workers, memory_order_acquire)) sched_yield();
    socket_ids[index] = ar_net_udp_bind(loopback, sizeof(loopback) - 1, 0);
    return NULL;
}

static void *send_udp_packets(void *argument) {
    intptr_t index = (intptr_t)argument;
    atomic_fetch_add_explicit(&senders_ready, 1, memory_order_release);
    while (!atomic_load_explicit(&start_senders, memory_order_acquire)) sched_yield();
    (void)ar_net_udp_send_to(
        socket_ids[index], &payload, 1, loopback, sizeof(loopback) - 1, 9
    );
    return NULL;
}

int main(void) {
    pthread_t workers[UDP_WORKERS];
    for (intptr_t i = 0; i < UDP_WORKERS; ++i) {
        if (pthread_create(&workers[i], NULL, bind_udp_socket, (void *)i) != 0) return 1;
    }
    atomic_store_explicit(&start_workers, 1, memory_order_release);
    for (int i = 0; i < UDP_WORKERS; ++i) {
        if (pthread_join(workers[i], NULL) != 0) return 2;
    }
    for (int i = 0; i < UDP_WORKERS; ++i) {
        if (socket_ids[i] < 0) return 3;
        for (int j = 0; j < i; ++j) {
            if (socket_ids[i] == socket_ids[j]) return 4;
        }
    }
    for (int i = 0; i < UDP_WORKERS; ++i) {
        if (pthread_create(&workers[i], NULL, send_udp_packets, (void *)(intptr_t)i) != 0) return 5;
    }
    while (atomic_load_explicit(&senders_ready, memory_order_acquire) != UDP_WORKERS) sched_yield();
    atomic_store_explicit(&start_senders, 1, memory_order_release);
    for (int i = 0; i < UDP_WORKERS; ++i) ar_net_udp_close(socket_ids[i]);
    for (int i = 0; i < UDP_WORKERS; ++i) {
        if (pthread_join(workers[i], NULL) != 0) return 6;
    }
    return 0;
}
"#
    );
    let directory = env::temp_dir().join(format!("arandu-udp-c-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("udp.c");
    let executable = directory.join("udp");
    fs::write(&path, source).unwrap();
    let compiler = env::var("CC").unwrap_or_else(|_| "cc".into());
    let mut command = Command::new(compiler);
    command.args(["-O2", "-pthread"]);
    if cfg!(target_os = "linux") && env::var_os("ARANDU_C_TSAN").is_some() {
        command.arg("-fsanitize=thread");
    }
    let output = command
        .arg(&path)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "C compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(&executable).output().unwrap();
    assert!(
        output.status.success(),
        "UDP table stress failed: status={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(unix)]
fn generated_c_udp_accepts_localhost_ipv6_and_more_than_64_sockets() {
    let (amir, tc) = compile_src(
        r#"
extern "C" {
    func ar_net_udp_bind(host: ptr[u8]?, host_len: usize, port: int): int
}
func main(): int {
    return unsafe { ar_net_udp_bind(nil, 0 as usize, 0) }
}
"#,
    );
    let source = format!(
        "#define main arandu_unused_main\n{}\n#undef main\n{}",
        emit_c(&amir, &tc),
        r#"
int main(void) {
    const uint8_t v6[] = "::1";
    int v6_id = ar_net_udp_bind(v6, sizeof(v6) - 1, 0);
    if (v6_id < 0) return 7;
    ar_net_udp_close(v6_id);

    const uint8_t local[] = "localhost";
    int local_id = ar_net_udp_bind(local, sizeof(local) - 1, 0);
    if (local_id < 0) return 8;
    ar_net_udp_close(local_id);

    enum { UDP_COUNT = 70 };
    int ids[UDP_COUNT];
    const uint8_t loopback[] = "127.0.0.1";
    for (int i = 0; i < UDP_COUNT; i++) {
        ids[i] = ar_net_udp_bind(loopback, sizeof(loopback) - 1, 0);
        if (ids[i] < 0) return 9;
        for (int j = 0; j < i; j++) {
            if (ids[i] == ids[j]) return 10;
        }
    }
    int first = ids[0];
    ar_net_udp_close(first);
    int reused = ar_net_udp_bind(loopback, sizeof(loopback) - 1, 0);
    if (reused < 0 || reused == first) return 11;
    if (ar_net_udp_send_to(first, loopback, 1, loopback, sizeof(loopback) - 1, 9) != -1) return 12;
    for (int i = 1; i < UDP_COUNT; i++) ar_net_udp_close(ids[i]);
    ar_net_udp_close(reused);
    return 0;
}
"#
    );
    let directory = env::temp_dir().join(format!(
        "arandu-udp-parity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("udp.c");
    let executable = directory.join("udp");
    fs::write(&path, source).unwrap();
    let compiler = env::var("CC").unwrap_or_else(|_| "cc".into());
    let output = Command::new(compiler)
        .args(["-O2", "-pthread"])
        .arg(&path)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "C compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(&executable).output().unwrap();
    assert!(
        output.status.success(),
        "UDP parity failed: status={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn parity_ordinary_bit_names_are_not_intrinsics() {
    test_zero_result_all_opt_levels(
        "ordinary_bit_names",
        "func bitCountOnes32(value: u32): u32 { return value + 7 } func main(): int { if bitCountOnes32(5) != 12 { return 1 }; return 0 }",
    );
}

#[test]
fn parity_ordinary_extern_with_intrinsic_spelling_keeps_its_c_abi() {
    let (amir, tc) = compile_src(
        r#"
extern "C" {
    func sliceLen(value: u32): u32
}

func main(): int {
    let result = unsafe { sliceLen(5) }
    if result == 0 { return 1 }
    return 0
}
"#,
    );
    let emitted = emit_c(&amir, &tc);
    assert!(
        emitted.contains("sliceLen("),
        "ordinary extern should retain its declared C symbol"
    );
    assert!(
        !emitted.contains("ar_slice_len"),
        "ordinary extern must not be rewritten to the slice intrinsic"
    );
}

#[test]
fn parity_user_black_box_name_is_not_a_compiler_barrier() {
    test_zero_result_all_opt_levels(
        "user_black_box_name",
        "func blackBox(value: int): int { return value + 1 } func main(): int { if blackBox(41) != 42 { return 1 }; return 0 }",
    );
}

#[test]
fn parity_integer_division_and_remainder_boundaries() {
    test_zero_result_all_opt_levels(
        "integer_division_boundaries",
        r#"
module std.testing
func blackBox<T>(value: T): T { return value }
func main(): int {
    let minusOne = blackBox<int>(-1)
    let minimum = blackBox<int>(-2147483648)
    if minimum % minusOne != 0 { return 1 }
    if blackBox<int>(-43) / blackBox<int>(5) != -8 { return 2 }
    if blackBox<int>(-43) % blackBox<int>(5) != -3 { return 3 }
    let wideMinimum = blackBox<i64>(-9223372036854775808)
    let wideMinusOne = blackBox<i64>(-1)
    if wideMinimum % wideMinusOne != 0 { return 4 }
    if wideMinimum / blackBox<i64>(1) != wideMinimum { return 5 }
    if blackBox<u32>(4294967295) / blackBox<u32>(2) != 2147483647 { return 6 }
    if blackBox<u64>(18446744073709551615) % blackBox<u64>(2) != 1 { return 7 }
    if blackBox<i8>(-128) % blackBox<i8>(-1) != 0 { return 8 }
    if blackBox<i16>(-32768) % blackBox<i16>(-1) != 0 { return 9 }
    return 0
}
"#,
    );
}

#[test]
fn parity_dynamic_shifts_mask_the_value_execution_width() {
    test_zero_result_all_opt_levels(
        "dynamic_shift_width",
        r#"
module std.testing
func blackBox<T>(value: T): T { return value }
func main(): int {
    let wideCount = blackBox<i32>(32)
    if blackBox<i32>(1) << wideCount != 1 { return 1 }
    if blackBox<i32>(-8) >> wideCount != -8 { return 2 }
    if blackBox<u32>(2147483648) >> blackBox<u32>(33) != 1073741824 { return 3 }
    if blackBox<i64>(-8) >> blackBox<i64>(65) != -4 { return 4 }
    if blackBox<u64>(1) << blackBox<u64>(127) != (9223372036854775808 as u64) { return 5 }
    if blackBox<i32>(1) << blackBox<i32>(31) != -2147483648 { return 6 }
    if blackBox<i32>(-1) << blackBox<i32>(1) != -2 { return 7 }
    if blackBox<i8>(-128) >> blackBox<i8>(8) != -1 { return 8 }
    if blackBox<u8>(255) >> blackBox<u8>(8) != 0 { return 9 }
    if blackBox<i16>(-32768) >> blackBox<i16>(16) != -1 { return 10 }
    if blackBox<u16>(65535) >> blackBox<u16>(16) != 0 { return 11 }
    if blackBox<i32>(-8) >> blackBox<i32>(-1) != -1 { return 12 }
    return 0
}
"#,
    );
}

#[test]
fn cranelift_integer_division_traps_match_declared_width() {
    const CASE: &str = "ARANDU_INTEGER_TRAP_CASE";
    if let Ok(case) = env::var(CASE) {
        let fields: Vec<_> = case.split(':').collect();
        let ty = fields[0];
        let operator = fields[1];
        let left = fields[2];
        let right = fields[3];
        let (amir, tc) = compile_src(&format!(
            "module std.testing\nfunc blackBox<T>(value: T): T {{ return value }} func main(): int {{ return (blackBox<{ty}>({left}) {operator} blackBox<{ty}>({right})) as int }}"
        ));
        execute_cranelift(&amir, &tc);
        return;
    }
    let executable = env::current_exe().unwrap();
    for (ty, minimum) in [
        ("i8", "-128"),
        ("i16", "-32768"),
        ("i32", "-2147483648"),
        ("i64", "-9223372036854775808"),
    ] {
        for (operator, left, right) in [("/", minimum, "-1"), ("/", "7", "0"), ("%", "7", "0")] {
            let output = Command::new(&executable)
                .args([
                    "--exact",
                    "cranelift_integer_division_traps_match_declared_width",
                ])
                .env(CASE, format!("{ty}:{operator}:{left}:{right}"))
                .output()
                .unwrap();
            assert!(
                !output.status.success(),
                "{ty} {left} {operator} {right}: must trap"
            );
            assert_ne!(
                output.status.code(),
                Some(101),
                "child test panicked: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn c_integer_division_traps_before_undefined_operations() {
    let directory = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&directory).unwrap();
    let compiler = env::var("CC").unwrap_or_else(|_| "gcc".into());
    for (ty, c_ty, minimum, signed) in [
        ("i32", "int32_t", "INT32_MIN", true),
        ("i64", "int64_t", "INT64_MIN", true),
        ("u32", "uint32_t", "0", false),
        ("u64", "uint64_t", "0", false),
    ] {
        for operation in ["/", "%"] {
            let (amir, tc) = compile_src(&format!(
                "public func probe(a: {ty}, b: {ty}): {ty} {{ return a {operation} b }} func main(): int {{ return 0 }}"
            ));
            let emitted = emit_c(&amir, &tc);
            let source = format!(
                "#define main arandu_unused_main\n{emitted}\n#undef main\nint main(int argc, char **argv) {{ (void)argv; volatile {c_ty} a = argc == 1 ? 7 : {minimum}; volatile {c_ty} b = argc == 1 ? 0 : ({c_ty})-1; volatile {c_ty} result = probe(a, b); return result != 0; }}\n"
            );
            for optimization in ["-O0", "-O1", "-O2", "-Os"] {
                let name = format!(
                    "integer_trap_{ty}_{}_{optimization}",
                    if operation == "/" { "div" } else { "rem" }
                );
                let source_file = directory.join(format!("{name}.c"));
                let executable = directory.join(format!("{name}.exe"));
                fs::write(&source_file, &source).unwrap();
                let compilation = c_compiler(&compiler)
                    .arg(optimization)
                    .arg(&source_file)
                    .arg("-o")
                    .arg(&executable)
                    .args(arandu_middle::TargetSpec::for_host().cc_link_args())
                    .output()
                    .unwrap();
                assert!(
                    compilation.status.success(),
                    "{name}: {}",
                    String::from_utf8_lossy(&compilation.stderr)
                );
                let zero = Command::new(&executable).output().unwrap();
                assert!(!zero.status.success(), "{name}: zero divisor must trap");
                if signed {
                    let overflow = Command::new(&executable).arg("minimum").output().unwrap();
                    assert_eq!(
                        overflow.status.success(),
                        operation == "%",
                        "{name}: MIN / -1 traps; MIN % -1 is zero"
                    );
                }
            }
        }
    }
}

#[test]
fn parity_unit_arguments_and_locals_have_no_c_storage() {
    let (amir, tc) = compile_src(
        "func unit(): void {} func consume(a: void, x: int, b: void, y: int): int { return x + y } func main(): int { let value = unit(); return consume(value, 19, unit(), 23) }",
    );
    let emitted = emit_c(&amir, &tc);
    assert!(!emitted.contains("void p") && !emitted.contains("void l"));
    assert_eq!(execute_c("unit_arguments_and_locals", &amir, &tc), 42);
    assert_eq!(execute_cranelift(&amir, &tc), 42);
}

#[test]
fn parity_null_pointer_argument_is_not_confused_with_unit() {
    let (amir, tc) = compile_src(
        "func missing(value: ptr[int]?, marker: int): int { if value == nil { return marker }; return 1 } func main(): int { return missing(nil, 42) }",
    );
    assert_eq!(execute_c("null_pointer_argument", &amir, &tc), 42);
    assert_eq!(execute_cranelift(&amir, &tc), 42);
}

#[test]
fn foreign_unit_parameters_are_erased_in_c_declarations_and_calls() {
    let (amir, tc) = compile_src(
        "extern \"C\" { func abs(unit: void, value: i32): i32 }\nfunc unit(): void {}\nfunc main(): int { return unsafe { abs(unit(), -42) } as int }\n",
    );
    assert_eq!(execute_c("foreign_unit_parameter", &amir, &tc), 42);
}

#[test]
fn parity_generic_array_return_is_concrete_before_codegen() {
    let (amir, tc) = compile_src_mono(
        "func values<comptime N: uint>(): [N]i32 { return [7; N] } func main(): int { let values = values<4>(); return values[3] as int - 7 }",
    );
    for function in &amir.funcs {
        assert!(!matches!(
            tc.type_info.type_interner.resolve(function.return_type),
            arandu_middle::types::ArType::ConstArray(..)
        ));
    }
    assert_eq!(execute_c("generic_array_return", &amir, &tc), 0);
    assert_eq!(execute_cranelift(&amir, &tc), 0);
}

#[test]
fn parity_print_does_not_append_a_newline() {
    let (amir, tc) = compile_src(
        r#"
import io
func main(): int {
    io.print("hello")
    io.print("")
    io.print("\0world")
    io.println("") // Delimit the harness's separate numeric result line.
    return 0
}
"#,
    );
    let (status, output) = execute_c_output("print_without_newline", &amir, &tc);
    assert_eq!(status, 0);
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
    let expected = format!("hello\0world{newline}0{newline}");
    assert_eq!(output.as_bytes(), expected.as_bytes());
}

#[test]
fn parity_byte_to_char_preserves_every_unsigned_codepoint() {
    test_zero_result_all_opt_levels(
        "byte_to_char",
        r#"
func main(): int {
    let mut value: u32 = 0
    while value < 256 {
        let byteValue = value as u8
        let character = byteValue as char
        if character as u32 != value { return 1 }
        value = value + 1
    }
    return 0
}
"#,
    );
}

#[test]
fn parity_scalar_slot_reuse_preserves_long_expression_chains() {
    let mut source = String::from("func main(): int { let mut value: int = 0\n");
    for _ in 0..256 {
        source.push_str("value = ((value + 1) * 2) / 2\n");
    }
    source.push_str("return value }\n");
    let (amir, tc) = compile_src(&source);
    let emitted = emit_c(&amir, &tc);
    let declared = emitted
        .lines()
        .filter(|line| {
            let line = line.trim();
            (line.starts_with("int32_t t") || line.starts_with("int64_t t"))
                && line.ends_with(';')
                && !line.contains('=')
        })
        .count();
    let temps: usize = amir.funcs.iter().map(|func| func.temps.len()).sum();
    assert!(
        declared < temps / 4,
        "declared {declared} locals for {temps} AMIR temps"
    );
    assert_eq!(execute_cranelift(&amir, &tc), 256);
    assert_eq!(execute_c("scalar_slot_reuse_chain", &amir, &tc), 256);
}

#[test]
fn c_runtime_and_private_bodies_follow_live_dependencies() {
    let (amir, tc) = compile_src(
        "func unused_leaf(): int { return 12 } func unused_parent(): int { return unused_leaf() } public func exported(x: int): int { return x + 1 } func used(x: int): int { return x + 2 } func main(): int { return used(40) - 42 }",
    );
    let c = emit_c(&amir, &tc);
    assert!(!c.contains("unused_leaf(") && !c.contains("unused_parent("));
    assert!(c.contains("exported(") && c.contains("used("));
    for absent in [
        "ar_gen_entry",
        "ar_gen_shutdown_raw",
        "ar_rt_slots",
        "ar_co_poll",
        "ar_env_c_argc",
        "ar_fs_read",
        "ar_f64_to_str",
        "pthread.h",
        "dirent.h",
        "windows.h",
    ] {
        assert!(!c.contains(absent), "unexpected runtime group {absent}");
    }
    assert_eq!(execute_c("live_private_bodies", &amir, &tc), 0);
}

#[test]
fn c_retains_empty_tooling_entrypoints_without_source_calls() {
    let (amir, tc) = compile_src(
        "struct Benchmark { handle: int } @Test func empty_case(): void {} @Benchmark func measure(mut bench: Benchmark): void {} func main(): int { return 0 }",
    );
    let c = emit_c(&amir, &tc);
    assert!(c.contains("empty_case("));
    assert!(c.contains("measure("));
    assert_eq!(execute_c("empty_tooling_roots", &amir, &tc), 0);
}

#[test]
fn c_constant_rotations_preserve_unsigned_widths_and_shared_values() {
    let source = r#"
public func rotate32(x: u32): u32 { return (x >> 17) | (x << 15) }
public func rotate64(x: u64): u64 { return (x << 63) | (x >> 1) }
public func signedRotate(x: i32): i32 { return (x >> 17) | (x << 15) }
public func dynamic(x: u32, n: u32): u32 { return (x >> n) | (x << (32 - n)) }
func main(): int {
    if rotate32(0x12345678) != 0x2b3c091a { return 1 }
    if rotate64(0x8000000000000001) != 0xc000000000000000 { return 2 }
    let x: u32 = 0x12345678
    let a = x >> 17
    let b = x << 15
    if (a | b) != 0x2b3c091a || a != 0x91a { return 3 }
    return 0
}
"#;
    let (mut amir, tc) = compile_src(source);
    optimize_amir_checked_with_level(
        &mut amir,
        &tc.symbols,
        &tc.type_info.type_interner,
        OptLevel::O1,
    )
    .unwrap();
    let c = emit_c(&amir, &tc);
    assert!(c.contains("ar_rotr32(") && c.contains("ar_rotr64("));
    test_zero_result_all_opt_levels("constant_rotations", source);
}

fn compile_src(src: &str) -> (AmirProgram, TypeCheckResult) {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.is_empty(),
        "type check failed: {:?}",
        tc.diagnostics
    );

    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let (amir, _) = lower_to_amir_with_interfaces(&mut tc, &hir, 8).expect("AMIR lowering failed");
    (amir, tc)
}

fn execute_cranelift(amir: &AmirProgram, tc: &TypeCheckResult) -> i32 {
    let backend = CraneliftBackend::try_new().unwrap();
    let compiled =
        CodegenBackend::compile(backend, amir, tc.symbols.as_ref(), tc.type_info.as_ref())
            .expect("cranelift compile failed");

    unsafe {
        let main_fn =
            arandu_semantics::CompiledCode::get_fn::<unsafe fn() -> i32>(&compiled, "main")
                .expect("main not found");
        main_fn()
    }
}

#[test]
fn parity_float_literals_round_once_and_preserve_signed_zero_and_subnormals() {
    let source = r#"
func main(): int {
    let negative: f64 = -0.0
    if 1.0 / negative >= 0.0 { return 1 }
    let tiny: f64 = 5e-324
    if tiny + tiny != 1e-323 { return 2 }
    let direct: f32 = 1.000000059604644775390625000000000000000000000000000001
    let next: f32 = 1.00000011920928955078125
    if direct != next { return 3 }
    return 0
}
"#;
    for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
        let (mut amir, tc) = compile_src(source);
        optimize_amir_checked_with_level(
            &mut amir,
            &tc.symbols,
            &tc.type_info.type_interner,
            level,
        )
        .unwrap();
        assert_eq!(execute_cranelift(&amir, &tc), 0, "{level:?}");
        assert_eq!(
            execute_c(&format!("float_ieee_{level:?}"), &amir, &tc),
            0,
            "{level:?}"
        );
    }
}

#[test]
fn c_float_encoding_helpers_preserve_all_ieee_categories() {
    let (mut amir, tc) = compile_src(
        "public func single(): f32 { return 0.0 } public func wide(): f64 { return 0.0 } func main(): int { return 0 }",
    );
    for (name, ty) in [
        ("single", arandu_middle::types::Primitive::F32),
        ("wide", arandu_middle::types::Primitive::F64),
    ] {
        let id = amir.literal_pool.intern_float_bits(
            arandu_middle::ctfe::ConstFloat::new(
                arandu_middle::ctfe::FloatType::new(ty, DataLayout::host()).unwrap(),
                0,
            )
            .unwrap(),
        );
        let function = amir
            .funcs
            .iter_mut()
            .find(|f| tc.symbols.get(f.symbol).name == name)
            .unwrap();
        for statement in function.stmts.payloads.iter_mut() {
            if let AmirStmt::Assign {
                lhs: arandu_middle::amir::TempId(0),
                rhs,
            } = statement
            {
                *rhs = AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Pool(id)));
            }
        }
    }
    let emitted = emit_c(&amir, &tc);
    let source = format!(
        "#define main arandu_unused_main\n{emitted}\n#undef main\n{}",
        r#"
int main(void) {
    const uint32_t singles[] = { 0, UINT32_C(2147483648), 1, UINT32_C(2139095040), UINT32_C(4286578688), UINT32_C(2143294004) };
    const uint64_t doubles[] = { 0, UINT64_C(9223372036854775808), 1, UINT64_C(9218868437227405312), UINT64_C(18442240474082181120), UINT64_C(9221120237041095220) };
    for (size_t i = 0; i < 6; ++i) {
        float single = ar_f32_from_bits(singles[i]);
        double wide = ar_f64_from_bits(doubles[i]);
        uint32_t single_bits;
        uint64_t wide_bits;
        memcpy(&single_bits, &single, sizeof(single_bits));
        memcpy(&wide_bits, &wide, sizeof(wide_bits));
        if (single_bits != singles[i] || wide_bits != doubles[i]) return 1;
    }
    return 0;
}
"#
    );
    let directory = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&directory).unwrap();
    let source_file = directory.join("float_encoding_helpers.c");
    let executable = directory.join("float_encoding_helpers.exe");
    fs::write(&source_file, source).unwrap();
    let compiler = env::var("CC").unwrap_or_else(|_| "gcc".into());
    let compilation = c_compiler(&compiler)
        .arg("-O2")
        .arg(&source_file)
        .arg("-o")
        .arg(&executable)
        .args(arandu_middle::TargetSpec::for_host().cc_link_args())
        .output()
        .unwrap();
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    assert!(Command::new(executable).status().unwrap().success());
}
fn emit_c(amir: &AmirProgram, tc: &TypeCheckResult) -> String {
    // Host parity only; Cranelift is host-only — see solidification matrix.
    arandu_backend_c::emit_c(
        amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        arandu_middle::layout::DataLayout::host(),
    )
    .unwrap()
}

#[test]
fn static_sources_require_known_byte_order_and_support_windows_macros() {
    let (amir, tc) = compile_src(
        "func main(): int { let values: [8]int = [16909060, 2, 3, 4, 5, 6, 7, 8]; return values[0] }",
    );
    let emitted = emit_c(&amir, &tc);
    let start = emitted
        .find("#if defined(__BYTE_ORDER__)")
        .expect("static source");
    let end = emitted[start..].find("#endif").expect("byte order guard") + start;
    let source = format!(
        "typedef unsigned char uint8_t;\n{}\n",
        &emitted[start..end + 6]
    );
    let directory = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&directory).unwrap();
    let source_file = directory.join("static_source_byte_order.c");
    fs::write(&source_file, source).unwrap();
    let compiler = env::var("CC").unwrap_or_else(|_| "gcc".into());
    for (name, macros, prefix) in [
        ("windows", vec!["-D_WIN32=1"], Some("4,3,2,1,")),
        (
            "little",
            vec!["-D__BYTE_ORDER__=1234", "-D__ORDER_LITTLE_ENDIAN__=1234"],
            Some("4,3,2,1,"),
        ),
        (
            "big",
            vec!["-D__BYTE_ORDER__=4321", "-D__ORDER_BIG_ENDIAN__=4321"],
            Some("1,2,3,4,"),
        ),
        ("unknown", vec![], None),
    ] {
        let output = Command::new(&compiler)
            .args([
                "-E",
                "-P",
                "-U_WIN32",
                "-U__BYTE_ORDER__",
                "-U__ORDER_BIG_ENDIAN__",
                "-U__ORDER_LITTLE_ENDIAN__",
            ])
            .args(macros)
            .arg(&source_file)
            .output()
            .unwrap();
        if let Some(prefix) = prefix {
            assert!(
                output.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains(prefix),
                "{name}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        } else {
            assert!(
                !output.status.success(),
                "unknown byte order must fail closed"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("requires a known target byte order")
            );
        }
    }
}

#[test]
fn c_backend_genref_runtime_has_monotonic_type_erased_storage() {
    let (amir, tc) = compile_src(
        "extern \"C\" { func ar_gen_shutdown_raw(): void }\nfunc main(): int { unsafe { ar_gen_shutdown_raw() }; return 0 }",
    );
    let emitted = emit_c(&amir, &tc);

    assert!(emitted.contains("typedef struct ar_gen_entry"));
    assert!(emitted.contains("ar_gen_alloc_aligned"));
    assert!(emitted.contains("ar_gen_next_token == UINT64_MAX"));
    assert!(emitted.contains("ar_gen_shutdown_raw"));
    assert!(!emitted.contains("ar_gen_slots[256]"));
}

#[test]
fn c_backend_emits_opaque_black_box_barrier() {
    let (amir, tc) = compile_src(
        r#"
        module std.testing
        func blackBox<T>(value: T): T { return value }
        func main(): int {
            return blackBox<int>(42)
        }
        "#,
    );
    let emitted = emit_c(&amir, &tc);
    assert!(emitted.contains("AR_BENCH_NOINLINE"));
    assert!(emitted.contains("ar_bench_black_box_i64((int64_t)("));
    test_execution_parity(
        "black_box_barrier",
        r#"
        module std.testing
        func blackBox<T>(value: T): T { return value }
        func main(): int { return blackBox<int>(42) }
        "#,
    );
}

#[test]
fn parity_black_box_preserves_fixed_float_types() {
    test_zero_result_all_opt_levels(
        "fixed_float_black_box",
        "module std.testing\nfunc blackBox<T>(value: T): T { return value } func main(): int { if blackBox<f32>(2.5 as f32) != 2.5 { return 1 }; if blackBox<f64>(3.75 as f64) != 3.75 { return 2 }; return 0 }",
    );
}

#[test]
fn explicit_destructor_runs_through_both_backend_pipelines() {
    test_execution_parity(
        "destructor_epilogue",
        r#"
struct Resource { handle: ptr[u8] }

@Destructor
func Resource.close(own self): void {}

func main(): int {
    let resource = Resource { handle: nil }
    return 0
}

"#,
    );
}

#[test]
fn c_backend_genref_runtime_executes_beyond_legacy_capacity() {
    let (amir, tc) = compile_src(
        "extern \"C\" { func ar_gen_shutdown_raw(): void }\nfunc main(): int { unsafe { ar_gen_shutdown_raw() }; return 0 }",
    );
    let emitted = emit_c(&amir, &tc);
    let source = format!(
        "#define main arandu_unused_main\n{emitted}\n#undef main\n{}",
        r#"
typedef struct __attribute__((aligned(64))) { uint64_t words[8]; } AlignedProbe;
static int probe_drops = 0;
static void drop_probe(void *payload) {
    AlignedProbe *probe = (AlignedProbe *)payload;
    if (((uintptr_t)probe & 63U) != 0) abort();
    ++probe_drops;
    if (probe_drops == 1) ar_gen_shutdown_raw();
}

int main(void) {
    uint64_t handles[1024];
    for (int64_t i = 0; i < 1024; ++i) {
        int64_t value = i + 7;
        handles[i] = ar_gen_upsert_raw(0, &value, sizeof(value), _Alignof(int64_t), NULL);
    }
    for (int64_t i = 0; i < 1024; ++i) {
        int64_t value = 0;
        if (!ar_gen_get_raw(handles[i], &value, sizeof(value), _Alignof(int64_t)) || value != i + 7) return 1;
        value = i + 9;
        if (!ar_gen_set_raw(handles[i], &value, sizeof(value), _Alignof(int64_t), NULL)) return 2;
        value = 0;
        if (!ar_gen_get_raw(handles[i], &value, sizeof(value), _Alignof(int64_t)) || value != i + 9) return 3;
    }
    for (int64_t i = 0; i < 1024; ++i) {
        int64_t value = 0;
        if (!ar_gen_remove_raw(handles[i], &value, sizeof(value), _Alignof(int64_t)) || value != i + 9) return 4;
        if (ar_gen_get_raw(handles[i], &value, sizeof(value), _Alignof(int64_t))) return 5;
    }
    for (int64_t i = 0; i < 1024; ++i) {
        int64_t value = i + 11;
        uint64_t next = ar_gen_insert_raw(&value, sizeof(value), _Alignof(int64_t), NULL);
        if (next == 0 || next <= handles[1023]) return 6;
    }
    ar_gen_shutdown_raw();
    AlignedProbe first = {{1}};
    AlignedProbe second = {{2}};
    if (!ar_gen_insert_raw(&first, sizeof(first), _Alignof(AlignedProbe), drop_probe)) return 7;
    if (!ar_gen_insert_raw(&second, sizeof(second), _Alignof(AlignedProbe), drop_probe)) return 8;
    ar_gen_shutdown_raw();
    if (probe_drops != 2) return 9;
    return 0;
}
"#
    );
    let out_dir = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&out_dir).unwrap();
    let c_file = out_dir.join("genref_dynamic_capacity.c");
    let exe_file = out_dir.join("genref_dynamic_capacity.exe");
    fs::write(&c_file, source).unwrap();
    let cc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let compiled = c_compiler(&cc)
        .arg(&c_file)
        .arg("-o")
        .arg(&exe_file)
        .status()
        .unwrap_or_else(|_| panic!("failed to invoke C compiler '{cc}'"));
    assert!(
        compiled.success(),
        "C GenRef stress fixture did not compile"
    );
    let status = Command::new(&exe_file)
        .status()
        .expect("failed to run C GenRef stress fixture");
    assert!(status.success(), "C GenRef stress fixture failed: {status}");
}

fn assert_backend_rejection_parity(
    amir: &AmirProgram,
    tc: &TypeCheckResult,
    expected_marker: &str,
) {
    let c_error = arandu_backend_c::emit_c(
        amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::host(),
    )
    .unwrap_err();
    let jit_error = CraneliftBackend::try_new()
        .unwrap()
        .compile(amir, tc.symbols.as_ref(), tc.type_info.as_ref())
        .err()
        .expect("Cranelift must reject malformed AMIR before producing a module");

    assert_eq!(c_error.code, arandu_middle::DiagCode::ICEGEN002);
    assert_eq!(jit_error.code, c_error.code);
    assert_eq!(jit_error.message, c_error.message);
    assert!(c_error.message.contains(expected_marker));
}

fn execute_c(name: &str, amir: &AmirProgram, tc: &TypeCheckResult) -> i32 {
    execute_c_output(name, amir, tc).0
}

fn execute_c_output(name: &str, amir: &AmirProgram, tc: &TypeCheckResult) -> (i32, String) {
    // 1. Generate C (no debug dumps — keep tests pure / CI-friendly).
    let mut c_code = emit_c(amir, tc);

    // CEmitter emits `int32_t main(void)`. We rename it to `arandu_main` via a preprocessor
    // macro so we can wrap it in a standard C `main` that captures and prints the return
    // value for parity comparison with the Cranelift result.
    c_code = format!("#define main arandu_main\n{}\n#undef main\n", c_code);
    c_code.push_str(
        r#"
#include <stdio.h>
int main() {
    int32_t res = arandu_main();
    printf("%d\n", res);
    return 0;
}
"#,
    );

    let out_dir = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&out_dir).unwrap();
    let c_file = out_dir.join(format!("{}.c", name));
    let exe_file = out_dir.join(format!("{}.exe", name)); // .exe works on windows

    fs::write(&c_file, c_code).unwrap();

    // Compiler selection: use $CC env var if set, otherwise fallback to gcc.
    let cc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());

    // TargetSpec supplies libm and the host system libraries.
    let compile_status = c_compiler(&cc)
        .arg(&c_file)
        .arg("-o")
        .arg(&exe_file)
        .args(arandu_middle::TargetSpec::for_host().cc_link_args())
        .output()
        .unwrap_or_else(|_| {
            panic!(
                "failed to invoke C compiler '{}'. Parity tests require a C compiler in PATH.",
                cc
            )
        });

    assert!(
        compile_status.status.success(),
        "C compilation failed for {name}: {}",
        String::from_utf8_lossy(&compile_status.stderr)
    );

    let output = Command::new(&exe_file)
        .output()
        .expect("failed to run compiled executable");

    assert!(
        output.status.success(),
        "C program crashed for {name}: status={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    // Last line is the harness exit code (`printf("%d\n", res)`). Earlier lines
    // may be `io.println` output (ToStr product path).
    let stdout = String::from_utf8(output.stdout).unwrap();
    let last_line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let actual_result: i32 = last_line
        .parse()
        .unwrap_or_else(|_| panic!("failed to parse C exit line as integer: {stdout:?}"));

    (actual_result, stdout)
}

fn test_execution_result(name: &str, src: &str) -> (i32, i32) {
    let (amir, tc) = compile_src(src);
    let actual_result = execute_c(name, &amir, &tc);

    // 2. Run via Cranelift
    let expected = execute_cranelift(&amir, &tc);

    assert_eq!(
        expected, actual_result,
        "Execution mismatch for {}! Cranelift={}, C={}",
        name, expected, actual_result
    );
    (expected, actual_result)
}

#[test]
fn public_layout_expressions_fold_once_for_c_and_cranelift() {
    let (amir, typed) = compile_src_mono(
        "func size<T>(): usize { return @sizeOf(T) }\nfunc main(): int { return (@sizeOf([3]u16) + @alignOf(i64) + size<int>()) as int }",
    );
    assert_eq!(execute_c("layout_public", &amir, &typed), 18);
    assert_eq!(execute_cranelift(&amir, &typed), 18);
}

fn generated_integer_fixture() -> (String, i32) {
    const CASES: i64 = 64;
    let mut source = String::new();
    let mut expected = 0i64;
    let mut state = 0x6a09_e667_f3bc_c909u64;

    for index in 0..CASES {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let a = i64::from(state.to_le_bytes()[1] % 31) - 15;
        let b = i64::from(state.to_le_bytes()[3] % 19) - 9;
        let c = i64::from(state.to_le_bytes()[5] % 23) - 11;
        let divisor = i64::from(state.to_le_bytes()[7] % 7) + 1;
        let value = ((a * b) + c) / divisor;
        let threshold = i64::from(state.to_le_bytes()[0] % 11) - 5;
        let selected = if value >= threshold {
            value + index
        } else {
            threshold - value
        };
        expected += selected;
        source.push_str(&format!(
            "func generated{index}(): int {{\n\
             let a: int = {a}\n\
             let b: int = {b}\n\
             let c: int = {c}\n\
             let value: int = ((a * b) + c) / {divisor}\n\
             if value >= {threshold} {{ return value + {index} }}\n\
             return {threshold} - value\n\
             }}\n"
        ));
    }
    source.push_str("func main(): int {\n    return ");
    for index in 0..CASES {
        if index != 0 {
            source.push_str(" + ");
        }
        source.push_str(&format!("generated{index}()"));
    }
    source.push_str("\n}\n");

    (
        source,
        i32::try_from(expected).expect("bounded oracle result"),
    )
}

#[derive(Clone, Copy, Debug)]
enum IntegerOp {
    Add,
    Subtract,
    Multiply,
    Xor,
}

struct IntegerProgram<const N: usize> {
    operations: [IntegerOp; N],
    operands: [i64; N],
    initial: i64,
    threshold: i64,
    branch_delta: i64,
    loop_delta: i64,
    loop_count: i64,
}

impl<const N: usize> IntegerProgram<N> {
    fn from_seed(seed: u64) -> Self {
        let mut state = seed;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            state.to_le_bytes()[4]
        };
        let operations = std::array::from_fn(|_| match next() % 4 {
            0 => IntegerOp::Add,
            1 => IntegerOp::Subtract,
            2 => IntegerOp::Multiply,
            _ => IntegerOp::Xor,
        });
        let operands = std::array::from_fn(|index| match operations[index] {
            IntegerOp::Multiply => i64::from(next() % 5) - 2,
            IntegerOp::Xor => i64::from(next() % 32),
            IntegerOp::Add | IntegerOp::Subtract => i64::from(next() % 17) - 8,
        });
        Self {
            operations,
            operands,
            initial: i64::from(next() % 41) - 20,
            threshold: i64::from(next() % 31) - 15,
            branch_delta: i64::from(next() % 13) - 6,
            loop_delta: i64::from(next() % 7) - 3,
            loop_count: i64::from(next() % 4),
        }
    }

    fn evaluate(&self) -> i32 {
        let mut value = self.initial;
        for (&operation, &operand) in self.operations.iter().zip(&self.operands) {
            value = match operation {
                IntegerOp::Add => value + operand,
                IntegerOp::Subtract => value - operand,
                IntegerOp::Multiply => value * operand,
                IntegerOp::Xor => value ^ operand,
            };
        }
        if value >= self.threshold {
            value += self.branch_delta;
        } else {
            value -= self.branch_delta;
        }
        value += self.loop_delta * self.loop_count;
        i32::try_from(value).expect("bounded structural integer oracle")
    }

    fn emit_source(&self) -> String {
        let mut source = String::with_capacity(768);
        self.emit_function(&mut source, "main");
        source
    }

    fn emit_function(&self, source: &mut String, name: &str) {
        writeln!(source, "func {name}(): int {{").unwrap();
        writeln!(source, "    let mut value: int = {}", self.initial).unwrap();
        for (&operation, &operand) in self.operations.iter().zip(&self.operands) {
            let operator = match operation {
                IntegerOp::Add => '+',
                IntegerOp::Subtract => '-',
                IntegerOp::Multiply => '*',
                IntegerOp::Xor => '^',
            };
            writeln!(source, "    set value = value {operator} ({operand})").unwrap();
        }
        writeln!(source, "    if value >= {} {{", self.threshold).unwrap();
        writeln!(
            source,
            "        set value = value + ({})",
            self.branch_delta
        )
        .unwrap();
        writeln!(source, "    }} else {{").unwrap();
        writeln!(
            source,
            "        set value = value - ({})",
            self.branch_delta
        )
        .unwrap();
        writeln!(source, "    }}").unwrap();
        writeln!(source, "    let mut index: int = 0").unwrap();
        writeln!(source, "    while index < {} {{", self.loop_count).unwrap();
        writeln!(source, "        set value = value + ({})", self.loop_delta).unwrap();
        writeln!(source, "        set index = index + 1").unwrap();
        writeln!(source, "    }}").unwrap();
        writeln!(source, "    return value").unwrap();
        writeln!(source, "}}").unwrap();
    }
}

const STRUCTURAL_INTEGER_SEEDS: [u64; 8] = [
    0,
    1,
    0x243f_6a88_85a3_08d3,
    0x1319_8a2e_0370_7344,
    0xa409_3822_299f_31d0,
    0x082e_fa98_ec4e_6c89,
    u64::MAX - 1,
    u64::MAX,
];

fn structural_integer_suite() -> String {
    let mut source = String::with_capacity(STRUCTURAL_INTEGER_SEEDS.len() * 768);
    let mut expected = [0i32; STRUCTURAL_INTEGER_SEEDS.len()];
    for (index, seed) in STRUCTURAL_INTEGER_SEEDS.into_iter().enumerate() {
        let program = IntegerProgram::<12>::from_seed(seed);
        program.emit_function(&mut source, &format!("generated{index}"));
        expected[index] = program.evaluate();
    }
    writeln!(source, "func main(): int {{").unwrap();
    for (index, expected) in expected.into_iter().enumerate() {
        writeln!(
            source,
            "    if generated{index}() != {expected} {{ return {} }}",
            index + 1
        )
        .unwrap();
    }
    writeln!(source, "    return 0").unwrap();
    writeln!(source, "}}").unwrap();
    source
}

fn test_execution_parity(name: &str, src: &str) {
    let _ = test_execution_result(name, src);
}

fn test_zero_result_all_opt_levels(name: &str, source: &str) {
    for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::Os] {
        let (mut amir, tc) = compile_src(source);
        optimize_amir_checked_with_level(
            &mut amir,
            &tc.symbols,
            &tc.type_info.type_interner,
            level,
        )
        .expect("valid aggregate fixture must optimize");
        assert_eq!(execute_cranelift(&amir, &tc), 0, "{name}: {level:?}");
        assert_eq!(
            execute_c(&format!("{name}_{level:?}"), &amir, &tc),
            0,
            "{name}: {level:?}"
        );
    }
}

#[test]
fn size_optimization_retains_helper_calls_and_matches_speed_execution() {
    let source =
        "func add(a: int, b: int): int { return a + b } func main(): int { return add(19, 23) }";
    for level in [OptLevel::O1, OptLevel::Os] {
        let (mut amir, tc) = compile_src(source);
        optimize_amir_checked_with_level(
            &mut amir,
            &tc.symbols,
            &tc.type_info.type_interner,
            level,
        )
        .unwrap();
        let main = amir
            .funcs
            .iter()
            .find(|func| tc.symbols.host_func_name(tc.symbols.get(func.symbol)) == "main")
            .unwrap();
        let has_call = main
            .blocks
            .iter()
            .flat_map(|block| main.block_stmts(block.id))
            .any(|stmt| matches!(stmt, AmirStmt::Call { .. }));
        assert_eq!(has_call, level == OptLevel::Os);
        assert_eq!(execute_cranelift(&amir, &tc), 42);
        assert_eq!(execute_c(&format!("size_call_{level:?}"), &amir, &tc), 42);
    }
}

fn test_execution_parity_mono(name: &str, src: &str) {
    let (amir, tc) = compile_src_mono(src);
    let actual_result = execute_c(name, &amir, &tc);
    let expected = execute_cranelift(&amir, &tc);
    assert_eq!(
        expected, actual_result,
        "Execution mismatch for {}! Cranelift={}, C={}",
        name, expected, actual_result
    );
}

#[test]
fn generated_test_registry_entrypoint_compiles_and_executes() {
    let (amir, tc) = compile_src("func smoke(): void {}");
    let mut source = emit_c(&amir, &tc);
    let mut registry = arandu_codegen::testing::TestRegistry::default();
    registry.insert(arandu_codegen::testing::TestEntry {
        id: "sample::test::smoke::smoke".into(),
        function: "smoke".into(),
    });
    source.push_str(&registry.emit_c_entrypoint());

    let out_dir = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&out_dir).unwrap();
    let c_file = out_dir.join("generated_test_harness.c");
    let exe_file = out_dir.join("generated_test_harness.exe");
    fs::write(&c_file, source).unwrap();

    let cc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let compiled = c_compiler(&cc)
        .arg(&c_file)
        .arg("-o")
        .arg(&exe_file)
        .args(arandu_middle::TargetSpec::for_host().cc_link_args())
        .status()
        .unwrap_or_else(|_| panic!("failed to invoke C compiler '{cc}'"));
    assert!(
        compiled.success(),
        "generated C test harness did not compile"
    );
    let status = Command::new(&exe_file)
        .status()
        .expect("failed to execute generated C test harness");
    assert!(
        status.success(),
        "generated C test harness failed: {status}"
    );
}

#[test]
fn c_emission_is_byte_deterministic() {
    let src = r#"
        struct Pair { left: int; right: int }
        func main(): int {
            let pair = Pair { left: 20, right: 22 }
            return pair.left + pair.right
        }
    "#;
    let (amir, tc) = compile_src(src);

    let first = emit_c(&amir, &tc);
    let second = emit_c(&amir, &tc);
    let (fresh_amir, fresh_tc) = compile_src(src);
    let fresh = emit_c(&fresh_amir, &fresh_tc);

    assert_eq!(first.as_bytes(), second.as_bytes());
    assert_eq!(first.as_bytes(), fresh.as_bytes());
}

#[test]
fn c_backend_rejects_residual_null_coalesce_without_partial_success() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    let assign = amir
        .funcs
        .iter_mut()
        .flat_map(|func| func.stmts.payloads.raw.iter_mut())
        .find_map(|stmt| match stmt {
            AmirStmt::Assign { rhs, .. } => Some(rhs),
            _ => None,
        })
        .expect("fixture must lower at least one assignment");
    *assign = AmirRvalue::Binary {
        op: BinaryOp::NullCoalesce,
        left: AmirOperand::Constant(AmirConstant::Bool(true)),
        right: AmirOperand::Constant(AmirConstant::Bool(false)),
    };

    let error = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::host(),
    )
    .unwrap_err();
    assert_eq!(error.code, arandu_middle::DiagCode::ICEGEN001);
}

#[test]
fn c_backend_rejects_unsupported_len_without_partial_success() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    let assign = amir
        .funcs
        .iter_mut()
        .flat_map(|func| func.stmts.payloads.raw.iter_mut())
        .find_map(|stmt| match stmt {
            AmirStmt::Assign { rhs, .. } => Some(rhs),
            _ => None,
        })
        .expect("fixture must lower at least one assignment");
    *assign = AmirRvalue::Len(AmirOperand::Constant(AmirConstant::Bool(true)));

    let error = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::host(),
    )
    .unwrap_err();
    assert_eq!(error.code, arandu_middle::DiagCode::ICEGEN001);
    assert!(error.message.contains("Len"));
}

#[test]
fn c_backend_rejects_out_of_range_field_access_with_ice() {
    let src = "struct Pair { left: int; right: int }\nfunc main(): int { let pair = Pair { left: 20, right: 22 }; return pair.left }";
    let (mut amir, tc) = compile_src(src);
    let field = amir
        .funcs
        .iter_mut()
        .flat_map(|func| func.stmts.payloads.raw.iter_mut())
        .find_map(|stmt| match stmt {
            AmirStmt::Assign {
                rhs: AmirRvalue::FieldAccess { field, .. },
                ..
            } => Some(field),
            _ => None,
        })
        .expect("fixture must lower a field access rvalue");
    *field = usize::MAX;

    let error = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::host(),
    )
    .unwrap_err();
    assert_eq!(error.code, arandu_middle::DiagCode::ICEGEN001);
    assert!(error.message.contains("FieldAccess index"));
}

#[test]
fn c_backend_rejects_unknown_struct_literal_field_with_ice() {
    let src = "struct Pair { left: int; right: int }\nfunc main(): int { let pair = Pair { left: 20, right: 22 }; return pair.left }";
    let (mut amir, tc) = compile_src(src);
    let field_name = amir
        .funcs
        .iter_mut()
        .flat_map(|func| func.stmts.payloads.raw.iter_mut())
        .find_map(|stmt| match stmt {
            AmirStmt::Assign {
                rhs: AmirRvalue::StructLiteral { fields, .. },
                ..
            } => fields.first_mut().map(|(name, _)| name),
            _ => None,
        })
        .expect("fixture must lower a struct literal");
    *field_name = "missing".into();

    let error = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::host(),
    )
    .unwrap_err();
    assert_eq!(error.code, arandu_middle::DiagCode::ICEGEN001);
    assert!(error.message.contains("unknown field `missing`"));
}

#[test]
fn c_backend_rejects_unknown_place_field_symbol_with_ice() {
    let src = "struct Pair { left: int; right: int }\nfunc main(): int { let mut pair = Pair { left: 20, right: 22 }; pair.left = 1; return pair.left }";
    let (mut amir, tc) = compile_src(src);
    let field = amir
        .funcs
        .iter_mut()
        .flat_map(|func| func.stmts.payloads.raw.iter_mut())
        .find_map(|stmt| match stmt {
            AmirStmt::Store { lhs, .. } => {
                lhs.projections
                    .iter_mut()
                    .find_map(|projection| match projection {
                        arandu_middle::amir::AmirProjection::Field(field) => Some(field),
                        _ => None,
                    })
            }
            _ => None,
        })
        .expect("fixture must lower a projected field store");
    *field = arandu_middle::SymbolId::DUMMY;

    let error = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::host(),
    )
    .unwrap_err();
    assert_eq!(error.code, arandu_middle::DiagCode::ICEGEN001);
    assert!(error.message.contains("unknown field symbol"));
}

#[test]
fn both_backends_reject_the_same_invalid_ssa_edge() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    amir.funcs[0].blocks[0].terminator = arandu_middle::amir::AmirTerminator::Goto {
        target: arandu_middle::amir::BlockId::from_usize(0),
        args: vec![AmirOperand::Copy(arandu_middle::amir::TempId::from_usize(
            0,
        ))],
    };

    assert_backend_rejection_parity(&amir, &tc, "SSA-EDGE");
}

#[test]
fn both_backends_reject_the_same_poison_type() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    amir.funcs[0].temps[0].ty = tc.type_info.type_interner.error_type_id();

    assert_backend_rejection_parity(&amir, &tc, "TYP-1");
}

#[test]
fn both_backends_reject_the_same_out_of_bounds_statement_range() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    let invalid_len = amir.funcs[0].stmts.len() + 1;
    amir.funcs[0].blocks[0].statements = arandu_middle::layout::DenseRange::new(0, invalid_len);

    assert_backend_rejection_parity(&amir, &tc, "IR-RANGE");
}

#[test]
fn parity_index_addressing_combined_with_shift() {
    test_execution_parity(
        "index_shift_addressing",
        r#"
        func main(): int {
            let values: [4]int = [3, 5, 7, 11]
            let index: int = 1 << 1
            return values[index] + (1024 >> 5)
        }
        "#,
    );
}

#[test]
fn generated_integer_programs_match_independent_oracle() {
    // Keep the original left-associated 64-call expression and exercise all
    // compiler phases with a bounded stack on every host, including macOS.
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let (source, expected) = generated_integer_fixture();
            let (jit, c) = test_execution_result("generated_integer_oracle", &source);
            assert_eq!(
                jit, expected,
                "Cranelift disagreed with the independent oracle"
            );
            assert_eq!(c, expected, "C disagreed with the independent oracle");
        })
        .expect("spawn bounded compiler worker")
        .join()
        .expect("bounded compiler worker failed");
}

#[test]
fn optimization_levels_preserve_the_generated_integer_oracle() {
    let (source, expected) = generated_integer_fixture();

    for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::Os] {
        let (mut amir, tc) = compile_src(&source);
        optimize_amir_checked_with_level(
            &mut amir,
            tc.symbols.as_ref(),
            &tc.type_info.type_interner,
            level,
        )
        .unwrap_or_else(|error| panic!("{level:?} optimization rejected valid AMIR: {error:?}"));

        let jit = execute_cranelift(&amir, &tc);
        let c = execute_c(&format!("generated_integer_{level:?}"), &amir, &tc);
        assert_eq!(jit, expected, "{level:?} Cranelift result changed");
        assert_eq!(c, expected, "{level:?} C result changed");
    }
}

#[test]
fn structural_integer_programs_agree_across_optimization_levels() {
    for seed in STRUCTURAL_INTEGER_SEEDS {
        let program = IntegerProgram::<12>::from_seed(seed);
        let source = program.emit_source();
        let expected = program.evaluate();

        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::Os] {
            let (mut amir, tc) = compile_src(&source);
            optimize_amir_checked_with_level(
                &mut amir,
                tc.symbols.as_ref(),
                &tc.type_info.type_interner,
                level,
            )
            .unwrap_or_else(|error| {
                panic!("seed {seed:#018x} {level:?} rejected valid AMIR: {error:?}\n{source}")
            });
            let actual = execute_cranelift(&amir, &tc);
            assert_eq!(
                actual, expected,
                "seed {seed:#018x} changed under {level:?}\n{source}"
            );
        }
    }
}

#[test]
fn structural_integer_suite_agrees_between_backends_and_opt_levels() {
    let source = structural_integer_suite();

    for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::Os] {
        let (mut amir, tc) = compile_src(&source);
        optimize_amir_checked_with_level(
            &mut amir,
            tc.symbols.as_ref(),
            &tc.type_info.type_interner,
            level,
        )
        .unwrap_or_else(|error| panic!("{level:?} rejected structural suite: {error:?}"));

        let jit = execute_cranelift(&amir, &tc);
        let c = execute_c(&format!("structural_integer_{level:?}"), &amir, &tc);
        assert_eq!(jit, 0, "{level:?} Cranelift failed generated case {jit}");
        assert_eq!(c, 0, "{level:?} C failed generated case {c}");
    }
}

#[test]
fn parity_fibonacci() {
    let src = r#"
    func fib(n: int): int {
        if n <= 1 {
            return n
        }
        return fib(n - 1) + fib(n - 2)
    }
    
    func main(): int {
        return fib(10)
    }
    "#;
    test_execution_parity("fibonacci", src);
}

#[test]
fn contextual_tagged_array_payloads_keep_the_declared_element_width() {
    let source = r#"
func some(): Option<[64]u8> { return Option.Some([42; 64]) }
func ok(): Result<[64]u8, int> { return Result.Ok([41; 64]) }
func error(): Result<int, [64]u8> { return Result.Err([40; 64]) }
func main(): int {
    match some() { Some(a) => { if a[63] != 42 { return 1 } } None => { return 2 } }
    match ok() { Ok(a) => { if a[63] != 41 { return 3 } } Err(e) => { return 4 } }
    match error() { Ok(a) => { return 5 } Err(e) => { if e[63] != 40 { return 6 } } }
    return 0
}
"#;
    let (native, c) = test_execution_result("contextual_tagged_arrays", source);
    assert_eq!((native, c), (0, 0));
}

#[test]
fn unit_result_nil_has_materialized_tagged_backing() {
    test_zero_result_all_opt_levels(
        "unit_result_nil",
        r#"
func success(): Result<void, Err> { return nil }
func forward(): Result<void, Err> { return success() }
func main(): int {
    match forward() { Ok(_) => { return 0 } Err(e) => { return 1 } }
}
"#,
    );
}

#[test]
fn parity_struct_layout() {
    let src = r#"
    struct Point {
        x: int
        y: byte
        z: int
    }
    
    func main(): int {
        let p = Point { x: 10, y: 5 as byte, z: 20 }
        return p.z
    }
    "#;
    test_execution_parity("struct_layout", src);
}

#[test]
fn parity_str_literal() {
    let src = r#"
    func get_len(s: str): int {
        return 42 // fixed value; this test verifies str structs can be passed without crashing
    }
    func main(): int {
        return get_len("hello")
    }
    "#;
    test_execution_parity("str_literal", src);
}

#[test]
fn parity_string_interpolation() {
    // Builds an interpolated string and only checks that the program runs
    // end-to-end on both backends (C + Cranelift) without crash.
    let src = r#"
    func main(): int {
        let name = "Bruno"
        let msg = "Oi, ${name}"
        return 0
    }
    "#;
    test_execution_parity("string_interpolation", src);
}

#[test]
fn constant_interpolation_needs_no_runtime_conversion_or_owned_string_drop() {
    let source = r#"
func main(): int {
    if "é\0:${-42}:${0xff}:${true}:${'λ'}" != "é\0:-42:255:true:λ" { return 1 }
    if "before:${"n=${42}"}:after" != "before:n=42:after" { return 2 }
    if "${/** } ignored */ 42 /* { ignored */}:${/// } ignored
42 //! { ignored
}" != "42:42" { return 3 }
    return 0
}
"#;
    let (amir, tc) = compile_src(source);
    for function in &amir.funcs {
        for statement in function.stmts.payloads.iter() {
            assert!(!matches!(
                statement,
                AmirStmt::Free(_)
                    | AmirStmt::Assign {
                        rhs: AmirRvalue::ToStr { .. } | AmirRvalue::StringInterp { .. },
                        ..
                    }
            ));
        }
    }
    let c = emit_c(&amir, &tc);
    assert!(!c.contains("ar_i64_to_str(") && !c.contains("ar_bool_to_str("));
    test_zero_result_all_opt_levels("literal_interpolation", source);
}

#[test]
fn interpolation_with_calls_preserves_evaluation_order_and_effects() {
    let source = r#"
struct Counter { public value: int }
func next(counter: mut ref Counter): int {
    counter.value += 1
    return counter.value
}
func main(): int {
    let mut counter = Counter { value: 0 }
    let text = "${next(mut ref counter)}:${next(mut ref counter)}"
    if text != "1:2" || counter.value != 2 { return 1 }
    return 0
}
"#;
    test_zero_result_all_opt_levels("interpolation_effects", source);
}

#[test]
fn parity_enum_layout() {
    let src = r#"
    enum Status {
        Ok(int)
        Err(byte)
    }
    
    func main(): int {
        let r: Status = Status.Ok(42)
        let mut out: int = 0
        match r {
            Status.Ok(v) => { out = v; }
            Status.Err(_) => { out = -1; }
        }
        return out
    }
    "#;
    test_execution_parity("enum_layout", src);
}

#[test]
fn parity_generic_enum_payload_layout_and_match() {
    let src = r#"
    enum Option<T> { Some(T), None }

    func value(option: Option<int>): int {
        match option {
            Option.Some(item) => { return item; }
            Option.None => { return 0; }
        }
    }

    func main(): int {
        return value(Option.Some(42)) - 42
    }
    "#;
    test_execution_parity("generic_enum_payload_layout", src);
}

#[test]
fn parity_option_niche_ref() {
    let src = r#"
    func check_opt(opt: Option<ref int>): int {
        match opt {
            Some(r) => { return *r; }
            None => { return -1; }
        }
    }

    func main(): int {
        let x: int = 42
        let some_val: Option<ref int> = Option.Some(ref x)
        let none_val: Option<ref int> = nil
        let a = check_opt(some_val)
        let b = check_opt(none_val)
        if a != 42 { return 1 }
        if b != -1 { return 2 }
        return 0
    }
    "#;
    test_execution_parity("option_niche_ref", src);
}

#[test]
fn parity_ssa_pattern_bind() {
    let src = r#"
    enum Wrapper {
        Val(int)
    }
    
    func main(): int {
        let w: Wrapper = Wrapper.Val(123)
        let mut res: int = 0
        if w is Wrapper.Val(x) {
            res = x
        }
        return res
    }
    "#;
    test_execution_parity("ssa_pattern_bind", src);
}

#[test]
fn parity_ssa_pattern_bind_multi_arms() {
    let src = r#"
    enum Wrapper {
        Val(int)
        Other(int)
    }
    
    func main(): int {
        let w: Wrapper = Wrapper.Other(42)
        let mut res: int = 0
        match w {
            Wrapper.Val(x) => {
                res = x
            }
            Wrapper.Other(y) => {
                res = y
            }
        }
        return res
    }
    "#;
    test_execution_parity("ssa_pattern_bind_multi_arms", src);
}

#[test]
fn parity_structural_option_result_equality() {
    test_zero_result_all_opt_levels(
        "structural_option_result_equality",
        r#"
func main(): int {
    let a: Option<int> = Option.Some(7)
    let b: Option<int> = Option.Some(7)
    let c: Option<int> = Option.Some(8)
    let n: Option<int> = Option.None
    let m: Option<int> = Option.None
    if a != b || a == c || a == n || n != m { return 1 }
    let x: Option<Option<int>> = Option.Some(a)
    let y: Option<Option<int>> = Option.Some(b)
    let z: Option<Option<int>> = Option.Some(c)
    if x != y || x == z { return 2 }
    let ok: Result<int, int> = Result.Ok(42)
    let same: Result<int, int> = Result.Ok(42)
    let bad: Result<int, int> = Result.Err(42)
    if ok != same || ok == bad { return 3 }
    let text: Option<str> = Option.Some("hello")
    let other: Option<str> = Option.Some("hello")
    if text != other { return 4 }
    return 0
}
"#,
    );
}

#[test]
fn parity_enum_equality_uses_values_not_padding_or_float_bits() {
    test_zero_result_all_opt_levels(
        "enum_equality_values",
        r#"
struct Padded { small: u8, wide: i64 }
func main(): int {
    let a: Option<[3]int> = Option.Some([1, 2, 3])
    let b: Option<[3]int> = Option.Some([1, 2, 3])
    let c: Option<[3]int> = Option.Some([1, 2, 4])
    if a != b || a == c { return 1 }
    let x: Option<Padded> = Option.Some(Padded { small: 1, wide: 42 })
    let y: Option<Padded> = Option.Some(Padded { small: 1, wide: 42 })
    if x != y { return 2 }
    let plus: Option<f64> = Option.Some(0.0)
    let minus: Option<f64> = Option.Some(-0.0)
    if plus != minus { return 3 }
    let nan: f64 = 0.0 / 0.0
    let wrapped: Option<f64> = Option.Some(nan)
    if wrapped == wrapped { return 4 }
    return 0
}
"#,
    );
}

#[test]
fn parity_array_index_access() {
    let src = r#"
    func dummy(xs: [3]int) {}

    func main(): int {
        let mut xs = [10, 20, 30]
        let idx = 1
        xs[idx] = 42
        dummy(xs)
        return 42
    }
    "#;
    test_execution_parity("array_index_access", src);
}

#[test]
fn parity_array_register_abi_covers_calls_parameters_and_returns() {
    test_zero_result_all_opt_levels(
        "array_register_abi",
        r#"
func integers(x: [2]i64): [2]i64 { return [x[1], x[0]] }
func floats(x: [2]f64): [2]f64 { return [x[1], x[0]] }
func odd(x: [3]u8): [3]u8 { return [x[2], x[1], x[0]] }
func nested(x: [2][2]u8): [2][2]u8 { return [x[1], x[0]] }
func tail(x: [11]u8): [11]u8 { return [x[10], x[9], x[8], x[7], x[6], x[5], x[4], x[3], x[2], x[1], x[0]] }
struct Tiny { a: u8, b: u8, c: u8 }
func tiny(x: Tiny): Tiny { return Tiny { a: x.c, b: x.b, c: x.a } }
func main(): int {
    let ints: [2]i64 = [17, 42]
    let ir = integers(ints)
    if ir[0] != 42 || ir[1] != 17 { return 1 }
    let fs: [2]f64 = [1.5, 2.5]
    let fr = floats(fs)
    if fr[0] != 2.5 || fr[1] != 1.5 { return 2 }
    let os: [3]u8 = [128, 192, 255]
    let or = odd(os)
    if or[0] != 255 || or[2] != 128 { return 3 }
    let ns: [2][2]u8 = [[1, 2], [3, 4]]
    let nr = nested(ns)
    if nr[0][0] != 3 || nr[1][1] != 2 { return 4 }
    let ts: [11]u8 = [128, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    let tr = tail(ts)
    if tr[0] != 10 || tr[8] != 2 || tr[10] != 128 { return 5 }
    let small = tiny(Tiny { a: 1, b: 2, c: 3 })
    if small.a != 3 || small.b != 2 || small.c != 1 { return 6 }
    return 0
}
"#,
    );
}

#[test]
fn parity_large_aggregate_results_are_owned_by_each_caller() {
    test_zero_result_all_opt_levels(
        "large_caller_owned_result",
        r#"
struct Packet { bytes: [24]u8, sum: i64 }
func reverse(x: [24]u8): [24]u8 {
    let mut result: [24]u8 = [0; 24]
    let mut i: usize = 0
    while i < 24 { result[i] = x[23 - i]; i += 1 }
    return result
}
func recurse(depth: int, x: [24]u8): [24]u8 {
    if depth == 0 { return x }
    return reverse(recurse(depth - 1, x))
}
func packet(value: u8): Packet {
    return Packet { bytes: [value; 24], sum: 42 }
}
func main(): int {
    let input: [24]u8 = [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24]
    let first = recurse(3, input)
    let second = recurse(2, input)
    if first[0] != 24 || first[23] != 1 { return 1 }
    if second[0] != 1 || second[23] != 24 { return 2 }
    let p = packet(255)
    let q = packet(128)
    if p.bytes[23] != 255 || q.bytes[23] != 128 || p.sum != 42 { return 3 }
    let mut i: int = 0
    while i < 20 {
        let next = reverse(input)
        if next[0] != 24 || first[23] != 1 { return 4 }
        i += 1
    }
    return 0
}
"#,
    );
}

#[test]
fn parity_enum_multi_variant_switch() {
    let src = r#"
    enum Color {
        Red
        Green
        Blue
        Yellow(int)
    }
    
    func main(): int {
        let c: Color = Color.Yellow(100)
        let mut out: int = 0
        match c {
            Color.Red => { out = 1; }
            Color.Green => { out = 2; }
            Color.Blue => { out = 3; }
            Color.Yellow(v) => { out = v; }
        }
        return out
    }
    "#;
    test_execution_parity("enum_multi_variant_switch", src);
}

#[test]
fn parity_array_reassignment() {
    let src = r#"
    func main(): int {
        let mut arr = [10, 20, 30]
        arr = [99, 98, 97]
        return arr[1]
    }
    "#;
    test_execution_parity("array_reassignment", src);
}

#[test]
fn parity_control_flow_diamond() {
    let src = r#"
    func main(): int {
        let x = 10
        let mut out = 0
        if x > 5 {
            out = 1
        } else {
            out = 2
        }
        return out
    }
    "#;
    test_execution_parity("control_flow_diamond", src);
}

#[test]
fn parity_to_str_int_interp() {
    // ToStr v0.1: int formatted into string interp; both backends exit 0.
    let src = r#"
    func main(): int {
        let n: int = 42
        let s = "n=${n}"
        let t = "b=${true}"
        return 0
    }
    "#;
    test_execution_parity("to_str_int_interp", src);
}

#[test]
fn reassigned_owned_string_never_frees_static_storage() {
    let src = r#"
    func main(): int {
        let mut text = "owned=${1}"
        text = "static"
        return 0
    }
    "#;
    test_execution_parity("reassigned_owned_string", src);
}

#[test]
fn parity_io_println_to_str() {
    // Exercise the official `io.println` lowering. This parity harness compares
    // process status; stdout behavior has its own runtime contract tests.
    let src = r#"
    import io
    func main(): int {
        io.println(42)
        io.println("n=${7}")
        return 0
    }
    "#;
    test_execution_parity("io_println_to_str", src);
}

#[test]
fn parity_io_eprint_emits_the_runtime_alias() {
    let src = r#"
    import io
    func print_err(msg: str): void {
        io.eprint(msg)
    }
    func main(): int {
        print_err("stderr")
        return 0
    }
    "#;
    let (amir, tc) = compile_src(src);
    let emitted = emit_c(&amir, &tc);

    assert!(emitted.contains("static void io__eprint(ArStr s)"));
    assert!(emitted.contains("static void eprint(ArStr s) { io__eprint(s); }"));
    test_execution_parity("io_eprint_runtime_alias", src);
}

#[test]
fn parity_to_str_method_and_float() {
    let src = r#"
    import io
    func main(): int {
        let n: int = 10
        let f: float = 2.0
        io.println(n.to_str())
        io.println(f.to_str())
        return 0
    }
    "#;
    test_execution_parity("to_str_method_float", src);
}

#[test]
fn c_emit_to_str_helpers_present() {
    let src = r#"
    func main(): int {
        let n: int = 7
        let s = "x=${n}"
        return 0
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = emit_c(&amir, &tc);
    assert!(
        c.contains("ar_i64_to_str"),
        "expected ToStr helper in emit, got:\n{c}"
    );
    assert!(
        c.contains("to_str") || c.contains("ar_i64_to_str("),
        "expected ToStr call site"
    );
}

#[test]
fn c_emit_arstr_is_fat_pointer() {
    // S-C-AUDIT: ArStr matches LayoutEngine fat pointer (host 64 → int64_t len).
    let src = r#"
    func main(): int {
        let s = "hi"
        return 0
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = emit_c(&amir, &tc);
    assert!(
        c.contains("typedef struct { const uint8_t *ptr; int64_t len; } ArStr;"),
        "expected ArStr fat-pointer typedef, got headers:\n{}",
        c.lines().take(40).collect::<Vec<_>>().join("\n")
    );
    assert!(c.contains("AR_STR_"), "expected named string constants");
}

#[test]
fn c_emit_arstr_layout_32bit() {
    // S-C-32BIT: emit-only with W=4 (no Cranelift). ArStr.len is int32_t.
    let src = r#"
    extern "C" { func ar_vec_malloc(size: usize): ptr[u8] }
    func main(): int {
        let s = "hi"
        let buffer = unsafe { ar_vec_malloc(1) }
        return 0
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::ptr_width(4),
    )
    .unwrap();
    assert!(
        c.contains("typedef struct { const uint8_t *ptr; int32_t len; } ArStr;"),
        "expected 32-bit ArStr, headers:\n{}",
        c.lines().take(40).collect::<Vec<_>>().join("\n")
    );
    assert!(c.contains("static void *ar_vec_malloc(uint32_t size)"));
    assert!(c.contains("#define AR_VEC_ALIGN 16"));
    assert!(c.contains("((void **)aligned)[-1] = raw"));
    assert!(c.contains(
        "typedef struct { uint8_t *data; uint32_t len; uint32_t capacity; } ArOwnedStringRuntime;"
    ));
    assert!(!c.contains("ar_str_len"));
}

#[test]
fn c_emit_arstr_i686_sysv() {
    // DataLayout::i686_sysv: pointer 4; i64/f64 abi_align 4 — ArStr still {ptr, int32_t len}.
    let src = r#"
    func main(): int {
        let s = "hi"
        return 0
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::i686_sysv(),
    )
    .unwrap();
    assert!(
        c.contains("typedef struct { const uint8_t *ptr; int32_t len; } ArStr;"),
        "i686 ArStr: {}",
        c.lines().take(30).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn c_emit_extern_declaration_present() {
    let src = r#"
    extern "C" {
        func my_custom_extern_func(x: int): int
    }
    func main(): int {
        unsafe {
            return my_custom_extern_func(42)
        }
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = emit_c(&amir, &tc);
    assert!(
        c.contains("int32_t my_custom_extern_func(int32_t);"),
        "expected custom extern function declaration, got:\n{}",
        c
    );
}

#[test]
fn parity_references_and_deref() {
    let src = r#"
    func takes_ref(p: &int): int {
        return *p
    }
    func main(): int {
        let x: int = 123
        return takes_ref(x)
    }
    "#;
    test_execution_parity("references_and_deref", src);
}

#[test]
fn parity_mixed_alignment_packing() {
    let src = r#"
    struct MixedLayout {
        a: byte
        b: int
        c: bool
        d: int
    }
    func main(): int {
        let m = MixedLayout { a: 42 as byte, b: 999999, c: true, d: 123456 }
        if (m.a as int) == 42 && m.b == 999999 && m.c && m.d == 123456 {
            return 0
        }
        return 1
    }
    "#;
    test_execution_parity("mixed_alignment_packing", src);
}

#[test]
fn coroutine_value_uses_pointer_abi_in_both_backends() {
    let result = test_execution_result(
        "coroutine_pointer_abi",
        r#"
extern "C" {
    func ar_co_block_on_i64(state: ptr[u8]): i64
    func ar_co_free(state: ptr[u8]): void
}
async func answer(): int { return 42 }
func main(): int {
    let job = answer()
    let p = unsafe { job as ptr[u8] }
    let v = unsafe { ar_co_block_on_i64(p) as int }
    unsafe { ar_co_free(p) }
    return v
}
"#,
    );
    assert_eq!(result, (42, 42));
}

#[test]
fn cooperative_task_table_matches_in_both_backends() {
    let result = test_execution_result(
        "rt_task_table",
        r#"
extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
    func ar_rt_cancel_i64(handle: int): void
}
async func answer(): int { return 42 }
func main(): int {
    let job = answer()
    let handle = unsafe { ar_rt_spawn_i64(job as ptr[u8]) }
    return unsafe { ar_rt_join_i64(handle) }
}
"#,
    );
    assert_eq!(result, (42, 42));
}

#[test]
fn cooperative_cancel_before_join_recovers_slot_in_c() {
    let (amir, tc) = compile_src(
        r#"
extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
    func ar_rt_cancel_i64(handle: int): void
}
async func answer(): int { return 42 }
func main(): int {
    let job = answer()
    let handle = unsafe { ar_rt_spawn_i64(job as ptr[u8]) }
    unsafe { ar_rt_cancel_i64(handle) }
    let job2 = answer()
    let handle2 = unsafe { ar_rt_spawn_i64(job2 as ptr[u8]) }
    if handle2 != handle {
        return 1
    }
    return unsafe { ar_rt_join_i64(handle2) }
}
"#,
    );
    // The C fixture runs in its own process, so no unrelated test can claim
    // the released slot between cancel and spawn. Keep the exact reuse check
    // here rather than in the Cranelift parity fixture, whose runtime table is
    // shared by concurrently running tests.
    assert_eq!(execute_c("rt_task_cancel_slot_reuse", &amir, &tc), 42);
}

#[test]
fn cooperative_cancel_before_join_matches_across_backends() {
    let result = test_execution_result(
        "rt_task_cancel_parity",
        r#"
extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
    func ar_rt_cancel_i64(handle: int): void
}
async func answer(): int { return 42 }
func main(): int {
    let job = answer()
    let handle = unsafe { ar_rt_spawn_i64(job as ptr[u8]) }
    unsafe { ar_rt_cancel_i64(handle) }
    let job2 = answer()
    let handle2 = unsafe { ar_rt_spawn_i64(job2 as ptr[u8]) }
    let result = unsafe { ar_rt_join_i64(handle2) }
    unsafe { ar_rt_cancel_i64(handle2) }
    return result
}
"#,
    );
    assert_eq!(result, (42, 42));
}

#[test]
fn cooperative_cached_join_reuses_result_without_polling() {
    let result = test_execution_result(
        "rt_task_cached",
        r#"
extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
    func ar_rt_cancel_i64(handle: int): void
}
async func answer(): int { return 42 }
func main(): int {
    let job = answer()
    let handle = unsafe { ar_rt_spawn_i64(job as ptr[u8]) }
    let first = unsafe { ar_rt_join_i64(handle) }
    let cached = unsafe { ar_rt_join_i64(handle) }
    if first != 42 || cached != 42 {
        return 1
    }
    unsafe { ar_rt_cancel_i64(handle) }
    return 0
}
"#,
    );
    assert_eq!(result, (0, 0));
}

#[test]
fn owned_job_lifecycle_preserves_fields_and_cleanup_in_c() {
    let source = include_str!("../../arandu_cli/tests/fixtures/owned_result_lifecycle.aru");
    for optimized in [false, true] {
        let (mut amir, tc) = compile_src(source);
        if optimized {
            optimize_amir_checked_with_level(
                &mut amir,
                tc.symbols.as_ref(),
                &tc.type_info.type_interner,
                OptLevel::O2,
            )
            .expect("valid owned job must optimize");
        }
        let name = if optimized {
            "owned_job_opt"
        } else {
            "owned_job"
        };
        let (status, stdout) = execute_c_output(name, &amir, &tc);
        assert_eq!(status, 0);
        assert_eq!(
            stdout.replace("\r\n", "\n"),
            "30\n20\n0\n",
            "optimized={optimized}"
        );
    }
}

/// SL_P Fase 4: a compiler-shaped job thunk written as an ordinary generic
/// function. `dispatch` reads a `Job<R>` payload from `context`, runs it and
/// writes the `R` result into `result` — the exact `WorkThunk` ABI
/// (`(ptr, ptr) -> i32`). `main` feeds it blobs through the `alloc`/`free`
/// builtins so the same source type-checks and lowers in both backends.
const GENERIC_WORK_THUNK_SRC: &str = r#"
module std.core.workthunk

extern "arandu-intrinsic" {
    func ptrRead<T>(p: ptr[T]) : T
    func ptrWrite<T>(p: ptr[T], val: T) : void
}

interface Job<R> {
    func run(shared self): R
}

struct Stats { code: int, comment: int, blank: int }
struct CountJob { amount: int }

func CountJob.run(shared self): Stats {
    return Stats { code: self.amount, comment: 2, blank: 3 }
}

func dispatch<R, C: Job<R>>(context: ptr[C], result: ptr[R]): i32 {
    let job = unsafe { ptrRead<C>(context) }
    let out = job.run()
    unsafe { ptrWrite<R>(result, out) }
    return 0
}

func main(): int {
    let job = CountJob { amount: 37 }
    let c = alloc(8) as ptr[CountJob]
    unsafe { ptrWrite<CountJob>(c, job) }
    let r = alloc(24) as ptr[Stats]
    let rc = dispatch<Stats, CountJob>(c, r)
    let out = unsafe { ptrRead<Stats>(r) }
    if rc != 0 { return 9 }
    if out.code != 37 { return 1 }
    if out.comment != 2 { return 2 }
    if out.blank != 3 { return 3 }
    unsafe { free(c) }
    unsafe { free(r) }
    return 0
}
"#;

#[test]
fn generic_work_thunk_runs_identically_in_c_and_cranelift() {
    // Runs the real production pipeline (`monomorphize_program`) on both
    // backends; `execute_c` wraps the emitted C with a C main and compares the
    // exit code with the Cranelift-run Arandu `main`.
    let (amir, tc) = compile_src_mono(GENERIC_WORK_THUNK_SRC);
    let actual_result = execute_c("generic_work_thunk", &amir, &tc);
    let expected = execute_cranelift(&amir, &tc);
    assert_eq!(
        expected, actual_result,
        "Execution mismatch for generic_work_thunk! Cranelift={expected}, C={actual_result}"
    );
    assert_eq!(expected, 0);
}

/// Host-side mirror of the Arandu structs fed to/read from the thunk blobs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct CountJobHost {
    amount: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct StatsHost {
    code: i32,
    comment: i32,
    blank: i32,
}

/// Compile the generic thunk source and return the exact host name under which
/// the monomorphized `dispatch<Stats, CountJob>` instance was registered.
fn compile_generic_work_thunk() -> (AmirProgram, TypeCheckResult, String) {
    let (amir, tc) = compile_src_mono(GENERIC_WORK_THUNK_SRC);
    let instance_name = amir
        .funcs
        .iter()
        .filter_map(|f| {
            let name = tc.symbols.host_func_name(tc.symbols.get(f.symbol));
            name.contains("_A$std.core.workthunk.dispatch$I_")
                .then(|| name.to_string())
        })
        .next()
        .expect("monomorphized dispatch instance missing");
    assert!(
        instance_name.starts_with("std.core.workthunk.dispatch."),
        "instance must be registered under its qualified host name, got {instance_name}"
    );
    (amir, tc, instance_name)
}

/// Same as [`compile_src`] but runs the monomorphization pass, matching the
/// production pipeline: generic callees become real instanced functions
/// (`_A$...`) instead of being inlined at the call site by AMIR lowering.
fn compile_src_mono(src: &str) -> (AmirProgram, TypeCheckResult) {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.is_empty(),
        "type check failed: {:?}",
        tc.diagnostics
    );

    let mut hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let _specialized =
        arandu_semantics::monomorphize_program(&mut tc, &mut hir).expect("monomorphization failed");
    let (amir, _) = lower_to_amir_with_interfaces(&mut tc, &hir, 8).expect("AMIR lowering failed");
    (amir, tc)
}

#[test]
fn generic_work_thunk_is_host_callable_at_workthunk_abi() {
    let (amir, tc, instance_name) = compile_generic_work_thunk();
    let backend = CraneliftBackend::try_new().unwrap();
    let compiled =
        CodegenBackend::compile(backend, &amir, tc.symbols.as_ref(), tc.type_info.as_ref())
            .expect("cranelift compile failed");

    let via_main = unsafe {
        let main_fn =
            arandu_semantics::CompiledCode::get_fn::<unsafe fn() -> i32>(&compiled, "main")
                .expect("main not found");
        main_fn()
    };
    assert_eq!(via_main, 0);

    let thunk: arandu_runtime::worker_runtime::WorkThunk = unsafe {
        arandu_semantics::CompiledCode::get_fn(&compiled, &instance_name)
            .expect("instance not exported under its host name")
    };

    let mut context = CountJobHost { amount: 37 };
    let mut result = std::mem::MaybeUninit::<StatsHost>::uninit();
    let status = unsafe {
        thunk(
            (&mut context as *mut CountJobHost).cast::<u8>(),
            result.as_mut_ptr().cast::<u8>(),
        )
    };
    assert_eq!(status, arandu_runtime::worker_runtime::WORK_COMPLETED);
    let stats = unsafe { result.assume_init() };
    assert_eq!(
        stats,
        StatsHost {
            code: 37,
            comment: 2,
            blank: 3
        }
    );
}

#[test]
fn generic_work_thunk_executes_inside_worker_pool_thread() {
    let (amir, tc, instance_name) = compile_generic_work_thunk();
    let backend = CraneliftBackend::try_new().unwrap();
    let compiled =
        CodegenBackend::compile(backend, &amir, tc.symbols.as_ref(), tc.type_info.as_ref())
            .expect("cranelift compile failed");
    let thunk: arandu_runtime::worker_runtime::WorkThunk = unsafe {
        arandu_semantics::CompiledCode::get_fn(&compiled, &instance_name)
            .expect("instance not exported under its host name")
    };

    let pool = arandu_runtime::worker_scheduler::WorkerPool::new(2, 4)
        .expect("pool with two workers must spawn");
    // SAFETY: `CountJobHost`/`StatsHost` mirror the Arandu layouts and the
    // thunk obeys the `WorkThunk` lifecycle contract.
    let task = unsafe {
        arandu_runtime::worker_runtime::WorkerTask::try_new::<CountJobHost, StatsHost>(
            CountJobHost { amount: 41 },
            thunk,
        )
        .expect("static-sized payload must be encodable")
    };
    let pending = pool
        .core()
        .submit(task)
        .expect("bounded admission must accept one task");
    let result = pending.wait().expect("task must complete before shutdown");
    let stats = result
        .try_take::<StatsHost>()
        .expect("typed result must extract");
    assert_eq!(
        stats,
        StatsHost {
            code: 41,
            comment: 2,
            blank: 3
        }
    );
}

const PARALLEL_FOLD_PARITY_SRC: &str = r#"
module std.core.parallel_parity

extern "arandu-intrinsic" {
    func ptrRead<T>(p: ptr[T]) : T
    func ptrWrite<T>(p: ptr[T], val: T) : void
    func ptrOffset<T>(base: ptr[T], offset: i32) : ptr[T]
    func sliceFromRaw(owner: ptr[int], data: ptr[int], len: uint): []int
    func sliceLen<T>(source: []T): uint
    func sliceSubslice<T>(source: []T, start: uint, len: uint): []T
}

interface ParallelJob<T, R> {
    func run(self: ref Self, item: ref T, state: mut ref R): void
}

interface Combine<R> {
    func combine(self: ref Self, dest: mut ref R, partial: ref R): void
}

struct Stats { code: int, comment: int, blank: int }

struct LineCountJob {}
struct StatsCombiner {}

func LineCountJob.run(self: ref LineCountJob, item: ref int, state: mut ref Stats): void {
    state.code = state.code + *item
    state.comment = state.comment + 1
    state.blank = state.blank + 2
}

func StatsCombiner.combine(self: ref StatsCombiner, dest: mut ref Stats, partial: ref Stats): void {
    dest.code = dest.code + partial.code
    dest.comment = dest.comment + partial.comment
    dest.blank = dest.blank + partial.blank
}

@Repr("C")
struct ChunkContext {
    subslice: []int,
    seed: Stats,
    job: LineCountJob,
    stop_flag: ptr[int],
}

func dispatchChunk(context: ptr[ChunkContext], result: ptr[Stats]): i32 {
    let ctx: ChunkContext = unsafe { ptrRead<ChunkContext>(context) }
    let mut state = ctx.seed
    let count = sliceLen<int>(ctx.subslice)
    let mut i: uint = 0
    let nullp: ptr[int] = nil
    while i < count {
        if ctx.stop_flag != nullp {
            let flag_val: int = unsafe { ptrRead<int>(ctx.stop_flag) }
            if flag_val != 0 {
                return 2
            }
        }
        ctx.job.run(ref ctx.subslice[i], mut ref state)
        i = i + 1
    }
    unsafe { ptrWrite<Stats>(result, state) }
    return 0
}

func foldSeq(data: []int, seed: Stats, job: LineCountJob): Stats {
    let mut acc = Stats { code: seed.code, comment: seed.comment, blank: seed.blank }
    let len = sliceLen<int>(data)
    let mut i: uint = 0
    while i < len {
        job.run(ref data[i], mut ref acc)
        i = i + 1
    }
    return acc
}

func parallelFoldSim(
    data: []int,
    seed: Stats,
    job: LineCountJob,
    combine: StatsCombiner,
    workers: uint
): Stats {
    let total_len = sliceLen<int>(data)
    if total_len == 0 {
        return Stats { code: seed.code, comment: seed.comment, blank: seed.blank }
    }
    if total_len <= 4 || workers <= 1 {
        return foldSeq(data, seed, job)
    }
    let mut chunk_count = workers
    if chunk_count > total_len {
        chunk_count = total_len
    }
    let base_chunk_size = total_len / chunk_count
    let remainder = total_len % chunk_count

    let mut acc = Stats { code: seed.code, comment: seed.comment, blank: seed.blank }
    let mut offset: uint = 0
    let mut c: uint = 0
    while c < chunk_count {
        let mut current_size = base_chunk_size
        if c < remainder {
            current_size = current_size + 1
        }
        if current_size > 0 {
            let chunk_slice = sliceSubslice<int>(data, offset, current_size)
            let chunk_seed = Stats { code: 0, comment: 0, blank: 0 }
            let chunk_res = foldSeq(chunk_slice, chunk_seed, job)
            combine.combine(mut ref acc, ref chunk_res)
            offset = offset + current_size
        }
        c = c + 1
    }
    return acc
}

func main(): int {
    let raw = alloc(64) as ptr[int]
    let mut i: int = 0
    while i < 8 {
        let p = unsafe { ptrOffset<int>(raw, (i as i32)) }
        let it = (i + 1) * 10
        unsafe { ptrWrite<int>(p, it) }
        i = i + 1
    }
    let items = unsafe { sliceFromRaw(raw, raw, 8 as uint) }

    let seed = Stats { code: 0, comment: 0, blank: 0 }
    let job = LineCountJob {}
    let combiner = StatsCombiner {}

    let seq = foldSeq(items, seed, job)
    let par = parallelFoldSim(items, seed, job, combiner, 3 as uint)

    // Bit-identical assertion: parallel reduction must equal sequential fold!
    if seq.code != par.code { return 1 }
    if seq.comment != par.comment { return 2 }
    if seq.blank != par.blank { return 3 }

    // Check specific calculated values:
    // 8 items, sum of lines = 10+20+30+40+50+60+70+80 = 360
    // comment = 8, blank = 16
    if par.code != 360 { return 4 }
    if par.comment != 8 { return 5 }
    if par.blank != 16 { return 6 }

    unsafe { free(raw) }

    return 0
}
"#;

#[test]
fn parallel_fold_sim_runs_identically_in_c_and_cranelift() {
    let (amir, tc) = compile_src_mono(PARALLEL_FOLD_PARITY_SRC);
    let actual_result = execute_c("parallel_fold_parity", &amir, &tc);
    let expected = execute_cranelift(&amir, &tc);
    assert_eq!(
        expected, actual_result,
        "Execution mismatch for parallel_fold_parity! Cranelift={expected}, C={actual_result}"
    );
    assert_eq!(expected, 0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct SliceDescriptorHost {
    ptr: *const i32,
    len: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct ChunkContextHost {
    subslice: SliceDescriptorHost,
    seed: StatsHost,
    stop_flag: *const i64,
}

unsafe impl Send for ChunkContextHost {}

#[test]
fn parallel_dispatch_chunk_executes_in_worker_pool_and_honors_cancellation() {
    let (amir, tc) = compile_src_mono(PARALLEL_FOLD_PARITY_SRC);
    let backend = CraneliftBackend::try_new().unwrap();
    let compiled =
        CodegenBackend::compile(backend, &amir, tc.symbols.as_ref(), tc.type_info.as_ref())
            .expect("cranelift compile failed");

    let instance_name = amir
        .funcs
        .iter()
        .filter_map(|f| {
            let name = tc.symbols.host_func_name(tc.symbols.get(f.symbol));
            (name.contains("dispatchChunk") || name.ends_with(".dispatchChunk"))
                .then(|| name.to_string())
        })
        .next()
        .expect("dispatchChunk instance missing");

    let thunk: arandu_runtime::worker_runtime::WorkThunk = unsafe {
        arandu_semantics::CompiledCode::get_fn(&compiled, &instance_name)
            .expect("dispatchChunk not exported under its host name")
    };

    let pool = arandu_runtime::worker_scheduler::WorkerPool::new(2, 4).unwrap();

    let items: Vec<i32> = vec![10, 25, 15];
    let desc = SliceDescriptorHost {
        ptr: items.as_ptr(),
        len: items.len() as u64,
    };
    let stop_flag: i64 = 0;
    let stats = StatsHost {
        code: 0,
        comment: 0,
        blank: 0,
    };

    let ctx = ChunkContextHost {
        subslice: desc,
        seed: stats,
        stop_flag: &stop_flag,
    };

    // 1. Successful execution across real OS worker thread
    let task = unsafe {
        arandu_runtime::worker_runtime::WorkerTask::try_new::<ChunkContextHost, StatsHost>(
            ctx, thunk,
        )
    }
    .unwrap();

    let pending = pool.core().submit(task).unwrap();
    let result = pending.wait().unwrap().try_take::<StatsHost>().unwrap();
    assert_eq!(
        result,
        StatsHost {
            code: 50,
            comment: 3,
            blank: 6
        }
    );

    // 2. Cooperative cancellation via stop_flag (returns WORK_CANCELED = 2 -> WorkerError::Canceled)
    let canceled_flag: i64 = 1;
    let cancel_stats = StatsHost {
        code: 0,
        comment: 0,
        blank: 0,
    };
    let cancel_ctx = ChunkContextHost {
        subslice: desc,
        seed: cancel_stats,
        stop_flag: &canceled_flag,
    };
    let cancel_task = unsafe {
        arandu_runtime::worker_runtime::WorkerTask::try_new::<ChunkContextHost, StatsHost>(
            cancel_ctx, thunk,
        )
    }
    .unwrap();
    let cancel_pending = pool.core().submit(cancel_task).unwrap();
    assert_eq!(
        cancel_pending.wait().unwrap_err(),
        arandu_runtime::worker_runtime::WorkerError::Canceled
    );

    // 3. Pre-admission cancellation check (JDK-8311867)
    let pre_cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let pre_cancel_task = unsafe {
        arandu_runtime::worker_runtime::WorkerTask::try_new::<ChunkContextHost, StatsHost>(
            ctx, thunk,
        )
    }
    .unwrap()
    .with_cancel_token(pre_cancel_token);
    let pre_pending = pool.core().submit(pre_cancel_task).unwrap();
    assert_eq!(
        pre_pending.wait().unwrap_err(),
        arandu_runtime::worker_runtime::WorkerError::Canceled
    );
}

#[test]
fn c_backend_worker_pool_reuses_threads_and_handles_self_help_and_errors() {
    let (amir, tc) = compile_src(
        "extern \"C\" { func ar_rt_spawn_i64(state: ptr[u8]): i64 }\npublic func probe(state: ptr[u8]): i64 { return unsafe { ar_rt_spawn_i64(state) } }\nfunc main(): int { return 0 }",
    );
    let c_code = emit_c(&amir, &tc);

    let test_harness = r#"
#include <stdio.h>
#include <assert.h>

static int32_t thunk_add_one(uint8_t *ctx, uint8_t *res) {
    int64_t val = *(int64_t*)ctx;
    *(int64_t*)res = val + 1;
    return 0;
}

static int32_t thunk_nested_self_help(uint8_t *ctx, uint8_t *res) {
    int64_t val = *(int64_t*)ctx;
    int64_t inner_in = val * 10;
    int64_t inner_out = 0;
    uint8_t *inner_ctx = (uint8_t*)&inner_in;
    uint8_t *inner_res = (uint8_t*)&inner_out;
    int32_t status = ar_rt_parallel_fold_run(1, &inner_ctx, thunk_add_one, &inner_res, 2, NULL);
    if (status != 0) return 99;
    *(int64_t*)res = inner_out;
    return 0;
}

static _Atomic int failure_gate = 0;

static int32_t thunk_fail_at_5_and_7(uint8_t *ctx, uint8_t *res) {
    int64_t val = *(int64_t*)ctx;
    (void)res;
    if (val == 5 || val == 7) {
        atomic_fetch_add_explicit(&failure_gate, 1, memory_order_release);
        while (atomic_load_explicit(&failure_gate, memory_order_acquire) < 2) {}
        return val == 5 ? 505 : 707;
    }
    return 0;
}

static int32_t thunk_fail_only_at_5(uint8_t *ctx, uint8_t *res) {
    int64_t val = *(int64_t*)ctx;
    (void)res;
    if (val == 5) return 505;
    return 0;
}

int main(void) {
    // 1. Repeated calls: verify worker pool reuses threads across multiple calls
    for (int iter = 0; iter < 50; iter++) {
        int64_t in_vals[8] = {1, 2, 3, 4, 5, 6, 7, 8};
        int64_t out_vals[8] = {0};
        uint8_t *ctx_ptrs[8];
        uint8_t *res_ptrs[8];
        for (int i = 0; i < 8; i++) {
            ctx_ptrs[i] = (uint8_t*)&in_vals[i];
            res_ptrs[i] = (uint8_t*)&out_vals[i];
        }
        int32_t status = ar_rt_parallel_fold_run(8, ctx_ptrs, thunk_add_one, res_ptrs, 4, NULL);
        if (status != 0) return 10;
        for (int i = 0; i < 8; i++) {
            if (out_vals[i] != in_vals[i] + 1) return 11;
        }
    }

    // 2. Nested self-help execution: prevents deadlock when worker submits parallel work
    {
        int64_t in_vals[4] = {2, 3, 4, 5};
        int64_t out_vals[4] = {0};
        uint8_t *ctx_ptrs[4];
        uint8_t *res_ptrs[4];
        for (int i = 0; i < 4; i++) {
            ctx_ptrs[i] = (uint8_t*)&in_vals[i];
            res_ptrs[i] = (uint8_t*)&out_vals[i];
        }
        int32_t status = ar_rt_parallel_fold_run(4, ctx_ptrs, thunk_nested_self_help, res_ptrs, 2, NULL);
        if (status != 0) return 20;
        if (out_vals[0] != 21 || out_vals[1] != 31 || out_vals[2] != 41 || out_vals[3] != 51) return 21;
    }

    // 3. Lowest ordinal and its code remain paired under concurrent failures.
    {
        int64_t in_vals[8] = {0, 1, 2, 3, 4, 5, 6, 7};
        int64_t out_vals[8] = {0};
        uint8_t *ctx_ptrs[8];
        uint8_t *res_ptrs[8];
        for (int i = 0; i < 8; i++) {
            ctx_ptrs[i] = (uint8_t*)&in_vals[i];
            res_ptrs[i] = (uint8_t*)&out_vals[i];
        }
        for (int iter = 0; iter < 200; iter++) {
            atomic_store_explicit(&failure_gate, 0, memory_order_release);
            int32_t status = ar_rt_parallel_fold_run(
                8, ctx_ptrs, thunk_fail_at_5_and_7, res_ptrs, 4, NULL
            );
            if (status != 505) return 30;
        }
    }

    // 4. A worker failure publishes cancellation when a stop flag is supplied.
    {
        int64_t in_vals[8] = {0, 1, 2, 3, 4, 5, 6, 7};
        int64_t out_vals[8] = {0};
        uint8_t *ctx_ptrs[8];
        uint8_t *res_ptrs[8];
        for (int i = 0; i < 8; i++) {
            ctx_ptrs[i] = (uint8_t*)&in_vals[i];
            res_ptrs[i] = (uint8_t*)&out_vals[i];
        }
        _Atomic int64_t stop_flag = 0;
        int32_t status = ar_rt_parallel_fold_run(
            8, ctx_ptrs, thunk_fail_only_at_5, res_ptrs, 4, (int64_t*)&stop_flag
        );
        if (status != 505) return 40;
        if (atomic_load(&stop_flag) != 1) return 41;
    }

    // 5. Pre-admission cancellation check
    {
        int64_t in_vals[2] = {1, 2};
        int64_t out_vals[2] = {0};
        uint8_t *ctx_ptrs[2] = {(uint8_t*)&in_vals[0], (uint8_t*)&in_vals[1]};
        uint8_t *res_ptrs[2] = {(uint8_t*)&out_vals[0], (uint8_t*)&out_vals[1]};
        _Atomic int64_t stop_flag = 1;
        int32_t status = ar_rt_parallel_fold_run(2, ctx_ptrs, thunk_add_one, res_ptrs, 2, (int64_t*)&stop_flag);
        if (status != 2) return 50;
    }

    return 0;
}
"#;

    let out_dir = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&out_dir).unwrap();
    let c_file = out_dir.join("worker_pool_suite.c");
    let exe_file = out_dir.join("worker_pool_suite.exe");

    let c_code = format!("#define main arandu_main\n{}\n#undef main\n", c_code);
    let full_src = format!("{}\n{}", c_code, test_harness);
    fs::write(&c_file, full_src).unwrap();

    let cc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let compile_status = c_compiler(&cc)
        .arg(&c_file)
        .arg("-o")
        .arg(&exe_file)
        .arg("-pthread")
        .args(arandu_middle::TargetSpec::for_host().cc_link_args())
        .output()
        .expect("compile worker pool suite");

    assert!(
        compile_status.status.success(),
        "Compilation failed: {}",
        String::from_utf8_lossy(&compile_status.stderr)
    );

    let run_status = Command::new(&exe_file)
        .output()
        .expect("run worker pool suite");

    assert!(
        run_status.status.success(),
        "Worker pool suite failed with exit code: {:?}, stderr: {}",
        run_status.status.code(),
        String::from_utf8_lossy(&run_status.stderr)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn c_backend_worker_pool_falls_back_when_thread_creation_is_partial() {
    let (amir, tc) = compile_src(
        "extern \"C\" { func ar_rt_spawn_i64(state: ptr[u8]): i64 }\npublic func probe(state: ptr[u8]): i64 { return unsafe { ar_rt_spawn_i64(state) } }\nfunc main(): int { return 0 }",
    );
    let c_code = emit_c(&amir, &tc);
    let harness = r#"
#define main arandu_main
__GENERATED_C__
#undef main
#include <errno.h>
#include <unistd.h>

static unsigned create_attempts = 0;
int __real_pthread_create(pthread_t*, const pthread_attr_t*, void *(*)(void*), void*);
int __wrap_pthread_create(pthread_t *thread, const pthread_attr_t *attr,
                          void *(*start)(void*), void *arg) {
    if (create_attempts++ != 0) return EAGAIN;
    return __real_pthread_create(thread, attr, start, arg);
}

static int32_t thunk_add_one(uint8_t *ctx, uint8_t *res) {
    *(int64_t*)res = *(int64_t*)ctx + 1;
    return 0;
}

int main(void) {
    alarm(5); /* a regression must fail instead of hanging the test suite */
    int64_t input[8] = {0, 1, 2, 3, 4, 5, 6, 7};
    int64_t output[8] = {0};
    uint8_t *contexts[8];
    uint8_t *results[8];
    for (int i = 0; i < 8; i++) {
        contexts[i] = (uint8_t*)&input[i];
        results[i] = (uint8_t*)&output[i];
    }
    if (ar_rt_parallel_fold_run(8, contexts, thunk_add_one, results, 4, NULL) != 0) return 1;
    for (int i = 0; i < 8; i++) if (output[i] != input[i] + 1) return 2;
    return 0;
}
"#;
    let full_src = harness.replace("__GENERATED_C__", &c_code);
    let out_dir = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&out_dir).unwrap();
    let c_file = out_dir.join("pool_partial_failure.c");
    let exe_file = out_dir.join("pool_partial_failure.exe");
    fs::write(&c_file, full_src).unwrap();

    let cc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let compile = c_compiler(&cc)
        .arg(&c_file)
        .arg("-o")
        .arg(&exe_file)
        .arg("-pthread")
        .arg("-Wl,--wrap=pthread_create")
        .args(arandu_middle::TargetSpec::for_host().cc_link_args())
        .output()
        .expect("compile partial worker creation harness");
    assert!(
        compile.status.success(),
        "partial worker creation harness must compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&exe_file)
        .status()
        .expect("run partial worker creation harness");
    assert!(
        run.success(),
        "partial worker creation fallback failed: {run}"
    );
}

#[test]
fn parity_safe_mem_swap_and_replace() {
    let src = r#"
module std.core.mem_test

extern "arandu-intrinsic" {
    func refWrite<T>(dest: mut ref T, val: T): void
}

func swap<T>(x: mut ref T, y: mut ref T): void {
    let tmp = *x
    refWrite<T>(x, *y)
    refWrite<T>(y, tmp)
}

func replace<T>(dest: mut ref T, src: T): T {
    let old = *dest
    refWrite<T>(dest, src)
    return old
}

func main(): int {
    let mut a = 10
    let mut b = 20
    swap<int>(mut ref a, mut ref b)
    if a != 20 || b != 10 {
        return 1
    }
    let old = replace<int>(mut ref a, 99)
    if old != 20 || a != 99 {
        return 2
    }
    return 0
}
"#;
    let (amir, tc) = compile_src_mono(src);
    let c_res = execute_c("safe_mem_parity", &amir, &tc);
    assert_eq!(c_res, 0, "C backend failed safe mem test");
    let clif_res = execute_cranelift(&amir, &tc);
    assert_eq!(clif_res, 0, "Cranelift backend failed safe mem test");
}

#[test]
fn parity_safe_fmt_formatter() {
    let src = r#"
module std.core.fmt_test

extern "arandu-intrinsic" {
    func mutSliceFromRaw(owner: ptr[u8], data: ptr[u8], len: uint): mut []u8
}

struct Formatter {
    buf: mut []u8
    len: uint
}

func newFormatter(buf: mut []u8): Formatter {
    return Formatter {
        buf: buf,
        len: 0,
    }
}

func Formatter.writeByte(self: mut ref Formatter, b: u8): bool {
    self.buf[self.len] = b
    self.len = self.len + 1
    return true
}

func main(): int {
    let raw = alloc(16) as ptr[u8]
    let s = unsafe { mutSliceFromRaw(raw, raw, 16 as uint) }
    let mut f = newFormatter(s)
    f.writeByte(65 as u8)
    f.writeByte(66 as u8)
    if f.buf[0] != (65 as u8) || f.buf[1] != (66 as u8) {
        unsafe { free(raw) }
        return 1
    }
    if f.len != 2 {
        unsafe { free(raw) }
        return 2
    }
    unsafe { free(raw) }
    return 0
}
"#;
    let (amir, tc) = compile_src(src);
    let c_res = execute_c("safe_fmt_parity", &amir, &tc);
    assert_eq!(c_res, 0, "C backend failed safe fmt test");
    let clif_res = execute_cranelift(&amir, &tc);
    assert_eq!(clif_res, 0, "Cranelift backend failed safe fmt test");
}

#[test]
fn parity_string_bytes_intrinsic_preserves_fat_pointer_words() {
    let src = r#"
module std.core.str_bytes_parity

extern "arandu-intrinsic" {
    func strBytes(source: str): []u8
}

func main(): int {
    let source = "abcd"
    let bytes = unsafe { strBytes(source) }
    if bytes[0] != (97 as u8) || bytes[1] != (98 as u8) || bytes[3] != (100 as u8) {
        return 1
    }
    return 0
}
"#;
    let (amir, tc) = compile_src(src);
    let c_res = execute_c("str_bytes_parity", &amir, &tc);
    assert_eq!(c_res, 0, "C backend failed strBytes parity test");
    let clif_res = execute_cranelift(&amir, &tc);
    assert_eq!(clif_res, 0, "Cranelift backend failed strBytes parity test");
}

#[test]
fn parity_mut_ref_slice_preserves_data_and_length_words() {
    let src = r#"
module std.core.mut_slice_parity

extern "arandu-intrinsic" {
    func mutSliceFromRaw(owner: ptr[u8], data: ptr[u8], len: uint): mut []u8
    func mutSliceLen<T>(source: mut []T): uint
}

func fill(buf: mut ref mut []u8): uint {
    let len = unsafe { mutSliceLen<u8>(*buf) }
    if len != 4 { return 99 }
    buf[1] = 42 as u8
    return len
}

func observedLen(buf: ref mut []u8): uint {
    return unsafe { mutSliceLen<u8>(*buf) }
}

func main(): int {
    let raw = alloc(4) as ptr[u8]
    let mut bytes = unsafe { mutSliceFromRaw(raw, raw, 4 as uint) }
    let len = fill(mut ref bytes)
    if len != 4 || observedLen(ref bytes) != 4 || bytes[1] != (42 as u8) {
        unsafe { free(raw) }
        return 1
    }
    unsafe { free(raw) }
    return 0
}
"#;
    let (amir, tc) = compile_src(src);
    let c_res = execute_c("mut_ref_slice_parity", &amir, &tc);
    assert_eq!(c_res, 0, "C backend lost a mut-ref slice ABI word");
    let clif_res = execute_cranelift(&amir, &tc);
    assert_eq!(clif_res, 0, "Cranelift lost a mut-ref slice ABI word");
}

#[test]
fn parity_slice_data_pointer() {
    let src = r#"
module std.core.slice_data_test

extern "arandu-intrinsic" {
    func sliceFromRaw(owner: ptr[u8], data: ptr[u8], len: uint): []u8
    func sliceData<T>(source: []T): ptr[T]
    func ptrRead<T>(p: ptr[T]): T
    func ptrWrite<T>(p: ptr[T], val: T): void
    func ptrOffset<T>(base: ptr[T], offset: i32): ptr[T]
}

func asPtr<T>(s: []T): ptr[T] {
    unsafe {
        return sliceData<T>(s)
    }
}

func main(): int {
    let raw = alloc(8) as ptr[u8]
    unsafe {
        ptrWrite<u8>(raw, 42 as u8)
        ptrWrite<u8>(ptrOffset<u8>(raw, 1), 99 as u8)
    }
    let s = unsafe { sliceFromRaw(raw, raw, 8 as uint) }
    let p = asPtr<u8>(s)
    let v0 = unsafe { ptrRead<u8>(p) }
    if v0 != (42 as u8) {
        unsafe { free(raw) }
        return 1
    }
    let p1 = unsafe { ptrOffset<u8>(p, 1) }
    let v1 = unsafe { ptrRead<u8>(p1) }
    if v1 != (99 as u8) {
        unsafe { free(raw) }
        return 2
    }
    unsafe { ptrWrite<u8>(p, 77 as u8) }
    if s[0] != (77 as u8) {
        unsafe { free(raw) }
        return 3
    }
    unsafe { free(raw) }
    return 0
}
"#;
    let (amir, tc) = compile_src_mono(src);
    let c_res = execute_c("slice_data_parity", &amir, &tc);
    assert_eq!(c_res, 0, "C backend failed slice data test");
    let clif_res = execute_cranelift(&amir, &tc);
    assert_eq!(clif_res, 0, "Cranelift backend failed slice data test");
}

#[test]
fn parity_slice_split_at() {
    let src = r#"
module std.core.slice_split_test

extern "arandu-intrinsic" {
    func sliceFromRaw(owner: ptr[int], data: ptr[int], len: uint): []int
    func sliceLen(source: []int): uint
    func sliceSubslice(source: []int, start: uint, len: uint): []int
    func ptrWrite<T>(p: ptr[T], val: T): void
    func ptrOffset<T>(base: ptr[T], offset: i32): ptr[T]
}

struct Split {
    left: []int
    right: []int
}

func splitAt(s: []int, mid: uint): Split {
    let total = unsafe { sliceLen(s) }
    let left = unsafe { sliceSubslice(s, 0, mid) }
    let right = unsafe { sliceSubslice(s, mid, total - mid) }
    return Split { left: left, right: right }
}

func main(): int {
    let raw = alloc(32) as ptr[int]
    unsafe {
        ptrWrite<int>(raw, 10)
        ptrWrite<int>(ptrOffset<int>(raw, 1), 20)
        ptrWrite<int>(ptrOffset<int>(raw, 2), 30)
        ptrWrite<int>(ptrOffset<int>(raw, 3), 40)
    }
    let s = unsafe { sliceFromRaw(raw, raw, 4 as uint) }
    let parts = splitAt(s, 2 as uint)
    if parts.left[0] != 10 || parts.left[1] != 20 {
        unsafe { free(raw) }
        return 1
    }
    if parts.right[0] != 30 || parts.right[1] != 40 {
        unsafe { free(raw) }
        return 2
    }
    unsafe { free(raw) }
    return 0
}
"#;
    let (amir, tc) = compile_src(src);
    let c_res = execute_c("slice_split_parity", &amir, &tc);
    assert_eq!(c_res, 0, "C backend failed slice split test");
    let clif_res = execute_cranelift(&amir, &tc);
    assert_eq!(clif_res, 0, "Cranelift backend failed slice split test");
}

#[test]
fn parity_parallel_fold_non_copy() {
    let src = r#"
module std.core.parallel_non_copy_test

extern "arandu-intrinsic" {
    func ptrRead<T>(p: ptr[T]): T
    func ptrWrite<T>(p: ptr[T], val: T): void
    func ptrOffset<T>(base: ptr[T], offset: i32): ptr[T]
    func sliceFromRaw(owner: ptr[int], data: ptr[int], len: uint): []int
    func sliceLen(source: []int): uint
    func sliceSubslice(source: []int, start: uint, len: uint): []int
}

interface AccumulatorInit<R> {
    func init(self: ref Self): R
}

interface ParallelJob<T, R> {
    func run(self: ref Self, item: ref T, state: mut ref R): void
}

interface Combine<R> {
    func combine(self: ref Self, dest: mut ref R, partial: ref R): void
}

struct ManagedAcc {
    total: int,
    count: int,
}

struct AccFactory {
    tag: int,
}

func AccFactory.init(self: ref AccFactory): ManagedAcc {
    return ManagedAcc { total: self.tag, count: 0 }
}

struct SumJob {}
func SumJob.run(self: ref SumJob, item: ref int, state: mut ref ManagedAcc): void {
    state.total = state.total + *item
    state.count = state.count + 1
}

struct AccCombine {}
func AccCombine.combine(self: ref AccCombine, dest: mut ref ManagedAcc, partial: ref ManagedAcc): void {
    dest.total = dest.total + partial.total
    dest.count = dest.count + partial.count
}

struct ChunkContextWithInit {
    subslice: []int,
    job: SumJob,
    init: AccFactory,
    stop_flag: ptr[int],
}

func dispatchChunkWithInit(ctx: ref ChunkContextWithInit, result: mut ref ManagedAcc): i32 {
    let mut state = ctx.init.init()
    let count = unsafe { sliceLen(ctx.subslice) }
    let mut i: uint = 0
    let nullp: ptr[int] = nil
    while i < count {
        if ctx.stop_flag != nullp {
            let flag_val: int = unsafe { ptrRead<int>(ctx.stop_flag) }
            if flag_val != 0 {
                return 2
            }
        }
        ctx.job.run(ref ctx.subslice[i], mut ref state)
        i = i + 1
    }
    result.total = state.total
    result.count = state.count
    return 0
}

func parallelFoldWithInitSim(
    data: []int,
    seed: ManagedAcc,
    factory: AccFactory,
    job: SumJob,
    combine: AccCombine,
    workers: uint
): ManagedAcc {
    let total_len = unsafe { sliceLen(data) }
    if total_len == 0 {
        return seed
    }
    let mut chunk_count = workers
    if chunk_count > total_len {
        chunk_count = total_len
    }
    let base_chunk_size = total_len / chunk_count
    let remainder = total_len % chunk_count

    let mut acc = seed
    let mut offset: uint = 0
    let mut c: uint = 0
    while c < chunk_count {
        let mut current_size = base_chunk_size
        if c < remainder {
            current_size = current_size + 1
        }
        if current_size > 0 {
            let chunk_slice = unsafe { sliceSubslice(data, offset, current_size) }
            let nullp: ptr[int] = nil
            let ctx = ChunkContextWithInit {
                subslice: chunk_slice,
                job: job,
                init: factory,
                stop_flag: nullp,
            }
            let mut chunk_res = ManagedAcc { total: 0, count: 0 }
            let code = dispatchChunkWithInit(ref ctx, mut ref chunk_res)
            if code == 0 {
                combine.combine(mut ref acc, ref chunk_res)
            }
            offset = offset + current_size
        }
        c = c + 1
    }
    return acc
}

func main(): int {
    let raw = unsafe { alloc(48) as ptr[int] }
    let mut i: int = 0
    while i < 6 {
        let p = unsafe { ptrOffset<int>(raw, i as i32) }
        let val = (i + 1) * 10
        unsafe { ptrWrite<int>(p, val) }
        i = i + 1
    }
    let data = unsafe { sliceFromRaw(raw, raw, 6 as uint) }

    let seed = ManagedAcc { total: 0, count: 0 }
    let factory = AccFactory { tag: 100 }
    let job = SumJob {}
    let combiner = AccCombine {}

    // 3 workers over 6 items -> 3 chunks of 2 items
    // Each chunk starts with tag=100 from factory.init():
    // Chunk 0: 100 + 10 + 20 = 130, count 2
    // Chunk 1: 100 + 30 + 40 = 170, count 2
    // Chunk 2: 100 + 50 + 60 = 210, count 2
    // Total combined: 130 + 170 + 210 = 510, count: 6
    let res = parallelFoldWithInitSim(data, seed, factory, job, combiner, 3 as uint)

    if res.total != 510 {
        unsafe { free(raw as ptr[u8]) }
        return 1
    }
    if res.count != 6 {
        unsafe { free(raw as ptr[u8]) }
        return 2
    }

    unsafe { free(raw as ptr[u8]) }
    return 0
}
"#;
    let (amir, tc) = compile_src_mono(src);
    let c_res = execute_c("parallel_fold_non_copy", &amir, &tc);
    assert_eq!(c_res, 0, "C backend failed non-copy parallel fold test");
    let clif_res = execute_cranelift(&amir, &tc);
    assert_eq!(
        clif_res, 0,
        "Cranelift backend failed non-copy parallel fold test"
    );
}

#[test]
fn parity_parallel_float_determinism() {
    let src = r#"
module std.core.parallel_float_determinism

extern "arandu-intrinsic" {
    func ptrRead<T>(p: ptr[T]): T
    func ptrWrite<T>(p: ptr[T], val: T): void
    func ptrOffset<T>(base: ptr[T], offset: i32): ptr[T]
    func sliceFromRaw(owner: ptr[float], data: ptr[float], len: uint): []float
    func sliceLen(source: []float): uint
    func sliceSubslice(source: []float, start: uint, len: uint): []float
}

struct FloatAcc {
    val: float,
}

struct FloatJob {}

func FloatJob.run(self: ref FloatJob, item: ref float, state: mut ref FloatAcc): void {
    state.val = state.val + *item
}

struct FloatCombine {}

func FloatCombine.combine(self: ref FloatCombine, dest: mut ref FloatAcc, partial: ref FloatAcc): void {
    dest.val = dest.val + partial.val
}

// In Arandu parallel reduction, the chunk partition depends strictly on input size
// and chunk granularity, NOT on worker count. Workers only schedule chunk batches.
// Partial results are then combined strictly in ordinal order (0, 1, ..., chunk_count - 1).
func runOrdinalFloatReduction(
    data: []float,
    chunk_count: uint,
    worker_count: uint
): FloatAcc {
    let total_len = unsafe { sliceLen(data) }
    let base_chunk_size = total_len / chunk_count
    let remainder = total_len % chunk_count

    let job = FloatJob {}
    let combiner = FloatCombine {}

    // 1. Each chunk calculates its partial sum
    let partials_raw = unsafe { alloc(chunk_count * (8 as uint)) as ptr[FloatAcc] }
    let mut offset: uint = 0
    let mut c: uint = 0
    while c < chunk_count {
        let mut current_size = base_chunk_size
        if c < remainder {
            current_size = current_size + 1
        }
        let chunk_slice = unsafe { sliceSubslice(data, offset, current_size) }
        let mut chunk_acc = FloatAcc { val: 0.0 }
        let len = unsafe { sliceLen(chunk_slice) }
        let mut j: uint = 0
        while j < len {
            job.run(ref chunk_slice[j], mut ref chunk_acc)
            j = j + 1
        }
        let p = unsafe { ptrOffset<FloatAcc>(partials_raw, c as i32) }
        unsafe { ptrWrite<FloatAcc>(p, chunk_acc) }
        offset = offset + current_size
        c = c + 1
    }

    // 2. Combining partial sums strictly in ordinal order:
    let mut acc = FloatAcc { val: 0.0 }
    let mut k: uint = 0
    while k < chunk_count {
        let p = unsafe { ptrOffset<FloatAcc>(partials_raw, k as i32) }
        let partial_val = unsafe { ptrRead<FloatAcc>(p) }
        combiner.combine(mut ref acc, ref partial_val)
        k = k + 1
    }

    unsafe { free(partials_raw as ptr[u8]) }
    return acc
}

func main(): int {
    let len: uint = 16
    let raw = unsafe { alloc(len * (8 as uint)) as ptr[float] }
    let mut i: int = 0
    // Fill with values that have non-terminating binary expansions to test IEEE-754 rounding
    while i < 16 {
        let p = unsafe { ptrOffset<float>(raw, i as i32) }
        let f = (i as float) * 0.125 + 0.1
        unsafe { ptrWrite<float>(p, f) }
        i = i + 1
    }
    let data = unsafe { sliceFromRaw(raw, raw, len) }

    // Regardless of how many workers (1, 2, 4, 8) process the fixed chunks (4 chunks),
    // the canonical ordinal reduction yields bit-for-bit identical results!
    let fixed_chunks: uint = 4
    let res1 = runOrdinalFloatReduction(data, fixed_chunks, 1 as uint)
    let res2 = runOrdinalFloatReduction(data, fixed_chunks, 2 as uint)
    let res4 = runOrdinalFloatReduction(data, fixed_chunks, 4 as uint)
    let res8 = runOrdinalFloatReduction(data, fixed_chunks, 8 as uint)

    if res1.val != res2.val {
        unsafe { free(raw as ptr[u8]) }
        return 1
    }
    if res2.val != res4.val {
        unsafe { free(raw as ptr[u8]) }
        return 2
    }
    if res4.val != res8.val {
        unsafe { free(raw as ptr[u8]) }
        return 3
    }

    unsafe { free(raw as ptr[u8]) }
    return 0
}
"#;
    let (amir, tc) = compile_src_mono(src);
    let c_res = execute_c("parallel_float_determinism", &amir, &tc);
    assert_eq!(c_res, 0, "C backend failed float determinism test");
    let clif_res = execute_cranelift(&amir, &tc);
    assert_eq!(
        clif_res, 0,
        "Cranelift backend failed float determinism test"
    );
}

#[test]
fn parity_pointer_tag_enum() {
    let src = r#"
    enum Node {
        Leaf(ref int)
        Branch(ref int)
        Empty
    }

    func eval_node(n: Node): int {
        match n {
            Node.Leaf(r) => { return *r; }
            Node.Branch(r) => { return *r * 2; }
            Node.Empty => { return 0; }
        }
    }

    func main(): int {
        let x: int = 15
        let y: int = 25
        let n1: Node = Node.Leaf(ref x)
        let n2: Node = Node.Branch(ref y)
        let n3: Node = Node.Empty
        let r1 = eval_node(n1)
        let r2 = eval_node(n2)
        let r3 = eval_node(n3)
        if r1 != 15 { return 1; }
        if r2 != 50 { return 2; }
        if r3 != 0 { return 3; }
        return 0;
    }
    "#;
    test_execution_parity("pointer_tag_enum", src);
}

#[test]
fn c_backend_emits_native_trap_abort_model() {
    let (amir, tc) = compile_src(
        r#"
        module std.core.abort_parity

        extern "arandu-intrinsic" {
            func abort(): void
        }

        func safe_or_abort(x: int): int {
            if x < 0 {
                unsafe {
                    abort();
                }
            }
            return x * 2;
        }

        func main(): int {
            return safe_or_abort(21);
        }
        "#,
    );
    let emitted = emit_c(&amir, &tc);
    assert!(emitted.contains("#define AR_ABORT() __builtin_trap()"));
    assert!(emitted.contains("#define AR_UNREACHABLE() __builtin_trap()"));
    assert!(emitted.contains("AR_ABORT();"));

    test_execution_parity(
        "abort_parity_safe_path",
        r#"
        module std.core.abort_parity

        extern "arandu-intrinsic" {
            func abort(): void
        }

        func safe_or_abort(x: int): int {
            if x < 0 {
                unsafe {
                    abort();
                }
            }
            return x * 2;
        }

        func main(): int {
            return safe_or_abort(21);
        }
        "#,
    );
}

#[test]
fn parity_user_defined_alloc_and_free() {
    test_execution_parity(
        "parity_user_defined_alloc_and_free",
        r#"
        func alloc(size: int): int {
            return size * 3
        }

        func free(value: int): int {
            return value + 5
        }

        func main(): int {
            let a = alloc(10)
            let b = free(20)
            return a + b
        }
        "#,
    );
}

#[test]
fn parity_user_defined_generic_option() {
    test_execution_parity_mono(
        "parity_user_defined_generic_option",
        r#"
        enum Option<T> {
            Some(T),
            None,
        }

        func unwrap_or(opt: Option<int>, default_val: int): int {
            return match opt {
                Option.Some(v) => v
                Option.None => default_val
            }
        }

        func wrap<T>(x: T): Option<T> {
            return Option.Some(x)
        }

        func main(): int {
            let a = Option.Some(42)
            let b: Option<int> = Option.None
            let c = wrap(15)
            return unwrap_or(a, 0) + unwrap_or(b, 8) + unwrap_or(c, 0)
        }
        "#,
    );
}

#[test]
fn parity_integer_interpolation_extremes_nested_and_repeated_in_loops() {
    test_zero_result_all_opt_levels(
        "integer_interpolation_fusion",
        r#"
struct Counter { value: int }
func next(counter: mut ref Counter): int {
    counter.value = counter.value + 1
    return counter.value
}
func main(): int {
    let lo: i64 = -9223372036854775808
    let hi: u64 = 18446744073709551615
    let small: i8 = -128
    let unsignedSmall: u8 = 255
    let empty = ""
    let text = "${empty}${lo}|${hi}|${small}|${unsignedSmall}|${0}"
    if text != "-9223372036854775808|18446744073709551615|-128|255|0" { return 1 }
    let independentlyUsed = lo.to_str()
    if independentlyUsed != "-9223372036854775808" { return 5 }
    if "${independentlyUsed}/${independentlyUsed}" != "-9223372036854775808/-9223372036854775808" { return 6 }
    if "\0:${small}:é" != "\0:-128:é" { return 7 }
    let nested = "before:${"v=${small}"}:after"
    if nested != "before:v=-128:after" { return 2 }
    let mut counter = Counter { value: 0 }
    let ordered = "${next(mut ref counter)}:${next(mut ref counter)}:${counter.value}"
    if ordered != "1:2:2" { return 3 }
    let mut i: int = 0
    while i < 100 {
        let repeated = "${i}:${i}"
        let first = "${i}"
        if repeated != "${first}:${first}" { return 4 }
        i = i + 1
    }
    return 0
}
"#,
    );
}

#[test]
fn integer_concat_admission_rejects_escape_duplicate_use_and_jump_arguments() {
    use arandu_codegen::string_interp::integer_concat_temps;
    use arandu_middle::amir::AmirTerminator;
    let (program, tc) = compile_src(
        r#"
func main(): int {
    let n: int = 42
    let s = "n=${n}"
    return 0
}
"#,
    );
    let function = &program.funcs[0];
    let plan = integer_concat_temps(function, &tc.type_info.type_interner);
    let temp = plan
        .iter()
        .position(Option::is_some)
        .expect("integer part admitted");
    let temp_id = arandu_middle::amir::TempId::from_usize(temp);
    let check_rejected = |function: &arandu_middle::amir::AmirFunc| {
        assert_eq!(
            integer_concat_temps(function, &tc.type_info.type_interner)[temp],
            None
        );
    };
    let concat_id = function
        .stmts
        .payloads
        .iter()
        .enumerate()
        .find_map(|(id, statement)| {
            matches!(
                statement,
                AmirStmt::Assign {
                    rhs: AmirRvalue::StringInterp { .. },
                    ..
                }
            )
            .then_some(id)
        })
        .unwrap();
    let free_id = function.stmts.payloads.iter().enumerate().find_map(|(id, statement)| {
        matches!(statement, AmirStmt::Free(AmirOperand::Copy(t) | AmirOperand::Move(t)) if *t == temp_id).then_some(id)
    }).unwrap();

    let mut reused = function.clone();
    if let AmirStmt::Assign {
        rhs: AmirRvalue::StringInterp { parts },
        ..
    } = reused.stmt_mut(arandu_middle::amir::InstrId::from_usize(concat_id))
    {
        parts.push(AmirOperand::Copy(temp_id));
    }
    check_rejected(&reused);

    let mut escaped = function.clone();
    *escaped.stmt_mut(arandu_middle::amir::InstrId::from_usize(free_id)) = AmirStmt::Assign {
        lhs: arandu_middle::amir::TempId::from_usize(0),
        rhs: AmirRvalue::Use(AmirOperand::Copy(temp_id)),
    };
    check_rejected(&escaped);

    let mut jump = function.clone();
    jump.blocks[0].terminator = AmirTerminator::Goto {
        target: jump.blocks[0].id,
        args: vec![AmirOperand::Copy(temp_id)],
    };
    check_rejected(&jump);

    let mut cross_block = function.clone();
    let mut second = cross_block.blocks[0].clone();
    let range = second.statements.as_range();
    second.id = arandu_middle::amir::BlockId::from_usize(cross_block.blocks.len());
    second.statements = arandu_middle::DenseRange::new(concat_id, range.end - concat_id);
    cross_block.blocks[0].statements =
        arandu_middle::DenseRange::new(range.start, concat_id - range.start);
    cross_block.blocks[0].terminator = AmirTerminator::Goto {
        target: second.id,
        args: vec![],
    };
    cross_block.blocks.push(second);
    check_rejected(&cross_block);

    let mut no_free = function.clone();
    *no_free.stmt_mut(arandu_middle::amir::InstrId::from_usize(free_id)) = AmirStmt::Nop;
    check_rejected(&no_free);
}

#[test]
fn parity_user_function_with_runtime_helper_name_is_an_ordinary_call() {
    test_zero_result_all_opt_levels(
        "user_copy_value",
        r#"
struct User {}
func User.ar_rt_copy_value(destination: int, source: int, size: int): int {
    return destination + source + size
}
func main(): int {
    return User.ar_rt_copy_value(10, 20, 12) - 42
}
"#,
    );
}

#[test]
fn parity_shorthand_record_pattern_uses_scrutinee_enum_identity() {
    test_zero_result_all_opt_levels(
        "record_pattern_identity",
        r#"
enum First { Item { value: int } }
enum Second { Item { value: int } }
func read(value: Second): int {
    return match value { Item { value: result } => result }
}
func main(): int {
    return read(Second.Item { value: 42 }) - 42
}
"#,
    );
}

#[test]
fn c_backend_array_uses_typed_storage_for_reads_and_writes_under_strict_aliasing() {
    let source = r#"
struct Holder {
    values: [4]u64
}

func mutate_holder(mut h: Holder, idx: usize, delta: u64): u64 {
    h.values[idx] = h.values[idx] + delta
    return h.values[idx]
}

func main(): int {
    let mut arr: [4]u64 = [10, 20, 30, 40]
    let mut i: usize = 0
    while i < 4 {
        arr[i] = arr[i] + (i as u64) * 5
        i = i + 1
    }
    let snapshot = arr
    arr[2] = 999
    if snapshot[2] != 40 { return 1 }
    if arr[0] != 10 || arr[1] != 25 || arr[2] != 999 || arr[3] != 55 { return 2 }

    let mut matrix: [2][2]i64 = [[1, 2], [3, 4]]
    matrix[1][0] = matrix[0][1] + 10
    if matrix[1][0] != 12 { return 3 }

    let h = Holder { values: snapshot }
    if mutate_holder(h, 3, 5) != 60 { return 4 }
    return 0
}
"#;
    let (amir, tc) = compile_src(source);
    let emitted = emit_c(&amir, &tc);

    assert!(
        emitted.contains(
            "typedef struct AR_MAY_ALIAS { _Alignas(8) uint64_t data[4]; } ArType_Array_4_uint64_t;"
        ),
        "expected typed C array storage in typedef, got:\n{emitted}"
    );
    assert!(
        emitted.contains(".data["),
        "expected array indexing through .data member"
    );
    assert!(
        !emitted.contains("((uint64_t*)&"),
        "array indexing must not cast struct address to uint64_t*:\n{emitted}"
    );

    test_zero_result_all_opt_levels("array_typed_storage_strict_aliasing", source);

    let wrapped = format!(
        "#define main arandu_main\n{emitted}\n#undef main\nint main(void) {{ return arandu_main(); }}\n"
    );
    let out_dir = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&out_dir).unwrap();
    let c_file = out_dir.join("array_strict_aliasing_o2.c");
    let exe_file = out_dir.join("array_strict_aliasing_o2.exe");
    fs::write(&c_file, wrapped).unwrap();

    let cc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let compile = c_compiler(&cc)
        .args([
            "-O2",
            "-fstrict-aliasing",
            "-Wstrict-aliasing=2",
            "-Werror=strict-aliasing",
        ])
        .arg(&c_file)
        .arg("-o")
        .arg(&exe_file)
        .args(arandu_middle::TargetSpec::for_host().cc_link_args())
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "C strict-aliasing O2 compilation failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let status = Command::new(&exe_file).status().unwrap();
    assert!(
        status.success(),
        "strict-aliasing O2 binary exited with {status}"
    );
}

#[test]
fn cyclic_phi_swaps_match_native_parallel_copy_semantics() {
    use arandu_middle::amir::{
        AmirBasicBlock, AmirFunc, AmirStmtTable, AmirTemp, AmirTerminator, BlockId, BlockParam,
        LocalId, TempId,
    };
    use arandu_middle::cfg::compute_cfg_edges;
    use arandu_middle::layout::DenseRange;
    use arandu_middle::literal_pool::AmirLiteralPool;
    use arandu_middle::types::{ArType, Primitive};
    let temp = |id, ty| AmirTemp {
        id: TempId::from_usize(id),
        ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_middle::Span::new(0, 0, 0),
    };
    for strings in [false, true] {
        let (mut amir, tc) = compile_src("func main(): int { return 0 }");
        let interner = &tc.type_info.type_interner;
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let boolean = interner.intern(ArType::Primitive(Primitive::Bool));
        let value = if strings {
            interner.intern(ArType::Primitive(Primitive::Str))
        } else {
            int
        };
        let mut pool = AmirLiteralPool::default();
        let zero = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("0")));
        let one = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("1")));
        let left = AmirOperand::Constant(AmirConstant::Pool(if strings {
            pool.intern_str("a")
        } else {
            pool.intern_int("11")
        }));
        let right = AmirOperand::Constant(AmirConstant::Pool(if strings {
            pool.intern_str("long")
        } else {
            pool.intern_int("22")
        }));
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(4),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Lt,
                left: AmirOperand::Copy(TempId::from_usize(3)),
                right: one,
            },
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: if strings {
                AmirRvalue::Len(AmirOperand::Copy(TempId::from_usize(2)))
            } else {
                AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(2)))
            },
        });
        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![left, right, zero],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                params: DenseRange::new(0, 3),
                statements: DenseRange::new(0, 1),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(4)),
                    if_true: BlockId::from_usize(2),
                    true_args: vec![],
                    if_false: BlockId::from_usize(3),
                    false_args: vec![],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![
                        AmirOperand::Copy(TempId::from_usize(2)),
                        AmirOperand::Copy(TempId::from_usize(1)),
                        one,
                    ],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                params: DenseRange::empty(),
                statements: DenseRange::new(1, 1),
                terminator: AmirTerminator::Return,
            },
        ];
        let func = AmirFunc {
            symbol: amir.funcs[0].symbol,
            return_type: int,
            receiver: None,
            params: vec![],
            locals: vec![],
            temps: vec![
                temp(0, int),
                temp(1, value),
                temp(2, value),
                temp(3, int),
                temp(4, boolean),
            ],
            block_params: (0..3)
                .map(|index| BlockParam {
                    id: TempId::from_usize(index + 1),
                    local: LocalId::from_usize(index),
                    ty: if index == 2 { int } else { value },
                    from: None,
                    moved: false,
                })
                .collect(),
            cfg: compute_cfg_edges(&blocks),
            blocks,
            stmts,
        };
        amir.funcs[0] = func;
        amir.literal_pool = pool;
        let expected = if strings { 1 } else { 11 };
        assert_eq!(execute_cranelift(&amir, &tc), expected);
        assert_eq!(
            execute_c(
                if strings {
                    "phi_string_swap"
                } else {
                    "phi_scalar_swap"
                },
                &amir,
                &tc
            ),
            expected
        );
    }
}

#[test]
fn c_literal_io_keeps_length_aware_helpers_and_flushes() {
    let (amir, tc) = compile_src(
        r#"
import io

func main(): int {
    let message = "hello\0 literal"
    let alias = message
    io.println(alias)
    io.print("freestanding io")
    io.eprint("stderr direct\n")
    io.println("")
    return 0
}
"#,
    );
    let emitted = emit_c(&amir, &tc);
    for helper in [
        "static void io__println",
        "static void io__print",
        "static void io__eprint",
    ] {
        assert!(
            emitted.contains(helper),
            "missing length-aware I/O helper {helper}"
        );
    }
    assert!(emitted.contains("fflush(stdout)") && emitted.contains("fflush(stderr)"));
    let (status, output) = execute_c_output("static_println_zero_alloc", &amir, &tc);
    assert_eq!(status, 0);
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
    let expected = format!("hello\0 literal{newline}freestanding io{newline}0{newline}");
    assert_eq!(output.as_bytes(), expected.as_bytes());
}

#[test]
fn c_loop_cfg_preserves_explicit_edges() {
    let (amir, tc) = compile_src(
        r#"
func main(): int {
    let mut i = 0
    let mut sum = 0
    while i < 10 {
        sum = sum + i
        i = i + 1
    }
    return sum
}
"#,
    );
    let emitted = emit_c(&amir, &tc);
    assert!(
        emitted.contains("goto bb") && !emitted.contains("while (1) {"),
        "CFG edges must remain explicit until loop reconstruction is proven: {emitted}"
    );
    assert_eq!(execute_cranelift(&amir, &tc), 45);
    assert_eq!(execute_c("structured_while_loop", &amir, &tc), 45);
}

#[test]
fn c_user_print_names_preserve_return_values_and_side_effects() {
    let (amir, tc) = compile_src(
        r#"
import io
struct User {}
func print(value: str): int { io.print(value) return 7 }
func User.println(value: str): int { io.print(value) return 9 }
func main(): int {
    let left = print("left")
    let right = User.println("right")
    io.println("")
    return left + right
}
"#,
    );
    let emitted = emit_c(&amir, &tc);
    assert!(emitted.contains("= print(") && emitted.contains("= User__println("));
    assert_eq!(execute_cranelift(&amir, &tc), 16);
    let (result, output) = execute_c_output("user_print_returns_and_effects", &amir, &tc);
    assert_eq!(result, 16);
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
    assert_eq!(output, format!("leftright{newline}16{newline}"));
}

#[test]
fn c_latch_exit_and_noncontiguous_exit_follow_the_cfg() {
    use arandu_middle::amir::{
        AmirBasicBlock, AmirFunc, AmirStmtTable, AmirTemp, AmirTerminator, BlockId, BlockParam,
        LocalId, TempId,
    };
    use arandu_middle::cfg::compute_cfg_edges;
    use arandu_middle::layout::DenseRange;
    use arandu_middle::types::{ArType, Primitive};
    for early_exit in [false, true] {
        let (mut program, tc) = compile_src("func main(): int { return 0 }");
        let int = tc
            .type_info
            .type_interner
            .intern(ArType::Primitive(Primitive::Int));
        let boolean = tc
            .type_info
            .type_interner
            .intern(ArType::Primitive(Primitive::Bool));
        let mut pool = arandu_middle::literal_pool::AmirLiteralPool::default();
        let zero = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("0")));
        let one = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("1")));
        let guard = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int(if early_exit {
            "1"
        } else {
            "6"
        })));
        let error = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("99")));
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::Add,
                left: AmirOperand::Copy(TempId::from_usize(1)),
                right: one,
            },
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(3),
            rhs: AmirRvalue::Binary {
                op: BinaryOp::GtEqual,
                left: AmirOperand::Copy(TempId::from_usize(2)),
                right: guard,
            },
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(2))),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(error),
        });
        let block = |id, range, terminator| AmirBasicBlock {
            id: BlockId::from_usize(id),
            params: if id == 1 {
                DenseRange::new(0, 1)
            } else {
                DenseRange::empty()
            },
            statements: range,
            terminator,
        };
        let blocks = vec![
            block(
                0,
                DenseRange::empty(),
                AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![zero],
                },
            ),
            block(
                1,
                DenseRange::new(0, 2),
                AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(3)),
                    if_true: BlockId::from_usize(4),
                    true_args: vec![],
                    if_false: BlockId::from_usize(2),
                    false_args: vec![],
                },
            ),
            block(
                2,
                DenseRange::empty(),
                AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(false)),
                    if_true: BlockId::from_usize(1),
                    true_args: vec![AmirOperand::Copy(TempId::from_usize(2))],
                    if_false: BlockId::from_usize(3),
                    false_args: vec![],
                },
            ),
            block(3, DenseRange::new(2, 1), AmirTerminator::Return),
            block(4, DenseRange::new(3, 1), AmirTerminator::Return),
        ];
        program.funcs[0] = AmirFunc {
            symbol: program.funcs[0].symbol,
            return_type: int,
            receiver: None,
            params: vec![],
            locals: vec![],
            temps: (0..4)
                .map(|id| AmirTemp {
                    id: TempId::from_usize(id),
                    ty: if id == 3 { boolean } else { int },
                    is_copy: true,
                    is_nullable: false,
                    span: arandu_middle::Span::new(0, 0, 0),
                })
                .collect(),
            block_params: vec![BlockParam {
                id: TempId::from_usize(1),
                local: LocalId::from_usize(0),
                ty: int,
                from: None,
                moved: false,
            }],
            cfg: compute_cfg_edges(&blocks),
            blocks,
            stmts,
        };
        program.literal_pool = pool;
        let expected = if early_exit { 99 } else { 1 };
        assert_eq!(execute_cranelift(&program, &tc), expected);
        assert_eq!(
            execute_c(
                if early_exit {
                    "loop_noncontiguous_exit"
                } else {
                    "loop_latch_fallthrough_exit"
                },
                &program,
                &tc
            ),
            expected
        );
    }
}

#[test]
fn c_adjacent_true_target_with_arguments_keeps_its_label() {
    use arandu_middle::amir::{
        AmirBasicBlock, AmirStmtTable, AmirTemp, AmirTerminator, BlockId, BlockParam, LocalId,
        TempId,
    };
    use arandu_middle::cfg::compute_cfg_edges;
    use arandu_middle::layout::DenseRange;
    let (mut program, tc) = compile_src("func main(): int { return 0 }");
    let function = &mut program.funcs[0];
    let int = function.return_type;
    let mut pool = arandu_middle::literal_pool::AmirLiteralPool::default();
    let seven = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("7")));
    let nine = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("9")));
    let mut stmts = AmirStmtTable::new();
    for id in [1, 2] {
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(id))),
        });
    }
    let blocks = vec![
        AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Branch {
                condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                if_true: BlockId::from_usize(1),
                true_args: vec![seven],
                if_false: BlockId::from_usize(2),
                false_args: vec![nine],
            },
        },
        AmirBasicBlock {
            id: BlockId::from_usize(1),
            params: DenseRange::new(0, 1),
            statements: DenseRange::new(0, 1),
            terminator: AmirTerminator::Return,
        },
        AmirBasicBlock {
            id: BlockId::from_usize(2),
            params: DenseRange::new(1, 1),
            statements: DenseRange::new(1, 1),
            terminator: AmirTerminator::Return,
        },
    ];
    function.temps = (0..3)
        .map(|id| AmirTemp {
            id: TempId::from_usize(id),
            ty: int,
            is_copy: true,
            is_nullable: false,
            span: arandu_middle::Span::new(0, 0, 0),
        })
        .collect();
    function.block_params = (0..2)
        .map(|index| BlockParam {
            id: TempId::from_usize(index + 1),
            local: LocalId::from_usize(index),
            ty: int,
            from: None,
            moved: false,
        })
        .collect();
    function.cfg = compute_cfg_edges(&blocks);
    function.blocks = blocks;
    function.stmts = stmts;
    program.literal_pool = pool;
    let emitted = emit_c(&program, &tc);
    assert!(emitted.contains("bb1:") && emitted.contains("goto bb1;"));
    assert_eq!(execute_cranelift(&program, &tc), 7);
    assert_eq!(
        execute_c("adjacent_true_target_arguments", &program, &tc),
        7
    );
}

#[test]
fn parity_borrowed_projections_preserve_pointer_loads_and_single_evaluation() {
    test_zero_result_all_opt_levels(
        "borrowed_projections",
        include_str!("../../../tests/corpus/borrowed_projections.aru"),
    );
}

#[test]
fn parity_borrowed_string_equality_compares_contents_in_both_operand_orders() {
    test_zero_result_all_opt_levels(
        "borrowed_string_equality",
        include_str!("../../../tests/corpus/borrowed_string_equality.aru"),
    );
}

#[test]
#[cfg(unix)]
fn generated_c_parking_lot_preserves_instance_wakes_tokens_and_thread_lifetimes() {
    let (amir, tc) = compile_src(
        "extern \"C\" { func ar_rt_condvar_wait_timeout(key: ptr[u8], seq: usize, ms: u64): i32 }\npublic func probe(key: ptr[u8]): i32 { return unsafe { ar_rt_condvar_wait_timeout(key, 0 as usize, 0 as u64) } }\nfunc main(): int { return 0 }",
    );
    let source = format!(
        "#define main arandu_unused_main\n{}\n#undef main\n{}",
        emit_c(&amir, &tc),
        include_str!("parking_lot_harness.c"),
    );
    let directory = env::temp_dir().join(format!("arandu-parking-c-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("parking.c");
    fs::write(&path, source).unwrap();
    let compiler = env::var("CC").unwrap_or_else(|_| "cc".into());
    for optimization in ["-O0", "-O2"] {
        let executable = directory.join("parking");
        let output = Command::new(&compiler)
            .args([
                optimization,
                "-pthread",
                "-fsanitize=undefined",
                "-fno-sanitize-recover=all",
            ])
            .arg(&path)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(executable).output().unwrap();
        assert!(
            output.status.success(),
            "{:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn jit_condvar_timeout_bridge_uses_pointer_sized_sequence_and_u64_duration() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (amir, tc) = compile_src(
        r#"
extern "C" {
    func ar_rt_condvar_wait_timeout(key: ptr[u8], seq: usize, ms: u64): i32
    func ar_rt_condvar_notify_one(key: ptr[u8]): void
}
public func timedWait(key: ptr[u8], seq: usize, ms: u64): i32 {
    return unsafe { ar_rt_condvar_wait_timeout(key, seq, ms) }
}
public func notify(key: ptr[u8]): void { unsafe { ar_rt_condvar_notify_one(key) } }
func main(): int { return 0 }
"#,
    );
    let module = CraneliftBackend::try_new()
        .unwrap()
        .compile(&amir, &tc.symbols, &tc.type_info)
        .unwrap();
    // SAFETY: the declarations above specify these exact ABI signatures; the
    // compiled module and aligned atomic sequence outlive every call/worker.
    let wait = unsafe {
        module
            .get_fn::<unsafe extern "C" fn(*const u8, usize, u64) -> i32>("timedWait")
            .unwrap()
    };
    let notify = unsafe {
        module
            .get_fn::<unsafe extern "C" fn(*const u8)>("notify")
            .unwrap()
    };
    let sequence = AtomicUsize::new(0);
    let key = (&sequence as *const AtomicUsize).cast::<u8>();
    // SAFETY: all accesses use the stable atomic word; no concurrent mutation
    // exists for the timeout and mismatch cases.
    unsafe {
        assert_eq!(wait(key, 1, u64::MAX), 1);
        assert_eq!(wait(key, 0, 0), 0);
    }
    std::thread::scope(|scope| {
        let sequence = &sequence;
        let worker = scope.spawn(move || {
            // SAFETY: scope retains the atomic allocation, and no non-atomic
            // access to it is performed while the JIT helper may read it.
            unsafe { wait((sequence as *const AtomicUsize).cast(), 0, 5_000) }
        });
        sequence.fetch_add(1, Ordering::Release);
        // SAFETY: notify follows the atomic sequence increment and shares its
        // live allocation; it is valid before or after waiter registration.
        unsafe {
            notify(key);
        }
        assert_eq!(worker.join().unwrap(), 1);
    });
}

#[test]
#[cfg(unix)]
fn generated_c_phase_lock_alternates_batches_preserves_fifo_and_cleans_cancellation() {
    let (amir, tc) = compile_src(
        "extern \"C\" { func ar_rt_rwlock_try_write(key: ptr[u8]): i32 }\npublic func probe(key: ptr[u8]): i32 { return unsafe { ar_rt_rwlock_try_write(key) } }\nfunc main(): int { return 0 }",
    );
    let source = format!(
        "#define main arandu_unused_main\n{}\n#undef main\n{}",
        emit_c(&amir, &tc),
        include_str!("phase_lock_harness.c"),
    );
    let directory = env::temp_dir().join(format!("arandu-phase-lock-c-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("phase_lock.c");
    fs::write(&path, source).unwrap();
    let compiler = env::var("CC").unwrap_or_else(|_| "cc".into());
    for optimization in ["-O0", "-O2"] {
        let executable = directory.join("phase_lock");
        let output = Command::new(&compiler)
            .args([
                optimization,
                "-pthread",
                "-fsanitize=undefined",
                "-fno-sanitize-recover=all",
            ])
            .arg(&path)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(executable).output().unwrap();
        assert!(
            output.status.success(),
            "{:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn generated_c_runtime_memory_helpers_are_clean_under_address_and_ub_sanitizers() {
    let (amir, tc) = compile_src(
        r#"
extern "C" {
    func ar_gen_shutdown_raw(): void
    func ar_str_split_last(s: str, sep: str): str
    func ar_rt_spawn_i64(state: ptr[u8]): i64
}
public func probe(s: str, state: ptr[u8], x: float): str {
    unsafe { ar_gen_shutdown_raw() }
    let spawned = unsafe { ar_rt_spawn_i64(state) }
    return unsafe { ar_str_split_last(s, "${x}${spawned}") }
}
func main(): int { return 0 }
"#,
    );
    let source = format!(
        "#define main arandu_unused_main\n{}\n#undef main\n{}",
        emit_c(&amir, &tc),
        include_str!("runtime_memory_harness.c"),
    );
    let directory = env::temp_dir().join(format!("arandu-runtime-memory-c-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("runtime_memory.c");
    fs::write(&path, source).unwrap();
    let compiler = env::var("CC").unwrap_or_else(|_| "cc".into());
    for optimization in ["-O0", "-O2"] {
        let executable = directory.join("runtime_memory");
        let output = Command::new(&compiler)
            .args([
                optimization,
                "-g",
                "-pthread",
                "-fsanitize=address,undefined,float-cast-overflow",
                "-fno-sanitize-recover=all",
                "-fno-omit-frame-pointer",
            ])
            .arg(&path)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(executable)
            .env("ASAN_OPTIONS", "detect_leaks=1:abort_on_error=1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{optimization} {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn path_only_programs_include_the_variadic_concat_header() {
    // The path group emits the concat runtime (variadic `ar_str_concat_n`)
    // without any interpolation in the program itself.
    let (amir, tc) = compile_src(
        r#"
extern "C" { func ar_path_join(a: str, b: str): str }
func main(): int {
    let joined = unsafe { ar_path_join("a", "b") }
    if joined == "a/b" { return 0 }
    return 1
}
"#,
    );
    let c = emit_c(&amir, &tc);
    assert!(c.contains("ar_str_concat_n") && c.contains("#include <stdarg.h>"));
    assert_eq!(execute_c("path_only_concat_header", &amir, &tc), 0);
}
