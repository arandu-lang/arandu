pub(crate) fn ownership_to_receiver_kind(o: Ownership) -> ReceiverKind {
    match o {
        Ownership::Own => ReceiverKind::Own,
        Ownership::Mut => ReceiverKind::Mut,
        Ownership::Shared => ReceiverKind::Shared,
    }
}

use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{
    HirBindingItem, HirBlock, HirBlockId, HirCondition, HirForBinding, HirForClause, HirSimpleStmt,
    HirStmt, HirStmtId, HirStmtKind, ReceiverKind,
};
use crate::ops::SetOp;
use crate::passes::lowering::require_def_symbol;
use crate::passes::type_checker::types::ArType;
use arandu_lexer::Span;
use arandu_parser::ast_pool::{AstPool, ExprKind, StmtId};
use arandu_parser::{Block, Condition, DeferBody, ForClause, Ownership, SimpleStmt, Stmt};

pub(crate) fn lower_block_raw(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    block: &Block,
) -> Result<HirBlock, Diagnostic> {
    let mut statements = Vec::new();
    for &s in pool.stmt_list(block.statements) {
        statements.push(lower_stmt(type_check, pool, hir_pool, s)?);
    }
    let statements_range = hir_pool.alloc_stmt_list(&statements);
    Ok(HirBlock {
        statements: statements_range,
        span: block.span,
    })
}

pub(crate) fn lower_block(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    block: &Block,
) -> Result<HirBlockId, Diagnostic> {
    let hir = lower_block_raw(type_check, pool, hir_pool, block)?;
    Ok(hir_pool.alloc_block(hir))
}

fn stmt_span(pool: &AstPool, stmt: StmtId) -> Span {
    match pool.stmt(stmt) {
        Stmt::VarDecl { span, .. }
        | Stmt::Set { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::Break { span }
        | Stmt::Continue { span }
        | Stmt::Free { span, .. }
        | Stmt::Expr { span, .. }
        | Stmt::If { span, .. }
        | Stmt::For { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Match { span, .. }
        | Stmt::Defer { span, .. }
        | Stmt::ErrDefer { span, .. }
        | Stmt::Unsafe { span, .. }
        | Stmt::Error(span) => *span,
    }
}

fn lower_stmt_raw(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    stmt: StmtId,
) -> Result<HirStmt, Diagnostic> {
    let stmt_ref = pool.stmt(stmt);
    let kind = match stmt_ref {
        Stmt::VarDecl {
            bindings, value, ..
        } => {
            let value_ty_id = type_check.type_info.expr_type_id(*value);
            let error_id = arandu_middle::types::TypeInterner::preinterned_error_id();
            let mut hir_bindings = Vec::new();
            for (i, b) in bindings.iter().enumerate() {
                let symbol = require_def_symbol(&type_check.resolved, b.span)?;
                let ty = type_check
                    .type_info
                    .decl_type_id(symbol)
                    .or_else(|| {
                        value_ty_id.and_then(|vid| {
                            match type_check.type_info.type_interner.resolve(vid) {
                                ArType::Tuple(elems) => type_check
                                    .type_info
                                    .type_interner
                                    .type_args(elems)
                                    .get(i)
                                    .copied(),
                                _ if bindings.len() == 1 => Some(vid),
                                _ => None,
                            }
                        })
                    })
                    .unwrap_or(error_id);
                hir_bindings.push(HirBindingItem {
                    symbol,
                    ty,
                    span: b.span,
                });
            }
            let value_vid = super::expr::lower_expr(type_check, pool, hir_pool, *value)?;
            let bindings_range = hir_pool.alloc_binding_list(&hir_bindings);
            HirStmtKind::VarDecl {
                bindings: bindings_range,
                value: value_vid,
            }
        }
        Stmt::Set {
            places, op, value, ..
        } => {
            let hir_places: Result<Vec<_>, Diagnostic> = places
                .iter()
                .map(|p| super::place::lower_place(type_check, pool, hir_pool, p))
                .collect();
            let value_vid = super::expr::lower_expr(type_check, pool, hir_pool, *value)?;
            let places_range = hir_pool.alloc_place_list(&hir_places?);
            HirStmtKind::Set {
                places: places_range,
                op: SetOp::from(op.clone()),
                value: value_vid,
            }
        }
        Stmt::Return { values, .. } => {
            let hir_values: Result<Vec<_>, Diagnostic> = values
                .iter()
                .map(|v| super::expr::lower_expr(type_check, pool, hir_pool, *v))
                .collect();
            let values_range = hir_pool.alloc_expr_list(&hir_values?);
            HirStmtKind::Return {
                values: values_range,
            }
        }
        Stmt::Break { .. } => HirStmtKind::Break,
        Stmt::Continue { .. } => HirStmtKind::Continue,
        Stmt::Free { expr, .. } => {
            let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
            HirStmtKind::Free(eid)
        }
        Stmt::Expr { expr, .. } => {
            if let ExprKind::Call { callee, args, .. } = pool.expr(*expr) {
                let callee_id = *callee;
                if let Some(callee_sym) = type_check.resolved.expr_symbol(callee_id)
                    && Some(callee_sym) == type_check.symbols.builtin_free
                {
                    let arg_ids = pool.expr_list(*args);
                    let eid = super::expr::lower_expr(type_check, pool, hir_pool, arg_ids[0])?;
                    HirStmtKind::Free(eid)
                } else {
                    let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
                    HirStmtKind::Expr(eid)
                }
            } else {
                let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
                HirStmtKind::Expr(eid)
            }
        }
        Stmt::If {
            condition,
            then_block,
            else_block,
            ..
        } => HirStmtKind::If {
            condition: lower_condition(type_check, pool, hir_pool, condition)?,
            then_block: super::stmt::lower_block(type_check, pool, hir_pool, then_block)?,
            else_block: else_block
                .as_ref()
                .map(|b| super::stmt::lower_block(type_check, pool, hir_pool, b))
                .transpose()?,
        },
        Stmt::For { clause, body, .. } => HirStmtKind::For {
            clause: lower_for_clause(type_check, pool, hir_pool, clause)?,
            body: super::stmt::lower_block(type_check, pool, hir_pool, body)?,
        },
        Stmt::While {
            condition, body, ..
        } => HirStmtKind::While {
            condition: lower_condition(type_check, pool, hir_pool, condition)?,
            body: super::stmt::lower_block(type_check, pool, hir_pool, body)?,
        },
        Stmt::Match { expr, .. } => {
            let expr_id = *expr;
            match pool.expr(expr_id) {
                ExprKind::Match { value, arms } => {
                    let arm_ids = pool.match_arm_list(*arms);
                    let vid = super::expr::lower_expr(type_check, pool, hir_pool, *value)?;
                    let arms_range =
                        super::pattern::lower_match_arms(type_check, pool, hir_pool, arm_ids)?;
                    HirStmtKind::Match {
                        value: vid,
                        arms: arms_range,
                    }
                }
                _ => {
                    let eid = super::expr::lower_expr(type_check, pool, hir_pool, expr_id)?;
                    HirStmtKind::Expr(eid)
                }
            }
        }
        Stmt::Defer { body, .. } => {
            let block = match body {
                DeferBody::Expr { span, expr } => {
                    let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
                    let stmt_id = hir_pool.alloc_stmt(HirStmt {
                        kind: HirStmtKind::Expr(eid),
                        span: *span,
                    });
                    let statements_range = hir_pool.alloc_stmt_list(&[stmt_id]);
                    hir_pool.alloc_block(HirBlock {
                        statements: statements_range,
                        span: *span,
                    })
                }
                DeferBody::Block { block, .. } => {
                    super::stmt::lower_block(type_check, pool, hir_pool, block)?
                }
            };
            HirStmtKind::Defer(block)
        }
        Stmt::ErrDefer { body, .. } => {
            let block = match body {
                DeferBody::Expr { span, expr } => {
                    let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
                    let stmt_id = hir_pool.alloc_stmt(HirStmt {
                        kind: HirStmtKind::Expr(eid),
                        span: *span,
                    });
                    let statements_range = hir_pool.alloc_stmt_list(&[stmt_id]);
                    hir_pool.alloc_block(HirBlock {
                        statements: statements_range,
                        span: *span,
                    })
                }
                DeferBody::Block { block, .. } => {
                    super::stmt::lower_block(type_check, pool, hir_pool, block)?
                }
            };
            HirStmtKind::ErrDefer(block)
        }
        Stmt::Unsafe { block, .. } => {
            HirStmtKind::Unsafe(super::stmt::lower_block(type_check, pool, hir_pool, block)?)
        }
        Stmt::Error(_) => HirStmtKind::Error,
    };
    Ok(HirStmt {
        kind,
        span: stmt_span(pool, stmt),
    })
}

pub(crate) fn lower_stmt(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    stmt: StmtId,
) -> Result<HirStmtId, Diagnostic> {
    let hir = lower_stmt_raw(type_check, pool, hir_pool, stmt)?;
    Ok(hir_pool.alloc_stmt(hir))
}

pub(crate) fn lower_condition(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    cond: &Condition,
) -> Result<HirCondition, Diagnostic> {
    match cond {
        Condition::Expr { expr, .. } => {
            let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
            Ok(HirCondition::Expr(eid))
        }
        Condition::Is { expr, pattern, .. } => {
            let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
            let pat_id = super::pattern::lower_pattern_to_id(type_check, pool, hir_pool, *pattern)?;
            Ok(HirCondition::Is {
                expr: eid,
                pattern: pat_id,
            })
        }
        Condition::And { conditions, .. } => {
            let mut lowered = Vec::with_capacity(conditions.len());
            for condition in conditions {
                lowered.push(lower_condition(type_check, pool, hir_pool, condition)?);
            }
            Ok(HirCondition::And(lowered.into_boxed_slice()))
        }
    }
}

pub(crate) fn lower_for_clause(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    clause: &ForClause,
) -> Result<HirForClause, Diagnostic> {
    match clause {
        ForClause::In {
            span,
            bindings,
            iterable,
        } => {
            let mut hir_bindings = Vec::new();
            let error_id = arandu_middle::types::TypeInterner::preinterned_error_id();
            for b in bindings {
                let symbol = require_def_symbol(&type_check.resolved, b.span)?;
                let ty = type_check
                    .type_info
                    .decl_type_id(symbol)
                    .unwrap_or(error_id);
                hir_bindings.push(HirForBinding {
                    symbol,
                    ty,
                    span: b.span,
                });
            }
            let bindings_range = hir_pool.alloc_for_binding_list(&hir_bindings);
            let eid = super::expr::lower_expr(type_check, pool, hir_pool, *iterable)?;
            Ok(HirForClause::In {
                span: *span,
                bindings: bindings_range,
                iterable: eid,
            })
        }
        ForClause::CStyle {
            span,
            init,
            condition,
            step,
        } => {
            let init = init
                .as_ref()
                .map(|s| lower_simple_stmt(type_check, pool, hir_pool, s))
                .transpose()?;
            let condition = condition
                .as_ref()
                .map(|e| super::expr::lower_expr(type_check, pool, hir_pool, *e))
                .transpose()?;
            let step = step
                .as_ref()
                .map(|s| lower_simple_stmt(type_check, pool, hir_pool, s))
                .transpose()?;
            Ok(HirForClause::CStyle {
                span: *span,
                init,
                condition,
                step,
            })
        }
    }
}

pub(crate) fn lower_simple_stmt(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    stmt: &SimpleStmt,
) -> Result<HirSimpleStmt, Diagnostic> {
    match stmt {
        SimpleStmt::VarDecl {
            bindings, value, ..
        } => {
            let mut hir_bindings = Vec::new();
            let error_id = arandu_middle::types::TypeInterner::preinterned_error_id();
            for b in bindings {
                let symbol = require_def_symbol(&type_check.resolved, b.span)?;
                let ty = type_check
                    .type_info
                    .decl_type_id(symbol)
                    .unwrap_or(error_id);
                hir_bindings.push(HirBindingItem {
                    symbol,
                    ty,
                    span: b.span,
                });
            }
            let bindings_range = hir_pool.alloc_binding_list(&hir_bindings);
            let vid = super::expr::lower_expr(type_check, pool, hir_pool, *value)?;
            Ok(HirSimpleStmt::VarDecl {
                bindings: bindings_range,
                value: vid,
            })
        }
        SimpleStmt::Set {
            places, op, value, ..
        } => {
            let hir_places: Result<Vec<_>, Diagnostic> = places
                .iter()
                .map(|p| super::place::lower_place(type_check, pool, hir_pool, p))
                .collect();
            let vid = super::expr::lower_expr(type_check, pool, hir_pool, *value)?;
            let places_range = hir_pool.alloc_place_list(&hir_places?);
            Ok(HirSimpleStmt::Set {
                places: places_range,
                op: SetOp::from(op.clone()),
                value: vid,
            })
        }
        SimpleStmt::Expr { expr, .. } => {
            let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
            Ok(HirSimpleStmt::Expr(eid))
        }
    }
}
