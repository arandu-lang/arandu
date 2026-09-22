use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::Diagnostic;
use arandu_parser::Program;
use arandu_resolve::ResolutionResult;
use arandu_semantics::{amir::AmirProgram, TypeCheckResult};
use arandu_typeck::type_checker::TargetInfo;
use salsa::Accumulator;

use std::sync::Arc;

#[derive(Clone)]
pub struct ModuleSignatures {
    value: Arc<TypeCheckResult>,
    hash: blake3::Hash,
}

impl ModuleSignatures {
    fn new(value: TypeCheckResult) -> Self {
        let hash = crate::stable_hash::type_signature_hash(&value);
        Self {
            value: Arc::new(value),
            hash,
        }
    }
}

impl PartialEq for ModuleSignatures {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

impl Eq for ModuleSignatures {}

impl std::ops::Deref for ModuleSignatures {
    type Target = TypeCheckResult;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

/// Explicit type-check target read from the Salsa [`crate::db::TargetConfig`]
/// input (CLI `--layout=`, default host). Keeps typeck pure: the pointer width
/// never hard-codes a 64-bit assumption inside a query.
fn database_target_info(db: &dyn ArandCompilerDb) -> TargetInfo {
    let layout = db.target_config().data_layout(db);
    TargetInfo {
        // `DataLayout` stores byte widths; `TargetInfo` expects bits.
        pointer_width: (layout.pointer_width() * 8) as u8,
    }
}

pub fn cycle_recover(
    _db: &dyn ArandCompilerDb,
    _id: salsa::Id,
    file: SourceFile,
) -> HashEq<ResolutionResult> {
    tracing::debug!(
        target: "arandu_query",
        file = ?file.file_id(_db),
        "cycle_recover for resolve"
    );
    HashEq::new(ResolutionResult::cycle_fallback())
}

#[salsa::tracked]
pub fn local_symbols(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<ResolutionResult> {
    let program_res = parse(db, file);
    let resolved = match &**program_res {
        Ok(program) => arandu_resolve::resolve_local_with_poll(*file.file_id(db), program, || {
            db.unwind_if_revision_cancelled();
        }),
        Err(_) => ResolutionResult {
            is_cycle_fallback: false,
            symbols: Arc::new(arandu_semantics::SymbolTable::default()),
            resolved: Arc::new(arandu_semantics::ResolvedNames::default()),
            docs: arandu_semantics::DocCommentMap::default(),
            diagnostics: vec![],
        },
    };

    HashEq::new(resolved)
}

/// Symbols visible to other files via `import`.
///
/// Root fix for multi-module privacy: only **global-scope** symbols marked
/// `is_public` (from `public` decls, extern surface, prelude) are exported.
/// Private free functions / methods no longer leak across modules.
#[salsa::tracked]
pub fn exported_symbols(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> Arc<arandu_middle::ExportedSymbolTable> {
    let locals = local_symbols(db, file);
    let mut map = std::collections::BTreeMap::new();

    let global_scope = locals.symbols.global_scope();
    for symbol in locals.symbols.iter() {
        if symbol.scope == global_scope && symbol.is_public {
            map.insert(symbol.name.to_string(), (symbol.id, symbol.kind));
        }
    }

    Arc::new(arandu_middle::ExportedSymbolTable { symbols: map })
}

/// Real definition span for `symbol_id` (from the owning file's resolve result).
///
/// Never panics on unknown / cross-file ids: returns a zero-width span.
pub fn symbol_span(
    db: &dyn ArandCompilerDb,
    symbol_id: arandu_middle::SymbolId,
) -> arandu_base::Span {
    let empty = arandu_base::Span::new(symbol_id.file_id, 0, 0);
    let Some(file) = db.source_file_by_id(symbol_id.file_id) else {
        return empty;
    };
    let resolved = resolve(db, file);
    let Some(symbol) = resolved.symbols.try_get(symbol_id) else {
        return empty;
    };
    let span = symbol.span;
    arandu_base::Span::new(span.file_id, span.start, span.end)
}

/// P5: authoritative CST (rowan). Built from source alone — no AST dependency.
///
/// Uses the DB CST cache + [`arandu_parser::reparse_subtree`] when a single
/// contiguous edit is detected against the previous tree for this file.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "syntax_tree",
    file = ?file.file_id(db),
))]
pub fn syntax_tree(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> HashEq<arandu_parser::SyntaxTree> {
    let text = file.text(db);
    let file_id = *file.file_id(db);
    // Prefer DatabaseImpl incremental path; share Arc text with SourceFile.
    let tree = if let Some(impl_db) = db.as_db_impl() {
        impl_db.syntax_tree_for_arc(file_id, Arc::clone(text))
    } else {
        arandu_parser::parse_syntax_arc(Arc::clone(text))
    };
    HashEq::new(tree)
}

// Kept separate from lowering diagnostics so importing a parse query does not
// accidentally publish another file's syntax errors at a caller boundary.
#[salsa::accumulator]
#[derive(Debug, Clone)]
struct ParseDiagnostic(arandu_parser::ParseError);

/// AST for typeck/resolve: **lowered from CST tokens** (no re-lex, no dual parse).
///
/// Memo stores `Arc<Program>` so per-item queries share the same program without deep-clone.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "parse",
    file = ?file.file_id(db),
))]
pub fn parse(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> HashEq<Result<Arc<Program>, arandu_parser::ParseError>> {
    let tree = syntax_tree(db, file);
    let output = arandu_parser::syntax::lower_syntax_to_program_recovering(tree, *file.file_id(db));
    let first_error = output.diagnostics.first().cloned();
    for diagnostic in output.diagnostics {
        ParseDiagnostic(diagnostic).accumulate(db);
    }
    match first_error {
        Some(error) => HashEq::new(Err(error)),
        None => HashEq::new(Ok(Arc::new(output.program))),
    }
}

/// Recovering-parse diagnostics (lex + syntax) for the IDE boundary.
///
/// `parse` stops at the first error; this surfaces **all** lexical and
/// syntactic errors without a re-lex (diagnostics come from the CST tokens).
/// Used by `file_ide_diagnostics` so the Problems panel is never silently
/// empty on a malformed file.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "parse_diagnostics",
    file = ?file.file_id(db),
))]
pub fn parse_diagnostics(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<Vec<Diagnostic>> {
    // Reuse the same recovering CST lower that produced parse's result.
    let diagnostics = parse::accumulated::<ParseDiagnostic>(db, file)
        .into_iter()
        .map(|diagnostic| Diagnostic::from(diagnostic.0.clone()))
        .collect();
    HashEq::new(diagnostics)
}

#[salsa::tracked(cycle_result = cycle_recover)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "resolve",
    file = ?file.file_id(db),
))]
pub fn resolve(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<ResolutionResult> {
    // Module existence is a first-class input of name resolution. Keep this
    // dependency explicit at the tracked-query boundary: the pure resolver
    // reaches it through `SourceDatabase`, which is intentionally unaware of
    // Salsa and therefore must not be relied upon as the only dependency edge.
    // Body-only edits do not touch the listing, preserving importer cutoff.
    if let Some(roots) = db.as_db_impl().and_then(crate::DatabaseImpl::module_roots) {
        let listing = roots.package_listing(db);
        let _ = listing.entries(db);
    }
    if let Some(database) = db.as_db_impl() {
        if let Some(manifest) = database.project_manifest() {
            let _ = manifest.content_hash(db);
        }
        if let Some(map) = database.package_module_map() {
            let _ = map.bindings(db);
        }
    }
    let program_res = parse(db, file);
    let locals_arc = local_symbols(db, file);

    // The resolver mutates its seed; the memoized local result must stay immutable.
    let locals_owned = (*locals_arc.value).clone();
    let resolved = match &**program_res {
        Ok(program) => arandu_resolve::resolve_imports_and_bodies_with_poll(
            &arandu_resolve::SourceDbLoader(db.as_source_db()),
            program,
            locals_owned,
            || db.unwind_if_revision_cancelled(),
        ),
        Err(_) => locals_owned,
    };

    HashEq::new(resolved)
}

/// Signature-level type-check over an explicit program + resolution result.
///
/// Shared by [`module_signatures`] and [`ide_type_check`] so the imported
/// interface merge stays single-sourced across the strict and IDE paths.
fn signatures_from_program(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    program: &Program,
    resolved_arc: &ResolutionResult,
) -> TypeCheckResult {
    // The checker owns its tables behind Arc; share the resolution result's
    // handles (O(1)) and let the first mutation copy once (COW).
    let symbols = Arc::clone(&resolved_arc.symbols);
    let resolved = Arc::clone(&resolved_arc.resolved);
    let diagnostics = resolved_arc.diagnostics.clone();
    let mut checker = arandu_semantics::TypeChecker::new(
        symbols,
        resolved,
        diagnostics,
        &program.pool,
        database_target_info(db),
    );

    // Merge imported type info (path rewrite shared with resolve).
    // Each `module_signatures` is Salsa-memoized; merge_from is the cold cost.
    for import in &program.imports {
        if let Some(path) = arandu_resolve::canonicalize_import_path(import) {
            if let Some(imported_file) = db.as_source_db().resolve_module_path(&path) {
                let imported_sigs = module_signatures(db, imported_file);
                tracing::debug!(
                    target: "arandu_query",
                    %path,
                    file = ?file.file_id(db),
                    diags = ?imported_sigs.diagnostics,
                    "merged imported module signatures"
                );
                checker
                    .type_info
                    .merge_from(imported_sigs.type_info.as_ref());
                // Generic exports may mention an interface imported by
                // their defining module (for example `T: marker.Sync`).
                // Preserve those contract identities by SymbolId without
                // adding their names to this module's visible scope.
                for constraints in imported_sigs.type_info.param_constraints.values() {
                    for constraint in constraints.iter() {
                        if checker.symbols.try_get(constraint.iface_sym).is_none() {
                            if let Some(symbol) =
                                imported_sigs.symbols.try_get(constraint.iface_sym).cloned()
                            {
                                Arc::make_mut(&mut checker.symbols)
                                    .register_imported_symbol(symbol);
                            }
                        }
                    }
                }
                // Body-derived contracts cross the module boundary
                // through their own HashEq query. A dependency body
                // edit therefore stops here when the public relation is
                // unchanged, while a changed relation invalidates the
                // importing item's checks.
                let recovered_cycle = imported_sigs
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains("cyclic"));
                if !recovered_cycle {
                    let imported_interfaces = borrow_interfaces(db, imported_file);
                    for (symbol, summary) in &imported_interfaces.entries {
                        checker
                            .type_info
                            .return_borrow_summaries
                            .insert(*symbol, summary.clone());
                    }
                }
                for diag in &imported_sigs.diagnostics {
                    if diag.message.contains("cyclic") {
                        checker.diagnostics.push(diag.clone());
                    }
                }
            }
        }
    }

    arandu_semantics::check_signatures(&mut checker, program);
    checker.finish()
}

pub fn cycle_recover_module_signatures(
    _db: &dyn ArandCompilerDb,
    _id: salsa::Id,
    _file: SourceFile,
) -> ModuleSignatures {
    let mut res = TypeCheckResult::empty();
    res.diagnostics.push(arandu_middle::Diagnostic::error(
        arandu_middle::DiagCode::N006ImportConflict,
        "cyclic module signature dependency detected".to_string(),
        arandu_middle::Span::new(0, 0, 0),
    ));
    ModuleSignatures::new(res)
}

#[salsa::tracked(cycle_result = cycle_recover_module_signatures)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "module_signatures",
    file = ?file.file_id(db),
))]
pub fn module_signatures(db: &dyn ArandCompilerDb, file: SourceFile) -> ModuleSignatures {
    let program_res = parse(db, file);
    let resolved_arc = resolve(db, file);

    let res = match &**program_res {
        Ok(program) => signatures_from_program(db, file, program, resolved_arc.value.as_ref()),
        Err(_) => TypeCheckResult {
            symbols: std::sync::Arc::new(arandu_semantics::SymbolTable::default()),
            resolved: Arc::clone(&resolved_arc.resolved),
            type_info: std::sync::Arc::new(arandu_semantics::TypeInfo::default()),
            diagnostics: vec![],
        },
    };

    ModuleSignatures::new(res)
}

/// Per-item input for body typeck: holds current [`Program`] but **HashEq**
/// only fingerprints that item's source span (sibling edits early-cutoff).
#[derive(Clone)]
pub struct ItemSourceInput {
    pub program: Arc<Program>,
    pub item_sym: arandu_middle::SymbolId,
    /// Start of the complete top-level item, including its attributes. Used by
    /// structured fixes without re-parsing diagnostic presentation text.
    pub item_start: u32,
    /// blake3 of the item's source slice for StableHash / early cutoff.
    pub(crate) body_fp: blake3::Hash,
}

/// Backward-compatible alias used by older call sites / StableHash.
pub type FuncBodyInput = ItemSourceInput;

/// Extract one item's AST dependency from `parse`+`resolve` with content-addressed HashEq.
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

    let program_res = parse(db, file);
    let resolved = resolve(db, file);
    let text = file.text(db);

    let Ok(program) = &**program_res else {
        return HashEq::new(ItemSourceInput {
            program: Arc::new(empty_program()),
            item_sym,
            item_start: 0,
            body_fp: blake3::hash(b"parse-error"),
        });
    };

    // Depend on CST for incremental invalidation; fingerprint from text slices (no String alloc).
    let tree = syntax_tree(db, file);
    let ranges = tree.item_ranges();

    let mut body_fp = blake3::hash(b"item-missing");
    let mut item_start = 0;
    for decl_id in &program.decls {
        let decl = program.pool.decl(*decl_id);
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
        if !matches {
            continue;
        }
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
        let mut h = blake3::Hasher::new();
        h.update(b"item_body_v4");
        // Prefer covering CST ITEM range (zero-copy slice of shared text).
        let mut used_cst = false;
        for &(s, e) in &ranges {
            if s <= span.start && span.end <= e {
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
        break;
    }

    HashEq::new(ItemSourceInput {
        program: Arc::clone(program),
        item_sym,
        item_start,
        body_fp,
    })
}

fn empty_program() -> Program {
    Program {
        span: arandu_base::Span::new(0, 0, 0),
        module: None,
        imports: vec![],
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
    let body_in = item_source_input(db, file, item_sym);
    let signatures = module_signatures(db, file);
    let res = arandu_semantics::check_item_body_only(
        signatures,
        body_in.program.as_ref(),
        item_sym,
        database_target_info(db),
    );
    HashEq::new(res)
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
    let program_res = parse(db, file);
    let signatures = module_signatures(db, file);

    let Ok(program) = &**program_res else {
        return HashEq::from_arc(Arc::clone(&signatures.value));
    };

    let item_syms = arandu_semantics::body_item_symbols(program, signatures.resolved.as_ref());

    // O(1) Arc share until the first body merge; avoid deep-cloning TypeInfo up front.
    let mut merged_info = Arc::clone(&signatures.type_info);
    let mut diagnostics = signatures.diagnostics.clone();

    for &item_sym in &item_syms {
        let item = item_body_typeck(db, file, item_sym);
        Arc::make_mut(&mut merged_info).merge_from(item.type_info.as_ref());
        diagnostics.extend(item.diagnostics.iter().cloned());
        diagnostics.extend(
            crate::dataflow::item_attribute_validation(db, file, item_sym)
                .iter()
                .cloned(),
        );
    }

    // Residual for decls without primary keys (normally empty).
    let residual =
        arandu_semantics::check_non_func_bodies_only(signatures, program, database_target_info(db));
    if !residual.diagnostics.is_empty()
        || residual.type_info.expr_types.iter().any(|s| s.is_some())
        || !residual.type_info.decl_types.is_empty()
    {
        Arc::make_mut(&mut merged_info).merge_from(residual.type_info.as_ref());
        diagnostics.extend(residual.diagnostics);
    }

    let res = TypeCheckResult {
        symbols: Arc::clone(&signatures.symbols),
        resolved: Arc::clone(&signatures.resolved),
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
/// through [`signatures_from_program`]. No diagnostics are accumulated: the IDE
/// surfaces recovering-parse diagnostics separately.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "ide_type_check",
    file = ?file.file_id(db),
))]
pub fn ide_type_check(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<TypeCheckResult> {
    let tree = syntax_tree(db, file);
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

    let signatures = signatures_from_program(db, file, &program, &resolved);

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
            database_target_info(db),
        );
        Arc::make_mut(&mut merged_info).merge_from(item.type_info.as_ref());
        diagnostics.extend(item.diagnostics);
    }

    let residual = arandu_semantics::check_non_func_bodies_only(
        &signatures,
        &program,
        database_target_info(db),
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

/// AMIR plus the **post-monomorphize** [`TypeCheckResult`] used to build it.
///
/// Codegen must use `type_check` from this bundle — monomorphization allocates
/// new symbols that do not exist on the pre-mono `type_check` query alone.
/// Returning both from one Salsa query avoids the old CLI double path
/// (local HIR+mono + `lower_amir` HIR+mono) while keeping symbol tables aligned.
#[derive(Debug, Clone)]
pub struct LowerAmirArtifacts {
    pub amir: AmirProgram,
    pub type_check: TypeCheckResult,
}

/// Canonical borrow contracts isolated from the function bodies that proved
/// them. Consumers use this narrow query so Salsa can cut propagation when an
/// implementation edit leaves the public dependency relation unchanged.
#[derive(Debug, Clone)]
pub struct BorrowInterfaces {
    pub entries: Vec<(
        arandu_middle::SymbolId,
        arandu_middle::types::ReturnBorrowSummary,
    )>,
}

/// Collect transitive imports of `root`, lower each to HIR, and link into `hir`.
///
/// ## Why `file_typeck_view` (not `type_check`)
///
/// Salsa accumulators bubble: calling `type_check` on an import from inside
/// `lower_amir(entry)` would re-accumulate the import's body diagnostics into
/// `lower_amir::accumulated(entry)`, so `check` of a clean entry would fail on
/// unrelated stdlib residuals (e.g. `std.alloc`). Body typeck for the link path
/// must not accumulate — [`file_typeck_view`] returns the same `TypeCheckResult`
/// without the DiagnosticsAccumulator side effect.
///
/// Skips cycles, missing modules (prelude-only), parse failures, and modules
/// that cannot lower (`lower_to_hir` error). Import body errors stay on that
/// module's own `type_check` query when the user checks that file directly.
fn link_imported_hir_modules(
    db: &dyn ArandCompilerDb,
    root: SourceFile,
    type_check_result: &mut TypeCheckResult,
    hir: &mut arandu_semantics::hir::HirProgram,
) {
    let mut visited = std::collections::HashSet::new();
    visited.insert(*root.file_id(db));

    fn walk(
        db: &dyn ArandCompilerDb,
        file: SourceFile,
        visited: &mut std::collections::HashSet<u32>,
        type_check_result: &mut TypeCheckResult,
        hir: &mut arandu_semantics::hir::HirProgram,
    ) {
        let program_res = parse(db, file);
        let Ok(program) = &**program_res else {
            return;
        };

        for import in &program.imports {
            let Some(path) = arandu_resolve::canonicalize_import_path(import) else {
                continue;
            };
            let Some(imported_file) = db.as_source_db().resolve_module_path(&path) else {
                // Prelude-only or missing file — nothing to lower.
                continue;
            };
            let imported_id = *imported_file.file_id(db);
            if !visited.insert(imported_id) {
                continue;
            }

            // Depth-first: link dependencies of the import first (post-order-ish).
            walk(db, imported_file, visited, type_check_result, hir);

            let imported_parse = parse(db, imported_file);
            let Ok(imported_program) = &**imported_parse else {
                continue;
            };

            // Full body typeck without accumulating diags into the entry pipeline.
            let imported_tc_arc = file_typeck_view(db, imported_file);
            // Skip modules with hard type errors — signatures already merged via
            // module_signatures; codegen for those bodies is not required for
            // entry check when the entry only references public signatures.
            if imported_tc_arc
                .diagnostics
                .iter()
                .any(|d| matches!(d.severity, arandu_middle::Severity::Error))
            {
                tracing::debug!(
                    target: "arandu_query",
                    %path,
                    "skip HIR link for import (body typeck has errors)"
                );
                continue;
            }
            let mut imported_tc = (**imported_tc_arc).clone();

            match arandu_semantics::lower_to_hir(&mut imported_tc, imported_program) {
                Ok(temp_hir) => {
                    tracing::debug!(
                        target: "arandu_query",
                        %path,
                        decls = temp_hir.decls.len(),
                        "linking imported HIR module"
                    );
                    arandu_semantics::link_hir_module(
                        type_check_result,
                        hir,
                        &imported_tc,
                        &temp_hir,
                    );
                }
                Err(_) => {
                    tracing::debug!(
                        target: "arandu_query",
                        %path,
                        "skip HIR link for import (lower_to_hir failed)"
                    );
                }
            }
        }
    }

    walk(db, root, &mut visited, type_check_result, hir);
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "lower_amir",
    file = ?file.file_id(db),
))]
pub fn lower_amir(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<LowerAmirArtifacts> {
    let program_res = parse(db, file);
    let type_check_result_arc = type_check(db, file);

    // Clone for mutation: lower_to_hir / monomorphize update symbols + type_info.
    // Arc fields are O(1); mono may Arc::make_mut type_info once.
    let mut type_check_result = (**type_check_result_arc).clone();

    let empty_amir = || AmirProgram {
        funcs: vec![],
        literal_pool: arandu_middle::literal_pool::AmirLiteralPool::default(),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
    };

    let mut hir = {
        arandu_base::time_pass!("lower-hir");
        match &**program_res {
            Ok(program) => match arandu_semantics::lower_to_hir(&mut type_check_result, program) {
                Ok(h) => h,
                Err(diags) => {
                    for diag in diags {
                        arandu_middle::db::DiagnosticsAccumulator(diag).accumulate(db);
                    }
                    return HashEq::new(LowerAmirArtifacts {
                        amir: empty_amir(),
                        type_check: type_check_result,
                    });
                }
            },
            Err(_) => {
                return HashEq::new(LowerAmirArtifacts {
                    amir: empty_amir(),
                    type_check: type_check_result,
                });
            }
        }
    };

    // Multi-file HIR: lower imported modules and append their function bodies so
    // monomorphize + codegen see real definitions (not just merged signatures).
    {
        arandu_base::time_pass!("link-hir-imports");
        link_imported_hir_modules(db, file, &mut type_check_result, &mut hir);
    }

    {
        arandu_base::time_pass!("monomorphize");
        if let Err(diags) = arandu_semantics::passes::monomorphize::monomorphize_program(
            &mut type_check_result,
            &mut hir,
        ) {
            for diag in diags {
                arandu_middle::db::DiagnosticsAccumulator(diag).accumulate(db);
            }
            return HashEq::new(LowerAmirArtifacts {
                amir: empty_amir(),
                type_check: type_check_result,
            });
        }
    }

    let amir = {
        arandu_base::time_pass!("lower-amir-body");
        let pointer_width = db.target_config().data_layout(db).pointer_width();
        match arandu_semantics::lower_to_amir_with_interfaces(
            &mut type_check_result,
            &hir,
            pointer_width,
        ) {
            Ok((a, diags)) => {
                for diag in diags {
                    arandu_middle::db::DiagnosticsAccumulator(diag).accumulate(db);
                }
                a
            }
            Err(diags) => {
                for diag in diags {
                    arandu_middle::db::DiagnosticsAccumulator(diag).accumulate(db);
                }
                empty_amir()
            }
        }
    };
    HashEq::new(LowerAmirArtifacts {
        amir,
        type_check: type_check_result,
    })
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "borrow_interfaces",
    file = ?file.file_id(db),
))]
pub fn borrow_interfaces(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<BorrowInterfaces> {
    let lowered = lower_amir(db, file);
    let mut entries = lowered
        .type_check
        .type_info
        .return_borrow_summaries
        .iter()
        .map(|(symbol, summary)| (*symbol, summary.clone()))
        .collect::<Vec<_>>();
    entries.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    HashEq::new(BorrowInterfaces { entries })
}

#[salsa::tracked]
pub fn module_dependency_graph(
    db: &dyn ArandCompilerDb,
    root: SourceFile,
) -> HashEq<petgraph::Graph<u32, ()>> {
    use petgraph::Graph;
    let mut graph = Graph::new();
    let mut visited = std::collections::HashMap::new();

    fn walk(
        db: &dyn ArandCompilerDb,
        file: SourceFile,
        graph: &mut Graph<u32, ()>,
        visited: &mut std::collections::HashMap<u32, petgraph::graph::NodeIndex>,
    ) -> petgraph::graph::NodeIndex {
        let file_id = *file.file_id(db.as_source_db());
        if let Some(&node) = visited.get(&file_id) {
            return node;
        }

        let node = graph.add_node(file_id);
        visited.insert(file_id, node);

        let program_res = crate::passes::parse(db, file);
        if let Ok(program) = &**program_res {
            for import in &program.imports {
                if let Some(path) = arandu_resolve::canonicalize_import_path(import) {
                    if let Some(imported_file) = db.as_source_db().resolve_module_path(&path) {
                        let imported_node = walk(db, imported_file, graph, visited);
                        graph.add_edge(node, imported_node, ());
                    }
                }
            }
        }

        node
    }

    walk(db, root, &mut graph, &mut visited);
    HashEq::new(graph)
}
