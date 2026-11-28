//! Last-use copy elision (`Copy` -> `Move`) and `W010` (`large_implicit_copy`) lint.
//!
//! Uses intraprocedural liveness so only genuinely dead-after-use operands are
//! demoted to moves, and `W010` only fires when a large `Copy` value is actually
//! duplicated while still live.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::amir::visit::{
    for_each_stmt_operand, for_each_stmt_operand_mut, for_each_terminator_operand,
    for_each_terminator_operand_mut,
};
use crate::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt, BlockId, InstrId, TempId};
use crate::layout::{DataLayout, LayoutEngine, StructLayoutProvider};
use crate::liveness::{analyze_local_liveness, analyze_temp_liveness};
use crate::types::TypeInterner;
use crate::{DiagCode, Diagnostic, Span, SymbolTable};

/// Default target-size threshold (in bytes) for [`DiagCode::W010LargeImplicitCopy`].
pub const DEFAULT_LARGE_COPY_THRESHOLD_BYTES: u64 = 256;

/// Demotes `AmirOperand::Copy(temp)` to `AmirOperand::Move(temp)` whenever
/// `temp` is dead after the instruction/terminator and appears at most once
/// in that instruction/terminator.
///
/// Returns `true` if any operand was rewritten.
pub fn demote_last_use_copies(func: &mut AmirFunc) -> bool {
    if func.blocks.is_empty() || func.temps.is_empty() {
        return false;
    }

    let temp_liveness = analyze_temp_liveness(func);
    let mut changed = false;

    for block_idx in 0..func.blocks.len() {
        let block_id = func.blocks[block_idx].id;
        let mut live = temp_liveness.live_out(block_id).clone();

        // 1. Terminator: count occurrences of each temp so a duplicate operand
        // within the same terminator is never demoted prematurely.
        let mut term_counts: FxHashMap<TempId, usize> = FxHashMap::default();
        for_each_terminator_operand(&func.blocks[block_idx].terminator, |op| {
            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                *term_counts.entry(*t).or_insert(0) += 1;
            }
        });
        for_each_terminator_operand_mut(&mut func.blocks[block_idx].terminator, |op| {
            if let AmirOperand::Copy(t) = *op
                && !live.contains(t)
                && term_counts.get(&t).copied() == Some(1)
            {
                *op = AmirOperand::Move(t);
                changed = true;
            }
        });
        for &t in term_counts.keys() {
            live.insert(t);
        }

        // 2. Statements in reverse order.
        let stmt_ids: Vec<InstrId> = func.blocks[block_idx]
            .statements
            .iter_ids::<InstrId>()
            .collect();
        for instr_id in stmt_ids.into_iter().rev() {
            let def = match func.stmt(instr_id) {
                AmirStmt::Assign { lhs, .. } => Some(*lhs),
                AmirStmt::Call { lhs: Some(t), .. } => Some(*t),
                _ => None,
            };
            if let Some(d) = def {
                live.remove(d);
            }

            let mut stmt_counts: FxHashMap<TempId, usize> = FxHashMap::default();
            for_each_stmt_operand(func.stmt(instr_id), |op| {
                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                    *stmt_counts.entry(*t).or_insert(0) += 1;
                }
            });
            for_each_stmt_operand_mut(func.stmt_mut(instr_id), |op| {
                if let AmirOperand::Copy(t) = *op
                    && !live.contains(t)
                    && stmt_counts.get(&t).copied() == Some(1)
                {
                    *op = AmirOperand::Move(t);
                    changed = true;
                }
            });
            for &t in stmt_counts.keys() {
                live.insert(t);
            }
        }
    }

    changed
}

/// Checks for implicit copies of large `Copy` values (`>= threshold_bytes` in
/// `data_layout`) that are duplicated while still live, tagging each diagnostic
/// with its AMIR block.
#[must_use]
pub fn check_large_implicit_copies_by_block(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    types: &dyn StructLayoutProvider,
    data_layout: DataLayout,
    threshold_bytes: u64,
) -> Vec<(BlockId, Diagnostic)> {
    if func.blocks.is_empty() || threshold_bytes == 0 {
        return Vec::new();
    }

    let local_liveness = analyze_local_liveness(func);
    let temp_liveness = analyze_temp_liveness(func);
    let engine = LayoutEngine::from_data_layout(data_layout);
    let mut reported: FxHashSet<(Span, u32)> = FxHashSet::default();
    let mut diagnostics = Vec::new();

    for block in &func.blocks {
        let block_id = block.id;
        let stmts: Vec<&AmirStmt> = func.block_stmts(block_id).collect();

        // Check whole-local loads of large `Copy` locals that remain live after
        // the load statement.
        for (stmt_idx, stmt) in stmts.iter().enumerate() {
            let AmirStmt::Assign {
                lhs,
                rhs: AmirRvalue::Load(place),
            } = stmt
            else {
                continue;
            };
            if !place.projections.is_empty() {
                continue;
            }
            let Some(local_info) = func.locals.get(place.local.as_usize()) else {
                continue;
            };
            if types.is_copy_type(local_info.ty) != Some(true) {
                continue;
            }
            let still_live = local_liveness
                .live_before(block_id, stmt_idx + 1)
                .is_some_and(|live_after| live_after.contains(place.local));
            if !still_live {
                continue;
            }
            let Ok(layout) = engine.layout_of(local_info.ty, interner, types) else {
                continue;
            };
            if layout.size < threshold_bytes {
                continue;
            }
            let diag_span = func
                .temps
                .get(lhs.as_usize())
                .map_or(local_info.span, |temp| temp.span);
            if !reported.insert((diag_span, place.local.as_usize() as u32)) {
                continue;
            }
            let ty_name = interner.resolve(local_info.ty).display(symbols, interner);
            let diag = Diagnostic::warning(
                DiagCode::W010LargeImplicitCopy,
                format!(
                    "implicit copy of large '{ty_name}' value ({} bytes)",
                    layout.size
                ),
                diag_span,
            )
            .with_label(
                diag_span,
                format!("'{ty_name}' is duplicated here while still live afterward"),
            )
            .with_hint(format!(
                "pass by shared reference (`ref {ty_name}`) or annotate the type with `@NoCopy`"
            ));
            diagnostics.push((block_id, diag));
        }

        // Check SSA temp copies of large `Copy` values that remain live after
        // the use (e.g. duplicated temp operands).
        let mut live_temps = temp_liveness.live_out(block_id).clone();
        let mut check_temp_copy = |temp: TempId, fallback_span: Option<Span>, still_live: bool| {
            if !still_live {
                return;
            }
            let Some(temp_info) = func.temps.get(temp.as_usize()) else {
                return;
            };
            if types.is_copy_type(temp_info.ty) != Some(true) {
                return;
            }
            let Ok(layout) = engine.layout_of(temp_info.ty, interner, types) else {
                return;
            };
            if layout.size < threshold_bytes {
                return;
            }
            let diag_span = fallback_span
                .filter(|s| s.start != s.end)
                .unwrap_or(temp_info.span);
            // Namespace temp keys distinctly from local keys via high bit.
            let key = (temp.as_usize() as u32) | 0x8000_0000;
            if !reported.insert((diag_span, key)) {
                return;
            }
            let ty_name = interner.resolve(temp_info.ty).display(symbols, interner);
            let diag = Diagnostic::warning(
                DiagCode::W010LargeImplicitCopy,
                format!(
                    "implicit copy of large '{ty_name}' value ({} bytes)",
                    layout.size
                ),
                diag_span,
            )
            .with_label(
                diag_span,
                format!("'{ty_name}' is duplicated here while still live afterward"),
            )
            .with_hint(format!(
                "pass by shared reference (`ref {ty_name}`) or annotate the type with `@NoCopy`"
            ));
            diagnostics.push((block_id, diag));
        };

        let mut term_counts: FxHashMap<TempId, usize> = FxHashMap::default();
        for_each_terminator_operand(&block.terminator, |op| {
            if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                *term_counts.entry(*t).or_insert(0) += 1;
            }
        });
        for_each_terminator_operand(&block.terminator, |op| {
            if let AmirOperand::Copy(t) = *op {
                let dup_in_term = term_counts.get(&t).copied().unwrap_or(0) > 1;
                check_temp_copy(t, None, live_temps.contains(t) || dup_in_term);
            }
        });
        for &t in term_counts.keys() {
            live_temps.insert(t);
        }

        for stmt in stmts.into_iter().rev() {
            let def = match stmt {
                AmirStmt::Assign { lhs, .. } => Some(*lhs),
                AmirStmt::Call { lhs: Some(t), .. } => Some(*t),
                _ => None,
            };
            let stmt_span = def
                .and_then(|d| func.temps.get(d.as_usize()))
                .map(|t| t.span);
            if let Some(d) = def {
                live_temps.remove(d);
            }
            let mut stmt_counts: FxHashMap<TempId, usize> = FxHashMap::default();
            for_each_stmt_operand(stmt, |op| {
                if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
                    *stmt_counts.entry(*t).or_insert(0) += 1;
                }
            });
            for_each_stmt_operand(stmt, |op| {
                if let AmirOperand::Copy(t) = *op {
                    let dup_in_stmt = stmt_counts.get(&t).copied().unwrap_or(0) > 1;
                    check_temp_copy(t, stmt_span, live_temps.contains(t) || dup_in_stmt);
                }
            });
            for &t in stmt_counts.keys() {
                live_temps.insert(t);
            }
        }
    }

    diagnostics.sort_by_key(|(bid, d)| (bid.as_usize(), d.span.start, d.span.end));
    diagnostics
}

/// Same as [`check_large_implicit_copies_by_block`], returning only diagnostics.
#[must_use]
pub fn check_large_implicit_copies(
    func: &AmirFunc,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    types: &dyn StructLayoutProvider,
    data_layout: DataLayout,
    threshold_bytes: u64,
) -> Vec<Diagnostic> {
    check_large_implicit_copies_by_block(
        func,
        symbols,
        interner,
        types,
        data_layout,
        threshold_bytes,
    )
    .into_iter()
    .map(|(_, d)| d)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::program::extend_block_range;
    use crate::amir::{
        AmirBasicBlock, AmirConstant, AmirLocal, AmirPlace, AmirStmtTable, AmirTemp,
        AmirTerminator, LocalId,
    };
    use crate::layout::{DenseRange, StructFields, StructLayoutProvider};
    use crate::types::{ArType, Primitive};
    use arandu_middle::types::TypeId;
    use smallvec::smallvec;

    struct DummyTypes {
        copy_types: FxHashSet<TypeId>,
    }

    impl StructLayoutProvider for DummyTypes {
        fn get_struct_fields(&self, _sym: crate::SymbolId) -> Option<&StructFields> {
            None
        }
        fn get_generic_params(&self, _sym: crate::SymbolId) -> Option<&[crate::SymbolId]> {
            None
        }
        fn get_enum_variants(
            &self,
            _sym: crate::SymbolId,
        ) -> Option<Vec<arandu_middle::EnumPayloadShape>> {
            None
        }
        fn is_copy_type(&self, ty: TypeId) -> Option<bool> {
            Some(self.copy_types.contains(&ty))
        }
    }

    fn make_block(
        id: usize,
        statements: Vec<AmirStmt>,
        terminator: AmirTerminator,
        stmts: &mut AmirStmtTable,
    ) -> AmirBasicBlock {
        let mut range = DenseRange::empty();
        for stmt in statements {
            let instr = stmts.push(stmt);
            extend_block_range(&mut range, instr);
        }
        AmirBasicBlock {
            id: BlockId::from_usize(id),
            statements: range,
            params: DenseRange::empty(),
            terminator,
        }
    }

    #[test]
    fn demotes_last_use_copy_to_move_and_preserves_live_copy() {
        let interner = TypeInterner::new();
        let int_ty = interner.intern(ArType::Primitive(Primitive::Int));

        let t0 = TempId::from_usize(0);
        let t1 = TempId::from_usize(1);
        let t2 = TempId::from_usize(2);
        let temps = vec![
            AmirTemp {
                id: t0,
                ty: int_ty,
                span: Span::new(0, 0, 1),
                is_copy: true,
                is_nullable: false,
            },
            AmirTemp {
                id: t1,
                ty: int_ty,
                span: Span::new(0, 2, 3),
                is_copy: true,
                is_nullable: false,
            },
            AmirTemp {
                id: t2,
                ty: int_ty,
                span: Span::new(0, 4, 5),
                is_copy: true,
                is_nullable: false,
            },
        ];

        // t0 = 42
        // t1 = Use(Copy(t0))   <-- t0 is still live afterward: MUST stay Copy
        // t2 = Use(Copy(t0))   <-- last use of t0: MUST become Move
        // goto bb1(Copy(t2))   <-- last use of t2: MUST become Move
        let mut stmts = AmirStmtTable::new();
        let blocks = vec![
            make_block(
                0,
                vec![
                    AmirStmt::Assign {
                        lhs: t0,
                        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                    },
                    AmirStmt::Assign {
                        lhs: t1,
                        rhs: AmirRvalue::Use(AmirOperand::Copy(t0)),
                    },
                    AmirStmt::Assign {
                        lhs: t2,
                        rhs: AmirRvalue::Use(AmirOperand::Copy(t0)),
                    },
                ],
                AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![AmirOperand::Copy(t2)],
                },
                &mut stmts,
            ),
            make_block(1, vec![], AmirTerminator::Return, &mut stmts),
        ];
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        let mut func = AmirFunc {
            symbol: crate::SymbolId::new(0, 1),
            return_type: int_ty,
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps,
            blocks,
            block_params: Vec::new(),
            stmts,
            cfg,
        };

        assert!(demote_last_use_copies(&mut func));
        let stmt_ids: Vec<InstrId> = func.block_stmt_ids(BlockId::from_usize(0)).collect();
        assert!(
            matches!(
                func.stmt(stmt_ids[1]),
                AmirStmt::Assign {
                    rhs: AmirRvalue::Use(AmirOperand::Copy(t)),
                    ..
                } if *t == t0
            ),
            "non-last use must stay Copy"
        );
        assert!(
            matches!(
                func.stmt(stmt_ids[2]),
                AmirStmt::Assign {
                    rhs: AmirRvalue::Use(AmirOperand::Move(t)),
                    ..
                } if *t == t0
            ),
            "last use in statement must become Move"
        );
        assert!(
            matches!(
                &func.blocks[0].terminator,
                AmirTerminator::Goto { args, .. } if matches!(args.as_slice(), [AmirOperand::Move(t)] if *t == t2)
            ),
            "last use in terminator must become Move"
        );
        // Idempotent on second run.
        assert!(!demote_last_use_copies(&mut func));
    }

    #[test]
    fn large_implicit_copy_warns_only_when_duplicated_while_live() {
        let interner = TypeInterner::new();
        let symbols = SymbolTable::new(0);
        let f64_ty = interner.intern(ArType::Primitive(Primitive::F64));
        let big_arr_ty = interner.intern(ArType::Array(64, f64_ty)); // 512 bytes

        let mut copy_types = FxHashSet::default();
        copy_types.insert(big_arr_ty);
        let types = DummyTypes { copy_types };

        let l0 = LocalId::from_usize(0);
        let locals = vec![AmirLocal {
            id: l0,
            symbol: None,
            ty: big_arr_ty,
            is_memory: true,
            span: Span::new(0, 0, 4),
            use_span: None,
        }];
        let t0 = TempId::from_usize(0);
        let t1 = TempId::from_usize(1);
        let temps = vec![
            AmirTemp {
                id: t0,
                ty: big_arr_ty,
                span: Span::new(0, 10, 12),
                is_copy: true,
                is_nullable: false,
            },
            AmirTemp {
                id: t1,
                ty: big_arr_ty,
                span: Span::new(0, 20, 22),
                is_copy: true,
                is_nullable: false,
            },
        ];
        // First load duplicates l0 (still live for second load) -> W010.
        // Second load is last use of l0 -> no W010.
        let mut stmts = AmirStmtTable::new();
        let blocks = vec![make_block(
            0,
            vec![
                AmirStmt::Assign {
                    lhs: t0,
                    rhs: AmirRvalue::Load(AmirPlace {
                        local: l0,
                        projections: smallvec![],
                    }),
                },
                AmirStmt::Assign {
                    lhs: t1,
                    rhs: AmirRvalue::Load(AmirPlace {
                        local: l0,
                        projections: smallvec![],
                    }),
                },
            ],
            AmirTerminator::Return,
            &mut stmts,
        )];
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        let func = AmirFunc {
            symbol: crate::SymbolId::new(0, 1),
            return_type: big_arr_ty,
            receiver: None,
            params: Vec::new(),
            locals,
            temps,
            blocks,
            block_params: Vec::new(),
            stmts,
            cfg,
        };

        let diags = check_large_implicit_copies(
            &func,
            &symbols,
            &interner,
            &types,
            DataLayout::ptr_width(8),
            DEFAULT_LARGE_COPY_THRESHOLD_BYTES,
        );
        assert_eq!(diags.len(), 1, "expected exactly 1 W010, got: {diags:?}");
        assert_eq!(diags[0].code, DiagCode::W010LargeImplicitCopy);
        assert_eq!(diags[0].span, Span::new(0, 10, 12));
    }
}
