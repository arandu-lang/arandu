//! Hand-lower type expressions.

use super::cursor::{Cursor, HandCtx};
use crate::ast::ast_pool::TypeExprId;
use crate::{IndexRange, ResultType, TypeExpr, TypeName};
use arandu_lexer::{Token, TokenKind};
use smallvec::smallvec;
use smol_str::SmolStr;

#[must_use]
pub fn can_start_type_kind(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::IdentType
            | TokenKind::IdentValue
            | TokenKind::KwPtr
            | TokenKind::KwOwn
            | TokenKind::KwRef
            | TokenKind::KwMut
            | TokenKind::LBracket
            | TokenKind::LParen
            | TokenKind::KwFunc
            | TokenKind::Amp // `&T` / `&mut T`
    ) || primitive_type_token_name(kind).is_some()
}

#[must_use]
pub fn primitive_type_token_name(kind: TokenKind) -> Option<&'static str> {
    match kind {
        TokenKind::TypeInt => Some("int"),
        TokenKind::TypeUint => Some("uint"),
        TokenKind::TypeFloat => Some("float"),
        TokenKind::TypeI8 => Some("i8"),
        TokenKind::TypeI16 => Some("i16"),
        TokenKind::TypeI32 => Some("i32"),
        TokenKind::TypeI64 => Some("i64"),
        TokenKind::TypeU8 => Some("u8"),
        TokenKind::TypeU16 => Some("u16"),
        TokenKind::TypeU32 => Some("u32"),
        TokenKind::TypeU64 => Some("u64"),
        TokenKind::TypeF32 => Some("f32"),
        TokenKind::TypeF64 => Some("f64"),
        TokenKind::TypeBool => Some("bool"),
        TokenKind::TypeByte => Some("byte"),
        TokenKind::TypeChar => Some("char"),
        TokenKind::TypeStr => Some("str"),
        TokenKind::TypeAny => Some("any"),
        TokenKind::TypeErr => Some("Err"),
        _ => None,
    }
}

/// Single-token type (primitive or simple named).
pub fn try_hand_lower_type(
    pool: &mut crate::ast::ast_pool::AstPool,
    source: &str,
    t: &Token,
    file_id: u32,
) -> Option<TypeExprId> {
    let mut ctx = HandCtx {
        pool,
        source,
        file_id,
    };
    let toks = [t];
    let mut cur = Cursor::new(&toks);
    let ty = parse_type(&mut ctx, &mut cur)?;
    if !cur.at_end() {
        return None;
    }
    Some(ty)
}

/// Parse a type expression advancing `cur`.
pub fn parse_type(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<TypeExprId> {
    let start_tok = cur.peek()?;
    let start = start_tok.start;

    // Canonical ownership spelling: `own T`, `ref T`, `mut ref T`.
    if cur.eat(TokenKind::KwOwn) {
        return parse_type(ctx, cur);
    }
    if cur.eat(TokenKind::KwRef) {
        let inner = parse_type(ctx, cur)?;
        let end = ctx.pool.type_expr_span(inner).end;
        return Some(ctx.pool.alloc_type_expr(TypeExpr::Ref {
            span: ctx.span(start, end),
            inner,
        }));
    }
    if cur.eat(TokenKind::KwMut) {
        cur.expect(TokenKind::KwRef)?;
        let inner = parse_type(ctx, cur)?;
        let end = ctx.pool.type_expr_span(inner).end;
        return Some(ctx.pool.alloc_type_expr(TypeExpr::RefMut {
            span: ctx.span(start, end),
            inner,
        }));
    }

    // `&mut T` / `&T` — safe reference types (F2.0). Not raw `ptr[T]`.
    if cur.eat(TokenKind::Amp) {
        let is_mut = cur.eat(TokenKind::KwMut);
        let inner = parse_type(ctx, cur)?;
        let end = ctx.pool.type_expr_span(inner).end;
        let span = ctx.span(start, end);
        return Some(ctx.pool.alloc_type_expr(if is_mut {
            TypeExpr::RefMut { span, inner }
        } else {
            TypeExpr::Ref { span, inner }
        }));
    }

    // `ptr[T]` (RD requires brackets after KwPtr)
    if cur.eat(TokenKind::KwPtr) {
        cur.expect(TokenKind::LBracket)?;
        let inner = parse_type(ctx, cur)?;
        let close = cur.expect(TokenKind::RBracket)?;
        return Some(ctx.pool.alloc_type_expr(TypeExpr::Pointer {
            span: ctx.span(start, close.start + close.len),
            inner,
        }));
    }

    // `[]T` slice or `[N]T` array
    if cur.eat(TokenKind::LBracket) {
        if cur.eat(TokenKind::RBracket) {
            let inner = parse_type(ctx, cur)?;
            let end = ctx.pool.type_expr_span(inner).end;
            return Some(ctx.pool.alloc_type_expr(TypeExpr::Slice {
                span: ctx.span(start, end),
                inner,
            }));
        }
        let size_tok = cur.peek()?;
        if !matches!(
            size_tok.kind,
            TokenKind::IntDec
                | TokenKind::IntHex
                | TokenKind::IntBin
                | TokenKind::IntOct
                | TokenKind::IdentValue
                | TokenKind::IdentType
        ) {
            return None;
        }
        let size = SmolStr::new(ctx.text(size_tok)?);
        cur.bump();
        cur.expect(TokenKind::RBracket)?;
        let elem = parse_type(ctx, cur)?;
        let end = ctx.pool.type_expr_span(elem).end;
        return Some(ctx.pool.alloc_type_expr(TypeExpr::Array {
            span: ctx.span(start, end),
            size,
            size_span: ctx.token_span(size_tok),
            elem,
        }));
    }

    // `(T)` grouped type or `(T, U) -> R` function type. A comma without an
    // arrow is not silently accepted as a tuple because tuple types do not yet
    // have an AST representation.
    if cur.eat(TokenKind::LParen) {
        let mut params = Vec::new();
        let mut saw_comma = false;
        if cur.peek_kind() != Some(TokenKind::RParen) {
            loop {
                params.push(parse_type(ctx, cur)?);
                if !cur.eat(TokenKind::Comma) {
                    break;
                }
                saw_comma = true;
                if cur.peek_kind() == Some(TokenKind::RParen) {
                    break;
                }
            }
        }
        let close = cur.expect(TokenKind::RParen)?;
        if cur.eat(TokenKind::Arrow) {
            let result_start = cur.peek()?.start;
            let result_ty = parse_type(ctx, cur)?;
            let end = ctx.pool.type_expr_span(result_ty).end;
            let result = ResultType::Single {
                span: ctx.span(result_start, end),
                ty: result_ty,
            };
            let params = ctx.pool.alloc_type_expr_list(&params);
            return Some(ctx.pool.alloc_type_expr(TypeExpr::Func {
                span: ctx.span(start, end),
                params,
                result: Some(result),
            }));
        }
        if saw_comma || params.len() != 1 {
            return None;
        }
        let inner = params[0];
        Some(ctx.pool.alloc_type_expr(TypeExpr::Group {
            span: ctx.span(start, close.start + close.len),
            inner,
        }))
    } else {
        parse_named_or_primitive_type(ctx, cur, start)
    }
}

fn parse_named_or_primitive_type(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
) -> Option<TypeExprId> {
    let t = cur.bump()?;
    let span = ctx.token_span(t);
    let mut ty = if let Some(name) = primitive_type_token_name(t.kind) {
        ctx.pool.alloc_type_expr(TypeExpr::Primitive {
            span,
            name: SmolStr::new_static(name),
        })
    } else {
        match t.kind {
            // Type name: IdentType alone, or (IdentValue .)+ IdentType/IdentValue.
            // Do not swallow `Type.member` (e.g. Result.Ok) used in expressions.
            TokenKind::IdentType => {
                let text = ctx.text(t)?;
                let path = smallvec![SmolStr::new(text)];
                let name = TypeName {
                    span: ctx.span(start, t.start + t.len),
                    path,
                };
                let (args, args_end) = if cur.peek_kind() == Some(TokenKind::Lt) {
                    parse_generic_type_args(ctx, cur)?
                } else {
                    (ctx.pool.alloc_type_expr_list(&[]), name.span.end)
                };
                ctx.pool.alloc_type_expr(TypeExpr::Named {
                    span: ctx.span(start, args_end),
                    name,
                    args,
                })
            }
            TokenKind::IdentValue => {
                let mut path = smallvec![SmolStr::new(ctx.text(t)?)];
                let mut end = t.start + t.len;
                // module segments: value.value...
                while cur.peek_kind() == Some(TokenKind::Dot)
                    && cur
                        .peek_at(1)
                        .is_some_and(|n| matches!(n.kind, TokenKind::IdentValue))
                {
                    cur.bump(); // Dot
                    let seg = cur.bump()?;
                    path.push(SmolStr::new(ctx.text(seg)?));
                    end = seg.start + seg.len;
                }
                // optional final Type after modules: a.b.Type
                if cur.peek_kind() == Some(TokenKind::Dot)
                    && cur
                        .peek_at(1)
                        .is_some_and(|n| matches!(n.kind, TokenKind::IdentType))
                {
                    cur.bump();
                    let seg = cur.bump()?;
                    path.push(SmolStr::new(ctx.text(seg)?));
                    end = seg.start + seg.len;
                }
                let name = TypeName {
                    span: ctx.span(start, end),
                    path,
                };
                let (args, args_end) = if cur.peek_kind() == Some(TokenKind::Lt) {
                    parse_generic_type_args(ctx, cur)?
                } else {
                    (ctx.pool.alloc_type_expr_list(&[]), name.span.end)
                };
                ctx.pool.alloc_type_expr(TypeExpr::Named {
                    span: ctx.span(start, args_end),
                    name,
                    args,
                })
            }
            _ => return None,
        }
    };

    // postfix ?
    if cur.eat(TokenKind::Question) {
        let end = ctx.pool.type_expr_span(ty).end + 1;
        ty = ctx.pool.alloc_type_expr(TypeExpr::Nullable {
            span: ctx.span(start, end),
            inner: ty,
        });
    }

    Some(ty)
}

fn parse_generic_type_args(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<(IndexRange, u32)> {
    cur.expect(TokenKind::Lt)?;
    let mut args = Vec::new();
    if !cur.at_gt() {
        loop {
            if cur.peek_kind() == Some(TokenKind::IntDec) {
                let token = cur.bump()?;
                let value = SmolStr::new(ctx.text(token)?);
                args.push(ctx.pool.alloc_type_expr(TypeExpr::Const {
                    span: ctx.token_span(token),
                    value,
                }));
            } else {
                args.push(parse_type(ctx, cur)?);
            }
            if cur.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
    }
    let (gt_start, gt_len) = cur.expect_gt()?;
    Some((ctx.pool.alloc_type_expr_list(&args), gt_start + gt_len))
}

/// `: T` result type (single).
pub fn parse_result_type(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<ResultType> {
    let ty_start = cur.peek()?.start;
    let ty = parse_type(ctx, cur)?;
    let ty_end = ctx.pool.type_expr_span(ty).end;
    Some(ResultType::Single {
        span: ctx.span(ty_start, ty_end),
        ty,
    })
}

/// Dotted path of idents (`a.b.c`).
pub fn parse_dotted_ident_path(
    ctx: &HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<smallvec::SmallVec<[SmolStr; 3]>> {
    let mut path = smallvec::SmallVec::new();
    loop {
        let t = cur.peek()?;
        if !matches!(t.kind, TokenKind::IdentValue | TokenKind::IdentType)
            && !crate::parser::is_contextual_module_segment(&t.kind)
        {
            return None;
        }
        path.push(SmolStr::new(ctx.text(t)?));
        cur.bump();
        if cur.eat(TokenKind::Dot) {
            continue;
        }
        break;
    }
    if path.is_empty() { None } else { Some(path) }
}
