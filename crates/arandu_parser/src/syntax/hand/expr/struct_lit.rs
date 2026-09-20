//! Parsing struct literals and type-led expressions in hand-lower.

use super::super::cursor::{Cursor, HandCtx};
use super::super::ty::parse_type;
use super::try_hand_lower_expr;
use crate::ast::ast_pool::{ExprId, ExprKind};
use crate::{FieldInit, TypeExpr, TypeName};
use arandu_lexer::TokenKind;
use smol_str::SmolStr;

/// Type-like path segment: starts with uppercase (Arandu IdentType convention).
pub(super) fn is_type_like_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase())
}

/// Collect `Path` / `a.b.Type` Field chains into type path segments.
pub(super) fn type_path_segments_from_expr(
    ctx: &HandCtx<'_>,
    expr: ExprId,
) -> Option<smallvec::SmallVec<[SmolStr; 3]>> {
    match ctx.pool.expr(expr) {
        ExprKind::Path { path } if path.len() == 1 && is_type_like_name(&path[0]) => {
            Some(path.clone())
        }
        ExprKind::Field { base, field } if is_type_like_name(field) => {
            let mut segs = type_path_segments_from_expr_module(ctx, *base)?;
            segs.push(field.clone());
            Some(segs)
        }
        _ => None,
    }
}

/// Module path prefix: `lib` or `a.b` (all value/module segments).
pub(super) fn type_path_segments_from_expr_module(
    ctx: &HandCtx<'_>,
    expr: ExprId,
) -> Option<smallvec::SmallVec<[SmolStr; 3]>> {
    match ctx.pool.expr(expr) {
        ExprKind::Path { path } => Some(path.clone()),
        ExprKind::Field { base, field } => {
            let mut segs = type_path_segments_from_expr_module(ctx, *base)?;
            segs.push(field.clone());
            Some(segs)
        }
        _ => None,
    }
}

/// After a type-shaped path, `{` starts a struct lit if empty or `ident:`.
pub(super) fn looks_like_struct_lit_after_type_path(
    ctx: &HandCtx<'_>,
    cur: &Cursor<'_>,
    left: ExprId,
) -> bool {
    if type_path_segments_from_expr(ctx, left).is_none() {
        return false;
    }
    // Peek inside `{` without consuming.
    if cur.peek_kind() != Some(TokenKind::LBrace) {
        return false;
    }
    match cur.peek_at(1).map(|t| t.kind) {
        Some(TokenKind::RBrace) => true, // `Type {}`
        Some(TokenKind::IdentValue | TokenKind::IdentType) => {
            // `Type { field: ... }`
            cur.peek_at(2).is_some_and(|t| t.kind == TokenKind::Colon)
        }
        _ => false,
    }
}

/// Parse `{ field: expr, ... }` after a type-shaped Path/Field into StructLiteral.
pub(super) fn try_struct_lit_from_type_path(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    left: ExprId,
) -> Option<ExprId> {
    let segs = type_path_segments_from_expr(ctx, left)?;
    let left_span = ctx.pool.expr_span(left);
    let name = TypeName {
        span: left_span,
        path: segs,
    };
    let empty_args = ctx.pool.alloc_type_expr_list(&[]);
    let ty = ctx.pool.alloc_type_expr(TypeExpr::Named {
        span: left_span,
        name,
        args: empty_args,
    });

    cur.expect(TokenKind::LBrace)?;
    let mut fields = Vec::new();
    if cur.peek_kind() != Some(TokenKind::RBrace) {
        loop {
            if cur.peek_kind() == Some(TokenKind::RangeExclusive) {
                let dot_tok = cur.expect(TokenKind::RangeExclusive)?;
                let value = try_hand_lower_expr(ctx, cur, 0)?;
                let fend = ctx.pool.expr_span(value).end;
                let init_id = ctx.pool.alloc_field_init(FieldInit {
                    span: ctx.span(dot_tok.start, fend),
                    name: SmolStr::new(".."),
                    value,
                });
                fields.push(init_id);
                if cur.eat(TokenKind::Comma) {
                    // optional trailing comma
                }
                break;
            }
            let name_tok = cur.peek()?;
            if !matches!(name_tok.kind, TokenKind::IdentValue | TokenKind::IdentType) {
                return None;
            }
            let fname = SmolStr::new(ctx.text(name_tok)?);
            let fstart = name_tok.start;
            cur.bump();
            let value = if cur.eat(TokenKind::Colon) {
                try_hand_lower_expr(ctx, cur, 0)?
            } else {
                let fspan = ctx.span(fstart, name_tok.start + name_tok.len);
                ctx.pool.alloc_expr(
                    ExprKind::Path {
                        path: smallvec::smallvec![fname.clone()],
                    },
                    fspan,
                )
            };
            let fend = ctx.pool.expr_span(value).end;
            let init_id = ctx.pool.alloc_field_init(FieldInit {
                span: ctx.span(fstart, fend),
                name: fname,
                value,
            });
            fields.push(init_id);
            if !cur.eat(TokenKind::Comma) {
                break;
            }
            if cur.peek_kind() == Some(TokenKind::RBrace) {
                break;
            }
        }
    }
    let close = cur.expect(TokenKind::RBrace)?;
    let range = ctx.pool.alloc_field_init_list(&fields);
    Some(ctx.pool.alloc_expr(
        ExprKind::StructLiteral { ty, fields: range },
        ctx.span(left_span.start, close.start + close.len),
    ))
}

pub(super) fn parse_type_led(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
) -> Option<ExprId> {
    let ty = parse_type(ctx, cur)?;
    if cur.eat(TokenKind::LBrace) {
        let mut fields = Vec::new();
        if cur.peek_kind() != Some(TokenKind::RBrace) {
            loop {
                let name_tok = cur.expect(TokenKind::IdentValue)?;
                let name = SmolStr::new(ctx.text(name_tok)?);
                let fstart = name_tok.start;
                cur.expect(TokenKind::Colon)?;
                let value = try_hand_lower_expr(ctx, cur, 0)?;
                let fend = ctx.pool.expr_span(value).end;
                let init_id = ctx.pool.alloc_field_init(FieldInit {
                    span: ctx.span(fstart, fend),
                    name,
                    value,
                });
                fields.push(init_id);
                if !cur.eat(TokenKind::Comma) {
                    break;
                }
                if cur.peek_kind() == Some(TokenKind::RBrace) {
                    break;
                }
            }
        }
        let close = cur.expect(TokenKind::RBrace)?;
        let range = ctx.pool.alloc_field_init_list(&fields);
        return Some(ctx.pool.alloc_expr(
            ExprKind::StructLiteral { ty, fields: range },
            ctx.span(start, close.start + close.len),
        ));
    }
    // Type.member
    let named_info = match ctx.pool.type_expr(ty) {
        TypeExpr::Named { name, args, .. } if args.is_empty() => Some(name.clone()),
        TypeExpr::Primitive { span, name } => Some(TypeName {
            span: *span,
            path: smallvec::smallvec![name.clone()],
        }),
        _ => None,
    };
    if let Some(type_name) = named_info
        && cur.eat(TokenKind::Dot)
    {
        let mem = cur.peek()?;
        if !mem.kind.is_contextual_member_name() {
            return None;
        }
        let member = SmolStr::new(ctx.text(mem)?);
        cur.bump();
        return Some(ctx.pool.alloc_expr(
            ExprKind::TypePath { type_name, member },
            ctx.span(start, mem.start + mem.len),
        ));
    }
    None
}
