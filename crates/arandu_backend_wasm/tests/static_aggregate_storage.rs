#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{compile_source, run_main_i32};

#[allow(clippy::panic)]
fn memory_copies(bytes: &[u8], name: &str) -> usize {
    let export = common::find_func_export(bytes, name).unwrap();
    let mut imported = 0;
    let mut index = None;
    let mut code_index = 0;
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload.unwrap() {
            wasmparser::Payload::ImportSection(section) => {
                for import in section.into_imports() {
                    if matches!(import.unwrap().ty, wasmparser::TypeRef::Func(_)) {
                        imported += 1;
                    }
                }
            }
            wasmparser::Payload::ExportSection(section) => {
                for entry in section {
                    let entry = entry.unwrap();
                    if entry.name == export {
                        index = Some(entry.index);
                    }
                }
            }
            wasmparser::Payload::CodeSectionEntry(body) => {
                if index == Some(imported + code_index) {
                    return body
                        .get_operators_reader()
                        .unwrap()
                        .into_iter()
                        .filter(|operator| {
                            matches!(
                                operator.as_ref().unwrap(),
                                wasmparser::Operator::MemoryCopy { .. }
                            )
                        })
                        .count();
                }
                code_index += 1;
            }
            _ => {}
        }
    }
    panic!("missing function body {export}")
}

#[test]
fn serialized_enclosing_struct_does_not_materialize_its_array_operand() {
    let bytes = compile_source(
        r#"
struct Packet { bytes: [64]u8, marker: u32 }
public func initial(): Packet { return Packet { bytes: [7; 64], marker: 42 } }
func main(): int {
    let original = initial()
    let mut changed = original
    changed.bytes[1] = 99
    if original.bytes[1] != 7 || changed.bytes[1] != 99 { return 1 }
    if original.marker != 42 || changed.marker != 42 { return 2 }
    return 0
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
    assert_eq!(
        memory_copies(&bytes, "initial"),
        2,
        "owned return temporary and ABI transfer; no intermediate array or struct copy"
    );
}

#[test]
fn copied_literal_remains_mutable_independently_across_calls() {
    let bytes = compile_source(
        r#"
public func initial(): [64]u8 { return [7; 64] }
func main(): int {
    let mut a = initial()
    a[0] = 99
    let b = initial()
    if a[0] != 99 || b[0] != 7 || b[63] != 7 { return 1 }
    return 0
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
    assert_eq!(
        memory_copies(&bytes, "initial"),
        2,
        "copy readonly bytes into owned result storage, then ABI transfer"
    );
}

#[test]
fn borrowed_aggregate_and_multiple_copy_uses_keep_independent_storage() {
    let bytes = compile_source(
        r#"
func update(bytes: mut ref [64]u8): void { bytes[0] = 99 }
func observe(bytes: ref [64]u8): u8 { return bytes[0] }
func main(): int {
    let original: [64]u8 = [7; 64]
    let first = original
    let second = original
    update(mut ref first)
    if observe(ref original) != 7 || observe(ref second) != 7 { return 1 }
    if observe(ref first) != 99 { return 2 }
    return 0
}
"#,
    );
    assert_eq!(run_main_i32(&bytes), 0);
}

#[derive(wasmtime::component::ComponentType, wasmtime::component::Lift, Debug, PartialEq)]
#[component(record)]
struct WideRecord {
    a: u64,
    b: u64,
    c: u64,
    d: u64,
}

#[test]
fn static_owned_return_preserves_component_record_abi() {
    let bytes = common::compile_source_component(
        r#"
public struct WideRecord { public a: u64, public b: u64, public c: u64, public d: u64 }
public func initial(): WideRecord { return WideRecord { a: 1, b: 2, c: 3, d: 4 } }
"#,
        "static-storage",
    );
    let engine = wasmtime::Engine::default();
    let component = wasmtime::component::Component::new(&engine, &bytes).unwrap();
    let linker = wasmtime::component::Linker::<()>::new(&engine);
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component).unwrap();
    let interface = instance
        .get_export_index(&mut store, None, "arandu:static-storage/exports@0.1.0")
        .unwrap();
    let index = instance
        .get_export_index(&mut store, Some(&interface), "initial")
        .unwrap();
    let initial = instance
        .get_typed_func::<(), (WideRecord,)>(&mut store, &index)
        .unwrap();
    for _ in 0..100 {
        let (record,) = initial.call(&mut store, ()).unwrap();
        assert_eq!(
            record,
            WideRecord {
                a: 1,
                b: 2,
                c: 3,
                d: 4
            }
        );
    }
}

#[test]
fn canonical_allocator_keeps_alignment_after_odd_sizes_and_reuse() {
    let bytes = compile_source("func main(): int { return 0 }");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, &bytes).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    let realloc = instance
        .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "cabi_realloc")
        .unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let mut allocations = Vec::new();
    for size in [1, 3, 5, 17, 31, 64, 257] {
        let pointer = realloc.call(&mut store, (0, 0, 16, size)).unwrap();
        assert_eq!(pointer % 16, 0);
        let offset = usize::try_from(pointer).unwrap();
        memory.write(&mut store, offset, &[7]).unwrap();
        let grown = realloc
            .call(&mut store, (pointer, size, 8, size + 3))
            .unwrap();
        assert_eq!(grown % 16, 0);
        assert_eq!(memory.data(&store)[usize::try_from(grown).unwrap()], 7);
        allocations.push((grown, size + 3));
    }
    for (pointer, size) in allocations {
        assert_eq!(realloc.call(&mut store, (pointer, size, 16, 0)).unwrap(), 0);
    }
    for align in [1, 2, 4, 8, 16] {
        let pointer = realloc.call(&mut store, (0, 0, align, 9)).unwrap();
        assert_eq!(pointer % 16, 0, "reused free-list block");
    }
    for align in [0, 3, 32] {
        assert!(realloc.call(&mut store, (0, 0, align, 9)).is_err());
    }
    for size in [-1, -15, -16] {
        assert!(realloc.call(&mut store, (0, 0, 16, size)).is_err());
    }
}

#[test]
fn static_array_passed_as_a_block_argument_keeps_its_materialization() {
    use arandu_middle::amir::{
        AmirBasicBlock, AmirConstant, AmirFunc, AmirOperand, AmirProgram, AmirRvalue, AmirStmt,
        AmirStmtTable, AmirTerminator, BlockId, BlockParam, LocalId, TempId,
    };
    use arandu_middle::layout::DenseRange;
    use arandu_middle::literal_pool::AmirLiteralPool;
    use arandu_middle::types::{ArType, Primitive};

    let info = arandu_semantics::TypeInfo::default();
    let interner = &info.type_interner;
    let int = interner.intern(ArType::Primitive(Primitive::Int));
    let array = interner.intern(ArType::Array(64, int));
    let mut pool = AmirLiteralPool::default();
    let seven = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("7")));
    let zero = AmirOperand::Constant(AmirConstant::Pool(pool.intern_int("0")));
    let mut statements = AmirStmtTable::new();
    statements.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Array {
            items: vec![seven; 64],
        },
    });
    statements.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::IndexAccess {
            base: AmirOperand::Copy(TempId::from_usize(2)),
            index: zero,
        },
    });
    let blocks = vec![
        AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::new(0, 1),
            terminator: AmirTerminator::Goto {
                target: BlockId::from_usize(1),
                args: vec![AmirOperand::Copy(TempId::from_usize(1))],
            },
        },
        AmirBasicBlock {
            id: BlockId::from_usize(1),
            params: DenseRange::new(0, 1),
            statements: DenseRange::new(1, 1),
            terminator: AmirTerminator::Return,
        },
    ];
    let function = AmirFunc {
        symbol: common::foreign_sym(1),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![
            common::temp(0, int),
            common::temp(1, array),
            common::temp(2, array),
        ],
        cfg: arandu_middle::cfg::compute_cfg_edges(&blocks),
        blocks,
        block_params: vec![BlockParam {
            id: TempId::from_usize(2),
            local: LocalId::from_usize(0),
            ty: array,
            from: None,
            moved: false,
        }],
        stmts: statements,
    };
    let program = AmirProgram {
        funcs: vec![function],
        literal_pool: pool,
        extern_funcs: Default::default(),
        debug_bindings: vec![],
        debug_blocks: vec![],
    };
    let mut symbols = arandu_semantics::SymbolTable::new(0);
    common::register_imported_sym(&mut symbols, common::foreign_sym(1), "main");
    let bytes =
        arandu_backend_wasm::emit_wasm(&program, &symbols, interner, &info, common::wasm32())
            .unwrap();
    assert_eq!(run_main_i32(&bytes), 7);
    assert_eq!(
        memory_copies(&bytes, "main"),
        1,
        "block arguments are not proven readonly copies"
    );

    // Move forwards the actual backing pointer instead of copying bytes. It
    // cannot receive a readonly placeholder, even for an otherwise Copy type.
    let mut moved = program;
    let function = &mut moved.funcs[0];
    let Some(AmirStmt::Assign {
        rhs: array_literal, ..
    }) = function
        .stmts
        .get(arandu_middle::amir::InstrId::from_usize(0))
    else {
        panic!("array constructor");
    };
    let mut statements = AmirStmtTable::new();
    statements.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: array_literal.clone(),
    });
    statements.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::Use(AmirOperand::Move(TempId::from_usize(1))),
    });
    let zero = AmirOperand::Constant(AmirConstant::Pool(moved.literal_pool.intern_int("0")));
    statements.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::IndexAccess {
            base: AmirOperand::Copy(TempId::from_usize(2)),
            index: zero,
        },
    });
    function.stmts = statements;
    function.block_params.clear();
    function.blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        params: DenseRange::empty(),
        statements: DenseRange::new(0, 3),
        terminator: AmirTerminator::Return,
    }];
    function.cfg = arandu_middle::cfg::compute_cfg_edges(&function.blocks);
    let bytes = arandu_backend_wasm::emit_wasm(&moved, &symbols, interner, &info, common::wasm32())
        .unwrap();
    assert_eq!(run_main_i32(&bytes), 7);
    assert_eq!(
        memory_copies(&bytes, "main"),
        1,
        "Move must receive owned storage"
    );
}
