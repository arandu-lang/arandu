//! Structural hashing for Salsa memo equality (RC-HASHEQ).
//!
//! Never uses full `Debug` of IR graphs — only deterministic fields (IDs,
//! spans, diagnostic codes, counts, ordered maps).

mod amir;
mod nodes;
mod symbols;
mod typeck;

pub use symbols::StableHash;
pub(crate) use symbols::{hash_literal_entry, hash_return_borrow_summary};
pub(crate) use typeck::type_signature_hash;

#[cfg(test)]
mod tests {
    use super::{type_signature_hash, StableHash};

    fn amir_func(terminator: arandu_middle::amir::AmirTerminator) -> arandu_middle::amir::AmirFunc {
        use arandu_middle::amir::{AmirFunc, AmirStmtTable};
        use arandu_middle::layout::DenseRange;
        use arandu_middle::types::{ArType, TypeInterner};

        let interner = TypeInterner::new();
        AmirFunc {
            symbol: arandu_middle::SymbolId::new(0, 0),
            return_type: interner.intern(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: Vec::new(),
            blocks: vec![arandu_middle::amir::AmirBasicBlock {
                id: arandu_middle::amir::BlockId::from_usize(0),
                params: DenseRange::empty(),
                statements: DenseRange::empty(),
                terminator,
            }],
            block_params: Vec::new(),
            stmts: AmirStmtTable::new(),
            cfg: arandu_middle::cfg::ControlFlowGraph::default(),
        }
    }

    #[test]
    fn resolution_hash_distinguishes_symbol_kind_with_identical_name_and_span() {
        use arandu_middle::{ResolutionResult, Span, SymbolKind};
        let result = |kind| {
            let mut result = ResolutionResult::cycle_fallback();
            let scope = result.symbols.global_scope();
            std::sync::Arc::make_mut(&mut result.symbols)
                .define(scope, "value", kind, Span::new(0, 0, 5))
                .unwrap();
            result
        };
        let local = result(SymbolKind::Local);
        let parameter = result(SymbolKind::Param);
        assert_ne!(local.stable_hash(), parameter.stable_hash());
        assert_eq!(local.stable_hash(), result(SymbolKind::Local).stable_hash());
    }

    #[test]
    fn program_hash_changes_when_only_a_literal_changes() {
        let first = arandu_parser::parse("func answer(): int { return 1 }").unwrap();
        let second = arandu_parser::parse("func answer(): int { return 2 }").unwrap();
        assert_ne!(Ok(first).stable_hash(), Ok(second).stable_hash());
    }

    #[test]
    fn graph_hash_changes_when_edge_changes() {
        let mut g1 = petgraph::Graph::<u32, ()>::new();
        let n0 = g1.add_node(1);
        let n1 = g1.add_node(2);
        let n2 = g1.add_node(3);
        g1.add_edge(n0, n1, ());
        g1.add_edge(n1, n2, ());

        let mut g2 = petgraph::Graph::<u32, ()>::new();
        let n0 = g2.add_node(1);
        let n1 = g2.add_node(2);
        let n2 = g2.add_node(3);
        g2.add_edge(n0, n2, ());
        g2.add_edge(n1, n2, ());

        assert_ne!(g1.stable_hash(), g2.stable_hash());
    }

    #[test]
    fn amir_program_hash_changes_when_literal_pool_changes() {
        let mut p1 = arandu_middle::amir::AmirProgram {
            funcs: Vec::new(),
            literal_pool: arandu_middle::literal_pool::AmirLiteralPool::default(),
            extern_funcs: rustc_hash::FxHashMap::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        let mut p2 = arandu_middle::amir::AmirProgram {
            funcs: Vec::new(),
            literal_pool: arandu_middle::literal_pool::AmirLiteralPool::default(),
            extern_funcs: rustc_hash::FxHashMap::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        p1.literal_pool.intern_int("42");
        p2.literal_pool.intern_int("43");
        assert_ne!(p1.stable_hash(), p2.stable_hash());
    }

    #[test]
    fn amir_hash_tracks_statement_operands_and_terminator_arguments() {
        use arandu_middle::amir::{AmirConstant, AmirOperand, AmirStmt, AmirTerminator, BlockId};

        let mut first = amir_func(AmirTerminator::Goto {
            target: BlockId::from_usize(0),
            args: vec![AmirOperand::Constant(AmirConstant::Bool(false))],
        });
        let second = amir_func(AmirTerminator::Goto {
            target: BlockId::from_usize(0),
            args: vec![AmirOperand::Constant(AmirConstant::Bool(true))],
        });
        assert_ne!(first.stable_hash(), second.stable_hash());

        first
            .stmts
            .push(AmirStmt::Free(AmirOperand::Constant(AmirConstant::Bool(
                false,
            ))));
        let mut changed_stmt = first.clone();
        *changed_stmt
            .stmts
            .get_mut(arandu_middle::amir::InstrId::from_usize(0))
            .unwrap() = AmirStmt::Free(AmirOperand::Constant(AmirConstant::Bool(true)));
        assert_ne!(first.stable_hash(), changed_stmt.stable_hash());
    }

    #[test]
    fn amir_program_hash_tracks_debug_bindings() {
        use arandu_middle::amir::{AmirDebugBinding, AmirProgram, LocalId, TempId};

        let make_program = |local| AmirProgram {
            funcs: Vec::new(),
            literal_pool: arandu_middle::literal_pool::AmirLiteralPool::default(),
            extern_funcs: rustc_hash::FxHashMap::default(),
            debug_bindings: vec![AmirDebugBinding {
                function: arandu_middle::SymbolId::new(0, 1),
                temp: TempId::from_usize(2),
                local: LocalId::from_usize(local),
            }],
            debug_blocks: Vec::new(),
        };
        assert_ne!(make_program(3).stable_hash(), make_program(4).stable_hash());
    }

    #[test]
    fn amir_program_hash_tracks_extern_signatures_independent_of_map_order() {
        use arandu_middle::amir::AmirProgram;
        use arandu_middle::types::{ArType, Primitive};
        use arandu_middle::SymbolId;

        let first_symbol = SymbolId::new(0, 1);
        let second_symbol = SymbolId::new(0, 2);
        let make_program = |reverse: bool, second_param: Primitive| {
            let mut extern_funcs = rustc_hash::FxHashMap::default();
            let first = (
                first_symbol,
                (vec![ArType::Primitive(Primitive::Int)], ArType::Void),
            );
            let second = (
                second_symbol,
                (vec![ArType::Primitive(second_param)], ArType::Void),
            );
            if reverse {
                extern_funcs.insert(second.0, second.1);
                extern_funcs.insert(first.0, first.1);
            } else {
                extern_funcs.insert(first.0, first.1);
                extern_funcs.insert(second.0, second.1);
            }
            AmirProgram {
                funcs: Vec::new(),
                literal_pool: arandu_middle::literal_pool::AmirLiteralPool::default(),
                extern_funcs,
                debug_bindings: Vec::new(),
                debug_blocks: Vec::new(),
            }
        };

        assert_eq!(
            make_program(false, Primitive::Bool).stable_hash(),
            make_program(true, Primitive::Bool).stable_hash()
        );
        assert_ne!(
            make_program(false, Primitive::Bool).stable_hash(),
            make_program(false, Primitive::Char).stable_hash()
        );
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn type_signatures_distinguish_frozen_aggregate_arguments_with_equal_display() {
        use arandu_middle::ctfe::{ConstAggregate, ConstValue};
        use arandu_middle::types::{ArType, Primitive, TypeShape};
        use arandu_middle::SymbolId;
        use arandu_semantics::TypeCheckResult;
        use std::sync::Arc;

        let make = |flag| {
            let mut checked = TypeCheckResult::empty();
            let info = Arc::make_mut(&mut checked.type_info);
            let frozen = ConstAggregate::new(
                TypeShape::Tuple(vec![TypeShape::Primitive(Primitive::Bool)]),
                vec![ConstValue::Bool(flag)],
            )
            .expect("closed constant tuple");
            let argument = info
                .type_interner
                .intern(ArType::FrozenConst(Arc::new(ConstValue::Aggregate(frozen))));
            let signature = info.type_interner.intern(ArType::named(
                SymbolId::new(100, 8),
                &[argument],
                &info.type_interner,
            ));
            info.record_decl_type(SymbolId::new(100, 7), signature);
            checked
        };
        let a = make(false);
        let b = make(true);
        let presentation = |checked: &TypeCheckResult| {
            checked.type_info.type_interner.display(
                checked
                    .type_info
                    .decl_type_id(SymbolId::new(100, 7))
                    .expect("signature"),
                &checked.symbols,
            )
        };
        assert_eq!(presentation(&a), presentation(&b));
        assert_ne!(type_signature_hash(&a), type_signature_hash(&b));
        assert_ne!(a.stable_hash(), b.stable_hash());
        assert_eq!(type_signature_hash(&a), type_signature_hash(&make(false)));
    }

    #[test]
    fn cached_instance_hashes_do_not_depend_on_interner_allocation_order() {
        use arandu_middle::types::{ArType, Primitive};
        use arandu_middle::SymbolId;
        use arandu_semantics::TypeCheckResult;
        use std::sync::Arc;

        let make = |reverse| {
            let mut checked = TypeCheckResult::empty();
            let info = Arc::make_mut(&mut checked.type_info);
            let interner = &info.type_interner;
            let int = interner.intern(ArType::Primitive(Primitive::Int));
            let flag = interner.intern(ArType::Primitive(Primitive::Bool));
            let choices = [(ArType::Array(2, int), 1), (ArType::Array(3, flag), 2)];
            for index in if reverse { [1, 0] } else { [0, 1] } {
                let (ty, identity) = &choices[index];
                let tid = interner.intern(ty.clone());
                info.destructor_instances
                    .insert(tid, SymbolId::new(100, *identity));
                info.variant_instantiations
                    .insert((SymbolId::new(100, 0), vec![tid]), (vec![tid], tid));
            }
            checked
        };
        assert_eq!(make(false).stable_hash(), make(true).stable_hash());
        assert_eq!(
            type_signature_hash(&make(false)),
            type_signature_hash(&make(true))
        );
    }

    #[test]
    fn type_check_hash_tracks_codegen_and_safety_metadata() {
        use arandu_middle::{EffectFlags, SymbolId};
        use std::sync::Arc;

        let baseline = arandu_semantics::TypeCheckResult::empty();
        let baseline_hash = baseline.stable_hash();
        let symbol = SymbolId::new(100, 7);

        let mut with_effect = baseline.clone();
        Arc::make_mut(&mut with_effect.type_info)
            .function_effects
            .insert(symbol, EffectFlags::FILE_READ);
        assert_ne!(baseline_hash, with_effect.stable_hash());

        let mut with_interior_mutability = baseline.clone();
        Arc::make_mut(&mut with_interior_mutability.type_info)
            .interior_mutable_types
            .insert(symbol);
        assert_ne!(baseline_hash, with_interior_mutability.stable_hash());

        let mut with_repr_c = baseline.clone();
        Arc::make_mut(&mut with_repr_c.type_info)
            .struct_repr_c
            .insert(symbol);
        assert_ne!(baseline_hash, with_repr_c.stable_hash());

        let mut with_destructor = baseline.clone();
        Arc::make_mut(&mut with_destructor.type_info)
            .destructors
            .insert(symbol, SymbolId::new(100, 8));
        assert_ne!(baseline_hash, with_destructor.stable_hash());

        let mut with_builtin = baseline.clone();
        Arc::make_mut(&mut with_builtin.symbols).builtin_alloc = Some(symbol);
        assert_ne!(baseline_hash, with_builtin.stable_hash());

        let mut with_scope = baseline.clone();
        Arc::make_mut(&mut with_scope.symbols).new_scope(arandu_middle::ScopeId(0));
        assert_ne!(baseline_hash, with_scope.stable_hash());

        let mut with_resolution = baseline.clone();
        Arc::make_mut(&mut with_resolution.resolved)
            .value_ref(arandu_middle::Span::new(100, 4, 9), symbol);
        assert_ne!(baseline_hash, with_resolution.stable_hash());
        assert_eq!(
            type_signature_hash(&baseline),
            type_signature_hash(&with_resolution),
            "implementation references must not invalidate a module signature"
        );

        let another_symbol = SymbolId::new(100, 3);
        let mut first_order = baseline.clone();
        let first_effects = &mut Arc::make_mut(&mut first_order.type_info).function_effects;
        first_effects.insert(symbol, EffectFlags::FILE_READ);
        first_effects.insert(another_symbol, EffectFlags::THREAD);

        let mut reverse_order = baseline;
        let reverse_effects = &mut Arc::make_mut(&mut reverse_order.type_info).function_effects;
        reverse_effects.insert(another_symbol, EffectFlags::THREAD);
        reverse_effects.insert(symbol, EffectFlags::FILE_READ);
        assert_eq!(first_order.stable_hash(), reverse_order.stable_hash());
    }
}

#[cfg(test)]
mod vector_hash_tests {
    use super::nodes::{hash_ar_type, hash_rvalue};
    use arandu_middle::{
        amir::{AmirOperand, AmirRvalue, TempId, VectorOp},
        types::{ArType, VectorType},
    };
    use blake3::Hasher;

    #[test]
    fn vector_hashes_track_shape_operation_lane_mask_and_operands() {
        let mut shapes = std::collections::HashSet::new();
        for shape in VectorType::ALL {
            let mut hash = Hasher::new();
            hash_ar_type(&mut hash, &ArType::Vector(shape));
            assert!(shapes.insert(hash.finalize()));
        }
        let a = AmirOperand::Copy(TempId::from_usize(1));
        let b = AmirOperand::Copy(TempId::from_usize(2));
        let mut reversed = std::array::from_fn(|index| u8::try_from(index).unwrap());
        reversed[0] = 16;
        let cases = [
            AmirRvalue::VectorOp {
                op: VectorOp::Add,
                left: a,
                right: b,
            },
            AmirRvalue::VectorOp {
                op: VectorOp::Sub,
                left: a,
                right: b,
            },
            AmirRvalue::VectorOp {
                op: VectorOp::Add,
                left: b,
                right: a,
            },
            AmirRvalue::VectorCompare {
                op: arandu_middle::amir::VectorCompareOp::Eq,
                left: a,
                right: b,
            },
            AmirRvalue::VectorCompare {
                op: arandu_middle::amir::VectorCompareOp::Ne,
                left: a,
                right: b,
            },
            AmirRvalue::VectorSelect {
                mask: a,
                if_true: a,
                if_false: b,
            },
            AmirRvalue::VectorSelect {
                mask: b,
                if_true: a,
                if_false: b,
            },
            AmirRvalue::VectorSplat { value: a },
            AmirRvalue::VectorShuffle {
                left: a,
                right: b,
                mask: std::array::from_fn(|index| u8::try_from(index).unwrap()),
            },
            AmirRvalue::VectorShuffle {
                left: a,
                right: b,
                mask: reversed,
            },
            AmirRvalue::VectorExtractLane { vector: a, lane: 0 },
            AmirRvalue::VectorExtractLane { vector: a, lane: 1 },
            AmirRvalue::VectorInsertLane {
                vector: a,
                lane: 0,
                value: b,
            },
        ];
        let mut hashes = std::collections::HashSet::new();
        for value in cases {
            let mut hash = Hasher::new();
            hash_rvalue(&mut hash, &value);
            assert!(hashes.insert(hash.finalize()));
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod authority_hash_tests {
    use super::{type_signature_hash, StableHash};
    use arandu_semantics::TypeCheckResult;

    #[test]
    fn semantic_signature_hash_includes_lang_item_identity_without_source_spans() {
        use arandu_middle::symbol_table::{LangItem, SymbolKind};
        let mut plain = TypeCheckResult::empty();
        let scope = plain.symbols.global_scope();
        let id = std::sync::Arc::make_mut(&mut plain.symbols)
            .define(
                scope,
                "Owner",
                SymbolKind::Struct,
                arandu_middle::Span::new(100, 105, 0),
            )
            .expect("fresh declaration");
        let mut canonical = plain.clone();
        std::sync::Arc::make_mut(&mut canonical.symbols).set_lang_item(id, LangItem::Arc);
        assert_ne!(type_signature_hash(&plain), type_signature_hash(&canonical));
    }

    #[test]
    fn semantic_hashes_include_sealed_interface_metadata() {
        let baseline = TypeCheckResult::empty();
        let interface = arandu_middle::SymbolId::new(100, 7);
        let ty = arandu_middle::SymbolId::new(100, 8);

        let mut sealed = baseline.clone();
        std::sync::Arc::make_mut(&mut sealed.symbols)
            .sealed_interfaces
            .insert(interface);
        assert_ne!(baseline.stable_hash(), sealed.stable_hash());
        assert_ne!(type_signature_hash(&baseline), type_signature_hash(&sealed));

        let mut implemented = baseline.clone();
        std::sync::Arc::make_mut(&mut implemented.symbols)
            .interface_implementations
            .insert((ty, interface));
        assert_ne!(baseline.stable_hash(), implemented.stable_hash());
        assert_ne!(
            type_signature_hash(&baseline),
            type_signature_hash(&implemented)
        );
    }

    #[test]
    fn semantic_signature_hash_includes_private_field_visibility() {
        let baseline = TypeCheckResult::empty();
        let field = arandu_middle::SymbolId::new(100, 9);
        let mut private = baseline.clone();
        std::sync::Arc::make_mut(&mut private.type_info)
            .private_fields
            .insert(field);

        assert_ne!(baseline.stable_hash(), private.stable_hash());
        assert_ne!(
            type_signature_hash(&baseline),
            type_signature_hash(&private)
        );
    }
}
