use super::{Lexer, Mark};
use crate::ident::is_bidi_control;
use crate::ident::is_digit;
use crate::ident::is_ident_start;
use crate::{LexError, LexErrorCode, Span, TokenKind};

#[derive(Clone, Copy)]
enum StringTerminator {
    Quote,
    TripleQuote,
}

impl<'a> Lexer<'a> {
    pub(super) fn lex_raw_string(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        self.bump_ascii(2);
        while !self.is_at_end() && self.peek() != Some('"') {
            if let Some(ch) = self.peek()
                && is_bidi_control(ch)
            {
                let err_start = self.mark();
                self.bump();
                return Err(self.error_from(
                    err_start,
                    LexErrorCode::BidiTrojanSource,
                    "unescaped bidirectional Unicode control character (Trojan Source, CWE-1307)",
                ));
            }
            self.bump();
        }
        if self.is_at_end() {
            return Err(self.error_from(
                start,
                LexErrorCode::UnterminatedRawString,
                "unterminated raw string literal",
            ));
        }
        self.bump();
        self.push_token(TokenKind::RawString, self.span_from(start), false);
        Ok(())
    }

    pub(super) fn lex_raw_multiline_string(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        self.bump_ascii(4);
        while !self.is_at_end() && !self.starts_with("\"\"\"") {
            if let Some(ch) = self.peek()
                && is_bidi_control(ch)
            {
                let err_start = self.mark();
                self.bump();
                return Err(self.error_from(
                    err_start,
                    LexErrorCode::BidiTrojanSource,
                    "unescaped bidirectional Unicode control character (Trojan Source, CWE-1307)",
                ));
            }
            self.bump();
        }
        if self.is_at_end() {
            return Err(self.error_from(
                start,
                LexErrorCode::UnterminatedRawString,
                "unterminated raw multiline string literal",
            ));
        }
        self.bump_ascii(3);
        self.push_token(TokenKind::RawString, self.span_from(start), false);
        Ok(())
    }

    pub(super) fn lex_string(&mut self, interpolation_mode: bool) -> Result<(), LexError> {
        let start = self.mark();
        self.bump();
        self.push_token(TokenKind::StringStart, self.span_from(start), false);
        self.lex_string_body(
            StringTerminator::Quote,
            interpolation_mode,
            LexErrorCode::UnterminatedString,
        )?;
        let end = self.mark();
        self.bump();
        self.push_token(TokenKind::StringEnd, self.span_from(end), false);
        Ok(())
    }

    pub(super) fn lex_multiline_string(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        self.bump_ascii(3);
        self.push_token(
            TokenKind::MultilineStringStart,
            self.span_from(start),
            false,
        );
        self.lex_string_body(
            StringTerminator::TripleQuote,
            true,
            LexErrorCode::UnterminatedMultilineString,
        )?;
        let end = self.mark();
        self.bump_ascii(3);
        self.push_token(TokenKind::MultilineStringEnd, self.span_from(end), false);
        Ok(())
    }

    fn lex_string_body(
        &mut self,
        terminator: StringTerminator,
        allow_newline: bool,
        unterminated_code: LexErrorCode,
    ) -> Result<(), LexError> {
        let mut text_start = self.mark();
        while !self.is_at_end() && !self.at_string_terminator(terminator) {
            if !allow_newline && matches!(self.peek(), Some('\n' | '\r')) {
                return Err(self.error_from(
                    text_start,
                    unterminated_code,
                    "unterminated string literal",
                ));
            }
            if let Some(ch) = self.peek()
                && is_bidi_control(ch)
            {
                let err_start = self.mark();
                self.bump();
                return Err(self.error_from(
                    err_start,
                    LexErrorCode::BidiTrojanSource,
                    "unescaped bidirectional Unicode control character (Trojan Source, CWE-1307)",
                ));
            }
            if self.starts_with("${") {
                self.flush_text(text_start, self.pos);
                let start = self.mark();
                self.bump_ascii(2);
                self.push_token(TokenKind::InterpStart, self.span_from(start), false);
                self.lex_interpolation()?;
                text_start = self.mark();
                continue;
            }
            if self.peek() == Some('$')
                && self
                    .source
                    .as_bytes()
                    .get(self.pos + 1)
                    .copied()
                    .is_some_and(|b| is_ident_start(b as char))
            {
                self.flush_text(text_start, self.pos);
                // Emit a synthetic InterpStart at the `$` position.
                let dollar_start = self.mark();
                self.bump_ascii(1); // consume `$`
                let interp_start_span = self.span_from(dollar_start);
                self.push_token(TokenKind::InterpStart, interp_start_span, false);
                // Lex the identifier that follows.
                self.lex_ident_or_keyword();
                // Emit a synthetic InterpEnd immediately after.
                let end_span = self.span_from(self.mark());
                self.push_token(TokenKind::InterpEnd, end_span, false);
                text_start = self.mark();
                continue;
            }
            if self.peek() == Some('\\') {
                self.flush_text(text_start, self.pos);
                self.lex_escape()?;
                text_start = self.mark();
                continue;
            }
            self.bump();
        }
        if self.is_at_end() {
            return Err(self.error_from(
                text_start,
                unterminated_code,
                "unterminated string literal",
            ));
        }
        self.flush_text(text_start, self.pos);
        Ok(())
    }

    fn at_string_terminator(&self, terminator: StringTerminator) -> bool {
        match terminator {
            StringTerminator::Quote => self.peek() == Some('"'),
            StringTerminator::TripleQuote => self.starts_with("\"\"\""),
        }
    }

    fn lex_interpolation(&mut self) -> Result<(), LexError> {
        let mut brace_depth = 0usize;
        while !self.is_at_end() {
            if self.peek() == Some('}') {
                if brace_depth == 0 {
                    break;
                }
                brace_depth -= 1;
                self.lex_operator_or_punctuation()?;
                continue;
            }
            let prev_pos = self.pos;
            self.skip_whitespace();
            if self.pos > prev_pos {
                continue;
            }
            if self.peek() == Some('"') {
                self.lex_string(true)?;
            } else if self.peek().is_some_and(is_digit) {
                self.lex_number()?;
            } else if self.peek().is_some_and(is_ident_start) {
                self.lex_ident_or_keyword();
            } else if self.starts_with("//") {
                self.skip_line_comment()?;
            } else if self.starts_with("/*") {
                self.skip_block_comment()?;
            } else {
                if self.peek() == Some('{') {
                    brace_depth += 1;
                }
                self.lex_operator_or_punctuation()?;
            }
        }
        if self.is_at_end() {
            return Err(LexError::new(
                LexErrorCode::UnclosedInterpolation,
                "expected `}` to close string interpolation",
                self.current_span(),
            ));
        }
        let end = self.mark();
        self.bump();
        self.push_token(TokenKind::InterpEnd, self.span_from(end), false);
        Ok(())
    }

    fn lex_escape(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        self.consume_escape_sequence(start)?;
        self.push_token(TokenKind::StringEscape, self.span_from(start), false);
        Ok(())
    }

    fn consume_escape_sequence(&mut self, start: Mark) -> Result<(), LexError> {
        self.bump();
        match self.peek() {
            Some('n' | 't' | 'r' | '0' | '\\' | '"' | '\'' | '$') => {
                self.bump();
            }
            Some('u') => {
                self.bump();
                if self.peek() != Some('{') {
                    return Err(self.error_from(
                        start,
                        LexErrorCode::InvalidUnicodeEscape,
                        "invalid unicode escape",
                    ));
                }
                self.bump();
                let mut digits = 0;
                while self.peek().is_some_and(|ch| ch.is_ascii_hexdigit()) {
                    digits += 1;
                    self.bump();
                }
                if digits == 0 || self.peek() != Some('}') {
                    return Err(self.error_from(
                        start,
                        LexErrorCode::InvalidUnicodeEscape,
                        "invalid unicode escape",
                    ));
                }
                self.bump(); // }
                let code_str = self.slice_from(start.pos);
                let hex = &code_str[3..code_str.len() - 1];
                let value = u32::from_str_radix(hex, 16).unwrap_or(u32::MAX);
                if char::from_u32(value).is_none() {
                    return Err(self.error_from(
                        start,
                        LexErrorCode::InvalidUnicodeEscape,
                        "unicode escape must be a valid Unicode scalar value",
                    ));
                }
            }
            _ => {
                return Err(self.error_from(
                    start,
                    LexErrorCode::InvalidEscape,
                    "invalid escape sequence",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn lex_char(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        self.bump();
        if self.peek() == Some('\'') {
            return Err(self.error_from(start, LexErrorCode::EmptyChar, "empty char literal"));
        }
        let mut count = 0;
        if self.peek() == Some('\\') {
            let escape_start = self.mark();
            self.consume_escape_sequence(escape_start)?;
            count = 1;
        } else {
            while !self.is_at_end() && self.peek() != Some('\'') {
                if matches!(self.peek(), Some('\n' | '\r')) {
                    return Err(self.error_from(
                        start,
                        LexErrorCode::UnterminatedChar,
                        "unterminated char literal",
                    ));
                }
                if let Some(ch) = self.peek()
                    && is_bidi_control(ch)
                {
                    let err_start = self.mark();
                    self.bump();
                    return Err(self.error_from(
                        err_start,
                        LexErrorCode::BidiTrojanSource,
                        "unescaped bidirectional Unicode control character (Trojan Source, CWE-1307)",
                    ));
                }
                count += 1;
                self.bump();
            }
        }
        if self.is_at_end() || self.peek() != Some('\'') {
            return Err(self.error_from(
                start,
                LexErrorCode::UnterminatedChar,
                "unterminated char literal",
            ));
        }
        if count > 1 {
            return Err(self.error_from(
                start,
                LexErrorCode::CharTooLong,
                "char literal contains more than one codepoint",
            ));
        }
        self.bump();
        self.push_token(TokenKind::Char, self.span_from(start), false);
        Ok(())
    }

    fn flush_text(&mut self, start: Mark, end_pos: usize) {
        if end_pos <= start.pos {
            return;
        }
        let span = Span::new(0, start.pos as u32, end_pos as u32);
        self.push_token(TokenKind::StringText, span, false);
    }
}
