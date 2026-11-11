use super::{Lexer, Mark};
use crate::ident::is_digit;
use crate::ident::is_ident_start;
use crate::{LexError, LexErrorCode, TokenKind};

#[derive(Clone, Copy)]
enum Radix {
    Bin,
    Oct,
    Hex,
}

impl Radix {
    fn is_valid_digit(self, ch: char) -> bool {
        match self {
            Radix::Bin => matches!(ch, '0' | '1' | '_'),
            Radix::Oct => matches!(ch, '0'..='7' | '_'),
            Radix::Hex => ch.is_ascii_hexdigit() || ch == '_',
        }
    }

    fn token_kind(self) -> TokenKind {
        match self {
            Radix::Bin => TokenKind::IntBin,
            Radix::Oct => TokenKind::IntOct,
            Radix::Hex => TokenKind::IntHex,
        }
    }

    fn invalid_digit_code(self) -> LexErrorCode {
        match self {
            Radix::Bin => LexErrorCode::InvalidBinaryDigit,
            Radix::Oct => LexErrorCode::InvalidOctalDigit,
            Radix::Hex => LexErrorCode::InvalidHexDigit,
        }
    }
}

impl<'a> Lexer<'a> {
    pub(super) fn lex_number(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        if self.starts_with("0x") {
            return self.lex_radix_number(start, Radix::Hex);
        }
        if self.starts_with("0b") {
            return self.lex_radix_number(start, Radix::Bin);
        }
        if self.starts_with("0o") {
            return self.lex_radix_number(start, Radix::Oct);
        }

        self.bump_digits_or_underscores();
        let mut is_float = false;
        // `1.len()` and `t.1.len()`: a dot followed by a name is a member
        // access on the integer, not a decimal point.
        if self.peek() == Some('.')
            && self.peek_next() != Some('.')
            && !self.peek_next().is_some_and(is_ident_start)
        {
            is_float = true;
            self.bump();
            if !self.peek().is_some_and(is_digit) {
                return Err(self.error_from(
                    start,
                    LexErrorCode::InvalidNumericLiteral,
                    "expected digit after decimal point",
                ));
            }
            self.bump_digits_or_underscores();
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            is_float = true;
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            if !self.peek().is_some_and(is_digit) {
                return Err(self.error_from(
                    start,
                    LexErrorCode::InvalidNumericLiteral,
                    "expected digit in float exponent",
                ));
            }
            self.bump_digits_or_underscores();
        }

        let lexeme = self.slice_from(start.pos);
        if lexeme.ends_with('_') {
            return Err(self.error_from(
                start,
                LexErrorCode::InvalidNumericLiteral,
                "numeric literal cannot end with `_`",
            ));
        }
        if lexeme.contains("__") {
            return Err(self.error_from(
                start,
                LexErrorCode::InvalidNumericLiteral,
                "numeric literal cannot contain multiple consecutive underscores",
            ));
        }
        if !is_float && lexeme.len() > 1 && lexeme.starts_with('0') {
            return Err(self.error_from(
                start,
                LexErrorCode::LeadingZero,
                "decimal literals cannot have leading zeroes",
            ));
        }
        if self.peek().is_some_and(is_ident_start) {
            return Err(self.error_from(
                start,
                LexErrorCode::InvalidNumericLiteral,
                "numeric literal cannot be directly followed by an identifier",
            ));
        }

        let kind = if is_float {
            TokenKind::Float
        } else {
            TokenKind::IntDec
        };
        self.push_token(kind, self.span_from(start), false);
        Ok(())
    }

    fn lex_radix_number(&mut self, start: Mark, radix: Radix) -> Result<(), LexError> {
        self.bump_ascii(2);
        let digit_start = self.pos;
        while self
            .peek()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            let Some(ch) = self.peek() else {
                break;
            };
            if !radix.is_valid_digit(ch) {
                return Err(self.error_from(
                    start,
                    radix.invalid_digit_code(),
                    "invalid digit for numeric literal",
                ));
            }
            self.bump();
        }
        if self.pos == digit_start {
            return Err(self.error_from(
                start,
                LexErrorCode::InvalidNumericLiteral,
                "expected digit after numeric prefix",
            ));
        }
        let lexeme = self.slice_from(start.pos);
        if lexeme.ends_with('_') {
            return Err(self.error_from(
                start,
                LexErrorCode::InvalidNumericLiteral,
                "numeric literal cannot end with `_`",
            ));
        }
        if lexeme.contains("__") {
            return Err(self.error_from(
                start,
                LexErrorCode::InvalidNumericLiteral,
                "numeric literal cannot contain multiple consecutive underscores",
            ));
        }
        if self.peek().is_some_and(is_ident_start) {
            return Err(self.error_from(
                start,
                LexErrorCode::InvalidNumericLiteral,
                "numeric literal cannot be directly followed by an identifier",
            ));
        }
        self.push_token(radix.token_kind(), self.span_from(start), false);
        Ok(())
    }

    pub(super) fn bump_digits_or_underscores(&mut self) {
        while self.peek().is_some_and(|ch| is_digit(ch) || ch == '_') {
            self.bump();
        }
    }
}
