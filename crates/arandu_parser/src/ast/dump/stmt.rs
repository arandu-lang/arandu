use arandu_lexer::Span;

use super::super::ast_pool::AstPool;
use super::super::{
    BindingItem, Block, Condition, DeferBody, ForClause, Place, PlaceSuffix, SimpleStmt, Stmt,
};
use super::decl::dump_type;
use super::expr::{dump_expr, dump_pattern};
use super::{dump_set_op, dump_span};

pub(super) fn dump_block_body(pool: &AstPool, block: &Block, out: &mut Vec<String>, indent: usize) {
    for &stmt in pool.stmt_list(block.statements) {
        dump_stmt(pool, stmt, out, indent);
    }
}

pub(super) fn dump_stmt(
    pool: &AstPool,
    stmt: crate::ast_pool::StmtId,
    out: &mut Vec<String>,
    indent: usize,
) {
    let stmt = pool.stmt(stmt);
    let pad = " ".repeat(indent);
    match stmt {
        Stmt::VarDecl {
            span,
            bindings,
            value,
        } => {
            let bindings = bindings
                .iter()
                .map(|b| dump_binding(pool, b))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!(
                "{pad}Var {} {bindings} = {}",
                dump_span(*span),
                dump_expr(pool, *value)
            ));
        }
        Stmt::Set {
            span,
            places,
            op,
            value,
        } => {
            let places = places
                .iter()
                .map(|p| dump_place(pool, p))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!(
                "{pad}Set {} {places} {} {}",
                dump_span(*span),
                dump_set_op(op),
                dump_expr(pool, *value)
            ));
        }
        Stmt::Return { span, values } => {
            let values = values
                .iter()
                .map(|val| dump_expr(pool, *val))
                .collect::<Vec<_>>()
                .join(", ");
            if values.is_empty() {
                out.push(format!("{pad}Return {}", dump_span(*span)));
            } else {
                out.push(format!("{pad}Return {} {values}", dump_span(*span)));
            }
        }
        Stmt::Break { span } => out.push(format!("{pad}Break {}", dump_span(*span))),
        Stmt::Continue { span } => out.push(format!("{pad}Continue {}", dump_span(*span))),
        Stmt::Free { span, expr } => {
            out.push(format!(
                "{pad}Free {} {}",
                dump_span(*span),
                dump_expr(pool, *expr)
            ));
        }
        Stmt::Expr { span, expr, .. } => {
            out.push(format!(
                "{pad}Expr {} {}",
                dump_span(*span),
                dump_expr(pool, *expr)
            ));
        }
        Stmt::If {
            span,
            condition,
            then_block,
            else_block,
        } => {
            out.push(format!(
                "{pad}If {} {}",
                dump_span(*span),
                dump_condition(pool, condition)
            ));
            dump_block_body(pool, then_block, out, indent + 2);
            if let Some(else_block) = else_block {
                out.push(format!("{pad}Else {}", dump_span(else_block.span)));
                dump_block_body(pool, else_block, out, indent + 2);
            }
        }
        Stmt::For { span, clause, body } => {
            out.push(format!(
                "{pad}For {}{}",
                dump_span(*span),
                dump_for_clause(pool, clause)
            ));
            dump_block_body(pool, body, out, indent + 2);
        }
        Stmt::While {
            span,
            condition,
            body,
        } => {
            out.push(format!(
                "{pad}While {} {}",
                dump_span(*span),
                dump_condition(pool, condition)
            ));
            dump_block_body(pool, body, out, indent + 2);
        }
        Stmt::Match { span, expr } => {
            out.push(format!(
                "{pad}MatchStmt {} {}",
                dump_span(*span),
                dump_expr(pool, *expr)
            ));
        }
        Stmt::Defer { span, body } => dump_defer_body(pool, "Defer", *span, body, out, indent),
        Stmt::ErrDefer { span, body } => {
            dump_defer_body(pool, "ErrDefer", *span, body, out, indent)
        }
        Stmt::Unsafe { span, block } => {
            out.push(format!("{pad}Unsafe {}", dump_span(*span)));
            dump_block_body(pool, block, out, indent + 2);
        }
        Stmt::Error(span) => out.push(format!("{pad}StmtError {}", dump_span(*span))),
    }
}

pub(super) fn dump_condition(pool: &AstPool, condition: &Condition) -> String {
    match condition {
        Condition::Expr { span, expr } => {
            format!("Condition {} {}", dump_span(*span), dump_expr(pool, *expr))
        }
        Condition::Is {
            span,
            expr,
            pattern,
        } => {
            format!(
                "Is {} ({}, {})",
                dump_span(*span),
                dump_expr(pool, *expr),
                dump_pattern(pool, pool.pattern(*pattern))
            )
        }
        Condition::And { span, conditions } => {
            let clauses = conditions
                .iter()
                .map(|condition| dump_condition(pool, condition))
                .collect::<Vec<_>>()
                .join(", ");
            format!("And {} ({clauses})", dump_span(*span))
        }
    }
}

fn dump_for_clause(pool: &AstPool, clause: &ForClause) -> String {
    match clause {
        ForClause::In {
            span,
            bindings,
            iterable,
        } => {
            let bindings = bindings
                .iter()
                .map(|binding| {
                    if binding.mutable {
                        format!("mut {}", binding.name)
                    } else {
                        binding.name.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                " In {} {bindings} in {}",
                dump_span(*span),
                dump_expr(pool, *iterable)
            )
        }
        ForClause::CStyle {
            span,
            init,
            condition,
            step,
        } => format!(
            " C {} init {}; condition {}; step {}",
            dump_span(*span),
            init.as_ref()
                .map_or_else(|| "none".to_string(), |stmt| dump_simple_stmt(pool, stmt)),
            condition
                .as_ref()
                .map_or_else(|| "none".to_string(), |expr| dump_expr(pool, *expr)),
            step.as_ref()
                .map_or_else(|| "none".to_string(), |stmt| dump_simple_stmt(pool, stmt))
        ),
    }
}

fn dump_simple_stmt(pool: &AstPool, stmt: &SimpleStmt) -> String {
    match stmt {
        SimpleStmt::VarDecl {
            span,
            bindings,
            value,
        } => {
            let bindings = bindings
                .iter()
                .map(|b| dump_binding(pool, b))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Var {} {bindings} = {}",
                dump_span(*span),
                dump_expr(pool, *value)
            )
        }
        SimpleStmt::Set {
            span,
            places,
            op,
            value,
        } => {
            let places = places
                .iter()
                .map(|p| dump_place(pool, p))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Set {} {places} {} {}",
                dump_span(*span),
                dump_set_op(op),
                dump_expr(pool, *value)
            )
        }
        SimpleStmt::Expr { span, expr } => {
            format!("Expr {} {}", dump_span(*span), dump_expr(pool, *expr))
        }
    }
}

pub(super) fn dump_defer_body(
    pool: &AstPool,
    label: &str,
    span: Span,
    body: &DeferBody,
    out: &mut Vec<String>,
    indent: usize,
) {
    let pad = " ".repeat(indent);
    match body {
        DeferBody::Expr { expr, .. } => {
            out.push(format!(
                "{pad}{label} {} Expr({})",
                dump_span(span),
                dump_expr(pool, *expr)
            ));
        }
        DeferBody::Block { block, .. } => {
            out.push(format!(
                "{pad}{label} {} Block {}",
                dump_span(span),
                dump_span(block.span)
            ));
            dump_block_body(pool, block, out, indent + 2);
        }
    }
}

fn dump_binding(pool: &AstPool, binding: &BindingItem) -> String {
    let mut out = format!("{} ", dump_span(binding.span));
    if binding.mutable {
        out.push_str("mut ");
    }
    out.push_str(&binding.name);
    if let Some(ty) = &binding.ty {
        out.push(' ');
        out.push_str(&dump_type(pool.type_expr(*ty), pool));
    }
    out
}

pub(super) fn dump_place(pool: &AstPool, place: &Place) -> String {
    let mut out = format!("{} {}", dump_span(place.span), place.root);
    for suffix in &place.suffixes {
        match suffix {
            PlaceSuffix::Field { name, .. } => {
                out.push('.');
                out.push_str(name);
            }
            PlaceSuffix::Index { expr, .. } => {
                out.push('[');
                out.push_str(&dump_expr(pool, *expr));
                out.push(']');
            }
        }
    }
    out
}

pub(super) fn dump_inline_block(pool: &AstPool, label: &str, span: Span, block: &Block) -> String {
    format!(
        "{label} {} {}",
        dump_span(span),
        dump_block_inline(pool, block)
    )
}

pub(super) fn dump_block_inline(pool: &AstPool, block: &Block) -> String {
    let stmts = pool
        .stmt_list(block.statements)
        .iter()
        .map(|stmt| dump_stmt_inline(pool, *stmt))
        .collect::<Vec<_>>()
        .join("; ");
    format!("Block {}[{stmts}]", dump_span(block.span))
}

pub(super) fn dump_stmt_inline(pool: &AstPool, stmt: crate::ast_pool::StmtId) -> String {
    let mut out = Vec::new();
    dump_stmt(pool, stmt, &mut out, 0);
    out.join(" ")
}
