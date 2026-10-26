use std::fmt;

use super::{Token, TokenKind};

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Error(code) => write!(f, "ERROR({code:?})"),
            other => f.write_str(other.name()),
        }
    }
}

impl TokenKind {
    pub const COUNT: usize = 141;

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
                | TokenKind::KwComptime
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
                | TokenKind::KwInternal
                | TokenKind::KwPrivate
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

    /// Returns whether this token represents a built-in primitive type keyword
    /// (`str`, `char`, `int`, `float`, `u8`..`u64`, `i8`..`i64`, `bool`, etc.).
    #[must_use]
    pub const fn is_primitive_type(self) -> bool {
        matches!(
            self,
            TokenKind::TypeInt
                | TokenKind::TypeUint
                | TokenKind::TypeIsize
                | TokenKind::TypeUsize
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
                | TokenKind::KwPtr
        )
    }

    /// Returns whether this token can name a member after `.` or `?.`.
    ///
    /// Keywords remain reserved in ordinary identifier positions, but become
    /// contextual after member access (`value.set`, `Type.match`, ...).
    #[must_use]
    pub const fn is_contextual_member_name(self) -> bool {
        matches!(self, TokenKind::IdentValue | TokenKind::IdentType) || self.is_keyword()
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
            TokenKind::ByteChar => format!("BYTE_CHAR({})", token.byte_content(source)),
            TokenKind::ByteString => {
                format!("BYTE_STRING({})", token.byte_string_content(source))
            }
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
            TokenKind::RawString
            | TokenKind::ByteString
            | TokenKind::StringStart
            | TokenKind::MultilineStringStart => "string literal".to_string(),
            TokenKind::Char => {
                let content = token.char_content(source);
                if content.is_empty() {
                    "character literal".to_string()
                } else {
                    content.to_string()
                }
            }
            TokenKind::ByteChar => {
                let content = token.byte_content(source);
                if content.is_empty() {
                    "byte literal".to_string()
                } else {
                    format!("b'{content}'")
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
            TokenKind::ByteChar => "BYTE_CHAR",
            TokenKind::ByteString => "BYTE_STRING",
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
            TokenKind::KwComptime => "KW_COMPTIME",
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
            TokenKind::KwInternal => "KW_INTERNAL",
            TokenKind::KwPrivate => "KW_PRIVATE",
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
            TokenKind::TypeIsize => "TYPE_ISIZE",
            TokenKind::TypeUsize => "TYPE_USIZE",
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
            TokenKind::PipeGt => "PIPE_GT",
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
                | TokenKind::TypeIsize
                | TokenKind::TypeUsize
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
                | TokenKind::ByteChar
                | TokenKind::ByteString
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
                | TokenKind::PipeGt
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
