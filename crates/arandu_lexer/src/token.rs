use std::fmt;

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
    #[must_use]
    pub fn span(&self, file_id: u32) -> Span {
        Span::new(file_id, self.start, self.start + self.len)
    }

    #[must_use]
    pub fn lexeme<'a>(&self, source: &'a str) -> &'a str {
        if self.inserted || matches!(self.kind, TokenKind::Error(_) | TokenKind::Eof) {
            return "";
        }
        &source[self.start as usize..(self.start + self.len) as usize]
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

    #[must_use]
    pub fn dump(&self, source: &str) -> String {
        if self.kind == TokenKind::Semicolon && self.inserted {
            "SEMICOLON(inserted)".to_string()
        } else {
            self.kind.display_with(self, source)
        }
    }
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

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Error(code) => write!(f, "ERROR({code:?})"),
            other => f.write_str(other.name()),
        }
    }
}

impl TokenKind {
    pub const COUNT: usize = 133;

    /// Returns `true` if this token kind represents a language keyword.
    #[must_use]
    pub const fn is_keyword(self) -> bool {
        matches!(
            self,
            TokenKind::KwIf
                | TokenKind::KwElse
                | TokenKind::KwFor
                | TokenKind::KwIn
                | TokenKind::KwWhile
                | TokenKind::KwMatch
                | TokenKind::KwReturn
                | TokenKind::KwBreak
                | TokenKind::KwContinue
                | TokenKind::KwFunc
                | TokenKind::KwAsync
                | TokenKind::KwAwait
                | TokenKind::KwStruct
                | TokenKind::KwEnum
                | TokenKind::KwInterface
                | TokenKind::KwConst
                | TokenKind::KwType
                | TokenKind::KwModule
                | TokenKind::KwImport
                | TokenKind::KwFrom
                | TokenKind::KwAs
                | TokenKind::KwPublic
                | TokenKind::KwExtern
                | TokenKind::KwUnsafe
                | TokenKind::KwWhere
                | TokenKind::KwCatch
                | TokenKind::KwIs
                | TokenKind::KwSet
                | TokenKind::KwOwn
                | TokenKind::KwMut
                | TokenKind::KwShared
                | TokenKind::KwRef
                | TokenKind::KwSelf
                | TokenKind::KwPtr
                | TokenKind::KwAlloc
                | TokenKind::KwFree
                | TokenKind::KwDefer
                | TokenKind::KwErrdefer
                | TokenKind::KwLet
                | TokenKind::KwImpl
        )
    }

    #[must_use]
    pub const fn index(&self) -> usize {
        match self {
            TokenKind::IdentValue => 0,
            TokenKind::IdentType => 1,
            TokenKind::DocComment => 2,
            TokenKind::IntDec => 3,
            TokenKind::IntHex => 4,
            TokenKind::IntBin => 5,
            TokenKind::IntOct => 6,
            TokenKind::Float => 7,
            TokenKind::StringStart => 8,
            TokenKind::StringText => 9,
            TokenKind::StringEscape => 10,
            TokenKind::InterpStart => 11,
            TokenKind::InterpEnd => 12,
            TokenKind::StringEnd => 13,
            TokenKind::RawString => 14,
            TokenKind::MultilineStringStart => 15,
            TokenKind::MultilineStringEnd => 16,
            TokenKind::Char => 17,
            TokenKind::KwIf => 18,
            TokenKind::KwElse => 19,
            TokenKind::KwFor => 20,
            TokenKind::KwIn => 21,
            TokenKind::KwWhile => 22,
            TokenKind::KwMatch => 23,
            TokenKind::KwReturn => 24,
            TokenKind::KwBreak => 25,
            TokenKind::KwContinue => 26,
            TokenKind::KwFunc => 27,
            TokenKind::KwAsync => 28,
            TokenKind::KwAwait => 29,
            TokenKind::KwStruct => 30,
            TokenKind::KwEnum => 31,
            TokenKind::KwInterface => 32,
            TokenKind::KwConst => 33,
            TokenKind::KwType => 34,
            TokenKind::KwModule => 35,
            TokenKind::KwImport => 36,
            TokenKind::KwFrom => 37,
            TokenKind::KwAs => 38,
            TokenKind::KwPublic => 39,
            TokenKind::KwExtern => 40,
            TokenKind::KwUnsafe => 41,
            TokenKind::KwWhere => 42,
            TokenKind::KwCatch => 43,
            TokenKind::KwIs => 44,
            TokenKind::KwSet => 45,
            TokenKind::KwOwn => 46,
            TokenKind::KwMut => 47,
            TokenKind::KwShared => 48,
            TokenKind::KwSelf => 49,
            TokenKind::KwPtr => 50,
            TokenKind::KwAlloc => 51,
            TokenKind::KwFree => 52,
            TokenKind::KwDefer => 53,
            TokenKind::KwErrdefer => 54,
            TokenKind::KwLet => 129,
            TokenKind::KwRef => 130,
            TokenKind::KwImpl => 131,
            TokenKind::TypeInt => 55,
            TokenKind::TypeUint => 56,
            TokenKind::TypeFloat => 57,
            TokenKind::TypeI8 => 58,
            TokenKind::TypeI16 => 59,
            TokenKind::TypeI32 => 60,
            TokenKind::TypeI64 => 61,
            TokenKind::TypeU8 => 62,
            TokenKind::TypeU16 => 63,
            TokenKind::TypeU32 => 64,
            TokenKind::TypeU64 => 65,
            TokenKind::TypeF32 => 66,
            TokenKind::TypeF64 => 67,
            TokenKind::TypeBool => 68,
            TokenKind::TypeByte => 69,
            TokenKind::TypeChar => 70,
            TokenKind::TypeStr => 71,
            TokenKind::TypeAny => 72,
            TokenKind::TypeErr => 73,
            TokenKind::BoolTrue => 74,
            TokenKind::BoolFalse => 75,
            TokenKind::Nil => 76,
            TokenKind::LParen => 77,
            TokenKind::RParen => 78,
            TokenKind::LBracket => 79,
            TokenKind::RBracket => 80,
            TokenKind::LBrace => 81,
            TokenKind::RBrace => 82,
            TokenKind::Comma => 83,
            TokenKind::Dot => 84,
            TokenKind::Colon => 85,
            TokenKind::Semicolon => 86,
            TokenKind::At => 87,
            TokenKind::Plus => 88,
            TokenKind::Minus => 89,
            TokenKind::Star => 90,
            TokenKind::Slash => 91,
            TokenKind::Percent => 92,
            TokenKind::Amp => 93,
            TokenKind::Pipe => 94,
            TokenKind::Caret => 95,
            TokenKind::Lt => 96,
            TokenKind::Gt => 97,
            TokenKind::Equal => 98,
            TokenKind::Bang => 99,
            TokenKind::Tilde => 100,
            TokenKind::Question => 101,
            TokenKind::SafeDot => 102,
            TokenKind::SafeIndexStart => 103,
            TokenKind::NullCoalesce => 104,
            TokenKind::LogicalOr => 105,
            TokenKind::LogicalAnd => 106,
            TokenKind::FatArrow => 107,
            TokenKind::PlusEqual => 108,
            TokenKind::MinusEqual => 109,
            TokenKind::StarEqual => 110,
            TokenKind::SlashEqual => 111,
            TokenKind::PercentEqual => 112,
            TokenKind::AmpEqual => 113,
            TokenKind::PipeEqual => 114,
            TokenKind::CaretEqual => 115,
            TokenKind::ShiftLeftEqual => 116,
            TokenKind::ShiftRightEqual => 117,
            TokenKind::ShiftLeft => 118,
            TokenKind::ShiftRight => 119,
            TokenKind::EqualEqual => 120,
            TokenKind::BangEqual => 121,
            TokenKind::LtEqual => 122,
            TokenKind::GtEqual => 123,
            TokenKind::RangeInclusive => 124,
            TokenKind::RangeExclusive => 125,
            TokenKind::Ellipsis => 126,
            TokenKind::Arrow => 127,
            TokenKind::Eof => 128,
            TokenKind::Error(_) => 132,
        }
    }

    #[must_use]
    pub const fn index_to_token_kind(index: usize) -> TokenKind {
        match index {
            0 => TokenKind::IdentValue,
            1 => TokenKind::IdentType,
            2 => TokenKind::DocComment,
            3 => TokenKind::IntDec,
            4 => TokenKind::IntHex,
            5 => TokenKind::IntBin,
            6 => TokenKind::IntOct,
            7 => TokenKind::Float,
            8 => TokenKind::StringStart,
            9 => TokenKind::StringText,
            10 => TokenKind::StringEscape,
            11 => TokenKind::InterpStart,
            12 => TokenKind::InterpEnd,
            13 => TokenKind::StringEnd,
            14 => TokenKind::RawString,
            15 => TokenKind::MultilineStringStart,
            16 => TokenKind::MultilineStringEnd,
            17 => TokenKind::Char,
            18 => TokenKind::KwIf,
            19 => TokenKind::KwElse,
            20 => TokenKind::KwFor,
            21 => TokenKind::KwIn,
            22 => TokenKind::KwWhile,
            23 => TokenKind::KwMatch,
            24 => TokenKind::KwReturn,
            25 => TokenKind::KwBreak,
            26 => TokenKind::KwContinue,
            27 => TokenKind::KwFunc,
            28 => TokenKind::KwAsync,
            29 => TokenKind::KwAwait,
            30 => TokenKind::KwStruct,
            31 => TokenKind::KwEnum,
            32 => TokenKind::KwInterface,
            33 => TokenKind::KwConst,
            34 => TokenKind::KwType,
            35 => TokenKind::KwModule,
            36 => TokenKind::KwImport,
            37 => TokenKind::KwFrom,
            38 => TokenKind::KwAs,
            39 => TokenKind::KwPublic,
            40 => TokenKind::KwExtern,
            41 => TokenKind::KwUnsafe,
            42 => TokenKind::KwWhere,
            43 => TokenKind::KwCatch,
            44 => TokenKind::KwIs,
            45 => TokenKind::KwSet,
            46 => TokenKind::KwOwn,
            47 => TokenKind::KwMut,
            48 => TokenKind::KwShared,
            49 => TokenKind::KwSelf,
            50 => TokenKind::KwPtr,
            51 => TokenKind::KwAlloc,
            52 => TokenKind::KwFree,
            53 => TokenKind::KwDefer,
            54 => TokenKind::KwErrdefer,
            55 => TokenKind::TypeInt,
            56 => TokenKind::TypeUint,
            57 => TokenKind::TypeFloat,
            58 => TokenKind::TypeI8,
            59 => TokenKind::TypeI16,
            60 => TokenKind::TypeI32,
            61 => TokenKind::TypeI64,
            62 => TokenKind::TypeU8,
            63 => TokenKind::TypeU16,
            64 => TokenKind::TypeU32,
            65 => TokenKind::TypeU64,
            66 => TokenKind::TypeF32,
            67 => TokenKind::TypeF64,
            68 => TokenKind::TypeBool,
            69 => TokenKind::TypeByte,
            70 => TokenKind::TypeChar,
            71 => TokenKind::TypeStr,
            72 => TokenKind::TypeAny,
            73 => TokenKind::TypeErr,
            74 => TokenKind::BoolTrue,
            75 => TokenKind::BoolFalse,
            76 => TokenKind::Nil,
            77 => TokenKind::LParen,
            78 => TokenKind::RParen,
            79 => TokenKind::LBracket,
            80 => TokenKind::RBracket,
            81 => TokenKind::LBrace,
            82 => TokenKind::RBrace,
            83 => TokenKind::Comma,
            84 => TokenKind::Dot,
            85 => TokenKind::Colon,
            86 => TokenKind::Semicolon,
            87 => TokenKind::At,
            88 => TokenKind::Plus,
            89 => TokenKind::Minus,
            90 => TokenKind::Star,
            91 => TokenKind::Slash,
            92 => TokenKind::Percent,
            93 => TokenKind::Amp,
            94 => TokenKind::Pipe,
            95 => TokenKind::Caret,
            96 => TokenKind::Lt,
            97 => TokenKind::Gt,
            98 => TokenKind::Equal,
            99 => TokenKind::Bang,
            100 => TokenKind::Tilde,
            101 => TokenKind::Question,
            102 => TokenKind::SafeDot,
            103 => TokenKind::SafeIndexStart,
            104 => TokenKind::NullCoalesce,
            105 => TokenKind::LogicalOr,
            106 => TokenKind::LogicalAnd,
            107 => TokenKind::FatArrow,
            108 => TokenKind::PlusEqual,
            109 => TokenKind::MinusEqual,
            110 => TokenKind::StarEqual,
            111 => TokenKind::SlashEqual,
            112 => TokenKind::PercentEqual,
            113 => TokenKind::AmpEqual,
            114 => TokenKind::PipeEqual,
            115 => TokenKind::CaretEqual,
            116 => TokenKind::ShiftLeftEqual,
            117 => TokenKind::ShiftRightEqual,
            118 => TokenKind::ShiftLeft,
            119 => TokenKind::ShiftRight,
            120 => TokenKind::EqualEqual,
            121 => TokenKind::BangEqual,
            122 => TokenKind::LtEqual,
            123 => TokenKind::GtEqual,
            124 => TokenKind::RangeInclusive,
            125 => TokenKind::RangeExclusive,
            126 => TokenKind::Ellipsis,
            127 => TokenKind::Arrow,
            128 => TokenKind::Eof,
            129 => TokenKind::KwLet,
            130 => TokenKind::KwRef,
            131 => TokenKind::KwImpl,
            _ => TokenKind::Error(crate::LexErrorCode::InvalidChar),
        }
    }

    #[must_use]
    pub fn display_with(self, token: &Token, source: &str) -> String {
        match self {
            TokenKind::IdentValue => format!("IDENT_VALUE({})", token.lexeme(source)),
            TokenKind::IdentType => format!("IDENT_TYPE({})", token.lexeme(source)),
            TokenKind::DocComment => format!("DOC_COMMENT({})", token.lexeme(source)),
            TokenKind::IntDec => format!("INT_DEC({})", token.lexeme(source)),
            TokenKind::IntHex => format!("INT_HEX({})", token.lexeme(source)),
            TokenKind::IntBin => format!("INT_BIN({})", token.lexeme(source)),
            TokenKind::IntOct => format!("INT_OCT({})", token.lexeme(source)),
            TokenKind::Float => format!("FLOAT({})", token.lexeme(source)),
            TokenKind::StringText => format!("STRING_TEXT({})", token.lexeme(source)),
            TokenKind::StringEscape => format!("STRING_ESCAPE({})", token.lexeme(source)),
            TokenKind::RawString => {
                format!("RAW_STRING({})", token.raw_string_content(source))
            }
            TokenKind::Char => format!("CHAR({})", token.char_content(source)),
            TokenKind::Error(code) => format!("ERROR({code:?})"),
            other => other.name().to_string(),
        }
    }

    /// Renders the token as it should appear in user-facing error messages.
    ///
    /// Unlike [`display_with`](TokenKind::display_with), this never leaks
    /// parser-internal kind names (`EQUAL`, `IDENT_TYPE(...)`, `KW_FUNC`): idents
    /// and literals keep their source spelling, punctuation renders as its symbol
    /// and keywords as the keyword text.
    #[must_use]
    pub fn user_facing_display(self, token: &Token, source: &str) -> String {
        match self {
            TokenKind::IdentValue
            | TokenKind::IdentType
            | TokenKind::IntDec
            | TokenKind::IntHex
            | TokenKind::IntBin
            | TokenKind::IntOct
            | TokenKind::Float => token.lexeme(source).to_string(),
            TokenKind::DocComment => "documentation comment".to_string(),
            TokenKind::StringText | TokenKind::StringEscape => "string content".to_string(),
            TokenKind::InterpStart | TokenKind::InterpEnd => "string interpolation".to_string(),
            TokenKind::StringEnd | TokenKind::MultilineStringEnd => "end of string".to_string(),
            TokenKind::RawString | TokenKind::StringStart | TokenKind::MultilineStringStart => {
                "string literal".to_string()
            }
            TokenKind::Char => {
                let content = token.char_content(source);
                if content.is_empty() {
                    "character literal".to_string()
                } else {
                    content.to_string()
                }
            }
            TokenKind::Eof => "end of file".to_string(),
            TokenKind::Error(_) => "invalid character".to_string(),
            // Punctuation, keywords and type keywords spell the source lexeme
            // (e.g. `=`, `{`, `func`, `int`), so no internal name ever surfaces.
            other => {
                let lexeme = token.lexeme(source);
                if lexeme.is_empty() {
                    other.name().to_string()
                } else {
                    lexeme.to_string()
                }
            }
        }
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            TokenKind::IdentValue => "IDENT_VALUE",
            TokenKind::IdentType => "IDENT_TYPE",
            TokenKind::DocComment => "DOC_COMMENT",
            TokenKind::IntDec => "INT_DEC",
            TokenKind::IntHex => "INT_HEX",
            TokenKind::IntBin => "INT_BIN",
            TokenKind::IntOct => "INT_OCT",
            TokenKind::Float => "FLOAT",
            TokenKind::StringStart => "STRING_START",
            TokenKind::StringText => "STRING_TEXT",
            TokenKind::StringEscape => "STRING_ESCAPE",
            TokenKind::InterpStart => "INTERP_START",
            TokenKind::InterpEnd => "INTERP_END",
            TokenKind::StringEnd => "STRING_END",
            TokenKind::RawString => "RAW_STRING",
            TokenKind::MultilineStringStart => "MULTILINE_STRING_START",
            TokenKind::MultilineStringEnd => "MULTILINE_STRING_END",
            TokenKind::Char => "CHAR",
            TokenKind::KwIf => "KW_IF",
            TokenKind::KwElse => "KW_ELSE",
            TokenKind::KwFor => "KW_FOR",
            TokenKind::KwIn => "KW_IN",
            TokenKind::KwWhile => "KW_WHILE",
            TokenKind::KwMatch => "KW_MATCH",
            TokenKind::KwReturn => "KW_RETURN",
            TokenKind::KwBreak => "KW_BREAK",
            TokenKind::KwContinue => "KW_CONTINUE",
            TokenKind::KwFunc => "KW_FUNC",
            TokenKind::KwAsync => "KW_ASYNC",
            TokenKind::KwAwait => "KW_AWAIT",
            TokenKind::KwStruct => "KW_STRUCT",
            TokenKind::KwEnum => "KW_ENUM",
            TokenKind::KwInterface => "KW_INTERFACE",
            TokenKind::KwConst => "KW_CONST",
            TokenKind::KwType => "KW_TYPE",
            TokenKind::KwModule => "KW_MODULE",
            TokenKind::KwImport => "KW_IMPORT",
            TokenKind::KwFrom => "KW_FROM",
            TokenKind::KwAs => "KW_AS",
            TokenKind::KwPublic => "KW_PUBLIC",
            TokenKind::KwExtern => "KW_EXTERN",
            TokenKind::KwUnsafe => "KW_UNSAFE",
            TokenKind::KwWhere => "KW_WHERE",
            TokenKind::KwCatch => "KW_CATCH",
            TokenKind::KwIs => "KW_IS",
            TokenKind::KwSet => "KW_SET",
            TokenKind::KwOwn => "KW_OWN",
            TokenKind::KwMut => "KW_MUT",
            TokenKind::KwShared => "KW_SHARED",
            TokenKind::KwRef => "KW_REF",
            TokenKind::KwSelf => "KW_SELF",
            TokenKind::KwPtr => "KW_PTR",
            TokenKind::KwAlloc => "KW_ALLOC",
            TokenKind::KwFree => "KW_FREE",
            TokenKind::KwDefer => "KW_DEFER",
            TokenKind::KwErrdefer => "KW_ERRDEFER",
            TokenKind::KwLet => "KW_LET",
            TokenKind::KwImpl => "KW_IMPL",
            TokenKind::TypeInt => "TYPE_INT",
            TokenKind::TypeUint => "TYPE_UINT",
            TokenKind::TypeFloat => "TYPE_FLOAT",
            TokenKind::TypeI8 => "TYPE_I8",
            TokenKind::TypeI16 => "TYPE_I16",
            TokenKind::TypeI32 => "TYPE_I32",
            TokenKind::TypeI64 => "TYPE_I64",
            TokenKind::TypeU8 => "TYPE_U8",
            TokenKind::TypeU16 => "TYPE_U16",
            TokenKind::TypeU32 => "TYPE_U32",
            TokenKind::TypeU64 => "TYPE_U64",
            TokenKind::TypeF32 => "TYPE_F32",
            TokenKind::TypeF64 => "TYPE_F64",
            TokenKind::TypeBool => "TYPE_BOOL",
            TokenKind::TypeByte => "TYPE_BYTE",
            TokenKind::TypeChar => "TYPE_CHAR",
            TokenKind::TypeStr => "TYPE_STR",
            TokenKind::TypeAny => "TYPE_ANY",
            TokenKind::TypeErr => "TYPE_ERR",
            TokenKind::BoolTrue => "BOOL_TRUE",
            TokenKind::BoolFalse => "BOOL_FALSE",
            TokenKind::Nil => "NIL",
            TokenKind::LParen => "LPAREN",
            TokenKind::RParen => "RPAREN",
            TokenKind::LBracket => "LBRACKET",
            TokenKind::RBracket => "RBRACKET",
            TokenKind::LBrace => "LBRACE",
            TokenKind::RBrace => "RBRACE",
            TokenKind::Comma => "COMMA",
            TokenKind::Dot => "DOT",
            TokenKind::Colon => "COLON",
            TokenKind::Semicolon => "SEMICOLON",
            TokenKind::At => "AT",
            TokenKind::Plus => "PLUS",
            TokenKind::Minus => "MINUS",
            TokenKind::Star => "STAR",
            TokenKind::Slash => "SLASH",
            TokenKind::Percent => "PERCENT",
            TokenKind::Amp => "AMP",
            TokenKind::Pipe => "PIPE",
            TokenKind::Caret => "CARET",
            TokenKind::Lt => "LT",
            TokenKind::Gt => "GT",
            TokenKind::Equal => "EQUAL",
            TokenKind::Bang => "BANG",
            TokenKind::Tilde => "TILDE",
            TokenKind::Question => "QUESTION",
            TokenKind::SafeDot => "SAFE_DOT",
            TokenKind::SafeIndexStart => "SAFE_INDEX_START",
            TokenKind::NullCoalesce => "NULL_COALESCE",
            TokenKind::LogicalOr => "LOGICAL_OR",
            TokenKind::LogicalAnd => "LOGICAL_AND",
            TokenKind::FatArrow => "FAT_ARROW",
            TokenKind::PlusEqual => "PLUS_EQUAL",
            TokenKind::MinusEqual => "MINUS_EQUAL",
            TokenKind::StarEqual => "STAR_EQUAL",
            TokenKind::SlashEqual => "SLASH_EQUAL",
            TokenKind::PercentEqual => "PERCENT_EQUAL",
            TokenKind::AmpEqual => "AMP_EQUAL",
            TokenKind::PipeEqual => "PIPE_EQUAL",
            TokenKind::CaretEqual => "CARET_EQUAL",
            TokenKind::ShiftLeftEqual => "SHIFT_LEFT_EQUAL",
            TokenKind::ShiftRightEqual => "SHIFT_RIGHT_EQUAL",
            TokenKind::ShiftLeft => "SHIFT_LEFT",
            TokenKind::ShiftRight => "SHIFT_RIGHT",
            TokenKind::EqualEqual => "EQUAL_EQUAL",
            TokenKind::BangEqual => "BANG_EQUAL",
            TokenKind::LtEqual => "LT_EQUAL",
            TokenKind::GtEqual => "GT_EQUAL",
            TokenKind::RangeInclusive => "RANGE_INCLUSIVE",
            TokenKind::RangeExclusive => "RANGE_EXCLUSIVE",
            TokenKind::Ellipsis => "ELLIPSIS",
            TokenKind::Arrow => "ARROW",
            TokenKind::Eof => "EOF",
            TokenKind::Error(_) => "ERROR",
        }
    }

    #[must_use]
    pub fn can_end_statement(self) -> bool {
        TOKEN_FLAGS_TABLE[self.index()].can_end
    }

    #[must_use]
    pub fn prevents_semicolon_before(self) -> bool {
        TOKEN_FLAGS_TABLE[self.index()].prevents
    }
}

#[derive(Clone, Copy)]
struct TokenFlags {
    can_end: bool,
    prevents: bool,
}

static TOKEN_FLAGS_TABLE: [TokenFlags; TokenKind::COUNT] = {
    let mut table = [TokenFlags {
        can_end: false,
        prevents: false,
    }; TokenKind::COUNT];
    let mut i = 0;
    while i < TokenKind::COUNT {
        let kind = TokenKind::index_to_token_kind(i);
        let can_end = matches!(
            kind,
            TokenKind::IdentValue
                | TokenKind::IdentType
                | TokenKind::TypeInt
                | TokenKind::TypeUint
                | TokenKind::TypeFloat
                | TokenKind::TypeI8
                | TokenKind::TypeI16
                | TokenKind::TypeI32
                | TokenKind::TypeI64
                | TokenKind::TypeU8
                | TokenKind::TypeU16
                | TokenKind::TypeU32
                | TokenKind::TypeU64
                | TokenKind::TypeF32
                | TokenKind::TypeF64
                | TokenKind::TypeBool
                | TokenKind::TypeByte
                | TokenKind::TypeChar
                | TokenKind::TypeStr
                | TokenKind::TypeAny
                | TokenKind::TypeErr
                | TokenKind::IntDec
                | TokenKind::IntHex
                | TokenKind::IntBin
                | TokenKind::IntOct
                | TokenKind::Float
                | TokenKind::BoolTrue
                | TokenKind::BoolFalse
                | TokenKind::Nil
                | TokenKind::Char
                | TokenKind::StringEnd
                | TokenKind::RawString
                | TokenKind::MultilineStringEnd
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::Question
                | TokenKind::KwReturn
                | TokenKind::KwBreak
                | TokenKind::KwContinue
        );
        let prevents = matches!(
            kind,
            TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::Comma
                | TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Percent
                | TokenKind::Amp
                | TokenKind::Pipe
                | TokenKind::Caret
                | TokenKind::ShiftLeft
                | TokenKind::ShiftRight
                | TokenKind::Dot
                | TokenKind::SafeDot
                | TokenKind::SafeIndexStart
                | TokenKind::Question
                | TokenKind::NullCoalesce
                | TokenKind::LogicalOr
                | TokenKind::LogicalAnd
                | TokenKind::Equal
                | TokenKind::EqualEqual
                | TokenKind::BangEqual
                | TokenKind::Lt
                | TokenKind::Gt
                | TokenKind::LtEqual
                | TokenKind::GtEqual
                | TokenKind::RangeExclusive
                | TokenKind::RangeInclusive
                | TokenKind::FatArrow
                | TokenKind::Arrow
                | TokenKind::KwElse
                | TokenKind::KwCatch
                | TokenKind::KwAs
                | TokenKind::KwWhere
        );
        table[i] = TokenFlags { can_end, prevents };
        i += 1;
    }
    table
};
