//! Tests for WIT named interface generation and component exports.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_backend_wasm::wit_gen::generate_wit;
use arandu_middle::amir::AmirProgram;
use arandu_middle::amir::block::{AmirBasicBlock, BlockId};
use arandu_middle::amir::local::AmirTemp;
use arandu_middle::amir::program::AmirFunc;
use arandu_middle::amir::stmt::{AmirStmtTable, AmirTerminator};
use arandu_middle::cfg::ControlFlowGraph;
use arandu_middle::layout::DenseRange;
use arandu_middle::literal_pool::AmirLiteralPool;
use arandu_middle::symbol_table::{Symbol, SymbolId, SymbolKind};
use arandu_middle::types::{ArType, Primitive, TypeInterner};
use arandu_semantics::SymbolTable;

#[test]
fn generates_named_interface_for_public_interface_symbols() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));

    let mut symbols = SymbolTable::new(0);

    fn foreign_sym(local_id: u32) -> SymbolId {
        SymbolId::new(999, local_id)
    }

    // 1. Declare public interface "Calculator"
    let iface_sym = foreign_sym(1);
    symbols.imported_symbols.insert(
        iface_sym,
        Symbol {
            id: iface_sym,
            name: "Calculator".into(),
            kind: SymbolKind::Interface,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );

    // 2. Declare public method "Calculator.add"
    let method_sym = foreign_sym(2);
    symbols.imported_symbols.insert(
        method_sym,
        Symbol {
            id: method_sym,
            name: "Calculator.add".into(),
            kind: SymbolKind::Func,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );

    // 3. Declare standalone public function "ping"
    let ping_sym = foreign_sym(3);
    symbols.imported_symbols.insert(
        ping_sym,
        Symbol {
            id: ping_sym,
            name: "ping".into(),
            kind: SymbolKind::Func,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );

    // Build minimal AMIR func for Calculator.add
    let add_func = AmirFunc {
        symbol: method_sym,
        return_type: i32_ty,
        receiver: None,
        params: vec![
            arandu_middle::amir::local::TempId::from_usize(0),
            arandu_middle::amir::local::TempId::from_usize(1),
        ],
        locals: vec![],
        temps: vec![
            AmirTemp {
                id: arandu_middle::amir::local::TempId::from_usize(0),
                ty: i32_ty,
                is_copy: true,
                is_nullable: false,
                span: arandu_base::Span::new(0, 0, 0),
            },
            AmirTemp {
                id: arandu_middle::amir::local::TempId::from_usize(1),
                ty: i32_ty,
                is_copy: true,
                is_nullable: false,
                span: arandu_base::Span::new(0, 0, 0),
            },
        ],
        blocks: vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }],
        block_params: vec![],
        stmts: AmirStmtTable::new(),
        cfg: ControlFlowGraph::default(),
    };

    // Build minimal AMIR func for ping
    let ping_func = AmirFunc {
        symbol: ping_sym,
        return_type: i32_ty,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![],
        blocks: vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }],
        block_params: vec![],
        stmts: AmirStmtTable::new(),
        cfg: ControlFlowGraph::default(),
    };

    let program = AmirProgram {
        funcs: vec![add_func, ping_func],
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: Default::default(),
    };

    let type_info = arandu_semantics::TypeInfo::new();
    let wit = generate_wit(&program, &symbols, &interner, &type_info, "math-service")
        .expect("WIT generation must succeed");

    // Must declare the named interface "calculator"
    assert!(
        wit.contains("interface calculator {"),
        "expected 'interface calculator {{', got:\n{wit}"
    );
    assert!(
        wit.contains("add: func("),
        "expected method 'add' in interface calculator, got:\n{wit}"
    );

    // Must export the named interface in the world
    assert!(
        wit.contains("export calculator;"),
        "expected 'export calculator;', got:\n{wit}"
    );

    // Must also export the standalone function under exports
    assert!(
        wit.contains("interface exports {"),
        "expected 'interface exports {{', got:\n{wit}"
    );
    assert!(
        wit.contains("ping: func("),
        "expected 'ping: func(' in exports, got:\n{wit}"
    );
    assert!(
        wit.contains("export exports;"),
        "expected 'export exports;', got:\n{wit}"
    );
}

#[test]
fn wit_multiple_interfaces_with_scoped_types() {
    use arandu_middle::layout::{StructFieldInfo, StructFields};
    use std::sync::Arc;
    use wit_parser::Resolve;

    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));

    fn foreign_sym(local_id: u32) -> SymbolId {
        SymbolId::new(999, local_id)
    }

    let mut symbols = SymbolTable::new(0);

    // 1. Declare struct Pixel
    let pixel_sym = foreign_sym(10);
    let pixel_ty = interner.intern(ArType::named(pixel_sym, &[], &interner));
    symbols.imported_symbols.insert(
        pixel_sym,
        Symbol {
            id: pixel_sym,
            name: "Pixel".into(),
            kind: SymbolKind::Struct,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );

    // 2. Declare public interface "Graphics"
    let iface_sym = foreign_sym(1);
    symbols.imported_symbols.insert(
        iface_sym,
        Symbol {
            id: iface_sym,
            name: "Graphics".into(),
            kind: SymbolKind::Interface,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );

    // 3. Declare public method "Graphics.draw(p: Pixel) -> i32"
    let draw_sym = foreign_sym(2);
    symbols.imported_symbols.insert(
        draw_sym,
        Symbol {
            id: draw_sym,
            name: "Graphics.draw".into(),
            kind: SymbolKind::Func,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );

    // 4. Declare standalone public function "ping" (takes no records)
    let ping_sym = foreign_sym(3);
    symbols.imported_symbols.insert(
        ping_sym,
        Symbol {
            id: ping_sym,
            name: "ping".into(),
            kind: SymbolKind::Func,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );

    let draw_func = AmirFunc {
        symbol: draw_sym,
        return_type: i32_ty,
        receiver: None,
        params: vec![arandu_middle::amir::local::TempId::from_usize(0)],
        locals: vec![],
        temps: vec![AmirTemp {
            id: arandu_middle::amir::local::TempId::from_usize(0),
            ty: pixel_ty,
            is_copy: false,
            is_nullable: false,
            span: arandu_base::Span::new(0, 0, 0),
        }],
        blocks: vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }],
        block_params: vec![],
        stmts: AmirStmtTable::new(),
        cfg: ControlFlowGraph::default(),
    };

    let ping_func = AmirFunc {
        symbol: ping_sym,
        return_type: i32_ty,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![],
        blocks: vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }],
        block_params: vec![],
        stmts: AmirStmtTable::new(),
        cfg: ControlFlowGraph::default(),
    };

    let program = AmirProgram {
        funcs: vec![draw_func, ping_func],
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: Default::default(),
    };

    let mut type_info = arandu_semantics::TypeInfo::new();
    type_info.struct_fields.insert(
        pixel_sym,
        Arc::new(StructFields::from_entries([
            StructFieldInfo {
                name: "r".into(),
                symbol: None,
                ty: i32_ty,
                index: 0,
            },
            StructFieldInfo {
                name: "g".into(),
                symbol: None,
                ty: i32_ty,
                index: 1,
            },
            StructFieldInfo {
                name: "b".into(),
                symbol: None,
                ty: i32_ty,
                index: 2,
            },
        ])),
    );

    let wit = generate_wit(&program, &symbols, &interner, &type_info, "scoped-service")
        .expect("WIT generation must succeed");

    // Graphics interface must contain record pixel
    assert!(
        wit.contains("interface graphics {\n  record pixel {"),
        "graphics interface must define pixel, got:\n{wit}"
    );

    // exports interface must NOT define record pixel
    let exports_section = wit.split("interface exports {").nth(1).unwrap();
    assert!(
        !exports_section.contains("record pixel"),
        "exports interface must not redundantly define pixel, got:\n{wit}"
    );

    // Ensure the generated WIT is 100% valid according to wit-parser
    let mut resolve = Resolve::new();
    let pkg_id = resolve.push_source("generated.wit", &wit);
    assert!(
        pkg_id.is_ok(),
        "wit-parser must accept generated scoped WIT: {:?}",
        pkg_id.err()
    );
}

#[test]
fn wit_receiver_is_exposed_once() {
    let interner = TypeInterner::new();
    let i32_ty = interner.intern(ArType::Primitive(Primitive::I32));

    let mut symbols = SymbolTable::new(0);

    fn foreign_sym(local_id: u32) -> SymbolId {
        SymbolId::new(999, local_id)
    }

    // 1. Declare public interface "Widget"
    let iface_sym = foreign_sym(1);
    symbols.imported_symbols.insert(
        iface_sym,
        Symbol {
            id: iface_sym,
            name: "Widget".into(),
            kind: SymbolKind::Interface,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );

    // 2. Declare public method "Widget.tag" with a shared receiver.
    let method_sym = foreign_sym(2);
    symbols.imported_symbols.insert(
        method_sym,
        Symbol {
            id: method_sym,
            name: "Widget.tag".into(),
            kind: SymbolKind::Func,
            span: arandu_base::Span::new(0, 0, 0),
            scope: arandu_middle::symbol_table::ScopeId(0),
            is_public: true,
            lang_item: None,
        },
    );

    // The AMIR contract materializes the receiver as `params[0]`.
    let receiver_temp = arandu_middle::amir::local::TempId::from_usize(0);
    let tag_func = AmirFunc {
        symbol: method_sym,
        return_type: i32_ty,
        receiver: Some(arandu_middle::amir::local::AmirReceiver {
            temp: receiver_temp,
            kind: arandu_middle::hir::ReceiverKind::Shared,
        }),
        params: vec![receiver_temp],
        locals: vec![],
        temps: vec![AmirTemp {
            id: receiver_temp,
            ty: i32_ty,
            is_copy: true,
            is_nullable: false,
            span: arandu_base::Span::new(0, 0, 0),
        }],
        blocks: vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }],
        block_params: vec![],
        stmts: AmirStmtTable::new(),
        cfg: ControlFlowGraph::default(),
    };

    let program = AmirProgram {
        funcs: vec![tag_func],
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: Default::default(),
    };

    let type_info = arandu_semantics::TypeInfo::new();
    let wit = generate_wit(&program, &symbols, &interner, &type_info, "widget-service")
        .expect("WIT generation must succeed");

    // The receiver must be exposed exactly once, as `self`. Before the fix it
    // was also emitted as `p0`, producing a duplicate parameter and invalid WIT.
    assert!(
        wit.contains("self: s32"),
        "expected the receiver to be exposed as `self`, got:\n{wit}"
    );
    assert!(
        !wit.contains("p0: s32"),
        "the receiver must not also be exposed as `p0`, got:\n{wit}"
    );
}
