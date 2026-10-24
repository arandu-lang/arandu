//! Formatter oracle: `arandu_fmt` must be idempotent, its edit lists must
//! reproduce the formatted text exactly, and formatting a lexically clean,
//! lowerable source must preserve the token stream (spelling, kind and
//! automatic semicolon insertion). Comments are checked by count so a
//! formatter rewrite cannot silently drop documentation.

use arandu_fmt::{format_edits, format_edits_in_range, format_source, TextEdit};
use arandu_lexer::{lex_recovering, TokenKind};
use arandu_parser::{lower_syntax_to_program, parse_syntax};

pub(crate) fn run(data: &[u8]) {
    let (range, body) = match data {
        [a, b, rest @ ..] => (Some((*a, *b)), rest),
        _ => (None, data),
    };
    let source = String::from_utf8_lossy(body);
    check_source(&source, range);
}

/// Deterministic entry point shared by the fuzz target and unit regressions.
pub fn check_source(source: &str, range: Option<(u8, u8)>) {
    let once = format_source(source);
    let twice = format_source(&once);
    assert_eq!(once, twice, "format_source is not idempotent");

    let edits = format_edits(source);
    let applied = apply_edits(source, &edits);
    assert_eq!(
        applied, once,
        "format_edits does not reproduce format_source"
    );

    if is_clean(source) {
        assert!(
            is_clean(&once),
            "formatting a clean source produced an unparseable result"
        );
        assert_eq!(
            significant_tokens(source),
            significant_tokens(&once),
            "formatting changed the token stream"
        );
        assert_eq!(
            comment_count(source),
            comment_count(&once),
            "formatting dropped or duplicated comments"
        );
    }

    if let Some((a, b)) = range {
        let len = source.len().max(1);
        let start = (usize::from(a) * len / 255) as u32;
        let end = (usize::from(b) * len / 255) as u32;
        let ranged = format_edits_in_range(source, start.min(end), start.max(end));
        let applied = apply_edits(source, &ranged);
        if is_clean(source) {
            assert_eq!(
                significant_tokens(source),
                significant_tokens(&applied),
                "range formatting changed the token stream"
            );
        }
    }
}

/// Apply non-overlapping, sorted UTF-8 byte edits; any malformed edit list is
/// a formatter contract violation.
fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for edit in edits {
        let start = edit.start as usize;
        let end = edit.end as usize;
        assert!(
            cursor <= start && start <= end && end <= source.len(),
            "edit {start}..{end} is unsorted, overlapping or out of bounds (cursor {cursor}, len {})",
            source.len()
        );
        assert!(
            source.is_char_boundary(start) && source.is_char_boundary(end),
            "edit {start}..{end} splits a UTF-8 scalar"
        );
        out.push_str(&source[cursor..start]);
        out.push_str(&edit.new_text);
        cursor = end;
    }
    out.push_str(&source[cursor..]);
    out
}

fn is_clean(source: &str) -> bool {
    let tree = parse_syntax(source);
    tree.lex_diagnostics().is_empty() && lower_syntax_to_program(&tree, 0).is_ok()
}

fn significant_tokens(source: &str) -> Vec<(TokenKind, bool, String)> {
    let lexed = lex_recovering(source);
    lexed
        .tokens
        .iter()
        .filter(|token| token.kind != TokenKind::Eof)
        .map(|token| (token.kind, token.inserted, token.lexeme(source).to_owned()))
        .collect()
}

/// Count `//` and `/*` openers outside string and character literals, using
/// the lexer's own literal spans as the authority.
fn comment_count(source: &str) -> usize {
    let lexed = lex_recovering(source);
    let mut literal_spans = lexed
        .tokens
        .iter()
        .filter(|token| !token.inserted && token.kind != TokenKind::DocComment)
        .map(|token| (token.start as usize, token.end() as usize));
    let bytes = source.as_bytes();
    let mut count = 0;
    let mut index = 0;
    let mut span = literal_spans.next();
    while index + 1 < bytes.len() {
        while let Some((_, end)) = span {
            if end <= index {
                span = literal_spans.next();
            } else {
                break;
            }
        }
        if span.is_some_and(|(start, end)| start <= index && index < end) {
            index += 1;
            continue;
        }
        if bytes[index] == b'/' && matches!(bytes[index + 1], b'/' | b'*') {
            count += 1;
            index = skip_comment(bytes, index);
            continue;
        }
        index += 1;
    }
    count
}

fn skip_comment(bytes: &[u8], start: usize) -> usize {
    if bytes.get(start + 1) == Some(&b'/') {
        bytes[start..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(bytes.len(), |offset| start + offset)
    } else {
        bytes[start + 2..]
            .windows(2)
            .position(|window| window == b"*/")
            .map_or(bytes.len(), |offset| start + 2 + offset + 2)
    }
}

#[cfg(test)]
mod tests {
    use super::check_source;

    #[test]
    fn formatter_oracle_accepts_canonical_and_messy_sources() {
        for source in [
            "",
            "func main(): int { return 0 }\n",
            "func main() :int{\nlet x :int = 1\n  return x\n}\n",
            "// lead\nfunc a(): int { return 1 } /* tail */\n",
            "struct P { x: int, y: int }\nfunc main(): int { let p = P { x: 1, y: 2 }\n return p.x }\n",
            "func main(): int { return ( }\n",
        ] {
            check_source(source, None);
            check_source(source, Some((0, 255)));
        }
    }
}
