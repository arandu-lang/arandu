mod index;
mod kind;

use crate::LexErrorCode;

pub use arandu_base::span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub start: u32,
    pub len: u32,
    pub kind: TokenKind,
    pub inserted: bool,
}

const _: () = assert!(std::mem::size_of::<Token>() == 12);
const _: () = assert!(std::mem::size_of::<TokenKind>() == 2);

impl Token {
    #[inline]
    #[must_use]
    pub fn end(&self) -> u32 {
        self.start.saturating_add(self.len)
    }

    #[must_use]
    pub fn span(&self, file_id: u32) -> Span {
        Span::new(file_id, self.start, self.end())
    }

    #[must_use]
    pub fn lexeme<'a>(&self, source: &'a str) -> &'a str {
        if self.inserted || matches!(self.kind, TokenKind::Error(_) | TokenKind::Eof) {
            return "";
        }
        let start = self.start as usize;
        let end = self.end() as usize;
        source.get(start..end.min(source.len())).unwrap_or("")
    }

    #[must_use]
    pub fn raw_string_content<'a>(&self, source: &'a str) -> &'a str {
        let lexeme = self.lexeme(source);
        if let Some(stripped) = lexeme.strip_prefix("r\"\"\"") {
            return stripped.strip_suffix("\"\"\"").unwrap_or("");
        }
        if let Some(stripped) = lexeme.strip_prefix("r\"") {
            return stripped.strip_suffix('"').unwrap_or("");
        }
        ""
    }

    #[must_use]
    pub fn char_content<'a>(&self, source: &'a str) -> &'a str {
        let lexeme = self.lexeme(source);
        lexeme
            .strip_prefix('\'')
            .and_then(|text| text.strip_suffix('\''))
            .unwrap_or("")
    }

    /// Decode the scalar value of a lexed character literal.
    #[must_use]
    pub fn char_value(&self, source: &str) -> Option<char> {
        decode_char_content(self.char_content(source))
    }

    #[must_use]
    pub fn byte_content<'a>(&self, source: &'a str) -> &'a str {
        let lexeme = self.lexeme(source);
        lexeme
            .strip_prefix("b'")
            .and_then(|text| text.strip_suffix('\''))
            .unwrap_or("")
    }

    /// Decode the byte value of a lexed byte character literal.
    #[must_use]
    pub fn byte_value(&self, source: &str) -> Option<u8> {
        decode_byte_content(self.byte_content(source))
    }

    #[must_use]
    pub fn dump(&self, source: &str) -> String {
        if self.kind == TokenKind::Semicolon && self.inserted {
            "SEMICOLON(inserted)".to_string()
        } else {
            self.kind.display_with(self, source)
        }
    }
}

/// Decode the contents between the quotes of a character literal.
#[must_use]
pub fn decode_char_content(content: &str) -> Option<char> {
    if let Some(escaped) = content.strip_prefix('\\') {
        return match escaped {
            "n" => Some('\n'),
            "t" => Some('\t'),
            "r" => Some('\r'),
            "0" => Some('\0'),
            "\\" => Some('\\'),
            "\"" => Some('"'),
            "'" => Some('\''),
            "$" => Some('$'),
            unicode if unicode.starts_with("u{") && unicode.ends_with('}') => {
                let digits = &unicode[2..unicode.len() - 1];
                if digits.is_empty() {
                    return None;
                }
                u32::from_str_radix(digits, 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ => None,
        };
    }

    let mut chars = content.chars();
    let value = chars.next()?;
    chars.next().is_none().then_some(value)
}

/// Encode a Unicode scalar as a valid Arandu character literal.
#[must_use]
pub fn char_literal(value: char) -> String {
    let escaped = match value {
        '\n' => "\\n",
        '\t' => "\\t",
        '\r' => "\\r",
        '\0' => "\\0",
        '\\' => "\\\\",
        '\'' => "\\'",
        '"' => "\\\"",
        '$' => "\\$",
        value => return format!("'{value}'"),
    };
    format!("'{escaped}'")
}

/// Decode the contents between the quotes of a byte literal.
#[must_use]
pub fn decode_byte_content(content: &str) -> Option<u8> {
    if let Some(escaped) = content.strip_prefix('\\') {
        return match escaped {
            "n" => Some(b'\n'),
            "t" => Some(b'\t'),
            "r" => Some(b'\r'),
            "0" => Some(b'\0'),
            "\\" => Some(b'\\'),
            "\"" => Some(b'"'),
            "'" => Some(b'\''),
            hex if hex.starts_with('x') && hex.len() == 3 => u8::from_str_radix(&hex[1..], 16).ok(),
            _ => None,
        };
    }

    if content.len() == 1 {
        let b = content.as_bytes()[0];
        if b.is_ascii() {
            return Some(b);
        }
    }
    None
}

/// Encode a byte as a valid Arandu byte character literal.
#[must_use]
pub fn byte_literal(value: u8) -> String {
    let escaped = match value {
        b'\n' => "\\n",
        b'\t' => "\\t",
        b'\r' => "\\r",
        b'\0' => "\\0",
        b'\\' => "\\\\",
        b'\'' => "\\'",
        b'"' => "\\\"",
        b if b.is_ascii_graphic() || b == b' ' => return format!("b'{}'", b as char),
        b => return format!("b'\\x{b:02x}'"),
    };
    format!("b'{escaped}'")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TokenKind {
    IdentValue,
    IdentType,
    DocComment,
    IntDec,
    IntHex,
    IntBin,
    IntOct,
    Float,
    StringStart,
    StringText,
    StringEscape,
    InterpStart,
    InterpEnd,
    StringEnd,
    RawString,
    MultilineStringStart,
    MultilineStringEnd,
    Char,
    ByteChar,
    KwIf,
    KwElse,
    KwFor,
    KwIn,
    KwWhile,
    KwMatch,
    KwReturn,
    KwBreak,
    KwContinue,
    KwFunc,
    KwAsync,
    KwComptime,
    KwAwait,
    KwStruct,
    KwEnum,
    KwInterface,
    KwConst,
    KwType,
    KwModule,
    KwImport,
    KwFrom,
    KwAs,
    KwPublic,
    KwInternal,
    KwPrivate,
    KwExtern,
    KwUnsafe,
    KwWhere,
    KwCatch,
    KwIs,
    KwSet,
    KwOwn,
    KwMut,
    KwShared,
    KwRef,
    KwSelf,
    KwPtr,
    KwAlloc,
    KwFree,
    KwDefer,
    KwErrdefer,
    KwLet,
    KwImpl,
    TypeInt,
    TypeUint,
    TypeIsize,
    TypeUsize,
    TypeFloat,
    TypeI8,
    TypeI16,
    TypeI32,
    TypeI64,
    TypeU8,
    TypeU16,
    TypeU32,
    TypeU64,
    TypeF32,
    TypeF64,
    TypeBool,
    TypeByte,
    TypeChar,
    TypeStr,
    TypeAny,
    TypeErr,
    BoolTrue,
    BoolFalse,
    Nil,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    Colon,
    Semicolon,
    At,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Lt,
    Gt,
    Equal,
    Bang,
    Tilde,
    Question,
    SafeDot,
    SafeIndexStart,
    NullCoalesce,
    LogicalOr,
    LogicalAnd,
    FatArrow,
    PlusEqual,
    MinusEqual,
    StarEqual,
    SlashEqual,
    PercentEqual,
    AmpEqual,
    PipeEqual,
    CaretEqual,
    ShiftLeftEqual,
    ShiftRightEqual,
    ShiftLeft,
    ShiftRight,
    EqualEqual,
    BangEqual,
    LtEqual,
    GtEqual,
    RangeInclusive,
    RangeExclusive,
    Ellipsis,
    Arrow,
    Eof,
    Error(LexErrorCode),
}
