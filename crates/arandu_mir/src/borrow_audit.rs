//! Deterministic evidence for the borrowed-view safety campaign.
//!
//! This module does not relax the borrow checker. It exposes where AMIR keeps
//! an origin and where the current representation loses it, so an
//! interprocedural contract can be designed from executable evidence instead
//! of inference from source syntax.

use std::collections::BTreeSet;

use crate::SymbolId;
use crate::amir::{
    AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId, TempId,
};
use crate::borrow_facts::{LoanKind, analyze_borrow_facts};
use crate::escape_analysis::{EscapeKind, find_escapes};
use crate::types::{ArType, TypeInterner};

/// AMIR boundary inspected by the audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BorrowAuditOperation {
    Borrow,
    BorrowMut,
    RelativeBorrow,
    Copy,
    Load,
    Store,
    Aggregate,
    CallResult,
    BlockArgument,
    Return,
}

/// Whether the current AMIR representation retains the origin relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginStatus {
    Preserved,
    Lost,
}

/// One deterministic observation in block/statement order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowAuditObservation {
    pub block: BlockId,
    /// Statement offset inside the block. Terminators use `stmt_count`.
    pub statement: usize,
    pub operation: BorrowAuditOperation,
    pub status: OriginStatus,
    /// All possible root locals, sorted by dense id.
    pub origins: Vec<LocalId>,
    pub result: Option<TempId>,
    pub callee: Option<SymbolId>,
    pub reason: &'static str,
}

/// Stable loan evidence; bitsets are normalized into sorted vectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowAuditLoan {
    pub kind: LoanKind,
    pub origin: LocalId,
    pub holder_temps: Vec<TempId>,
    pub holder_locals: Vec<LocalId>,
    pub origin_block: BlockId,
}

/// Stable escape evidence produced by the existing safety checker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowAuditEscape {
    pub kind: EscapeKind,
    pub origin: LocalId,
    pub block: BlockId,
    pub reason: &'static str,
}

/// Complete pure report for a single AMIR function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowAuditReport {
    pub function: SymbolId,
    pub loans: Vec<BorrowAuditLoan>,
    pub observations: Vec<BorrowAuditObservation>,
    pub escapes: Vec<BorrowAuditEscape>,
}

impl BorrowAuditReport {
    #[must_use]
    pub fn has_origin_loss(&self) -> bool {
        self.observations
            .iter()
            .any(|observation| observation.status == OriginStatus::Lost)
    }
}

/// Audit a lowered function without I/O or mutable global state.
#[must_use]
pub fn audit_borrow_origins(func: &AmirFunc, interner: &TypeInterner) -> BorrowAuditReport {
    let facts = analyze_borrow_facts(func);
    let loans = facts
        .loans
        .iter()
        .map(|loan| BorrowAuditLoan {
            kind: loan.kind,
            origin: loan.place_local,
            holder_temps: loan.holder_temps.iter().collect(),
            holder_locals: loan.holder_locals.iter().collect(),
            origin_block: loan.origin_block,
        })
        .collect::<Vec<_>>();

    let temp_origins = origins_by_temp(func.temps.len(), &loans);
    let local_origins = origins_by_local(func.locals.len(), &loans);
    let mut observations = Vec::new();

    for block in &func.blocks {
        for (statement, stmt) in func.block_stmts(block.id).enumerate() {
            observe_stmt(
                func,
                interner,
                block.id,
                statement,
                stmt,
                &temp_origins,
                &local_origins,
                &mut observations,
            );
        }
        observe_terminator(
            func,
            interner,
            block.id,
            func.block_stmts(block.id).count(),
            &block.terminator,
            &temp_origins,
            &mut observations,
        );
    }

    let escapes = find_escapes(func, interner)
        .into_iter()
        .map(|escape| BorrowAuditEscape {
            kind: escape.kind,
            origin: escape.place_local,
            block: escape.block,
            reason: escape.reason,
        })
        .collect();

    BorrowAuditReport {
        function: func.symbol,
        loans,
        observations,
        escapes,
    }
}

fn origins_by_temp(count: usize, loans: &[BorrowAuditLoan]) -> Vec<Vec<LocalId>> {
    let mut origins = vec![BTreeSet::new(); count];
    for loan in loans {
        for holder in &loan.holder_temps {
            if let Some(entry) = origins.get_mut(holder.as_usize()) {
                entry.insert(loan.origin);
            }
        }
    }
    origins
        .into_iter()
        .map(|items| items.into_iter().collect())
        .collect()
}

fn origins_by_local(count: usize, loans: &[BorrowAuditLoan]) -> Vec<Vec<LocalId>> {
    let mut origins = vec![BTreeSet::new(); count];
    for loan in loans {
        for holder in &loan.holder_locals {
            if let Some(entry) = origins.get_mut(holder.as_usize()) {
                entry.insert(loan.origin);
            }
        }
    }
    origins
        .into_iter()
        .map(|items| items.into_iter().collect())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn observe_stmt(
    func: &AmirFunc,
    interner: &TypeInterner,
    block: BlockId,
    statement: usize,
    stmt: &AmirStmt,
    temp_origins: &[Vec<LocalId>],
    local_origins: &[Vec<LocalId>],
    observations: &mut Vec<BorrowAuditObservation>,
) {
    match stmt {
        AmirStmt::Assign { lhs, rhs } => match rhs {
            AmirRvalue::Borrow(place) => observations.push(observation(
                block,
                statement,
                BorrowAuditOperation::Borrow,
                vec![place.local],
                Some(*lhs),
                None,
                "shared borrow records its root place",
            )),
            AmirRvalue::BorrowMut(place) => observations.push(observation(
                block,
                statement,
                BorrowAuditOperation::BorrowMut,
                vec![place.local],
                Some(*lhs),
                None,
                "mutable borrow records its root place",
            )),
            AmirRvalue::RelativeBorrow { local, .. } => observations.push(observation(
                block,
                statement,
                BorrowAuditOperation::RelativeBorrow,
                vec![*local],
                Some(*lhs),
                None,
                "relative borrow records its frame-local origin",
            )),
            AmirRvalue::Use(operand) if is_reference_temp(func, interner, *lhs) => {
                let origins = operand_origins(operand, temp_origins);
                observations.push(observation(
                    block,
                    statement,
                    BorrowAuditOperation::Copy,
                    origins,
                    Some(*lhs),
                    None,
                    "reference copy must retain every possible origin",
                ));
            }
            AmirRvalue::Load(place) if is_reference_temp(func, interner, *lhs) => {
                let origins = local_origins
                    .get(place.local.as_usize())
                    .cloned()
                    .unwrap_or_default();
                observations.push(observation(
                    block,
                    statement,
                    BorrowAuditOperation::Load,
                    origins,
                    Some(*lhs),
                    None,
                    "loading a stored reference must restore its origin",
                ));
            }
            aggregate if is_aggregate_rvalue(aggregate) => {
                let origins = rvalue_operand_origins(aggregate, temp_origins);
                if !origins.is_empty() {
                    observations.push(BorrowAuditObservation {
                        block,
                        statement,
                        operation: BorrowAuditOperation::Aggregate,
                        status: if temp_origins
                            .get(lhs.as_usize())
                            .is_some_and(|tracked| !tracked.is_empty())
                        {
                            OriginStatus::Preserved
                        } else {
                            OriginStatus::Lost
                        },
                        origins: temp_origins.get(lhs.as_usize()).cloned().unwrap_or(origins),
                        result: Some(*lhs),
                        callee: None,
                        reason: "aggregate holder paths preserve nested reference origins",
                    });
                }
            }
            _ => {}
        },
        AmirStmt::Store { lhs, rhs } => {
            let origins = operand_origins(rhs, temp_origins);
            if !origins.is_empty() {
                observations.push(observation(
                    block,
                    statement,
                    BorrowAuditOperation::Store,
                    local_origins
                        .get(lhs.local.as_usize())
                        .cloned()
                        .unwrap_or(origins),
                    None,
                    None,
                    "storing a carrier retains nested reference origins in the destination place",
                ));
            }
        }
        AmirStmt::Call {
            lhs: Some(lhs),
            callee,
            args: _,
            return_borrow,
        } => {
            let callee_symbol = match callee {
                AmirOperand::FunctionRef(symbol) => Some(*symbol),
                _ => None,
            };
            if return_borrow.is_some() || is_reference_temp(func, interner, *lhs) {
                let tracked = temp_origins
                    .get(lhs.as_usize())
                    .cloned()
                    .unwrap_or_default();
                observations.push(BorrowAuditObservation {
                    block,
                    statement,
                    operation: BorrowAuditOperation::CallResult,
                    status: if !tracked.is_empty() {
                        OriginStatus::Preserved
                    } else {
                        OriginStatus::Lost
                    },
                    origins: tracked,
                    result: Some(*lhs),
                    callee: callee_symbol,
                    reason: if return_borrow.is_some() {
                        "AMIR Call composes its exported structural return-dependency summary"
                    } else {
                        "borrow-bearing call result has no exported return-dependency summary"
                    },
                });
            }
        }
        _ => {}
    }
}

fn observe_terminator(
    func: &AmirFunc,
    interner: &TypeInterner,
    block: BlockId,
    statement: usize,
    terminator: &AmirTerminator,
    temp_origins: &[Vec<LocalId>],
    observations: &mut Vec<BorrowAuditObservation>,
) {
    let mut observe_args = |args: &[AmirOperand]| {
        for arg in args {
            let origins = operand_origins(arg, temp_origins);
            if !origins.is_empty() {
                observations.push(observation(
                    block,
                    statement,
                    BorrowAuditOperation::BlockArgument,
                    origins,
                    None,
                    None,
                    "CFG block arguments preserve reference origins",
                ));
            }
        }
    };

    match terminator {
        AmirTerminator::Return if is_reference_type(interner, func.return_type) => {
            let origins = temp_origins.first().cloned().unwrap_or_default();
            observations.push(observation(
                block,
                statement,
                BorrowAuditOperation::Return,
                origins,
                Some(TempId::from_usize(0)),
                None,
                "borrowed return must identify all source parameters or owners",
            ));
        }
        AmirTerminator::Goto { args, .. } | AmirTerminator::Suspend { args, .. } => {
            observe_args(args);
        }
        AmirTerminator::Branch {
            true_args,
            false_args,
            ..
        } => {
            observe_args(true_args);
            observe_args(false_args);
        }
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            for (_, _, args) in targets {
                observe_args(args);
            }
            observe_args(&otherwise.1);
        }
        AmirTerminator::Unreachable | AmirTerminator::Return | AmirTerminator::Trap(_) => {}
    }
}

fn observation(
    block: BlockId,
    statement: usize,
    operation: BorrowAuditOperation,
    origins: Vec<LocalId>,
    result: Option<TempId>,
    callee: Option<SymbolId>,
    reason: &'static str,
) -> BorrowAuditObservation {
    BorrowAuditObservation {
        block,
        statement,
        operation,
        status: if origins.is_empty() {
            OriginStatus::Lost
        } else {
            OriginStatus::Preserved
        },
        origins,
        result,
        callee,
        reason,
    }
}

fn is_reference_temp(func: &AmirFunc, interner: &TypeInterner, temp: TempId) -> bool {
    func.temps
        .get(temp.as_usize())
        .is_some_and(|value| is_reference_type(interner, value.ty))
}

fn is_reference_type(interner: &TypeInterner, ty: crate::types::TypeId) -> bool {
    interner
        .try_resolve(ty)
        .is_some_and(|ty| matches!(ty, ArType::Ref(_) | ArType::RefMut(_)))
}

fn operand_origins(operand: &AmirOperand, temp_origins: &[Vec<LocalId>]) -> Vec<LocalId> {
    match operand {
        AmirOperand::Copy(temp) | AmirOperand::Move(temp) => temp_origins
            .get(temp.as_usize())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn rvalue_operand_origins(rvalue: &AmirRvalue, temp_origins: &[Vec<LocalId>]) -> Vec<LocalId> {
    let mut origins = BTreeSet::new();
    crate::amir::for_each_rvalue_operand(rvalue, |operand| {
        origins.extend(operand_origins(operand, temp_origins));
    });
    origins.into_iter().collect()
}

fn is_aggregate_rvalue(rvalue: &AmirRvalue) -> bool {
    matches!(
        rvalue,
        AmirRvalue::StructLiteral { .. }
            | AmirRvalue::Array { .. }
            | AmirRvalue::Tuple { .. }
            | AmirRvalue::EnumConstruct { .. }
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::Span;
    use crate::amir::{AmirBasicBlock, AmirLocal, AmirPlace, AmirStmtTable, AmirTemp};
    use crate::cfg::compute_cfg_edges;
    use crate::layout::DenseRange;
    use crate::types::Primitive;
    use smallvec::smallvec;

    fn reference_func(with_call: bool) -> (AmirFunc, TypeInterner) {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Borrow(AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec![],
            }),
        });
        if with_call {
            stmts.push(AmirStmt::Call {
                lhs: Some(TempId::from_usize(2)),
                callee: AmirOperand::FunctionRef(SymbolId::new(0, 9)),
                args: smallvec![AmirOperand::Copy(TempId::from_usize(1))],
                return_borrow: None,
            });
        } else {
            stmts.push(AmirStmt::Assign {
                lhs: TempId::from_usize(2),
                rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(1))),
            });
        }
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Use(AmirOperand::Copy(TempId::from_usize(2))),
        });
        let blocks = vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            params: DenseRange::empty(),
            statements: DenseRange::new(0, 3),
            terminator: AmirTerminator::Return,
        }];
        let cfg = compute_cfg_edges(&blocks);
        let func = AmirFunc {
            symbol: SymbolId::new(0, 1),
            return_type: ref_int,
            receiver: None,
            params: Vec::new(),
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int,
                is_memory: true,
                symbol: None,
                span: Span::new(0, 0, 1),
                use_span: None,
            }],
            temps: (0..3)
                .map(|index| AmirTemp {
                    id: TempId::from_usize(index),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: Span::new(0, index as u32, index as u32 + 1),
                })
                .collect(),
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };
        (func, interner)
    }

    #[test]
    fn copies_and_returns_preserve_the_complete_origin_set() {
        let (func, interner) = reference_func(false);
        let report = audit_borrow_origins(&func, &interner);

        assert!(!report.has_origin_loss(), "{report:#?}");
        assert!(report.observations.iter().any(|item| {
            item.operation == BorrowAuditOperation::Return
                && item.origins == vec![LocalId::from_usize(0)]
        }));
    }

    #[test]
    fn reference_call_result_is_reported_as_an_origin_gap() {
        let (func, interner) = reference_func(true);
        let first = audit_borrow_origins(&func, &interner);
        let second = audit_borrow_origins(&func, &interner);

        assert_eq!(first, second, "audit evidence must be deterministic");
        assert!(first.has_origin_loss());
        assert!(first.observations.iter().any(|item| {
            item.operation == BorrowAuditOperation::CallResult
                && item.status == OriginStatus::Lost
                && item.callee == Some(SymbolId::new(0, 9))
        }));
    }

    #[test]
    fn summarized_call_result_preserves_the_argument_origin() {
        let (mut func, interner) = reference_func(true);
        let AmirStmt::Call { return_borrow, .. } = func
            .stmts
            .get_mut(crate::amir::InstrId::from_usize(1))
            .unwrap()
        else {
            panic!("expected call statement");
        };
        *return_borrow = Some(crate::amir::CallBorrowDependency::direct(
            0,
            crate::types::BorrowKind::Shared,
        ));

        let report = audit_borrow_origins(&func, &interner);
        assert!(!report.has_origin_loss(), "{report:#?}");
        assert!(report.observations.iter().any(|item| {
            item.operation == BorrowAuditOperation::CallResult
                && item.status == OriginStatus::Preserved
                && item.origins == vec![LocalId::from_usize(0)]
        }));
    }

    #[test]
    fn aggregate_holder_preserves_its_nested_origin() {
        let (mut func, interner) = reference_func(false);
        *func
            .stmts
            .get_mut(crate::amir::InstrId::from_usize(1))
            .unwrap() = AmirStmt::Assign {
            lhs: TempId::from_usize(2),
            rhs: AmirRvalue::Tuple {
                items: vec![AmirOperand::Copy(TempId::from_usize(1))],
            },
        };

        let report = audit_borrow_origins(&func, &interner);
        assert!(report.observations.iter().any(|item| {
            item.operation == BorrowAuditOperation::Aggregate
                && item.status == OriginStatus::Preserved
                && item.origins == vec![LocalId::from_usize(0)]
        }));
    }
}
