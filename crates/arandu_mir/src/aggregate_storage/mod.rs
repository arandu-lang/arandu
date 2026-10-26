//! Shared lifetime admission for invocation-owned aggregate scratch storage.
//! Target backends supply their result ABI, not a second ownership checker.

mod cycles;
mod scratch;
mod string_origin;

pub use cycles::cyclic_aggregate_local_store;
pub use scratch::{ScratchResultAbi, function_scratch_safe, scratch_type_safe};

#[cfg(test)]
use arandu_middle::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, TempId};
#[cfg(test)]
use arandu_middle::types::{ArType, Primitive, TypeId, TypeInterner};
#[cfg(test)]
use arandu_middle::{SymbolKind, SymbolTable};
#[cfg(test)]
use string_origin::independent_string_result;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use arandu_middle::Span;
    use arandu_middle::SymbolId;
    use arandu_middle::amir::{AmirBasicBlock, AmirStmtTable, AmirTemp};
    use arandu_typeck::TypeInfo;
    use smallvec::smallvec;

    fn temp(id: usize, ty: TypeId) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty,
            is_copy: true,
            is_nullable: false,
            span: Span::new(0, 10 + id as u32, 11 + id as u32),
        }
    }

    /// One-block function whose statements are built around an opaque
    /// (extern) callee, so admission rules can be exercised per call shape.
    fn shape(
        return_type: TypeId,
        temps: Vec<AmirTemp>,
        stmts: impl Fn(SymbolId) -> Vec<AmirStmt>,
    ) -> (AmirFunc, SymbolTable) {
        shape_named(return_type, "opaque", temps, stmts)
    }

    fn shape_named(
        return_type: TypeId,
        extern_name: &str,
        temps: Vec<AmirTemp>,
        stmts: impl Fn(SymbolId) -> Vec<AmirStmt>,
    ) -> (AmirFunc, SymbolTable) {
        shape_with_callee_kind(
            return_type,
            extern_name,
            SymbolKind::ExternFunc,
            temps,
            stmts,
        )
    }

    fn shape_with_callee_kind(
        return_type: TypeId,
        callee_name: &str,
        callee_kind: SymbolKind,
        temps: Vec<AmirTemp>,
        stmts: impl Fn(SymbolId) -> Vec<AmirStmt>,
    ) -> (AmirFunc, SymbolTable) {
        let mut symbols = SymbolTable::new(0);
        let scope = symbols.global_scope();
        let caller = symbols
            .define(scope, "caller", SymbolKind::Func, Span::new(0, 0, 0))
            .expect("define caller");
        let opaque = symbols
            .define(scope, callee_name, callee_kind, Span::new(0, 0, 0))
            .expect("define extern");
        let mut table = AmirStmtTable::new();
        let built = stmts(opaque);
        let count = built.len();
        for statement in built {
            table.push(statement);
        }
        let blocks = vec![AmirBasicBlock {
            id: arandu_middle::amir::BlockId::from_usize(0),
            statements: DenseRange::new(0, count),
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let cfg = compute_cfg_edges(&blocks);
        let function = AmirFunc {
            symbol: caller,
            return_type,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps,
            blocks,
            block_params: Vec::new(),
            stmts: table,
            cfg,
        };
        (function, symbols)
    }

    fn safe(function: &AmirFunc, symbols: &SymbolTable, interner: &TypeInterner) -> bool {
        function_scratch_safe(
            function,
            symbols,
            interner,
            ScratchResultAbi::CallerOwnedAggregates,
        )
    }

    /// `Free(str)` releases a runtime buffer — never admitted backing, since
    /// `str` itself is rejected as a scratch type — and a value-passed extern
    /// boundary (`ptr` + scalars) cannot name this frame's storage. Neither
    /// may disqualify the function's aggregate scratch.
    #[test]
    fn string_frees_and_value_passed_externs_stay_admitted() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let pointer = interner.intern(ArType::Ptr(byte));
        let (function, symbols) = shape(
            int,
            vec![
                temp(0, string),
                temp(1, pointer),
                temp(2, int),
                temp(3, interner.intern(ArType::Primitive(Primitive::Bool))),
            ],
            |opaque| {
                vec![
                    AmirStmt::Free(AmirOperand::Copy(TempId::from_usize(0))),
                    AmirStmt::Call {
                        lhs: None,
                        callee: AmirOperand::FunctionRef(opaque),
                        args: smallvec![
                            AmirOperand::Copy(TempId::from_usize(1)),
                            AmirOperand::Copy(TempId::from_usize(2)),
                            AmirOperand::Copy(TempId::from_usize(3)),
                        ],
                        return_borrow: None,
                    },
                ]
            },
        );
        assert!(
            safe(&function, &symbols, &interner),
            "Free(str) and value-passed extern arguments must stay admitted"
        );
    }

    /// The flip side: anything that can carry the address of this frame's
    /// backing into opaque code — an aggregate argument, a safe borrow, or
    /// an aggregate result written through an sret pointer — keeps the
    /// function off scratch promotion.
    #[test]
    fn backing_handed_to_opaque_code_disables_scratch() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let array = interner.intern(ArType::Array(4, byte));
        let borrow = interner.intern(ArType::Ref(byte));

        let (function, symbols) = shape(int, vec![temp(0, array)], |opaque| {
            vec![AmirStmt::Call {
                lhs: None,
                callee: AmirOperand::FunctionRef(opaque),
                args: smallvec![AmirOperand::Copy(TempId::from_usize(0))],
                return_borrow: None,
            }]
        });
        assert!(
            !safe(&function, &symbols, &interner),
            "an aggregate argument hands opaque code our backing address"
        );

        let (function, symbols) = shape(int, vec![temp(0, borrow)], |opaque| {
            vec![AmirStmt::Call {
                lhs: None,
                callee: AmirOperand::FunctionRef(opaque),
                args: smallvec![AmirOperand::Copy(TempId::from_usize(0))],
                return_borrow: None,
            }]
        });
        assert!(
            !safe(&function, &symbols, &interner),
            "a safe borrow argument can be retained past slot reuse"
        );

        let (function, symbols) = shape(int, vec![temp(0, array)], |opaque| {
            vec![AmirStmt::Call {
                lhs: Some(TempId::from_usize(0)),
                callee: AmirOperand::FunctionRef(opaque),
                args: smallvec![],
                return_borrow: None,
            }]
        });
        assert!(
            !safe(&function, &symbols, &interner),
            "an aggregate result exposes the sret destination to the callee"
        );
    }

    /// The backend lowers intrinsics to fixed operations — `ptrWrite`
    /// becomes a store through the pointer, `ptrOffset` an add — so an
    /// aggregate at an intrinsic boundary never reaches opaque code. This is
    /// what keeps `vec.tryPush<T>` on scratch instead of a per-push malloc.
    #[test]
    fn intrinsic_boundaries_stay_admitted_with_aggregate_arguments() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let pointer = interner.intern(ArType::Ptr(byte));
        let array = interner.intern(ArType::Array(4, byte));
        let (function, mut symbols) = shape_named(
            int,
            "ptrWrite",
            vec![temp(0, pointer), temp(1, array)],
            |intrinsic| {
                vec![AmirStmt::Call {
                    lhs: None,
                    callee: AmirOperand::FunctionRef(intrinsic),
                    args: smallvec![
                        AmirOperand::Copy(TempId::from_usize(0)),
                        AmirOperand::Copy(TempId::from_usize(1)),
                    ],
                    return_borrow: None,
                }]
            },
        );
        symbols.set_lang_item(
            SymbolId::new(0, 1),
            arandu_middle::symbol_table::LangItem::CompilerIntrinsic,
        );
        assert!(
            safe(&function, &symbols, &interner),
            "an intrinsic boundary must not disqualify scratch"
        );
    }

    #[test]
    fn view_results_are_not_admitted_by_any_backing_abi() {
        let interner = TypeInterner::new();
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let mut function = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: byte,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks: Vec::new(),
            block_params: Vec::new(),
            stmts: Default::default(),
            cfg: Default::default(),
        };
        let symbols = SymbolTable::new(0);
        for abi in [
            ScratchResultAbi::ScalarsOnly,
            ScratchResultAbi::CallerOwnedAggregates,
            ScratchResultAbi::TransferredAggregates,
        ] {
            assert!(function_scratch_safe(&function, &symbols, &interner, abi));
            for ty in [
                ArType::Primitive(Primitive::Str),
                ArType::Slice(byte),
                ArType::Ref(byte),
                ArType::RefMut(byte),
                ArType::Ptr(byte),
            ] {
                function.return_type = interner.intern(ty);
                assert!(!function_scratch_safe(&function, &symbols, &interner, abi));
            }
            function.return_type = byte;
        }
    }

    #[test]
    fn called_coroutines_with_borrowed_or_aggregate_payloads_disable_scratch() {
        let interner = TypeInterner::new();
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        let array = interner.intern(ArType::Array(4, byte));
        let slice = interner.intern(ArType::Slice(byte));
        let reference = interner.intern(ArType::Ref(byte));
        for payload in [byte, array, string, slice, reference] {
            let coroutine = interner.intern(ArType::Coroutine(payload));
            let (function, symbols) = shape_with_callee_kind(
                coroutine,
                "async_helper",
                SymbolKind::Func,
                vec![temp(0, coroutine)],
                |callee| {
                    vec![AmirStmt::Call {
                        lhs: Some(TempId::from_usize(0)),
                        callee: AmirOperand::FunctionRef(callee),
                        args: smallvec![],
                        return_borrow: None,
                    }]
                },
            );
            for abi in [
                ScratchResultAbi::ScalarsOnly,
                ScratchResultAbi::CallerOwnedAggregates,
                ScratchResultAbi::TransferredAggregates,
            ] {
                assert_eq!(
                    function_scratch_safe(&function, &symbols, &interner, abi),
                    payload == byte && abi == ScratchResultAbi::CallerOwnedAggregates,
                    "only scalar caller-owned coroutine results qualify for scratch"
                );
            }
        }
    }

    #[test]
    fn nested_safe_views_are_excluded_but_raw_resource_handles_are_not() {
        let interner = TypeInterner::new();
        let info = TypeInfo::default();
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        for view in [
            ArType::Primitive(Primitive::Str),
            ArType::Slice(byte),
            ArType::Ref(byte),
            ArType::RefMut(byte),
            ArType::Coroutine(byte),
        ] {
            let inner = interner.intern(view);
            let array = interner.intern(ArType::Array(4, inner));
            let option = interner.intern(ArType::Option(array));
            assert!(!scratch_type_safe(option, &interner, &info));
        }
        let pointer = interner.intern(ArType::Ptr(byte));
        let array = interner.intern(ArType::Array(4, pointer));
        assert!(scratch_type_safe(array, &interner, &info));
    }

    #[test]
    fn string_results_require_every_definition_to_have_external_backing() {
        use arandu_middle::amir::{AmirConstant, LocalId};
        use arandu_middle::literal_pool::LiteralId;
        let interner = TypeInterner::new();
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        let literal = AmirOperand::Constant(AmirConstant::Pool(LiteralId(0)));
        let (mut function, symbols) = shape(string, vec![temp(0, string), temp(1, string)], |_| {
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Use(literal),
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
                },
            ]
        });
        assert!(safe(&function, &symbols, &interner));
        function.stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Load(arandu_middle::amir::AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec::SmallVec::new(),
            }),
        });
        // An additional definition with unknown provenance invalidates the
        // whole proof, even if another return path is a literal.
        assert!(!safe(&function, &symbols, &interner));
        let (mut forwarded, symbols) =
            shape(string, vec![temp(0, string), temp(1, string)], |_| {
                vec![AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
                }]
            });
        assert!(!safe(&forwarded, &symbols, &interner));
        forwarded.params.push(TempId::from_usize(1));
        assert!(safe(&forwarded, &symbols, &interner));
        forwarded.params.clear();
        forwarded.stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(0))),
        });
        assert!(!safe(&forwarded, &symbols, &interner));
        let (called, symbols) = shape(string, vec![temp(0, string)], |callee| {
            vec![AmirStmt::Call {
                lhs: Some(TempId::from_usize(0)),
                callee: AmirOperand::FunctionRef(callee),
                args: smallvec::SmallVec::new(),
                return_borrow: None,
            }]
        });
        assert!(!safe(&called, &symbols, &interner));
    }

    #[test]
    fn string_backing_proof_has_a_fixed_depth_bound() {
        use arandu_middle::amir::AmirConstant;
        use arandu_middle::literal_pool::LiteralId;
        let interner = TypeInterner::new();
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        for (length, admitted) in [(64, true), (65, false)] {
            let (function, _) = shape(
                string,
                (0..length).map(|id| temp(id, string)).collect(),
                |_| {
                    (0..length)
                        .map(|id| AmirStmt::Assign {
                            lhs: TempId::from_usize(id),
                            rhs: AmirRvalue::Use(if id + 1 < length {
                                AmirOperand::Copy(TempId::from_usize(id + 1))
                            } else {
                                AmirOperand::Constant(AmirConstant::Pool(LiteralId(0)))
                            }),
                        })
                        .collect()
                },
            );
            assert_eq!(independent_string_result(&function, &interner), admitted);
        }
    }

    #[test]
    fn local_string_backing_rejects_mutable_aliases_and_unproved_stores() {
        use arandu_middle::amir::{AmirLocal, AmirPlace, LocalId};
        let interner = TypeInterner::new();
        let string = interner.intern(ArType::Primitive(Primitive::Str));
        let local = LocalId::from_usize(0);
        let place = AmirPlace {
            local,
            projections: smallvec::SmallVec::new(),
        };
        let (mut function, _symbols) = shape(
            string,
            vec![temp(0, string), temp(1, string), temp(2, string)],
            |_| {
                vec![
                    AmirStmt::Store {
                        lhs: place.clone(),
                        rhs: AmirOperand::Copy(TempId::from_usize(1)),
                    },
                    AmirStmt::Assign {
                        lhs: TempId::from_usize(0),
                        rhs: AmirRvalue::Load(place.clone()),
                    },
                ]
            },
        );
        function.params.push(TempId::from_usize(1));
        function.locals.push(AmirLocal {
            id: local,
            ty: string,
            is_memory: true,
            symbol: None,
            span: Span::new(0, 0, 0),
            use_span: None,
        });
        assert!(independent_string_result(&function, &interner));
        let mut aliased = function.clone();
        aliased.stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::BorrowMut(place.clone()),
        });
        assert!(!independent_string_result(&aliased, &interner));
        let mut overwritten = function.clone();
        overwritten.stmts.push(AmirStmt::Store {
            lhs: place.clone(),
            rhs: AmirOperand::Copy(TempId::from_usize(2)),
        });
        assert!(!independent_string_result(&overwritten, &interner));
        let mut destroyed = function.clone();
        destroyed.stmts.push(AmirStmt::Destroy(place));
        assert!(!independent_string_result(&destroyed, &interner));
        function.stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local,
                projections: smallvec::smallvec![arandu_middle::amir::AmirProjection::Index(
                    AmirOperand::Copy(TempId::from_usize(1))
                )],
            },
            rhs: AmirOperand::Copy(TempId::from_usize(1)),
        });
        assert!(!independent_string_result(&function, &interner));
    }

    #[test]
    fn generic_enum_payloads_use_concrete_lifetime_shapes() {
        use arandu_typeck::EnumPayloadShape;
        use std::sync::Arc;

        let mut info = TypeInfo::default();
        let interner = &info.type_interner;
        let owner = SymbolId::new(0, 20);
        let parameter = SymbolId::new(0, 21);
        let unit = SymbolId::new(0, 22);
        let payload = SymbolId::new(0, 23);
        let generic = interner.intern(ArType::Named(parameter, crate::hir::IndexRange::empty()));
        info.generic_params.insert(owner, Arc::new(vec![parameter]));
        info.enum_variants
            .insert(unit, (owner, EnumPayloadShape::Unit));
        info.enum_variants
            .insert(payload, (owner, EnumPayloadShape::Tuple(vec![generic])));
        info.enum_variant_tags.insert(unit, 0);
        info.enum_variant_tags.insert(payload, 1);
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let closed = interner.intern(ArType::Array(4, int));
        for (argument, admitted) in [
            (closed, true),
            (interner.intern(ArType::Primitive(Primitive::Str)), false),
            (interner.intern(ArType::Ref(int)), false),
            (interner.intern(ArType::Slice(int)), false),
            (interner.intern(ArType::Coroutine(int)), false),
        ] {
            let enum_ty =
                interner.intern(ArType::Named(owner, interner.push_type_args(&[argument])));
            assert_eq!(scratch_type_safe(enum_ty, interner, &info), admitted);
        }
        let malformed = interner.intern(ArType::Named(
            owner,
            interner.push_type_args(&[closed, int]),
        ));
        assert!(!scratch_type_safe(malformed, interner, &info));
    }

    #[test]
    fn acyclic_aggregate_joins_and_cyclic_value_handles_stay_admitted() {
        use arandu_middle::amir::{AmirConstant, BlockId, BlockParam, LocalId};

        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let byte = interner.intern(ArType::Primitive(Primitive::U8));
        let pointer = interner.intern(ArType::Ptr(byte));
        let gen_ref = interner.intern(ArType::GenRef);
        let array = interner.intern(ArType::Array(4, byte));
        let symbols = SymbolTable::new(0);

        let diamond_blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Copy(TempId::from_usize(0))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: vec![AmirOperand::Copy(TempId::from_usize(1))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                params: DenseRange::new(0, 1),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let diamond_cfg = compute_cfg_edges(&diamond_blocks);
        let diamond = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: vec![temp(0, array), temp(1, array), temp(2, array)],
            blocks: diamond_blocks,
            block_params: vec![BlockParam {
                id: TempId::from_usize(2),
                local: LocalId::from_usize(0),
                ty: array,
                from: None,
                moved: false,
            }],
            stmts: Default::default(),
            cfg: diamond_cfg,
        };
        assert!(
            safe(&diamond, &symbols, &interner),
            "acyclic diamond join with aggregate block param must stay admitted"
        );

        let loop_blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![AmirOperand::Copy(TempId::from_usize(0))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                params: DenseRange::new(0, 1),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                    if_true: BlockId::from_usize(2),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(3),
                    false_args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![AmirOperand::Copy(TempId::from_usize(1))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let loop_cfg = compute_cfg_edges(&loop_blocks);
        for handle_ty in [pointer, gen_ref] {
            let handle_loop = AmirFunc {
                symbol: SymbolId::new(0, 0),
                return_type: int,
                receiver: None,
                params: Vec::new(),
                locals: Vec::new(),
                temps: vec![temp(0, handle_ty), temp(1, handle_ty)],
                blocks: loop_blocks.clone(),
                block_params: vec![BlockParam {
                    id: TempId::from_usize(1),
                    local: LocalId::from_usize(0),
                    ty: handle_ty,
                    from: None,
                    moved: false,
                }],
                stmts: Default::default(),
                cfg: loop_cfg.clone(),
            };
            assert!(
                safe(&handle_loop, &symbols, &interner),
                "cyclic value-passed handle block param must stay admitted"
            );
        }

        let cyclic_aggregate = AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: int,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: vec![temp(0, array), temp(1, array)],
            blocks: loop_blocks,
            block_params: vec![BlockParam {
                id: TempId::from_usize(1),
                local: LocalId::from_usize(0),
                ty: array,
                from: None,
                moved: false,
            }],
            stmts: Default::default(),
            cfg: loop_cfg,
        };
        assert!(
            !safe(&cyclic_aggregate, &symbols, &interner),
            "cyclic aggregate block param must remain rejected"
        );
    }

    #[test]
    fn loop_carried_aggregate_locals_are_detected_only_inside_cycles() {
        use arandu_middle::amir::{AmirConstant, AmirLocal, AmirPlace, BlockId, LocalId};
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let named = interner.intern(ArType::named(SymbolId::new(0, 7), &[], &interner));
        // Stores local 0 once in each listed block (blocks in ascending order).
        let build = |local_ty: TypeId, store_blocks: &[usize]| {
            let mut stmts = AmirStmtTable::new();
            for _ in store_blocks {
                stmts.push(AmirStmt::Store {
                    lhs: AmirPlace {
                        local: LocalId::from_usize(0),
                        projections: smallvec![],
                    },
                    rhs: AmirOperand::Move(TempId::from_usize(0)),
                });
            }
            let range = |block: usize| match store_blocks.iter().position(|b| *b == block) {
                Some(index) => DenseRange::new(index, 1),
                None => DenseRange::empty(),
            };
            let blocks = vec![
                AmirBasicBlock {
                    id: BlockId::from_usize(0),
                    params: DenseRange::empty(),
                    statements: range(0),
                    terminator: AmirTerminator::Goto {
                        target: BlockId::from_usize(1),
                        args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: BlockId::from_usize(1),
                    params: DenseRange::empty(),
                    statements: range(1),
                    terminator: AmirTerminator::Branch {
                        condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                        if_true: BlockId::from_usize(2),
                        true_args: Vec::new(),
                        if_false: BlockId::from_usize(3),
                        false_args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: BlockId::from_usize(2),
                    params: DenseRange::empty(),
                    statements: range(2),
                    terminator: AmirTerminator::Goto {
                        target: BlockId::from_usize(1),
                        args: Vec::new(),
                    },
                },
                AmirBasicBlock {
                    id: BlockId::from_usize(3),
                    params: DenseRange::empty(),
                    statements: range(3),
                    terminator: AmirTerminator::Return,
                },
            ];
            let cfg = compute_cfg_edges(&blocks);
            AmirFunc {
                symbol: SymbolId::new(0, 0),
                return_type: int,
                receiver: None,
                params: Vec::new(),
                locals: vec![AmirLocal {
                    id: LocalId::from_usize(0),
                    ty: local_ty,
                    is_memory: true,
                    symbol: None,
                    span: Span::new(0, 0, 1),
                    use_span: None,
                }],
                temps: vec![temp(0, local_ty)],
                blocks,
                block_params: Vec::new(),
                stmts,
                cfg,
            }
        };
        // Initialized before the loop and reassigned inside it: loop-carried.
        assert!(cyclic_aggregate_local_store(
            &build(named, &[0, 2]),
            &interner
        ));
        assert!(cyclic_aggregate_local_store(
            &build(named, &[0, 1]),
            &interner
        ));
        // A single binding inside the loop is per-iteration.
        assert!(!cyclic_aggregate_local_store(
            &build(named, &[2]),
            &interner
        ));
        // Reassignment outside any cycle cannot meet a re-entered site.
        assert!(!cyclic_aggregate_local_store(
            &build(named, &[0, 3]),
            &interner
        ));
        assert!(!cyclic_aggregate_local_store(
            &build(int, &[0, 2]),
            &interner
        ));
    }
}
