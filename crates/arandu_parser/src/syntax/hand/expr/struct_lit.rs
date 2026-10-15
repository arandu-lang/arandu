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

/// Peek inside `{` without consuming to see if it looks like a struct literal.
pub(super) fn looks_like_struct_lit_brace(cur: &Cursor<'_>) -> bool {
    if cur.peek_kind() != Some(TokenKind::LBrace) {
        return false;
    }
    match cur.peek_at(1).map(|t| t.kind) {
        Some(TokenKind::RBrace | TokenKind::RangeExclusive) => true,
        Some(TokenKind::IdentValue | TokenKind::IdentType) => cur.peek_at(2).is_some_and(|t| {
            matches!(
                t.kind,
                TokenKind::Colon | TokenKind::Comma | TokenKind::RBrace
            )
        }),
        _ => false,
    }
}

/// After a type-shaped path, `{` starts a struct lit if empty or `ident:` / shorthand.
pub(super) fn looks_like_struct_lit_after_type_path(
    ctx: &HandCtx<'_>,
    cur: &Cursor<'_>,
    left: ExprId,
) -> bool {
    if type_path_segments_from_expr(ctx, left).is_none() {
        return false;
    }
    looks_like_struct_lit_brace(cur)
}

fn parse_struct_lit_fields_brace(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<(crate::ast::IndexRange, u32)> {
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
    Some((range, close.start + close.len))
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

    let (range, end) = parse_struct_lit_fields_brace(ctx, cur)?;
    Some(ctx.pool.alloc_expr(
        ExprKind::StructLiteral { ty, fields: range },
        ctx.span(left_span.start, end),
    ))
}

pub(super) fn parse_type_led(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
) -> Option<ExprId> {
    let ty = parse_type(ctx, cur)?;
    if cur.peek_kind() == Some(TokenKind::LBrace) {
        let (range, end) = parse_struct_lit_fields_brace(ctx, cur)?;
        return Some(ctx.pool.alloc_expr(
            ExprKind::StructLiteral { ty, fields: range },
            ctx.span(start, end),
        ));
    }
    // Type.member / Type<T>.member
    let named_info = match ctx.pool.type_expr(ty) {
        TypeExpr::Named { name, args, .. } => Some((name.clone(), *args)),
        TypeExpr::Primitive { span, name } => Some((
            TypeName {
                span: *span,
                path: smallvec::smallvec![name.clone()],
            },
            crate::ast::IndexRange::empty(),
        )),
        _ => None,
    };
    if let Some((mut type_name, args)) = named_info.clone()
        && cur.eat(TokenKind::Dot)
    {
        let mem = cur.peek()?;
        if !mem.kind.is_contextual_member_name() {
            return None;
        }
        let member = SmolStr::new(ctx.text(mem)?);
        cur.bump();
        let member_span = ctx.span(start, mem.start + mem.len);
        if looks_like_struct_lit_brace(cur) {
            type_name.path.push(member);
            type_name.span = member_span;
            let variant_ty = ctx.pool.alloc_type_expr(TypeExpr::Named {
                span: member_span,
                name: type_name,
                args,
            });
            let (range, end) = parse_struct_lit_fields_brace(ctx, cur)?;
            return Some(ctx.pool.alloc_expr(
                ExprKind::StructLiteral {
                    ty: variant_ty,
                    fields: range,
                },
                ctx.span(start, end),
            ));
        }
        let tp = ctx
            .pool
            .alloc_expr(ExprKind::TypePath { type_name, member }, member_span);
        if args.is_empty() {
            return Some(tp);
        }
        return Some(
            ctx.pool
                .alloc_expr(ExprKind::Generic { callee: tp, args }, member_span),
        );
    }
    if let Some((name, args)) = named_info
        && args.is_empty()
        && name.path.len() > 1
    {
        let span = name.span;
        return Some(
            ctx.pool
                .alloc_expr(ExprKind::Path { path: name.path }, span),
        );
    }
    None
}
