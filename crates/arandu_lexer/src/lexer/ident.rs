use crate::TokenKind;

pub(super) const FLAG_IDENT_START: u8 = 1 << 0;
pub(super) const FLAG_IDENT_CONTINUE: u8 = 1 << 1;
pub(super) const FLAG_WHITESPACE: u8 = 1 << 2;
pub(super) const FLAG_DIGIT: u8 = 1 << 3;

pub(super) const CHAR_PROPERTIES: [u8; 128] = {
    let mut table = [0u8; 128];

    // Fill whitespace
    table[b' ' as usize] |= FLAG_WHITESPACE;
    table[b'\t' as usize] |= FLAG_WHITESPACE;
    table[b'\r' as usize] |= FLAG_WHITESPACE;
    table[b'\n' as usize] |= FLAG_WHITESPACE;

    // Fill identifier start (a-z, A-Z, _)
    let mut i = b'a';
    while i <= b'z' {
        table[i as usize] |= FLAG_IDENT_START | FLAG_IDENT_CONTINUE;
        i += 1;
    }
    let mut i = b'A';
    while i <= b'Z' {
        table[i as usize] |= FLAG_IDENT_START | FLAG_IDENT_CONTINUE;
        i += 1;
    }
    table[b'_' as usize] |= FLAG_IDENT_START | FLAG_IDENT_CONTINUE;

    // Fill digits (0-9)
    let mut i = b'0';
    while i <= b'9' {
        table[i as usize] |= FLAG_IDENT_CONTINUE | FLAG_DIGIT;
        i += 1;
    }

    table
};

#[inline]
pub(crate) fn is_ident_start(ch: char) -> bool {
    let val = ch as u32;
    if val < 128 {
        (CHAR_PROPERTIES[val as usize] & FLAG_IDENT_START) != 0
    } else {
        unicode_ident::is_xid_start(ch)
    }
}

#[inline]
pub(crate) fn is_ident_continue(ch: char) -> bool {
    let val = ch as u32;
    if val < 128 {
        (CHAR_PROPERTIES[val as usize] & FLAG_IDENT_CONTINUE) != 0
    } else {
        unicode_ident::is_xid_continue(ch)
    }
}

#[inline]
pub(super) fn is_digit(ch: char) -> bool {
    let val = ch as u32;
    val < 128 && (CHAR_PROPERTIES[val as usize] & FLAG_DIGIT) != 0
}

pub(crate) fn keyword_kind(text: &str) -> Option<TokenKind> {
    Some(match text {
        "if" => TokenKind::KwIf,
        "else" => TokenKind::KwElse,
        "for" => TokenKind::KwFor,
        "in" => TokenKind::KwIn,
        "while" => TokenKind::KwWhile,
        "match" => TokenKind::KwMatch,
        "return" => TokenKind::KwReturn,
        "break" => TokenKind::KwBreak,
        "continue" => TokenKind::KwContinue,
        "func" => TokenKind::KwFunc,
        "async" => TokenKind::KwAsync,
        "comptime" => TokenKind::KwComptime,
        "await" => TokenKind::KwAwait,
        "struct" => TokenKind::KwStruct,
        "enum" => TokenKind::KwEnum,
        "interface" => TokenKind::KwInterface,
        "const" => TokenKind::KwConst,
        "type" => TokenKind::KwType,
        "module" => TokenKind::KwModule,
        "import" => TokenKind::KwImport,
        "as" => TokenKind::KwAs,
        "public" => TokenKind::KwPublic,
        "internal" => TokenKind::KwInternal,
        "private" => TokenKind::KwPrivate,
        "extern" => TokenKind::KwExtern,
        "unsafe" => TokenKind::KwUnsafe,
        "where" => TokenKind::KwWhere,
        "catch" => TokenKind::KwCatch,
        "is" => TokenKind::KwIs,
        "let" => TokenKind::KwLet,
        "impl" => TokenKind::KwImpl,
        "set" => TokenKind::KwSet,
        "own" => TokenKind::KwOwn,
        "mut" => TokenKind::KwMut,
        "shared" => TokenKind::KwShared,
        "ref" => TokenKind::KwRef,
        "self" => TokenKind::KwSelf,
        "ptr" => TokenKind::KwPtr,
        "defer" => TokenKind::KwDefer,
        "errdefer" => TokenKind::KwErrdefer,
        "int" => TokenKind::TypeInt,
        "uint" => TokenKind::TypeUint,
        "isize" => TokenKind::TypeIsize,
        "usize" => TokenKind::TypeUsize,
        "float" => TokenKind::TypeFloat,
        "i8" => TokenKind::TypeI8,
        "i16" => TokenKind::TypeI16,
        "i32" => TokenKind::TypeI32,
        "i64" => TokenKind::TypeI64,
        "u8" => TokenKind::TypeU8,
        "u16" => TokenKind::TypeU16,
        "u32" => TokenKind::TypeU32,
        "u64" => TokenKind::TypeU64,
        "f32" => TokenKind::TypeF32,
        "f64" => TokenKind::TypeF64,
        "bool" => TokenKind::TypeBool,
        "byte" => TokenKind::TypeByte,
        "char" => TokenKind::TypeChar,
        "str" => TokenKind::TypeStr,
        "any" => TokenKind::TypeAny,
        "true" => TokenKind::BoolTrue,
        "false" => TokenKind::BoolFalse,
        "nil" => TokenKind::Nil,
        _ => return None,
    })
}

/// Returns whether a character is an unescaped bidirectional Unicode formatting control character
/// that can be used in Trojan Source attacks (CVE-2021-42574, CWE-1307).
#[inline]
pub(crate) fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{202A}' // LEFT-TO-RIGHT EMBEDDING (LRE)
        | '\u{202B}' // RIGHT-TO-LEFT EMBEDDING (RLE)
        | '\u{202C}' // POP DIRECTIONAL FORMATTING (PDF)
        | '\u{202D}' // LEFT-TO-RIGHT OVERRIDE (LRO)
        | '\u{202E}' // RIGHT-TO-LEFT OVERRIDE (RLO)
        | '\u{2066}' // LEFT-TO-RIGHT ISOLATE (LRI)
        | '\u{2067}' // RIGHT-TO-LEFT ISOLATE (RLI)
        | '\u{2068}' // FIRST STRONG ISOLATE (FSI)
        | '\u{2069}' // POP DIRECTIONAL ISOLATE (PDI)
        | '\u{200E}' // LEFT-TO-RIGHT MARK (LRM)
        | '\u{200F}' // RIGHT-TO-LEFT MARK (RLM)
        | '\u{061C}' // ARABIC LETTER MARK (ALM)
    )
}
