//! Hand-lower expressions (Pratt + postfix + primary forms).

mod control;
mod postfix;
mod primary;
mod struct_lit;

use super::cursor::{Cursor, HandCtx, bin_bp};
use super::stmt::parse_block_tokens;
use super::ty::parse_type;
use crate::CatchHandler;
use crate::ast::ast_pool::{ExprId, ExprKind};
use arandu_lexer::{Token, TokenKind};
use postfix::parse_unary_primary_post;
use smol_str::SmolStr;

/// Parse expression from token slice; must consume all tokens.
pub fn try_hand_lower_expr_all(
    pool: &mut crate::ast::ast_pool::AstPool,
    source: &str,
    toks: &[&Token],
    file_id: u32,
) -> Option<ExprId> {
    if toks.is_empty() {
        return None;
    }
    let mut ctx = HandCtx::new(pool, source, file_id);
    let mut cur = Cursor::new(toks);
    let expr = try_hand_lower_expr(&mut ctx, &mut cur, 0)?;
    cur.skip_semis();
    if !cur.at_end() {
        return None;
    }
    Some(expr)
}

const MAX_EXPR_DEPTH: u32 = 105;

/// Parse expression with minimum binding power.
pub fn try_hand_lower_expr(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    min_bp: u8,
) -> Option<ExprId> {
    if ctx.depth >= MAX_EXPR_DEPTH {
        return None;
    }
    ctx.depth += 1;
    let res = try_hand_lower_expr_inner(ctx, cur, min_bp);
    ctx.depth -= 1;
    res
}

fn try_hand_lower_expr_inner(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    min_bp: u8,
) -> Option<ExprId> {
    let mut left = parse_unary_primary_post(ctx, cur)?;

    loop {
        // catch / as as postfix-infix
        match cur.peek_kind() {
            Some(TokenKind::KwCatch) if min_bp <= 10 => {
                let span_start = ctx.pool.expr_span(left).start;
                cur.bump(); // catch
                let handler = if cur.peek_kind() == Some(TokenKind::Pipe) {
                    let pipe = cur.bump()?;
                    let err_tok = cur.expect(TokenKind::IdentValue)?;
                    let error = SmolStr::new(ctx.text(err_tok)?);
                    cur.expect(TokenKind::Pipe)?;
                    let block = parse_block_tokens(ctx, cur)?;
                    let catch_handler = CatchHandler::Block {
                        span: ctx.span(pipe.start, block.span.end),
                        error,
                        block,
                    };
                    ctx.pool.alloc_catch_handler(catch_handler)
                } else {
                    let expr = try_hand_lower_expr(ctx, cur, 10)?;
                    let catch_handler = CatchHandler::Expr {
                        span: ctx.pool.expr_span(expr),
                        expr,
                    };
                    ctx.pool.alloc_catch_handler(catch_handler)
                };
                let end = match ctx.pool.catch_handler(handler) {
                    CatchHandler::Block { span, .. } | CatchHandler::Expr { span, .. } => span.end,
                };
                left = ctx.pool.alloc_expr(
                    ExprKind::Catch {
                        expr: left,
                        handler,
                    },
                    ctx.span(span_start, end),
                );
                continue;
            }
            Some(TokenKind::KwAs) if min_bp <= 140 => {
                // cast bp 140 matches RD
                let span_start = ctx.pool.expr_span(left).start;
                cur.bump();
                let ty = parse_type(ctx, cur)?;
                let end = ctx.pool.type_expr_span(ty).end;
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::Cast { expr: left, ty }, ctx.span(span_start, end));
                continue;
            }
            _ => {}
        }

        let Some(kind) = cur.peek_kind() else {
            break;
        };
        let Some((l_bp, r_bp, op)) = bin_bp(kind) else {
            break;
        };
        if l_bp < min_bp {
            break;
        }
        // chained ranges need parens
        if matches!(
            op,
            crate::BinaryOp::RangeExclusive | crate::BinaryOp::RangeInclusive
        ) && matches!(
            ctx.pool.expr(left),
            ExprKind::Binary {
                op: crate::BinaryOp::RangeExclusive | crate::BinaryOp::RangeInclusive,
                ..
            }
        ) {
            return None;
        }
        cur.bump();

        let right = try_hand_lower_expr(ctx, cur, r_bp)?;
        let left_span = ctx.pool.expr_span(left);
        let right_span = ctx.pool.expr_span(right);
        let span = ctx.span(left_span.start, right_span.end);
        left = if op == crate::BinaryOp::NullCoalesce {
            ctx.pool
                .alloc_expr(ExprKind::NullCoalesce { left, right }, span)
        } else {
            ctx.pool
                .alloc_expr(ExprKind::Binary { op, left, right }, span)
        };
    }

    Some(left)
}
