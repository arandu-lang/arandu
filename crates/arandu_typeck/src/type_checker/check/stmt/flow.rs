use arandu_parser::Stmt;
use arandu_parser::ast_pool::AstPool;

use super::super::super::TypeChecker;
use super::super::super::constraints::ConstraintOrigin;
use super::super::super::types::{ArType, Primitive};
use super::super::block::check_block;
use super::super::condition::check_condition;

pub(super) fn check_if_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    condition: &arandu_parser::Condition,
    then_block: &arandu_parser::Block,
    else_block: Option<&arandu_parser::Block>,
) {
    check_condition(checker, condition);
    check_block(checker, pool, then_block);
    if let Some(eb) = else_block {
        check_block(checker, pool, eb);
    }
}

pub(super) fn check_while_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    condition: &arandu_parser::Condition,
    body: &arandu_parser::Block,
) {
    check_condition(checker, condition);
    checker.ctx.enter_loop();
    check_block(checker, pool, body);
    checker.ctx.exit_loop();
}

pub(super) fn check_for_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    clause: &arandu_parser::ForClause,
    body: &arandu_parser::Block,
) {
    match clause {
        arandu_parser::ForClause::In {
            span: _,
            bindings,
            iterable,
        } => {
            let iterable_ty_id = super::super::super::synth::synth_expr(checker, *iterable);
            let iterable_ty = checker.resolve(iterable_ty_id);
            let elem_ty = match &iterable_ty {
                ArType::Array(_, inner)
                | ArType::ConstArray(_, inner)
                | ArType::Slice(inner)
                | ArType::MutSlice(inner)
                | ArType::Range(inner) => checker.type_info.type_interner.resolve(*inner),
                ArType::Error => ArType::Error,
                _ => {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T005OperatorNotApplicable,
                        format!(
                            "expected array, slice, or range, found '{}'",
                            iterable_ty.display(&checker.symbols, &checker.type_info.type_interner)
                        ),
                        checker.pool.expr_span(*iterable),
                    ));
                    ArType::Error
                }
            };
            if let Some(binding) = bindings.first() {
                let binding_key = crate::NodeKey::from(binding.span);
                if let Some(symbol_id) = checker.resolved.definitions.get(&binding_key).copied() {
                    let elem_ty_id = checker.intern(elem_ty);
                    checker.ctx.bind(symbol_id, elem_ty_id);
                    checker.record_decl_type(symbol_id, elem_ty_id);
                }
            }
        }
        arandu_parser::ForClause::CStyle {
            init,
            condition,
            step,
            ..
        } => {
            if let Some(init_stmt) = init {
                check_simple_stmt(checker, pool, init_stmt);
            }
            if let Some(cond_expr) = condition {
                let cond_ty_id = super::super::super::synth::synth_expr(checker, *cond_expr);
                let cond_ty = checker.resolve(cond_ty_id);
                if !cond_ty.is_error()
                    && !super::super::super::types::unify(
                        &cond_ty,
                        &ArType::Primitive(Primitive::Bool),
                        &checker.type_info.type_interner,
                    )
                {
                    checker.add_constraint(
                        ArType::Primitive(Primitive::Bool),
                        cond_ty,
                        ConstraintOrigin::Condition {
                            span: checker.pool.expr_span(*cond_expr),
                        },
                    );
                }
            }
            if let Some(step_stmt) = step {
                check_simple_stmt(checker, pool, step_stmt);
            }
        }
    }
    checker.ctx.enter_loop();
    check_block(checker, pool, body);
    checker.ctx.exit_loop();
}

fn check_simple_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    stmt: &arandu_parser::SimpleStmt,
) {
    match stmt {
        arandu_parser::SimpleStmt::VarDecl {
            span,
            bindings,
            value,
        } => {
            super::check_stmt(
                checker,
                pool,
                &Stmt::VarDecl {
                    span: *span,
                    bindings: bindings.clone(),
                    value: *value,
                },
            );
        }
        arandu_parser::SimpleStmt::Set {
            span,
            places,
            op,
            value,
        } => {
            super::check_stmt(
                checker,
                pool,
                &Stmt::Set {
                    span: *span,
                    places: places.clone(),
                    op: op.clone(),
                    value: *value,
                },
            );
        }
        arandu_parser::SimpleStmt::Expr { expr, span: _ } => {
            super::super::super::synth::synth_expr(checker, *expr);
        }
    }
}

pub(super) fn check_free_stmt(
    checker: &mut TypeChecker<'_>,
    span: arandu_base::Span,
    expr: arandu_parser::ast_pool::ExprId,
) {
    checker.current_observed_effects = checker
        .current_observed_effects
        .union(arandu_middle::EffectFlags::HEAP);
    if !checker.ctx.is_in_unsafe() {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::O014FreeRequiresUnsafe,
                "`free` requires an `unsafe` block",
                span,
            )
            .with_label(
                span,
                "`free` is unsafe and must be inside an `unsafe` block",
            ),
        );
    }
    let ty_id = super::super::super::synth::synth_expr(checker, expr);
    let ty = checker.resolve(ty_id);
    if !ty.is_error() && !matches!(ty, ArType::Ptr(_)) {
        let interner = &checker.type_info.type_interner;
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::O011FreeRequiresPtr,
                format!(
                    "`free` requires a pointer type (`ptr[T]`), found '{}'",
                    ty.display(&checker.symbols, interner)
                ),
                span,
            )
            .with_label(
                checker.pool.expr_span(expr),
                format!(
                    "expression has type '{}'",
                    ty.display(&checker.symbols, interner)
                ),
            ),
        );
    }
}

pub(super) fn check_defer_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    body: &arandu_parser::DeferBody,
) {
    match body {
        arandu_parser::DeferBody::Block { block, .. } => {
            check_block(checker, pool, block);
        }
        arandu_parser::DeferBody::Expr { expr, .. } => {
            super::super::super::synth::synth_expr(checker, *expr);
        }
    }
}

pub(super) fn check_loop_control_stmt(
    checker: &mut TypeChecker<'_>,
    stmt: &Stmt,
    span: arandu_base::Span,
) {
    if !checker.ctx.is_in_loop() {
        let msg = match stmt {
            Stmt::Break { .. } => "`break` is only allowed inside a loop",
            Stmt::Continue { .. } => "`continue` is only allowed inside a loop",
            // Defensive invariant: only Break/Continue reach this path. A new
            // loop-control statement added upstream must surface as a reportable
            // ICE instead of panicking the compiler out of the host process.
            _ => {
                checker.diagnostics.push(crate::Diagnostic::ice(
                    crate::DiagCode::ICET001,
                    "loop-control checker reached an unexpected statement kind",
                    span,
                ));
                return;
            }
        };
        checker.diagnostics.push(crate::Diagnostic::error(
            crate::DiagCode::N011BreakContinueOutsideLoop,
            msg,
            span,
        ));
    }
}
