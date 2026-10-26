use crate::db::{FingerprintEq, HashEq};
use crate::{ArandCompilerDb, SourceFile};
use arandu_parser::Program;
use arandu_semantics::TypeCheckResult;
use salsa::Accumulator;

use std::sync::Arc;

/// Per-item input for body typeck: holds current [`Program`] but **HashEq**
/// only fingerprints that item's source span (sibling edits early-cutoff).
#[derive(Clone)]
pub struct ItemSourceInput {
    pub program: Arc<Program>,
    pub item_sym: arandu_middle::SymbolId,
    /// Start of the source item, including its attributes. Used by
    /// structured fixes without re-parsing diagnostic presentation text.
    pub item_start: u32,
    /// Conservative text-slice prefilter, not syntax recognition. A false value
    /// avoids scanning the shared AST arena in ordinary, non-staged items.
    pub(crate) may_have_comptime: bool,
    /// blake3 of the item's source slice for StableHash / early cutoff.
    pub(crate) body_fp: blake3::Hash,
}

/// Backward-compatible alias used by older call sites / StableHash.
pub type FuncBodyInput = ItemSourceInput;

/// Extract one item's AST dependency from canonical AST + declaration identities.
/// Body resolution is deliberately not a dependency of this source boundary.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_source_input",
    file = ?file.file_id(db),
    item = ?item_sym,
))]
pub fn item_source_input(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: arandu_middle::SymbolId,
) -> HashEq<ItemSourceInput> {
    use arandu_parser::TopLevelDecl;

    let program_res = super::resolution::parse(db, file);
    let headers = super::resolution::resolved_headers(db, file);
    let resolved = &headers.declarations;
    let text = file.text(db);

    let Ok(program) = &**program_res else {
        return HashEq::new(ItemSourceInput {
            program: Arc::new(empty_program()),
            item_sym,
            item_start: 0,
            may_have_comptime: false,
            body_fp: blake3::hash(b"parse-error"),
        });
    };

    // Depend on CST for incremental invalidation; fingerprint from text slices (no String alloc).
    let tree = super::resolution::syntax_tree(db, file);
    let ranges = tree.item_ranges();

    let mut body_fp = blake3::hash(b"item-missing");
    let mut item_start = 0;
    let mut may_have_comptime = false;
    let mut selected = None;
    program.for_each_decl_recursive(|decl_id, decl| {
        let matches = match arandu_semantics::primary_def_key(decl) {
            Some(key) => resolved.resolved.definitions.get(&key) == Some(&item_sym),
            None => false,
        } || matches!(
            decl,
            TopLevelDecl::Extern(ext)
                if ext.members.iter().any(|m| {
                    resolved.resolved.definitions.get(&arandu_middle::NodeKey::from(m.span))
                        == Some(&item_sym)
                })
        );
        if matches && selected.is_none() {
            selected = Some((decl_id, decl));
        }
    });
    if let Some((decl_id, decl)) = selected {
        let span = arandu_semantics::item_source_span(decl);
        item_start = span.start;
        // Floor/ceil to char boundaries — spans can land mid-UTF-8 sequence
        // (e.g. multi-byte comment characters adjacent to an item).
        let floor = |i: usize| {
            let mut i = i.min(text.len());
            while i > 0 && !text.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        let ceil = |i: usize| {
            let mut i = i.min(text.len());
            while i < text.len() && !text.is_char_boundary(i) {
                i += 1;
            }
            i
        };
        let start = floor(span.start as usize);
        let end = ceil(span.end as usize).max(start);
        may_have_comptime = text[start..end].contains("comptime");
        let mut h = blake3::Hasher::new();
        h.update(b"item_body_v4");
        // Prefer covering CST ITEM range (zero-copy slice of shared text).
        let mut used_cst = false;
        let is_top_level = program.decls.contains(&decl_id);
        for &(s, e) in &ranges {
            // CST item ranges describe top-level declarations. Using a
            // containing module for a nested member would invalidate every
            // sibling body; fingerprint that member's own AST source span.
            if is_top_level && s <= span.start && span.end <= e {
                let s = floor(s as usize);
                let e = ceil(e as usize).max(s);
                h.update(text[s..e].as_bytes());
                used_cst = true;
                break;
            }
        }
        if !used_cst {
            h.update(text[start..end].as_bytes());
        }
        body_fp = h.finalize();
    }

    HashEq::new(ItemSourceInput {
        program: Arc::clone(program),
        item_sym,
        item_start,
        may_have_comptime,
        body_fp,
    })
}

pub(super) fn empty_program() -> Program {
    Program {
        span: arandu_base::Span::new(0, 0, 0),
        module: None,
        imports: vec![],
        interface_impls: vec![],
        decls: vec![],
        docs: vec![],
        pool: arandu_parser::ast_pool::AstPool::default(),
    }
}

/// Per-item body typeck (P1 funcs + P2 all top-level body items).
///
/// Depends on [`item_source_input`] (HashEq by item source span) + [`module_signatures`].
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_body_typeck",
    file = ?file.file_id(db),
    item = ?item_sym,
))]
pub fn item_body_typeck(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: arandu_middle::SymbolId,
) -> HashEq<TypeCheckResult> {
    let typed = crate::ctfe::item_staged_typing(db, file, item_sym);
    let signatures = super::signatures::module_signatures(db, file);
    if signatures.type_info.return_borrow_summaries == typed.type_info.return_borrow_summaries {
        return HashEq::share(typed);
    }
    // The checker only needs declared types. Compose flow metadata here so the
    // existing ownership/IDE consumers retain their contract-sensitive cutoff.
    let mut result = (**typed).clone();
    result.type_info_mut().return_borrow_summaries =
        signatures.type_info.return_borrow_summaries.clone();
    HashEq::new(result)
}

/// Canonical body typing against declarations, without requesting callee flow.
/// The compatibility view above shares this memo instead of checking again.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_typing", file = ?file.file_id(db), item = ?item_sym,
))]
pub fn item_typing(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: arandu_middle::SymbolId,
) -> FingerprintEq<TypeCheckResult> {
    let body_in = item_source_input(db, file, item_sym);
    let signatures = super::signatures::declaration_signatures(db, file);
    // Every read is folded in, including those that only select a branch.
    let mut identity = blake3::Hasher::new();
    identity.update(b"ItemTyping/fingerprint/v1");
    identity.update(&item_sym.file_id.to_le_bytes());
    identity.update(&item_sym.local_id.0.to_le_bytes());
    identity.update(crate::StableHash::stable_hash(body_in).as_bytes());
    identity.update(signatures.identity().as_bytes());
    if signatures
        .resolved
        .deferred_comptime_functions
        .contains(&item_sym)
    {
        identity.update(b"deferred");
        return FingerprintEq::fingerprinted(
            db,
            "item_typing",
            Arc::clone(&signatures.value),
            identity.finalize(),
        );
    }
    let arguments = crate::ctfe::item_const_arguments(db, file, item_sym);
    identity.update(crate::StableHash::stable_hash(arguments).as_bytes());
    let mut staged;
    let initial = if arguments.values.is_empty()
        && arguments.typed_values.is_empty()
        && arguments.diagnostics.is_empty()
    {
        &**signatures
    } else {
        staged = (**signatures).clone();
        std::sync::Arc::make_mut(&mut staged.resolved)
            .comptime_arguments
            .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
        std::sync::Arc::make_mut(&mut staged.resolved)
            .typed_comptime_arguments
            .extend(
                arguments
                    .typed_values
                    .iter()
                    .map(|(&key, value)| (key, value.clone())),
            );
        &staged
    };
    let mut concrete;
    let deferred_header = initial.type_info.deferred_headers.contains(&item_sym)
        && !initial
            .type_info
            .generic_params
            .get(&item_sym)
            .is_some_and(|parameters| !parameters.is_empty());
    let initial = if deferred_header {
        concrete = initial.clone();
        let key = arandu_middle::types::FunctionInstance {
            definition: item_sym,
            arguments: Vec::new(),
        };
        if let Err(errors) = crate::ctfe::contracts::prepare_owner(
            db,
            file,
            &key,
            &mut concrete,
            &crate::ctfe::DependencyContext::default(),
        ) {
            concrete.diagnostics.extend(errors);
            return FingerprintEq::exact(concrete);
        }
        &concrete
    } else {
        initial
    };
    let mut res = crate::ctfe::contracts::check_body(
        db,
        initial,
        body_in.program.as_ref(),
        item_sym,
        &arandu_middle::types::GenericSubst::new(),
        &crate::ctfe::DependencyContext::default(),
    );
    res.diagnostics
        .extend(arguments.diagnostics.iter().cloned());
    // `prepare_owner` reads are not fingerprinted, and a failed header read
    // leaves a diagnostic without installing the header it read.
    if deferred_header || !res.diagnostics.is_empty() {
        return FingerprintEq::exact(res);
    }
    crate::ctfe::contracts::fingerprint_installed_headers(db, initial, &res, &mut identity);
    FingerprintEq::fingerprinted(db, "item_typing", res, identity.finalize())
}

/// Composed file typeck: signatures + per-item body memos (P2).
///
/// This is the incremental-friendly view; [`type_check`] delegates here.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "file_typeck_view",
    file = ?file.file_id(db),
))]
pub fn file_typeck_view(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<TypeCheckResult> {
    compose_file_typing(db, file, false)
}

/// Declaration-based body memos composed before borrow-interface inference.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "file_typing", file = ?file.file_id(db),
))]
pub fn file_typing(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<TypeCheckResult> {
    compose_file_typing(db, file, true)
}

fn compose_file_typing(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    declarations_only: bool,
) -> HashEq<TypeCheckResult> {
    let program_res = super::resolution::parse(db, file);
    let signatures = if declarations_only {
        super::signatures::declaration_signatures(db, file)
    } else {
        super::signatures::module_signatures(db, file)
    };

    let Ok(program) = &**program_res else {
        return HashEq::from_arc(Arc::clone(&signatures.value));
    };

    let item_syms = arandu_semantics::body_item_symbols(program, signatures.resolved.as_ref());

    // O(1) Arc share until the first body merge; avoid deep-cloning TypeInfo up front.
    let mut merged_info = Arc::clone(&signatures.type_info);
    let mut merged_resolved = Arc::clone(&signatures.resolved);
    let mut diagnostics = signatures.diagnostics.clone();

    let mut item_spans = rustc_hash::FxHashMap::default();
    program.for_each_decl_recursive(|_, declaration| {
        if let Some(&owner) = arandu_semantics::primary_def_key(declaration)
            .and_then(|key| signatures.resolved.definitions.get(&key))
        {
            item_spans.insert(owner, arandu_semantics::item_source_span(declaration));
        }
    });
    for &item_sym in &item_syms {
        db.unwind_if_revision_cancelled();
        let item = if declarations_only {
            crate::ctfe::item_staged_typing(db, file, item_sym)
        } else {
            item_body_typeck(db, file, item_sym)
        };
        Arc::make_mut(&mut merged_info).merge_from(item.type_info.as_ref());
        // Body typing can resolve expected-type enum sugar and struct patterns.
        // Compose only this item's span; a template context from a later item
        // must not overwrite another body's newly resolved references.
        if !Arc::ptr_eq(&item.resolved, &signatures.resolved) {
            if let Some(&span) = item_spans.get(&item_sym) {
                let contains =
                    |key: &arandu_middle::NodeKey| span.start <= key.start && key.end <= span.end;
                let names = Arc::make_mut(&mut merged_resolved);
                names.value_refs.extend(
                    item.resolved
                        .value_refs
                        .iter()
                        .filter(|(key, _)| contains(key))
                        .map(|(&key, &value)| (key, value)),
                );
                names.type_refs.extend(
                    item.resolved
                        .type_refs
                        .iter()
                        .filter(|(key, _)| contains(key))
                        .map(|(&key, &value)| (key, value)),
                );
                for (index, use_span) in program.pool.expr_spans.iter().enumerate() {
                    if span.start <= use_span.start && use_span.end <= span.end {
                        if let Some(value) =
                            item.resolved.expr_symbols.get(index).copied().flatten()
                        {
                            if names.expr_symbols.len() <= index {
                                names.expr_symbols.resize(index + 1, None);
                            }
                            names.expr_symbols[index] = Some(value);
                        }
                    }
                }
            }
        }
        Arc::make_mut(&mut merged_resolved)
            .typed_comptime_arguments
            .extend(
                item.resolved
                    .typed_comptime_arguments
                    .iter()
                    .map(|(&key, value)| (key, value.clone())),
            );
        if !item.resolved.comptime_arguments.is_empty() {
            Arc::make_mut(&mut merged_resolved)
                .comptime_arguments
                .extend(
                    item.resolved
                        .comptime_arguments
                        .iter()
                        .map(|(&key, &value)| (key, value)),
                );
        }
        diagnostics.extend(item.diagnostics.iter().cloned());
        diagnostics.extend(
            crate::dataflow::item_attribute_validation(db, file, item_sym)
                .iter()
                .cloned(),
        );
    }

    // Residual for decls without primary keys (normally empty).
    let residual = arandu_semantics::check_non_func_bodies_only(
        signatures,
        program,
        super::database_target_info(db),
    );
    if !residual.diagnostics.is_empty()
        || residual.type_info.expr_types.iter().any(|s| s.is_some())
        || !residual.type_info.decl_types.is_empty()
    {
        Arc::make_mut(&mut merged_info).merge_from(residual.type_info.as_ref());
        diagnostics.extend(residual.diagnostics);
    }

    let mut symbols = Arc::clone(&signatures.symbols);
    if !merged_info.thread_captures.is_empty() {
        Arc::make_mut(&mut merged_info).materialize_thread_captures(Arc::make_mut(&mut symbols));
    }

    let res = TypeCheckResult {
        symbols,
        resolved: merged_resolved,
        type_info: merged_info,
        diagnostics,
    };
    HashEq::new(res)
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "type_check",
    file = ?file.file_id(db),
))]
pub fn type_check(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<TypeCheckResult> {
    // P1: compose per-function body checks (early cutoff across funcs).
    let res = file_typeck_view(db, file);

    tracing::debug!(
        target: "arandu_query",
        file = ?file.file_id(db),
        diags = ?res.diagnostics,
        "type_check complete (file_typeck_view)"
    );

    for diag in &res.diagnostics {
        arandu_middle::db::DiagnosticsAccumulator(diag.clone()).accumulate(db);
    }

    // Share the same Arc as `file_typeck_view` — no deep clone of TypeCheckResult.
    HashEq::share(res)
}

/// IDE-only type-check that tolerates recoverable syntax errors.
///
/// Compiler queries stay strict: [`parse`] still reports the first error and
/// [`type_check`] still yields no signatures for an invalid program. This query
/// exists so completion, hover and navigation keep working while a buffer is
/// mid-edit — an editor triggers completion right after `receiver.` or `name:`,
/// exactly when there is no valid program yet.
///
/// It lowers the already-built CST with the recovering lowerer, runs the same
/// pure resolver and per-item body checks, and merges imported signatures
/// through [`signatures_from_program`], then restores imported flow-derived
/// contracts before checking bodies. No diagnostics are accumulated: the IDE
/// surfaces recovering-parse diagnostics separately.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "ide_type_check",
    file = ?file.file_id(db),
))]
pub fn ide_type_check(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<TypeCheckResult> {
    let tree = super::resolution::syntax_tree(db, file);
    let file_id = *file.file_id(db);
    let program =
        arandu_parser::syntax::lower_syntax_to_program_recovering(tree.value.as_ref(), file_id)
            .program;

    let locals = arandu_resolve::resolve_local_with_poll(file_id, &program, || {
        db.unwind_if_revision_cancelled();
    });
    let resolved = arandu_resolve::resolve_imports_and_bodies_with_poll(
        &arandu_resolve::SourceDbLoader(db.as_source_db()),
        &program,
        locals,
        || db.unwind_if_revision_cancelled(),
    );

    let mut signatures = Arc::new(super::resolution::signatures_from_program(
        db, file, &program, &resolved, false,
    ));
    super::resolution::merge_imported_borrow_interfaces(db, &program, &mut signatures);

    // Mirror `file_typeck_view` without per-item memos: this is a transient
    // fallback for malformed buffers, not the incremental hot path.
    let item_syms = arandu_semantics::body_item_symbols(&program, signatures.resolved.as_ref());
    let mut merged_info = Arc::clone(&signatures.type_info);
    let mut diagnostics = signatures.diagnostics.clone();
    for &item_sym in &item_syms {
        let item = arandu_semantics::check_item_body_only(
            &signatures,
            &program,
            item_sym,
            super::database_target_info(db),
        );
        Arc::make_mut(&mut merged_info).merge_from(item.type_info.as_ref());
        diagnostics.extend(item.diagnostics);
    }

    let residual = arandu_semantics::check_non_func_bodies_only(
        &signatures,
        &program,
        super::database_target_info(db),
    );
    if !residual.diagnostics.is_empty()
        || residual
            .type_info
            .expr_types
            .iter()
            .any(|slot| slot.is_some())
        || !residual.type_info.decl_types.is_empty()
    {
        Arc::make_mut(&mut merged_info).merge_from(residual.type_info.as_ref());
        diagnostics.extend(residual.diagnostics);
    }

    HashEq::new(TypeCheckResult {
        symbols: Arc::clone(&signatures.symbols),
        resolved: Arc::clone(&signatures.resolved),
        type_info: merged_info,
        diagnostics,
    })
}
