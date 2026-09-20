//! Statement, block, place and control-flow pretty printing implementation.

use super::types::{HirPrettyCtx, display_type};
use crate::hir::{
    HirBlock, HirCondition, HirForClause, HirMatchArmBody, HirPlace, HirPlaceSuffix, HirSimpleStmt,
    HirStmt, HirStmtKind, SetOp, symbol_name,
};

impl HirBlock {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        for &stmt_id in ctx.pool.stmt_list(self.statements) {
            ctx.pool.stmt(stmt_id).pretty_print_to(out, indent, ctx);
        }
    }
}

impl HirStmt {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        match &self.kind {
            HirStmtKind::VarDecl { bindings, value } => {
                let bindings_slice = ctx.pool.bindings_list(*bindings);
                if bindings_slice.len() == 1 {
                    let b = &bindings_slice[0];
                    out.push_str(&format!(
                        "{}Var {}: {} =\n",
                        ind,
                        symbol_name(ctx.symbols, b.symbol),
                        display_type(b.ty, ctx)
                    ));
                } else {
                    let b_strs: Vec<String> = bindings_slice
                        .iter()
                        .map(|b| {
                            format!(
                                "{}: {}",
                                symbol_name(ctx.symbols, b.symbol),
                                display_type(b.ty, ctx)
                            )
                        })
                        .collect();
                    out.push_str(&format!("{}Var ({}) =\n", ind, b_strs.join(", ")));
                }
                value.pretty_print_to(out, indent + 1, ctx);
            }
            HirStmtKind::Set { places, op, value } => {
                let place_strs: Vec<String> = ctx
                    .pool
                    .places_list(*places)
                    .iter()
                    .map(|p| p.pretty_print(ctx))
                    .collect();
                out.push_str(&format!(
                    "{}Set ({}) {}\n",
                    ind,
                    place_strs.join(", "),
                    set_op_str(op)
                ));
                value.pretty_print_to(out, indent + 1, ctx);
            }
            HirStmtKind::Return { values } => {
                if values.is_empty() {
                    out.push_str(&format!("{ind}Return\n"));
                } else {
                    out.push_str(&format!("{ind}Return\n"));
                    for &v in ctx.pool.expr_list(*values) {
                        v.pretty_print_to(out, indent + 1, ctx);
                    }
                }
            }
            HirStmtKind::Break => {
                out.push_str(&format!("{ind}Break\n"));
            }
            HirStmtKind::Continue => {
                out.push_str(&format!("{ind}Continue\n"));
            }
            HirStmtKind::Free(expr) => {
                out.push_str(&format!("{ind}Free\n"));
                expr.pretty_print_to(out, indent + 1, ctx);
            }
            HirStmtKind::Expr(expr) => {
                out.push_str(&format!("{ind}Expr\n"));
                expr.pretty_print_to(out, indent + 1, ctx);
            }
            HirStmtKind::If {
                condition,
                then_block,
                else_block,
            } => {
                out.push_str(&format!("{ind}If\n"));
                condition.pretty_print_to(out, indent + 1, ctx);
                let block_ind = "  ".repeat(indent + 1);
                out.push_str(&format!("{block_ind}Then\n"));
                ctx.pool
                    .block(*then_block)
                    .pretty_print_to(out, indent + 2, ctx);
                if let Some(else_blk) = else_block {
                    out.push_str(&format!("{block_ind}Else\n"));
                    ctx.pool
                        .block(*else_blk)
                        .pretty_print_to(out, indent + 2, ctx);
                }
            }
            HirStmtKind::While { condition, body } => {
                out.push_str(&format!("{ind}While\n"));
                condition.pretty_print_to(out, indent + 1, ctx);
                ctx.pool.block(*body).pretty_print_to(out, indent + 1, ctx);
            }
            HirStmtKind::For { clause, body } => {
                out.push_str(&format!("{ind}For\n"));
                clause.pretty_print_to(out, indent + 1, ctx);
                ctx.pool.block(*body).pretty_print_to(out, indent + 1, ctx);
            }
            HirStmtKind::Match { value, arms } => {
                out.push_str(&format!("{ind}Match\n"));
                value.pretty_print_to(out, indent + 1, ctx);
                let arm_ind = "  ".repeat(indent + 1);
                for arm in ctx.pool.match_arms_list(*arms) {
                    let guard_str = if let Some(ref g) = arm.guard {
                        format!(" if {}", g.pretty_print_inline(ctx))
                    } else {
                        String::new()
                    };
                    let mut pat_str = String::new();
                    arm.pattern.pretty_print_to(&mut pat_str, 0, ctx);
                    out.push_str(&format!("{}Arm({}{}):\n", arm_ind, pat_str, guard_str));
                    match &arm.body {
                        HirMatchArmBody::Expr(expr) => {
                            expr.pretty_print_to(out, indent + 2, ctx);
                        }
                        HirMatchArmBody::Block(block) => {
                            ctx.pool.block(*block).pretty_print_to(out, indent + 2, ctx);
                        }
                    }
                }
            }
            HirStmtKind::Defer(block) => {
                out.push_str(&format!("{ind}Defer\n"));
                ctx.pool.block(*block).pretty_print_to(out, indent + 1, ctx);
            }
            HirStmtKind::ErrDefer(block) => {
                out.push_str(&format!("{ind}ErrDefer\n"));
                ctx.pool.block(*block).pretty_print_to(out, indent + 1, ctx);
            }
            HirStmtKind::Unsafe(block) => {
                out.push_str(&format!("{ind}Unsafe\n"));
                ctx.pool.block(*block).pretty_print_to(out, indent + 1, ctx);
            }
            HirStmtKind::Error => {
                out.push_str(&format!("{ind}<ErrorStmt>\n"));
            }
        }
    }
}

impl HirCondition {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        match self {
            HirCondition::Expr(expr) => {
                expr.pretty_print_to(out, indent, ctx);
            }
            HirCondition::Is { expr, pattern } => {
                out.push_str(&format!("{ind}Is\n"));
                expr.pretty_print_to(out, indent + 1, ctx);
                let pat_ind = "  ".repeat(indent + 1);
                let mut pat_str = String::new();
                pattern.pretty_print_to(&mut pat_str, 0, ctx);
                out.push_str(&format!("{pat_ind}Pattern: {pat_str}\n"));
            }
            HirCondition::And(conditions) => {
                out.push_str(&format!("{ind}And\n"));
                for condition in conditions {
                    condition.pretty_print_to(out, indent + 1, ctx);
                }
            }
        }
    }
}

impl HirForClause {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        match self {
            HirForClause::In {
                bindings, iterable, ..
            } => {
                let b_strs: Vec<String> = ctx
                    .pool
                    .for_bindings_list(*bindings)
                    .iter()
                    .map(|b| {
                        format!(
                            "{}: {}",
                            symbol_name(ctx.symbols, b.symbol),
                            display_type(b.ty, ctx)
                        )
                    })
                    .collect();
                out.push_str(&format!("{}In ({})\n", ind, b_strs.join(", ")));
                iterable.pretty_print_to(out, indent + 1, ctx);
            }
            HirForClause::CStyle {
                init,
                condition,
                step,
                ..
            } => {
                out.push_str(&format!("{ind}CStyle\n"));
                let sub_ind = "  ".repeat(indent + 1);
                if let Some(init_stmt) = init {
                    out.push_str(&format!("{sub_ind}Init\n"));
                    init_stmt.pretty_print_to(out, indent + 2, ctx);
                }
                if let Some(cond_expr) = condition {
                    out.push_str(&format!("{sub_ind}Condition\n"));
                    cond_expr.pretty_print_to(out, indent + 2, ctx);
                }
                if let Some(step_stmt) = step {
                    out.push_str(&format!("{sub_ind}Step\n"));
                    step_stmt.pretty_print_to(out, indent + 2, ctx);
                }
            }
        }
    }
}

impl HirSimpleStmt {
    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        match self {
            HirSimpleStmt::VarDecl { bindings, value } => {
                let bindings_slice = ctx.pool.bindings_list(*bindings);
                if bindings_slice.len() == 1 {
                    let b = &bindings_slice[0];
                    out.push_str(&format!(
                        "{}Var {}: {} =\n",
                        ind,
                        symbol_name(ctx.symbols, b.symbol),
                        display_type(b.ty, ctx)
                    ));
                } else {
                    let b_strs: Vec<String> = bindings_slice
                        .iter()
                        .map(|b| {
                            format!(
                                "{}: {}",
                                symbol_name(ctx.symbols, b.symbol),
                                display_type(b.ty, ctx)
                            )
                        })
                        .collect();
                    out.push_str(&format!("{}Var ({}) =\n", ind, b_strs.join(", ")));
                }
                value.pretty_print_to(out, indent + 1, ctx);
            }
            HirSimpleStmt::Set { places, op, value } => {
                let place_strs: Vec<String> = ctx
                    .pool
                    .places_list(*places)
                    .iter()
                    .map(|p| p.pretty_print(ctx))
                    .collect();
                out.push_str(&format!(
                    "{}Set ({}) {}\n",
                    ind,
                    place_strs.join(", "),
                    set_op_str(op)
                ));
                value.pretty_print_to(out, indent + 1, ctx);
            }
            HirSimpleStmt::Expr(expr) => {
                expr.pretty_print_to(out, indent, ctx);
            }
        }
    }
}

impl HirPlace {
    pub(super) fn pretty_print(&self, ctx: &HirPrettyCtx<'_>) -> String {
        let mut out = symbol_name(ctx.symbols, self.root_symbol).to_string();
        for suffix in &self.suffixes {
            match suffix {
                HirPlaceSuffix::Field { name, .. } => {
                    out.push_str(&format!(".{name}"));
                }
                HirPlaceSuffix::Index { expr, .. } => {
                    out.push_str(&format!("[{}]", expr.pretty_print_inline(ctx)));
                }
            }
        }
        out
    }
}

pub(super) fn set_op_str(op: &SetOp) -> &str {
    match op {
        SetOp::Assign => "=",
        SetOp::AddAssign => "+=",
        SetOp::SubAssign => "-=",
        SetOp::MulAssign => "*=",
        SetOp::DivAssign => "/=",
        SetOp::ModAssign => "%=",
        SetOp::BitAndAssign => "&=",
        SetOp::BitOrAssign => "|=",
        SetOp::BitXorAssign => "^=",
        SetOp::ShiftLeftAssign => "<<=",
        SetOp::ShiftRightAssign => ">>=",
    }
}
