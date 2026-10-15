//! Shared CST context for interactive signature help.
use arandu_lexer::TokenKind;
use arandu_query::{AnalysisSnapshot, SourceFile};

#[derive(Debug, PartialEq, Eq)]
pub struct CallContext {
    pub name: String,
    pub callee_start: u32,
    pub active_parameter: u32,
}

pub fn call_context(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    cursor_offset: u32,
) -> Option<CallContext> {
    let tree = arandu_query::passes::syntax_tree(&snap.db, source);
    let tokens: Vec<_> = tree
        .tokens()
        .iter()
        .filter(|token| {
            token.start < cursor_offset
                && !token.inserted
                && !matches!(token.kind, TokenKind::DocComment | TokenKind::Eof)
        })
        .collect();

    let mut parenthesis_depth = 0_u32;
    let open_index = tokens.iter().enumerate().rev().find_map(|(index, token)| {
        match token.kind {
            TokenKind::RParen => parenthesis_depth = parenthesis_depth.saturating_add(1),
            TokenKind::LParen if parenthesis_depth == 0 => return Some(index),
            TokenKind::LParen => parenthesis_depth = parenthesis_depth.saturating_sub(1),
            _ => {}
        }
        None
    })?;
    // A grouping parenthesis is not a call. Do not search past operators or
    // delimiters for an unrelated earlier identifier.
    let mut callee_index = open_index.checked_sub(1)?;
    if tokens[callee_index].kind == TokenKind::Gt {
        let mut depth = 1_u32;
        while depth != 0 {
            callee_index = callee_index.checked_sub(1)?;
            match tokens[callee_index].kind {
                TokenKind::Gt => depth += 1,
                TokenKind::Lt => depth -= 1,
                _ => {}
            }
        }
        callee_index = callee_index.checked_sub(1)?;
    }
    let callee = &tokens[callee_index];
    if !matches!(callee.kind, TokenKind::IdentValue | TokenKind::IdentType) {
        return None;
    }

    let mut delimiter_depth = 0_u32;
    let mut active_parameter = 0_u32;
    for token in &tokens[open_index + 1..] {
        match token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                delimiter_depth = delimiter_depth.saturating_add(1)
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                delimiter_depth = delimiter_depth.saturating_sub(1)
            }
            TokenKind::Comma if delimiter_depth == 0 => {
                active_parameter = active_parameter.saturating_add(1);
            }
            _ => {}
        }
    }
    Some(CallContext {
        name: callee.lexeme(tree.text()).to_string(),
        callee_start: callee.start,
        active_parameter,
    })
}

/// Resolve the callee through semantic identities, including namespace members.
/// Unknown or invalid callees must not pick an unrelated function by spelling.
#[must_use]
pub fn callee_symbol(
    snap: &AnalysisSnapshot,
    source: SourceFile,
    tc: &arandu_semantics::TypeCheckResult,
    context: &CallContext,
) -> Option<arandu_middle::SymbolId> {
    crate::symbol_at(tc, context.callee_start).or_else(|| {
        let parsed = arandu_query::passes::parse(&snap.db, source);
        parsed
            .as_ref()
            .as_ref()
            .ok()
            .and_then(|program| crate::expr_symbol_at(program, tc, context.callee_start))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_grouping_and_tracks_nested_arguments_and_generics() {
        for (text, expected) in [
            ("func main(): void { unknown + (", None),
            ("func main(): void { foo(1, [2, 3], ", Some(("foo", 2))),
            ("func main(): void { foo<int>(1, ", Some(("foo", 1))),
            ("func main(): void { foo(\"),[\", ", Some(("foo", 1))),
        ] {
            let mut host = arandu_query::AnalysisHost::new();
            let file = host.new_file("main.aru".into(), text.into());
            let snapshot = host.snapshot();
            let result = call_context(&snapshot, file, text.len() as u32);
            assert_eq!(
                result
                    .as_ref()
                    .map(|value| (value.name.as_str(), value.active_parameter)),
                expected
            );
        }
    }
}
