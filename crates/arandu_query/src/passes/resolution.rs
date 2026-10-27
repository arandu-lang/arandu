use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::Diagnostic;
use arandu_parser::Program;
use arandu_resolve::ResolutionResult;
use arandu_semantics::TypeCheckResult;
use salsa::Accumulator;

use std::sync::Arc;

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
    if let Some((program, _)) = super::items::composed_file(db, file) {
        return HashEq::share(program);
    }
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
    let locals_arc = super::symbols::local_symbols(db, file);

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
            &super::typing::empty_program(),
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

pub(super) fn signatures_with_imports(
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
        super::database_target_info(db),
    );

    checker.type_info.target_identity = db.target_config().identity(db).clone();
    checker.type_info.target_layout = *db.target_config().data_layout(db);

    // Merge imported type info (path rewrite shared with resolve).
    // Each `declaration_signatures` is Salsa-memoized; merge_from is the cold cost.
    let mut merged_files = rustc_hash::FxHashSet::default();
    let mut merge_imported_module = |imported_file: SourceFile, path: &str| {
        if !merged_files.insert(imported_file)
            || super::symbols::exported_symbols(db, imported_file).is_cycle
        {
            return;
        }
        let imported_sigs = if headers_only {
            super::signatures::seed_header_signatures(db, imported_file)
        } else {
            super::signatures::header_signatures(db, imported_file)
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
        // Import aliases carry names/IDs, not the original declaration identity.
        // Retain canonical SIMD/interior-mutability and intrinsic metadata by
        // SymbolId, including re-exports. Path suffixes and imported spellings
        // cannot promote a user nominal type to a compiler-owned type.
        for symbol in imported_sigs
            .symbols
            .iter()
            .filter(|symbol| symbol.lang_item.is_some() && symbol.span.end > symbol.span.start)
        {
            let symbols = Arc::make_mut(&mut checker.symbols);
            if symbols.try_get(symbol.id).is_none() {
                symbols.register_imported_symbol(symbol.clone());
            } else if let Some(marker) = symbol.lang_item {
                symbols.set_lang_item(symbol.id, marker);
            }
        }
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
                        Arc::make_mut(&mut checker.symbols).register_imported_symbol(symbol);
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
                if let Some(symbol) = imported_sigs.symbols.try_get(destructor_id).cloned() {
                    Arc::make_mut(&mut checker.symbols).register_imported_symbol(symbol);
                }
            }
        }
        for ((type_id, member_name), member_sym) in &imported_sigs.symbols.associated_members {
            if let Some(symbol) = imported_sigs.symbols.try_get(*member_sym) {
                let is_accessible = match symbol.visibility {
                    arandu_parser::Visibility::Public => true,
                    arandu_parser::Visibility::Internal => db
                        .as_source_db()
                        .same_package(*file.file_id(db), imported_file),
                    arandu_parser::Visibility::Private | arandu_parser::Visibility::Module => false,
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
                    Arc::make_mut(&mut checker.symbols).register_imported_symbol(symbol.clone());
                }
            }
        }

        for diag in &imported_sigs.diagnostics {
            if diag.message.contains("cyclic") {
                checker.diagnostics.push(diag.clone());
            }
        }
    };

    for import in program.imports.iter().filter(|_| include_imports) {
        if let Some(path) = arandu_resolve::canonicalize_import_path(import) {
            if let Some(imported_file) = db.as_source_db().resolve_module_path(&path) {
                merge_imported_module(imported_file, &path);
            }
        }
    }

    let current_mod = program.module.as_ref().map(|m| m.path.join("."));
    if include_imports
        && !current_mod
            .as_deref()
            .is_some_and(|m| m.starts_with("std.core."))
    {
        for &(path, mod_name) in arandu_resolve::AMBIENT_PRIMITIVE_MODULES {
            if current_mod.as_deref() == Some(mod_name) {
                continue;
            }
            if let Some(imported_file) = db.as_source_db().resolve_module_path(path) {
                if imported_file != file {
                    merge_imported_module(imported_file, path);
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
pub(super) fn merge_imported_borrow_interfaces(
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
        if super::symbols::exported_symbols(db, imported_file).is_cycle {
            continue;
        }
        let signatures = super::signatures::declaration_signatures(db, imported_file);
        if signatures
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("cyclic"))
        {
            continue;
        }
        let interfaces = super::lower::borrow_interfaces(db, imported_file);
        for (symbol, summary) in &interfaces.entries {
            let checked = Arc::make_mut(result);
            Arc::make_mut(&mut checked.type_info)
                .return_borrow_summaries
                .insert(*symbol, summary.clone());
        }
    }
}
