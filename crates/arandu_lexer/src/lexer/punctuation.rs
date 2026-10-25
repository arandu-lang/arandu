use crate::TokenKind;

use super::ident::{is_ident_continue, is_ident_start, keyword_kind};

pub(super) fn peek_kind_from(rest: &str) -> TokenKind {
    if rest.chars().next().is_some_and(is_ident_start) {
        let end = rest
            .char_indices()
            .find_map(|(index, ch)| (!is_ident_continue(ch)).then_some(index))
            .unwrap_or(rest.len());
        if let Some(kind) = keyword_kind(&rest[..end]) {
            return kind;
        }
    }

    token_kind_from_prefix(rest.as_bytes()).map_or(TokenKind::Eof, |(kind, _)| kind)
}

pub(super) fn token_kind_from_prefix(bytes: &[u8]) -> Option<(TokenKind, usize)> {
    match bytes {
        // 3 caracteres
        [b'<', b'<', b'=', ..] => Some((TokenKind::ShiftLeftEqual, 3)),
        [b'>', b'>', b'=', ..] => Some((TokenKind::ShiftRightEqual, 3)),
        [b'.', b'.', b'.', ..] => Some((TokenKind::Ellipsis, 3)),
        [b'.', b'.', b'=', ..] => Some((TokenKind::RangeInclusive, 3)),

        // 2 caracteres
        [b'?', b'.', ..] => Some((TokenKind::SafeDot, 2)),
        [b'?', b'[', ..] => Some((TokenKind::SafeIndexStart, 2)),
        [b'?', b'?', ..] => Some((TokenKind::NullCoalesce, 2)),
        [b'|', b'|', ..] => Some((TokenKind::LogicalOr, 2)),
        [b'|', b'>', ..] => Some((TokenKind::PipeGt, 2)),
        [b'&', b'&', ..] => Some((TokenKind::LogicalAnd, 2)),
        [b'=', b'>', ..] => Some((TokenKind::FatArrow, 2)),
        [b'-', b'>', ..] => Some((TokenKind::Arrow, 2)),

        [b'+', b'=', ..] => Some((TokenKind::PlusEqual, 2)),
        [b'-', b'=', ..] => Some((TokenKind::MinusEqual, 2)),
        [b'*', b'=', ..] => Some((TokenKind::StarEqual, 2)),
        [b'/', b'=', ..] => Some((TokenKind::SlashEqual, 2)),
        [b'%', b'=', ..] => Some((TokenKind::PercentEqual, 2)),
        [b'&', b'=', ..] => Some((TokenKind::AmpEqual, 2)),
        [b'|', b'=', ..] => Some((TokenKind::PipeEqual, 2)),
        [b'^', b'=', ..] => Some((TokenKind::CaretEqual, 2)),

        [b'<', b'<', ..] => Some((TokenKind::ShiftLeft, 2)),
        [b'>', b'>', ..] => Some((TokenKind::ShiftRight, 2)),

        [b'=', b'=', ..] => Some((TokenKind::EqualEqual, 2)),
        [b'!', b'=', ..] => Some((TokenKind::BangEqual, 2)),
        [b'<', b'=', ..] => Some((TokenKind::LtEqual, 2)),
        [b'>', b'=', ..] => Some((TokenKind::GtEqual, 2)),

        [b'.', b'.', ..] => Some((TokenKind::RangeExclusive, 2)),

        // 1 caractere
        [b'(', ..] => Some((TokenKind::LParen, 1)),
        [b')', ..] => Some((TokenKind::RParen, 1)),
        [b'[', ..] => Some((TokenKind::LBracket, 1)),
        [b']', ..] => Some((TokenKind::RBracket, 1)),
        [b'{', ..] => Some((TokenKind::LBrace, 1)),
        [b'}', ..] => Some((TokenKind::RBrace, 1)),

        [b',', ..] => Some((TokenKind::Comma, 1)),
        [b'.', ..] => Some((TokenKind::Dot, 1)),
        [b':', ..] => Some((TokenKind::Colon, 1)),
        [b';', ..] => Some((TokenKind::Semicolon, 1)),
        [b'@', ..] => Some((TokenKind::At, 1)),

        [b'+', ..] => Some((TokenKind::Plus, 1)),
        [b'-', ..] => Some((TokenKind::Minus, 1)),
        [b'*', ..] => Some((TokenKind::Star, 1)),
        [b'/', ..] => Some((TokenKind::Slash, 1)),
        [b'%', ..] => Some((TokenKind::Percent, 1)),

        [b'&', ..] => Some((TokenKind::Amp, 1)),
        [b'|', ..] => Some((TokenKind::Pipe, 1)),
        [b'^', ..] => Some((TokenKind::Caret, 1)),

        [b'<', ..] => Some((TokenKind::Lt, 1)),
        [b'>', ..] => Some((TokenKind::Gt, 1)),

        [b'=', ..] => Some((TokenKind::Equal, 1)),
        [b'!', ..] => Some((TokenKind::Bang, 1)),
        [b'~', ..] => Some((TokenKind::Tilde, 1)),
        [b'?', ..] => Some((TokenKind::Question, 1)),

        _ => None,
    }
}
