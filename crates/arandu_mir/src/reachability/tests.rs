#![allow(clippy::unwrap_used)]

use super::{function_dependencies, live_functions};
use crate::amir::{
    AmirBasicBlock, AmirFunc, AmirLocal, AmirOperand, AmirProgram, AmirRvalue, AmirStmt,
    AmirStmtTable, AmirTemp, AmirTerminator, BlockId, BlockParam, GenArenaDomain, LocalId, TempId,
};
use crate::cfg::compute_cfg_edges;
use crate::layout::{DenseRange, StructFieldInfo, StructFields};
use crate::types::{ArType, Primitive, TypeId, TypeShape};
use crate::{Span, SymbolId, SymbolTable};
use arandu_middle::Visibility;
use arandu_middle::symbol_table::{ScopeId, Symbol, SymbolKind};
use arandu_typeck::{EnumPayloadShape, TypeInfo};
use std::sync::Arc;

fn program(funcs: Vec<AmirFunc>) -> AmirProgram {
    AmirProgram {
        funcs,
        literal_pool: Default::default(),
        extern_funcs: Default::default(),
        debug_bindings: vec![],
        debug_blocks: vec![],
    }
}

fn function(symbols: &mut SymbolTable, id: u32, name: &str, ty: TypeId) -> AmirFunc {
    let symbol = SymbolId::new(999, id);
    symbols.imported_symbols_mut().insert(
        symbol,
        Symbol {
            id: symbol,
            name: name.into(),
            kind: SymbolKind::Func,
            span: Span::new(0, 0, 0),
            scope: ScopeId(0),
            visibility: Visibility::Private,
            lang_item: None,
        },
    );
    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    }];
    AmirFunc {
        symbol,
        return_type: ty,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![],
        cfg: compute_cfg_edges(&blocks),
        blocks,
        block_params: vec![],
        stmts: AmirStmtTable::new(),
    }
}

#[test]
fn closure_keeps_jump_callbacks_transitive_calls_tooling_and_live_destructors() {
    let mut symbols = SymbolTable::new(0);
    let mut info = TypeInfo::default();
    let void = info.type_interner.intern(ArType::Void);
    let live_ty = info.type_interner.intern(ArType::Primitive(Primitive::Int));
    let dead_ty = info.type_interner.intern(ArType::Primitive(Primitive::U64));
    let mut main = function(&mut symbols, 0, "main", void);
    let mut callback = function(&mut symbols, 1, "callback", void);
    let leaf = function(&mut symbols, 2, "leaf", void);
    let tooling = function(&mut symbols, 3, "test_entry", void);
    let destructor = function(&mut symbols, 4, "live_drop", void);
    let mut unused = function(&mut symbols, 5, "unused", void);
    let unused_drop = function(&mut symbols, 6, "unused_drop", void);
    main.blocks[0].terminator = AmirTerminator::Goto {
        target: BlockId::from_usize(1),
        args: vec![AmirOperand::FunctionRef(callback.symbol)],
    };
    main.blocks.push(AmirBasicBlock {
        id: BlockId::from_usize(1),
        params: DenseRange::new(0, 1),
        statements: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    });
    let callback_ty = info
        .type_interner
        .intern(ArType::func(&[], void, &info.type_interner));
    main.temps.push(AmirTemp {
        id: TempId::from_usize(0),
        ty: callback_ty,
        is_copy: true,
        is_nullable: false,
        span: Span::new(0, 0, 0),
    });
    main.block_params.push(BlockParam {
        id: TempId::from_usize(0),
        local: LocalId::from_usize(0),
        ty: callback_ty,
        from: None,
        moved: false,
    });
    main.locals.push(AmirLocal {
        id: LocalId::from_usize(0),
        ty: callback_ty,
        is_memory: false,
        symbol: None,
        span: Span::new(0, 0, 0),
        use_span: None,
    });
    main.cfg = compute_cfg_edges(&main.blocks);
    for (func, ty) in [(&mut main, live_ty), (&mut unused, dead_ty)] {
        func.locals.push(AmirLocal {
            id: LocalId::from_usize(func.locals.len()),
            ty,
            is_memory: false,
            symbol: None,
            span: Span::new(0, 0, 0),
            use_span: None,
        });
    }
    callback.stmts.push(AmirStmt::Call {
        lhs: None,
        callee: AmirOperand::FunctionRef(leaf.symbol),
        args: Default::default(),
        return_borrow: None,
    });
    callback.blocks[0].statements = DenseRange::new(0, 1);
    symbols.tooling_entrypoints.insert(tooling.symbol);
    info.destructor_instances.insert(live_ty, destructor.symbol);
    info.destructor_instances
        .insert(dead_ty, unused_drop.symbol);
    let program = crate::amir::AmirProgram {
        funcs: vec![
            main,
            callback,
            leaf,
            tooling,
            destructor,
            unused,
            unused_drop,
        ],
        literal_pool: Default::default(),
        extern_funcs: Default::default(),
        debug_bindings: vec![],
        debug_blocks: vec![],
    };
    let live = live_functions(&program, &symbols, &info.type_interner, &info);
    for id in 0..5 {
        assert!(live.contains(&SymbolId::new(999, id)));
    }
    for id in 5..7 {
        assert!(!live.contains(&SymbolId::new(999, id)));
    }
    let mut reversed = program.clone();
    reversed.funcs.reverse();
    assert_eq!(
        live,
        live_functions(&reversed, &symbols, &info.type_interner, &info)
    );
}

#[test]
fn library_preserves_private_host_lookup() {
    let mut symbols = SymbolTable::new(0);
    let info = TypeInfo::default();
    let void = info.type_interner.intern(ArType::Void);
    let helper = function(&mut symbols, 0, "private_helper", void);
    let symbol = helper.symbol;
    let program = crate::amir::AmirProgram {
        funcs: vec![helper],
        literal_pool: Default::default(),
        extern_funcs: Default::default(),
        debug_bindings: vec![],
        debug_blocks: vec![],
    };
    assert!(live_functions(&program, &symbols, &info.type_interner, &info).contains(&symbol));
}

#[test]
fn namespace_alias_keeps_actual_definition_and_its_transitive_dependency() {
    let mut symbols = SymbolTable::new(0);
    let info = TypeInfo::default();
    let void = info.type_interner.intern(ArType::Void);
    let mut main = function(&mut symbols, 0, "main", void);
    let mut helper = function(&mut symbols, 1, "helper", void);
    let leaf = function(&mut symbols, 2, "leaf", void);
    let dead = function(&mut symbols, 3, "dead", void);
    let alias = SymbolId::new(999, 4);
    let declaration_span = Span::new(999, 10, 20);
    symbols
        .imported_symbols_mut()
        .get_mut(&helper.symbol)
        .unwrap()
        .span = declaration_span;
    symbols.imported_symbols_mut().insert(
        alias,
        Symbol {
            id: alias,
            name: "dep.helper".into(),
            kind: SymbolKind::NamespaceMember,
            span: declaration_span,
            scope: ScopeId(0),
            visibility: Visibility::Private,
            lang_item: None,
        },
    );
    for (function, target) in [(&mut main, alias), (&mut helper, leaf.symbol)] {
        function.stmts.push(AmirStmt::Call {
            lhs: None,
            callee: AmirOperand::FunctionRef(target),
            args: Default::default(),
            return_borrow: None,
        });
        function.blocks[0].statements = DenseRange::new(0, 1);
    }
    let program = program(vec![main, helper, leaf, dead]);
    let live = live_functions(&program, &symbols, &info.type_interner, &info);
    for id in [0, 1, 2, 4] {
        assert!(live.contains(&SymbolId::new(999, id)));
    }
    assert!(!live.contains(&SymbolId::new(999, 3)));
}

#[test]
fn projected_struct_and_enum_payload_drops_keep_private_destructors() {
    let mut symbols = SymbolTable::new(0);
    let mut info = TypeInfo::default();
    let void = info.type_interner.intern(ArType::Void);
    let main = function(&mut symbols, 0, "main", void);
    let mut api = function(&mut symbols, 1, "consume", void);
    let destructor = function(&mut symbols, 2, "Inner.close", void);
    let dead = function(&mut symbols, 3, "unused", void);
    symbols
        .imported_symbols_mut()
        .get_mut(&api.symbol)
        .unwrap()
        .visibility = Visibility::Public;
    let inner = SymbolId::new(999, 10);
    let outer = SymbolId::new(999, 11);
    let enum_id = SymbolId::new(999, 12);
    let variant = SymbolId::new(999, 13);
    let inner_ty = info
        .type_interner
        .intern(ArType::named(inner, &[], &info.type_interner));
    let outer_ty = info
        .type_interner
        .intern(ArType::named(outer, &[], &info.type_interner));
    let enum_ty = info
        .type_interner
        .intern(ArType::named(enum_id, &[], &info.type_interner));
    info.destructor_instances
        .insert(inner_ty, destructor.symbol);
    info.struct_fields.insert(
        outer,
        Arc::new(StructFields::from_entries([StructFieldInfo {
            name: "inner".into(),
            symbol: Some(SymbolId::new(999, 14)),
            ty: inner_ty,
            index: 0,
        }])),
    );
    info.enum_variants.insert(
        variant,
        (enum_id, EnumPayloadShape::Tuple(vec![inner_ty, void])),
    );
    info.enum_variant_tags.insert(variant, 0);
    // Each dependency must work alone: no direct Inner temp/local exists.
    for ty in [outer_ty, enum_ty] {
        api.locals = vec![AmirLocal {
            id: LocalId::from_usize(0),
            ty,
            is_memory: true,
            symbol: None,
            span: Span::new(0, 0, 0),
            use_span: None,
        }];
        let program = program(vec![
            main.clone(),
            api.clone(),
            destructor.clone(),
            dead.clone(),
        ]);
        let live = live_functions(&program, &symbols, &info.type_interner, &info);
        assert!(live.contains(&destructor.symbol));
        assert!(!live.contains(&dead.symbol));
    }
}

#[test]
fn gen_payload_metadata_and_block_parameters_keep_drop_shims() {
    let mut symbols = SymbolTable::new(0);
    let mut info = TypeInfo::default();
    let void = info.type_interner.intern(ArType::Void);
    let mut main = function(&mut symbols, 0, "main", void);
    let destructor = function(&mut symbols, 1, "Payload.close", void);
    let dead = function(&mut symbols, 2, "unused", void);
    let payload_ty = info.type_interner.intern(ArType::named(
        SymbolId::new(999, 10),
        &[],
        &info.type_interner,
    ));
    info.destructor_instances
        .insert(payload_ty, destructor.symbol);
    let operand = AmirOperand::Copy(TempId::from_usize(0));
    let arena = GenArenaDomain::CompilerManaged;
    let origin = Span::new(0, 0, 0);
    let operations = [
        AmirRvalue::GenInsert {
            value: operand,
            payload_ty,
            arena,
            origin,
        },
        AmirRvalue::GenSet {
            gen_ref: operand,
            value: operand,
            payload_ty,
            arena,
            origin,
        },
        AmirRvalue::GenUpsert {
            gen_ref: operand,
            value: operand,
            payload_ty,
            arena,
            origin,
        },
    ];
    for rhs in operations {
        main.stmts = AmirStmtTable::new();
        main.stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs,
        });
        main.blocks[0].statements = DenseRange::new(0, 1);
        let program = program(vec![main.clone(), destructor.clone(), dead.clone()]);
        let live = live_functions(&program, &symbols, &info.type_interner, &info);
        assert!(live.contains(&destructor.symbol));
        assert!(!live.contains(&dead.symbol));
    }
    main.stmts = AmirStmtTable::new();
    main.blocks[0].statements = DenseRange::empty();
    main.block_params.push(BlockParam {
        id: TempId::from_usize(0),
        local: LocalId::from_usize(0),
        ty: payload_ty,
        from: None,
        moved: false,
    });
    let program = program(vec![main, destructor.clone(), dead.clone()]);
    let live = live_functions(&program, &symbols, &info.type_interner, &info);
    assert!(live.contains(&destructor.symbol));
    assert!(!live.contains(&dead.symbol));
}

#[test]
fn structural_bounds_fail_conservatively_without_discarding_imports() {
    let mut symbols = SymbolTable::new(0);
    let info = TypeInfo::default();
    let void = info.type_interner.intern(ArType::Void);
    let mut deep = void;
    for _ in 0..TypeShape::MAX_DEPTH {
        deep = info.type_interner.intern(ArType::Option(deep));
    }
    let main = function(&mut symbols, 0, "main", deep);
    let dead = function(&mut symbols, 1, "unused", void);
    let mut program = program(vec![main, dead]);
    let external = SymbolId::new(999, 2);
    program
        .extern_funcs
        .insert(external, (vec![], ArType::Void));
    assert!(
        function_dependencies(
            &program.funcs[0],
            &program,
            &symbols,
            &info.type_interner,
            &info,
        )
        .is_err()
    );
    let live = live_functions(&program, &symbols, &info.type_interner, &info);
    for symbol in [SymbolId::new(999, 0), SymbolId::new(999, 1), external] {
        assert!(live.contains(&symbol));
    }
}

#[test]
fn inconsistent_generic_field_metadata_disables_pruning() {
    let mut symbols = SymbolTable::new(0);
    let mut info = TypeInfo::default();
    let void = info.type_interner.intern(ArType::Void);
    let nominal = SymbolId::new(999, 10);
    let parameter = SymbolId::new(999, 11);
    let nominal_ty = info
        .type_interner
        .intern(ArType::named(nominal, &[], &info.type_interner));
    info.generic_params
        .insert(nominal, Arc::new(vec![parameter]));
    info.struct_fields.insert(
        nominal,
        Arc::new(StructFields::from_entries([StructFieldInfo {
            name: "field".into(),
            symbol: None,
            ty: void,
            index: 0,
        }])),
    );
    let main = function(&mut symbols, 0, "main", nominal_ty);
    let dead = function(&mut symbols, 1, "unused", void);
    let mut program = program(vec![main, dead]);
    let external = SymbolId::new(999, 2);
    program
        .extern_funcs
        .insert(external, (vec![], ArType::Void));
    assert!(
        function_dependencies(
            &program.funcs[0],
            &program,
            &symbols,
            &info.type_interner,
            &info,
        )
        .is_err()
    );
    let live = live_functions(&program, &symbols, &info.type_interner, &info);
    assert!(live.contains(&SymbolId::new(999, 1)));
    assert!(live.contains(&external));
}
