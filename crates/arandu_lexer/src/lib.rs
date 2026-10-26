mod error;
mod lexer;
mod token;

pub(crate) use lexer::ident;
pub(crate) use lexer::punctuation;

pub mod simd;

pub use error::{LexError, LexErrorCode};
pub use lexer::Lexer;
pub use token::{
    Span, Token, TokenKind, byte_literal, char_literal, decode_byte_content,
    decode_byte_string_content, decode_char_content,
};

/// Classify a complete source spelling as an identifier token.
///
/// This is the canonical lexical check used by refactorings: it rejects
/// keywords, primitive types, literals and strings instead of maintaining a
/// second reserved-word list in an IDE client.
#[must_use]
pub fn identifier_kind(text: &str) -> Option<TokenKind> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if !ident::is_ident_start(first) || !chars.all(ident::is_ident_continue) {
        return None;
    }
    if ident::keyword_kind(text).is_some() {
        return None;
    }
    Some(if first.is_ascii_uppercase() {
        TokenKind::IdentType
    } else {
        TokenKind::IdentValue
    })
}

/// Lexes source, stopping at the first error.
///
/// # Errors
///
/// Returns the first [`LexError`] if the source contains invalid tokens.
pub fn lex<'a>(source: &'a str) -> Result<Lexed<'a>, LexError> {
    Lexer::new(source).lex()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lexed<'a> {
    pub source: &'a str,
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<LexError>,
}

#[must_use]
pub fn lex_recovering<'a>(source: &'a str) -> Lexed<'a> {
    Lexer::new(source).lex_recovering()
}

/// Lexes source and returns a newline-separated token dump.
///
/// # Errors
///
/// Returns the first [`LexError`] if the source contains invalid tokens.
pub fn lex_to_string(source: &str) -> Result<String, LexError> {
    let lexed = lex(source)?;
    let mut out = String::new();
    for (i, token) in lexed.tokens.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&token.dump(lexed.source));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_keywords_and_primitive_types() {
        let dump = lex_to_string("func main(): int { return 1 }").unwrap();
        assert!(dump.contains("KW_FUNC"));
        assert!(dump.contains("TYPE_INT"));
        assert!(dump.contains("KW_RETURN"));
    }

    #[test]
    fn inner_doc_comment_lexes_as_doc_comment() {
        let dump = lex_to_string("//! Module overview.").unwrap();
        assert!(dump.contains("DOC_COMMENT"));
        let dump = lex_to_string("/// Item docs.").unwrap();
        assert!(dump.contains("DOC_COMMENT"));
        let dump = lex_to_string("// Regular comment.").unwrap();
        assert!(!dump.contains("DOC_COMMENT"));
    }

    #[test]
    fn identifier_kind_uses_the_language_lexer_contract() {
        assert_eq!(identifier_kind("value_2"), Some(TokenKind::IdentValue));
        assert_eq!(identifier_kind("Point"), Some(TokenKind::IdentType));
        assert_eq!(identifier_kind("ação"), Some(TokenKind::IdentValue));
        assert_eq!(identifier_kind("índice"), Some(TokenKind::IdentValue));
        assert_eq!(identifier_kind("variável_1"), Some(TokenKind::IdentValue));
        assert_eq!(identifier_kind("2value"), None);
        assert_eq!(identifier_kind("return"), None);
        assert_eq!(identifier_kind("int"), None);
        assert_eq!(identifier_kind("two words"), None);
        assert_eq!(identifier_kind("🎉_emoji"), None);
        assert_eq!(identifier_kind("±plusminus"), None);
    }

    #[test]
    fn source_longer_than_four_gibibytes_is_rejected_before_span_truncation() {
        assert!(!crate::lexer::source_exceeds_span_limit(u32::MAX as usize));
        assert!(crate::lexer::source_exceeds_span_limit(
            (u32::MAX as usize).saturating_add(1)
        ));
    }

    #[test]
    fn reports_unterminated_string() {
        let err = lex("\"open").unwrap_err();
        assert_eq!(err.code, LexErrorCode::UnterminatedString);
    }

    #[test]
    fn reports_empty_char() {
        let err = lex("''").unwrap_err();
        assert_eq!(err.code, LexErrorCode::EmptyChar);
    }

    #[test]
    fn reports_invalid_escape() {
        let err = lex("\"\\q\"").unwrap_err();
        assert_eq!(err.code, LexErrorCode::InvalidEscape);
    }

    #[test]
    fn reports_unterminated_block_comment() {
        let err = lex("/* open").unwrap_err();
        assert_eq!(err.code, LexErrorCode::UnterminatedBlockComment);
    }

    #[test]
    fn reports_invalid_binary_digit() {
        let err = lex("0b102").unwrap_err();
        assert_eq!(err.code, LexErrorCode::InvalidBinaryDigit);
    }

    #[test]
    fn reports_invalid_unicode_escape_in_char() {
        let err = lex("'\\u{}'").unwrap_err();
        assert_eq!(err.code, LexErrorCode::InvalidUnicodeEscape);
    }

    #[test]
    fn character_literal_encoder_and_decoder_round_trip_scalars() {
        for value in ['a', 'é', '中', '🦀', '\n', '\t', '\0', '\'', '\\', '"', '$'] {
            let source = char_literal(value);
            let token = lex(&source).expect("encoded char literal must lex").tokens[0];
            assert_eq!(token.char_value(&source), Some(value), "{source:?}");
        }
        let token = lex("'\\u{1F980}'").expect("unicode escape must lex").tokens[0];
        assert_eq!(token.char_value("'\\u{1F980}'"), Some('🦀'));
    }

    #[test]
    fn interpolation_uses_canonical_character_and_raw_string_tokens() {
        let source = r#""${'λ'}:${'}'}:${b'{'}:${r"raw}"}:end""#;
        let lexed = lex(source).expect("literal braces must not close interpolation");
        let kinds: Vec<_> = lexed.tokens.iter().map(|token| token.kind).collect();
        assert_eq!(
            kinds
                .iter()
                .filter(|&&kind| kind == TokenKind::InterpEnd)
                .count(),
            4
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|&&kind| kind == TokenKind::Char)
                .count(),
            2
        );
        assert!(kinds.contains(&TokenKind::ByteChar));
        assert!(kinds.contains(&TokenKind::RawString));
    }

    #[test]
    fn interpolation_discards_documentation_comments_and_their_braces() {
        let source =
            "\"${/** } ignored */ 42 /* { ignored */}:${/// } ignored\n42 //! { ignored\n}\"";
        let lexed = lex(source).expect("comments must not close interpolation");
        let kinds: Vec<_> = lexed.tokens.iter().map(|token| token.kind).collect();
        assert!(!kinds.contains(&TokenKind::DocComment));
        assert_eq!(
            kinds
                .iter()
                .filter(|&&kind| kind == TokenKind::InterpEnd)
                .count(),
            2
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|&&kind| kind == TokenKind::IntDec)
                .count(),
            2
        );
    }

    #[test]
    fn rejects_surrogate_unicode_escapes_as_non_scalars() {
        let error = lex("'\\u{D800}'").unwrap_err();
        assert_eq!(error.code, LexErrorCode::InvalidUnicodeEscape);
        let error = lex("\"\\u{D800}\"").unwrap_err();
        assert_eq!(error.code, LexErrorCode::InvalidUnicodeEscape);
    }

    #[test]
    fn byte_literal_lexing_and_decoding() {
        for b in *b"/*#aZ0 \n\t\r\0'\\" {
            let source = byte_literal(b);
            let result = lex(&source).expect("encoded byte literal must lex");
            assert_eq!(result.tokens[0].kind, TokenKind::ByteChar);
            assert_eq!(result.tokens[0].byte_value(&source), Some(b), "{source:?}");
        }

        let hex = "b'\\x41'";
        let res = lex(hex).expect("hex byte literal must lex");
        assert_eq!(res.tokens[0].kind, TokenKind::ByteChar);
        assert_eq!(res.tokens[0].byte_value(hex), Some(65));

        assert_eq!(lex("b''").unwrap_err().code, LexErrorCode::EmptyChar);
        assert_eq!(lex("b'ab'").unwrap_err().code, LexErrorCode::CharTooLong);
        assert_eq!(
            lex("b'open").unwrap_err().code,
            LexErrorCode::UnterminatedChar
        );
    }

    #[test]
    fn byte_string_lexing_preserves_raw_bytes_and_escapes() {
        for (source, expected) in [
            ("b\"\"", &b""[..]),
            ("b\"AB\"", b"AB"),
            ("b\"a\\0\\n\\t\\r\\\\\\\"\\'\"", b"a\0\n\t\r\\\"'"),
            ("b\"\\x00\\x7f\\xff\"", &[0x00, 0x7f, 0xff]),
            ("b\"\\x41B\"", b"AB"),
        ] {
            let result = lex(source).expect("byte string must lex");
            assert_eq!(result.tokens[0].kind, TokenKind::ByteString, "{source:?}");
            assert_eq!(
                result.tokens[0].byte_string_bytes(source).as_deref(),
                Some(expected),
                "{source:?}"
            );
        }

        // Byte strings are not interpolated: `$` and `${}` stay literal bytes.
        let literal = "b\"${x} $y\"";
        let result = lex(literal).expect("byte strings do not interpolate");
        assert_eq!(result.tokens[0].kind, TokenKind::ByteString);
        assert_eq!(
            result.tokens[0].byte_string_bytes(literal).as_deref(),
            Some(&b"${x} $y"[..])
        );
        assert!(
            !result
                .tokens
                .iter()
                .any(|token| matches!(token.kind, TokenKind::InterpStart | TokenKind::StringStart))
        );
    }

    #[test]
    fn byte_string_rejects_non_ascii_unterminated_and_control_characters() {
        assert_eq!(lex("b\"é\"").unwrap_err().code, LexErrorCode::InvalidChar);
        assert_eq!(
            lex("b\"open").unwrap_err().code,
            LexErrorCode::UnterminatedString
        );
        assert_eq!(
            lex("b\"line\nbreak\"").unwrap_err().code,
            LexErrorCode::UnterminatedString
        );
        assert_eq!(
            lex("b\"\u{202e}\"").unwrap_err().code,
            LexErrorCode::BidiTrojanSource
        );
        assert_eq!(
            lex("b\"\\q\"").unwrap_err().code,
            LexErrorCode::InvalidEscape
        );
    }
}
