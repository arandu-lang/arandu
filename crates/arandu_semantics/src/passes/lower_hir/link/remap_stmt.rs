use super::PoolOffsets;
use super::remap_expr::remap_condition;
use arandu_middle::hir::{HirForClause, HirSimpleStmt, HirStmt, HirStmtKind};
use arandu_middle::types::{TypeId, TypeInterner};
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

fn remap_simple_stmt(stmt: &HirSimpleStmt, offs: &PoolOffsets) -> HirSimpleStmt {
    match stmt {
        HirSimpleStmt::VarDecl { bindings, value } => HirSimpleStmt::VarDecl {
            bindings: offs.range(*bindings, offs.bindings),
            value: offs.expr_id(*value),
        },
        HirSimpleStmt::Set { places, op, value } => HirSimpleStmt::Set {
            places: offs.range(*places, offs.places),
            op: *op,
            value: offs.expr_id(*value),
        },
        HirSimpleStmt::Expr(e) => HirSimpleStmt::Expr(offs.expr_id(*e)),
    }
}

pub(super) fn remap_stmt(
    stmt: &HirStmt,
    offs: &PoolOffsets,
    _from: &TypeInterner,
    _dest_tc: &mut TypeCheckResult,
    _ty_cache: &mut FxHashMap<TypeId, TypeId>,
) -> HirStmt {
    let kind = match &stmt.kind {
        HirStmtKind::VarDecl { bindings, value } => HirStmtKind::VarDecl {
            bindings: offs.range(*bindings, offs.bindings),
            value: offs.expr_id(*value),
        },
        HirStmtKind::Set { places, op, value } => HirStmtKind::Set {
            places: offs.range(*places, offs.places),
            op: *op,
            value: offs.expr_id(*value),
        },
        HirStmtKind::Return { values } => HirStmtKind::Return {
            values: offs.range(*values, offs.expr_ids),
        },
        HirStmtKind::Break => HirStmtKind::Break,
        HirStmtKind::Continue => HirStmtKind::Continue,
        HirStmtKind::Free(e) => HirStmtKind::Free(offs.expr_id(*e)),
        HirStmtKind::Expr(e) => HirStmtKind::Expr(offs.expr_id(*e)),
        HirStmtKind::If {
            condition,
            then_block,
            else_block,
        } => HirStmtKind::If {
            condition: remap_condition(condition, offs),
            then_block: offs.block_id(*then_block),
            else_block: else_block.map(|b| offs.block_id(b)),
        },
        HirStmtKind::For {
            clause,
            body,
            comptime_bounds,
            comptime_bodies,
        } => HirStmtKind::For {
            comptime_bounds: *comptime_bounds,
            comptime_bodies: comptime_bodies
                .as_ref()
                .map(|bodies| bodies.iter().map(|&body| offs.block_id(body)).collect()),
            clause: match clause {
                HirForClause::In {
                    span,
                    bindings,
                    iterable,
                } => HirForClause::In {
                    span: *span,
                    bindings: offs.range(*bindings, offs.for_bindings),
                    iterable: offs.expr_id(*iterable),
                },
                HirForClause::CStyle {
                    span,
                    init,
                    condition,
                    step,
                } => HirForClause::CStyle {
                    span: *span,
                    init: init.as_ref().map(|s| remap_simple_stmt(s, offs)),
                    condition: condition.map(|c| offs.expr_id(c)),
                    step: step.as_ref().map(|s| remap_simple_stmt(s, offs)),
                },
            },
            body: offs.block_id(*body),
        },
        HirStmtKind::While { condition, body } => HirStmtKind::While {
            condition: remap_condition(condition, offs),
            body: offs.block_id(*body),
        },
        HirStmtKind::Match { value, arms } => HirStmtKind::Match {
            value: offs.expr_id(*value),
            arms: offs.range(*arms, offs.match_arms),
        },
        HirStmtKind::Defer(b) => HirStmtKind::Defer(offs.block_id(*b)),
        HirStmtKind::ErrDefer(b) => HirStmtKind::ErrDefer(offs.block_id(*b)),
        HirStmtKind::Unsafe(b) => HirStmtKind::Unsafe(offs.block_id(*b)),
        HirStmtKind::Scope(b) => HirStmtKind::Scope(offs.block_id(*b)),
        HirStmtKind::Error => HirStmtKind::Error,
    };
    HirStmt {
        kind,
        span: stmt.span,
    }
}
