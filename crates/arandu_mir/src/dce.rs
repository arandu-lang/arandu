use crate::amir::{
    AmirFunc, AmirOperand, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator, BlockId, TempId,
    for_each_rvalue_operand, for_each_terminator_operand,
};
use crate::layout::DenseRange;
use crate::{DiagCode, Diagnostic, Span};
use smallvec::SmallVec;
use std::collections::VecDeque;

/// Mark-Sweep Dead Code Elimination (Cooper & Torczon, Engineering a Compiler, ch. 10).
///
/// Phase 1 — Mark: seeds are side-effecting instructions (Call, Store, Free, Destroy,
/// StorageLive/Dead, Alloc).  Transitively marks the defining instruction of every
/// temp operand used by a live instruction.  **All** terminator operand uses are
/// seeds (conditions **and** block-param jump args via [`for_each_terminator_operand`]).
///
/// Phase 2 — Sweep: removes every unmarked statement by rebuilding the stmt table.
///
/// Returns `true` if any instruction was removed.
pub fn mark_sweep_dce(func: &mut AmirFunc) -> Result<bool, Diagnostic> {
    let n_stmts = func.stmts.len();
    let n_temps = func.temps.len();
    if n_stmts == 0 || n_temps == 0 {
        return Ok(false);
    }

    // --- def map: TempId → all defining instructions ----------------------
    // AMIR temps are usually SSA, but branch results (including `_0`) may
    // currently have one definition per incoming path before phi materializes
    // them as block parameters. A single last-def map can select a definition
    // on an unreachable arm and delete the one that reaches a live use.
    let mut defs_of = vec![SmallVec::<[crate::amir::InstrId; 2]>::new(); n_temps];
    for bi in 0..func.blocks.len() {
        let bid = BlockId::from_usize(bi);
        for stmt_id in func.block_stmt_ids(bid) {
            if let AmirStmt::Assign { lhs, .. } = func.stmt(stmt_id) {
                defs_of[lhs.as_usize()].push(stmt_id);
            }
        }
    }

    // --- Mark phase ---------------------------------------------------------
    //
    // Principle: the roots of a DCE mark-phase are everything observable from
    // outside the function:
    //   - Return value (`_0` in AMIR convention) — **every** path's def
    //   - Side effects (Call, Store, Free, Destroy, Alloc, StorageLive/Dead)
    //   - Terminator operand uses (conditions + jump args → block params)
    //
    // Future roots to add when the language grows:
    //   - Mutable reference parameters (writes visible to caller)
    //   - Closure captures by reference
    //
    let mut live = vec![false; n_stmts];
    let mut queue: VecDeque<usize> = VecDeque::new();

    // Seed: return register `_0` is live on every defining path.
    for bi in 0..func.blocks.len() {
        let bid = BlockId::from_usize(bi);
        for stmt_id in func.block_stmt_ids(bid) {
            if matches!(func.stmt(stmt_id), AmirStmt::Store {
                lhs: crate::amir::AmirPlace { local, projections },
                ..
            } if local.as_usize() == 0 && projections.is_empty())
            {
                let idx = stmt_id.as_usize();
                if !live[idx] {
                    live[idx] = true;
                    queue.push_back(idx);
                }
            }
        }
    }
    for &definition in &defs_of[0] {
        let idx = definition.as_usize();
        if !live[idx] {
            live[idx] = true;
            queue.push_back(idx);
        }
    }

    // Seeds: side-effecting stmts + terminator operand defs
    for bi in 0..func.blocks.len() {
        let bid = BlockId::from_usize(bi);

        for temp in collect_terminator_temps(&func.block(bid).terminator) {
            for &def in &defs_of[temp.as_usize()] {
                let idx = def.as_usize();
                if !live[idx] {
                    live[idx] = true;
                    queue.push_back(idx);
                }
            }
        }

        for stmt_id in func.block_stmt_ids(bid) {
            if has_side_effect(func.stmt(stmt_id)) {
                let idx = stmt_id.as_usize();
                if !live[idx] {
                    live[idx] = true;
                    queue.push_back(idx);
                }
            }
        }
    }

    // Propagate: for each live instruction, its operand defs become live
    while let Some(idx) = queue.pop_front() {
        let stmt = func.stmt(crate::amir::InstrId::from_usize(idx));
        for temp in collect_stmt_temps(stmt) {
            for &def in &defs_of[temp.as_usize()] {
                let didx = def.as_usize();
                if !live[didx] {
                    live[didx] = true;
                    queue.push_back(didx);
                }
            }
        }
    }

    // --- Sweep phase: rebuild stmt table, moving live stmts (no clone) -----
    let any_removed = live.iter().any(|&l| !l);
    if !any_removed {
        return Ok(false);
    }

    let old = std::mem::replace(&mut func.stmts, crate::amir::AmirStmtTable::new());
    let ice_span = optimization_span(func);
    let mut slots: Vec<Option<AmirStmt>> = old.payloads.raw.into_iter().map(Some).collect();
    let mut new_stmts = crate::amir::AmirStmtTable::new();
    let mut new_ranges: Vec<DenseRange> = Vec::with_capacity(func.blocks.len());

    // Blocks already retain their statement ranges while the old payload
    // table is moved. Iterate those ranges instead of copying every ID.
    for block in &func.blocks {
        let start = new_stmts.len();
        let mut kept = 0usize;
        for idx in block.statements.as_range() {
            if live[idx] {
                let stmt = slots.get_mut(idx).and_then(Option::take).ok_or_else(|| {
                    Diagnostic::ice(
                        DiagCode::ICEO001,
                        format!("DCE encountered duplicate or out-of-range statement id {idx}"),
                        ice_span,
                    )
                })?;
                new_stmts.push(stmt);
                kept += 1;
            }
        }
        new_ranges.push(DenseRange::new(start, kept));
    }

    func.stmts = new_stmts;
    for (block, range) in func.blocks.iter_mut().zip(new_ranges) {
        block.statements = range;
    }

    Ok(true)
}

fn optimization_span(func: &AmirFunc) -> Span {
    func.temps
        .first()
        .map(|temp| temp.span)
        .or_else(|| func.locals.first().map(|local| local.span))
        .unwrap_or_else(|| Span::new(func.symbol.file_id, 0, 0))
}

// ---------------------------------------------------------------------------
// Side-effect / temp helpers
// ---------------------------------------------------------------------------

fn has_side_effect(stmt: &AmirStmt) -> bool {
    match stmt {
        AmirStmt::Call { .. }
        | AmirStmt::Store { .. }
        | AmirStmt::Free(_)
        | AmirStmt::Destroy(_)
        | AmirStmt::StorageLive(_)
        | AmirStmt::StorageDead(_) => true,
        AmirStmt::Assign { rhs, .. } => matches!(
            rhs,
            AmirRvalue::Alloc(_)
                | AmirRvalue::GenInsert { .. }
                | AmirRvalue::GenGet { .. }
                | AmirRvalue::GenSet { .. }
                | AmirRvalue::GenUpsert { .. }
                | AmirRvalue::GenRemove { .. }
                | AmirRvalue::BlackBox { .. }
        ),
        AmirStmt::Nop => false,
    }
}

fn collect_terminator_temps(term: &AmirTerminator) -> SmallVec<[TempId; 4]> {
    // Single source of truth: middle::amir::visit::for_each_terminator_operand.
    // Must include Goto/Suspend/Branch jump args (SSA block-param edges).
    let mut t = SmallVec::new();
    for_each_terminator_operand(term, |op| {
        t.extend(collect_operand_temps(op));
    });
    t
}

fn collect_stmt_temps(stmt: &AmirStmt) -> SmallVec<[TempId; 4]> {
    match stmt {
        AmirStmt::Assign { rhs, .. } => collect_rvalue_temps(rhs),
        AmirStmt::Store { lhs, rhs } => {
            let mut temps = collect_operand_temps(rhs);
            for proj in &lhs.projections {
                if let AmirProjection::Index(op) = proj {
                    temps.extend(collect_operand_temps(op));
                }
            }
            temps
        }
        AmirStmt::Call { callee, args, .. } => {
            let mut temps = collect_operand_temps(callee);
            for arg in args {
                temps.extend(collect_operand_temps(arg));
            }
            temps
        }
        AmirStmt::Free(op) => collect_operand_temps(op),
        AmirStmt::Destroy(place) => {
            let mut temps = SmallVec::new();
            for proj in &place.projections {
                if let AmirProjection::Index(op) = proj {
                    temps.extend(collect_operand_temps(op));
                }
            }
            temps
        }
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => SmallVec::new(),
    }
}

fn collect_rvalue_temps(rvalue: &AmirRvalue) -> SmallVec<[TempId; 4]> {
    // Shared visitor keeps DCE in sync with new AmirRvalue variants (RC-ANALYSIS-LOAD).
    let mut t = SmallVec::new();
    for_each_rvalue_operand(rvalue, |op| {
        t.extend(collect_operand_temps(op));
    });
    t
}

fn collect_operand_temps(op: &AmirOperand) -> SmallVec<[TempId; 4]> {
    match op {
        AmirOperand::Copy(t) | AmirOperand::Move(t) => {
            let mut v = SmallVec::new();
            v.push(*t);
            v
        }
        _ => SmallVec::new(),
    }
}

#[cfg(test)]
mod tests {

    fn intern_ty(ty: crate::types::ArType) -> crate::types::TypeId {
        // Fresh interner per call is OK in unit tests (pre-interns primitives).
        crate::types::TypeInterner::new().intern(ty)
    }
    use super::*;
    use crate::amir::program::extend_block_range;
    use crate::amir::{
        AmirBasicBlock, AmirConstant, AmirPlace, AmirStmtTable, AmirTemp, BlockId, GenArenaDomain,
        LocalId, TempId,
    };
    use crate::layout::DenseRange;
    use crate::ops::BinaryOp;
    use crate::passes::type_checker::types::{ArType, Primitive};

    fn int_temp(id: usize) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: intern_ty(ArType::Primitive(Primitive::Int)),
            is_copy: true,
            is_nullable: false,
            span: arandu_lexer::Span::new(0, 0, 0),
        }
    }

    fn bool_temp(id: usize) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: intern_ty(ArType::Primitive(Primitive::Bool)),
            is_copy: true,
            is_nullable: false,
            span: arandu_lexer::Span::new(0, 0, 0),
        }
    }

    fn func(statements: Vec<AmirStmt>, temps: Vec<AmirTemp>) -> AmirFunc {
        let mut stmts = AmirStmtTable::new();
        let mut range = DenseRange::empty();
        for stmt in statements {
            let instr = stmts.push(stmt);
            extend_block_range(&mut range, instr);
        }
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: range,
            params: DenseRange::empty(),
            terminator: AmirTerminator::Return,
        }];
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps,
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        }
    }

    #[test]
    fn keeps_call_side_effect() {
        let mut f = func(
            vec![AmirStmt::Call {
                lhs: Some(TempId::from_usize(0)),
                callee: AmirOperand::FunctionRef(crate::SymbolId::new(0, 1)),
                args: smallvec::smallvec![],
                return_borrow: None,
            }],
            vec![bool_temp(0)],
        );
        assert!(!mark_sweep_dce(&mut f).unwrap());
        assert_eq!(f.blocks[0].statements.len, 1);
    }

    #[test]
    fn removes_unused_assign() {
        let mut f = func(
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(1),
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
            }],
            vec![bool_temp(0), bool_temp(1)],
        );
        assert!(mark_sweep_dce(&mut f).unwrap());
        assert_eq!(f.blocks[0].statements.len, 0);
    }

    #[test]
    fn keeps_gen_operation_when_result_is_unused() {
        let payload_ty = intern_ty(ArType::Primitive(Primitive::Int));
        let mut f = func(
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(1),
                rhs: AmirRvalue::GenInsert {
                    value: AmirOperand::Copy(TempId::from_usize(0)),
                    payload_ty,
                    arena: GenArenaDomain::CompilerManaged,
                    origin: arandu_lexer::Span::new(0, 4, 8),
                },
            }],
            vec![int_temp(0), int_temp(1)],
        );

        assert!(!mark_sweep_dce(&mut f).unwrap());
        assert_eq!(f.blocks[0].statements.len, 1);
    }

    #[test]
    fn keeps_binary_when_result_is_used() {
        let mut f = func(
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Add,
                        left: AmirOperand::Constant(AmirConstant::Pool(
                            crate::literal_pool::LiteralId(0),
                        )),
                        right: AmirOperand::Constant(AmirConstant::Pool(
                            crate::literal_pool::LiteralId(1),
                        )),
                    },
                },
                AmirStmt::Store {
                    lhs: AmirPlace {
                        local: LocalId::from_usize(0),
                        projections: smallvec::smallvec![],
                    },
                    rhs: AmirOperand::Copy(TempId::from_usize(1)),
                },
            ],
            vec![int_temp(0), int_temp(1)],
        );
        assert!(!mark_sweep_dce(&mut f).unwrap());
        assert_eq!(f.blocks[0].statements.len, 2);
    }

    #[test]
    fn removes_unreachable_chain() {
        // a = b + 1; b = c * 2; c = 5; (all unused) → all removed in one pass
        let mut f = func(
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(2),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Add,
                        left: AmirOperand::Copy(TempId::from_usize(1)),
                        right: AmirOperand::Constant(AmirConstant::Bool(true)),
                    },
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Binary {
                        op: BinaryOp::Mul,
                        left: AmirOperand::Copy(TempId::from_usize(0)),
                        right: AmirOperand::Constant(AmirConstant::Bool(true)),
                    },
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(0),
                    rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                },
            ],
            vec![bool_temp(0), bool_temp(1), bool_temp(2)],
        );
        assert!(mark_sweep_dce(&mut f).unwrap());
        // _0 is always live, so the t0 = Use(true) stmt survives.
        // t2 = t1 + true and t1 = t0 * true are correctly removed.
        assert_eq!(f.blocks[0].statements.len, 1);
    }

    #[test]
    fn noop_when_nothing_to_remove() {
        let mut f = func(
            vec![AmirStmt::Store {
                lhs: AmirPlace {
                    local: LocalId::from_usize(0),
                    projections: smallvec::smallvec![],
                },
                rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
            }],
            vec![],
        );
        assert!(!mark_sweep_dce(&mut f).unwrap());
        assert_eq!(f.blocks[0].statements.len, 1);
    }

    #[test]
    fn black_box_is_an_observable_root_and_keeps_its_input_definition() {
        let int_ty = intern_ty(ArType::Primitive(Primitive::Int));
        let mut f = func(
            vec![
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(2),
                    rhs: AmirRvalue::BlackBox {
                        value: AmirOperand::Copy(TempId::from_usize(1)),
                        value_ty: int_ty,
                    },
                },
            ],
            vec![int_temp(0), int_temp(1), int_temp(2)],
        );
        assert!(!mark_sweep_dce(&mut f).unwrap());
        assert_eq!(f.blocks[0].statements.len, 2);
    }

    /// Return slot `_0` is assigned on multiple paths (not SSA). All defs must
    /// stay live — otherwise `--opt` drops `_0 = 1` on one branch.
    #[test]
    fn keeps_all_return_slot_defs_on_branches() {
        let mut stmts = AmirStmtTable::new();
        // bb0: branch
        // bb1: t0 = true; return
        // bb2: t0 = false; return
        let a1 = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
        });
        let a2 = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(false))),
        });
        let mut r1 = DenseRange::empty();
        let mut r2 = DenseRange::empty();
        extend_block_range(&mut r1, a1);
        extend_block_range(&mut r2, a2);
        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
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
                statements: r1,
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: r2,
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        let mut f = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Primitive(Primitive::Bool)),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: vec![bool_temp(0)],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        assert!(!mark_sweep_dce(&mut f).unwrap());
        assert_eq!(f.blocks[1].statements.len, 1);
        assert_eq!(f.blocks[2].statements.len, 1);
    }

    /// A result temp can have one def per branch before it is consumed at the
    /// join. DCE must retain both reaching defs; keeping only the last textual
    /// def leaves the live join use undefined after SCCP removes one arm.
    #[test]
    fn keeps_all_branch_defs_for_a_live_join_use() {
        let mut stmts = AmirStmtTable::new();
        let then_def = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
        });
        let else_def = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(false))),
        });
        let join_store = stmts.push(AmirStmt::Store {
            lhs: AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec::smallvec![],
            },
            rhs: AmirOperand::Move(TempId::from_usize(1)),
        });
        let mut then_range = DenseRange::empty();
        extend_block_range(&mut then_range, then_def);
        let mut else_range = DenseRange::empty();
        extend_block_range(&mut else_range, else_def);
        let mut join_range = DenseRange::empty();
        extend_block_range(&mut join_range, join_store);
        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::empty(),
                params: DenseRange::empty(),
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
                statements: then_range,
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(2),
                statements: else_range,
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(3),
                statements: join_range,
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        let mut f = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: vec![bool_temp(0), bool_temp(1)],
            blocks,
            block_params: Vec::new(),
            stmts,
            cfg,
        };

        assert!(!mark_sweep_dce(&mut f).unwrap());
        assert_eq!(f.blocks[1].statements.len, 1);
        assert_eq!(f.blocks[2].statements.len, 1);
        assert_eq!(f.blocks[3].statements.len, 1);
    }

    /// Regression: values only used as `Goto` jump args must stay live.
    /// Without this, `--opt` deletes the mul and leaves `goto header(undef)`.
    #[test]
    fn keeps_assign_only_used_as_goto_arg() {
        let mut stmts = AmirStmtTable::new();
        let mut range0 = DenseRange::empty();
        let mut range1 = DenseRange::empty();
        // bb0: t1 = use(true); goto bb1(t1)
        let a0 = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
        });
        extend_block_range(&mut range0, a0);
        // bb1: return (t0 always-live seed)
        let a1 = stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(false))),
        });
        extend_block_range(&mut range1, a1);

        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: range0,
                params: DenseRange::empty(),
                terminator: AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![AmirOperand::Copy(TempId::from_usize(1))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: range1,
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        let mut f = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: vec![bool_temp(0), bool_temp(1)],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        // t1 is only referenced from Goto.args — must not be removed.
        assert!(!mark_sweep_dce(&mut f).unwrap());
        assert_eq!(f.blocks[0].statements.len, 1);
    }

    #[test]
    fn overlapping_statement_ranges_return_ice() {
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(false))),
        });
        let blocks = (0..2)
            .map(|id| AmirBasicBlock {
                id: BlockId::from_usize(id),
                statements: DenseRange::new(0, 1),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            })
            .collect::<Vec<_>>();
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        let mut f = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Primitive(Primitive::Int)),
            receiver: None,
            params: Vec::new(),
            locals: Vec::new(),
            temps: vec![int_temp(0), int_temp(1)],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };

        let error = mark_sweep_dce(&mut f).unwrap_err();
        assert_eq!(error.code, DiagCode::ICEO001);
    }
}
