#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use arandu_backend_wasm::{emit_component_for_size, emit_wasm_for_size};
use arandu_query::{db::DatabaseImpl, passes::lower_amir};

fn compile_size(source: &str, component: bool) -> Vec<u8> {
    let mut db = DatabaseImpl::new();
    db.set_target_config(common::wasm32());
    let file = db.new_file("size.aru".into(), source.into());
    let diagnostics = arandu_query::passes::parse_diagnostics(&db, file);
    assert!(diagnostics.is_empty(), "{:?}", diagnostics.as_slice());
    let lowered = lower_amir(&db, file);
    assert!(
        lowered.type_check.diagnostics.is_empty(),
        "{:?}",
        lowered.type_check.diagnostics
    );
    let tc = &lowered.type_check;
    if component {
        emit_component_for_size(
            &lowered.amir,
            &tc.symbols,
            &tc.type_info.type_interner,
            tc.type_info.as_ref(),
            common::wasm32(),
            "size-test",
        )
        .unwrap()
    } else {
        emit_wasm_for_size(
            &lowered.amir,
            &tc.symbols,
            &tc.type_info.type_interner,
            tc.type_info.as_ref(),
            common::wasm32(),
        )
        .unwrap()
    }
}

#[test]
fn size_core_prunes_dead_functions_imports_literals_and_allocation_helpers() {
    let source = r#"
extern "C" { func unused_import(): int }
func dead(): str { return "unused-data-must-not-enter-the-artifact" }
func sum(a: int, b: int): int { return a + b }
public func api(): int { return 7 }
func main(): int { return sum(19, 23) }
"#;
    let baseline = common::compile_source(source);
    let bytes = compile_size(source, false);
    wasmparser::Validator::new().validate_all(&bytes).unwrap();
    assert_eq!(common::run_main_i32(&bytes), 42);
    assert!(bytes.len() < baseline.len());
    assert_eq!(bytes, compile_size(source, false));
    assert!(common::find_func_export(&bytes, "api").is_some());
    assert!(common::find_func_export(&baseline, "cabi_realloc").is_some());
    assert!(common::find_func_export(&bytes, "cabi_realloc").is_none());
    assert!(!bytes.windows(11).any(|part| part == b"unused-data"));
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        match payload.unwrap() {
            wasmparser::Payload::FunctionSection(section) => assert_eq!(section.count(), 3),
            wasmparser::Payload::ImportSection(section) => assert_eq!(section.count(), 0),
            _ => {}
        }
    }
}

#[test]
fn size_core_retains_allocator_when_dynamic_interpolation_calls_it() {
    let bytes = compile_size(
        r#"
public func text(n: int): str { return "n=${n}" }
func main(): int { if text(42) != "n=42" { return 1 } return 0 }
"#,
        false,
    );
    wasmparser::Validator::new().validate_all(&bytes).unwrap();
    let functions = wasmparser::Parser::new(0)
        .parse_all(&bytes)
        .find_map(|payload| match payload.unwrap() {
            wasmparser::Payload::FunctionSection(section) => Some(section.count()),
            _ => None,
        })
        .unwrap();
    assert_eq!(functions, 4, "two source functions plus allocator and free");
    assert_eq!(common::run_main_i32(&bytes), 0);
}

#[test]
fn size_component_preserves_string_abi_and_omits_unused_import_metadata() {
    let bytes = compile_size(
        r#"
extern "C" { func unused_import(): int }
public func greeting(): str { return "Olá" }
func main(): int { return 0 }
"#,
        true,
    );
    assert!(arandu_backend_wasm::is_component(&bytes));
    wasmparser::Validator::new().validate_all(&bytes).unwrap();
    assert!(common::find_func_export(&bytes, "cabi_realloc").is_some());
    assert!(!bytes.windows(13).any(|part| part == b"unused-import"));
    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes).unwrap();
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component).unwrap();
    let interface = instance
        .get_export_index(&mut store, None, "arandu:size-test/exports@0.1.0")
        .unwrap();
    let index = instance
        .get_export_index(&mut store, Some(&interface), "greeting")
        .unwrap();
    let greeting = instance
        .get_typed_func::<(), (wasmtime::component::WasmStr,)>(&mut store, &index)
        .unwrap();
    for _ in 0..100 {
        let (text,) = greeting.call(&mut store, ()).unwrap();
        assert_eq!(text.to_str(&mut store).unwrap(), "Olá");
    }
}

#[test]
fn constant_interpolation_preserves_unicode_nul_and_integer_spelling() {
    let source = r#"
func main(): int {
    if "é\0:${-42}:${0xff}:${true}:${'λ'}" != "é\0:-42:255:true:λ" { return 1 }
    if "before:${"n=${42}"}:after" != "before:n=42:after" { return 2 }
    return 0
}
"#;
    assert_eq!(common::run_main_i32(&compile_size(source, false)), 0);
}

#[test]
fn size_core_retains_allocator_for_implicit_large_aggregate_storage() {
    let bytes = compile_size(
        "func main(): int { let values: [20000]u8 = [7 as u8; 20000] if values[19999] == 7 { return 0 } return 1 }",
        false,
    );
    wasmparser::Validator::new().validate_all(&bytes).unwrap();
    assert_eq!(common::run_main_i32(&bytes), 0);
    let functions = wasmparser::Parser::new(0)
        .parse_all(&bytes)
        .find_map(|payload| match payload.unwrap() {
            wasmparser::Payload::FunctionSection(section) => Some(section.count()),
            _ => None,
        })
        .unwrap();
    assert_eq!(functions, 3, "main plus implicit allocation and free");
}
