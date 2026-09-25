use super::build::{SyntaxTree, build_item_green, find_top_level_item_spans, parse_syntax};
use super::kind::{SyntaxKind, SyntaxNode};
use arandu_lexer::{Token, TokenKind, lex_recovering};
use rowan::NodeOrToken;
use std::sync::Arc;

/// Single contiguous edit from `old` → `new` (LCP + LCS suffix), if any.
///
/// Returns `(edit_start, edit_end_in_old, replacement)`. Used by Salsa
/// `syntax_tree` to drive [`reparse_subtree`] after full-text commits.
#[must_use]
pub fn single_contiguous_edit(old: &str, new: &str) -> Option<(u32, u32, String)> {
    if old == new {
        return None;
    }
    let old_b = old.as_bytes();
    let new_b = new.as_bytes();
    let mut prefix = 0usize;
    let max_pre = old_b.len().min(new_b.len());
    while prefix < max_pre && old_b[prefix] == new_b[prefix] {
        prefix += 1;
    }
    while prefix > 0 && (!old.is_char_boundary(prefix) || !new.is_char_boundary(prefix)) {
        prefix -= 1;
    }

    let mut old_end = old_b.len();
    let mut new_end = new_b.len();
    while old_end > prefix && new_end > prefix && old_b[old_end - 1] == new_b[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }
    while old_end < old.len() && !old.is_char_boundary(old_end) {
        old_end += 1;
    }
    while new_end < new.len() && !new.is_char_boundary(new_end) {
        new_end += 1;
    }
    // If suffix walked past prefix due to boundary fixups, clamp.
    if old_end < prefix {
        old_end = prefix;
    }
    if new_end < prefix {
        new_end = prefix;
    }

    Some((
        prefix as u32,
        old_end as u32,
        new[prefix..new_end].to_string(),
    ))
}

#[must_use]
pub fn apply_text_edit(source: &str, start: u32, end: u32, replacement: &str) -> String {
    let start = (start as usize).min(source.len());
    let end = (end as usize).min(source.len()).max(start);
    let mut out = String::with_capacity(source.len() - (end - start) + replacement.len());
    out.push_str(&source[..start]);
    out.push_str(replacement);
    out.push_str(&source[end..]);
    out
}

/// Full reparse after edit (always correct). Prefer [`reparse_subtree`] when possible.
#[must_use]
pub fn reparse_edit(
    old_source: &str,
    start: u32,
    end: u32,
    replacement: &str,
    _item_spans: &[(u32, u32)],
) -> (String, SyntaxTree) {
    let new_source = apply_text_edit(old_source, start, end, replacement);
    let tree = parse_syntax(&new_source);
    (new_source, tree)
}

/// Splice tokens for an edited ITEM: re-lex only the item slice; shift siblings.
///
/// Uses the first **non-zero-width** token at/after `old_s` as the splice start so
/// leading whitespace gaps / zero-width ASI `;` of the *previous* item are kept.
#[must_use]
pub fn splice_tokens_for_item_edit(
    old_tokens: &[Token],
    old_s: u32,
    old_e: u32,
    delta: i64,
    item_tokens: &[Token],
) -> Vec<Token> {
    // Content start: first real (len>0) token inside the item range, else old_s.
    let content_s = old_tokens
        .iter()
        .find(|t| {
            !matches!(t.kind, TokenKind::Eof) && t.len > 0 && t.start >= old_s && t.start < old_e
        })
        .map(|t| t.start)
        .unwrap_or(old_s);

    let mut out = Vec::with_capacity(old_tokens.len() + item_tokens.len());
    let mut i = 0;
    // Prefix: tokens before content (includes zero-width ASI `;` of previous item).
    while i < old_tokens.len() {
        let t = old_tokens[i];
        if matches!(t.kind, TokenKind::Eof) {
            i += 1;
            continue;
        }
        if t.start < content_s {
            out.push(t);
            i += 1;
        } else {
            break;
        }
    }
    // Skip old tokens in [content_s, old_e).
    while i < old_tokens.len() {
        let t = old_tokens[i];
        if matches!(t.kind, TokenKind::Eof) {
            i += 1;
            continue;
        }
        if t.start < old_e {
            i += 1;
        } else {
            break;
        }
    }
    // New item tokens (absolute). Their offsets are relative to the edited
    // item's new start, so comparing them to `content_s` (an old-source offset)
    // would discard valid tokens when an edit shifts the item left.
    for t in item_tokens {
        if matches!(t.kind, TokenKind::Eof) {
            continue;
        }
        out.push(*t);
    }
    // Suffix: shift by delta.
    let replacement_end = (old_e as i64).checked_add(delta);
    let edited_item_has_boundary_asi = replacement_end.is_some_and(|end| {
        item_tokens.iter().any(|token| {
            token.kind == TokenKind::Semicolon && token.inserted && token.start as i64 == end
        })
    });
    while i < old_tokens.len() {
        let mut t = old_tokens[i];
        // The green item range ends before its zero-width ASI terminator. The
        // edited item's re-lex already supplies the replacement terminator;
        // retaining the old one here duplicates it at the same boundary.
        if edited_item_has_boundary_asi
            && t.start == old_e
            && t.len == 0
            && t.kind == TokenKind::Semicolon
            && t.inserted
        {
            i += 1;
            continue;
        }
        if !matches!(t.kind, TokenKind::Eof) {
            let new_start = (t.start as i64) + delta;
            if new_start >= 0 {
                t.start = new_start as u32;
                out.push(t);
            }
        }
        i += 1;
    }
    let eof_start = old_tokens
        .iter()
        .find(|token| matches!(token.kind, TokenKind::Eof))
        .map(|token| (token.start as i64 + delta).max(0) as u32)
        .unwrap_or_else(|| {
            out.last()
                .map(|token| token.start.saturating_add(token.len))
                .unwrap_or(0)
        });
    out.push(Token {
        start: eof_start,
        len: 0,
        kind: TokenKind::Eof,
        inserted: false,
    });
    out
}

/// Reparse **only the ITEM subtree** covering the edit when the edit stays inside one item.
///
/// Algorithm:
/// 1. Apply the text edit.
/// 2. If the edit range is contained in a single [`SyntaxKind::ITEM`], re-lex **only that
///    ITEM's new text**, rebuild its green node, and `GreenNodeData::replace_child` on the
///    root so **sibling ITEM green nodes are reused** (cheap `Arc` clone).
/// 3. **Splice** the token stream (no full-file re-lex) for lower.
/// 4. Otherwise fall back to full [`parse_syntax`].
#[must_use]
pub fn reparse_subtree(
    old: &SyntaxTree,
    edit_start: u32,
    edit_end: u32,
    replacement: &str,
) -> (String, SyntaxTree) {
    let new_source = apply_text_edit(old.text(), edit_start, edit_end, replacement);
    let delta = replacement.len() as i64 - (edit_end.saturating_sub(edit_start) as i64);

    let fallback = |new_source: String| {
        let tree = parse_syntax(&new_source);
        (new_source, tree)
    };

    let old_items = old.items();
    let Some(idx) = old.item_index_at(edit_start) else {
        return fallback(new_source);
    };
    let old_range = old_items[idx].text_range();
    let old_s = u32::from(old_range.start());
    let old_e = u32::from(old_range.end());
    if edit_start < old_s || edit_end > old_e {
        return fallback(new_source);
    }

    let new_s = old_s;
    let Some(new_e) = (old_e as i64).checked_add(delta) else {
        return fallback(new_source);
    };
    if new_e < new_s as i64 || new_e as usize > new_source.len() {
        return fallback(new_source);
    }
    let new_e = new_e as u32;

    // Locate the top-level item among root green children (trivia siblings stay put).
    let old_root = old.root();
    let root_green = old_root.green();
    let mut seen = 0usize;
    let mut child_index = None;
    for (i, child) in root_green.children().enumerate() {
        if let NodeOrToken::Node(n) = child {
            let k = SyntaxKind::from_raw(n.kind());
            if k.is_top_level_item() {
                if seen == idx {
                    child_index = Some(i);
                    break;
                }
                seen += 1;
            }
        }
    }
    let Some(child_index) = child_index else {
        return fallback(new_source);
    };

    // Re-lex + rebuild structured green for ONLY the edited item slice.
    let item_text = &new_source[new_s as usize..new_e as usize];
    let item_lexed = lex_recovering(item_text);
    // Containment in the OLD item does not imply the replacement is one item.
    // Building it unconditionally as a single node can hide a new declaration
    // from green-guided AST lowering, despite preserving every source byte.
    // Unclosed lexical constructs or braces can also consume the next sibling.
    let brace_depth = item_lexed
        .tokens
        .iter()
        .try_fold(0u32, |depth, token| match token.kind {
            TokenKind::LBrace => depth.checked_add(1),
            TokenKind::RBrace => depth.checked_sub(1),
            _ => Some(depth),
        });
    if !item_lexed.diagnostics.is_empty()
        || brace_depth != Some(0)
        || find_top_level_item_spans(item_text, &item_lexed.tokens, new_e - new_s).len() != 1
    {
        return fallback(new_source);
    }
    let new_item = build_item_green(item_text, &item_lexed.tokens);

    let new_green = root_green.replace_child(child_index, NodeOrToken::Node(new_item));
    let patched_root = SyntaxNode::new_root(new_green.clone());
    if patched_root.text().to_string() != new_source {
        return fallback(new_source);
    }

    // Splice tokens: re-lex only the edited item; keep prefix ASI with content_s.
    let item_diags = item_lexed.diagnostics;
    let item_tokens: Vec<Token> = item_lexed
        .tokens
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Eof))
        .map(|mut t| {
            t.start = t.start.saturating_add(new_s);
            t
        })
        .collect();
    let spliced = splice_tokens_for_item_edit(old.tokens(), old_s, old_e, delta, &item_tokens);

    // Merge lexical diagnostics from other items (shift post-edit ones by delta)
    let mut merged_diags = Vec::new();
    for err in old.lex_diagnostics() {
        if err.span.start < old_s {
            merged_diags.push(*err);
        } else if err.span.start >= old_e {
            let mut new_err = *err;
            if delta != 0 {
                let new_start = (new_err.span.start as i64)
                    .checked_add(delta)
                    .unwrap_or(0)
                    .max(0) as u32;
                let new_end = (new_err.span.end as i64)
                    .checked_add(delta)
                    .unwrap_or(0)
                    .max(0) as u32;
                new_err.span.start = new_start;
                new_err.span.end = new_end;
            }
            merged_diags.push(new_err);
        }
    }
    for err in &item_diags {
        let mut new_err = *err;
        new_err.span.start = new_err.span.start.saturating_add(new_s);
        new_err.span.end = new_err.span.end.saturating_add(new_s);
        merged_diags.push(new_err);
    }
    merged_diags.sort_by_key(|e| e.span.start);

    let tree = SyntaxTree {
        green: new_green,
        text: Arc::from(new_source.as_str()),
        tokens: Arc::new(spliced),
        lex_diagnostics: Arc::new(merged_diags),
    };
    (new_source, tree)
}
