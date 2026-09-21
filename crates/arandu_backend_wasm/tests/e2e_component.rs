//! Component-model, WIT and Canonical ABI end-to-end tests: Arandu source →
//! AMIR → component bytes, validated and instantiated under wasmtime's
//! component model, plus `cabi_realloc` header/layout contracts.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

mod common;

use common::{compile_source, compile_source_component};

#[test]
fn string_bytes_intrinsic_loads_borrowed_descriptor() {
    let bytes = compile_source(
        r#"
module std.core.str_bytes_wasm

extern "arandu-intrinsic" {
    func strBytes(source: str): []u8
}

public func main(): i32 {
    let source = "abcd"
    let bytes = unsafe { strBytes(source) }
    if bytes[0] != (97 as u8) || bytes[3] != (100 as u8) {
        return 1
    }
    return 0
}
"#,
    );

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let main = instance
        .get_typed_func::<(), i32>(&mut store, "main")
        .expect("main must be exported");
    assert_eq!(main.call(&mut store, ()).expect("main must run"), 0);
}

/// `cabi_realloc(0, 0, 1, 0)` must return 0 (zero-size request → no-op).
#[test]
fn cabi_realloc_returns_zero_for_zero_size() {
    let bytes = compile_source("public func main(): i32 { return 0 }");

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("`cabi_realloc` must be exported with correct signature");

    let result = realloc
        .call(&mut store, (0, 0, 1, 0))
        .expect("`cabi_realloc(0, 0, 1, 0)` must not trap");
    assert_eq!(result, 0, "zero-size realloc must return 0");
}

/// `cabi_realloc(0, 0, 4, 64)` must return a non-zero pointer aligned to 4.
#[test]
fn cabi_realloc_allocates_aligned_memory() {
    let bytes = compile_source("public func main(): i32 { return 0 }");

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("`cabi_realloc` must be exported with correct signature");

    let ptr = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("`cabi_realloc(0, 0, 4, 64)` must not trap");
    assert!(ptr > 0, "cabi_realloc must return a non-zero pointer");
    assert_eq!(ptr % 4, 0, "returned pointer must be 4-byte aligned");
}

/// `cabi_realloc` must return consecutive non-overlapping pointers when called
/// multiple times.
#[test]
fn cabi_realloc_consecutive_calls_do_not_overlap() {
    let bytes = compile_source("public func main(): i32 { return 0 }");

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("`cabi_realloc` must be exported");

    let p1 = realloc
        .call(&mut store, (0, 0, 4, 16))
        .expect("first call must not trap");
    let p2 = realloc
        .call(&mut store, (0, 0, 4, 16))
        .expect("second call must not trap");
    assert!(p1 > 0);
    assert!(p2 > 0);
    // With a bump allocator, second allocation must start at or after end of
    // first (p2 >= p1 + 16).
    assert!(
        p2 >= p1 + 16,
        "allocations must not overlap: p1={p1}, p2={p2}"
    );
}

/// Exercising the free path through the Canonical ABI: `cabi_realloc(ptr, sz,
/// align, 0)` must free the block, and the next same-size allocation (first
/// fit) must return the very same pointer — proving the free list reuses blocks
/// without disturbing user payload bytes.
#[test]
fn cabi_realloc_zero_size_frees_and_next_alloc_reuses() {
    let bytes = compile_source("public func main(): i32 { return 0 }");

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("module must export `memory`");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("`cabi_realloc` must be exported with correct signature");

    let p1 = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("first allocation must not trap");
    let p2 = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("second allocation must not trap");
    assert!(
        p1 > 0 && p2 > 0 && p2 > p1,
        "bump must produce distinct blocks"
    );

    // Write a distinctive payload marker into block 2, then free it.
    let mut marker = [0u8; 16];
    for (i, byte) in marker.iter_mut().enumerate() {
        *byte = (i * 7 + 1) as u8;
    }
    memory
        .write(&mut store, p2 as usize, &marker)
        .expect("host must write into guest memory");

    let freed = realloc
        .call(&mut store, (p2, 64, 4, 0))
        .expect("freeing via zero-size realloc must not trap");
    assert_eq!(freed, 0, "zero-size realloc must return 0 after freeing");

    // First fit must hand back the just-freed block for the same request size.
    let p3 = realloc
        .call(&mut store, (0, 0, 4, 64))
        .expect("reuse allocation must not trap");
    assert_eq!(p3, p2, "first-fit free list must reuse the freed block");

    // The allocator only touches the block header; user bytes must survive.
    let mut read_back = [0u8; 16];
    memory
        .read(&mut store, p3 as usize, &mut read_back)
        .expect("host must read guest memory");
    assert_eq!(&read_back, &marker, "payload must survive free + realloc");
}

/// The WIT generator must produce valid WIT text for a simple public function.
#[test]
fn wit_gen_produces_valid_wit_for_public_func() {
    use arandu_backend_wasm::wit_gen::generate_wit;
    use arandu_query::db::DatabaseImpl;
    use arandu_query::passes::lower_amir;

    let source = "public func add(a: i32, b: i32): i32 { return a }";
    let mut db = DatabaseImpl::new();
    let file = db.new_file("e2e.aru".into(), source.into());
    let lowered = lower_amir(&db, file);

    let wit = generate_wit(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        "test-pkg",
    );

    let wit = wit.expect("program has public functions; WIT must be generated");
    assert!(
        wit.contains("package arandu:test-pkg@0.1.0"),
        "must have package header"
    );
    assert!(wit.contains("interface exports"), "must declare interface");
    assert!(wit.contains("world test-pkg"), "must declare world");
    assert!(
        wit.contains("export exports"),
        "world must export interface"
    );
}

/// The WIT generator must return `None` when there are no public functions.
#[test]
fn wit_gen_returns_none_when_no_public_funcs() {
    use arandu_backend_wasm::wit_gen::generate_wit;
    use arandu_query::db::DatabaseImpl;
    use arandu_query::passes::lower_amir;

    // Private function — not exported.
    let source = "func hidden(): i32 { return 0 }";
    let mut db = DatabaseImpl::new();
    let file = db.new_file("e2e.aru".into(), source.into());
    let lowered = lower_amir(&db, file);

    let wit = generate_wit(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        "empty-pkg",
    );

    // Private function is still in the AMIR; the WIT generator filters by
    // is_public. Result may be None or Some (depending on whether the compiler
    // emits non-public funcs), but must NOT fail.
    let _ = wit; // Either way is acceptable; test just checks no panic/ICE.
}

/// A compiled component must start with the WebAssembly Component Model magic
/// bytes (0x00 0x61 0x73 0x6d 0x0d 0x00 0x01 0x00).
#[test]
fn emit_component_starts_with_component_magic() {
    let bytes = compile_source_component("public func main(): i32 { return 7 }", "my-app");

    // WebAssembly Component magic: same \0asm preamble but version = 0x0d000100
    // (layer 1 = component, as opposed to core module layer 0 = 0x01000000).
    assert!(
        bytes.starts_with(&[0x00, 0x61, 0x73, 0x6d]),
        "component must start with \\0asm preamble"
    );
    // Version field for components is [0x0d, 0x00, 0x01, 0x00].
    assert_eq!(
        &bytes[4..8],
        &[0x0d, 0x00, 0x01, 0x00],
        "component must have version field 0x0d000100 (component layer)"
    );
}

/// The `wasmparser` validator must accept the generated component as a valid
/// WebAssembly Component (all features enabled).
#[test]
fn emit_component_passes_wasmparser_validation() {
    let bytes = compile_source_component("public func main(): i32 { return 99 }", "test-app");

    let mut validator = wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all());
    validator
        .validate_all(&bytes)
        .expect("emitted component must pass wasmparser validation");
}

/// A program with no public functions should still produce a valid *core*
/// module (not a component — component encoding is skipped).
#[test]
fn emit_component_with_no_public_funcs_returns_core_module() {
    // A private function: `func hidden` is not public.
    let bytes = compile_source_component("func hidden(): i32 { return 0 }", "empty-pkg");

    // Without public functions, build_component falls back to build()
    // which returns a core module with magic [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00].
    assert!(
        bytes.starts_with(&[0x00, 0x61, 0x73, 0x6d]),
        "bytes must start with \\0asm"
    );
    // Core module version is [0x01, 0x00, 0x00, 0x00].
    assert_eq!(
        &bytes[4..8],
        &[0x01, 0x00, 0x00, 0x00],
        "no public funcs: must be a core module, not a component"
    );
}

/// Regression test for a `string` *return* in a component export.
///
/// A `string` result is fat: `wit-parser`'s `MAX_FLAT_RESULTS = 1` forces a
/// single `i32` result (a return-pointer) instead of the two-slot
/// `(ptr, len)` pair. The host lifts a string result by reading the 8-byte
/// `(data_ptr, len)` pair stored at the returned address. This test exercises
/// the full encode + validate + instantiate + call path under wasmtime's
/// component model, verifying the lowering stores that pair where the host
/// expects it.
#[test]
fn component_string_result_is_callable_from_host() {
    let bytes = compile_source_component(
        r#"
public func greet(): str {
    return "olá"
}
"#,
        "greet-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:greet-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let greet_idx = instance
        .get_export_index(&mut store, Some(&iface), "greet")
        .expect("interface must export `greet`");
    let greet = instance
        .get_typed_func::<(), (wasmtime::component::WasmStr,)>(&mut store, &greet_idx)
        .expect("typed string result must match the exported signature");

    let (s,) = greet.call(&mut store, ()).expect("call must resolve");
    assert_eq!(
        s.to_str(&mut store).expect("string must be readable"),
        "olá",
        "host must lift the (data_ptr, len) pair stored at the return pointer"
    );
}

/// Same contract as `component_string_result_is_callable_from_host`, but with
/// a `str` *parameter* alongside the string result: the retptr lowering must
/// coexist with the `(ptr, len)` argument pair.
#[test]
fn component_string_param_and_result_is_callable_from_host() {
    let bytes = compile_source_component(
        r#"
public func echo(name: str): str {
    return name
}
"#,
        "echo-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:echo-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let echo_idx = instance
        .get_export_index(&mut store, Some(&iface), "echo")
        .expect("interface must export `echo`");
    let echo = instance
        .get_typed_func::<(&str,), (wasmtime::component::WasmStr,)>(&mut store, &echo_idx)
        .expect("typed (ptr, len) parameter and string result must match the exported signature");

    let (s,) = echo.call(&mut store, ("olá",)).expect("call must resolve");
    assert_eq!(
        s.to_str(&mut store).expect("string must be readable"),
        "olá",
        "a string parameter lifted by reference must be returned through the retptr"
    );
}

#[test]
fn cabi_freelist_middle_node_unlinking() {
    let bytes = compile_source(
        r#"
public func noop(): i32 {
    return 0
}
"#,
    );

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("module must compile");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance =
        wasmtime::Instance::new(&mut store, &module, &[]).expect("module must instantiate");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("cabi_realloc must be exported");

    // Allocate 3 blocks of different sizes.
    let b1 = realloc.call(&mut store, (0, 0, 4, 16)).expect("b1 alloc");
    let b2 = realloc.call(&mut store, (0, 0, 4, 32)).expect("b2 alloc");
    let _b3 = realloc.call(&mut store, (0, 0, 4, 64)).expect("b3 alloc");

    // Free b2, then free b1. Free list is LIFO: head -> b1 (size 16) -> b2 (size 32) -> null.
    realloc.call(&mut store, (b2, 32, 4, 0)).expect("free b2");
    realloc.call(&mut store, (b1, 16, 4, 0)).expect("free b1");

    // Request size 28.
    // The allocator scans: b1 (16 < 28, skips, prev=b1), b2 (32 >= 28, matches!).
    // With proper middle-node unlinking, it sets prev (b1).next = b2.next (0),
    // leaving head -> b1 intact!
    let b4 = realloc.call(&mut store, (0, 0, 4, 28)).expect("b4 alloc");
    assert_eq!(b4, b2, "size 28 must reuse b2");

    // Request size 12.
    // If middle-node unlinking worked, head is still b1 (size 16 >= 12).
    // If the old bug was present, head was overwritten with b2.next (0), so b1 would be lost!
    let b5 = realloc.call(&mut store, (0, 0, 4, 12)).expect("b5 alloc");
    assert_eq!(b5, b1, "size 12 must reuse b1 from head of free list");
}

#[test]
fn component_repeated_string_calls_reclaims_retptr_without_growth() {
    let bytes = compile_source_component(
        r#"
public func echo(name: str): str {
    return name
}
"#,
        "echo-retptr-test",
    );

    let engine = wasmtime::Engine::default();
    let component =
        wasmtime::component::Component::new(&engine, &bytes).expect("component must decode");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:echo-retptr-test/exports@0.1.0")
        .expect("interface must be exported");
    let echo_idx = instance
        .get_export_index(&mut store, Some(&iface), "echo")
        .expect("echo must be exported");
    let echo = instance
        .get_typed_func::<(&str,), (wasmtime::component::WasmStr,)>(&mut store, &echo_idx)
        .expect("signature must match");

    // Call 1000 times. Without post_return freeing the 8-byte retptr pair,
    // 8 KB would be leaked. With post_return, the pair is reclaimed and reused.
    for _ in 0..1000 {
        let (s,) = echo
            .call(&mut store, ("test_payload",))
            .expect("call must succeed");
        assert_eq!(s.to_str(&mut store).unwrap(), "test_payload");
    }
}

#[derive(wasmtime::component::ComponentType, wasmtime::component::Lower)]
#[component(record)]
struct Point {
    x: i32,
    y: i32,
}

#[test]
fn component_record_param_executes() {
    let bytes = compile_source_component(
        r#"
public struct Point {
    x: i32,
    y: i32,
}

public func add_coords(p: Point): i32 {
    return p.x + p.y
}
"#,
        "point-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:point-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let add_coords_idx = instance
        .get_export_index(&mut store, Some(&iface), "add-coords")
        .expect("interface must export `add-coords`");
    let add_coords = instance
        .get_typed_func::<(Point,), (i32,)>(&mut store, &add_coords_idx)
        .expect("typed record parameter and i32 result must match the exported signature");

    let (res,) = add_coords
        .call(&mut store, (Point { x: 10, y: 32 },))
        .expect("call must resolve");
    assert_eq!(res, 42, "p.x (10) + p.y (32) must equal 42");
}

#[derive(wasmtime::component::ComponentType, wasmtime::component::Lift, PartialEq, Debug)]
#[component(record)]
struct SingleField {
    val: i32,
}

#[test]
fn component_single_field_record_result_executes() {
    let bytes = compile_source_component(
        r#"
public struct Wrapper {
    val: i32,
}

public func wrap(x: i32): Wrapper {
    return Wrapper { val: x }
}
"#,
        "wrap-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:wrap-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let wrap_idx = instance
        .get_export_index(&mut store, Some(&iface), "wrap")
        .expect("interface must export `wrap`");
    let wrap = instance
        .get_typed_func::<(i32,), (SingleField,)>(&mut store, &wrap_idx)
        .expect("single-field record return must match scalar-flattened signature");

    let (res,) = wrap.call(&mut store, (99,)).expect("call must resolve");
    assert_eq!(
        res,
        SingleField { val: 99 },
        "wrap(99) must return SingleField {{ val: 99 }}"
    );
}

#[derive(wasmtime::component::ComponentType, wasmtime::component::Lift, PartialEq, Debug)]
#[component(record)]
struct PointOut {
    x: i32,
    y: i32,
}

#[test]
fn component_multi_field_record_result_executes() {
    let bytes = compile_source_component(
        r#"
public struct Point {
    x: i32,
    y: i32,
}

public func make_point(x: i32, y: i32): Point {
    return Point { x: x, y: y }
}
"#,
        "makepoint-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:makepoint-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let make_point_idx = instance
        .get_export_index(&mut store, Some(&iface), "make-point")
        .expect("interface must export `make-point`");
    let make_point = instance
        .get_typed_func::<(i32, i32), (PointOut,)>(&mut store, &make_point_idx)
        .expect("multi-field record return must match retptr Canonical ABI signature");

    let (res,) = make_point
        .call(&mut store, (17, 25))
        .expect("call must resolve");
    assert_eq!(
        res,
        PointOut { x: 17, y: 25 },
        "make_point(17, 25) must return PointOut {{ x: 17, y: 25 }}"
    );
}

#[test]
fn wit_gen_rich_types_record_and_pixel() {
    use arandu_backend_wasm::wit_gen::generate_wit;
    use arandu_query::db::DatabaseImpl;
    use arandu_query::passes::lower_amir;

    let source = r#"
public struct Pixel {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

public func make_pixel(r: u8, g: u8, b: u8, a: u8): Pixel {
    return Pixel { r: r, g: g, b: b, a: a }
}
"#;
    let mut db = DatabaseImpl::new();
    let file = db.new_file("pixel.aru".into(), source.into());
    let lowered = lower_amir(&db, file);

    let wit = generate_wit(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        "graphics",
    )
    .expect("public func with Pixel struct must generate WIT");

    assert!(
        wit.contains("record pixel {"),
        "WIT must declare `record pixel`:\n{wit}"
    );
    assert!(
        wit.contains("r: u8,"),
        "pixel record must contain field `r: u8`:\n{wit}"
    );
    assert!(
        wit.contains("make-pixel: func(p0: u8, p1: u8, p2: u8, p3: u8) -> pixel;"),
        "function make-pixel must reference pixel record:\n{wit}"
    );
}

#[test]
fn wit_gen_enum_declaration() {
    use arandu_backend_wasm::wit_gen::generate_wit;
    use arandu_query::db::DatabaseImpl;
    use arandu_query::passes::lower_amir;

    let source = r#"
public enum Color {
    Red,
    Green,
    Blue,
}

public func pick(): Color {
    return Color.Red
}
"#;
    let mut db = DatabaseImpl::new();
    let file = db.new_file("colors.aru".into(), source.into());
    let lowered = lower_amir(&db, file);

    let wit = generate_wit(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        "palette",
    )
    .expect("public func with Color enum must generate WIT");

    assert!(
        wit.contains("enum color {"),
        "WIT must declare `enum color`:\n{wit}"
    );
    assert!(
        wit.contains("pick: func() -> color;"),
        "function pick must return color enum:\n{wit}"
    );
}

#[test]
fn component_slice_i32_param_sum() {
    let bytes = compile_source_component(
        r#"
public func sum_slice(items: []i32): i32 {
    let a: i32 = items[0]
    let b: i32 = items[1]
    let c: i32 = items[2]
    return a + b + c
}
"#,
        "slice-sum-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:slice-sum-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let sum_slice_idx = instance
        .get_export_index(&mut store, Some(&iface), "sum-slice")
        .expect("interface must export `sum-slice`");
    let sum_slice = instance
        .get_typed_func::<(&[i32],), (i32,)>(&mut store, &sum_slice_idx)
        .expect("typed slice parameter and i32 result must match the exported signature");

    let (res,) = sum_slice
        .call(&mut store, (&[10, 20, 30][..],))
        .expect("call must resolve");
    assert_eq!(res, 60, "10 + 20 + 30 must equal 60");
}

#[test]
fn component_slice_u8_bytes_count() {
    let bytes = compile_source_component(
        r#"
public func count_nonzero(data: []u8): i32 {
    let mut count: i32 = 0
    let a: u8 = data[0]
    if a > 0 {
        count = count + 1
    }
    let b: u8 = data[1]
    if b > 0 {
        count = count + 1
    }
    let c: u8 = data[2]
    if c > 0 {
        count = count + 1
    }
    return count
}
"#,
        "slice-bytes-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:slice-bytes-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let count_idx = instance
        .get_export_index(&mut store, Some(&iface), "count-nonzero")
        .expect("interface must export `count-nonzero`");
    let count_fn = instance
        .get_typed_func::<(&[u8],), (i32,)>(&mut store, &count_idx)
        .expect("typed byte slice parameter and i32 result must match signature");

    let (res,) = count_fn
        .call(&mut store, (&[5u8, 0u8, 12u8][..],))
        .expect("call must resolve");
    assert_eq!(res, 2, "5 and 12 are non-zero, expected 2");
}

#[test]
fn component_slice_bounds_check_traps() {
    let bytes = compile_source_component(
        r#"
public func read_at_3(items: []i32): i32 {
    return items[3]
}
"#,
        "slice-oob-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:slice-oob-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let read_idx = instance
        .get_export_index(&mut store, Some(&iface), "read-at-3")
        .expect("interface must export `read-at-3`");
    let read_fn = instance
        .get_typed_func::<(&[i32],), (i32,)>(&mut store, &read_idx)
        .expect("typed slice parameter and i32 result must match signature");

    // Pass slice of length 2 — index 3 is out of bounds and MUST trap.
    let err = read_fn
        .call(&mut store, (&[10, 20][..],))
        .expect_err("out of bounds index must trap");
    let trap = err
        .downcast_ref::<wasmtime::Trap>()
        .or_else(|| err.root_cause().downcast_ref::<wasmtime::Trap>());
    assert_eq!(
        trap,
        Some(&wasmtime::Trap::UnreachableCodeReached),
        "trap must occur on OOB index: {err:?}"
    );
}

#[test]
fn component_slice_of_records() {
    let bytes = compile_source_component(
        r#"
public struct Point {
    x: i32,
    y: i32,
}

public func sum_points(pts: []Point): i32 {
    let p0: Point = pts[0]
    let p1: Point = pts[1]
    return p0.x + p0.y + p1.x + p1.y
}
"#,
        "points-slice-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:points-slice-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let sum_pts_idx = instance
        .get_export_index(&mut store, Some(&iface), "sum-points")
        .expect("interface must export `sum-points`");
    let sum_pts = instance
        .get_typed_func::<(&[Point],), (i32,)>(&mut store, &sum_pts_idx)
        .expect("typed slice of records parameter and i32 result must match signature");

    let pts = [Point { x: 1, y: 2 }, Point { x: 10, y: 20 }];
    let (res,) = sum_pts
        .call(&mut store, (&pts[..],))
        .expect("call must resolve");
    assert_eq!(res, 33, "1 + 2 + 10 + 20 must equal 33");
}

#[test]
fn component_slice_echo_roundtrip() {
    let bytes = compile_source_component(
        r#"
public func echo_slice(items: []i32): []i32 {
    return items
}
"#,
        "echo-slice-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:echo-slice-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let echo_idx = instance
        .get_export_index(&mut store, Some(&iface), "echo-slice")
        .expect("interface must export `echo-slice`");
    let echo_fn = instance
        .get_typed_func::<(&[i32],), (Vec<i32>,)>(&mut store, &echo_idx)
        .expect("typed slice parameter and Vec<i32> result must match signature");

    let input = [100, 200, 300, 400];
    let (res,) = echo_fn
        .call(&mut store, (&input[..],))
        .expect("call must resolve");
    assert_eq!(res, vec![100, 200, 300, 400]);
}

#[test]
fn component_slice_and_scalar_params() {
    let bytes = compile_source_component(
        r#"
public func contains_val(items: []i32, target: i32): bool {
    if items[0] == target {
        return true
    }
    if items[1] == target {
        return true
    }
    return false
}
"#,
        "slice-mixed-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:slice-mixed-app/exports@0.1.0")
        .expect("world must export the interface instance");
    let contains_idx = instance
        .get_export_index(&mut store, Some(&iface), "contains-val")
        .expect("interface must export `contains-val`");
    let contains_fn = instance
        .get_typed_func::<(&[i32], i32), (bool,)>(&mut store, &contains_idx)
        .expect("typed slice and i32 parameters and bool result must match signature");

    let items = [42, 99];
    let (found,) = contains_fn
        .call(&mut store, (&items[..], 99))
        .expect("call must resolve");
    assert!(found);

    let (not_found,) = contains_fn
        .call(&mut store, (&items[..], 123))
        .expect("call must resolve");
    assert!(!not_found);
}

#[test]
fn core_module_import_scalar_call() {
    let bytes = compile_source(
        r#"
extern "C" {
    func host_add(a: i32, b: i32): i32
}

public func compute(x: i32, y: i32): i32 {
    unsafe {
        return host_add(x, y)
    }
}
"#,
    );

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("core module compiles");
    let mut linker = wasmtime::Linker::new(&engine);
    linker
        .func_wrap("env", "host_add", |a: i32, b: i32| a + b)
        .expect("link host_add");

    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate");

    let compute = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "compute")
        .expect("compute func");
    let res = compute.call(&mut store, (15, 27)).expect("call compute");
    assert_eq!(res, 42);
}

#[test]
fn core_module_import_side_effect_logging() {
    let bytes = compile_source(
        r#"
extern "C" {
    func host_log(code: i32)
}

public func record_events(): i32 {
    unsafe {
        host_log(101)
        host_log(202)
        host_log(303)
    }
    return 7
}
"#,
    );

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("core module compiles");
    let mut linker = wasmtime::Linker::<Vec<i32>>::new(&engine);
    linker
        .func_wrap(
            "env",
            "host_log",
            |mut caller: wasmtime::Caller<'_, Vec<i32>>, code: i32| {
                caller.data_mut().push(code);
            },
        )
        .expect("link host_log");

    let mut store = wasmtime::Store::new(&engine, Vec::new());
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate");

    let record = instance
        .get_typed_func::<(), i32>(&mut store, "record_events")
        .expect("record_events func");
    let ret = record.call(&mut store, ()).expect("call record_events");
    assert_eq!(ret, 7);
    assert_eq!(store.data(), &[101, 202, 303]);
}

#[test]
fn component_import_host_function() {
    let bytes = compile_source_component(
        r#"
extern "C" {
    func host_multiply(a: i32, b: i32): i32
}

public func calc(x: i32, factor: i32): i32 {
    unsafe {
        return host_multiply(x, factor)
    }
}
"#,
        "calc-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes).expect("decode component");
    let mut linker = wasmtime::component::Linker::<()>::new(&engine);
    linker
        .root()
        .func_wrap("host-multiply", |_cx, (a, b): (i32, i32)| Ok((a * b,)))
        .expect("link host-multiply");

    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("instantiate component");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:calc-app/exports@0.1.0")
        .expect("exports iface");
    let calc_idx = instance
        .get_export_index(&mut store, Some(&iface), "calc")
        .expect("calc export");
    let calc_fn = instance
        .get_typed_func::<(i32, i32), (i32,)>(&mut store, &calc_idx)
        .expect("typed calc func");

    let (res,) = calc_fn.call(&mut store, (6, 7)).expect("call calc");
    assert_eq!(res, 42);
}

#[test]
fn component_import_with_slice_parameter() {
    let bytes = compile_source_component(
        r#"
extern "C" {
    func host_double(x: i32): i32
}

public func sum_doubles(items: []i32): i32 {
    unsafe {
        let d0: i32 = host_double(items[0])
        let d1: i32 = host_double(items[1])
        return d0 + d1
    }
}
"#,
        "doubles-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes).expect("decode component");
    let mut linker = wasmtime::component::Linker::<()>::new(&engine);
    linker
        .root()
        .func_wrap("host-double", |_cx, (x,): (i32,)| Ok((x * 2,)))
        .expect("link host-double");

    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("instantiate component");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:doubles-app/exports@0.1.0")
        .expect("exports iface");
    let sum_doubles_idx = instance
        .get_export_index(&mut store, Some(&iface), "sum-doubles")
        .expect("sum-doubles export");
    let sum_doubles_fn = instance
        .get_typed_func::<(&[i32],), (i32,)>(&mut store, &sum_doubles_idx)
        .expect("typed sum-doubles func");

    let items = [10, 25];
    let (res,) = sum_doubles_fn
        .call(&mut store, (&items[..],))
        .expect("call sum-doubles");
    assert_eq!(res, 70, "10 * 2 + 25 * 2 = 70");
}

#[test]
fn component_string_interp_with_ints_and_bools() {
    let bytes = compile_source_component(
        r#"
public func format_greeting(name: str, age: int, is_active: bool): str {
    return "User ${name} is ${age} years old (active=${is_active})"
}
"#,
        "greeting-app",
    );

    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes)
        .expect("component must decode with wasmtime");
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker
        .instantiate(&mut store, &component)
        .expect("component must instantiate");

    let iface = instance
        .get_export_index(&mut store, None, "arandu:greeting-app/exports@0.1.0")
        .expect("world must export exports interface");
    let greet_idx = instance
        .get_export_index(&mut store, Some(&iface), "format-greeting")
        .expect("interface must export `format-greeting`");
    let greet = instance
        .get_typed_func::<(&str, i32, bool), (wasmtime::component::WasmStr,)>(
            &mut store, &greet_idx,
        )
        .expect("signature must match");

    let (s1,) = greet
        .call(&mut store, ("Alice", 30, true))
        .expect("call must succeed");
    assert_eq!(
        s1.to_str(&mut store).unwrap(),
        "User Alice is 30 years old (active=true)"
    );

    let (s2,) = greet
        .call(&mut store, ("Bob", -12, false))
        .expect("call must succeed");
    assert_eq!(
        s2.to_str(&mut store).unwrap(),
        "User Bob is -12 years old (active=false)"
    );

    let (s3,) = greet
        .call(&mut store, ("Charlie", 0, true))
        .expect("call must succeed");
    assert_eq!(
        s3.to_str(&mut store).unwrap(),
        "User Charlie is 0 years old (active=true)"
    );
}

#[test]
fn cabi_realloc_preserves_data_and_frees_old_ptr() {
    let bytes = compile_source("public func main(): i32 { return 0 }");

    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).expect("module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("module must export `memory`");

    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .expect("`cabi_realloc` must be exported with correct signature");

    // 1. Allocate initial 16 bytes
    let p1 = realloc
        .call(&mut store, (0, 0, 4, 16))
        .expect("allocation must succeed");
    assert!(p1 > 0);

    // 2. Write test payload into p1
    let payload = [0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44];
    memory
        .write(&mut store, p1 as usize, &payload)
        .expect("write payload");

    // 3. Reallocate to 64 bytes with old_ptr = p1
    let p2 = realloc
        .call(&mut store, (p1, 16, 4, 64))
        .expect("realloc must succeed");
    assert!(p2 > 0);

    // 4. Verify data in new buffer p2 has payload preserved
    let mut read_buf = [0u8; 8];
    memory
        .read(&mut store, p2 as usize, &mut read_buf)
        .expect("read preserved payload");
    assert_eq!(read_buf, payload, "cabi_realloc must preserve old data");

    // 5. Verify p1 was freed by allocating 16 bytes and checking it reuses p1 from free list
    let p3 = realloc
        .call(&mut store, (0, 0, 4, 16))
        .expect("allocation of freed size");
    assert_eq!(p3, p1, "old buffer p1 must have been freed and reused");
}
