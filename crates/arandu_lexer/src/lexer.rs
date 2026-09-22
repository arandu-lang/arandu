use crate::ident::{is_ident_continue, is_ident_start, keyword_kind};
use crate::punctuation::{peek_kind_from, token_kind_from_prefix};
use crate::simd::SimdBackendKind;

use crate::{LexError, LexErrorCode, Span, Token, TokenKind};

mod comment;
pub(crate) mod ident;
mod numeric;
pub(crate) mod punctuation;
mod string;

pub struct Lexer<'a> {
    source: &'a str,
    pos: usize,
    line: usize,
    col: usize,
    tokens: Vec<Token>,
    prev_significant: Option<TokenKind>,
    diagnostics: Vec<LexError>,
    backend: SimdBackendKind,
}

#[derive(Clone, Copy)]
pub(super) struct Mark {
    pub(super) pos: usize,
}

impl<'a> Lexer<'a> {
    /// Heuristic ratio for token buffer preallocation based on average token density.
    const ESTIMATED_BYTES_PER_TOKEN: usize = 10;
    /// Minimum initial token capacity for small files.
    const MIN_TOKEN_CAPACITY: usize = 32;

    #[must_use]
    pub fn new(source: &'a str) -> Self {
        let capacity =
            (source.len() / Self::ESTIMATED_BYTES_PER_TOKEN).max(Self::MIN_TOKEN_CAPACITY);
        Self {
            source,
            pos: 0,
            line: 1,
            col: 1,
            tokens: Vec::with_capacity(capacity),
            prev_significant: None,
            diagnostics: Vec::new(),
            backend: SimdBackendKind::detect(),
        }
    }

    /// Lexes the full source, returning tokens or the first lexical error.
    ///
    /// # Errors
    ///
    /// Returns the first [`LexError`] if the source contains invalid tokens.
    pub fn lex(self) -> Result<crate::Lexed<'a>, LexError> {
        let lexed = self.lex_recovering();
        if let Some(err) = lexed.diagnostics.first().copied() {
            Err(err)
        } else {
            Ok(lexed)
        }
    }

    #[must_use]
    pub fn lex_recovering(mut self) -> crate::Lexed<'a> {
        while !self.is_at_end() {
            let start_pos = self.pos;
            let result = self.lex_next_token();

            if let Err(err) = result {
                let code = err.code;
                let span = err.span;
                self.diagnostics.push(err);
                self.push_token(TokenKind::Error(code), span, false);
                if self.pos == start_pos {
                    self.bump();
                }
            }
        }

        self.insert_semicolon_if_needed();
        let span = self.current_span();
        self.push_token(TokenKind::Eof, span, false);

        crate::Lexed {
            source: self.source,
            tokens: self.tokens,
            diagnostics: self.diagnostics,
        }
    }

    fn lex_next_token(&mut self) -> Result<(), LexError> {
        let prev_pos = self.pos;
        self.skip_whitespace();
        if self.pos > prev_pos {
            return Ok(());
        }

        let bytes = self.source.as_bytes();
        let remaining = bytes.len() - self.pos;
        if remaining == 0 {
            return Ok(());
        }

        let first = bytes[self.pos];

        match first {
            b'/' => {
                if remaining >= 3 && bytes[self.pos + 1] == b'/' && bytes[self.pos + 2] == b'/' {
                    return self.lex_line_doc_comment();
                } else if remaining >= 3
                    && bytes[self.pos + 1] == b'/'
                    && bytes[self.pos + 2] == b'!'
                {
                    // Inner doc comment (`//!`): documents the enclosing module
                    // instead of the following item. Stripped by
                    // `clean_doc_line` like `///`.
                    return self.lex_line_doc_comment();
                } else if remaining >= 3
                    && bytes[self.pos + 1] == b'*'
                    && bytes[self.pos + 2] == b'*'
                {
                    return self.lex_block_doc_comment();
                } else if remaining >= 2 && bytes[self.pos + 1] == b'/' {
                    return self.skip_line_comment();
                } else if remaining >= 2 && bytes[self.pos + 1] == b'*' {
                    return self.skip_block_comment();
                }
            }
            b'r' => {
                if remaining >= 4 && &bytes[self.pos..self.pos + 4] == b"r\"\"\"" {
                    return self.lex_raw_multiline_string();
                } else if remaining >= 2 && bytes[self.pos + 1] == b'"' {
                    return self.lex_raw_string();
                }
            }
            b'"' => {
                if remaining >= 3 && bytes[self.pos + 1] == b'"' && bytes[self.pos + 2] == b'"' {
                    return self.lex_multiline_string();
                }
                return self.lex_string(false);
            }
            b'\'' => {
                return self.lex_char();
            }
            b'0'..=b'9' => {
                return self.lex_number();
            }
            _ => {}
        }

        if first >= 128 && self.peek().is_some_and(ident::is_bidi_control) {
            let start = self.mark();
            self.bump();
            return Err(self.error_from(
                start,
                LexErrorCode::BidiTrojanSource,
                "unescaped bidirectional Unicode control character (Trojan Source, CWE-1307)",
            ));
        }

        if first < 128 && is_ident_start(first as char)
            || first >= 128 && self.peek().is_some_and(is_ident_start)
        {
            self.lex_ident_or_keyword();
            return Ok(());
        }

        self.lex_operator_or_punctuation()
    }

    fn skip_whitespace(&mut self) {
        let (newlines, skipped, last_nl) = match self.backend {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            SimdBackendKind::Avx2 => unsafe {
                crate::simd::avx2::skip_whitespace(&self.source.as_bytes()[self.pos..])
            },
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            SimdBackendKind::Sse2 => unsafe {
                crate::simd::sse2::skip_whitespace(&self.source.as_bytes()[self.pos..])
            },
            #[cfg(target_arch = "aarch64")]
            SimdBackendKind::Neon => unsafe {
                crate::simd::neon::skip_whitespace(&self.source.as_bytes()[self.pos..])
            },
            _ => crate::simd::scalar::skip_whitespace(&self.source.as_bytes()[self.pos..]),
        };

        if skipped == 0 {
            return;
        }

        if newlines > 0 {
            let mut first_nl_offset = self.source.as_bytes()[self.pos..self.pos + skipped]
                .iter()
                .position(|&b| b == b'\n')
                .unwrap_or(0);

            if first_nl_offset > 0
                && self.source.as_bytes()[self.pos + first_nl_offset - 1] == b'\r'
            {
                first_nl_offset -= 1;
            }

            let first_nl_span = Span::new(
                0,
                (self.pos + first_nl_offset) as u32,
                (self.pos + first_nl_offset) as u32,
            );

            self.pos += skipped;
            self.line += newlines;
            if let Some(last_nl_idx) = last_nl {
                self.col = skipped - last_nl_idx;
            } else {
                self.col += skipped;
            }

            self.maybe_insert_semicolon_at(first_nl_span);
        } else {
            self.pos += skipped;
            self.col += skipped;
        }
    }

    fn maybe_insert_semicolon_at(&mut self, span: Span) {
        let can_insert = self
            .prev_significant
            .is_some_and(TokenKind::can_end_statement);
        if !can_insert {
            return;
        }

        if self
            .peek_next_significant_kind()
            .is_some_and(|kind| kind.prevents_semicolon_before())
        {
            return;
        }

        self.push_token(TokenKind::Semicolon, span, true);
    }

    fn insert_semicolon_if_needed(&mut self) {
        let can_insert = self
            .prev_significant
            .is_some_and(TokenKind::can_end_statement);
        if can_insert {
            let span = self.current_span();
            self.push_token(TokenKind::Semicolon, span, true);
        }
    }

    fn lex_ident_or_keyword(&mut self) {
        let start = self.mark();
        self.bump();
        let len = match self.backend {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            SimdBackendKind::Avx2 => unsafe {
                crate::simd::avx2::scan_identifier(&self.source.as_bytes()[self.pos..])
            },
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            SimdBackendKind::Sse2 => unsafe {
                crate::simd::sse2::scan_identifier(&self.source.as_bytes()[self.pos..])
            },
            #[cfg(target_arch = "aarch64")]
            SimdBackendKind::Neon => unsafe {
                crate::simd::neon::scan_identifier(&self.source.as_bytes()[self.pos..])
            },
            _ => crate::simd::scalar::scan_identifier(&self.source.as_bytes()[self.pos..]),
        };
        self.pos += len;
        self.col += len;

        while let Some(ch) = self.peek() {
            if is_ident_continue(ch) {
                self.bump();
            } else {
                break;
            }
        }
        let lexeme = self.slice_from(start.pos);
        let kind = keyword_kind(lexeme).unwrap_or_else(|| {
            let is_type = lexeme
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_uppercase());
            if is_type {
                TokenKind::IdentType
            } else {
                TokenKind::IdentValue
            }
        });
        self.push_token(kind, self.span_from(start), false);
    }

    fn lex_operator_or_punctuation(&mut self) -> Result<(), LexError> {
        let start = self.mark();
        let Some((kind, len)) = token_kind_from_prefix(&self.source.as_bytes()[self.pos..]) else {
            if self.is_at_end() {
                return Ok(());
            }
            self.bump();
            return Err(self.error_from(start, LexErrorCode::InvalidChar, "invalid character"));
        };
        self.bump_ascii(len);
        self.push_token(kind, self.span_from(start), false);
        Ok(())
    }

    fn peek_next_significant_kind(&self) -> Option<TokenKind> {
        let bytes = self.source.as_bytes();
        let mut i = self.pos;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                i += 1;
                continue;
            }
            let remaining = bytes.len() - i;
            if remaining >= 2 && bytes[i] == b'/' {
                if bytes[i + 1] == b'/' {
                    while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                        i += 1;
                    }
                    continue;
                } else if bytes[i + 1] == b'*' {
                    i += 2;
                    while i + 1 < bytes.len() {
                        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
            }
            // Ensure i is on a UTF-8 char boundary before slicing into &str
            let valid_i = (i..=bytes.len())
                .find(|&pos| self.source.is_char_boundary(pos))
                .unwrap_or(bytes.len());
            return Some(peek_kind_from(&self.source[valid_i..]));
        }
        Some(TokenKind::Eof)
    }

    fn push_token(&mut self, kind: TokenKind, span: Span, inserted: bool) {
        if !matches!(kind, TokenKind::DocComment) {
            self.prev_significant = Some(kind);
        }
        self.tokens.push(Token {
            start: span.start,
            len: span.end - span.start,
            kind,
            inserted,
        });
    }

    fn current_span(&self) -> Span {
        Span::new(0, self.pos as u32, self.pos as u32)
    }

    fn span_from(&self, start: Mark) -> Span {
        Span::new(0, start.pos as u32, self.pos as u32)
    }

    #[cold]
    #[inline(never)]
    fn error_from(&self, start: Mark, code: LexErrorCode, message: &'static str) -> LexError {
        LexError::new(code, message, self.span_from(start))
    }

    fn mark(&self) -> Mark {
        Mark { pos: self.pos }
    }

    fn slice_from(&self, start: usize) -> &str {
        &self.source[start..self.pos]
    }

    /// Advance past `n` bytes of known ASCII content that contains no newlines.
    /// All callers use this for fixed delimiters like `//`, `/*`, `*/`, `${`, `"""`, etc.
    fn bump_ascii(&mut self, n: usize) {
        debug_assert!(
            self.source.as_bytes()[self.pos..self.pos + n]
                .iter()
                .all(|&b| b.is_ascii() && b != b'\n'),
            "bump_ascii called with non-ASCII or newline content"
        );
        self.pos += n;
        self.col += n;
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    fn peek(&self) -> Option<char> {
        let bytes = self.source.as_bytes();
        if self.pos < bytes.len() {
            let b = bytes[self.pos];
            if b < 128 {
                Some(b as char)
            } else if self.source.is_char_boundary(self.pos) {
                self.source[self.pos..].chars().next()
            } else {
                None
            }
        } else {
            None
        }
    }

    fn peek_next(&self) -> Option<char> {
        let bytes = self.source.as_bytes();
        if self.pos < bytes.len() {
            let b = bytes[self.pos];
            if b < 128 {
                let next_pos = self.pos + 1;
                if next_pos < bytes.len() {
                    let b2 = bytes[next_pos];
                    if b2 < 128 {
                        return Some(b2 as char);
                    }
                }
            }
        }
        let mut chars = self.source[self.pos..].chars();
        chars.next()?;
        chars.next()
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.source.as_bytes()[self.pos..].starts_with(prefix.as_bytes())
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.source.len()
    }
}
