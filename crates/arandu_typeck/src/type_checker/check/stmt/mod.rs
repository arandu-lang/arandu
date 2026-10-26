mod flow;
mod return_;
mod set;
mod var;

use arandu_parser::Stmt;
use arandu_parser::ast_pool::AstPool;

use super::super::TypeChecker;
use super::block::check_block;

use flow::{
    check_defer_stmt, check_for_stmt, check_free_stmt, check_if_stmt, check_loop_control_stmt,
    check_while_stmt,
};
use return_::check_return_stmt;
use set::check_set_stmt;
use var::check_var_decl_stmt;

/// Roteador principal de checagem de tipo de instruções.
#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, pool, stmt))]
pub fn check_stmt(checker: &mut TypeChecker<'_>, pool: &AstPool, stmt: &Stmt) {
    match stmt {
        Stmt::VarDecl {
            span: _,
            bindings,
            value,
        } => {
            check_var_decl_stmt(checker, bindings, *value);
        }
        Stmt::Set {
            span: _,
            places,
            op: _,
            value,
        } => {
            check_set_stmt(checker, places, *value);
        }
        Stmt::Return { span, values } => {
            check_return_stmt(checker, *span, values);
        }
        Stmt::Expr { expr, .. } => {
            super::super::synth::synth_expr(checker, *expr);
        }
        Stmt::If {
            span,
            is_comptime,
            condition,
            then_block,
            else_block,
        } => {
            if *is_comptime {
                if let Some(&selected) = checker.resolved.comptime_branches.get(&(*span).into()) {
                    if selected {
                        check_block(checker, pool, then_block);
                    } else if let Some(block) = else_block {
                        check_block(checker, pool, block);
                    }
                }
            } else {
                check_if_stmt(checker, pool, condition, then_block, else_block.as_ref());
            }
        }
        Stmt::While {
            span: _,
            condition,
            body,
        } => {
            check_while_stmt(checker, pool, condition, body);
        }
        Stmt::For {
            span,
            is_comptime,
            clause,
            body,
        } => {
            if *is_comptime
                && !checker
                    .resolved
                    .comptime_loops
                    .contains_key(&(*span).into())
            {
                return;
            }
            if checker
                .resolved
                .deferred_loop_bodies
                .contains(&(*span).into())
            {
                let empty = arandu_parser::Block {
                    span: body.span,
                    statements: arandu_parser::ast_pool::IndexRange::empty(),
                };
                check_for_stmt(checker, pool, clause, &empty);
            } else {
                check_for_stmt(checker, pool, clause, body);
            }
        }
        Stmt::Match { span: _, expr } => {
            super::super::synth::synth_expr(checker, *expr);
        }
        Stmt::Free { span, expr } => {
            check_free_stmt(checker, *span, *expr);
        }
        Stmt::Defer { span: _, body } | Stmt::ErrDefer { span: _, body } => {
            check_defer_stmt(checker, pool, body);
        }
        Stmt::Break { span } | Stmt::Continue { span } => {
            check_loop_control_stmt(checker, stmt, *span);
        }
        Stmt::Unsafe { span: _, block } => {
            checker.ctx.enter_unsafe();
            check_block(checker, pool, block);
            checker.ctx.exit_unsafe();
        }
        Stmt::Error(_) => {}
    }
}
