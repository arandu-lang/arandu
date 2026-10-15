//! Definite Initialization Analysis (O008)
//!
//! A forward dataflow analysis over the AMIR CFG that tracks whether each
//! `LocalId` stack slot has been initialized at every program point. Reports
//! an error (`O008UseBeforeInit`) if a `Load` reads from a local that is
//! *possibly* uninitialized along some control-flow path.
//!
//! ## Lattice
//!
//! Each local is in one of two states:
//! - `Initialized` — the local has been written via `Store` on **all** paths.
//! - `MaybeUninitialized` — at least one path exists that has not written to it.
//!
//! The join operation for merge points is `intersect`: a local is initialized
//! at a merge only if it is initialized in *all* predecessors.
//!
//! ## Algorithm
//!
//! We use the simple iterative worklist algorithm over the RPO of the CFG,
//! propagating gen/kill sets per block until a fixpoint is reached. This is
//! the same class of algorithm as the LLVM `MaybeUninitializedPlaces` analysis.

#![allow(
    clippy::collapsible_match,
    clippy::single_match,
    clippy::collapsible_if
)]
use crate::amir::block::BlockId;
use crate::amir::for_each_rvalue_place;
use crate::amir::local::LocalId;
use crate::amir::program::AmirFunc;
use crate::amir::stmt::{AmirStmt, AmirTerminator};
use crate::amir::value::AmirRvalue;
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::{BitSet, SymbolTable};

use std::collections::VecDeque;

/// Return exits reachable before the result register has been defined. Unlike
/// local initialization, this register has no storage-dead/kill operation: walk
/// only still-uninitialized paths, stopping at the first definition. Each block
/// is visited at most once, including back-edges. The caller supplies validated
/// AMIR and decides whether its result type requires a value.
#[must_use]
pub fn uninitialized_return_exits(func: &AmirFunc) -> Vec<BlockId> {
    use crate::amir::TempId;
    let mut pending = VecDeque::from([BlockId::from_usize(0)]);
    let mut visited = vec![false; func.blocks.len()];
    let mut exits = Vec::new();
    while let Some(id) = pending.pop_front() {
        let Some(seen) = visited.get_mut(id.as_usize()) else {
            continue;
        };
        if *seen {
            continue;
        }
        *seen = true;
        let Some(block) = func.blocks.get(id.as_usize()) else {
            continue;
        };
        let defined = func
            .block_params(block.params)
            .iter()
            .any(|param| param.id == TempId(0))
            || func.block_stmts(id).any(|statement| match statement {
                AmirStmt::Assign { lhs, .. } => *lhs == TempId(0),
                AmirStmt::Call { lhs, .. } => *lhs == Some(TempId(0)),
                AmirStmt::Store { .. }
                | AmirStmt::Free(_)
                | AmirStmt::Destroy(_)
                | AmirStmt::StorageLive(_)
                | AmirStmt::StorageDead(_)
                | AmirStmt::Nop => false,
            });
        if defined {
            continue;
        }
        if matches!(block.terminator, AmirTerminator::Return) {
            exits.push(id);
        }
        if let Some(successors) = func.cfg.successors.get(id.as_usize()) {
            pending.extend(successors.iter().copied());
        }
    }
    exits.sort_unstable_by_key(|id| id.as_usize());
    exits
}

/// Cardinality of definitely-initialized locals at each block's entry (IN set).
///
/// Used by Salsa `block_dataflow_facts` — pure, no diagnostics.
#[must_use]
pub fn init_in_counts(func: &AmirFunc) -> Vec<u32> {
    let Some(block_in) = compute_init_in(func) else {
        return vec![0; func.blocks.len()];
    };
    block_in.iter().map(|s| s.len() as u32).collect()
}

/// Definitely initialized locals after each block. Drop elaboration consumes
/// these exact facts instead of maintaining a second initialization model.
#[must_use]
pub fn initialized_at_block_exit(func: &AmirFunc) -> Vec<BitSet<LocalId>> {
    let Some(block_in) = compute_init_in(func) else {
        return vec![BitSet::with_capacity(func.locals.len()); func.blocks.len()];
    };
    block_in
        .into_iter()
        .enumerate()
        .map(|(index, mut initialized)| {
            let bid = BlockId::from_usize(index);
            if let Some(block) = func.blocks.get(index) {
                for param in func.block_params(block.params) {
                    initialized.insert(param.local);
                }
            }
            for stmt in func.block_stmts(bid) {
                transfer_init_stmt(stmt, &mut initialized);
            }
            initialized
        })
        .collect()
}

/// Definitely initialized locals immediately before each statement.
pub(crate) fn initialized_before_statements(func: &AmirFunc) -> Vec<Vec<BitSet<LocalId>>> {
    let Some(block_in) = compute_init_in(func) else {
        return func
            .blocks
            .iter()
            .map(|block| {
                vec![BitSet::with_capacity(func.locals.len()); block.statements.len_usize()]
            })
            .collect();
    };
    func.blocks
        .iter()
        .map(|block| {
            let mut initialized = block_in[block.id.as_usize()].clone();
            for param in func.block_params(block.params) {
                initialized.insert(param.local);
            }
            let mut snapshots = Vec::with_capacity(block.statements.len_usize());
            for stmt in func.block_stmts(block.id) {
                snapshots.push(initialized.clone());
                transfer_init_stmt(stmt, &mut initialized);
            }
            snapshots
        })
        .collect()
}

/// Run definite-initialization analysis over a single AMIR function.
///
/// Returns a list of `O008` diagnostics for any load from a possibly
/// uninitialized local.
pub fn check_definite_init(func: &AmirFunc, symbols: &SymbolTable) -> Vec<Diagnostic> {
    check_definite_init_by_block(func, symbols)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

/// Same as [`check_definite_init`], but tags each diagnostic with the AMIR block
/// where the bad load was found (P3/P4 honesty: real span→block attribution).
#[must_use]
pub fn check_definite_init_by_block(
    func: &AmirFunc,
    symbols: &SymbolTable,
) -> Vec<(BlockId, Diagnostic)> {
    let Some(mut block_in) = compute_init_in(func) else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();

    for block in &func.blocks {
        let bi = block.id.as_usize();
        // Take ownership of each IN set — only used once during the check walk.
        let mut current = std::mem::take(&mut block_in[bi]);
        let bid = block.id;

        // Block parameters define and initialize their corresponding local at block entry.
        for param in func.block_params(block.params) {
            current.insert(param.local);
        }

        for stmt in func.block_stmts(block.id) {
            check_stmt_loads(stmt, &current, func, symbols, bid, &mut diagnostics);

            transfer_init_stmt(stmt, &mut current);
        }
    }

    diagnostics
}

/// Forward dataflow: IN set of definitely-initialized locals per block.
fn compute_init_in(func: &AmirFunc) -> Option<Vec<BitSet<LocalId>>> {
    let num_locals = func.locals.len();
    let num_blocks = func.blocks.len();

    if num_locals == 0 || num_blocks == 0 {
        return None;
    }

    let mut block_in = vec![BitSet::<LocalId>::all_set(num_locals); num_blocks];
    let mut block_out = vec![BitSet::<LocalId>::all_set(num_locals); num_blocks];

    let mut worklist = VecDeque::new();
    for block in &func.blocks {
        worklist.push_back(block.id);
    }

    let mut iterations = 0;
    let sanity_limit = num_blocks * num_locals + crate::analysis_limits::DATAFLOW_FIXPOINT_HEADROOM;

    while let Some(bid) = worklist.pop_front() {
        iterations += 1;
        assert!(
            iterations <= sanity_limit,
            "definite initialization checker failed to converge within theoretical limit: {iterations} > {sanity_limit} ({num_blocks} blocks) — possível bug de monotonicidade no dataflow"
        );

        let bi = bid.as_usize();
        let block = &func.blocks[bi];

        let new_in = if bid == BlockId::from_usize(0) || func.predecessors(bid).is_empty() {
            BitSet::with_capacity(num_locals)
        } else {
            let mut acc = BitSet::all_set(num_locals);
            for &pred in func.predecessors(bid) {
                acc.intersect_with(&block_out[pred.as_usize()]);
            }
            acc
        };

        let mut new_out = new_in.clone();
        for param in func.block_params(block.params) {
            new_out.insert(param.local);
        }
        for stmt in func.block_stmts(bid) {
            transfer_init_stmt(stmt, &mut new_out);
        }

        debug_assert!(
            block_out[bi].is_superset_of(&new_out),
            "Definite init dataflow is not monotonic at block {bi}"
        );

        if new_out != block_out[bi] {
            block_in[bi] = new_in;
            block_out[bi] = new_out;

            match &block.terminator {
                AmirTerminator::Return | AmirTerminator::Unreachable => {}
                AmirTerminator::Goto { target, .. } => {
                    worklist.push_back(*target);
                }
                AmirTerminator::Suspend { resume, .. } => {
                    worklist.push_back(*resume);
                }
                AmirTerminator::Branch {
                    if_true, if_false, ..
                } => {
                    worklist.push_back(*if_true);
                    worklist.push_back(*if_false);
                }
                AmirTerminator::SwitchInt {
                    targets, otherwise, ..
                } => {
                    for (_, b, _) in targets {
                        worklist.push_back(*b);
                    }
                    worklist.push_back(otherwise.0);
                }
            }
        } else {
            block_in[bi] = new_in;
        }
    }

    Some(block_in)
}

fn transfer_init_stmt(stmt: &AmirStmt, initialized: &mut BitSet<LocalId>) {
    match stmt {
        AmirStmt::Store { lhs, .. } if lhs.projections.is_empty() => {
            initialized.insert(lhs.local);
        }
        AmirStmt::StorageDead(local) => {
            initialized.remove(*local);
        }
        _ => {}
    }
}

/// Check whether any `Load` in a statement reads from an uninitialized local.
fn check_stmt_loads(
    stmt: &AmirStmt,
    current: &BitSet<LocalId>,
    func: &AmirFunc,
    symbols: &SymbolTable,
    block: BlockId,
    diagnostics: &mut Vec<(BlockId, Diagnostic)>,
) {
    match stmt {
        AmirStmt::Assign { rhs, .. } => {
            check_rvalue_loads(rhs, current, func, symbols, block, diagnostics);
        }
        AmirStmt::Store { rhs, lhs, .. } => {
            // If storing to a projection (e.g. x.field), the base must be initialized
            if !lhs.projections.is_empty() && !current.contains(lhs.local) {
                emit_uninit_diag(lhs.local, func, symbols, block, diagnostics);
            }
            // Check if rhs references uninitialized locals via operand
            // (operands are TempId-based, so they are always SSA-valid)
            let _ = rhs;
        }
        AmirStmt::Call { .. } | AmirStmt::Free(_) => {}
        AmirStmt::Destroy(place) => {
            if !current.contains(place.local) {
                emit_uninit_diag(place.local, func, symbols, block, diagnostics);
            }
        }
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
    }
}

fn check_rvalue_loads(
    rvalue: &AmirRvalue,
    current: &BitSet<LocalId>,
    func: &AmirFunc,
    symbols: &SymbolTable,
    block: BlockId,
    diagnostics: &mut Vec<(BlockId, Diagnostic)>,
) {
    // Shared place visitor (RC-ANALYSIS-LOAD): Load / Borrow / BorrowMut.
    for_each_rvalue_place(rvalue, |place| {
        if !current.contains(place.local) {
            emit_uninit_diag(place.local, func, symbols, block, diagnostics);
        }
    });
}

fn emit_uninit_diag(
    local: LocalId,
    func: &AmirFunc,
    symbols: &SymbolTable,
    block: BlockId,
    diagnostics: &mut Vec<(BlockId, Diagnostic)>,
) {
    let local_info = &func.locals[local.as_usize()];
    let name = local_info.symbol.map_or_else(
        || "<compiler local>".to_string(),
        |s| symbols.get(s).name.to_string(),
    );
    // Prefer use site → declaration → symbol span (S-SPAN-THREAD).
    let span = {
        if let Some(u) = local_info.use_span {
            if u.start != u.end {
                u
            } else if local_info.span.start != local_info.span.end {
                local_info.span
            } else {
                local_info
                    .symbol
                    .map(|s| symbols.get(s).span)
                    .unwrap_or(local_info.span)
            }
        } else if local_info.span.start != local_info.span.end {
            local_info.span
        } else {
            local_info
                .symbol
                .map(|s| symbols.get(s).span)
                .unwrap_or(local_info.span)
        }
    };
    diagnostics.push((
        block,
        Diagnostic::error(
            DiagCode::O008UseBeforeInit,
            format!("use of possibly uninitialized variable `{name}`"),
            span,
        )
        .with_note(format!(
            "variable `{name}` may not be initialized on all paths"
        )),
    ));
}

#[cfg(test)]
mod tests {

    fn intern_ty(ty: crate::types::ArType) -> crate::types::TypeId {
        // Fresh interner per call is OK in unit tests (pre-interns primitives).
        crate::types::TypeInterner::new().intern(ty)
    }
    use super::*;
    use crate::SymbolId;
    use crate::SymbolTable;
    use crate::amir::block::AmirBasicBlock;
    use crate::amir::local::{AmirLocal, AmirTemp, TempId};
    use crate::amir::program::{AmirFunc, extend_block_range};
    use crate::amir::stmt::{AmirStmt, AmirStmtTable, AmirTerminator};
    use crate::amir::value::{AmirConstant, AmirOperand, AmirPlace, AmirRvalue};
    use crate::layout::DenseRange;
    use crate::passes::type_checker::types::ArType;
    use arandu_lexer::Span;
    use smallvec::smallvec;

    fn make_symbol_table() -> SymbolTable {
        SymbolTable::new(0)
    }

    fn make_local(id: usize, sym: Option<SymbolId>) -> AmirLocal {
        AmirLocal {
            id: LocalId::from_usize(id),
            ty: intern_ty(ArType::Primitive(
                crate::passes::type_checker::types::Primitive::Int,
            )),
            is_memory: false,
            symbol: sym,
            span: Span::new(0, 0, 0),
            use_span: None,
        }
    }

    fn make_temp(id: usize) -> AmirTemp {
        AmirTemp {
            id: TempId::from_usize(id),
            ty: intern_ty(ArType::Primitive(
                crate::passes::type_checker::types::Primitive::Int,
            )),
            is_copy: true,
            is_nullable: false,
            span: Span::new(0, 0, 0),
        }
    }

    fn place(local: usize) -> AmirPlace {
        AmirPlace {
            local: LocalId::from_usize(local),
            projections: smallvec![],
        }
    }

    fn make_block(
        id: usize,
        statements: Vec<AmirStmt>,
        terminator: AmirTerminator,
        _successors: &[usize],
        _predecessors: &[usize],
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

    fn make_func(
        blocks: Vec<AmirBasicBlock>,
        stmts: AmirStmtTable,
        locals: Vec<AmirLocal>,
        temps: Vec<AmirTemp>,
    ) -> AmirFunc {
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        AmirFunc {
            symbol: SymbolId::new(0, 0),
            return_type: intern_ty(ArType::Void),
            receiver: None,
            params: vec![],
            locals,
            temps,
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        }
    }

    #[test]
    fn result_initialization_terminates_on_back_edges_and_ignores_unreachable_exits() {
        let mut statements = AmirStmtTable::new();
        let blocks = vec![
            make_block(
                0,
                vec![],
                AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: vec![],
                },
                &[],
                &[],
                &mut statements,
            ),
            make_block(
                1,
                vec![],
                AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(false)),
                    if_true: BlockId::from_usize(1),
                    if_false: BlockId::from_usize(2),
                    true_args: vec![],
                    false_args: vec![],
                },
                &[],
                &[],
                &mut statements,
            ),
            make_block(2, vec![], AmirTerminator::Return, &[], &[], &mut statements),
            make_block(3, vec![], AmirTerminator::Return, &[], &[], &mut statements),
        ];
        let mut function = make_func(blocks, statements, vec![], vec![make_temp(0)]);
        assert_eq!(
            uninitialized_return_exits(&function),
            vec![BlockId::from_usize(2)]
        );
        let instruction = function.stmts.push(AmirStmt::Assign {
            lhs: TempId(0),
            rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
        });
        extend_block_range(&mut function.blocks[0].statements, instruction);
        assert!(uninitialized_return_exits(&function).is_empty());
        *function
            .stmts
            .get_mut(instruction)
            .expect("result assignment") = AmirStmt::Call {
            lhs: Some(TempId(0)),
            callee: AmirOperand::FunctionRef(SymbolId::new(0, 1)),
            args: smallvec![],
            return_borrow: None,
        };
        assert!(
            uninitialized_return_exits(&function).is_empty(),
            "call results also define the register"
        );
    }

    #[test]
    fn test_no_error_when_stored_before_load() {
        // bb0: Store local0 = const; Assign t1 = Load(local0); return
        let mut stmts = AmirStmtTable::new();
        let blocks = vec![make_block(
            0,
            vec![
                AmirStmt::Store {
                    lhs: place(0),
                    rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
                },
                AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Load(place(0)),
                },
            ],
            AmirTerminator::Return,
            &[],
            &[],
            &mut stmts,
        )];

        let func = make_func(
            blocks,
            stmts,
            vec![make_local(0, None)],
            vec![make_temp(0), make_temp(1)],
        );

        let st = make_symbol_table();
        let diags = check_definite_init(&func, &st);
        assert!(diags.is_empty(), "expected no errors, got: {:?}", diags);
    }

    #[test]
    fn test_error_when_loaded_without_store() {
        // bb0: Assign t1 = Load(local0); return
        let mut stmts = AmirStmtTable::new();
        let blocks = vec![make_block(
            0,
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(1),
                rhs: AmirRvalue::Load(place(0)),
            }],
            AmirTerminator::Return,
            &[],
            &[],
            &mut stmts,
        )];

        let func = make_func(
            blocks,
            stmts,
            vec![make_local(0, None)],
            vec![make_temp(0), make_temp(1)],
        );

        let st = make_symbol_table();
        let diags = check_definite_init(&func, &st);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagCode::O008UseBeforeInit);
    }

    #[test]
    fn test_o008_prefers_use_span_over_decl_span() {
        // Declaration at 0..3; recorded use at 10..15 — O008 must point at use.
        let mut local = make_local(0, None);
        local.span = Span::new(0, 0, 3);
        local.use_span = Some(Span::new(0, 10, 15));

        let mut stmts = AmirStmtTable::new();
        let blocks = vec![make_block(
            0,
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(1),
                rhs: AmirRvalue::Load(place(0)),
            }],
            AmirTerminator::Return,
            &[],
            &[],
            &mut stmts,
        )];
        let func = make_func(blocks, stmts, vec![local], vec![make_temp(0), make_temp(1)]);
        let st = make_symbol_table();
        let diags = check_definite_init(&func, &st);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagCode::O008UseBeforeInit);
        assert_eq!(diags[0].span, Span::new(0, 10, 15));
    }

    #[test]
    fn test_error_on_conditional_init() {
        // bb0: Branch -> bb1, bb2
        // bb1: Store local0; Goto bb3
        // bb2: Goto bb3
        // bb3: Load local0  <-- ERROR: only initialized on one branch
        let mut stmts = AmirStmtTable::new();
        let blocks = vec![
            make_block(
                0,
                vec![],
                AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
                &[1, 2],
                &[],
                &mut stmts,
            ),
            make_block(
                1,
                vec![AmirStmt::Store {
                    lhs: place(0),
                    rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
                }],
                AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
                &[3],
                &[0],
                &mut stmts,
            ),
            make_block(
                2,
                vec![],
                AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
                &[3],
                &[0],
                &mut stmts,
            ),
            make_block(
                3,
                vec![AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Load(place(0)),
                }],
                AmirTerminator::Return,
                &[],
                &[1, 2],
                &mut stmts,
            ),
        ];

        let func = make_func(
            blocks,
            stmts,
            vec![make_local(0, None)],
            vec![make_temp(0), make_temp(1)],
        );

        let st = make_symbol_table();
        let diags = check_definite_init(&func, &st);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagCode::O008UseBeforeInit);
    }

    #[test]
    fn test_no_error_with_loop_init() {
        // bb0: Store local0 = const; Goto bb1
        // bb1: Load local0; Branch(cond, bb1, bb2)
        // bb2: return
        let mut stmts = AmirStmtTable::new();
        let blocks = vec![
            make_block(
                0,
                vec![AmirStmt::Store {
                    lhs: place(0),
                    rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
                }],
                AmirTerminator::Goto {
                    target: BlockId::from_usize(1),
                    args: Vec::new(),
                },
                &[1],
                &[],
                &mut stmts,
            ),
            make_block(
                1,
                vec![AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Load(place(0)),
                }],
                AmirTerminator::Branch {
                    condition: AmirOperand::Copy(TempId::from_usize(1)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
                &[1, 2],
                &[0],
                &mut stmts,
            ),
            make_block(2, vec![], AmirTerminator::Return, &[], &[1], &mut stmts),
        ];
        let func = make_func(
            blocks,
            stmts,
            vec![make_local(0, None)],
            vec![make_temp(0), make_temp(1)],
        );
        let st = make_symbol_table();
        let diags = check_definite_init(&func, &st);
        assert!(diags.is_empty(), "expected no errors, got: {:?}", diags);
    }

    #[test]
    fn test_no_error_with_empty_func() {
        let stmts = AmirStmtTable::new();
        let blocks = vec![make_block(
            0,
            vec![],
            AmirTerminator::Return,
            &[],
            &[],
            &mut stmts.clone(),
        )];
        let func = make_func(blocks, stmts, vec![], vec![]);
        let st = make_symbol_table();
        let diags = check_definite_init(&func, &st);
        assert!(diags.is_empty());
    }

    #[test]
    fn test_error_no_init_at_all() {
        let mut stmts = AmirStmtTable::new();
        let blocks = vec![make_block(
            0,
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(1),
                rhs: AmirRvalue::Load(place(0)),
            }],
            AmirTerminator::Return,
            &[],
            &[],
            &mut stmts,
        )];
        let func = make_func(
            blocks,
            stmts,
            vec![make_local(0, None)],
            vec![make_temp(0), make_temp(1)],
        );
        let st = make_symbol_table();
        let diags = check_definite_init(&func, &st);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagCode::O008UseBeforeInit);
    }

    #[test]
    fn test_no_error_on_both_branches_init() {
        // bb0: Branch -> bb1, bb2
        // bb1: Store local0; Goto bb3
        // bb2: Store local0; Goto bb3
        // bb3: Load local0  <-- OK: initialized on both branches
        let mut stmts = AmirStmtTable::new();
        let blocks = vec![
            make_block(
                0,
                vec![],
                AmirTerminator::Branch {
                    condition: AmirOperand::Constant(AmirConstant::Bool(true)),
                    if_true: BlockId::from_usize(1),
                    true_args: Vec::new(),
                    if_false: BlockId::from_usize(2),
                    false_args: Vec::new(),
                },
                &[1, 2],
                &[],
                &mut stmts,
            ),
            make_block(
                1,
                vec![AmirStmt::Store {
                    lhs: place(0),
                    rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
                }],
                AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
                &[3],
                &[0],
                &mut stmts,
            ),
            make_block(
                2,
                vec![AmirStmt::Store {
                    lhs: place(0),
                    rhs: AmirOperand::Constant(AmirConstant::Bool(true)),
                }],
                AmirTerminator::Goto {
                    target: BlockId::from_usize(3),
                    args: Vec::new(),
                },
                &[3],
                &[0],
                &mut stmts,
            ),
            make_block(
                3,
                vec![AmirStmt::Assign {
                    lhs: TempId::from_usize(1),
                    rhs: AmirRvalue::Load(place(0)),
                }],
                AmirTerminator::Return,
                &[],
                &[1, 2],
                &mut stmts,
            ),
        ];

        let func = make_func(
            blocks,
            stmts,
            vec![make_local(0, None)],
            vec![make_temp(0), make_temp(1)],
        );

        let st = make_symbol_table();
        let diags = check_definite_init(&func, &st);
        assert!(diags.is_empty(), "expected no errors, got: {:?}", diags);
    }

    #[test]
    fn test_no_o008_for_block_param_initialized_local() {
        // bb0 jumps to bb1 with arg
        // bb1 has param for local0, then loads local0 -> should have no O008
        let mut stmts = AmirStmtTable::new();
        let b0 = make_block(
            0,
            vec![],
            AmirTerminator::Goto {
                target: BlockId::from_usize(1),
                args: vec![AmirOperand::Constant(AmirConstant::Bool(true))],
            },
            &[1],
            &[],
            &mut stmts,
        );
        let b1 = make_block(
            1,
            vec![AmirStmt::Assign {
                lhs: TempId::from_usize(1),
                rhs: AmirRvalue::Load(place(0)),
            }],
            AmirTerminator::Return,
            &[],
            &[0],
            &mut stmts,
        );

        let mut func = make_func(
            vec![b0, b1],
            stmts,
            vec![make_local(0, None)],
            vec![make_temp(0), make_temp(1)],
        );
        func.blocks[1].params = DenseRange::new(0, 1);
        func.block_params.push(crate::amir::BlockParam {
            id: TempId::from_usize(0),
            local: LocalId::from_usize(0),
            ty: intern_ty(ArType::Primitive(
                crate::passes::type_checker::types::Primitive::Int,
            )),
            from: None,
            moved: false,
        });

        let st = make_symbol_table();
        let diags = check_definite_init(&func, &st);
        assert!(
            diags.is_empty(),
            "expected no uninitialized errors for block param, got: {:?}",
            diags
        );

        let exit_facts = initialized_at_block_exit(&func);
        assert!(
            exit_facts[1].contains(LocalId::from_usize(0)),
            "block 1 must report local 0 as definitely initialized"
        );
    }
}
