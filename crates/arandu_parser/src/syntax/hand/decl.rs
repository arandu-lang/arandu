//! Hand-lower top-level declarations (module, import, func, struct, …).

use super::cursor::{Cursor, HandCtx, drop_trailing_semi, tokens_in_range};
use super::expr::try_hand_lower_expr;
use super::ty::{parse_dotted_ident_path, parse_type};
use crate::ast::ast_pool::AstPool;
use crate::syntax::kind::{SyntaxKind, SyntaxNode};
use crate::{
    Attribute, ConstDecl, ExprKind, ExternDecl, GenericParam, ImportDecl, ImportItem,
    InterfaceDecl, ModuleDecl, TopLevelDecl, TypeAliasDecl, Visibility, WhereItem,
};
use arandu_lexer::{Span, Token, TokenKind};
use smallvec::{SmallVec, smallvec};
use smol_str::SmolStr;

/// Hand-lower any top-level green item into a [`TopLevelDecl`] (not module/import).
#[must_use]
pub fn try_hand_lower_top_level(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    item: &SyntaxNode,
    file_id: u32,
) -> Option<TopLevelDecl> {
    match item.kind() {
        SyntaxKind::FUNC_ITEM => {
            super::func::try_hand_lower_func_item(pool, source, tokens, item, file_id)
                .map(TopLevelDecl::Func)
        }
        SyntaxKind::STRUCT_ITEM => {
            super::adt::try_hand_lower_struct(pool, source, tokens, item, file_id)
                .map(TopLevelDecl::Struct)
        }
        SyntaxKind::ENUM_ITEM => {
            super::adt::try_hand_lower_enum(pool, source, tokens, item, file_id)
                .map(TopLevelDecl::Enum)
        }
        SyntaxKind::CONST_ITEM => {
            try_hand_lower_const(pool, source, tokens, item, file_id).map(TopLevelDecl::Const)
        }
        SyntaxKind::TYPE_ALIAS_ITEM => {
            try_hand_lower_type_alias(pool, source, tokens, item, file_id)
                .map(TopLevelDecl::TypeAlias)
        }
        SyntaxKind::INTERFACE_ITEM => try_hand_lower_interface(pool, source, tokens, item, file_id)
            .map(TopLevelDecl::Interface),
        SyntaxKind::EXTERN_ITEM => {
            try_hand_lower_extern(pool, source, tokens, item, file_id).map(TopLevelDecl::Extern)
        }
        SyntaxKind::ITEM => {
            // Heuristic: try func / const / struct by leading keyword after attrs.
            super::func::try_hand_lower_func_item(pool, source, tokens, item, file_id)
                .map(TopLevelDecl::Func)
                .or_else(|| {
                    try_hand_lower_const(pool, source, tokens, item, file_id)
                        .map(TopLevelDecl::Const)
                })
                .or_else(|| {
                    super::adt::try_hand_lower_struct(pool, source, tokens, item, file_id)
                        .map(TopLevelDecl::Struct)
                })
        }
        _ => None,
    }
}

pub(super) fn item_tokens<'a>(tokens: &'a [Token], item: &SyntaxNode) -> Vec<&'a Token> {
    let r = item.text_range();
    let mut toks = tokens_in_range(tokens, u32::from(r.start()), u32::from(r.end()));
    drop_trailing_semi(&mut toks);
    toks
}

/// Span from first..last significant token (matches RD marks better than green ranges,
/// which often include leading whitespace between items).
pub(super) fn token_bounds_span(file_id: u32, toks: &[&Token]) -> Option<Span> {
    let first = *toks.first()?;
    let last = *toks.last()?;
    Some(Span::new(file_id, first.start, last.start + last.len))
}

pub(super) fn parse_visibility(cur: &mut Cursor<'_>) -> Visibility {
    if cur.eat(TokenKind::KwPublic) {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

/// Skip item-leading `///` folded into the item span by `expand_item_start_left`.
/// Field/variant docs are **not** skipped here — they stay for parse_field to
/// reject hand lower so RD can attach them (see parser_contract doc test).
pub(super) fn skip_leading_doc_comments(cur: &mut Cursor<'_>) {
    while matches!(cur.peek_kind(), Some(TokenKind::DocComment)) {
        cur.bump();
    }
}

/// `@name` or `@name(...)` — args must be hand-lowerable exprs.
pub(super) fn parse_attributes(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<Vec<Attribute>> {
    let mut attrs = Vec::new();
    while cur.peek_kind() == Some(TokenKind::At) {
        let at = cur.bump()?;
        let name_tok = cur.peek()?;
        if !matches!(name_tok.kind, TokenKind::IdentValue | TokenKind::IdentType) {
            return None;
        }
        let name = SmolStr::new(ctx.text(name_tok)?);
        let start = at.start;
        cur.bump();
        let mut args = Vec::new();
        let mut end = name_tok.start + name_tok.len;
        if cur.eat(TokenKind::LParen) {
            if cur.peek_kind() != Some(TokenKind::RParen) {
                loop {
                    // Attr args may be bare string literals or identifiers (e.g. @Effects(Pure, Net))
                    let is_bare_ident = if let Some(tok) = cur.peek() {
                        matches!(tok.kind, TokenKind::IdentValue | TokenKind::IdentType)
                            && !cur.peek_at(1).is_some_and(|next| {
                                matches!(
                                    next.kind,
                                    TokenKind::Dot | TokenKind::LBrace | TokenKind::LParen
                                )
                            })
                    } else {
                        false
                    };
                    let arg = if is_bare_ident {
                        let t = cur.bump()?;
                        let text = SmolStr::new(ctx.text(t)?);
                        let span = ctx.token_span(t);
                        ctx.pool.alloc_expr(
                            ExprKind::Path {
                                path: smallvec::smallvec![text],
                            },
                            span,
                        )
                    } else {
                        try_hand_lower_expr(ctx, cur, 0)?
                    };
                    args.push(arg);
                    if cur.eat(TokenKind::Comma) {
                        continue;
                    }
                    break;
                }
            }
            let close = cur.expect(TokenKind::RParen)?;
            end = close.start + close.len;
        }
        attrs.push(Attribute {
            span: ctx.span(start, end),
            name_span: ctx.span(name_tok.start, name_tok.start + name_tok.len),
            name,
            args,
        });
    }
    Some(attrs)
}

pub(super) fn parse_generic_params(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<SmallVec<[GenericParam; 2]>> {
    if !cur.eat(TokenKind::Lt) {
        return Some(smallvec![]);
    }
    let mut params = SmallVec::new();
    if !cur.at_gt() {
        loop {
            let is_const = cur.eat(TokenKind::KwConst);
            let name_tok = cur.peek()?;
            if !matches!(name_tok.kind, TokenKind::IdentType | TokenKind::IdentValue) {
                return None;
            }
            let name = SmolStr::new(ctx.text(name_tok)?);
            let p_start = name_tok.start;
            cur.bump();
            let mut constraints = SmallVec::new();
            let mut p_end = name_tok.start + name_tok.len;
            let const_ty = if is_const {
                cur.expect(TokenKind::Colon)?;
                let ty = super::ty::parse_type(ctx, cur)?;
                p_end = ctx.pool.type_expr_span(ty).end;
                Some(ty)
            } else {
                None
            };
            if !is_const && cur.eat(TokenKind::Colon) {
                loop {
                    let ty = super::ty::parse_type(ctx, cur)?;
                    p_end = ctx.pool.type_expr_span(ty).end;
                    constraints.push(ty);
                    if cur.eat(TokenKind::Plus) {
                        continue;
                    }
                    break;
                }
            }
            // T2.1: `T = DefaultType` after optional constraints.
            let default = if !is_const && cur.eat(TokenKind::Equal) {
                let ty = super::ty::parse_type(ctx, cur)?;
                p_end = ctx.pool.type_expr_span(ty).end;
                Some(ty)
            } else {
                None
            };
            params.push(GenericParam {
                span: ctx.span(p_start, p_end),
                name,
                const_ty,
                constraints,
                default,
            });
            if cur.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
    }
    cur.expect_gt()?;
    Some(params)
}

pub(super) fn parse_where_clause(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
) -> Option<SmallVec<[WhereItem; 2]>> {
    if !cur.eat(TokenKind::KwWhere) {
        return Some(smallvec![]);
    }
    let mut items = SmallVec::new();
    loop {
        let name_tok = cur.peek()?;
        if !matches!(name_tok.kind, TokenKind::IdentType | TokenKind::IdentValue) {
            break;
        }
        let name = SmolStr::new(ctx.text(name_tok)?);
        let start = name_tok.start;
        cur.bump();
        cur.expect(TokenKind::Colon)?;
        let mut constraints = SmallVec::new();
        loop {
            let ty = super::ty::parse_type(ctx, cur)?;
            constraints.push(ty);
            if cur.eat(TokenKind::Plus) {
                continue;
            }
            break;
        }
        let item_end = constraints
            .last()
            .map(|c| ctx.pool.type_expr_span(*c).end)
            .unwrap_or(start + name_tok.len);
        items.push(WhereItem {
            span: ctx.span(start, item_end),
            name,
            constraints,
        });
        if cur.eat(TokenKind::Comma) {
            continue;
        }
        break;
    }
    Some(items)
}

/// `module a.b.c`
#[must_use]
pub fn try_hand_lower_module(
    source: &str,
    tokens: &[Token],
    item: &SyntaxNode,
    file_id: u32,
) -> Option<ModuleDecl> {
    let toks = item_tokens(tokens, item);
    if toks.is_empty() || !matches!(toks[0].kind, TokenKind::KwModule) {
        return None;
    }
    let mut throwaway = AstPool::new();
    let ctx = HandCtx::new(&mut throwaway, source, file_id);
    let mut cur = Cursor::new(&toks);
    cur.expect(TokenKind::KwModule)?;
    let path = parse_dotted_ident_path(&ctx, &mut cur)?;
    // Optional `;` terminator.
    let _ = cur.eat(TokenKind::Semicolon);
    if !cur.at_end() {
        return None;
    }
    // Same-line top-level without `;`/newline must be rejected (match RD).
    if let Some(last) = toks.last().copied() {
        let end = last.start + last.len;
        if let Some(next) = tokens
            .iter()
            .find(|t| !matches!(t.kind, TokenKind::Eof) && t.start >= end)
        {
            let between = source.get(end as usize..next.start as usize).unwrap_or("");
            let has_nl = between.contains('\n');
            let has_semi = toks.iter().any(|t| matches!(t.kind, TokenKind::Semicolon));
            if !has_nl && !has_semi && starts_top_level_tok(source, next) {
                return None;
            }
        }
    }
    Some(ModuleDecl {
        span: token_bounds_span(file_id, &toks)?,
        path,
    })
}

fn starts_top_level_decl(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::KwConst
            | TokenKind::KwType
            | TokenKind::KwFunc
            | TokenKind::KwAsync
            | TokenKind::KwStruct
            | TokenKind::KwEnum
            | TokenKind::KwInterface
            | TokenKind::KwExtern
            | TokenKind::KwPublic
            | TokenKind::At
            | TokenKind::KwImport
            | TokenKind::KwFrom
    )
}

/// Like [`starts_top_level_decl`], but also recognizes the soft keyword
/// `from` when it lexes as a plain identifier.
fn starts_top_level_tok(source: &str, tok: &Token) -> bool {
    starts_top_level_decl(tok.kind)
        || (tok.kind == TokenKind::IdentValue && tok.lexeme(source) == "from")
}

/// Consume the soft keyword `from` regardless of whether it lexed as the
/// (now retired) `KwFrom` keyword or as a plain identifier.
fn eat_soft_from(ctx: &HandCtx<'_>, cur: &mut Cursor<'_>) -> bool {
    if matches!(
        cur.peek(),
        Some(tok) if tok.kind == TokenKind::IdentValue && ctx.text(tok) == Some("from")
    ) {
        cur.bump();
        return true;
    }
    false
}

/// All import forms.
#[must_use]
pub fn try_hand_lower_import(
    source: &str,
    tokens: &[Token],
    item: &SyntaxNode,
    file_id: u32,
) -> Option<ImportDecl> {
    let toks = item_tokens(tokens, item);
    if toks.is_empty() {
        return None;
    }
    let mut pool = AstPool::new();
    let mut ctx = HandCtx::new(&mut pool, source, file_id);
    let mut cur = Cursor::new(&toks);
    let span = token_bounds_span(file_id, &toks)?;

    if eat_soft_from(&ctx, &mut cur) {
        // from path import { A, B as C }  OR  from "ext" import { … }
        if cur.peek_kind() == Some(TokenKind::StringStart) {
            let source_s = parse_static_string(&mut ctx, &mut cur)?;
            cur.expect(TokenKind::KwImport)?;
            let items = parse_import_brace_list(&mut ctx, &mut cur)?;
            if !cur.at_end() {
                return None;
            }
            return Some(ImportDecl::ExternalNamed {
                span,
                items,
                source: source_s,
            });
        }
        let path = parse_dotted_ident_path(&ctx, &mut cur)?;
        cur.expect(TokenKind::KwImport)?;
        let items = parse_import_brace_list(&mut ctx, &mut cur)?;
        if !cur.at_end() {
            return None;
        }
        return Some(ImportDecl::Named { span, items, path });
    }

    if cur.eat(TokenKind::KwImport) {
        if cur.peek_kind() == Some(TokenKind::StringStart) {
            let source_s = parse_static_string(&mut ctx, &mut cur)?;
            cur.expect(TokenKind::KwAs)?;
            let alias_tok = cur.expect(TokenKind::IdentValue)?;
            let alias = SmolStr::new(ctx.text(alias_tok)?);
            if !cur.at_end() {
                return None;
            }
            return Some(ImportDecl::ExternalAlias {
                span,
                source: source_s,
                alias,
            });
        }
        let path = parse_dotted_ident_path(&ctx, &mut cur)?;
        let alias = if cur.eat(TokenKind::KwAs) {
            let alias_tok = cur
                .peek()
                .filter(|t| matches!(t.kind, TokenKind::IdentValue | TokenKind::IdentType))?;
            let alias = SmolStr::new(ctx.text(alias_tok)?);
            cur.bump();
            alias
        } else {
            // RD default: last path segment
            path.last()?.clone()
        };
        if !cur.at_end() {
            return None;
        }
        return Some(ImportDecl::ModuleAlias { span, path, alias });
    }
    None
}

fn parse_static_string(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<SmolStr> {
    cur.expect(TokenKind::StringStart)?;
    let text_tok = cur.expect(TokenKind::StringText)?;
    let s = SmolStr::new(ctx.text(text_tok)?);
    cur.expect(TokenKind::StringEnd)?;
    Some(s)
}

fn parse_import_brace_list(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<Vec<ImportItem>> {
    cur.expect(TokenKind::LBrace)?;
    let mut items = Vec::new();
    if cur.peek_kind() != Some(TokenKind::RBrace) {
        loop {
            let name_tok = cur.peek()?;
            if !matches!(name_tok.kind, TokenKind::IdentValue | TokenKind::IdentType) {
                return None;
            }
            let name = SmolStr::new(ctx.text(name_tok)?);
            let start = name_tok.start;
            cur.bump();
            let mut end = name_tok.start + name_tok.len;
            let alias = if cur.eat(TokenKind::KwAs) {
                let a = cur.peek()?;
                if !matches!(a.kind, TokenKind::IdentValue | TokenKind::IdentType) {
                    return None;
                }
                let alias = SmolStr::new(ctx.text(a)?);
                end = a.start + a.len;
                cur.bump();
                Some(alias)
            } else {
                None
            };
            items.push(ImportItem {
                span: ctx.span(start, end),
                name,
                alias,
            });
            if cur.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
    }
    cur.expect(TokenKind::RBrace)?;
    Some(items)
}

fn try_hand_lower_const(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    item: &SyntaxNode,
    file_id: u32,
) -> Option<ConstDecl> {
    let toks = item_tokens(tokens, item);
    let mut ctx = HandCtx::new(pool, source, file_id);
    let mut cur = Cursor::new(&toks);
    skip_leading_doc_comments(&mut cur);
    let attrs = parse_attributes(&mut ctx, &mut cur)?;
    let visibility = parse_visibility(&mut cur);
    cur.expect(TokenKind::KwConst)?;
    let name_tok = cur.peek()?;
    if !matches!(name_tok.kind, TokenKind::IdentValue | TokenKind::IdentType) {
        return None;
    }
    let name = SmolStr::new(ctx.text(name_tok)?);
    cur.bump();
    let ty = if cur.peek_kind().is_some_and(|k| k != TokenKind::Equal) {
        Some(parse_type(&mut ctx, &mut cur)?)
    } else {
        None
    };
    cur.expect(TokenKind::Equal)?;
    let value = try_hand_lower_expr(&mut ctx, &mut cur, 0)?;
    if !cur.at_end() {
        return None;
    }
    Some(ConstDecl {
        span: token_bounds_span(file_id, &toks)?,
        attrs: attrs.into(),
        visibility,
        name,
        ty,
        value,
    })
}

fn try_hand_lower_type_alias(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    item: &SyntaxNode,
    file_id: u32,
) -> Option<TypeAliasDecl> {
    let toks = item_tokens(tokens, item);
    let mut ctx = HandCtx::new(pool, source, file_id);
    let mut cur = Cursor::new(&toks);
    skip_leading_doc_comments(&mut cur);
    let attrs = parse_attributes(&mut ctx, &mut cur)?;
    let visibility = parse_visibility(&mut cur);
    cur.expect(TokenKind::KwType)?;
    let name_tok = cur.expect(TokenKind::IdentType).or_else(|| {
        cur.peek()
            .filter(|t| matches!(t.kind, TokenKind::IdentValue))
            .and_then(|_| cur.bump())
    })?;
    let name = SmolStr::new(ctx.text(name_tok)?);
    let generic_params = parse_generic_params(&mut ctx, &mut cur)?;
    cur.expect(TokenKind::Equal)?;
    let ty = parse_type(&mut ctx, &mut cur)?;
    if !cur.at_end() {
        return None;
    }
    Some(TypeAliasDecl {
        span: token_bounds_span(file_id, &toks)?,
        attrs: attrs.into(),
        visibility,
        name,
        generic_params,
        ty,
    })
}

fn try_hand_lower_interface(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    item: &SyntaxNode,
    file_id: u32,
) -> Option<InterfaceDecl> {
    let toks = item_tokens(tokens, item);
    let mut ctx = HandCtx::new(pool, source, file_id);
    let mut cur = Cursor::new(&toks);
    skip_leading_doc_comments(&mut cur);
    let attrs = parse_attributes(&mut ctx, &mut cur)?;
    let visibility = parse_visibility(&mut cur);
    cur.expect(TokenKind::KwInterface)?;
    let name_tok = cur
        .peek()
        .filter(|t| matches!(t.kind, TokenKind::IdentType | TokenKind::IdentValue))?;
    let name = SmolStr::new(ctx.text(name_tok)?);
    cur.bump();
    let generic_params = parse_generic_params(&mut ctx, &mut cur)?;
    let where_clause = parse_where_clause(&mut ctx, &mut cur)?;
    // Same dual-path contract as RD `parse_interface_decl`: bare `self` means `Self`.
    let self_receiver = super::func::synthetic_self_receiver(arandu_lexer::Span::new(0, 0, 0));
    cur.expect(TokenKind::LBrace)?;
    let mut members = Vec::new();
    while cur.peek_kind() != Some(TokenKind::RBrace) && !cur.at_end() {
        while cur.eat(TokenKind::Semicolon) {}
        if cur.peek_kind() == Some(TokenKind::RBrace) {
            break;
        }
        members.push(super::func::parse_func_signature_with_receiver(
            &mut ctx,
            &mut cur,
            Some(&self_receiver),
        )?);
        let _ = cur.eat(TokenKind::Semicolon);
    }
    cur.expect(TokenKind::RBrace)?;
    if !cur.at_end() {
        return None;
    }
    Some(InterfaceDecl {
        span: token_bounds_span(file_id, &toks)?,
        attrs: attrs.into(),
        visibility,
        name,
        generic_params,
        where_clause,
        members,
    })
}

fn try_hand_lower_extern(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    item: &SyntaxNode,
    file_id: u32,
) -> Option<ExternDecl> {
    let toks = item_tokens(tokens, item);
    let mut ctx = HandCtx::new(pool, source, file_id);
    let mut cur = Cursor::new(&toks);
    skip_leading_doc_comments(&mut cur);
    let attrs = parse_attributes(&mut ctx, &mut cur)?;
    cur.expect(TokenKind::KwExtern)?;
    let abi = parse_static_string(&mut ctx, &mut cur)?;
    cur.expect(TokenKind::LBrace)?;
    let mut members = Vec::new();
    while cur.peek_kind() != Some(TokenKind::RBrace) && !cur.at_end() {
        while cur.eat(TokenKind::Semicolon) {}
        if cur.peek_kind() == Some(TokenKind::RBrace) {
            break;
        }
        members.push(super::func::parse_func_signature(&mut ctx, &mut cur)?);
        let _ = cur.eat(TokenKind::Semicolon);
    }
    cur.expect(TokenKind::RBrace)?;
    if !cur.at_end() {
        return None;
    }
    // Span starts at `extern` (attrs attached separately), ends at last token.
    let start = toks
        .iter()
        .find(|t| matches!(t.kind, TokenKind::KwExtern))
        .map(|t| t.start)
        .or_else(|| toks.first().map(|t| t.start))?;
    let end = toks.last().map(|t| t.end())?;
    Some(ExternDecl {
        span: Span::new(file_id, start, end),
        attrs: attrs.into(),
        abi,
        members,
    })
}
