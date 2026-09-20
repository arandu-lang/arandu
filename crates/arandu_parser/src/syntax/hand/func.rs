use super::cursor::{Cursor, HandCtx, tokens_in_range};
use super::decl::{
    parse_attributes, parse_generic_params, parse_visibility, parse_where_clause,
    skip_leading_doc_comments,
};
use super::stmt::try_hand_lower_block;
use super::ty::{parse_result_type, parse_type};
use crate::ast::ast_pool::AstPool;
use crate::syntax::kind::{SyntaxKind, SyntaxNode};
use crate::{FuncDecl, FuncName, FuncSignature, Ownership, Param, TypeName};
use arandu_lexer::{Span, Token, TokenKind};
use smallvec::smallvec;
use smol_str::SmolStr;

/// Free/method `func`, with `public`/`async`/attrs/generics/where when present.
#[must_use]
pub fn try_hand_lower_func_item(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    func: &SyntaxNode,
    file_id: u32,
) -> Option<FuncDecl> {
    let block_node = func.children().find(|n| n.kind() == SyntaxKind::BLOCK)?;
    let body = try_hand_lower_block(pool, source, tokens, &block_node, file_id)?;

    let fr = func.text_range();
    let fs = u32::from(fr.start());
    let bs = u32::from(block_node.text_range().start());
    let sig_toks = tokens_in_range(tokens, fs, bs);
    if sig_toks.is_empty() {
        return None;
    }

    let mut ctx = HandCtx {
        pool,
        source,
        file_id,
    };
    let mut cur = Cursor::new(&sig_toks);
    skip_leading_doc_comments(&mut cur);
    let attrs = parse_attributes(&mut ctx, &mut cur)?;
    let visibility = parse_visibility(&mut cur);
    let is_async = cur.eat(TokenKind::KwAsync);
    cur.expect(TokenKind::KwFunc)?;
    let name = parse_func_name(&mut ctx, &mut cur)?;
    let generic_params = parse_generic_params(&mut ctx, &mut cur)?;
    cur.expect(TokenKind::LParen)?;
    let recv = match &name {
        FuncName::Method { receiver, .. } => Some(receiver.clone()),
        FuncName::Free { .. } => None,
    };
    let params = parse_params(&mut ctx, &mut cur, recv.as_ref())?;
    cur.expect(TokenKind::RParen)?;
    let result = if cur.eat(TokenKind::Colon) {
        Some(parse_result_type(&mut ctx, &mut cur)?)
    } else {
        None
    };
    let where_clause = parse_where_clause(&mut ctx, &mut cur)?;
    if !cur.at_end() {
        return None;
    }

    // Span: RD marks at `async`/`func` after attrs+visibility are consumed.
    let sig_start = sig_toks
        .iter()
        .find(|t| matches!(t.kind, TokenKind::KwAsync | TokenKind::KwFunc))
        .map(|t| t.start)
        .or_else(|| sig_toks.first().map(|t| t.start))
        .unwrap_or(fs);
    let body_end = {
        let br = block_node.text_range();
        let bs = u32::from(br.start());
        let be = u32::from(br.end());
        tokens
            .iter()
            .rev()
            .find(|t| {
                matches!(t.kind, TokenKind::RBrace)
                    && !t.inserted
                    && t.start >= bs
                    && t.start < be.max(bs + 1)
            })
            .map(|t| t.start + t.len)
            .unwrap_or(body.span.end)
    };
    Some(FuncDecl {
        span: Span::new(file_id, sig_start, body_end),
        attrs: attrs.into(),
        visibility,
        is_async,
        name,
        generic_params,
        params,
        result,
        where_clause,
        body,
    })
}

pub(super) fn parse_func_name(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<FuncName> {
    let first = cur.peek()?;
    // Method: Type.method or path.Type.method — require IdentType/Value . IdentValue
    // without consuming multi-seg as free name.
    if matches!(first.kind, TokenKind::IdentType | TokenKind::IdentValue) {
        // Look ahead for `.` + method name and then `(`/`<`
        if cur
            .peek_at(1)
            .is_some_and(|t| matches!(t.kind, TokenKind::Dot))
            && cur
                .peek_at(2)
                .is_some_and(|t| t.kind.is_contextual_member_name())
            && cur
                .peek_at(3)
                .is_some_and(|t| matches!(t.kind, TokenKind::LParen | TokenKind::Lt))
        {
            let recv_tok = cur.bump()?;
            let recv_name = SmolStr::new(ctx.text(recv_tok)?);
            cur.expect(TokenKind::Dot)?;
            let method_tok = cur.peek().filter(|t| t.kind.is_contextual_member_name())?;
            cur.bump();
            let method = SmolStr::new(ctx.text(method_tok)?);
            return Some(FuncName::Method {
                span: ctx.span(recv_tok.start, method_tok.start + method_tok.len),
                receiver: TypeName {
                    span: ctx.token_span(recv_tok),
                    path: smallvec![recv_name],
                },
                name: method,
            });
        }
    }
    let name_tok = cur.expect(TokenKind::IdentValue)?;
    Some(FuncName::Free {
        span: ctx.token_span(name_tok),
        name: SmolStr::new(ctx.text(name_tok)?),
    })
}

pub(super) fn parse_params(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    method_receiver: Option<&TypeName>,
) -> Option<Vec<Param>> {
    let mut params = Vec::new();
    if cur.peek_kind() == Some(TokenKind::RParen) {
        return Some(params);
    }
    loop {
        let p_start_tok = cur.peek()?;
        let p_start = p_start_tok.start;
        // ownership keywords
        let ownership = match cur.peek_kind() {
            Some(TokenKind::KwOwn) => {
                cur.bump();
                Some(Ownership::Own)
            }
            Some(TokenKind::KwMut) => {
                cur.bump();
                Some(Ownership::Mut)
            }
            Some(TokenKind::KwShared) => {
                cur.bump();
                Some(Ownership::Shared)
            }
            _ => None,
        };
        let name_tok = cur.peek()?;
        let (name, is_receiver) = match name_tok.kind {
            TokenKind::KwSelf => {
                cur.bump();
                (SmolStr::new_static("self"), true)
            }
            TokenKind::IdentValue => {
                let n = SmolStr::new(ctx.text(name_tok)?);
                cur.bump();
                (n, false)
            }
            _ => return None,
        };
        let is_variadic = cur.eat(TokenKind::Ellipsis);
        let ty = if cur.eat(TokenKind::Colon) {
            parse_type(ctx, cur)?
        } else if is_receiver {
            if let Some(recv) = method_receiver {
                let args = ctx.pool.alloc_type_expr_list(&[]);
                ctx.pool.alloc_type_expr(crate::TypeExpr::Named {
                    span: recv.span,
                    name: recv.clone(),
                    args,
                })
            } else {
                let span = ctx.token_span(name_tok);
                let args = ctx.pool.alloc_type_expr_list(&[]);
                ctx.pool.alloc_type_expr(crate::TypeExpr::Named {
                    span,
                    name: TypeName {
                        span,
                        path: smallvec![SmolStr::new_static("Self")],
                    },
                    args,
                })
            }
        } else {
            return None;
        };
        // When self has an explicit `: Type`, use type end; if type span was
        // synthesized from the method receiver name (elsewhere), clamp to `self`.
        let ty_span = ctx.pool.type_expr_span(ty);
        let p_end = if is_receiver && ty_span.start < p_start {
            name_tok.start + name_tok.len
        } else {
            ty_span.end.max(name_tok.start + name_tok.len)
        };
        params.push(Param {
            span: ctx.span(p_start, p_end),
            attrs: smallvec![],
            ownership,
            name,
            ty,
            is_variadic,
            is_receiver,
        });
        if cur.eat(TokenKind::Comma) {
            continue;
        }
        break;
    }
    Some(params)
}

/// Synthetic `Self` type for bare `self` in interface / method signatures.
///
/// Must stay aligned with RD `parser/decl.rs::parse_interface_decl` (single
/// dual-path contract: hand and RD lower the same AST shape for `self`).
pub(super) fn synthetic_self_receiver(span: Span) -> TypeName {
    TypeName {
        span,
        path: smallvec![SmolStr::new_static("Self")],
    }
}

pub(super) fn parse_func_signature(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<FuncSignature> {
    parse_func_signature_with_receiver(ctx, cur, None)
}

/// Like [`parse_func_signature`], but `self` without `: Type` binds to `receiver`
/// when provided (interface methods → `Self`, same as RD).
pub(super) fn parse_func_signature_with_receiver(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    method_receiver: Option<&TypeName>,
) -> Option<FuncSignature> {
    let attrs = parse_attributes(ctx, cur)?;
    let start = cur.peek()?.start;
    cur.expect(TokenKind::KwFunc)?;
    let name_tok = cur.expect(TokenKind::IdentValue)?;
    let name = SmolStr::new(ctx.text(name_tok)?);
    let generic_params = parse_generic_params(ctx, cur)?;
    cur.expect(TokenKind::LParen)?;
    let params = parse_params(ctx, cur, method_receiver)?;
    cur.expect(TokenKind::RParen)?;
    let result = if cur.eat(TokenKind::Colon) {
        Some(parse_result_type(ctx, cur)?)
    } else {
        None
    };
    let where_clause = parse_where_clause(ctx, cur)?;
    let end = result
        .as_ref()
        .map(|r| match r {
            crate::ResultType::Single { span, .. } => span.end,
            crate::ResultType::Multi { span, .. } => span.end,
        })
        .unwrap_or(name_tok.start + name_tok.len);
    Some(FuncSignature {
        span: ctx.span(start, end),
        attrs: attrs.into(),
        name,
        generic_params,
        params,
        result,
        where_clause,
    })
}
