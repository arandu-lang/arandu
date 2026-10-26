//! Parsing control flow expressions (if, match, lambda) in hand-lower.

use super::super::cursor::{Cursor, HandCtx};
use super::super::pattern::parse_match_arm;
use super::super::stmt::parse_block_tokens;
use super::super::ty::{can_start_type_kind, parse_type};
use super::try_hand_lower_expr;
use crate::ast::ast_pool::{ExprId, ExprKind};
use crate::{Condition, LambdaBody, LambdaParam};
use arandu_lexer::TokenKind;
use smol_str::SmolStr;

pub(super) fn parse_if_expr(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
) -> Option<ExprId> {
    cur.expect(TokenKind::KwIf)?;
    // Condition tokens until depth-0 `{` (avoid trailing-block absorption).
    let toks = cur.remaining();
    let mut depth = 0i32;
    let mut brace_at = None;
    for (i, t) in toks.iter().enumerate() {
        if depth == 0 && matches!(t.kind, TokenKind::LBrace) {
            brace_at = Some(i);
            break;
        }
        match t.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    let brace_at = brace_at?;
    if brace_at == 0 {
        return None;
    }
    let mut ccur = Cursor::new(&toks[..brace_at]);
    let cond_expr = try_hand_lower_expr(ctx, &mut ccur, 0)?;
    if !ccur.at_end() {
        return None;
    }
    for _ in 0..brace_at {
        cur.bump();
    }
    let condition = Condition::Expr {
        span: ctx.pool.expr_span(cond_expr),
        expr: cond_expr,
    };
    let then_block = parse_block_tokens(ctx, cur)?;
    let else_block = if cur.eat(TokenKind::KwElse) {
        if cur.peek_kind() == Some(TokenKind::KwIf) {
            let nested_start = cur.peek()?.start;
            let nested = parse_if_expr(ctx, cur, nested_start)?;
            let nested_id = ctx.pool.alloc_stmt(crate::Stmt::Expr {
                span: ctx.pool.expr_span(nested),
                expr: nested,
                has_semi: false,
            });
            crate::Block {
                span: ctx.pool.expr_span(nested),
                statements: ctx.pool.alloc_stmt_list(&[nested_id]),
            }
        } else {
            parse_block_tokens(ctx, cur)?
        }
    } else {
        return None;
    };
    let then_id = ctx.pool.alloc_block(then_block);
    let else_id = ctx.pool.alloc_block(else_block);
    let end = ctx.pool.block(else_id).span.end;
    Some(ctx.pool.alloc_expr(
        ExprKind::If {
            condition,
            then_block: then_id,
            else_block: else_id,
        },
        ctx.span(start, end),
    ))
}

pub(super) fn parse_match_expr(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
) -> Option<ExprId> {
    cur.expect(TokenKind::KwMatch)?;
    // Value stops before `{` (no trailing-block call).
    let toks = cur.remaining();
    let mut depth = 0i32;
    let mut brace_at = None;
    for (i, t) in toks.iter().enumerate() {
        if depth == 0 && matches!(t.kind, TokenKind::LBrace) {
            brace_at = Some(i);
            break;
        }
        match t.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    let brace_at = brace_at?;
    if brace_at == 0 {
        return None;
    }
    let mut vcur = Cursor::new(&toks[..brace_at]);
    let value = try_hand_lower_expr(ctx, &mut vcur, 0)?;
    if !vcur.at_end() {
        return None;
    }
    for _ in 0..brace_at {
        cur.bump();
    }
    cur.expect(TokenKind::LBrace)?;
    let mut arms = Vec::new();
    while cur.peek_kind() != Some(TokenKind::RBrace) && !cur.at_end() {
        cur.skip_semis();
        if cur.peek_kind() == Some(TokenKind::RBrace) {
            break;
        }
        let arm = parse_match_arm(ctx, cur)?;
        arms.push(ctx.pool.alloc_match_arm(arm));
    }
    let close = cur.expect(TokenKind::RBrace)?;
    let range = ctx.pool.alloc_match_arm_list(&arms);
    Some(ctx.pool.alloc_expr(
        ExprKind::Match { value, arms: range },
        ctx.span(start, close.start + close.len),
    ))
}

pub(super) fn looks_like_lambda(cur: &Cursor<'_>) -> bool {
    let mut depth = 1i32;
    let mut i = 0usize;
    while let Some(t) = cur.peek_at(i) {
        match t.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    return cur
                        .peek_at(i + 1)
                        .is_some_and(|n| matches!(n.kind, TokenKind::FatArrow));
                }
            }
            TokenKind::Eof => return false,
            _ => {}
        }
        i += 1;
        if i > 64 {
            return false;
        }
    }
    false
}

pub(super) fn parse_lambda(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
) -> Option<ExprId> {
    let mut params = Vec::new();
    if cur.peek_kind() != Some(TokenKind::RParen) {
        loop {
            let name_tok = cur.expect(TokenKind::IdentValue)?;
            let name = SmolStr::new(ctx.text(name_tok)?);
            let p_start = name_tok.start;
            let ty = if cur.peek_kind().is_some_and(can_start_type_kind) {
                Some(parse_type(ctx, cur)?)
            } else {
                None
            };
            let p_end = ty
                .map(|id| ctx.pool.type_expr_span(id).end)
                .unwrap_or(name_tok.start + name_tok.len);
            params.push(LambdaParam {
                span: ctx.span(p_start, p_end),
                name,
                ty,
            });
            if cur.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
    }
    cur.expect(TokenKind::RParen)?;
    cur.expect(TokenKind::FatArrow)?;
    let body = if cur.peek_kind() == Some(TokenKind::LBrace) {
        let block = parse_block_tokens(ctx, cur)?;
        let end = block.span.end;
        let body = LambdaBody::Block {
            span: block.span,
            block,
        };
        let param_ids: Vec<_> = params
            .into_iter()
            .map(|p| ctx.pool.alloc_lambda_param(p))
            .collect();
        let params_range = ctx.pool.alloc_lambda_param_list(&param_ids);
        return Some(ctx.pool.alloc_expr(
            ExprKind::Lambda {
                params: params_range,
                body,
                is_move: false,
            },
            ctx.span(start, end),
        ));
    } else {
        let expr = try_hand_lower_expr(ctx, cur, 0)?;
        LambdaBody::Expr {
            span: ctx.pool.expr_span(expr),
            expr,
        }
    };
    let end = match &body {
        LambdaBody::Expr { span, .. } | LambdaBody::Block { span, .. } => span.end,
    };
    let param_ids: Vec<_> = params
        .into_iter()
        .map(|p| ctx.pool.alloc_lambda_param(p))
        .collect();
    let params_range = ctx.pool.alloc_lambda_param_list(&param_ids);
    Some(ctx.pool.alloc_expr(
        ExprKind::Lambda {
            params: params_range,
            body,
            is_move: false,
        },
        ctx.span(start, end),
    ))
}

pub(super) fn parse_pipe_lambda(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
    is_move: bool,
) -> Option<ExprId> {
    let mut params = Vec::new();
    if !cur.eat(TokenKind::LogicalOr) {
        cur.expect(TokenKind::Pipe)?;
        if cur.peek_kind() != Some(TokenKind::Pipe) {
            loop {
                let name_tok = cur.expect(TokenKind::IdentValue)?;
                let name = SmolStr::new(ctx.text(name_tok)?);
                let p_start = name_tok.start;
                let ty = if cur.eat(TokenKind::Colon)
                    || cur.peek_kind().is_some_and(can_start_type_kind)
                {
                    Some(parse_type(ctx, cur)?)
                } else {
                    None
                };
                let p_end = ty
                    .map(|id| ctx.pool.type_expr_span(id).end)
                    .unwrap_or(name_tok.start + name_tok.len);
                params.push(LambdaParam {
                    span: ctx.span(p_start, p_end),
                    name,
                    ty,
                });
                if cur.eat(TokenKind::Comma) {
                    continue;
                }
                break;
            }
        }
        cur.expect(TokenKind::Pipe)?;
    }
    let body = if cur.peek_kind() == Some(TokenKind::LBrace) {
        let block = parse_block_tokens(ctx, cur)?;
        LambdaBody::Block {
            span: block.span,
            block,
        }
    } else {
        let expr = try_hand_lower_expr(ctx, cur, 0)?;
        LambdaBody::Expr {
            span: ctx.pool.expr_span(expr),
            expr,
        }
    };
    let end = match &body {
        LambdaBody::Expr { span, .. } | LambdaBody::Block { span, .. } => span.end,
    };
    let param_ids: Vec<_> = params
        .into_iter()
        .map(|p| ctx.pool.alloc_lambda_param(p))
        .collect();
    let params_range = ctx.pool.alloc_lambda_param_list(&param_ids);
    Some(ctx.pool.alloc_expr(
        ExprKind::Lambda {
            params: params_range,
            body,
            is_move,
        },
        ctx.span(start, end),
    ))
}
