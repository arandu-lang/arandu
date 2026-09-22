//! Shared token cursor and context for hand-lower.

use crate::ast::ast_pool::AstPool;
use arandu_lexer::{Span, Token, TokenKind};

/// Mutable AST builder context shared across hand-lower helpers.
pub struct HandCtx<'a> {
    pub pool: &'a mut AstPool,
    pub source: &'a str,
    pub file_id: u32,
    pub depth: u32,
}

impl<'a> HandCtx<'a> {
    #[must_use]
    pub fn new(pool: &'a mut AstPool, source: &'a str, file_id: u32) -> Self {
        Self {
            pool,
            source,
            file_id,
            depth: 0,
        }
    }

    #[inline]
    #[must_use]
    pub fn span(&self, start: u32, end: u32) -> Span {
        Span::new(self.file_id, start, end)
    }

    #[inline]
    #[must_use]
    pub fn token_span(&self, t: &Token) -> Span {
        token_span(self.file_id, t)
    }

    #[inline]
    #[must_use]
    pub fn text<'s>(&'s self, t: &Token) -> Option<&'s str> {
        token_text(self.source, t)
    }
}

/// Non-EOF tokens inside `[start, end)`, dropping inserted ASI semis.
#[must_use]
pub fn tokens_in_range(tokens: &[Token], start: u32, end: u32) -> Vec<&Token> {
    tokens
        .iter()
        .filter(|t| {
            !matches!(t.kind, TokenKind::Eof)
                && t.start >= start
                && t.start < end
                && !(t.kind == TokenKind::Semicolon && t.inserted)
        })
        .collect()
}

/// Tokens for a green STMT node, trimmed of overspill (leading semis, outer `}`).
#[must_use]
pub fn stmt_tokens(tokens: &[Token], start: u32, end: u32) -> Vec<&Token> {
    let mut toks = tokens_in_range(tokens, start, end);
    trim_stmt_token_slice(&mut toks);
    toks
}

/// Drop leading semis and trailing closers that belong to outer constructs.
pub fn trim_stmt_token_slice(toks: &mut Vec<&Token>) {
    let start = toks
        .iter()
        .position(|t| !matches!(t.kind, TokenKind::Semicolon))
        .unwrap_or(toks.len());
    toks.drain(..start);
    // Drop trailing tokens once delimiter depth returns to 0 and we see an outer closer.
    let mut depth: i32 = 0;
    let mut cut = toks.len();
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
            TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                if depth == 0 {
                    cut = i;
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    toks.truncate(cut);
    drop_trailing_semi(toks);
}

#[inline]
#[must_use]
pub fn token_text<'a>(source: &'a str, t: &Token) -> Option<&'a str> {
    let ts = t.start as usize;
    let te = t.start.saturating_add(t.len) as usize;
    source.get(ts..te.min(source.len()))
}

#[inline]
#[must_use]
pub fn token_span(file_id: u32, t: &Token) -> Span {
    Span::new(file_id, t.start, t.end())
}

/// Lightweight cursor over a token slice.
#[derive(Clone, Copy)]
pub struct Cursor<'a> {
    toks: &'a [&'a Token],
    pos: usize,
    split_gt: Option<(u32, u32)>,
}

impl<'a> Cursor<'a> {
    #[must_use]
    pub fn new(toks: &'a [&'a Token]) -> Self {
        Self {
            toks,
            pos: 0,
            split_gt: None,
        }
    }

    #[must_use]
    pub fn pos(self) -> usize {
        self.pos
    }

    #[must_use]
    pub fn remaining(self) -> &'a [&'a Token] {
        &self.toks[self.pos..]
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.pos >= self.toks.len()
    }

    #[must_use]
    pub fn at_end(self) -> bool {
        self.is_empty()
    }

    #[must_use]
    pub fn peek(self) -> Option<&'a Token> {
        self.toks.get(self.pos).copied()
    }

    #[must_use]
    pub fn peek_kind(self) -> Option<TokenKind> {
        self.peek().map(|t| t.kind)
    }

    #[must_use]
    pub fn peek_at(self, offset: usize) -> Option<&'a Token> {
        self.toks.get(self.pos + offset).copied()
    }

    pub fn bump(&mut self) -> Option<&'a Token> {
        let t = self.peek()?;
        self.pos += 1;
        Some(t)
    }

    pub fn eat(&mut self, kind: TokenKind) -> bool {
        if self.peek_kind() == Some(kind) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn expect(&mut self, kind: TokenKind) -> Option<&'a Token> {
        let t = self.peek()?;
        if t.kind == kind {
            self.pos += 1;
            Some(t)
        } else {
            None
        }
    }

    #[must_use]
    pub fn at_gt(&self) -> bool {
        self.split_gt.is_some()
            || self.peek_kind() == Some(TokenKind::Gt)
            || self.peek_kind() == Some(TokenKind::ShiftRight)
    }

    pub fn expect_gt(&mut self) -> Option<(u32, u32)> {
        if let Some(span) = self.split_gt.take() {
            return Some(span);
        }
        let t = self.peek()?;
        if t.kind == TokenKind::Gt {
            self.pos += 1;
            Some((t.start, t.len))
        } else if t.kind == TokenKind::ShiftRight {
            self.pos += 1;
            self.split_gt = Some((t.start + 1, 1));
            Some((t.start, 1))
        } else {
            None
        }
    }

    /// Skip explicit semis.
    pub fn skip_semis(&mut self) {
        while self.eat(TokenKind::Semicolon) {}
    }
}

/// Drop trailing semicolon from a owned token vec.
pub fn drop_trailing_semi(toks: &mut Vec<&Token>) {
    if toks
        .last()
        .is_some_and(|t| matches!(t.kind, TokenKind::Semicolon))
    {
        toks.pop();
    }
}

/// Map set-operator token → AST op.
#[must_use]
pub fn set_op_from_token(kind: TokenKind) -> Option<crate::SetOp> {
    use crate::SetOp;
    match kind {
        TokenKind::Equal => Some(SetOp::Assign),
        TokenKind::PlusEqual => Some(SetOp::AddAssign),
        TokenKind::MinusEqual => Some(SetOp::SubAssign),
        TokenKind::StarEqual => Some(SetOp::MulAssign),
        TokenKind::SlashEqual => Some(SetOp::DivAssign),
        TokenKind::PercentEqual => Some(SetOp::ModAssign),
        TokenKind::AmpEqual => Some(SetOp::BitAndAssign),
        TokenKind::PipeEqual => Some(SetOp::BitOrAssign),
        TokenKind::CaretEqual => Some(SetOp::BitXorAssign),
        TokenKind::ShiftLeftEqual => Some(SetOp::ShiftLeftAssign),
        TokenKind::ShiftRightEqual => Some(SetOp::ShiftRightAssign),
        _ => None,
    }
}

/// Binary op binding powers (left, right) — same scale as RD `BINARY_OP_TABLE`.
/// Catch uses left_bp=10 (looser than all of these).
#[must_use]
pub fn bin_bp(kind: TokenKind) -> Option<(u8, u8, crate::BinaryOp)> {
    use crate::BinaryOp;
    // RD uses (bp, bp+1) as (left, right).
    let (op, bp) = match kind {
        TokenKind::NullCoalesce => (BinaryOp::NullCoalesce, 20),
        TokenKind::LogicalOr => (BinaryOp::Or, 30),
        TokenKind::LogicalAnd => (BinaryOp::And, 40),
        TokenKind::EqualEqual => (BinaryOp::Equal, 50),
        TokenKind::BangEqual => (BinaryOp::NotEqual, 50),
        TokenKind::Lt => (BinaryOp::Lt, 60),
        TokenKind::Gt => (BinaryOp::Gt, 60),
        TokenKind::LtEqual => (BinaryOp::LtEqual, 60),
        TokenKind::GtEqual => (BinaryOp::GtEqual, 60),
        TokenKind::RangeExclusive => (BinaryOp::RangeExclusive, 70),
        TokenKind::RangeInclusive => (BinaryOp::RangeInclusive, 70),
        TokenKind::Pipe => (BinaryOp::BitOr, 80),
        TokenKind::Caret => (BinaryOp::BitXor, 90),
        TokenKind::Amp => (BinaryOp::BitAnd, 100),
        TokenKind::ShiftLeft => (BinaryOp::ShiftLeft, 110),
        TokenKind::ShiftRight => (BinaryOp::ShiftRight, 110),
        TokenKind::Plus => (BinaryOp::Add, 120),
        TokenKind::Minus => (BinaryOp::Sub, 120),
        TokenKind::Star => (BinaryOp::Mul, 130),
        TokenKind::Slash => (BinaryOp::Div, 130),
        TokenKind::Percent => (BinaryOp::Mod, 130),
        _ => return None,
    };
    Some((bp, bp + 1, op))
}
