use std::fmt;

use crate::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexError {
    pub code: LexErrorCode,
    pub message: &'static str,
    pub span: Span,
}

impl LexError {
    #[cold]
    #[inline(never)]
    pub fn new(code: LexErrorCode, message: &'static str, span: Span) -> Self {
        Self {
            code,
            message,
            span,
        }
    }
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}: {} at byte {}",
            self.code, self.message, self.span.start
        )
    }
}

impl std::error::Error for LexError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LexErrorCode {
    InvalidChar,
    UnterminatedString,
    UnterminatedMultilineString,
    UnterminatedRawString,
    UnterminatedChar,
    EmptyChar,
    CharTooLong,
    InvalidEscape,
    InvalidUnicodeEscape,
    UnterminatedBlockComment,
    InvalidNumericLiteral,
    InvalidBinaryDigit,
    InvalidOctalDigit,
    InvalidHexDigit,
    LeadingZero,
    UnclosedInterpolation,
    BidiTrojanSource,
    /// The source is longer than a `u32` byte offset. Spans are not truncated.
    SourceTooLarge,
}
