#![allow(dead_code)]
// Shared helpers for the e2e_* test binaries; each binary uses only a subset.
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

//! Helpers shared by the end-to-end test binaries (`e2e_surface`,
//! `e2e_component`, `e2e_handbuilt`): compile Arandu source through the real
//! pipeline (CST → AST → resolve → typeck → AMIR) into wasm bytes and run
//! them inside the wasmtime runtime.

use arandu_backend_wasm::{emit_component, emit_wasm};
use arandu_middle::amir::AmirProgram;
use arandu_middle::amir::local::{AmirTemp, TempId};
use arandu_middle::amir::program::AmirFunc;
use arandu_middle::layout::DataLayout;
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::symbol_table::{Symbol, SymbolId, SymbolKind};
use arandu_middle::types::{TypeId, TypeInterner};
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::lower_amir;
use arandu_semantics::SymbolTable;

pub fn wasm32() -> DataLayout {
    DataLayout::ptr_width(4)
}

pub fn foreign_sym(local_id: u32) -> SymbolId {
    SymbolId::new(999, local_id)
}

pub fn register_imported_sym(symbols: &mut SymbolTable, id: SymbolId, name: &str) {
    symbols.imported_symbols.insert(
        id,
        Symbol {
            id,
            name: name.into(),
            kind: SymbolKind::Func,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );
}

pub fn temp(id: usize, ty: TypeId) -> AmirTemp {
    AmirTemp {
        id: TempId::from_usize(id),
        ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_base::Span::new(0, 0, 0),
    }
}

/// Compile a surface-language program through the real compiler pipeline and
/// return the emitted wasm bytes.
pub fn compile_source(source: &str) -> Vec<u8> {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("e2e.aru".into(), source.into());
    let lowered = lower_amir(&db, file);
    assert!(
        lowered.type_check.diagnostics.is_empty(),
        "source did not lower cleanly:\n{:?}",
        lowered.type_check.diagnostics
    );
    emit_wasm(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        wasm32(),
    )
    .expect("wasm backend must compile the lowered program")
}

/// Compile a surface-language program and return the emitted **Component**
/// bytes.
pub fn compile_source_component(source: &str, pkg_name: &str) -> Vec<u8> {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("e2e.aru".into(), source.into());
    let lowered = lower_amir(&db, file);
    assert!(
        lowered.type_check.diagnostics.is_empty(),
        "source did not lower cleanly:\n{:?}",
        lowered.type_check.diagnostics
    );
    emit_component(
        &lowered.amir,
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
        lowered.type_check.type_info.as_ref(),
        wasm32(),
        pkg_name,
    )
    .expect("component backend must compile the lowered program")
}

/// Name of the exported function whose bare (last `name.path` segment) is `want`.
pub fn find_func_export(bytes: &[u8], want: &str) -> Option<String> {
    let mut found = None;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        let payload = payload.expect("emitted bytes are valid wasm");
        if let wasmparser::Payload::ExportSection(section) = payload {
            for entry in section {
                let entry = entry.expect("valid export entry");
                if entry.kind == wasmparser::ExternalKind::Func {
                    let bare = entry.name.rsplit('.').next().unwrap_or(entry.name);
                    if bare == want {
                        found = Some(entry.name.to_owned());
                    }
                }
            }
        }
    }
    found
}

#[allow(clippy::panic)] // Test helper includes exported names to diagnose malformed output.
pub fn run_main_i32(bytes: &[u8]) -> i32 {
    let name = find_func_export(bytes, "main").unwrap_or_else(|| {
        let exports = wasmparser::Parser::new(0)
            .parse_all(bytes)
            .filter_map(Result::ok)
            .filter_map(|payload| match payload {
                wasmparser::Payload::ExportSection(section) => Some(section),
                _ => None,
            })
            .flat_map(|section| section.into_iter().filter_map(Result::ok))
            .filter(|entry| entry.kind == wasmparser::ExternalKind::Func)
            .map(|entry| entry.name.to_owned())
            .collect::<Vec<_>>();
        panic!("module must export `main`; function exports: {exports:?}");
    });
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let main = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .expect("`main` must have type () → i32");
    main.call(&mut store, ())
        .expect("`main` must run without trapping")
}

pub fn run_main_f64(bytes: &[u8]) -> f64 {
    let name = find_func_export(bytes, "main").expect("module must export `main`");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let main = instance
        .get_typed_func::<(), f64>(&mut store, &name)
        .expect("`main` must have type () → f64");
    main.call(&mut store, ())
        .expect("`main` must run without trapping")
}

pub fn run_main_i64(bytes: &[u8]) -> i64 {
    let name = find_func_export(bytes, "main").expect("module must export `main`");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let main = instance
        .get_typed_func::<(), i64>(&mut store, &name)
        .expect("`main` must have type () → i64");
    main.call(&mut store, ())
        .expect("`main` must run without trapping")
}

/// True iff running `main` traps inside wasmtime (used to assert bounds-check
/// behaviour that the surface compiler lowers into explicit `unreachable`).
pub fn run_main_traps(bytes: &[u8]) -> bool {
    let name = find_func_export(bytes, "main").expect("module must export `main`");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, bytes).expect("emitted module must instantiate");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("module must link");
    let main = instance
        .get_typed_func::<(), i32>(&mut store, &name)
        .expect("`main` must have type () → i32");
    main.call(&mut store, ()).is_err()
}

pub fn emit_one(func: AmirFunc, interner: &TypeInterner, pool: &mut AmirLiteralPool) -> Vec<u8> {
    let mut symbols = SymbolTable::new(0);
    register_imported_sym(&mut symbols, func.symbol, "main");
    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: std::mem::take(pool),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };
    emit_wasm(
        &program,
        &symbols,
        interner,
        &arandu_semantics::TypeInfo::default(),
        wasm32(),
    )
    .expect("hand-built program must emit")
}

pub fn emit_with_imported_symbols(
    func: AmirFunc,
    extra_symbols: &[(SymbolId, &str)],
    interner: &TypeInterner,
    pool: &mut AmirLiteralPool,
) -> Vec<u8> {
    let mut symbols = SymbolTable::new(0);
    register_imported_sym(&mut symbols, func.symbol, "main");
    for &(id, name) in extra_symbols {
        register_imported_sym(&mut symbols, id, name);
    }
    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: std::mem::take(pool),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };
    emit_wasm(
        &program,
        &symbols,
        interner,
        &arandu_semantics::TypeInfo::default(),
        wasm32(),
    )
    .expect("hand-built program must emit")
}
