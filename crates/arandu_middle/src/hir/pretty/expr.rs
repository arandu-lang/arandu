//! Expression pretty printing implementation.

use super::types::{HirPrettyCtx, display_type};
use crate::hir::{
    BinaryOp, HirCatchHandler, HirExpr, HirExprKind, HirLambdaBody, HirMatchArmBody, UnaryOp,
    symbol_name,
};

impl HirExpr {
    pub(super) fn pretty_print_inline(&self, ctx: &HirPrettyCtx<'_>) -> String {
        match &self.kind {
            HirExprKind::Int(v) => v.to_string(),
            HirExprKind::Float(v) => v.to_string(),
            HirExprKind::FloatBits(v) => v.display(),
            HirExprKind::FrozenBytes(v) => format!("bytes({:?})", v.as_bytes()),
            HirExprKind::Bool(v) => v.to_string(),
            HirExprKind::Char(v) => format!("'{v}'"),
            HirExprKind::Str(v) => format!("\"{v}\""),
            HirExprKind::StringInterp { .. } => "<StringInterp>".to_string(),
            HirExprKind::ToStr { value } => {
                format!("{}.to_str()", value.pretty_print_inline(ctx))
            }
            HirExprKind::Nil => "nil".to_string(),
            HirExprKind::Error => "<ErrorExpr>".to_string(),
            HirExprKind::Path { symbol } => ctx.symbols.get(*symbol).name.to_string(),
            HirExprKind::Binary { op, left, right } => {
                format!(
                    "{} {} {}",
                    left.pretty_print_inline(ctx),
                    op_str(op),
                    right.pretty_print_inline(ctx)
                )
            }
            HirExprKind::Unary { op, expr } => {
                format!("{}{}", unary_op_str(op), expr.pretty_print_inline(ctx))
            }
            HirExprKind::Index { base, index } => {
                format!(
                    "{}[{}]",
                    base.pretty_print_inline(ctx),
                    index.pretty_print_inline(ctx)
                )
            }
            HirExprKind::Field { base, field } => {
                format!("{}.{}", base.pretty_print_inline(ctx), field)
            }
            _ => "<expr>".to_string(),
        }
    }

    pub(super) fn pretty_print_to(&self, out: &mut String, indent: usize, ctx: &HirPrettyCtx<'_>) {
        let ind = "  ".repeat(indent);
        match &self.kind {
            HirExprKind::Path { symbol } => {
                let kind = ctx.symbols.get(*symbol).kind;
                let name = &ctx.symbols.get(*symbol).name;
                let prefix = if kind == crate::SymbolKind::Local || kind == crate::SymbolKind::Param
                {
                    "LocalRef"
                } else {
                    "Path"
                };
                out.push_str(&format!(
                    "{}{}({}): {}\n",
                    ind,
                    prefix,
                    name,
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::TypePath { member_symbol, .. } => {
                out.push_str(&format!(
                    "{}TypePath({}): {}\n",
                    ind,
                    symbol_name(ctx.symbols, *member_symbol),
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::Generic { callee, args } => {
                let args_strs: Vec<String> = args.iter().map(|a| display_type(*a, ctx)).collect();
                out.push_str(&format!(
                    "{}Generic<{}>: {}\n",
                    ind,
                    args_strs.join(", "),
                    display_type(self.ty, ctx)
                ));
                callee.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::Field { base, field } => {
                out.push_str(&format!(
                    "{}Field({}): {}\n",
                    ind,
                    field,
                    display_type(self.ty, ctx)
                ));
                base.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::SafeField { base, field } => {
                out.push_str(&format!(
                    "{}SafeField({}): {}\n",
                    ind,
                    field,
                    display_type(self.ty, ctx)
                ));
                base.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::Index { base, index } => {
                out.push_str(&format!("{}Index: {}\n", ind, display_type(self.ty, ctx)));
                base.pretty_print_to(out, indent + 1, ctx);
                index.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::SafeIndex { base, index } => {
                out.push_str(&format!(
                    "{}SafeIndex: {}\n",
                    ind,
                    display_type(self.ty, ctx)
                ));
                base.pretty_print_to(out, indent + 1, ctx);
                index.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::Try { expr } => {
                out.push_str(&format!("{}Try: {}\n", ind, display_type(self.ty, ctx)));
                expr.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::Call {
                callee,
                args,
                trailing_block,
            } => {
                out.push_str(&format!("{}Call: {}\n", ind, display_type(self.ty, ctx)));
                callee.pretty_print_to(out, indent + 1, ctx);
                for &a in ctx.pool.expr_list(*args) {
                    a.pretty_print_to(out, indent + 1, ctx);
                }
                if let Some(block) = trailing_block {
                    let sub_ind = "  ".repeat(indent + 1);
                    out.push_str(&format!("{sub_ind}TrailingBlock\n"));
                    ctx.pool.block(*block).pretty_print_to(out, indent + 2, ctx);
                }
            }
            HirExprKind::ResultCtor { variant, value } => {
                let name = match variant {
                    crate::hir::ResultCtorVariant::Ok => "Result.Ok",
                    crate::hir::ResultCtorVariant::Err => "Result.Err",
                    crate::hir::ResultCtorVariant::Some => "Option.Some",
                    crate::hir::ResultCtorVariant::None => "Option.None",
                    crate::hir::ResultCtorVariant::PollReady => "Poll.Ready",
                    crate::hir::ResultCtorVariant::PollPending => "Poll.Pending",
                };
                out.push_str(&format!(
                    "{}{}: {}\n",
                    ind,
                    name,
                    display_type(self.ty, ctx)
                ));
                value.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let name = &ctx.symbols.get(*struct_symbol).name;
                out.push_str(&format!(
                    "{}StructLiteral({}): {}\n",
                    ind,
                    name,
                    display_type(self.ty, ctx)
                ));
                let field_ind = "  ".repeat(indent + 1);
                for f in ctx.pool.field_inits_list(*fields) {
                    out.push_str(&format!("{}{}:\n", field_ind, f.name));
                    f.value.pretty_print_to(out, indent + 2, ctx);
                }
            }
            HirExprKind::Array { items } => {
                out.push_str(&format!("{}Array: {}\n", ind, display_type(self.ty, ctx)));
                for &item in ctx.pool.expr_list(*items) {
                    item.pretty_print_to(out, indent + 1, ctx);
                }
            }
            HirExprKind::ArrayRepeat { value } => {
                out.push_str(&format!(
                    "{}ArrayRepeat: {}\n",
                    ind,
                    display_type(self.ty, ctx)
                ));
                value.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::Tuple { items } => {
                out.push_str(&format!("{}Tuple: {}\n", ind, display_type(self.ty, ctx)));
                for &item in ctx.pool.expr_list(*items) {
                    item.pretty_print_to(out, indent + 1, ctx);
                }
            }
            HirExprKind::Lambda { params, body } => {
                let params_str: Vec<String> = ctx
                    .pool
                    .lambda_params_list(*params)
                    .iter()
                    .map(|p| {
                        format!(
                            "{}: {}",
                            symbol_name(ctx.symbols, p.symbol),
                            display_type(p.ty, ctx)
                        )
                    })
                    .collect();
                out.push_str(&format!(
                    "{}Lambda({}): {}\n",
                    ind,
                    params_str.join(", "),
                    display_type(self.ty, ctx)
                ));
                match body {
                    HirLambdaBody::Expr(expr) => {
                        expr.pretty_print_to(out, indent + 1, ctx);
                    }
                    HirLambdaBody::Block(block) => {
                        ctx.pool.block(*block).pretty_print_to(out, indent + 1, ctx);
                    }
                }
            }
            HirExprKind::Alloc { expr } => {
                out.push_str(&format!("{}Alloc: {}\n", ind, display_type(self.ty, ctx)));
                expr.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::AsyncBlock { block } => {
                out.push_str(&format!(
                    "{}AsyncBlock: {}\n",
                    ind,
                    display_type(self.ty, ctx)
                ));
                ctx.pool.block(*block).pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::ValueBlock { block } | HirExprKind::UnsafeBlock { block } => {
                out.push_str(&format!(
                    "{}UnsafeBlock: {}\n",
                    ind,
                    display_type(self.ty, ctx)
                ));
                ctx.pool.block(*block).pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                out.push_str(&format!("{}If: {}\n", ind, display_type(self.ty, ctx)));
                condition.pretty_print_to(out, indent + 1, ctx);
                let sub_ind = "  ".repeat(indent + 1);
                out.push_str(&format!("{sub_ind}Then\n"));
                ctx.pool
                    .block(*then_block)
                    .pretty_print_to(out, indent + 2, ctx);
                out.push_str(&format!("{sub_ind}Else\n"));
                ctx.pool
                    .block(*else_block)
                    .pretty_print_to(out, indent + 2, ctx);
            }
            HirExprKind::Match { value, arms } => {
                out.push_str(&format!("{}Match: {}\n", ind, display_type(self.ty, ctx)));
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
            HirExprKind::Catch { expr, handler } => {
                out.push_str(&format!("{}Catch: {}\n", ind, display_type(self.ty, ctx)));
                expr.pretty_print_to(out, indent + 1, ctx);
                let sub_ind = "  ".repeat(indent + 1);
                match handler {
                    HirCatchHandler::Expr(h_expr) => {
                        out.push_str(&format!("{sub_ind}Handler\n"));
                        h_expr.pretty_print_to(out, indent + 2, ctx);
                    }
                    HirCatchHandler::Block {
                        error_name, block, ..
                    } => {
                        let err_str = error_name.as_deref().unwrap_or("error");
                        out.push_str(&format!("{sub_ind}Handler({err_str})\n"));
                        ctx.pool.block(*block).pretty_print_to(out, indent + 2, ctx);
                    }
                }
            }
            HirExprKind::NullCoalesce { left, right } => {
                out.push_str(&format!(
                    "{}NullCoalesce: {}\n",
                    ind,
                    display_type(self.ty, ctx)
                ));
                left.pretty_print_to(out, indent + 1, ctx);
                right.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::Layout { query, operand_ty } => {
                out.push_str(&format!(
                    "{}@{}({}): {}\n",
                    ind,
                    query.name(),
                    display_type(*operand_ty, ctx),
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::Cast { expr, target_ty } => {
                out.push_str(&format!(
                    "{}Cast({}): {}\n",
                    ind,
                    display_type(*target_ty, ctx),
                    display_type(self.ty, ctx)
                ));
                expr.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::Unary { op, expr } => {
                out.push_str(&format!(
                    "{}Unary({}): {}\n",
                    ind,
                    unary_op_str(op),
                    display_type(self.ty, ctx)
                ));
                expr.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::Binary { op, left, right } => {
                out.push_str(&format!(
                    "{}Binary({}): {}\n",
                    ind,
                    op_str(op),
                    display_type(self.ty, ctx)
                ));
                left.pretty_print_to(out, indent + 1, ctx);
                right.pretty_print_to(out, indent + 1, ctx);
            }
            HirExprKind::Int(v) => {
                out.push_str(&format!(
                    "{}Int({}): {}\n",
                    ind,
                    v,
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::Float(v) => {
                out.push_str(&format!(
                    "{}Float({}): {}\n",
                    ind,
                    v,
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::FloatBits(v) => {
                out.push_str(&format!(
                    "{}Float({}): {}\n",
                    ind,
                    v.display(),
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::FrozenBytes(v) => {
                out.push_str(&format!(
                    "{}Bytes({:?}): {}\n",
                    ind,
                    v.as_bytes(),
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::Bool(v) => {
                out.push_str(&format!(
                    "{}Bool({}): {}\n",
                    ind,
                    v,
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::Char(v) => {
                out.push_str(&format!(
                    "{}Char({}): {}\n",
                    ind,
                    v,
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::Str(v) => {
                out.push_str(&format!(
                    "{}Str({}): {}\n",
                    ind,
                    v,
                    display_type(self.ty, ctx)
                ));
            }
            HirExprKind::StringInterp { parts } => {
                out.push_str(&format!(
                    "{}StringInterp: {}\n",
                    ind,
                    display_type(self.ty, ctx)
                ));
                for part in parts {
                    match part {
                        crate::hir::HirStringPart::Text(t) => {
                            out.push_str(&format!("{}  Text({:?})\n", ind, t));
                        }
                        crate::hir::HirStringPart::Expr(e) => {
                            ctx.pool.expr(*e).pretty_print_to(out, indent + 2, ctx);
                        }
                    }
                }
            }
            HirExprKind::ToStr { value } => {
                out.push_str(&format!("{}ToStr: {}\n", ind, display_type(self.ty, ctx)));
                ctx.pool.expr(*value).pretty_print_to(out, indent + 2, ctx);
            }
            HirExprKind::Nil => {
                out.push_str(&format!("{}Nil: {}\n", ind, display_type(self.ty, ctx)));
            }
            HirExprKind::Error => {
                out.push_str(&format!("{}Error: {}\n", ind, display_type(self.ty, ctx)));
            }
        }
    }
}

pub(super) fn op_str(op: &BinaryOp) -> &str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Equal => "==",
        BinaryOp::NotEqual => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::LtEqual => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEqual => ">=",
        BinaryOp::And => "and",
        BinaryOp::Or => "or",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::ShiftLeft => "<<",
        BinaryOp::ShiftRight => ">>",
        BinaryOp::NullCoalesce => "??",
        BinaryOp::RangeExclusive => "..",
        BinaryOp::RangeInclusive => "..=",
    }
}

pub(super) fn unary_op_str(op: &UnaryOp) -> &str {
    match op {
        UnaryOp::Not => "not ",
        UnaryOp::Neg => "-",
        UnaryOp::BitNot => "~",
        UnaryOp::Await => "await ",
        UnaryOp::Ref => "ref ",
        UnaryOp::RefMut => "mut ref ",
        UnaryOp::Deref => "*",
    }
}
