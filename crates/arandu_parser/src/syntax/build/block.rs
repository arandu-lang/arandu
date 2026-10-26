use super::{SyntaxKind, emit_tokens};
use arandu_lexer::{Token, TokenKind};
use rowan::GreenNodeBuilder;

/// `BLOCK` with `{` / `STMT`* / `}` (statements split on `;` at brace-depth 1).
pub(super) fn emit_block_with_stmts(
    builder: &mut GreenNodeBuilder<'_>,
    source: &str,
    tokens: &[Token],
    open_brace: u32,
    close_end: u32,
) {
    builder.start_node(SyntaxKind::BLOCK.into());

    // Tokens strictly inside the block range.
    let in_block: Vec<&Token> = tokens
        .iter()
        .filter(|t| {
            !matches!(t.kind, TokenKind::Eof | TokenKind::Error(_))
                && t.start >= open_brace
                && t.start < close_end
        })
        .collect();

    if in_block.is_empty() {
        builder.finish_node();
        return;
    }

    // Opening `{`
    let first = in_block[0];
    if matches!(first.kind, TokenKind::LBrace) {
        emit_tokens(
            builder,
            source,
            tokens,
            first.start,
            first.start.saturating_add(first.len),
        );
    }

    let close_tok = in_block
        .last()
        .filter(|t| matches!(t.kind, TokenKind::RBrace));
    let inner_end = close_tok.map_or(close_end, |t| t.start);

    // Split interior into STMTs on `;` at depth==1 (inside outer braces).
    let mut depth = 0i32;
    let mut stmt_start: Option<u32> = None;
    for t in &in_block {
        match t.kind {
            TokenKind::LBrace => {
                depth += 1;
                if depth == 1 {
                    // just opened outer `{` — next content starts after this token
                    stmt_start = Some(t.start.saturating_add(t.len));
                }
            }
            TokenKind::RBrace => {
                if depth == 1 {
                    // flush last stmt before closing `}`
                    if let Some(ss) = stmt_start
                        && ss < t.start
                    {
                        builder.start_node(SyntaxKind::STMT.into());
                        emit_tokens(builder, source, tokens, ss, t.start);
                        builder.finish_node();
                    }
                    stmt_start = None;
                }
                depth -= 1;
            }
            TokenKind::Semicolon if depth == 1 => {
                let te = t.start.saturating_add(t.len);
                if let Some(ss) = stmt_start
                    && ss < te
                {
                    builder.start_node(SyntaxKind::STMT.into());
                    emit_tokens(builder, source, tokens, ss, te);
                    builder.finish_node();
                }
                stmt_start = Some(te);
            }
            _ => {
                if depth == 1 && stmt_start.is_none() {
                    stmt_start = Some(t.start);
                }
            }
        }
    }

    // Closing `}`
    if let Some(t) = close_tok {
        emit_tokens(
            builder,
            source,
            tokens,
            t.start,
            t.start.saturating_add(t.len),
        );
    } else if inner_end < close_end {
        emit_tokens(builder, source, tokens, inner_end, close_end);
    }

    builder.finish_node();
}
