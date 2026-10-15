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
        Self::from_arc(Arc::new(value))
    }

    fn from_arc(value: Arc<TypeCheckResult>) -> Self {
        let hash = crate::stable_hash::type_signature_hash(&value);
        Self { value, hash }
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
pub(crate) fn database_target_info(db: &dyn ArandCompilerDb) -> TargetInfo {
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
/// Public surface only. Internal exports live in a separate query so public
pub fn cycle_recover_exported_symbols(
    _db: &dyn ArandCompilerDb,
    _id: salsa::Id,
    _file: SourceFile,
) -> Arc<arandu_middle::ExportedSymbolTable> {
    Arc::new(arandu_middle::ExportedSymbolTable::cycle_fallback())
}

pub fn cycle_recover_internal_symbols(
    _db: &dyn ArandCompilerDb,
    _id: salsa::Id,
    _file: SourceFile,
) -> Arc<arandu_middle::ExportedSymbolTable> {
    Arc::new(arandu_middle::ExportedSymbolTable::cycle_fallback())
}

/// Symbols visible to other files via `import`.
///
/// Public surface only. Internal exports live in a separate query so public
/// consumers retain early-cutoff when package-private declarations change.
#[salsa::tracked(cycle_result = cycle_recover_exported_symbols)]
pub fn exported_symbols(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> Arc<arandu_middle::ExportedSymbolTable> {
    let locals = local_symbols(db, file);
    let mut map = std::collections::BTreeMap::new();
    let mut sealed_symbols = std::collections::BTreeSet::new();
    let mut sealed_ids = rustc_hash::FxHashSet::default();

    let global_scope = locals.symbols.global_scope();
    for symbol in locals.symbols.iter() {
        if symbol.scope == global_scope && symbol.visibility == arandu_parser::Visibility::Public {
            if locals.symbols.sealed_interfaces.contains(&symbol.id) {
                sealed_symbols.insert(symbol.name.to_string());
                sealed_ids.insert(symbol.id);
            }
            map.insert(symbol.name.to_string(), (symbol.id, symbol.kind));
        }
    }

    let mut sealed_implementations: Vec<_> = locals
        .symbols
        .interface_implementations
        .iter()
        .filter(|(_, interface)| sealed_ids.contains(interface))
        .copied()
        .collect();

    let program_res = parse(db, file);
    if let Ok(program) = &**program_res {
        for import in &program.imports {
            if let arandu_parser::ImportDecl::ReExport {
                visibility: arandu_parser::Visibility::Public,
                items,
                ..
            } = import
            {
                if let Some(path_key) = arandu_resolve::canonicalize_import_path(import) {
                    if let Some(target_file) = db.as_source_db().resolve_module_path(&path_key) {
                        let target_exports = exported_symbols(db, target_file);
                        if target_exports.is_cycle {
                            return Arc::new(arandu_middle::ExportedSymbolTable::cycle_fallback());
                        }
                        for item in items {
                            let name = item.name.as_str();
                            if let Some(&(id, kind)) = target_exports.symbols.get(name) {
                                let export_name =
                                    item.alias.as_ref().unwrap_or(&item.name).to_string();
                                if target_exports.sealed_symbols.contains(name) {
                                    sealed_symbols.insert(export_name.clone());
                                    sealed_ids.insert(id);
                                }
                                for &(ty, iface) in &target_exports.sealed_implementations {
                                    if iface == id {
                                        sealed_implementations.push((ty, iface));
                                    }
                                }
                                if matches!(
                                    kind,
                                    arandu_middle::SymbolKind::Struct
                                        | arandu_middle::SymbolKind::Enum
                                        | arandu_middle::SymbolKind::TypeAlias
                                ) {
                                    let prefix = format!("{name}.");
                                    for (k, &(assoc_id, assoc_kind)) in &target_exports.symbols {
                                        if matches!(
                                            assoc_kind,
                                            arandu_middle::SymbolKind::AssociatedFunc
                                        ) {
                                            if let Some(method) = k.strip_prefix(&prefix) {
                                                map.insert(
                                                    format!("{export_name}.{method}"),
                                                    (assoc_id, assoc_kind),
                                                );
                                            }
                                        }
                                    }
                                }
                                map.insert(export_name, (id, kind));
                            }
                        }
                    }
                }
            }
        }
    }

    sealed_implementations.sort_unstable_by_key(|(ty, interface)| {
        (
            ty.file_id,
            ty.local_id.0,
            interface.file_id,
            interface.local_id.0,
        )
    });
    sealed_implementations.dedup();
    Arc::new(arandu_middle::ExportedSymbolTable {
        symbols: map,
        internal_symbols: std::collections::BTreeMap::new(),
        sealed_symbols,
        sealed_implementations,
        is_cycle: false,
    })
}

#[salsa::tracked(cycle_result = cycle_recover_internal_symbols)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "internal_symbols",
    file = ?file.file_id(db),
))]
pub fn internal_symbols(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> Arc<arandu_middle::ExportedSymbolTable> {
    let locals = local_symbols(db, file);
    let mut map = std::collections::BTreeMap::new();
    let mut sealed_symbols = std::collections::BTreeSet::new();
    let mut sealed_ids = rustc_hash::FxHashSet::default();
    let global_scope = locals.symbols.global_scope();
    for symbol in locals.symbols.iter() {
        if symbol.scope == global_scope && symbol.visibility == arandu_parser::Visibility::Internal
        {
            if locals.symbols.sealed_interfaces.contains(&symbol.id) {
                sealed_symbols.insert(symbol.name.to_string());
                sealed_ids.insert(symbol.id);
            }
            map.insert(symbol.name.to_string(), (symbol.id, symbol.kind));
        }
    }
    let mut sealed_implementations: Vec<_> = locals
        .symbols
        .interface_implementations
        .iter()
        .filter(|(_, interface)| sealed_ids.contains(interface))
        .copied()
        .collect();

    let program_res = parse(db, file);
    if let Ok(program) = &**program_res {
        for import in &program.imports {
            if let arandu_parser::ImportDecl::ReExport {
                visibility: arandu_parser::Visibility::Internal,
                items,
                ..
            } = import
            {
                if let Some(path_key) = arandu_resolve::canonicalize_import_path(import) {
                    if let Some(target_file) = db.as_source_db().resolve_module_path(&path_key) {
                        let is_same_package = db
                            .as_source_db()
                            .same_package(*file.file_id(db), target_file);
                        let target_exports = exported_symbols(db, target_file);
                        let target_internal = if is_same_package {
                            Some(internal_symbols(db, target_file))
                        } else {
                            None
                        };
                        if target_exports.is_cycle
                            || target_internal.as_ref().is_some_and(|t| t.is_cycle)
                        {
                            return Arc::new(arandu_middle::ExportedSymbolTable::cycle_fallback());
                        }
                        for item in items {
                            let name = item.name.as_str();
                            if let Some(&(id, kind)) =
                                target_exports.symbols.get(name).or_else(|| {
                                    target_internal
                                        .as_ref()
                                        .and_then(|t| t.internal_symbols.get(name))
                                })
                            {
                                let export_name =
                                    item.alias.as_ref().unwrap_or(&item.name).to_string();
                                if target_exports.sealed_symbols.contains(name)
                                    || target_internal
                                        .as_ref()
                                        .is_some_and(|t| t.sealed_symbols.contains(name))
                                {
                                    sealed_symbols.insert(export_name.clone());
                                    sealed_ids.insert(id);
                                }
                                for &(ty, iface) in
                                    target_exports.sealed_implementations.iter().chain(
                                        target_internal
                                            .as_ref()
                                            .map(|t| t.sealed_implementations.as_slice())
                                            .unwrap_or_default(),
                                    )
                                {
                                    if iface == id {
                                        sealed_implementations.push((ty, iface));
                                    }
                                }
                                if matches!(
                                    kind,
                                    arandu_middle::SymbolKind::Struct
                                        | arandu_middle::SymbolKind::Enum
                                        | arandu_middle::SymbolKind::TypeAlias
                                ) {
                                    let prefix = format!("{name}.");
                                    for (k, &(assoc_id, assoc_kind)) in
                                        target_exports.symbols.iter().chain(
                                            target_internal
                                                .iter()
                                                .flat_map(|t| t.internal_symbols.iter()),
                                        )
                                    {
                                        if matches!(
                                            assoc_kind,
                                            arandu_middle::SymbolKind::AssociatedFunc
                                        ) {
                                            if let Some(method) = k.strip_prefix(&prefix) {
                                                map.insert(
                                                    format!("{export_name}.{method}"),
                                                    (assoc_id, assoc_kind),
                                                );
                                            }
                                        }
                                    }
                                }
                                map.insert(export_name, (id, kind));
                            }
                        }
                    }
                }
            }
        }
    }

    sealed_implementations.sort_unstable_by_key(|(ty, interface)| {
        (
            ty.file_id,
            ty.local_id.0,
            interface.file_id,
            interface.local_id.0,
        )
    });
    sealed_implementations.dedup();
    Arc::new(arandu_middle::ExportedSymbolTable {
        symbols: std::collections::BTreeMap::new(),
        internal_symbols: map,
        sealed_symbols,
        sealed_implementations,
        is_cycle: false,
    })
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
    let program = parse(db, file);
    let headers = resolved_headers(db, file);
    let result = match &**program {
        Ok(program) => {
            let mut headers = (**headers).clone();
            program.for_each_decl_recursive(|_, declaration| {
                if let arandu_parser::TopLevelDecl::Func(function) = declaration {
                    if !function.generic_params.is_empty()
                        && (!crate::ctfe::roots::const_arguments(&program.pool, function.body.span)
                            .is_empty()
                            || program.pool.exprs.iter().zip(&program.pool.expr_spans).any(
                                |(expression, span)| {
                                    function.body.span.start <= span.start
                                        && span.end <= function.body.span.end
                                        && matches!(
                                            expression,
                                            arandu_parser::ExprKind::Comptime { .. }
                                        )
                                },
                            )
                            || program.pool.stmts.iter().any(|statement| {
                                let span = statement.span();
                                function.body.span.start <= span.start
                                    && span.end <= function.body.span.end
                                    && matches!(
                                        statement,
                                        arandu_parser::Stmt::If {
                                            is_comptime: true,
                                            ..
                                        } | arandu_parser::Stmt::For {
                                            is_comptime: true,
                                            ..
                                        }
                                    )
                            }))
                    {
                        if let Some(owner) = arandu_semantics::primary_def_key(declaration)
                            .and_then(|key| headers.declarations.resolved.definitions.get(&key))
                            .copied()
                        {
                            Arc::make_mut(&mut headers.declarations.resolved)
                                .deferred_comptime_functions
                                .insert(owner);
                        }
                    }
                }
            });
            if program.pool.stmts.iter().any(|stmt| {
                matches!(
                    stmt,
                    arandu_parser::Stmt::For {
                        is_comptime: true,
                        ..
                    }
                )
            }) {
                program.for_each_decl_recursive(|_, declaration| {
                    if !matches!(declaration, arandu_parser::TopLevelDecl::Func(_)) {
                        return;
                    }
                    if let Some(owner) = arandu_semantics::primary_def_key(declaration)
                        .and_then(|key| headers.declarations.resolved.definitions.get(&key))
                        .copied()
                    {
                        if headers
                            .declarations
                            .resolved
                            .deferred_comptime_functions
                            .contains(&owner)
                        {
                            return;
                        }
                        let loops = crate::ctfe::item_static_loops(db, file, owner);
                        Arc::make_mut(&mut headers.declarations.resolved)
                            .comptime_loops
                            .extend(loops.domains.iter().map(|(&key, &value)| (key, value)));
                        Arc::make_mut(&mut headers.declarations.resolved)
                            .deferred_loop_bodies
                            .extend(loops.domains.keys().copied().filter(|key| {
                                program.pool.stmts.iter().any(|statement| match statement {
                                    arandu_parser::Stmt::For { span, body, .. }
                                        if arandu_middle::NodeKey::from(*span) == *key =>
                                    {
                                        loops.empty_domain(key)
                                            || crate::ctfe::roots::loop_requires_occurrences(
                                                &program.pool,
                                                body,
                                            )
                                    }
                                    _ => false,
                                }) && !program.pool.exprs.iter().zip(&program.pool.expr_spans).any(
                                    |(expression, span)| {
                                        matches!(
                                            expression,
                                            arandu_parser::ExprKind::Comptime { .. }
                                        ) && span.start <= key.start
                                            && key.end <= span.end
                                    },
                                )
                            }));
                        headers
                            .declarations
                            .diagnostics
                            .extend(loops.diagnostics.iter().cloned());
                    }
                });
            }
            if program.pool.stmts.iter().any(|stmt| {
                matches!(
                    stmt,
                    arandu_parser::Stmt::If {
                        is_comptime: true,
                        ..
                    }
                )
            }) {
                program.for_each_decl_recursive(|_, declaration| {
                    if matches!(declaration, arandu_parser::TopLevelDecl::Func(_)) {
                        if let Some(owner) = arandu_semantics::primary_def_key(declaration)
                            .and_then(|key| headers.declarations.resolved.definitions.get(&key))
                            .copied()
                        {
                            if headers
                                .declarations
                                .resolved
                                .deferred_comptime_functions
                                .contains(&owner)
                            {
                                return;
                            }
                            let selected = crate::ctfe::item_static_branches(db, file, owner);
                            Arc::make_mut(&mut headers.declarations.resolved)
                                .comptime_branches
                                .extend(
                                    selected.decisions.iter().map(|(&key, &value)| (key, value)),
                                );
                            headers
                                .declarations
                                .diagnostics
                                .extend(selected.diagnostics.iter().cloned());
                        }
                    }
                });
            }
            arandu_resolve::resolve_bodies_with_poll(program, headers, || {
                db.unwind_if_revision_cancelled()
            })
        }
        Err(_) => headers.declarations.clone(),
    };
    HashEq::new(result)
}

/// Resolution of imports/declarations only. This is the pre-body staging edge;
/// headers do not request full resolution or execute CTFE themselves.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "resolved_headers", file = ?file.file_id(db),
))]
pub fn resolved_headers(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> HashEq<arandu_resolve::HeaderResolution> {
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
        Ok(program) => arandu_resolve::resolve_headers_with_poll(
            &arandu_resolve::SourceDbLoader(db.as_source_db()),
            program,
            locals_owned,
            || db.unwind_if_revision_cancelled(),
        ),
        Err(_) => arandu_resolve::resolve_headers_with_poll(
            &arandu_resolve::EmptyModuleLoader,
            &empty_program(),
            locals_owned,
            || {},
        ),
    };

    HashEq::new(resolved)
}

/// Signature-level type-check over an explicit program + resolution result.
///
/// Shared by [`declaration_signatures`] and [`ide_type_check`]. Only declaration
/// contracts cross imports here: no body check, borrow-interface solve or final
/// lowering may be requested from this staging boundary.
pub(crate) fn signatures_from_program(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    program: &Program,
    resolved_arc: &ResolutionResult,
    headers_only: bool,
) -> TypeCheckResult {
    signatures_with_imports(db, file, program, resolved_arc, headers_only, true)
}

fn signatures_with_imports(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    program: &Program,
    resolved_arc: &ResolutionResult,
    headers_only: bool,
    include_imports: bool,
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

    checker.type_info.target_identity = db.target_config().identity(db).clone();
    checker.type_info.target_layout = *db.target_config().data_layout(db);

    // Merge imported type info (path rewrite shared with resolve).
    // Each `declaration_signatures` is Salsa-memoized; merge_from is the cold cost.
    for import in program.imports.iter().filter(|_| include_imports) {
        if let Some(path) = arandu_resolve::canonicalize_import_path(import) {
            if let Some(imported_file) = db.as_source_db().resolve_module_path(&path) {
                if exported_symbols(db, imported_file).is_cycle {
                    continue;
                }
                let imported_sigs = if headers_only {
                    seed_header_signatures(db, imported_file)
                } else {
                    header_signatures(db, imported_file)
                };
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
                for (&var_id, &(parent_id, _)) in &imported_sigs.type_info.enum_variants {
                    if checker.symbols.try_get(var_id).is_none() {
                        if let Some(symbol) = imported_sigs.symbols.try_get(var_id).cloned() {
                            Arc::make_mut(&mut checker.symbols).register_imported_symbol(symbol);
                        }
                    }
                    if checker.symbols.try_get(parent_id).is_none() {
                        if let Some(symbol) = imported_sigs.symbols.try_get(parent_id).cloned() {
                            Arc::make_mut(&mut checker.symbols).register_imported_symbol(symbol);
                        }
                    }
                }
                for &struct_id in imported_sigs.type_info.struct_fields.keys() {
                    if checker.symbols.try_get(struct_id).is_none() {
                        if let Some(symbol) = imported_sigs.symbols.try_get(struct_id).cloned() {
                            Arc::make_mut(&mut checker.symbols).register_imported_symbol(symbol);
                        }
                    }
                }
                for &destructor_id in imported_sigs.type_info.destructors.values() {
                    if checker.symbols.try_get(destructor_id).is_none() {
                        if let Some(symbol) = imported_sigs.symbols.try_get(destructor_id).cloned()
                        {
                            Arc::make_mut(&mut checker.symbols).register_imported_symbol(symbol);
                        }
                    }
                }
                for ((type_id, member_name), member_sym) in
                    &imported_sigs.symbols.associated_members
                {
                    if let Some(symbol) = imported_sigs.symbols.try_get(*member_sym) {
                        let is_accessible = match symbol.visibility {
                            arandu_parser::Visibility::Public => true,
                            arandu_parser::Visibility::Internal => db
                                .as_source_db()
                                .same_package(*file.file_id(db), imported_file),
                            arandu_parser::Visibility::Private
                            | arandu_parser::Visibility::Module => false,
                        };
                        if !is_accessible {
                            continue;
                        }
                        if !checker
                            .symbols
                            .associated_members
                            .contains_key(&(*type_id, member_name.clone()))
                        {
                            Arc::make_mut(&mut checker.symbols)
                                .associated_members
                                .insert((*type_id, member_name.clone()), *member_sym);
                        }
                        if checker.symbols.try_get(*member_sym).is_none() {
                            Arc::make_mut(&mut checker.symbols)
                                .register_imported_symbol(symbol.clone());
                        }
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

    crate::ctfe::headers::defer_generic_headers(program, &mut checker);
    arandu_semantics::check_signatures(&mut checker, program);
    checker.finish()
}

/// Compose body-derived contracts only for consumers that perform ordinary
/// body/ownership checks. Keeping this edge out of declaration signatures is
/// essential for later CTFE staging; missing contracts must not silently weaken
/// the existing runtime or recovering IDE paths.
fn merge_imported_borrow_interfaces(
    db: &dyn ArandCompilerDb,
    program: &Program,
    result: &mut Arc<TypeCheckResult>,
) {
    for import in &program.imports {
        db.unwind_if_revision_cancelled();
        let Some(path) = arandu_resolve::canonicalize_import_path(import) else {
            continue;
        };
        let Some(imported_file) = db.as_source_db().resolve_module_path(&path) else {
            continue;
        };
        if exported_symbols(db, imported_file).is_cycle {
            continue;
        }
        let signatures = declaration_signatures(db, imported_file);
        if signatures
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("cyclic"))
        {
            continue;
        }
        let interfaces = borrow_interfaces(db, imported_file);
        for (symbol, summary) in &interfaces.entries {
            let checked = Arc::make_mut(result);
            Arc::make_mut(&mut checked.type_info)
                .return_borrow_summaries
                .insert(*symbol, summary.clone());
        }
    }
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

/// Canonical direct import edges. The query deliberately exposes only the
/// dependency shape, so a private body edit can early-cut off unchanged edges.
#[salsa::tracked]
pub(crate) fn module_import_edges(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> Vec<(String, Option<crate::db::FileId>)> {
    if let Some(roots) = db.as_db_impl().and_then(crate::DatabaseImpl::module_roots) {
        let _ = roots.package_listing(db).entries(db);
    }
    if let Some(database) = db.as_db_impl() {
        if let Some(map) = database.package_module_map() {
            let _ = map.bindings(db);
        }
    }

    let program_res = parse(db, file);
    let Ok(program) = &**program_res else {
        return Vec::new();
    };
    program
        .imports
        .iter()
        .filter_map(|import| {
            let path = arandu_resolve::canonicalize_import_path(import)?;
            let file_id = db
                .as_source_db()
                .resolve_module_path(&path)
                .map(|imported| *imported.file_id(db));
            Some((path, file_id))
        })
        .collect()
}

/// Fingerprint import topology across the reachable module graph without
/// recursing through semantic queries. This provides a cycle-safe Salsa edge
/// for consumers whose recursive `module_signatures` dependencies were
/// recovered while the import graph had a different cycle shape.
#[salsa::tracked]
fn module_graph_fingerprint(db: &dyn ArandCompilerDb, file: SourceFile) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"module-graph/v1");
    let mut pending = std::collections::VecDeque::from([file]);
    let mut visited = std::collections::HashSet::new();
    while let Some(file) = pending.pop_front() {
        let file_id = *file.file_id(db);
        if !visited.insert(file_id) {
            continue;
        }
        hasher.update(&file_id.to_le_bytes());
        let edges = module_import_edges(db, file);
        hasher.update(&(edges.len() as u64).to_le_bytes());
        for (path, imported_id) in edges {
            hasher.update(&(path.len() as u64).to_le_bytes());
            hasher.update(path.as_bytes());
            match imported_id {
                Some(imported_id) => {
                    hasher.update(&[1]);
                    hasher.update(&imported_id.to_le_bytes());
                    if let Some(imported_file) = db.source_file_by_id(*imported_id) {
                        pending.push_back(imported_file);
                    }
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
    }
    hasher.finalize()
}

/// Declaration-only staging boundary. Imports recurse through this same query,
/// never through [`module_signatures`] or [`borrow_interfaces`]. Signature-
/// inferred borrow contracts remain available, but body-derived ones do not.
#[salsa::tracked(cycle_result = cycle_recover_module_signatures)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "declaration_signatures",
    file = ?file.file_id(db),
))]
pub fn declaration_signatures(db: &dyn ArandCompilerDb, file: SourceFile) -> ModuleSignatures {
    let _ = module_graph_fingerprint(db, file);
    let program_res = parse(db, file);
    let resolved_arc = resolve(db, file);
    let package_implementations = sealed_package_implementations(db, file);
    let mut resolved = resolved_arc.value.as_ref().clone();
    if !package_implementations.is_empty() {
        Arc::make_mut(&mut resolved.symbols)
            .interface_implementations
            .extend(package_implementations);
    }

    let staged_headers = crate::ctfe::headers::module_arguments(db, file);
    Arc::make_mut(&mut resolved.resolved)
        .comptime_arguments
        .extend(
            staged_headers
                .values
                .iter()
                .map(|(&key, &value)| (key, value)),
        );
    Arc::make_mut(&mut resolved.resolved)
        .typed_comptime_arguments
        .extend(
            staged_headers
                .typed_values
                .iter()
                .map(|(&key, value)| (key, value.clone())),
        );
    let res = match &**program_res {
        Ok(program) => signatures_from_program(db, file, program, &resolved, false),
        Err(_) => TypeCheckResult {
            symbols: std::sync::Arc::new(arandu_semantics::SymbolTable::default()),
            resolved: Arc::clone(&resolved.resolved),
            type_info: std::sync::Arc::new(arandu_semantics::TypeInfo::default()),
            diagnostics: vec![],
        },
    };

    let mut res = res;
    res.diagnostics
        .extend(staged_headers.diagnostics.iter().cloned());
    crate::ctfe::globals::install_module_globals(db, file, &mut res);
    ModuleSignatures::new(res)
}

// A seed cycle publishes only this module's local contracts. Constant
// dependency queries independently detect value cycles; recursive imports
// must not preempt them with a whole-module signature failure.
fn cycle_recover_seed_headers(
    db: &dyn ArandCompilerDb,
    _id: salsa::Id,
    file: SourceFile,
) -> ModuleSignatures {
    let parsed = parse(db, file);
    let headers = resolved_headers(db, file);
    let mut checked = match &**parsed {
        Ok(program) => {
            signatures_with_imports(db, file, program, &headers.declarations, true, false)
        }
        Err(_) => TypeCheckResult::empty(),
    };
    checked.diagnostics.push(arandu_middle::Diagnostic::error(
        arandu_middle::DiagCode::N006ImportConflict,
        "cyclic module signature dependency detected",
        arandu_middle::Span::new(*file.file_id(db), 0, 0),
    ));
    ModuleSignatures::new(checked)
}

/// Initial typed headers for pre-resolution staging. Recurses only through this
/// same header boundary, never through function bodies or borrow contracts.
#[salsa::tracked(cycle_result = cycle_recover_seed_headers)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "seed_header_signatures", file = ?file.file_id(db),
))]
pub fn seed_header_signatures(db: &dyn ArandCompilerDb, file: SourceFile) -> ModuleSignatures {
    let _ = module_graph_fingerprint(db, file);
    let program = parse(db, file);
    let headers = resolved_headers(db, file);
    let result = match &**program {
        Ok(program) => signatures_from_program(db, file, program, &headers.declarations, true),
        Err(_) => TypeCheckResult::empty(),
    };
    ModuleSignatures::new(result)
}

/// Final header view with frozen constant types; seed queries never depend on it.
#[salsa::tracked(cycle_result = cycle_recover_module_signatures)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(query = "header_signatures", file = ?file.file_id(db)))]
pub fn header_signatures(db: &dyn ArandCompilerDb, file: SourceFile) -> ModuleSignatures {
    let mut checked = crate::ctfe::headers::staged_signatures(db, file);
    crate::ctfe::globals::install_module_globals(db, file, &mut checked);
    ModuleSignatures::new(checked)
}

/// Compatibility/body-checking view: declarations plus imported flow-derived
/// borrow contracts. Its callers retain their existing ownership guarantees;
/// only staged consumers may deliberately request declaration signatures alone.
#[salsa::tracked(cycle_result = cycle_recover_module_signatures)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "module_signatures",
    file = ?file.file_id(db),
))]
pub fn module_signatures(db: &dyn ArandCompilerDb, file: SourceFile) -> ModuleSignatures {
    let declarations = declaration_signatures(db, file);
    let program = parse(db, file);
    let Ok(program) = &**program else {
        return declarations.clone();
    };
    let mut checked = Arc::clone(&declarations.value);
    merge_imported_borrow_interfaces(db, program, &mut checked);
    if Arc::ptr_eq(&checked, &declarations.value) {
        declarations.clone()
    } else {
        ModuleSignatures::from_arc(checked)
    }
}

/// Collect explicit sealed-interface edges from every module in the current
/// package. Resolution remains per-file; this query only composes its typed
/// symbol IDs and never inspects the filesystem.
#[salsa::tracked]
fn sealed_package_implementations(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> Vec<(arandu_middle::SymbolId, arandu_middle::SymbolId)> {
    let Some(database) = db.as_db_impl() else {
        return resolve(db, file)
            .symbols
            .interface_implementations
            .iter()
            .copied()
            .collect();
    };
    let Some(map) = database.package_module_map() else {
        return resolve(db, file)
            .symbols
            .interface_implementations
            .iter()
            .copied()
            .collect();
    };
    let current_file = *file.file_id(db);
    let package = map
        .bindings(db)
        .iter()
        .find(|(_, binding)| *binding.file.file_id(db) == current_file)
        .map(|(_, binding)| binding.package);
    let Some(package) = package else {
        return resolve(db, file)
            .symbols
            .interface_implementations
            .iter()
            .copied()
            .collect();
    };

    let mut implementations = rustc_hash::FxHashSet::default();
    for (_, binding) in map.bindings(db).iter() {
        if binding.package != package {
            continue;
        }
        db.unwind_if_revision_cancelled();
        implementations.extend(
            resolve(db, binding.file)
                .symbols
                .interface_implementations
                .iter()
                .copied(),
        );
    }
    let mut implementations: Vec<_> = implementations.into_iter().collect();
    implementations.sort_unstable_by_key(|(ty, interface)| {
        (
            ty.file_id,
            ty.local_id.0,
            interface.file_id,
            interface.local_id.0,
        )
    });
    implementations
}

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

    let program_res = parse(db, file);
    let headers = resolved_headers(db, file);
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
    let tree = syntax_tree(db, file);
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

fn empty_program() -> Program {
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
    let signatures = module_signatures(db, file);
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
) -> HashEq<TypeCheckResult> {
    let body_in = item_source_input(db, file, item_sym);
    let signatures = declaration_signatures(db, file);
    if signatures
        .resolved
        .deferred_comptime_functions
        .contains(&item_sym)
    {
        return HashEq::from_arc(Arc::clone(&signatures.value));
    }
    let arguments = crate::ctfe::item_const_arguments(db, file, item_sym);
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
    let initial = if initial.type_info.deferred_headers.contains(&item_sym)
        && !initial
            .type_info
            .generic_params
            .get(&item_sym)
            .is_some_and(|parameters| !parameters.is_empty())
    {
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
            return HashEq::new(concrete);
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
    let program_res = parse(db, file);
    let signatures = if declarations_only {
        declaration_signatures(db, file)
    } else {
        module_signatures(db, file)
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

    let mut signatures = Arc::new(signatures_from_program(
        db, file, &program, &resolved, false,
    ));
    merge_imported_borrow_interfaces(db, &program, &mut signatures);

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

/// Shared canonical HIR/monomorphization stage. Its conservative fingerprint
/// records the typed source inputs, not Debug output or incidental addresses.
/// Narrow public projections (`borrow_interfaces`) provide semantic cutoff.
#[derive(Debug)]
pub struct PreparedHir {
    pub hir: Option<arandu_middle::hir::HirProgram>,
    pub type_check: TypeCheckResult,
    pub diagnostics: Vec<Diagnostic>,
    pub(crate) source_fingerprint: blake3::Hash,
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
    /// Concrete contracts use structural keys, never synthetic symbol numbers
    /// allocated in another producer's context.
    pub instances: Vec<(
        arandu_middle::types::FunctionInstance,
        arandu_middle::types::ReturnBorrowSummary,
    )>,
}

/// Collect transitive imports of `root`, lower each to HIR, and link into `hir`.
///
/// ## Why `file_typing` (not `type_check`)
///
/// Salsa accumulators bubble: calling `type_check` on an import from inside
/// `lower_amir(entry)` would re-accumulate the import's body diagnostics into
/// `lower_amir::accumulated(entry)`, so `check` of a clean entry would fail on
/// unrelated stdlib residuals (e.g. `std.alloc`). Body typeck for the link path
/// must not accumulate or request callee flow contracts: [`file_typing`] uses
/// the same canonical body checker, without the compatibility composition.
///
/// Skips cycles, missing modules (prelude-only), parse failures, and modules
/// that cannot lower (`lower_to_hir` error). Import body errors stay on that
/// module's own `type_check` query when the user checks that file directly.
fn link_imported_hir_modules(
    db: &dyn ArandCompilerDb,
    root: SourceFile,
    type_check_result: &mut TypeCheckResult,
    hir: &mut arandu_semantics::hir::HirProgram,
    fingerprint: &mut blake3::Hasher,
) {
    let mut visited = std::collections::HashSet::new();
    visited.insert(*root.file_id(db));

    fn walk(
        db: &dyn ArandCompilerDb,
        file: SourceFile,
        visited: &mut std::collections::HashSet<u32>,
        type_check_result: &mut TypeCheckResult,
        hir: &mut arandu_semantics::hir::HirProgram,
        fingerprint: &mut blake3::Hasher,
    ) {
        let program_res = parse(db, file);
        let Ok(program) = &**program_res else {
            return;
        };

        for import in &program.imports {
            db.unwind_if_revision_cancelled();
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
            walk(
                db,
                imported_file,
                visited,
                type_check_result,
                hir,
                fingerprint,
            );

            let imported_parse = parse(db, imported_file);
            fingerprint.update(crate::StableHash::stable_hash(&**imported_parse).as_bytes());
            let Ok(imported_program) = &**imported_parse else {
                continue;
            };

            // Full body typeck without accumulating diags into the entry pipeline.
            let imported_tc_arc = file_typing(db, imported_file);
            fingerprint.update(crate::StableHash::stable_hash(&**imported_tc_arc).as_bytes());
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

    walk(db, root, &mut visited, type_check_result, hir, fingerprint);
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "prepare_hir", file = ?file.file_id(db),
))]
pub fn prepare_hir(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<PreparedHir> {
    let program_res = parse(db, file);
    let type_check_result_arc = file_typing(db, file);
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"PreparedHir/sources/v1");
    fingerprint.update(crate::StableHash::stable_hash(&**program_res).as_bytes());
    fingerprint.update(crate::StableHash::stable_hash(&**type_check_result_arc).as_bytes());

    // Clone for mutation: lower_to_hir / monomorphize update symbols + type_info.
    // Arc fields are O(1); mono may Arc::make_mut type_info once.
    let mut type_check_result = (**type_check_result_arc).clone();

    let mut hir = {
        arandu_base::time_pass!("lower-hir");
        match &**program_res {
            Ok(program) => match arandu_semantics::lower_to_hir(&mut type_check_result, program) {
                Ok(h) => h,
                Err(diags) => {
                    return HashEq::new(PreparedHir {
                        hir: None,
                        type_check: type_check_result,
                        diagnostics: diags,
                        source_fingerprint: fingerprint.finalize(),
                    });
                }
            },
            Err(_) => {
                return HashEq::new(PreparedHir {
                    hir: None,
                    type_check: type_check_result,
                    diagnostics: Vec::new(),
                    source_fingerprint: fingerprint.finalize(),
                });
            }
        }
    };

    // Multi-file HIR: lower imported modules and append their function bodies so
    // monomorphize + codegen see real definitions (not just merged signatures).
    {
        arandu_base::time_pass!("link-hir-imports");
        link_imported_hir_modules(db, file, &mut type_check_result, &mut hir, &mut fingerprint);
    }

    {
        arandu_base::time_pass!("monomorphize");
        db.unwind_if_revision_cancelled();
        if let Err(diags) = arandu_semantics::passes::monomorphize::monomorphize_program(
            &mut type_check_result,
            &mut hir,
        ) {
            return HashEq::new(PreparedHir {
                hir: None,
                type_check: type_check_result,
                diagnostics: diags,
                source_fingerprint: fingerprint.finalize(),
            });
        }
    }
    db.unwind_if_revision_cancelled();

    HashEq::new(PreparedHir {
        hir: Some(hir),
        type_check: type_check_result,
        diagnostics: Vec::new(),
        source_fingerprint: fingerprint.finalize(),
    })
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "lower_amir", file = ?file.file_id(db),
))]
pub fn lower_amir(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<LowerAmirArtifacts> {
    // Executable assembly consumes independently checked instances. Share the
    // composed result instead of cloning the program into a compatibility memo.
    let composed = crate::runtime::runtime_program(db, file);
    for diagnostic in &composed.diagnostics {
        arandu_middle::db::DiagnosticsAccumulator(diagnostic.clone()).accumulate(db);
    }
    HashEq::from_arc(std::sync::Arc::clone(&composed.artifacts))
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "borrow_interfaces",
    file = ?file.file_id(db),
))]
pub fn borrow_interfaces(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<BorrowInterfaces> {
    use crate::StableHash;
    let declarations = declaration_signatures(db, file);
    let parsed = parse(db, file);
    let mut summaries = declarations.type_info.return_borrow_summaries.clone();
    let mut concrete = rustc_hash::FxHashMap::default();
    if let Ok(program) = &**parsed {
        for symbol in arandu_semantics::free_func_symbols(program, &declarations.resolved) {
            db.unwind_if_revision_cancelled();
            // Uninstantiated templates retain declaration contracts only.
            // Their executable proof belongs to each concrete instance.
            if declarations.type_info.generic_params.contains_key(&symbol) {
                continue;
            }
            let carries_borrow =
                declarations
                    .type_info
                    .decl_type(symbol)
                    .is_some_and(|ty| match ty {
                        arandu_middle::types::ArType::Func(_, result) => declarations
                            .type_info
                            .borrow_paths(result)
                            .map_or(true, |paths| !paths.is_empty()),
                        _ => false,
                    });
            if !carries_borrow {
                continue;
            }
            let instance = crate::runtime::Instance::new(
                db,
                file,
                arandu_middle::types::FunctionInstance {
                    definition: symbol,
                    arguments: Vec::new(),
                },
            );
            let contracts = crate::runtime::instance_contracts(db, instance);
            // Failed source recovery never certifies the callee. Runtime/IDE
            // units own and report its error; keep declaration recovery here.
            if !contracts.diagnostics.is_empty() {
                continue;
            }
            for (key, summary) in &contracts.entries {
                if key.arguments.is_empty() {
                    summaries.insert(key.definition, summary.clone());
                } else {
                    concrete.insert(key.clone(), summary.clone());
                }
            }
        }
    }
    let mut entries = summaries
        .into_iter()
        .filter(|(_, summary)| !summary.dependencies.is_empty())
        .collect::<Vec<_>>();
    entries.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    let mut instances = concrete
        .into_iter()
        .filter(|(_, summary)| !summary.dependencies.is_empty())
        .collect::<Vec<_>>();
    instances.sort_by_key(|(key, _)| {
        (
            key.definition.file_id,
            key.definition.local_id.0,
            *key.stable_hash().as_bytes(),
        )
    });
    HashEq::new(BorrowInterfaces { entries, instances })
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
