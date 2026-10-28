//! Typing context of one item in its own coordinates (RFC 0042, step C): what
//! lowering an item body reads, without a position of any other item.

use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_base::AstId;
use arandu_middle::{Span, SymbolId};
use arandu_resolve::ResolvedNames;
use arandu_semantics::TypeCheckResult;
use arandu_typeck::type_checker::info::ItemPlacement;
use std::sync::Arc;

/// [`super::declaration_signatures`] without positions: the tables keyed by
/// source position are empty, the symbols of this file carry the
/// `Span::new(0, 0, 0)` placeholder, and the declaration diagnostics are left
/// to [`item_context`]. A body edit that moves later items leaves it equal.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "signature_context", file = ?file.file_id(db),
))]
pub fn signature_context(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<TypeCheckResult> {
    HashEq::new(without_positions(
        super::signatures::declaration_signatures(db, file),
    ))
}

/// `result` without the positions of its file: the tables keyed by source
/// position are empty, the file's symbols carry the `Span::new(0, 0, 0)`
/// placeholder and there are no diagnostics.
pub(crate) fn without_positions(result: &TypeCheckResult) -> TypeCheckResult {
    let ResolvedNames {
        deferred_comptime_functions,
        deferred_loop_bodies: _,
        comptime_branches: _,
        comptime_loops: _,
        comptime_arguments: _,
        typed_comptime_arguments: _,
        definitions: _,
        expr_symbols: _,
        value_refs: _,
        type_refs: _,
        mutable_symbols,
    } = result.resolved.as_ref();
    let resolved = ResolvedNames {
        deferred_comptime_functions: deferred_comptime_functions.clone(),
        mutable_symbols: mutable_symbols.clone(),
        ..ResolvedNames::default()
    };
    let mut type_info = result.type_info.as_ref().clone();
    type_info.expr_types = Vec::new();
    type_info.ctfe_roots = Vec::new();
    type_info.ctfe_values = Default::default();
    type_info.header_requests = Default::default();
    type_info.struct_field_defaults = Default::default();
    type_info.thread_captures = Default::default();
    TypeCheckResult {
        symbols: Arc::new(result.symbols.map_spans(|_| Span::new(0, 0, 0))),
        resolved: Arc::new(resolved),
        type_info: Arc::new(type_info),
        diagnostics: Vec::new(),
    }
}

/// [`signature_context`] with the local typing of `item_sym` placed at the
/// start of its item, and the symbols and declaration diagnostics of that item
/// relative to it. Symbols of other items carry the placeholder. `None` when
/// the item does not type locally, or lowering it reads a position outside
/// it (import diagnostics).
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_context", file = ?file.file_id(db), item = ?item_sym,
))]
pub fn item_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> Option<(HashEq<TypeCheckResult>, AstId)> {
    let (local, at) = super::local_typing::item_local_typing(db, file, item_sym).as_ref()?;
    let signatures = super::signatures::declaration_signatures(db, file);
    if signatures
        .resolved
        .deferred_comptime_functions
        .contains(&item_sym)
    {
        return None;
    }
    let id = *super::items::item_ast_ids(db, file).get(&item_sym)?;
    let extent = super::items::item_positions(db, file).item_span(id)?;
    let own = *super::typing::item_source_spans(db, file).get(&item_sym)?;
    let relative = |span: Span| {
        (span.file_id == extent.file_id && extent.start <= span.start && span.end <= extent.end)
            .then(|| {
                Span::new(
                    span.file_id,
                    span.start - extent.start,
                    span.end - extent.start,
                )
            })
    };
    // A function body takes the declaration diagnostics inside its item and
    // those of the imports, which lie outside every body.
    let parsed = super::resolution::parse(db, file);
    let imports: Vec<Span> = match &**parsed {
        Ok(program) => program.imports.iter().map(|import| import.span()).collect(),
        Err(_) => return None,
    };
    let contains = |outer: Span, span: Span| {
        span.file_id == outer.file_id && outer.start <= span.start && span.end <= outer.end
    };
    let mut diagnostics = Vec::new();
    for diagnostic in &signatures.diagnostics {
        if imports
            .iter()
            .any(|&import| contains(import, diagnostic.span))
        {
            return None;
        }
        if !contains(own, diagnostic.span) {
            continue;
        }
        let mut diagnostic = diagnostic.clone();
        let mut ok = true;
        super::local_typing::for_each_span(&mut diagnostic, |span| {
            if span.file_id != own.file_id || *span == Span::new(0, 0, 0) {
                return;
            }
            match relative(*span) {
                Some(local) => *span = local,
                None => ok = false,
            }
        });
        if !ok {
            return None;
        }
        diagnostics.push(diagnostic);
    }
    let context = signature_context(db, file);
    let mut checked = (**context).clone();
    // The typing input already holds the same symbol view.
    checked.symbols = match super::local_typing::item_typing_input(db, file, item_sym) {
        Some(input) => Arc::clone(&input.symbols),
        None => Arc::new(
            signatures
                .symbols
                .map_spans(|symbol| relative(symbol.span).unwrap_or(Span::new(0, 0, 0))),
        ),
    };
    // The local names are the item's whole resolution: the symbol sets plus
    // every position inside its declaration.
    Arc::make_mut(&mut checked.type_info).merge_item_from(
        &local.type_info,
        &ItemPlacement {
            file_id: at.file_id,
            ..ItemPlacement::default()
        },
    );
    checked.resolved = Arc::new(local.resolved.clone());
    // Field defaults are expressions of the item program.
    if let Some(program) = super::programs::defaults_program(db, file, item_sym)? {
        if !program.defaults.is_empty() {
            checked.type_info_mut().struct_field_defaults = program.defaults.clone();
        }
    }
    checked
        .diagnostics
        .extend(local.diagnostics.iter().cloned());
    checked.diagnostics.extend(diagnostics);
    Some((HashEq::new(checked), id))
}
