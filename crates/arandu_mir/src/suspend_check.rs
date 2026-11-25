//! A3.2: OSSA checks at suspension frontiers (`await` / [`AmirTerminator::Suspend`]).
//!
//! Absolute (`Borrow`) refs whose live range crosses a suspend point would dangle
//! if the task state moves. A3.4 rewrites eligible borrows to pin-free
//! [`crate::amir::AmirRvalue::RelativeBorrow`] first; **this** pass only rejects
//! remaining absolute `Ref`/`RefMut` temps still live into a resume block (O010).

use crate::DiagCode;
use crate::SymbolTable;
use crate::amir::{AmirFunc, AmirTerminator};
use crate::borrow_facts::analyze_borrow_facts;
use crate::diagnostics::Diagnostic;
use crate::types::TypeInterner;

/// Reject borrows whose temp is live into a resume block after `Suspend`.
pub fn check_borrow_across_suspend(
    func: &AmirFunc,
    _symbols: &SymbolTable,
    _interner: &TypeInterner,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let has_suspend = func
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, AmirTerminator::Suspend { .. }));
    if !has_suspend {
        return diags;
    }

    let facts = analyze_borrow_facts(func);

    for block in &func.blocks {
        let AmirTerminator::Suspend { resume, args, .. } = &block.terminator else {
            continue;
        };
        let live_into_resume = facts.temp_live.live_in(*resume);
        for loan in &facts.loans {
            if loan.relative {
                continue;
            }
            let Some(holder) = loan.holder_temps.iter().find(|temp| {
                live_into_resume.contains(*temp)
                    || args.iter().any(|argument| {
                        matches!(
                            argument,
                            crate::amir::AmirOperand::Copy(found)
                                | crate::amir::AmirOperand::Move(found)
                                if found == temp
                        )
                    })
            }) else {
                continue;
            };
            let span = func
                .temps
                .get(holder.as_usize())
                .map_or_else(|| crate::Span::new(0, 0, 0), |temp| temp.span);
            diags.push(
                Diagnostic::error(
                    DiagCode::O010EscapeOfBorrowedValue,
                    "borrow cannot cross an `await` suspension point \
                     (absolute reference would outlive the stack frame / task state)",
                    span,
                )
                .with_label(
                    span,
                    "this value contains an absolute reference that is still live when the coroutine suspends",
                )
                .with_hint(
                    "copy the referent before `await`, or keep the borrow confined to \
                     a local variable or field that does not escape into a call",
                ),
            );
        }
    }

    diags
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amir::{
        AmirBasicBlock, AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirRvalue, AmirStmt,
        AmirStmtTable, AmirTemp, BlockId, LocalId, TempId,
    };
    use crate::layout::DenseRange;
    use crate::types::{ArType, Primitive};
    use smallvec::smallvec;

    #[test]
    fn aggregate_with_absolute_borrow_is_rejected_across_suspend() {
        let interner = TypeInterner::new();
        let int = interner.intern(ArType::Primitive(Primitive::Int));
        let ref_int = interner.intern(ArType::Ref(int));
        let tuple = interner.intern(ArType::tuple(&[ref_int], &interner));
        let coroutine = interner.intern(ArType::Coroutine(int));
        let mut stmts = AmirStmtTable::new();
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(0),
            rhs: AmirRvalue::Borrow(AmirPlace {
                local: LocalId::from_usize(0),
                projections: smallvec![],
            }),
        });
        stmts.push(AmirStmt::Assign {
            lhs: TempId::from_usize(1),
            rhs: AmirRvalue::Tuple {
                items: vec![AmirOperand::Copy(TempId::from_usize(0))],
            },
        });
        let blocks = vec![
            AmirBasicBlock {
                id: BlockId::from_usize(0),
                statements: DenseRange::new(0, 2),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Suspend {
                    future: AmirOperand::Copy(TempId::from_usize(2)),
                    resume: BlockId::from_usize(1),
                    args: vec![AmirOperand::Copy(TempId::from_usize(1))],
                },
            },
            AmirBasicBlock {
                id: BlockId::from_usize(1),
                statements: DenseRange::new(2, 0),
                params: DenseRange::empty(),
                terminator: AmirTerminator::Return,
            },
        ];
        let cfg = crate::cfg::compute_cfg_edges(&blocks);
        let func = AmirFunc {
            symbol: crate::SymbolId::new(0, 0),
            return_type: int,
            receiver: None,
            params: vec![],
            locals: vec![AmirLocal {
                id: LocalId::from_usize(0),
                ty: int,
                is_memory: true,
                symbol: None,
                span: crate::Span::new(0, 0, 1),
                use_span: None,
            }],
            temps: vec![
                AmirTemp {
                    id: TempId::from_usize(0),
                    ty: ref_int,
                    is_copy: true,
                    is_nullable: false,
                    span: crate::Span::new(0, 1, 2),
                },
                AmirTemp {
                    id: TempId::from_usize(1),
                    ty: tuple,
                    is_copy: true,
                    is_nullable: false,
                    span: crate::Span::new(0, 2, 3),
                },
                AmirTemp {
                    id: TempId::from_usize(2),
                    ty: coroutine,
                    is_copy: false,
                    is_nullable: false,
                    span: crate::Span::new(0, 3, 4),
                },
            ],
            blocks,

            block_params: Vec::new(),

            stmts,
            cfg,
        };

        let diagnostics = check_borrow_across_suspend(&func, &SymbolTable::new(0), &interner);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, DiagCode::O010EscapeOfBorrowedValue);
    }
}
