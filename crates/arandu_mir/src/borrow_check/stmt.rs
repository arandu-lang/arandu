use crate::BitSet;
use crate::SymbolTable;
use crate::amir::{
    AmirFunc, AmirOperand, AmirPlace, AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId,
    TempId, for_each_rvalue_operand,
};
use crate::borrow_facts::{FuncBorrowFacts, LoanKind, ProgramPoint};
use crate::diagnostics::{DiagCode, Diagnostic};

pub(super) fn check_borrows_from_facts(
    func: &AmirFunc,
    symbols: &SymbolTable,
    facts: FuncBorrowFacts,
) -> Vec<(BlockId, Diagnostic)> {
    if func.blocks.is_empty() || func.locals.is_empty() {
        return Vec::new();
    }

    let (temp_origins, origins_diverged) = super::liveness::temp_origins_from_loads(func);

    if facts.exceeded_limit || origins_diverged {
        let first_block = func.blocks.first().map(|b| b.id).unwrap_or(BlockId(0));
        let span = super::diag::local_span(LocalId(0), func, symbols);
        return vec![(
            first_block,
            Diagnostic::ice(
                DiagCode::ICEGEN001,
                "borrow facts solver exceeded defensive iteration limit without reaching fixed point",
                span,
            ),
        )];
    }

    let temp_live_before = super::liveness::compute_temp_live_before(func, &facts.temp_live);

    let mut diags = Vec::new();

    for block in &func.blocks {
        let bi = block.id.as_usize();
        let stmts: Vec<&AmirStmt> = func.block_stmts(block.id).collect();
        let live_before = temp_live_before.get(bi).map(Vec::as_slice).unwrap_or(&[]);

        for (si, stmt) in stmts.iter().enumerate() {
            let live = live_before
                .get(si)
                .cloned()
                .unwrap_or_else(|| facts.temp_live.live_in(block.id).clone());
            check_stmt(
                stmt,
                block.id,
                si,
                &live,
                &facts,
                &temp_origins,
                func,
                symbols,
                &mut diags,
            );
        }

        // Terminator may move values into successors.
        let n = stmts.len();
        let live = if n == 0 {
            facts.temp_live.live_in(block.id).clone()
        } else {
            live_before
                .get(n)
                .cloned()
                .unwrap_or_else(|| facts.temp_live.live_out(block.id).clone())
        };
        check_terminator_moves(
            &block.terminator,
            block.id,
            n,
            &live,
            &facts,
            &temp_origins,
            func,
            symbols,
            &mut diags,
        );
    }

    diags
}

fn check_stmt(
    stmt: &AmirStmt,
    block: BlockId,
    stmt_index: usize,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    temp_origins: &[Option<AmirPlace>],
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    let point = ProgramPoint { block, stmt_index };

    match stmt {
        AmirStmt::Assign { rhs, .. } => match rhs {
            AmirRvalue::Borrow(place) => {
                check_new_loan(
                    place,
                    LoanKind::Shared,
                    point,
                    live,
                    facts,
                    func,
                    symbols,
                    diags,
                );
            }
            AmirRvalue::BorrowMut(place) => {
                check_new_loan(
                    place,
                    LoanKind::Exclusive,
                    point,
                    live,
                    facts,
                    func,
                    symbols,
                    diags,
                );
            }
            AmirRvalue::RelativeBorrow { local, mutable } => {
                let place = AmirPlace {
                    local: *local,
                    projections: smallvec::SmallVec::new(),
                };
                check_new_loan(
                    &place,
                    if *mutable {
                        LoanKind::Exclusive
                    } else {
                        LoanKind::Shared
                    },
                    point,
                    live,
                    facts,
                    func,
                    symbols,
                    diags,
                );
            }
            other => {
                check_rvalue_moves(
                    other,
                    point,
                    live,
                    facts,
                    temp_origins,
                    func,
                    symbols,
                    diags,
                );
            }
        },
        AmirStmt::Store { lhs, rhs } => {
            // Mutation of the place (whole struct or projected field/element) while borrowed.
            if super::places::place_borrowed_at(lhs, live, facts, point) {
                diags.push((
                    block,
                    super::diag::conflict_diag(
                        lhs,
                        func,
                        symbols,
                        facts,
                        live,
                        "mutable borrow conflict",
                        "cannot assign while value is borrowed",
                        block,
                    ),
                ));
            }
            check_operand_move(rhs, point, live, facts, temp_origins, func, symbols, diags);
        }
        AmirStmt::Call { callee, args, .. } => {
            check_operand_move(
                callee,
                point,
                live,
                facts,
                temp_origins,
                func,
                symbols,
                diags,
            );
            for arg in args {
                check_operand_move(arg, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirStmt::Free(op) => {
            check_operand_move(op, point, live, facts, temp_origins, func, symbols, diags);
        }
        AmirStmt::Destroy(place) => {
            if super::places::place_borrowed_at(place, live, facts, point) {
                diags.push((
                    block,
                    super::diag::destroy_diag(place.local, func, symbols, facts, live, block),
                ));
            }
        }
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
    }
}

fn check_new_loan(
    place: &AmirPlace,
    kind: LoanKind,
    point: ProgramPoint,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    let active_shared = super::places::active_kind_at(place, LoanKind::Shared, live, facts, point);
    let active_excl = super::places::active_kind_at(place, LoanKind::Exclusive, live, facts, point);

    let conflict = match kind {
        // `&` conflicts only with active `&mut`
        LoanKind::Shared => active_excl,
        // `&mut` conflicts with any active loan
        LoanKind::Exclusive => active_shared || active_excl,
    };

    if conflict {
        diags.push((
            point.block,
            super::diag::conflict_diag(
                place,
                func,
                symbols,
                facts,
                live,
                "mutable borrow conflict",
                match kind {
                    LoanKind::Shared => "cannot borrow as shared while exclusively borrowed",
                    LoanKind::Exclusive => "cannot borrow as mutable while already borrowed",
                },
                point.block,
            ),
        ));
    }
}

fn check_rvalue_moves(
    rhs: &AmirRvalue,
    point: ProgramPoint,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    temp_origins: &[Option<AmirPlace>],
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    for_each_rvalue_operand(rhs, |op| {
        check_operand_move(op, point, live, facts, temp_origins, func, symbols, diags);
    });
}

fn check_terminator_moves(
    term: &AmirTerminator,
    block: BlockId,
    stmt_index: usize,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    temp_origins: &[Option<AmirPlace>],
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    let point = ProgramPoint { block, stmt_index };
    match term {
        AmirTerminator::Branch {
            condition,
            true_args,
            false_args,
            ..
        } => {
            check_operand_move(
                condition,
                point,
                live,
                facts,
                temp_origins,
                func,
                symbols,
                diags,
            );
            for a in true_args {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
            for a in false_args {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
            ..
        } => {
            check_operand_move(
                discriminant,
                point,
                live,
                facts,
                temp_origins,
                func,
                symbols,
                diags,
            );
            for (_, _, args) in targets {
                for a in args {
                    check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
                }
            }
            for a in &otherwise.1 {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirTerminator::Goto { args, .. } => {
            for a in args {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirTerminator::Suspend { future, args, .. } => {
            check_operand_move(
                future,
                point,
                live,
                facts,
                temp_origins,
                func,
                symbols,
                diags,
            );
            for a in args {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
    }
}

fn check_operand_move(
    op: &AmirOperand,
    point: ProgramPoint,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    temp_origins: &[Option<AmirPlace>],
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    let AmirOperand::Move(temp) = op else {
        return;
    };
    if func.temps.get(temp.as_usize()).is_some_and(|t| t.is_copy) {
        return;
    }
    let Some(place) = temp_origins.get(temp.as_usize()).and_then(Option::as_ref) else {
        return;
    };
    if super::places::place_borrowed_at(place, live, facts, point) {
        diags.push((
            point.block,
            super::diag::move_while_borrowed_diag(
                place.local,
                func,
                symbols,
                facts,
                live,
                point.block,
            ),
        ));
    }
}
