use std::fmt;

use arandu_base::source_registry::SourceRegistry;
use arandu_base::span::Span;
use arandu_lexer::Token;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub code: ParseErrorCode,
    pub message: Box<str>,
    pub span: Span,
    pub found: Box<str>,
    pub expected: &'static [&'static str],
}

impl ParseError {
    #[cold]
    #[inline(never)]
    #[tracing::instrument(
        level = "trace",
        target = "arandu_parser",
        skip(source, token, message)
    )]
    pub(super) fn new(
        code: ParseErrorCode,
        message: impl Into<String>,
        token: &Token,
        file_id: u32,
        source: &str,
    ) -> Self {
        Self {
            code,
            message: message.into().into_boxed_str(),
            span: token.span(file_id),
            found: token
                .kind
                .user_facing_display(token, source)
                .into_boxed_str(),
            expected: &[],
        }
    }

    #[cold]
    #[inline(never)]
    pub(super) fn expected(
        code: ParseErrorCode,
        message: impl Into<String>,
        token: &Token,
        file_id: u32,
        source: &str,
        expected: &'static [&'static str],
    ) -> Self {
        Self {
            code,
            message: message.into().into_boxed_str(),
            span: token.span(file_id),
            found: token
                .kind
                .user_facing_display(token, source)
                .into_boxed_str(),
            expected,
        }
    }

    pub(crate) fn from_lex(err: arandu_lexer::LexError, file_id: u32) -> Self {
        let mut span = err.span;
        span.file_id = file_id;
        Self {
            code: ParseErrorCode::Lex,
            message: Box::from(err.message),
            span,
            found: format!("{:?}", err.code).into_boxed_str(),
            expected: &[],
        }
    }
    #[must_use]
    pub fn format_for_cli(&self, registry: &SourceRegistry) -> String {
        let diag = arandu_diagnostics::Diagnostic::from(self.clone());
        diag.format_for_cli(registry)
    }
}

impl From<ParseError> for arandu_diagnostics::Diagnostic {
    fn from(err: ParseError) -> Self {
        let diag_code = match err.code {
            ParseErrorCode::Lex => match &*err.found {
                "BidiTrojanSource" => arandu_diagnostics::DiagCode::LX004BidiTrojanSource,
                "InvalidChar" => arandu_diagnostics::DiagCode::LX002InvalidUnicodeChar,
                "UnterminatedString"
                | "UnterminatedMultilineString"
                | "UnterminatedRawString"
                | "UnterminatedChar"
                | "UnterminatedBlockComment"
                | "UnclosedInterpolation" => arandu_diagnostics::DiagCode::LX001UnterminatedString,
                "InvalidNumericLiteral"
                | "InvalidBinaryDigit"
                | "InvalidOctalDigit"
                | "InvalidHexDigit"
                | "LeadingZero" => arandu_diagnostics::DiagCode::LX003InvalidNumericLiteral,
                _ => arandu_diagnostics::DiagCode::LX002InvalidUnicodeChar,
            },
            ParseErrorCode::ExpectedToken => arandu_diagnostics::DiagCode::P001UnexpectedToken,
            ParseErrorCode::ExpectedTopLevelDecl => {
                arandu_diagnostics::DiagCode::P001UnexpectedToken
            }
            ParseErrorCode::ExpectedExpression => {
                arandu_diagnostics::DiagCode::P005ExpectedExpression
            }
            ParseErrorCode::ExpectedType => arandu_diagnostics::DiagCode::P001UnexpectedToken,
            ParseErrorCode::ExpectedPlace => arandu_diagnostics::DiagCode::P001UnexpectedToken,
            ParseErrorCode::InvalidResultReturn => {
                arandu_diagnostics::DiagCode::P001UnexpectedToken
            }
        };
        let msg = format!("{} (found {})", err.message, err.found);
        arandu_diagnostics::Diagnostic::error(diag_code, msg, err.span)
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let registry = SourceRegistry::default();
        f.write_str(&self.format_for_cli(&registry))
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorCode {
    Lex,
    ExpectedToken,
    ExpectedTopLevelDecl,
    ExpectedExpression,
    ExpectedType,
    ExpectedPlace,
    /// Tuple error-return syntax; use `Result<T, E>` instead.
    InvalidResultReturn,
}
