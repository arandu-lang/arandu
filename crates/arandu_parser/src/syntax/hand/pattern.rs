//! Hand-lower patterns and match arms.

use super::cursor::{Cursor, HandCtx};
use super::expr::try_hand_lower_expr;
use super::stmt::parse_block_tokens;
use crate::ast::ast_pool::PatternId;
use crate::{FieldPattern, IndexRange, MatchArm, MatchArmBody, Pattern, TypeName};
use arandu_lexer::TokenKind;
use smallvec::smallvec;
use smol_str::SmolStr;

pub fn parse_pattern(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<PatternId> {
    // SYN.4: `p1 | p2 | …` has lower precedence than atomic patterns.
    let first = parse_pattern_atom(ctx, cur)?;
    if cur.peek_kind() != Some(TokenKind::Pipe) {
        return Some(first);
    }
    let start = ctx.pool.pattern(first).span().start;
    let mut alts = vec![first];
    while cur.eat(TokenKind::Pipe) {
        alts.push(parse_pattern_atom(ctx, cur)?);
    }
    let end = ctx.pool.pattern(*alts.last()?).span().end;
    let range = ctx.pool.alloc_pattern_list(&alts);
    Some(ctx.pool.alloc_pattern(Pattern::Or {
        span: ctx.span(start, end),
        alts: range,
    }))
}

fn parse_pattern_atom(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<PatternId> {
    let start_tok = cur.peek()?;
    let start = start_tok.start;

    // `_`
    if matches!(start_tok.kind, TokenKind::IdentValue) && ctx.text(start_tok)? == "_" {
        cur.bump();
        return Some(ctx.pool.alloc_pattern(Pattern::Wildcard {
            span: ctx.token_span(start_tok),
        }));
    }

    // tuple (a, b, ...)
    if cur.eat(TokenKind::LParen) {
        let mut items = vec![parse_pattern(ctx, cur)?];
        cur.expect(TokenKind::Comma)?;
        items.push(parse_pattern(ctx, cur)?);
        while cur.eat(TokenKind::Comma) {
            if cur.peek_kind() == Some(TokenKind::RParen) {
                break;
            }
            items.push(parse_pattern(ctx, cur)?);
        }
        let close = cur.expect(TokenKind::RParen)?;
        let range = ctx.pool.alloc_pattern_list(&items);
        return Some(ctx.pool.alloc_pattern(Pattern::Tuple {
            span: ctx.span(start, close.start + close.len),
            items: range,
        }));
    }

    // Type-led patterns: Enum / Struct / TypeTuple
    if matches!(start_tok.kind, TokenKind::IdentType) {
        let type_span = ctx.token_span(start_tok);
        let name = SmolStr::new(ctx.text(start_tok)?);
        cur.bump();
        if cur.eat(TokenKind::Dot) {
            let variant_tok = cur.peek().filter(|t| t.kind.is_contextual_member_name())?;
            let variant = SmolStr::new(ctx.text(variant_tok)?);
            cur.bump();
            let (payload, end) = if cur.eat(TokenKind::LParen) {
                let list = parse_pattern_list(ctx, cur, TokenKind::RParen)?;
                let close = cur.expect(TokenKind::RParen)?;
                (ctx.pool.alloc_pattern_list(&list), close.start + close.len)
            } else {
                (IndexRange::empty(), variant_tok.start + variant_tok.len)
            };
            return Some(ctx.pool.alloc_pattern(Pattern::Enum {
                span: ctx.span(start, end),
                type_name: TypeName {
                    span: type_span,
                    path: smallvec![name],
                },
                variant,
                payload,
            }));
        }
        if cur.eat(TokenKind::LBrace) {
            let mut fields = Vec::new();
            if cur.peek_kind() != Some(TokenKind::RBrace) {
                loop {
                    let fname_tok = cur.expect(TokenKind::IdentValue)?;
                    let fname = SmolStr::new(ctx.text(fname_tok)?);
                    let fstart = fname_tok.start;
                    let pattern = if cur.eat(TokenKind::Colon) {
                        Some(parse_pattern(ctx, cur)?)
                    } else {
                        None
                    };
                    let fend = pattern
                        .map(|p| ctx.pool.pattern(p).span().end)
                        .unwrap_or(fname_tok.start + fname_tok.len);
                    fields.push(ctx.pool.alloc_field_pattern(FieldPattern {
                        span: ctx.span(fstart, fend),
                        name: fname,
                        pattern,
                    }));
                    if !cur.eat(TokenKind::Comma) {
                        break;
                    }
                    if cur.peek_kind() == Some(TokenKind::RBrace) {
                        break;
                    }
                }
            }
            let close = cur.expect(TokenKind::RBrace)?;
            let range = ctx.pool.alloc_field_pattern_list(&fields);
            return Some(ctx.pool.alloc_pattern(Pattern::Struct {
                span: ctx.span(start, close.start + close.len),
                type_name: TypeName {
                    span: type_span,
                    path: smallvec![name],
                },
                fields: range,
            }));
        }
        if cur.eat(TokenKind::LParen) {
            let list = parse_pattern_list(ctx, cur, TokenKind::RParen)?;
            let close = cur.expect(TokenKind::RParen)?;
            let range = ctx.pool.alloc_pattern_list(&list);
            return Some(ctx.pool.alloc_pattern(Pattern::TypeTuple {
                span: ctx.span(start, close.start + close.len),
                name,
                payload: range,
            }));
        }
        return Some(ctx.pool.alloc_pattern(Pattern::TypeTuple {
            span: type_span,
            name,
            payload: IndexRange::empty(),
        }));
    }

    // Bind (`mut name` marks the binding mutable, not the matched value).
    let mutable = cur.eat(TokenKind::KwMut);
    if mutable || matches!(start_tok.kind, TokenKind::IdentValue) {
        let name_tok = if mutable { cur.peek()? } else { start_tok };
        if !matches!(name_tok.kind, TokenKind::IdentValue) {
            return None;
        }
        let name = SmolStr::new(ctx.text(name_tok)?);
        cur.bump();
        return Some(ctx.pool.alloc_pattern(Pattern::Bind {
            span: ctx.span(start, name_tok.start + name_tok.len),
            name,
            mutable,
        }));
    }

    // Literal / range
    let lit = parse_literal_expr(ctx, cur)?;
    if matches!(
        cur.peek_kind(),
        Some(TokenKind::RangeExclusive | TokenKind::RangeInclusive)
    ) {
        let inclusive = matches!(cur.peek_kind(), Some(TokenKind::RangeInclusive));
        cur.bump();
        let end = parse_literal_expr(ctx, cur)?;
        let span = ctx.span(start, ctx.pool.expr_span(end).end);
        return Some(ctx.pool.alloc_pattern(Pattern::Range {
            span,
            start: lit,
            inclusive,
            end,
        }));
    }
    Some(ctx.pool.alloc_pattern(Pattern::Literal {
        span: ctx.pool.expr_span(lit),
        expr: lit,
    }))
}

fn parse_pattern_list(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    end: TokenKind,
) -> Option<Vec<PatternId>> {
    let mut patterns = Vec::new();
    if cur.peek_kind() == Some(end) {
        return Some(patterns);
    }
    loop {
        patterns.push(parse_pattern(ctx, cur)?);
        if !cur.eat(TokenKind::Comma) {
            break;
        }
        if cur.peek_kind() == Some(end) {
            break;
        }
    }
    Some(patterns)
}

fn parse_literal_expr(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<crate::ast::ast_pool::ExprId> {
    match cur.peek_kind()? {
        TokenKind::IntDec
        | TokenKind::IntHex
        | TokenKind::IntBin
        | TokenKind::IntOct
        | TokenKind::Float
        | TokenKind::BoolTrue
        | TokenKind::BoolFalse
        | TokenKind::Char
        | TokenKind::StringStart
        | TokenKind::Nil => try_hand_lower_expr(ctx, cur, 100),
        _ => None,
    }
}

/// `pattern [if guard] => body`
pub fn parse_match_arm(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<MatchArm> {
    let start = cur.peek()?.start;
    let pattern = parse_pattern(ctx, cur)?;
    let guard = if cur.eat(TokenKind::KwIf) {
        Some(try_hand_lower_expr(ctx, cur, 0)?)
    } else {
        None
    };
    cur.expect(TokenKind::FatArrow)?;
    let body = if cur.peek_kind() == Some(TokenKind::LBrace) {
        let block = parse_block_tokens(ctx, cur)?;
        MatchArmBody::Block {
            span: block.span,
            block,
        }
    } else {
        let expr = try_hand_lower_expr(ctx, cur, 0)?;
        let _ = cur.eat(TokenKind::Semicolon);
        MatchArmBody::Expr {
            span: ctx.pool.expr_span(expr),
            expr,
        }
    };
    let end = match &body {
        MatchArmBody::Block { span, .. } | MatchArmBody::Expr { span, .. } => span.end,
    };
    Some(MatchArm {
        span: ctx.span(start, end),
        pattern,
        guard,
        body,
    })
}
