use super::cursor::{Cursor, HandCtx};
use super::decl::{
    item_tokens, parse_attributes, parse_generic_params, parse_visibility, parse_where_clause,
    skip_leading_doc_comments, token_bounds_span,
};
use super::ty::parse_type;
use crate::ast::ast_pool::AstPool;
use crate::syntax::kind::SyntaxNode;
use crate::{EnumDecl, EnumPayload, EnumVariant, FieldDecl, StructDecl};
use arandu_lexer::{Span, Token, TokenKind};
use smol_str::SmolStr;

pub(super) fn try_hand_lower_struct(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    item: &SyntaxNode,
    file_id: u32,
) -> Option<StructDecl> {
    let toks = item_tokens(tokens, item);
    let mut ctx = HandCtx::new(pool, source, file_id);
    let mut cur = Cursor::new(&toks);
    skip_leading_doc_comments(&mut cur);
    let attrs = parse_attributes(&mut ctx, &mut cur)?;
    let visibility = parse_visibility(&mut cur);
    cur.expect(TokenKind::KwStruct)?;
    let name_tok = cur
        .peek()
        .filter(|t| matches!(t.kind, TokenKind::IdentType | TokenKind::IdentValue))?;
    let name = SmolStr::new(ctx.text(name_tok)?);
    cur.bump();
    let generic_params = parse_generic_params(&mut ctx, &mut cur)?;
    let where_clause = parse_where_clause(&mut ctx, &mut cur)?;
    cur.expect(TokenKind::LBrace)?;
    let mut fields = Vec::new();
    while cur.peek_kind() != Some(TokenKind::RBrace) && !cur.at_end() {
        while cur.eat(TokenKind::Semicolon) {}
        if cur.peek_kind() == Some(TokenKind::RBrace) {
            break;
        }
        fields.push(parse_field(&mut ctx, &mut cur, true)?);
    }
    cur.expect(TokenKind::RBrace)?;
    if !cur.at_end() {
        return None;
    }
    // Span: RD marks at `struct` after attrs+visibility.
    let start = toks
        .iter()
        .find(|t| matches!(t.kind, TokenKind::KwStruct))
        .map(|t| t.start)
        .or_else(|| toks.first().map(|t| t.start))?;
    let end = toks.last().map(|t| t.end())?;
    Some(StructDecl {
        span: Span::new(file_id, start, end),
        attrs: attrs.into(),
        visibility,
        name,
        generic_params,
        where_clause,
        fields,
    })
}

pub(super) fn parse_field(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    require_semi: bool,
) -> Option<FieldDecl> {
    let field_start = cur.peek()?.start;
    let attrs = parse_attributes(ctx, cur)?;
    let visibility = parse_visibility(cur);
    let name_tok = cur.expect(TokenKind::IdentValue)?;
    let name = SmolStr::new(ctx.text(name_tok)?);
    cur.expect(TokenKind::Colon)?;
    let ty = parse_type(ctx, cur)?;
    if require_semi {
        let _ = cur.eat(TokenKind::Semicolon);
    } else {
        let _ = cur.eat(TokenKind::Comma);
        let _ = cur.eat(TokenKind::Semicolon);
    }
    let end = ctx.pool.type_expr_span(ty).end;
    Some(FieldDecl {
        span: ctx.span(field_start, end),
        attrs: attrs.into(),
        visibility,
        name,
        ty,
    })
}

pub(super) fn try_hand_lower_enum(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    item: &SyntaxNode,
    file_id: u32,
) -> Option<EnumDecl> {
    let toks = item_tokens(tokens, item);
    let mut ctx = HandCtx::new(pool, source, file_id);
    let mut cur = Cursor::new(&toks);
    skip_leading_doc_comments(&mut cur);
    let attrs = parse_attributes(&mut ctx, &mut cur)?;
    let visibility = parse_visibility(&mut cur);
    cur.expect(TokenKind::KwEnum)?;
    let name_tok = cur
        .peek()
        .filter(|t| matches!(t.kind, TokenKind::IdentType | TokenKind::IdentValue))?;
    let name = SmolStr::new(ctx.text(name_tok)?);
    cur.bump();
    let generic_params = parse_generic_params(&mut ctx, &mut cur)?;
    let where_clause = parse_where_clause(&mut ctx, &mut cur)?;
    cur.expect(TokenKind::LBrace)?;
    let mut variants = Vec::new();
    while cur.peek_kind() != Some(TokenKind::RBrace) && !cur.at_end() {
        while cur.eat(TokenKind::Semicolon) || cur.eat(TokenKind::Comma) {}
        if cur.peek_kind() == Some(TokenKind::RBrace) {
            break;
        }
        variants.push(parse_enum_variant(&mut ctx, &mut cur)?);
        let _ = cur.eat(TokenKind::Comma);
        let _ = cur.eat(TokenKind::Semicolon);
    }
    cur.expect(TokenKind::RBrace)?;
    if !cur.at_end() {
        return None;
    }
    Some(EnumDecl {
        span: token_bounds_span(file_id, &toks)?,
        attrs: attrs.into(),
        visibility,
        name,
        generic_params,
        where_clause,
        variants,
    })
}

pub(super) fn parse_enum_variant(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<EnumVariant> {
    let attrs = parse_attributes(ctx, cur)?;
    let name_tok = cur
        .peek()
        .filter(|t| matches!(t.kind, TokenKind::IdentType | TokenKind::IdentValue))?;
    let name = SmolStr::new(ctx.text(name_tok)?);
    let start = name_tok.start;
    cur.bump();
    let payload = if cur.eat(TokenKind::LParen) {
        let mut types = Vec::new();
        if cur.peek_kind() != Some(TokenKind::RParen) {
            loop {
                types.push(parse_type(ctx, cur)?);
                if cur.eat(TokenKind::Comma) {
                    continue;
                }
                break;
            }
        }
        let close = cur.expect(TokenKind::RParen)?;
        let range = ctx.pool.alloc_type_expr_list(&types);
        Some(EnumPayload::Tuple {
            span: ctx.span(start, close.start + close.len),
            types: range,
        })
    } else if cur.eat(TokenKind::LBrace) {
        let mut fields = Vec::new();
        while cur.peek_kind() != Some(TokenKind::RBrace) && !cur.at_end() {
            fields.push(parse_field(ctx, cur, false)?);
            let _ = cur.eat(TokenKind::Comma);
        }
        let close = cur.expect(TokenKind::RBrace)?;
        Some(EnumPayload::Struct {
            span: ctx.span(start, close.start + close.len),
            fields,
        })
    } else {
        None
    };
    let end = match &payload {
        Some(EnumPayload::Tuple { span, .. } | EnumPayload::Struct { span, .. }) => span.end,
        None => name_tok.start + name_tok.len,
    };
    Some(EnumVariant {
        span: ctx.span(start, end),
        attrs: attrs.into(),
        name,
        payload,
    })
}
