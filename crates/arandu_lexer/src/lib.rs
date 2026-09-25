mod error;
mod lexer;
mod token;

pub(crate) use lexer::ident;
pub(crate) use lexer::punctuation;

pub mod simd;

pub use error::{LexError, LexErrorCode};
pub use lexer::Lexer;
pub use token::{Span, Token, TokenKind, char_literal, decode_char_content};

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
    fn rejects_surrogate_unicode_escapes_as_non_scalars() {
        let error = lex("'\\u{D800}'").unwrap_err();
        assert_eq!(error.code, LexErrorCode::InvalidUnicodeEscape);
        let error = lex("\"\\u{D800}\"").unwrap_err();
        assert_eq!(error.code, LexErrorCode::InvalidUnicodeEscape);
    }
}
