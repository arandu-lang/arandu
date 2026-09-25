//! Integration tests for the Wasm backend.
//!
//! These tests verify that:
//! 1. An empty program produces a valid Wasm binary (magic + version header).
//! 2. A simple function compiles without panic.
//! 3. The emitted bytes pass wasmparser validation.
//! 4. The WasmEmitBackend roundtrips.

use arandu_backend_wasm::emit::{WasmModuleBuilder, validate_magic};
use arandu_middle::amir::block::{AmirBasicBlock, BlockId};
use arandu_middle::amir::local::AmirTemp;
use arandu_middle::amir::stmt::{AmirStmtTable, AmirTerminator};
use arandu_middle::amir::{AmirFunc, AmirProgram};
use arandu_middle::cfg::ControlFlowGraph;
use arandu_middle::layout::{DataLayout, DenseRange};
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::symbol_table::{Symbol, SymbolId, SymbolKind};
use arandu_middle::types::{ArType, Primitive, TypeInterner};
use arandu_semantics::SymbolTable;

fn wasm32_layout() -> DataLayout {
    DataLayout::ptr_width(4)
}

/// An empty layout provider (no structs/enums) for hand-built programs.
fn empty_provider() -> arandu_semantics::TypeInfo {
    arandu_semantics::TypeInfo::default()
}

/// A SymbolId in a foreign file (file_id != SymbolTable's file_id) so
/// that `SymbolTable::get()` looks it up via `imported_symbols` (which is pub).
fn foreign_sym(local_id: u32) -> SymbolId {
    SymbolId::new(999, local_id)
}

fn register_imported_sym(symbols: &mut SymbolTable, id: SymbolId, name: &str) {
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

fn make_empty_program() -> (AmirProgram, SymbolTable, TypeInterner) {
    let interner = TypeInterner::new();
    let program = AmirProgram {
        funcs: vec![],
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };
    let symbols = SymbolTable::new(0);
    (program, symbols, interner)
}

fn bb_return(id: usize) -> AmirBasicBlock {
    AmirBasicBlock {
        id: BlockId::from_usize(id),
        params: DenseRange::empty(),
        statements: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    }
}

fn make_void_func(interner: &TypeInterner) -> AmirFunc {
    let mut cfg = ControlFlowGraph::default();
    cfg.successors.push(vec![]);
    cfg.predecessors.push(vec![]);
    AmirFunc {
        symbol: foreign_sym(1),
        return_type: interner.intern(ArType::Void),
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![],
        blocks: vec![bb_return(0)],
        block_params: vec![],
        stmts: AmirStmtTable::new(),
        cfg,
    }
}

#[test]
fn empty_program_produces_valid_wasm_magic() {
    let (program, symbols, interner) = make_empty_program();
    let provider = empty_provider();
    let builder = WasmModuleBuilder::new(&program, &symbols, &interner, &provider, wasm32_layout());
    let bytes = builder
        .build()
        .expect("empty program must emit successfully");
    assert!(
        validate_magic(&bytes),
        "emitted bytes must start with Wasm magic+version"
    );
}

#[test]
fn empty_program_minimum_size() {
    let (program, symbols, interner) = make_empty_program();
    let provider = empty_provider();
    let builder = WasmModuleBuilder::new(&program, &symbols, &interner, &provider, wasm32_layout());
    let bytes = builder
        .build()
        .expect("empty program must emit successfully");
    assert!(
        bytes.len() >= 8,
        "wasm module too small: {} bytes",
        bytes.len()
    );
}

#[test]
fn single_void_func_produces_valid_wasm() {
    let interner = TypeInterner::new();
    let func = make_void_func(&interner);

    let mut symbols = SymbolTable::new(0);
    register_imported_sym(&mut symbols, func.symbol, "main");

    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };

    let provider = empty_provider();
    let builder = WasmModuleBuilder::new(&program, &symbols, &interner, &provider, wasm32_layout());
    let bytes = builder.build().expect("single func must emit successfully");
    assert!(
        validate_magic(&bytes),
        "single-func wasm must have valid magic"
    );
    assert!(bytes.len() > 8);
}

#[test]
fn single_void_func_passes_wasmparser() {
    let interner = TypeInterner::new();
    let func = make_void_func(&interner);

    let mut symbols = SymbolTable::new(0);
    register_imported_sym(&mut symbols, func.symbol, "main");

    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };

    let provider = empty_provider();
    let builder = WasmModuleBuilder::new(&program, &symbols, &interner, &provider, wasm32_layout());
    let bytes = builder.build().expect("emit must succeed");

    let mut validator =
        wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default());
    validator
        .validate_all(&bytes)
        .expect("wasmparser must validate the emitted module");
}

#[test]
fn wasm_emit_backend_roundtrip() {
    use arandu_backend_wasm::WasmEmitBackend;
    use arandu_codegen::CodegenBackend;

    let interner = TypeInterner::new();
    let func = make_void_func(&interner);

    let mut symbols = SymbolTable::new(0);
    register_imported_sym(&mut symbols, func.symbol, "hello");

    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };

    let type_info = arandu_semantics::TypeInfo::new();
    let backend = WasmEmitBackend::new();
    let result = backend.compile(&program, &symbols, &type_info);
    assert!(
        result.is_ok(),
        "WasmEmitBackend::compile failed: {:?}",
        result.err()
    );
    let module = result.expect("compile succeeded");
    assert!(validate_magic(module.bytes()));
}

#[test]
fn func_with_i32_param_passes_wasmparser() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));

    let param_temp = AmirTemp {
        id: arandu_middle::amir::local::TempId::from_usize(0),
        ty: i32_ty,
        is_copy: true,
        is_nullable: false,
        span: arandu_base::Span::new(0, 0, 0),
    };

    let mut cfg = ControlFlowGraph::default();
    cfg.successors.push(vec![]);
    cfg.predecessors.push(vec![]);

    let func = AmirFunc {
        symbol: foreign_sym(1),
        return_type: interner.intern(ArType::Void),
        receiver: None,
        params: vec![arandu_middle::amir::local::TempId::from_usize(0)],
        locals: vec![],
        temps: vec![param_temp],
        blocks: vec![bb_return(0)],
        block_params: vec![],
        stmts: AmirStmtTable::new(),
        cfg,
    };

    let mut symbols = SymbolTable::new(0);
    register_imported_sym(&mut symbols, func.symbol, "add_one");

    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    };

    let provider = empty_provider();
    let builder = WasmModuleBuilder::new(&program, &symbols, &interner, &provider, wasm32_layout());
    let bytes = builder.build().expect("emit must succeed");
    assert!(validate_magic(&bytes));

    let mut validator =
        wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::default());
    validator
        .validate_all(&bytes)
        .expect("wasmparser must validate");
}
