//! Postfix and unary expression parsing in hand-lower.

use super::super::cursor::{Cursor, HandCtx};
use super::super::stmt::parse_block_tokens;
use super::super::ty::parse_generic_type_args;
use super::primary::parse_primary;
use super::struct_lit::{looks_like_struct_lit_after_type_path, try_struct_lit_from_type_path};
use super::try_hand_lower_expr;
use crate::UnaryOp;
use crate::ast::ast_pool::{ExprId, ExprKind};
use arandu_lexer::TokenKind;
use smol_str::SmolStr;

pub(super) fn parse_unary_primary_post(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<ExprId> {
    if cur.peek_kind() == Some(TokenKind::LParen) {
        return parse_primary_post(ctx, cur);
    }
    parse_unary_dispatch(ctx, cur)
}

fn parse_unary_dispatch(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<ExprId> {
    let t = cur.peek()?;
    match t.kind {
        TokenKind::Minus | TokenKind::Bang | TokenKind::Tilde | TokenKind::KwAwait => {
            let op = match t.kind {
                TokenKind::Minus => UnaryOp::Neg,
                TokenKind::Bang => UnaryOp::Not,
                TokenKind::Tilde => UnaryOp::BitNot,
                TokenKind::KwAwait => UnaryOp::Await,
                _ => unreachable!(),
            };
            let start = t.start;
            cur.bump();
            // Prefix binds tighter than all binary ops and cast (RD uses 150; Cast is 140; Mul is 130).
            // min_bp=100 wrongly absorbed `*a + *b` as `*(a + *b)`.
            let expr = try_hand_lower_expr(ctx, cur, 150)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Unary { op, expr }, ctx.span(start, end)),
            )
        }
        // `&mut expr` / `&expr` — address-of (F2.0). Distinct from binary `a & b`.
        TokenKind::Amp => {
            let start = t.start;
            cur.bump();
            let is_mut = cur.eat(TokenKind::KwMut);
            let expr = try_hand_lower_expr(ctx, cur, 150)?;
            let end = ctx.pool.expr_span(expr).end;
            let op = if is_mut {
                UnaryOp::RefMut
            } else {
                UnaryOp::Ref
            };
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Unary { op, expr }, ctx.span(start, end)),
            )
        }
        // Canonical `ref expr` / `mut ref expr` safe borrows.
        TokenKind::KwRef => {
            let start = t.start;
            cur.bump();
            let expr = try_hand_lower_expr(ctx, cur, 150)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(ctx.pool.alloc_expr(
                ExprKind::Unary {
                    op: UnaryOp::Ref,
                    expr,
                },
                ctx.span(start, end),
            ))
        }
        TokenKind::KwMut if cur.peek_at(1).map(|token| token.kind) == Some(TokenKind::KwRef) => {
            let start = t.start;
            cur.bump();
            cur.bump();
            let expr = try_hand_lower_expr(ctx, cur, 150)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(ctx.pool.alloc_expr(
                ExprKind::Unary {
                    op: UnaryOp::RefMut,
                    expr,
                },
                ctx.span(start, end),
            ))
        }
        // `*expr` — deref (F2.0). Unary binds tighter than binary `*`/`+` (bp ≤ 130).
        TokenKind::Star => {
            let start = t.start;
            cur.bump();
            let expr = try_hand_lower_expr(ctx, cur, 150)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(ctx.pool.alloc_expr(
                ExprKind::Unary {
                    op: UnaryOp::Deref,
                    expr,
                },
                ctx.span(start, end),
            ))
        }
        TokenKind::KwAlloc => {
            let start = t.start;
            cur.bump();
            let expr = try_hand_lower_expr(ctx, cur, 100)?;
            let end = ctx.pool.expr_span(expr).end;
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Alloc { expr }, ctx.span(start, end)),
            )
        }
        _ => parse_primary_post(ctx, cur),
    }
}

pub(super) fn parse_primary_post(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<ExprId> {
    let left = parse_primary(ctx, cur)?;
    parse_postfix_tail(ctx, cur, left)
}

fn parse_postfix_tail(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    mut left: ExprId,
) -> Option<ExprId> {
    loop {
        match cur.peek_kind() {
            Some(TokenKind::LParen) => {
                cur.bump();
                let mut args = Vec::new();
                if cur.peek_kind() != Some(TokenKind::RParen) {
                    loop {
                        if matches!(cur.peek_kind(), Some(TokenKind::IdentValue))
                            && cur.peek_at(1).is_some_and(|t| t.kind == TokenKind::Colon)
                        {
                            let name_tok = cur.peek().copied()?;
                            let start_span = name_tok.start;
                            cur.bump();
                            let colon_tok = cur.bump()?;
                            if matches!(cur.peek_kind(), Some(TokenKind::Comma | TokenKind::RParen))
                            {
                                let name = SmolStr::new(ctx.text(&name_tok)?);
                                let span = ctx.span(start_span, colon_tok.start + colon_tok.len);
                                args.push(ctx.pool.alloc_expr(
                                    ExprKind::Path {
                                        path: smallvec::smallvec![name],
                                    },
                                    span,
                                ));
                                if cur.eat(TokenKind::Comma) {
                                    if cur.peek_kind() == Some(TokenKind::RParen) {
                                        break;
                                    }
                                    continue;
                                }
                                break;
                            }
                        }
                        args.push(try_hand_lower_expr(ctx, cur, 0)?);
                        if cur.eat(TokenKind::Comma) {
                            if cur.peek_kind() == Some(TokenKind::RParen) {
                                break;
                            }
                            continue;
                        }
                        break;
                    }
                }
                let close = cur.expect(TokenKind::RParen)?;
                let args_range = ctx.pool.alloc_expr_list(&args);
                let left_span = ctx.pool.expr_span(left);
                let mut end = close.start + close.len;
                // Trailing-block call `f(args) { ... }` only via allows_trailing_block.
                // Match scrutinees are sliced to exclude `{`, so this is mainly for
                // full-expression hand lower; keep consistent with RD allow_block_calls.
                let trailing = if cur.peek_kind() == Some(TokenKind::LBrace)
                    && allows_trailing_block(ctx, left)
                {
                    let block = parse_block_tokens(ctx, cur)?;
                    end = block.span.end;
                    Some(ctx.pool.alloc_block(block))
                } else {
                    None
                };
                left = ctx.pool.alloc_expr(
                    ExprKind::Call {
                        callee: left,
                        args: args_range,
                        trailing_block: trailing,
                    },
                    ctx.span(left_span.start, end),
                );
            }
            Some(TokenKind::LBrace) if allows_trailing_block(ctx, left) => {
                // Root fix: `lib.Type {}` / `lib.Type { x: 1 }` starts as Path+Field
                // (module is IdentValue). Empty `{}` successfully parses as a trailing
                // *block* call, so the type never resolves (silent Error). Prefer
                // struct-literal when the path ends in a type-like name and `{`
                // looks like field inits (empty or `name:`).
                if looks_like_struct_lit_after_type_path(ctx, cur, left)
                    && let Some(sl) = try_struct_lit_from_type_path(ctx, cur, left)
                {
                    left = sl;
                    continue;
                }
                // bare trailing block call: f { ... }
                let left_span = ctx.pool.expr_span(left);
                let block = parse_block_tokens(ctx, cur)?;
                let end = block.span.end;
                let block_id = ctx.pool.alloc_block(block);
                let empty = ctx.pool.alloc_expr_list(&[]);
                left = ctx.pool.alloc_expr(
                    ExprKind::Call {
                        callee: left,
                        args: empty,
                        trailing_block: Some(block_id),
                    },
                    ctx.span(left_span.start, end),
                );
            }
            Some(TokenKind::Lt) if looks_like_generic_args(cur) => {
                let (args, end) = parse_generic_type_args(ctx, cur)?;
                let left_span = ctx.pool.expr_span(left);
                left = ctx.pool.alloc_expr(
                    ExprKind::Generic { callee: left, args },
                    ctx.span(left_span.start, end),
                );
                // generic must be followed by call, trailing block, or `as` cast
                if cur.peek_kind() == Some(TokenKind::LParen) {
                    continue; // loop will handle call
                }
                if cur.peek_kind() == Some(TokenKind::LBrace) {
                    continue;
                }
                if cur.peek_kind() == Some(TokenKind::KwAs) {
                    continue;
                }
                return None;
            }
            Some(TokenKind::Dot) => {
                cur.bump();
                let field_tok = cur.peek()?;
                if !field_tok.kind.is_contextual_member_name() {
                    return None;
                }
                let field = SmolStr::new(ctx.text(field_tok)?);
                cur.bump();
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, field_tok.start + field_tok.len);
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::Field { base: left, field }, span);
            }
            Some(TokenKind::SafeDot) => {
                cur.bump();
                let field_tok = cur.peek().filter(|t| t.kind.is_contextual_member_name())?;
                cur.bump();
                let field = SmolStr::new(ctx.text(field_tok)?);
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, field_tok.start + field_tok.len);
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::SafeField { base: left, field }, span);
            }
            Some(TokenKind::LBracket) => {
                cur.bump();
                let index = try_hand_lower_expr(ctx, cur, 0)?;
                let close = cur.expect(TokenKind::RBracket)?;
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, close.start + close.len);
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::Index { base: left, index }, span);
            }
            Some(TokenKind::SafeIndexStart) => {
                cur.bump();
                let index = try_hand_lower_expr(ctx, cur, 0)?;
                let close = cur.expect(TokenKind::RBracket)?;
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, close.start + close.len);
                left = ctx
                    .pool
                    .alloc_expr(ExprKind::SafeIndex { base: left, index }, span);
            }
            Some(TokenKind::Question) => {
                let q = cur.bump()?;
                let left_span = ctx.pool.expr_span(left);
                let span = ctx.span(left_span.start, q.start + q.len);
                left = ctx.pool.alloc_expr(ExprKind::Try { expr: left }, span);
            }
            _ => break,
        }
    }

    Some(left)
}

fn allows_trailing_block(ctx: &HandCtx<'_>, left: ExprId) -> bool {
    matches!(
        ctx.pool.expr(left),
        ExprKind::Path { .. }
            | ExprKind::Field { .. }
            | ExprKind::Generic { .. }
            | ExprKind::TypePath { .. }
    )
}

fn looks_like_generic_args(cur: &Cursor<'_>) -> bool {
    // scan for matching `>` then `( ` or `{`
    let mut openers = smallvec::SmallVec::<[usize; 8]>::new();
    let mut delimiters = 0usize;
    let mut i = 0usize;
    while let Some(t) = cur.peek_at(i) {
        match t.kind {
            TokenKind::Lt => openers.push(delimiters),
            TokenKind::Gt => {
                if openers.last() == Some(&delimiters) {
                    openers.pop();
                }
                if openers.is_empty() {
                    return cur.peek_at(i + 1).is_some_and(|n| {
                        matches!(
                            n.kind,
                            TokenKind::LParen | TokenKind::LBrace | TokenKind::KwAs
                        )
                    });
                }
            }
            TokenKind::ShiftRight => {
                for _ in 0..2 {
                    if openers.last() == Some(&delimiters) {
                        openers.pop();
                    }
                }
                if openers.is_empty() {
                    return cur.peek_at(i + 1).is_some_and(|n| {
                        matches!(
                            n.kind,
                            TokenKind::LParen | TokenKind::LBrace | TokenKind::KwAs
                        )
                    });
                }
            }
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => delimiters += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                let Some(depth) = delimiters.checked_sub(1) else {
                    return false;
                };
                delimiters = depth;
                while openers.last().is_some_and(|depth| *depth > delimiters) {
                    openers.pop();
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
