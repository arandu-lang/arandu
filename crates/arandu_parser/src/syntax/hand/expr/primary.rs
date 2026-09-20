//! Primary expression parsing (literals, paths, collections, blocks) in hand-lower.

use super::super::cursor::{Cursor, HandCtx};
use super::super::stmt::parse_block_tokens;
use super::super::ty::primitive_type_token_name;
use super::control::{looks_like_lambda, parse_if_expr, parse_lambda, parse_match_expr};
use super::struct_lit::parse_type_led;
use super::try_hand_lower_expr;
use crate::StringPart;
use crate::ast::ast_pool::{ExprId, ExprKind};
use arandu_lexer::TokenKind;
use smallvec::smallvec;
use smol_str::SmolStr;

pub(super) fn parse_primary(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<ExprId> {
    let t = cur.peek()?;
    let start = t.start;
    match t.kind {
        TokenKind::IntDec | TokenKind::IntHex | TokenKind::IntBin | TokenKind::IntOct => {
            let value = SmolStr::new(ctx.text(t)?);
            cur.bump();
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Int { value }, ctx.token_span(t)),
            )
        }
        TokenKind::Float => {
            let value = SmolStr::new(ctx.text(t)?);
            cur.bump();
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Float { value }, ctx.token_span(t)),
            )
        }
        TokenKind::BoolTrue | TokenKind::BoolFalse => {
            cur.bump();
            Some(ctx.pool.alloc_expr(
                ExprKind::Bool {
                    value: matches!(t.kind, TokenKind::BoolTrue),
                },
                ctx.token_span(t),
            ))
        }
        TokenKind::Char => {
            let value = SmolStr::new(t.char_content(ctx.source));
            cur.bump();
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::Char { value }, ctx.token_span(t)),
            )
        }
        TokenKind::Nil => {
            cur.bump();
            Some(ctx.pool.alloc_expr(ExprKind::Nil, ctx.token_span(t)))
        }
        TokenKind::KwSelf => {
            cur.bump();
            Some(ctx.pool.alloc_expr(
                ExprKind::Path {
                    path: smallvec![SmolStr::new_static("self")],
                },
                ctx.token_span(t),
            ))
        }
        TokenKind::IdentValue => {
            // `mod.Type.method` pattern: identical check to RD parse_prefix
            if cur.peek_at(1).is_some_and(|t| t.kind == TokenKind::Dot)
                && cur
                    .peek_at(2)
                    .is_some_and(|t| t.kind == TokenKind::IdentType)
            {
                return parse_type_led(ctx, cur, start);
            }
            let text = ctx.text(t)?;
            cur.bump();
            Some(ctx.pool.alloc_expr(
                ExprKind::Path {
                    path: smallvec![SmolStr::new(text)],
                },
                ctx.token_span(t),
            ))
        }
        // T2.2: `.Ok(val)` / `.None` — leading Dot + type/value ident (+ optional call args).
        TokenKind::Dot => {
            cur.bump();
            let name_tok = cur.peek()?;
            if !matches!(name_tok.kind, TokenKind::IdentValue | TokenKind::IdentType) {
                return None;
            }
            let name = SmolStr::new(ctx.text(name_tok)?);
            cur.bump();
            let mut end = name_tok.start + name_tok.len;
            let args = if cur.eat(TokenKind::LParen) {
                let mut arg_ids = Vec::new();
                if cur.peek_kind() != Some(TokenKind::RParen) {
                    loop {
                        arg_ids.push(try_hand_lower_expr(ctx, cur, 0)?);
                        if cur.eat(TokenKind::Comma) {
                            continue;
                        }
                        break;
                    }
                }
                let close = cur.expect(TokenKind::RParen)?;
                end = close.start + close.len;
                ctx.pool.alloc_expr_list(&arg_ids)
            } else {
                ctx.pool.alloc_expr_list(&[])
            };
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::VariantSugar { name, args }, ctx.span(start, end)),
            )
        }
        TokenKind::IdentType if !ident_type_starts_type_led_expr(cur) => {
            let text = ctx.text(t)?;
            cur.bump();
            Some(ctx.pool.alloc_expr(
                ExprKind::Path {
                    path: smallvec![SmolStr::new(text)],
                },
                ctx.token_span(t),
            ))
        }
        TokenKind::IdentType => parse_type_led(ctx, cur, start),
        // type token as type-led (int is TypeInt etc.)
        k if primitive_type_token_name(k).is_some() => parse_type_led(ctx, cur, start),
        TokenKind::LParen => {
            cur.bump();
            if looks_like_lambda(cur) {
                return parse_lambda(ctx, cur, start);
            }
            let inner = try_hand_lower_expr(ctx, cur, 0)?;
            let close = cur.expect(TokenKind::RParen)?;
            Some(ctx.pool.alloc_expr(
                ExprKind::Group { expr: inner },
                ctx.span(start, close.start + close.len),
            ))
        }
        TokenKind::StringStart => parse_string(ctx, cur, start, TokenKind::StringEnd),
        TokenKind::MultilineStringStart => {
            parse_string(ctx, cur, start, TokenKind::MultilineStringEnd)
        }
        TokenKind::RawString => {
            let value = SmolStr::new(t.raw_string_content(ctx.source));
            cur.bump();
            let span = ctx.token_span(t);
            let part = StringPart::Text { span, text: value };
            let part_id = ctx.pool.alloc_string_part(part);
            let range = ctx.pool.alloc_string_part_list(&[part_id]);
            Some(
                ctx.pool
                    .alloc_expr(ExprKind::InterpolatedString { parts: range }, span),
            )
        }
        TokenKind::LBracket => {
            cur.bump();
            let mut items = Vec::new();
            if cur.peek_kind() != Some(TokenKind::RBracket) {
                loop {
                    items.push(try_hand_lower_expr(ctx, cur, 0)?);
                    if cur.eat(TokenKind::Comma) {
                        continue;
                    }
                    break;
                }
            }
            let close = cur.expect(TokenKind::RBracket)?;
            let range = ctx.pool.alloc_expr_list(&items);
            Some(ctx.pool.alloc_expr(
                ExprKind::Array { items: range },
                ctx.span(start, close.start + close.len),
            ))
        }
        TokenKind::KwIf => parse_if_expr(ctx, cur, start),
        TokenKind::KwMatch => parse_match_expr(ctx, cur, start),
        TokenKind::KwAsync => {
            cur.bump();
            let block = parse_block_tokens(ctx, cur)?;
            let end = block.span.end;
            let block_id = ctx.pool.alloc_block(block);
            Some(ctx.pool.alloc_expr(
                ExprKind::AsyncBlock { block: block_id },
                ctx.span(start, end),
            ))
        }
        TokenKind::KwUnsafe => {
            cur.bump();
            let block = parse_block_tokens(ctx, cur)?;
            let end = block.span.end;
            let block_id = ctx.pool.alloc_block(block);
            Some(ctx.pool.alloc_expr(
                ExprKind::UnsafeBlock { block: block_id },
                ctx.span(start, end),
            ))
        }
        _ => None,
    }
}

fn ident_type_starts_type_led_expr(cur: &Cursor<'_>) -> bool {
    match cur.peek_at(1).map(|token| token.kind) {
        Some(TokenKind::Dot) => true,
        Some(TokenKind::LBrace) => match cur.peek_at(2).map(|token| token.kind) {
            Some(TokenKind::RBrace) => true,
            Some(TokenKind::IdentValue | TokenKind::IdentType) => {
                cur.peek_at(3).is_some_and(|token| {
                    matches!(
                        token.kind,
                        TokenKind::Colon | TokenKind::Comma | TokenKind::RBrace
                    )
                })
            }
            _ => false,
        },
        Some(TokenKind::Lt) => {
            let mut depth = 0_u32;
            let mut offset = 1;
            while let Some(token) = cur.peek_at(offset) {
                match token.kind {
                    TokenKind::Lt => depth += 1,
                    TokenKind::Gt => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return cur.peek_at(offset + 1).is_some_and(|next| {
                                matches!(next.kind, TokenKind::LBrace | TokenKind::Dot)
                            });
                        }
                    }
                    TokenKind::ShiftRight => {
                        depth = depth.saturating_sub(2);
                        if depth == 0 {
                            return cur.peek_at(offset + 1).is_some_and(|next| {
                                matches!(next.kind, TokenKind::LBrace | TokenKind::Dot)
                            });
                        }
                    }
                    _ => {}
                }
                offset += 1;
            }
            false
        }
        _ => false,
    }
}

pub(super) fn parse_string(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
    end_kind: TokenKind,
) -> Option<ExprId> {
    cur.bump(); // start
    let mut parts = Vec::new();
    loop {
        let t = cur.peek()?;
        match t.kind {
            k if k == end_kind => {
                let end_tok = cur.bump()?;
                let range = ctx.pool.alloc_string_part_list(&parts);
                return Some(ctx.pool.alloc_expr(
                    ExprKind::InterpolatedString { parts: range },
                    ctx.span(start, end_tok.start + end_tok.len),
                ));
            }
            TokenKind::StringText | TokenKind::StringEscape => {
                let text = SmolStr::new(ctx.text(t)?);
                let span = ctx.token_span(t);
                cur.bump();
                parts.push(ctx.pool.alloc_string_part(StringPart::Text { span, text }));
            }
            TokenKind::InterpStart => {
                let interp_start = cur.bump()?;
                let expr = try_hand_lower_expr(ctx, cur, 0)?;
                let close = cur.expect(TokenKind::InterpEnd)?;
                let span = ctx.span(interp_start.start, close.start + close.len);
                parts.push(ctx.pool.alloc_string_part(StringPart::Expr { span, expr }));
            }
            _ => {
                let text = SmolStr::new(ctx.text(t).unwrap_or(""));
                let span = ctx.token_span(t);
                cur.bump();
                parts.push(ctx.pool.alloc_string_part(StringPart::Text { span, text }));
            }
        }
    }
}
