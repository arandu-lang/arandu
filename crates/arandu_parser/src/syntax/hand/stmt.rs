//! Hand-lower statements and blocks (token-first, green-optional).

use super::cursor::{Cursor, HandCtx, set_op_from_token, stmt_tokens};
use super::expr::try_hand_lower_expr;
use super::pattern::parse_pattern;
use super::ty::parse_type;
use crate::ast::ast_pool::{AstPool, StmtId};
use crate::syntax::kind::SyntaxNode;
use crate::{
    BindingItem, Block, Condition, DeferBody, ForBinding, ForClause, Place, PlaceSuffix,
    SimpleStmt, Stmt,
};
use arandu_lexer::{Token, TokenKind};
use smol_str::SmolStr;

/// Hand-lower a green `STMT` without RD.
#[must_use]
pub fn try_hand_lower_stmt(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    stmt: &SyntaxNode,
    file_id: u32,
) -> Option<StmtId> {
    let r = stmt.text_range();
    let toks = stmt_tokens(tokens, u32::from(r.start()), u32::from(r.end()));
    if toks.is_empty() {
        return None;
    }
    let mut ctx = HandCtx::new(pool, source, file_id);
    let mut cur = Cursor::new(&toks);
    let id = parse_stmt_tokens(&mut ctx, &mut cur)?;
    cur.skip_semis();
    if !cur.at_end() {
        return None;
    }
    Some(id)
}

/// Hand-lower every direct `STMT` of a green `BLOCK` (requires real `}`).
#[must_use]
pub fn try_hand_lower_block(
    pool: &mut AstPool,
    source: &str,
    tokens: &[Token],
    block: &SyntaxNode,
    file_id: u32,
) -> Option<Block> {
    let r = block.text_range();
    let bs = u32::from(r.start());
    let be = u32::from(r.end());
    let has_rbrace = tokens.iter().any(|t| {
        matches!(t.kind, TokenKind::RBrace) && !t.inserted && t.start > bs && t.start <= be
    });
    if !has_rbrace {
        return None;
    }

    // Token-first over the block span (robust to bad green STMT ranges).
    let mut toks: Vec<&Token> = tokens
        .iter()
        .filter(|t| {
            !matches!(t.kind, TokenKind::Eof)
                && t.start >= bs
                && t.start <= be
                && !(t.kind == TokenKind::Semicolon && t.inserted)
        })
        .collect();
    // Include closing RBrace if end-exclusive green range missed it.
    if !toks.iter().any(|t| matches!(t.kind, TokenKind::RBrace))
        && let Some(rb) = tokens
            .iter()
            .find(|t| matches!(t.kind, TokenKind::RBrace) && !t.inserted && t.start >= bs)
    {
        toks.push(rb);
    }
    let mut ctx = HandCtx::new(pool, source, file_id);
    let mut cur = Cursor::new(&toks);
    parse_block_tokens(&mut ctx, &mut cur)
}

/// Parse `{ ... }` from the cursor (token-first).
pub fn parse_block_tokens(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<Block> {
    let open = cur.expect(TokenKind::LBrace)?;
    let start = open.start;
    let mut statements = Vec::new();
    loop {
        cur.skip_semis();
        if cur.peek_kind() == Some(TokenKind::RBrace) {
            let close = cur.bump()?;
            return Some(Block {
                span: ctx.span(start, close.start + close.len),
                statements: ctx.pool.alloc_stmt_list(&statements),
            });
        }
        if cur.at_end() {
            return None;
        }
        statements.push(parse_stmt_tokens(ctx, cur)?);
    }
}

/// Parse one statement from the cursor (does not require trailing `;`).
pub fn parse_stmt_tokens(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<StmtId> {
    let start = cur.peek()?.start;
    match cur.peek_kind()? {
        TokenKind::KwBreak => {
            let t = cur.bump()?;
            let _ = cur.eat(TokenKind::Semicolon);
            Some(ctx.pool.alloc_stmt(Stmt::Break {
                span: ctx.token_span(t),
            }))
        }
        TokenKind::KwContinue => {
            let t = cur.bump()?;
            let _ = cur.eat(TokenKind::Semicolon);
            Some(ctx.pool.alloc_stmt(Stmt::Continue {
                span: ctx.token_span(t),
            }))
        }
        TokenKind::KwReturn => lower_return(ctx, cur, start),
        TokenKind::KwFree => {
            cur.bump();
            let expr = try_hand_lower_expr(ctx, cur, 0)?;
            let _ = cur.eat(TokenKind::Semicolon);
            let end = ctx.pool.expr_span(expr).end;
            Some(ctx.pool.alloc_stmt(Stmt::Free {
                span: ctx.span(start, end),
                expr,
            }))
        }
        TokenKind::KwLet => lower_let(ctx, cur, start),
        TokenKind::KwIf => lower_if(ctx, cur, start),
        TokenKind::KwWhile => lower_while(ctx, cur, start),
        TokenKind::KwFor => lower_for(ctx, cur, start),
        TokenKind::KwDefer => lower_defer(ctx, cur, start, false),
        TokenKind::KwErrdefer => lower_defer(ctx, cur, start, true),
        TokenKind::KwUnsafe => {
            cur.bump();
            let block = parse_block_tokens(ctx, cur)?;
            let end = block.span.end;
            Some(ctx.pool.alloc_stmt(Stmt::Unsafe {
                span: ctx.span(start, end),
                block,
            }))
        }
        TokenKind::KwSet => lower_set(ctx, cur, start, true),
        TokenKind::KwMatch => {
            let expr = try_hand_lower_expr(ctx, cur, 0)?;
            let _ = cur.eat(TokenKind::Semicolon);
            let end = ctx.pool.expr_span(expr).end;
            Some(ctx.pool.alloc_stmt(Stmt::Match {
                span: ctx.span(start, end),
                expr,
            }))
        }
        TokenKind::IdentValue | TokenKind::KwSelf => lower_ident_stmt(ctx, cur, start),
        _ => {
            // expr stmt
            let expr = try_hand_lower_expr(ctx, cur, 0)?;
            let has_semi = cur.eat(TokenKind::Semicolon);
            let end = ctx.pool.expr_span(expr).end;
            let span = ctx.span(start, end);
            if matches!(
                ctx.pool.expr(expr),
                crate::ast::ast_pool::ExprKind::Match { .. }
            ) {
                Some(ctx.pool.alloc_stmt(Stmt::Match { span, expr }))
            } else {
                Some(ctx.pool.alloc_stmt(Stmt::Expr {
                    span,
                    expr,
                    has_semi,
                }))
            }
        }
    }
}

fn lower_ident_stmt(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<StmtId> {
    // Lookahead: place [, place]* set_op  → assignment (incl. multi-place).
    let toks = cur.remaining();
    let mut probe = Cursor::new(toks);
    if parse_place(ctx, &mut probe).is_some() {
        while probe.eat(TokenKind::Comma) {
            if parse_place(ctx, &mut probe).is_none() {
                break;
            }
        }
        if probe
            .peek_kind()
            .is_some_and(|k| set_op_from_token(k).is_some())
        {
            return try_lower_set(ctx, cur, start, false);
        }
    }
    let expr = try_hand_lower_expr(ctx, cur, 0)?;
    let has_semi = cur.eat(TokenKind::Semicolon);
    let end = ctx.pool.expr_span(expr).end;
    Some(ctx.pool.alloc_stmt(Stmt::Expr {
        span: ctx.span(start, end),
        expr,
        has_semi,
    }))
}

fn lower_return(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<StmtId> {
    cur.expect(TokenKind::KwReturn)?;
    let values = if matches!(
        cur.peek_kind(),
        None | Some(TokenKind::Semicolon) | Some(TokenKind::RBrace)
    ) {
        Vec::new()
    } else {
        let mut values = vec![try_hand_lower_expr(ctx, cur, 0)?];
        while cur.eat(TokenKind::Comma) {
            values.push(try_hand_lower_expr(ctx, cur, 0)?);
        }
        values
    };
    let _ = cur.eat(TokenKind::Semicolon);
    let end = values
        .last()
        .map(|e| ctx.pool.expr_span(*e).end)
        .unwrap_or(start + 6);
    Some(ctx.pool.alloc_stmt(Stmt::Return {
        span: ctx.span(start, end),
        values,
    }))
}

fn lower_let(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<StmtId> {
    cur.expect(TokenKind::KwLet)?;
    let mut bindings = Vec::new();
    loop {
        let bind_start_tok = cur.peek()?;
        let mutable = cur.eat(TokenKind::KwMut);
        let name_tok = cur.expect(TokenKind::IdentValue)?;
        let name = SmolStr::new(ctx.text(name_tok)?);
        let bind_start = if mutable {
            bind_start_tok.start
        } else {
            name_tok.start
        };
        let ty = if cur.eat(TokenKind::Colon) {
            Some(parse_type(ctx, cur)?)
        } else {
            None
        };
        let bind_end = ty
            .map(|id| ctx.pool.type_expr_span(id).end)
            .unwrap_or(name_tok.start + name_tok.len);
        bindings.push(BindingItem {
            span: ctx.span(bind_start, bind_end),
            mutable,
            name,
            ty,
        });
        if cur.eat(TokenKind::Comma) {
            continue;
        }
        break;
    }
    cur.expect(TokenKind::Equal)?;
    let value = try_hand_lower_expr(ctx, cur, 0)?;
    let _ = cur.eat(TokenKind::Semicolon);
    let end = ctx.pool.expr_span(value).end;
    Some(ctx.pool.alloc_stmt(Stmt::VarDecl {
        span: ctx.span(start, end),
        bindings,
        value,
    }))
}

fn try_lower_set(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
    explicit_set: bool,
) -> Option<StmtId> {
    lower_set(ctx, cur, start, explicit_set)
}

fn lower_set(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
    explicit_set: bool,
) -> Option<StmtId> {
    if explicit_set {
        cur.expect(TokenKind::KwSet)?;
    }
    let mut places = vec![parse_place(ctx, cur)?];
    while cur.eat(TokenKind::Comma) {
        places.push(parse_place(ctx, cur)?);
    }
    let op = set_op_from_token(cur.peek_kind()?)?;
    cur.bump();
    let value = try_hand_lower_expr(ctx, cur, 0)?;
    let _ = cur.eat(TokenKind::Semicolon);
    let end = ctx.pool.expr_span(value).end;
    Some(ctx.pool.alloc_stmt(Stmt::Set {
        span: ctx.span(start, end),
        places,
        op,
        value,
    }))
}

fn parse_place(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<Place> {
    let root_tok = cur.peek()?;
    let root = match root_tok.kind {
        TokenKind::KwSelf => {
            cur.bump();
            SmolStr::new_static("self")
        }
        TokenKind::IdentValue => {
            let n = SmolStr::new(ctx.text(root_tok)?);
            cur.bump();
            n
        }
        _ => return None,
    };
    let start = root_tok.start;
    let mut end = root_tok.start + root_tok.len;
    let mut suffixes = Vec::new();
    loop {
        if cur.eat(TokenKind::Dot) {
            let field_tok = cur.peek().filter(|t| t.kind.is_contextual_member_name())?;
            cur.bump();
            let name = SmolStr::new(ctx.text(field_tok)?);
            end = field_tok.start + field_tok.len;
            suffixes.push(PlaceSuffix::Field {
                span: ctx.token_span(field_tok),
                name,
            });
        } else if cur.eat(TokenKind::LBracket) {
            let idx_start = cur.peek().map(|t| t.start).unwrap_or(end);
            let expr = try_hand_lower_expr(ctx, cur, 0)?;
            let close = cur.expect(TokenKind::RBracket)?;
            end = close.start + close.len;
            suffixes.push(PlaceSuffix::Index {
                span: ctx.span(idx_start, end),
                expr,
            });
        } else {
            break;
        }
    }
    Some(Place {
        span: ctx.span(start, end),
        root,
        suffixes,
    })
}

fn parse_condition(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>) -> Option<Condition> {
    let toks = cur.remaining();
    let brace_at = find_depth0(toks, TokenKind::LBrace)?;
    if brace_at == 0 {
        return None;
    }
    let cond_toks = &toks[..brace_at];
    for _ in 0..brace_at {
        cur.bump();
    }
    let clauses = split_depth0_and(cond_toks)?;
    if clauses.len() > 1
        && clauses
            .iter()
            .any(|clause| find_depth0(clause, TokenKind::KwIs).is_some())
    {
        let mut conditions = Vec::with_capacity(clauses.len());
        for clause in clauses {
            conditions.push(parse_condition_atom(ctx, clause)?);
        }
        let span = condition_span(ctx, cond_toks)?;
        return Some(Condition::And {
            span,
            conditions: conditions.into_boxed_slice(),
        });
    }
    parse_condition_atom(ctx, cond_toks)
}

fn parse_condition_atom(ctx: &mut HandCtx<'_>, cond_toks: &[&Token]) -> Option<Condition> {
    let span = condition_span(ctx, cond_toks)?;
    let first = cond_toks.first()?;
    if matches!(first.kind, TokenKind::KwLet)
        && let Some(eq_idx) = cond_toks
            .iter()
            .position(|t| matches!(t.kind, TokenKind::Equal))
    {
        let mut pcur = Cursor::new(&cond_toks[1..eq_idx]);
        let pattern = parse_pattern(ctx, &mut pcur)?;
        if !pcur.at_end() {
            return None;
        }
        let mut ecur = Cursor::new(&cond_toks[eq_idx + 1..]);
        let expr = try_hand_lower_expr(ctx, &mut ecur, 0)?;
        if !ecur.at_end() {
            return None;
        }
        return Some(Condition::Is {
            span,
            expr,
            pattern,
        });
    }
    if let Some(is_idx) = cond_toks
        .iter()
        .position(|t| matches!(t.kind, TokenKind::KwIs))
    {
        let mut ecur = Cursor::new(&cond_toks[..is_idx]);
        let expr = try_hand_lower_expr(ctx, &mut ecur, 0)?;
        if !ecur.at_end() {
            return None;
        }
        let mut pcur = Cursor::new(&cond_toks[is_idx + 1..]);
        let pattern = parse_pattern(ctx, &mut pcur)?;
        if !pcur.at_end() {
            return None;
        }
        return Some(Condition::Is {
            span,
            expr,
            pattern,
        });
    }
    let mut ccur = Cursor::new(cond_toks);
    let expr = try_hand_lower_expr(ctx, &mut ccur, 0)?;
    if !ccur.at_end() {
        return None;
    }
    Some(Condition::Expr {
        span: ctx.pool.expr_span(expr),
        expr,
    })
}

fn condition_span(ctx: &HandCtx<'_>, toks: &[&Token]) -> Option<arandu_lexer::Span> {
    let first = toks.first()?;
    let last = toks.last()?;
    Some(ctx.span(first.start, last.start + last.len))
}

fn split_depth0_and<'a>(toks: &'a [&'a Token]) -> Option<Vec<&'a [&'a Token]>> {
    let mut clauses = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (index, token) in toks.iter().enumerate() {
        if depth == 0 && token.kind == TokenKind::LogicalAnd {
            if start == index {
                return None;
            }
            clauses.push(&toks[start..index]);
            start = index + 1;
            continue;
        }
        match token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    if start == toks.len() {
        return None;
    }
    clauses.push(&toks[start..]);
    Some(clauses)
}

fn find_depth0(toks: &[&Token], target: TokenKind) -> Option<usize> {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate() {
        if depth == 0 && t.kind == target {
            return Some(i);
        }
        match t.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    None
}

fn lower_if(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<StmtId> {
    cur.expect(TokenKind::KwIf)?;
    let condition = parse_condition(ctx, cur)?;
    let then_block = parse_block_tokens(ctx, cur)?;
    let else_block = if cur.eat(TokenKind::KwElse) {
        if cur.peek_kind() == Some(TokenKind::KwIf) {
            let nested = lower_if(ctx, cur, cur.peek()?.start)?;
            Some(Block {
                span: ctx.pool.stmt_span(nested),
                statements: ctx.pool.alloc_stmt_list(&[nested]),
            })
        } else {
            Some(parse_block_tokens(ctx, cur)?)
        }
    } else {
        None
    };
    let end = else_block
        .as_ref()
        .map(|b| b.span.end)
        .unwrap_or(then_block.span.end);
    Some(ctx.pool.alloc_stmt(Stmt::If {
        span: ctx.span(start, end),
        condition,
        then_block,
        else_block,
    }))
}

fn lower_while(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<StmtId> {
    cur.expect(TokenKind::KwWhile)?;
    let condition = parse_condition(ctx, cur)?;
    let body = parse_block_tokens(ctx, cur)?;
    let end = body.span.end;
    Some(ctx.pool.alloc_stmt(Stmt::While {
        span: ctx.span(start, end),
        condition,
        body,
    }))
}

fn lower_for(ctx: &mut HandCtx<'_>, cur: &mut Cursor<'_>, start: u32) -> Option<StmtId> {
    cur.expect(TokenKind::KwFor)?;
    let toks = cur.remaining();
    let brace_at = find_depth0(toks, TokenKind::LBrace)?;
    let head = &toks[..brace_at];
    let has_in = head.iter().any(|t| matches!(t.kind, TokenKind::KwIn));
    let clause = if has_in {
        let mut h = Cursor::new(head);
        let mut bindings = Vec::new();
        loop {
            let mutable = h.eat(TokenKind::KwMut);
            let name_tok = h.expect(TokenKind::IdentValue)?;
            let name = SmolStr::new(ctx.text(name_tok)?);
            bindings.push(ForBinding {
                span: ctx.token_span(name_tok),
                mutable,
                name,
            });
            if h.eat(TokenKind::Comma) {
                continue;
            }
            break;
        }
        h.expect(TokenKind::KwIn)?;
        let iterable = try_hand_lower_expr(ctx, &mut h, 0)?;
        if !h.at_end() {
            return None;
        }
        ForClause::In {
            span: ctx.span(head[0].start, ctx.pool.expr_span(iterable).end),
            bindings,
            iterable,
        }
    } else {
        let mut parts: Vec<&[&Token]> = Vec::new();
        let mut s = 0usize;
        for (i, t) in head.iter().enumerate() {
            if matches!(t.kind, TokenKind::Semicolon) {
                parts.push(&head[s..i]);
                s = i + 1;
            }
        }
        parts.push(&head[s..]);
        if parts.len() != 3 {
            return None;
        }
        let init = if parts[0].is_empty() {
            None
        } else {
            Some(lower_simple(ctx, parts[0])?)
        };
        let condition = if parts[1].is_empty() {
            None
        } else {
            let mut c = Cursor::new(parts[1]);
            let e = try_hand_lower_expr(ctx, &mut c, 0)?;
            if !c.at_end() {
                return None;
            }
            Some(e)
        };
        let step = if parts[2].is_empty() {
            None
        } else {
            Some(lower_simple(ctx, parts[2])?)
        };
        ForClause::CStyle {
            span: ctx.span(
                head.first().map(|t| t.start).unwrap_or(start),
                head.last().map(|t| t.end()).unwrap_or(start),
            ),
            init,
            condition,
            step,
        }
    };
    // advance past head
    for _ in 0..brace_at {
        cur.bump();
    }
    let body = parse_block_tokens(ctx, cur)?;
    let end = body.span.end;
    Some(ctx.pool.alloc_stmt(Stmt::For {
        span: ctx.span(start, end),
        clause,
        body,
    }))
}

fn lower_simple(ctx: &mut HandCtx<'_>, toks: &[&Token]) -> Option<SimpleStmt> {
    let mut cur = Cursor::new(toks);
    let start = cur.peek()?.start;
    if cur.peek_kind() == Some(TokenKind::KwLet) {
        let id = lower_let(ctx, &mut cur, start)?;
        if !cur.at_end() {
            return None;
        }
        let Stmt::VarDecl {
            span,
            bindings,
            value,
        } = ctx.pool.stmt(id)
        else {
            return None;
        };
        return Some(SimpleStmt::VarDecl {
            span: *span,
            bindings: bindings.clone(),
            value: *value,
        });
    }
    let explicit_set = matches!(cur.peek_kind(), Some(TokenKind::KwSet));
    if let Some(id) = try_lower_set(ctx, &mut cur, start, explicit_set)
        && cur.at_end()
        && let Stmt::Set {
            span,
            places,
            op,
            value,
        } = ctx.pool.stmt(id)
    {
        return Some(SimpleStmt::Set {
            span: *span,
            places: places.clone(),
            op: op.clone(),
            value: *value,
        });
    }
    let mut cur = Cursor::new(toks);
    let expr = try_hand_lower_expr(ctx, &mut cur, 0)?;
    if !cur.at_end() {
        return None;
    }
    Some(SimpleStmt::Expr {
        span: ctx.span(start, ctx.pool.expr_span(expr).end),
        expr,
    })
}

fn lower_defer(
    ctx: &mut HandCtx<'_>,
    cur: &mut Cursor<'_>,
    start: u32,
    is_err: bool,
) -> Option<StmtId> {
    if is_err {
        cur.expect(TokenKind::KwErrdefer)?;
    } else {
        cur.expect(TokenKind::KwDefer)?;
    }
    let body = if cur.peek_kind() == Some(TokenKind::LBrace) {
        let block = parse_block_tokens(ctx, cur)?;
        DeferBody::Block {
            span: block.span,
            block,
        }
    } else {
        let expr = try_hand_lower_expr(ctx, cur, 0)?;
        let _ = cur.eat(TokenKind::Semicolon);
        DeferBody::Expr {
            span: ctx.pool.expr_span(expr),
            expr,
        }
    };
    let end = match &body {
        DeferBody::Block { span, .. } | DeferBody::Expr { span, .. } => span.end,
    };
    Some(ctx.pool.alloc_stmt(if is_err {
        Stmt::ErrDefer {
            span: ctx.span(start, end),
            body,
        }
    } else {
        Stmt::Defer {
            span: ctx.span(start, end),
            body,
        }
    }))
}
