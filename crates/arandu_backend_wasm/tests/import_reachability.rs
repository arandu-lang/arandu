//! Host requirements follow executable roots in every emission profile.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

fn imports(bytes: &[u8]) -> Vec<(String, String)> {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).unwrap();
    module
        .imports()
        .map(|import| (import.module().to_owned(), import.name().to_owned()))
        .collect()
}

#[test]
fn default_core_prunes_private_call_chains_and_their_host_requirements() {
    let source = r#"
extern "C" { func missing_host(): int }
func dead_leaf(): int { unsafe { return missing_host() } }
func dead_root(): int { return dead_leaf() }
func main(): int { return 42 }
"#;
    let bytes = common::compile_source(source);
    assert!(imports(&bytes).is_empty());
    assert!(common::find_func_export(&bytes, "dead_root").is_none());
    assert!(common::find_func_export(&bytes, "dead_leaf").is_none());
    assert!(common::find_func_export(&bytes, "cabi_realloc").is_some());
    assert_eq!(common::run_main_i32(&bytes), 42);
    assert_eq!(bytes, common::compile_source(source));
}

#[test]
fn default_core_keeps_transitive_calls_and_public_entrypoints() {
    let bytes = common::compile_source(
        r#"
extern "C" {
    func live_host(): int
    func public_host(): int
    func unused_host(): int
}
func leaf(): int { unsafe { return live_host() } }
func middle(): int { return leaf() }
public func api(): int { unsafe { return public_host() } }
func main(): int { return middle() }
"#,
    );
    assert_eq!(
        imports(&bytes),
        vec![
            ("env".to_owned(), "live_host".to_owned()),
            ("env".to_owned(), "public_host".to_owned()),
        ]
    );
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut linker = wasmtime::Linker::new(&engine);
    linker.func_wrap("env", "live_host", || 42_i32).unwrap();
    linker.func_wrap("env", "public_host", || 7_i32).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module).unwrap();
    for (name, expected) in [("main", 42), ("api", 7)] {
        let export = common::find_func_export(&bytes, name).unwrap();
        let function = instance
            .get_typed_func::<(), i32>(&mut store, &export)
            .unwrap();
        assert_eq!(function.call(&mut store, ()).unwrap(), expected);
    }
}

#[test]
fn library_without_main_preserves_private_host_lookup() {
    let bytes = common::compile_source(
        r#"
extern "C" { func required_host(): int func unused_host(): int }
func private_api(): int { unsafe { return required_host() } }
"#,
    );
    assert_eq!(
        imports(&bytes),
        vec![("env".to_owned(), "required_host".to_owned())]
    );
    assert!(common::find_func_export(&bytes, "private_api").is_some());
}

#[test]
fn default_component_prunes_dead_import_metadata_and_preserves_string_abi() {
    let bytes = common::compile_source_component(
        r#"
extern "C" { func missing_host(): int }
func dead(): int { unsafe { return missing_host() } }
public func greeting(): str { return "Olá" }
func main(): int { return 0 }
"#,
        "reachability-test",
    );
    assert!(arandu_backend_wasm::is_component(&bytes));
    wasmparser::Validator::new().validate_all(&bytes).unwrap();
    assert!(!bytes.windows(12).any(|part| part == b"missing-host"));
    assert!(common::find_func_export(&bytes, "cabi_realloc").is_some());
    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes).unwrap();
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component).unwrap();
    let interface = instance
        .get_export_index(&mut store, None, "arandu:reachability-test/exports@0.1.0")
        .unwrap();
    let index = instance
        .get_export_index(&mut store, Some(&interface), "greeting")
        .unwrap();
    let greeting = instance
        .get_typed_func::<(), (wasmtime::component::WasmStr,)>(&mut store, &index)
        .unwrap();
    let (text,) = greeting.call(&mut store, ()).unwrap();
    assert_eq!(text.to_str(&mut store).unwrap(), "Olá");
}

#[test]
fn freestanding_alloc_and_free_require_no_host_imports_and_execute_in_wasmtime() {
    let source = r#"
module std.core.allocFixture
extern "C" {
    func ar_rt_alloc_aligned(size: usize, align: usize): ptr[u8]
    func ar_rt_free_aligned(data: ptr[u8], size: usize, align: usize): void
}
extern "arandu-intrinsic" {
    func ptrRead<T>(p: ptr[T]): T
    func ptrWrite<T>(p: ptr[T], val: T): void
}

public func main(): int {
    let size: usize = 16 as usize
    let align: usize = 8 as usize
    let nullp: ptr[u8] = nil
    let p = unsafe { ar_rt_alloc_aligned(size, align) }
    if p == nullp { return 1 }
    unsafe {
        ptrWrite<u8>(p, 42 as u8)
        let val = ptrRead<u8>(p)
        if val != (42 as u8) { return 2 }
        ar_rt_free_aligned(p, size, align)
    }
    return 0
}
"#;
    let bytes = common::compile_trusted_core_fixture(source);
    assert!(
        imports(&bytes).is_empty(),
        "alloc and free must be freestanding internal wasm calls: {:?}",
        imports(&bytes)
    );
    assert_eq!(common::run_main_i32(&bytes), 0);
}

#[test]
fn over_aligned_raw_allocations_fail_closed_instead_of_under_aligning() {
    // Cells guarantee 16-byte alignment. A larger request reports allocation
    // failure (null) rather than returning a silently misaligned pointer.
    let source = r#"
module std.core.alignFixture
extern "C" {
    func ar_rt_alloc_aligned(size: usize, align: usize): ptr[u8]
    func ar_rt_free_aligned(data: ptr[u8], size: usize, align: usize): void
}

public func main(): int {
    let nullp: ptr[u8] = nil
    let wide = unsafe { ar_rt_alloc_aligned(64 as usize, 64 as usize) }
    if wide != nullp { return 1 }
    let p = unsafe { ar_rt_alloc_aligned(64 as usize, 16 as usize) }
    if p == nullp { return 2 }
    unsafe { ar_rt_free_aligned(p, 64 as usize, 16 as usize) }
    return 0
}
"#;
    let bytes = common::compile_trusted_core_fixture(source);
    assert_eq!(common::run_main_i32(&bytes), 0);
}
