//! HIR clone helpers for monomorphization expand.
//!
//! Deep-clones template bodies with type-parameter substitution and fresh symbols.

mod expr;
mod pattern;
mod stmt;

pub(super) use expr::{clone_expr, clone_place};
pub(super) use pattern::{clone_condition, clone_pattern_id};
pub(super) use stmt::{clone_block, clone_catch_handler, clone_match_arm};

use arandu_lexer::Span;
use arandu_middle::symbol_table::{SymbolId, SymbolKind};
use arandu_typeck::TypeCheckResult;

pub(super) fn fresh_symbol(
    tc: &mut TypeCheckResult,
    name: &str,
    kind: SymbolKind,
    span: Span,
) -> SymbolId {
    let mut candidate = name.to_string();
    let mut n = 0u32;
    let scope = tc.symbols.global_scope();
    loop {
        match tc.symbols_mut().define(scope, &candidate, kind, span) {
            Ok(id) => return id,
            Err(_) => {
                n += 1;
                candidate = format!("{name}__{n}");
            }
        }
    }
}
