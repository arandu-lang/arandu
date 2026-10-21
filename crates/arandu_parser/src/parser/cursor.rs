use super::{
    ParseError, ParseErrorCode, Parser, is_contextual_module_segment, token_expectation_names,
    user_facing_token_name,
};
use crate::Expr;
use arandu_lexer::{Token, TokenKind};
use smol_str::SmolStr;

impl<'a> Parser<'a> {
    /// Statement terminator: explicit `;`, newline ASI (already a `SEMICOLON` token),
    /// or omitted before `}` / EOF (one-line bodies like `return 1 }`).
    pub(super) fn expect_semicolon(&mut self) -> Result<(), ParseError> {
        if self.at_kind_name("SEMICOLON") {
            self.advance();
            return Ok(());
        }
        // Optional terminator before block/file end (does not affect if-expr blocks:
        // those use expression parsing, not `expect_semicolon`).
        if self.at_kind_name("RBRACE") || self.at_kind_name("EOF") {
            return Ok(());
        }
        self.expect_name("SEMICOLON")
    }

    pub(super) fn expect_ident_value(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::IdentValue => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected value identifier",
                self.current(),
                self.file_id,
                self.source,
                &["value identifier"],
            )),
        }
    }

    pub(super) fn expect_ident_type(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::IdentType | TokenKind::TypeErr => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected type identifier",
                self.current(),
                self.file_id,
                self.source,
                &["type identifier"],
            )),
        }
    }

    pub(super) fn expect_member_name(&mut self) -> Result<SmolStr, ParseError> {
        if self.current().kind.is_contextual_member_name() {
            let name = SmolStr::new(self.current_text());
            self.advance();
            Ok(name)
        } else {
            Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected member name",
                self.current(),
                self.file_id,
                self.source,
                &["member name"],
            ))
        }
    }

    pub(super) fn expect_name_like(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::TypeErr | TokenKind::IdentValue | TokenKind::IdentType => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            kind if is_contextual_module_segment(kind) => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected identifier",
                self.current(),
                self.file_id,
                self.source,
                &["identifier"],
            )),
        }
    }

    pub(super) fn expect_module_segment(&mut self) -> Result<SmolStr, ParseError> {
        match &self.current().kind {
            TokenKind::IdentValue => {
                let name = SmolStr::new(self.current_text());
                self.advance();
                Ok(name)
            }
            kind if is_contextual_module_segment(kind) => {
                let text = SmolStr::new(self.current_text());
                self.advance();
                Ok(text)
            }
            _ => Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                "expected module path segment",
                self.current(),
                self.file_id,
                self.source,
                &["module path segment"],
            )),
        }
    }

    pub(super) fn expect_name(&mut self, name: &str) -> Result<(), ParseError> {
        if self.at_kind_name(name) {
            self.advance();
            return Ok(());
        }
        Err(ParseError::expected(
            ParseErrorCode::ExpectedToken,
            format!("expected {}", user_facing_token_name(name)),
            self.current(),
            self.file_id,
            self.source,
            token_expectation_names(name),
        ))
    }

    pub(super) fn eat_name(&mut self, name: &str) -> bool {
        if self.at_kind_name(name) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(super) fn at_gt(&self) -> bool {
        matches!(self.current().kind, TokenKind::Gt | TokenKind::ShiftRight)
    }

    pub(super) fn eat_gt(&mut self) -> bool {
        if let Some(split) = self.split_gt.take() {
            self.suppression_window += 1;
            self.emit_token_event(&split);
            if self.pos < self.tokens.len() - 1 {
                self.pos += 1;
            }
            return true;
        }
        if self.tokens[self.pos].kind == TokenKind::ShiftRight {
            self.suppression_window += 1;
            let tok = self.tokens[self.pos];
            let first = Token {
                kind: TokenKind::Gt,
                start: tok.start,
                len: 1,
                inserted: false,
            };
            let second = Token {
                kind: TokenKind::Gt,
                start: tok.start + 1,
                len: 1,
                inserted: false,
            };
            self.emit_token_event(&first);
            self.split_gt = Some(second);
            return true;
        }
        if self.tokens[self.pos].kind == TokenKind::Gt {
            self.advance();
            return true;
        }
        false
    }

    pub(super) fn expect_gt(&mut self) -> Result<(), ParseError> {
        if self.eat_gt() {
            return Ok(());
        }
        Err(ParseError::expected(
            ParseErrorCode::ExpectedToken,
            format!("expected {}", user_facing_token_name("GT")),
            self.current(),
            self.file_id,
            self.source,
            token_expectation_names("GT"),
        ))
    }

    pub(super) fn expect_kind(&mut self, kind: TokenKind) -> Result<(), ParseError> {
        if self.current().kind == kind {
            self.advance();
            Ok(())
        } else {
            let name = kind.name();
            Err(ParseError::expected(
                ParseErrorCode::ExpectedToken,
                format!("expected {}", user_facing_token_name(name)),
                self.current(),
                self.file_id,
                self.source,
                token_expectation_names(name),
            ))
        }
    }

    pub(super) fn eat_kind(&mut self, kind: TokenKind) -> bool {
        if self.current().kind == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(super) fn at_kind_name(&self, name: &str) -> bool {
        self.current().kind.name() == name
    }

    /// Soft keywords lex as plain identifiers but keep a keyword meaning in
    /// specific positions (e.g. `from` inside import syntax).
    pub(super) fn at_soft_keyword(&self, word: &str) -> bool {
        self.current().kind == TokenKind::IdentValue && self.token_text(self.current()) == word
    }

    pub(super) fn current(&self) -> &Token {
        if let Some(ref tok) = self.split_gt {
            tok
        } else {
            &self.tokens[self.pos]
        }
    }

    pub(super) fn token_text(&self, token: &Token) -> &str {
        token.lexeme(self.source)
    }

    pub(super) fn current_text(&self) -> &str {
        self.token_text(self.current())
    }

    pub(super) fn previous(&self) -> &Token {
        &self.tokens[self.pos - 1]
    }

    pub(super) fn advance(&mut self) -> &Token {
        self.advance_raw()
    }

    pub(super) fn advance_raw(&mut self) -> &Token {
        self.suppression_window += 1;
        if let Some(split) = self.split_gt.take() {
            self.emit_token_event(&split);
            if self.pos < self.tokens.len() - 1 {
                self.pos += 1;
            }
            return if self.pos > 0 {
                &self.tokens[self.pos - 1]
            } else {
                &self.tokens[0]
            };
        }
        let idx = self.pos;
        let token = self.tokens[idx];
        self.emit_token_event(&token);
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        &self.tokens[idx]
    }

    pub(crate) fn report_error(&mut self, err: ParseError) {
        if self.suppression_window >= 3 {
            self.diagnostics.push(err);
        }
        self.suppression_window = 0;
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn synchronize_top_level(&mut self) {
        if matches!(
            self.current().kind,
            TokenKind::KwFunc
                | TokenKind::KwStruct
                | TokenKind::KwEnum
                | TokenKind::KwInterface
                | TokenKind::KwImpl
                | TokenKind::KwExtern
                | TokenKind::KwType
                | TokenKind::KwConst
                | TokenKind::KwImport
                | TokenKind::KwFrom
                | TokenKind::KwModule
        ) {
            return;
        }
        self.advance_raw();
        while !self.at_kind_name("EOF") {
            if matches!(
                self.current().kind,
                TokenKind::KwFunc
                    | TokenKind::KwStruct
                    | TokenKind::KwEnum
                    | TokenKind::KwInterface
                    | TokenKind::KwImpl
                    | TokenKind::KwExtern
                    | TokenKind::KwType
                    | TokenKind::KwConst
                    | TokenKind::KwImport
                    | TokenKind::KwFrom
                    | TokenKind::KwModule
            ) {
                break;
            }
            self.advance_raw();
        }
    }

    #[cold]
    #[inline(never)]
    pub(super) fn synchronize_stmt(&mut self) {
        if matches!(
            self.current().kind,
            TokenKind::RBrace
                | TokenKind::KwLet
                | TokenKind::KwReturn
                | TokenKind::KwIf
                | TokenKind::KwFor
                | TokenKind::KwWhile
                | TokenKind::KwMatch
                | TokenKind::KwBreak
                | TokenKind::KwContinue
                | TokenKind::KwDefer
                | TokenKind::KwErrdefer
        ) {
            return;
        }
        self.advance_raw();
        while !self.at_kind_name("EOF") {
            if self.previous().kind == TokenKind::Semicolon {
                break;
            }
            if matches!(
                self.current().kind,
                TokenKind::RBrace
                    | TokenKind::KwLet
                    | TokenKind::KwReturn
                    | TokenKind::KwIf
                    | TokenKind::KwFor
                    | TokenKind::KwWhile
                    | TokenKind::KwMatch
                    | TokenKind::KwBreak
                    | TokenKind::KwContinue
                    | TokenKind::KwDefer
                    | TokenKind::KwErrdefer
            ) {
                break;
            }
            self.advance_raw();
        }
    }

    pub(super) fn parse_expr_without_block_calls(
        &mut self,
        min_bp: u8,
    ) -> Result<Expr, ParseError> {
        let previous = self.allow_block_calls;
        self.allow_block_calls = false;
        let result = self.parse_expr(min_bp);
        self.allow_block_calls = previous;
        result
    }
}
