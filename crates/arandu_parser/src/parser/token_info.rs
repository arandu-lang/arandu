use super::Parser;
use crate::StringPart;
use arandu_lexer::{Span, TokenKind};
use smol_str::SmolStr;

pub(crate) fn is_type_token(kind: &TokenKind) -> bool {
    TOKEN_INFO_TABLE[kind.index()].is_type_token
}

pub(crate) fn primitive_type_name(kind: &TokenKind) -> Option<&'static str> {
    TOKEN_INFO_TABLE[kind.index()].primitive_type_name
}

#[derive(Clone, Copy)]
struct TokenInfo {
    is_type_token: bool,
    is_contextual_module_segment: bool,
    primitive_type_name: Option<&'static str>,
}

static TOKEN_INFO_TABLE: [TokenInfo; TokenKind::COUNT] = {
    let mut table = [TokenInfo {
        is_type_token: false,
        is_contextual_module_segment: false,
        primitive_type_name: None,
    }; TokenKind::COUNT];
    let mut i = 0;
    while i < TokenKind::COUNT {
        let kind = TokenKind::index_to_token_kind(i);
        let prim = match kind {
            TokenKind::TypeInt => Some("int"),
            TokenKind::TypeUint => Some("uint"),
            TokenKind::TypeIsize => Some("isize"),
            TokenKind::TypeUsize => Some("usize"),
            TokenKind::TypeFloat => Some("float"),
            TokenKind::TypeI8 => Some("i8"),
            TokenKind::TypeI16 => Some("i16"),
            TokenKind::TypeI32 => Some("i32"),
            TokenKind::TypeI64 => Some("i64"),
            TokenKind::TypeU8 => Some("u8"),
            TokenKind::TypeU16 => Some("u16"),
            TokenKind::TypeU32 => Some("u32"),
            TokenKind::TypeU64 => Some("u64"),
            TokenKind::TypeF32 => Some("f32"),
            TokenKind::TypeF64 => Some("f64"),
            TokenKind::TypeBool => Some("bool"),
            TokenKind::TypeByte => Some("byte"),
            TokenKind::TypeChar => Some("char"),
            TokenKind::TypeStr => Some("str"),
            TokenKind::TypeAny => Some("any"),
            TokenKind::TypeErr => Some("Err"),
            _ => None,
        };
        let is_type = prim.is_some()
            || matches!(
                kind,
                TokenKind::IdentType
                    | TokenKind::IdentValue
                    | TokenKind::KwOwn
                    | TokenKind::KwRef
                    | TokenKind::KwMut
            );
        let is_contextual = matches!(
            kind,
            TokenKind::IdentType
                | TokenKind::KwIf
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
                | TokenKind::KwDefer
                | TokenKind::KwErrdefer
                | TokenKind::KwLet
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
        );
        table[i] = TokenInfo {
            is_type_token: is_type,
            is_contextual_module_segment: is_contextual,
            primitive_type_name: prim,
        };
        i += 1;
    }
    table
};

pub(crate) fn user_facing_token_name(name: &str) -> &'static str {
    token_expectation_names(name)
        .first()
        .copied()
        .unwrap_or("token")
}

pub(crate) fn token_expectation_names(name: &str) -> &'static [&'static str] {
    match name {
        // Punctuation and symbol spellings (never the internal kind name).
        "AMP" => &["&"],
        "ARROW" => &["->"],
        "AT" => &["@"],
        "COLON" => &[":"],
        "COMMA" => &[","],
        "DOT" => &["."],
        "ELLIPSIS" => &["..."],
        "EQUAL" => &["="],
        "FAT_ARROW" => &["=>"],
        "GT" => &[">"],
        "INTERP_END" | "RBRACE" => &["}"],
        "LBRACE" => &["{"],
        "LBRACKET" => &["["],
        "LPAREN" => &["("],
        "LT" => &["<"],
        "PIPE" => &["|"],
        "PLUS" => &["+"],
        "QUESTION" => &["?"],
        "RANGE_EXCLUSIVE" => &[".."],
        "RANGE_INCLUSIVE" => &["..="],
        "RBRACKET" => &["]"],
        "RPAREN" => &[")"],
        "EOF" => &["end of file"],
        "SEMICOLON" => &["statement terminator"],
        "STRING_END" => &["end of string"],
        "STRING_START" => &["string literal"],
        // Keywords and type keywords spell their source text.
        "KW_AS" => &["as"],
        "KW_ASYNC" => &["async"],
        "KW_BREAK" => &["break"],
        "KW_CONST" => &["const"],
        "KW_CONTINUE" => &["continue"],
        "KW_DEFER" => &["defer"],
        "KW_ELSE" => &["else"],
        "KW_ENUM" => &["enum"],
        "KW_ERRDEFER" => &["errdefer"],
        "KW_EXTERN" => &["extern"],
        "KW_FOR" => &["for"],
        "KW_FROM" => &["from"],
        "KW_FUNC" => &["func"],
        "KW_IF" => &["if"],
        "KW_IMPL" => &["impl"],
        "KW_IMPORT" => &["import"],
        "KW_IN" => &["in"],
        "KW_INTERFACE" => &["interface"],
        "KW_IS" => &["is"],
        "KW_LET" => &["let"],
        "KW_MATCH" => &["match"],
        "KW_MODULE" => &["module"],
        "KW_MUT" => &["mut"],
        "KW_OWN" => &["own"],
        "KW_PTR" => &["ptr"],
        "KW_PUBLIC" => &["public"],
        "KW_INTERNAL" => &["internal"],
        "KW_PRIVATE" => &["private"],
        "KW_REF" => &["ref"],
        "KW_RETURN" => &["return"],
        "KW_SELF" => &["self"],
        "KW_SET" => &["set"],
        "KW_SHARED" => &["shared"],
        "KW_STRUCT" => &["struct"],
        "KW_TYPE" => &["type"],
        "KW_UNSAFE" => &["unsafe"],
        "KW_WHERE" => &["where"],
        "KW_WHILE" => &["while"],
        _ => &["token"],
    }
}

pub(crate) fn merge_text_parts(parts: Vec<StringPart>) -> Vec<StringPart> {
    let mut merged = Vec::new();
    let mut pending: Option<(arandu_lexer::Span, String)> = None;
    for part in parts {
        match part {
            StringPart::Text { span, text } => {
                if let Some((existing_span, existing)) = pending.as_mut() {
                    existing.push_str(text.as_str());
                    *existing_span = span_between(*existing_span, span);
                } else {
                    pending = Some((span, text.to_string()));
                }
            }
            other => {
                if let Some((span, text)) = pending.take() {
                    merged.push(StringPart::Text {
                        span,
                        text: SmolStr::new(text),
                    });
                }
                merged.push(other);
            }
        }
    }
    if let Some((span, text)) = pending.take() {
        merged.push(StringPart::Text {
            span,
            text: SmolStr::new(text),
        });
    }
    merged
}

pub(crate) fn is_contextual_module_segment(kind: &TokenKind) -> bool {
    TOKEN_INFO_TABLE[kind.index()].is_contextual_module_segment
}

pub(crate) fn is_primitive_type_token(kind: &TokenKind) -> bool {
    TOKEN_INFO_TABLE[kind.index()].primitive_type_name.is_some()
}

pub(crate) fn span_between(start: Span, end: Span) -> Span {
    Span {
        file_id: start.file_id,
        start: start.start,
        end: end.end,
    }
}

pub(crate) struct Checkpoint {
    pos: usize,
    events_len: usize,
    diagnostics_len: usize,
    pending_docs_len: usize,
    pool: crate::ast::ast_pool::PoolCheckpoint,
}

impl Checkpoint {
    pub(crate) fn new(parser: &Parser<'_>) -> Self {
        let pos = parser.pos;
        let events_len = parser.events.as_ref().map_or(0, |evs| evs.len());
        let diagnostics_len = parser.diagnostics.len();
        let pending_docs_len = parser.pending_docs.len();
        Self {
            pos,
            events_len,
            diagnostics_len,
            pending_docs_len,
            pool: parser.pool.checkpoint(),
        }
    }

    pub(crate) fn rollback(self, parser: &mut Parser<'_>) {
        parser.pos = self.pos;
        if let Some(evs) = &mut parser.events {
            evs.truncate(self.events_len);
        }
        parser.diagnostics.truncate(self.diagnostics_len);
        parser.pending_docs.truncate(self.pending_docs_len);
        self.pool.rollback(&mut parser.pool);
    }
}
