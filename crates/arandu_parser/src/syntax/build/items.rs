use arandu_lexer::{Token, TokenKind};

/// True when `tok` is the soft keyword `from` lexed as a plain identifier.
pub(super) fn is_soft_from_tok(source: &str, tok: &Token) -> bool {
    tok.kind == TokenKind::IdentValue && tok.lexeme(source) == "from"
}

pub(super) fn is_item_start_keyword(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::KwModule
            | TokenKind::KwImport
            | TokenKind::KwFrom
            | TokenKind::KwFunc
            | TokenKind::KwStruct
            | TokenKind::KwEnum
            | TokenKind::KwInterface
            | TokenKind::KwConst
            | TokenKind::KwType
            | TokenKind::KwExtern // Note: KwAsync / KwPublic are prefixes — folded via `expand_item_start_left`.
    )
}

/// Heuristic top-level item ranges from tokens (**brace-depth aware**).
///
/// Starts an item at module/import/decl keywords (optionally after `public`)
/// only when `{}` depth is 0 — so `func` methods inside `interface` / `extern`
/// / `struct` are not treated as new top-level items.
#[must_use]
pub fn find_top_level_item_spans(
    source: &str,
    tokens: &[Token],
    source_len: u32,
) -> Vec<(u32, u32)> {
    let mut starts: Vec<u32> = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    // Last item-start keyword seen at depth 0 (to suppress `import` after `from`).
    let mut last_item_kw: Option<TokenKind> = None;
    while i < tokens.len() {
        let tok = &tokens[i];
        if matches!(tok.kind, TokenKind::Eof | TokenKind::Error(_)) {
            i += 1;
            continue;
        }

        // Keyword check at current depth (before this token modifies depth).
        // Item openers: module/import/from/func/struct/… — not `async`/`public` alone
        // (those are prefixes). Leading `@attr` / `public` / `async` are folded left.
        if depth == 0 && (is_item_start_keyword(tok.kind) || is_soft_from_tok(source, tok)) {
            if matches!(tok.kind, TokenKind::KwImport)
                && matches!(last_item_kw, Some(TokenKind::KwFrom))
            {
                // `from path import {…}` — do not split at `import`.
            } else {
                let start = expand_item_start_left(tokens, i);
                starts.push(start);
                // `from` now lexes as `IdentValue`; re-tag it so the `from … import`
                // suppression above still recognizes the pair.
                last_item_kw = Some(if is_soft_from_tok(source, tok) {
                    TokenKind::KwFrom
                } else {
                    tok.kind
                });
                i += 1;
                continue;
            }
        }

        match tok.kind {
            TokenKind::LBrace => depth += 1,
            TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    last_item_kw = None;
                }
            }
            _ => {}
        }
        i += 1;
    }

    if starts.is_empty() {
        return Vec::new();
    }

    let mut spans = Vec::with_capacity(starts.len());
    for (idx, &start) in starts.iter().enumerate() {
        let end = if idx + 1 < starts.len() {
            starts[idx + 1]
        } else {
            // End of last item: last non-eof token end or source_len
            tokens
                .iter()
                .rev()
                .find(|t| !matches!(t.kind, TokenKind::Eof))
                .map(|t| t.start.saturating_add(t.len))
                .unwrap_or(source_len)
                .min(source_len)
        };
        if end > start {
            spans.push((start, end));
        }
    }
    spans
}

/// Include leading `@attr…`, `public`, `async`, and doc comments before an item keyword.
///
/// Doc comments must attach to the **following** item. Otherwise the previous
/// item's span absorbs `/// …` (end = next start), hand-lower sees leftover
/// tokens after `}`, falls back to RD, and fails mid-field (e.g. gen_arena.aru).
fn expand_item_start_left(tokens: &[Token], kw_index: usize) -> u32 {
    let mut idx = kw_index;
    let mut start = tokens[kw_index].start;
    while idx > 0 {
        let prev = &tokens[idx - 1];
        match prev.kind {
            // ASI often inserts `;` after bare `@Attr` / names before `public`/`func`.
            TokenKind::Semicolon => {
                idx -= 1;
            }
            TokenKind::DocComment => {
                start = prev.start;
                idx -= 1;
            }
            TokenKind::KwAsync | TokenKind::KwPublic => {
                start = prev.start;
                idx -= 1;
            }
            TokenKind::RParen => {
                // Walk back a `(…)` group (attribute args), then name + `@`.
                let mut depth = 1i32;
                start = prev.start;
                idx -= 1;
                while idx > 0 && depth > 0 {
                    match tokens[idx - 1].kind {
                        TokenKind::RParen => depth += 1,
                        TokenKind::LParen => depth -= 1,
                        _ => {}
                    }
                    start = tokens[idx - 1].start;
                    idx -= 1;
                }
                // optional attr name
                if idx > 0
                    && matches!(
                        tokens[idx - 1].kind,
                        TokenKind::IdentValue | TokenKind::IdentType
                    )
                {
                    start = tokens[idx - 1].start;
                    idx -= 1;
                }
                if idx > 0 && matches!(tokens[idx - 1].kind, TokenKind::At) {
                    start = tokens[idx - 1].start;
                    idx -= 1;
                } else {
                    break;
                }
            }
            TokenKind::IdentValue | TokenKind::IdentType if idx >= 2 => {
                if matches!(tokens[idx - 2].kind, TokenKind::At) {
                    start = tokens[idx - 2].start;
                    idx -= 2;
                } else {
                    break;
                }
            }
            TokenKind::At => {
                start = prev.start;
                idx -= 1;
            }
            _ => break,
        }
    }
    start
}
