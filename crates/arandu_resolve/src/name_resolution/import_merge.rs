use super::Resolver;

pub(super) fn merge_import_exports(
    db: &dyn crate::ModuleLoader,
    resolver: &mut Resolver<'_>,
    import: &arandu_parser::ImportDecl,
    global: crate::ScopeId,
) {
    // Merge exports from DB (single path helper — RC-PATH-TRIPLE).
    let module_path = crate::canonicalize_import_path(import);

    if let Some(path) = &module_path {
        if let Some(imported_file) = db.resolve_module_path(path) {
            let exports = db.exported_symbols(imported_file);
            let internal = db
                .same_package(resolver.symbols.file_id, imported_file)
                .then(|| db.internal_symbols(imported_file));
            resolver
                .symbols
                .interface_implementations
                .extend(exports.sealed_implementations.iter().copied());
            if let Some(table) = &internal {
                resolver
                    .symbols
                    .interface_implementations
                    .extend(table.sealed_implementations.iter().copied());
            }
            if matches!(import, arandu_parser::ImportDecl::ReExport { .. })
                && (exports.is_cycle
                    || internal.as_ref().is_some_and(|table| table.is_cycle)
                    || db.source_file_by_id(resolver.symbols.file_id) == Some(imported_file))
            {
                resolver.diagnostics.push(
                    arandu_middle::Diagnostic::error(
                        arandu_middle::DiagCode::N019CyclicReExport,
                        "cyclic re-export detected in module dependencies",
                        import.span(),
                    )
                    .with_hint("break the cycle by re-exporting from one direction only"),
                );
                return;
            }
            match import {
                arandu_parser::ImportDecl::ModuleAlias { alias, .. }
                | arandu_parser::ImportDecl::ExternalAlias { alias, .. } => {
                    let module_name = alias.clone();
                    // Pre-build a name→SymbolId index of types in this module's
                    // exports so that when we encounter an AssociatedFunc like
                    // "Widget.ok", we can resolve "Widget"'s SymbolId even before
                    // it appears in the global scope of the importing file.
                    let exported_types: rustc_hash::FxHashMap<&str, arandu_middle::SymbolId> =
                        exports
                            .symbols
                            .iter()
                            .chain(
                                internal
                                    .iter()
                                    .flat_map(|table| table.internal_symbols.iter()),
                            )
                            .filter(|&(_, &(_, k))| {
                                matches!(
                                    k,
                                    arandu_middle::SymbolKind::Struct
                                        | arandu_middle::SymbolKind::Enum
                                        | arandu_middle::SymbolKind::TypeAlias
                                )
                            })
                            .map(|(n, &(id, _))| (n.as_str(), id))
                            .collect();
                    for (name, &(id, kind)) in exports.symbols.iter().chain(
                        internal
                            .iter()
                            .flat_map(|table| table.internal_symbols.iter()),
                    ) {
                        if exports.sealed_symbols.contains(name)
                            || internal
                                .as_ref()
                                .is_some_and(|table| table.sealed_symbols.contains(name))
                        {
                            resolver.symbols.sealed_interfaces.insert(id);
                        }
                        let lang_item = exports.lang_items.get(&id).copied().or_else(|| {
                            internal
                                .as_ref()
                                .and_then(|table| table.lang_items.get(&id).copied())
                        });
                        let sym = arandu_middle::Symbol {
                            id,
                            name: name.clone().into(),
                            kind,
                            span: import.span(),
                            scope: global,
                            visibility: arandu_parser::Visibility::Public,
                            lang_item,
                        };
                        resolver.symbols.register_imported_symbol(sym);
                        resolver
                            .symbols
                            .module_members
                            .insert((module_name.clone(), name.clone().into()), id);
                        resolver
                            .symbols
                            .module_members
                            .insert((path.clone().into(), name.clone().into()), id);
                        // Root of T025 across modules: associated methods are
                        // exported as `"Type.method"` but interface satisfaction
                        // looks up `associated_members[TypeId][method]`. Rebuild
                        // that index on import.
                        if matches!(kind, arandu_middle::SymbolKind::AssociatedFunc)
                            && let Some((ty, method)) = name.rsplit_once('.')
                        {
                            // Register on the **local** type first when present
                            // (builtin `Result`/`Option` use ArType::Result and
                            // typeck looks up methods via the importing file's
                            // `Result` symbol). Also register on the exported
                            // type id for cross-module `Named` receivers
                            // (`Widget.method` where Widget is only in the
                            // exporting module).
                            let local_ty = resolver.symbols.lookup_type(global, ty);
                            let exported_ty = exported_types.get(ty).copied();
                            let mut linked = false;
                            for type_sym in [local_ty, exported_ty].into_iter().flatten() {
                                resolver
                                    .symbols
                                    .associated_members
                                    .insert((type_sym, smol_str::SmolStr::new(method)), id);
                                linked = true;
                            }
                            if linked {
                                // Import is "used" when it supplies methods for
                                // builtin types (`Result.expectOrAbort`) even if
                                // the alias name never appears in source.
                                if let Some(alias_sym) =
                                    resolver.symbols.lookup_module(global, alias.as_str())
                                {
                                    resolver.used_symbols.insert(alias_sym);
                                }
                            }
                        }
                    }
                }
                arandu_parser::ImportDecl::Named { items, .. }
                | arandu_parser::ImportDecl::ReExport { items, .. }
                | arandu_parser::ImportDecl::ExternalNamed { items, .. } => {
                    for item in items {
                        if let Some(&(id, kind)) =
                            exports.symbols.get(item.name.as_str()).or_else(|| {
                                internal.as_ref().and_then(|table| {
                                    table.internal_symbols.get(item.name.as_str())
                                })
                            })
                        {
                            if exports.sealed_symbols.contains(item.name.as_str())
                                || internal.as_ref().is_some_and(|table| {
                                    table.sealed_symbols.contains(item.name.as_str())
                                })
                            {
                                resolver.symbols.sealed_interfaces.insert(id);
                            }
                            if let arandu_parser::ImportDecl::ReExport { visibility, .. } = import
                                && *visibility == arandu_parser::Visibility::Public
                                && !exports.symbols.contains_key(item.name.as_str())
                                && internal.as_ref().is_some_and(|table| {
                                    table.internal_symbols.contains_key(item.name.as_str())
                                })
                            {
                                resolver.diagnostics.push(
                                    arandu_middle::Diagnostic::error(
                                        arandu_middle::DiagCode::N018ReExportNarrowing,
                                        format!(
                                            "cannot re-export internal symbol '{}' with public visibility",
                                            item.name
                                        ),
                                        item.span,
                                    )
                                    .with_hint(
                                        "reduce the re-export visibility or make the original declaration public",
                                    ),
                                );
                            }
                            let sym_visibility = match import {
                                arandu_parser::ImportDecl::ReExport { visibility, .. } => {
                                    *visibility
                                }
                                _ => arandu_parser::Visibility::Public,
                            };
                            let import_name = item.alias.as_ref().unwrap_or(&item.name).clone();
                            let lang_item = exports.lang_items.get(&id).copied().or_else(|| {
                                internal
                                    .as_ref()
                                    .and_then(|table| table.lang_items.get(&id).copied())
                            });
                            let sym = arandu_middle::Symbol {
                                id,
                                name: import_name.clone(),
                                kind,
                                span: item.span,
                                scope: global,
                                visibility: sym_visibility,
                                lang_item,
                            };
                            match resolver.symbols.insert_imported(sym) {
                                Ok(Some(placeholder_id)) => {
                                    if let Some(entry) =
                                        resolver.imported_symbols.remove(&placeholder_id)
                                    {
                                        resolver.imported_symbols.insert(id, entry);
                                    }
                                }
                                Ok(None) => {}
                                Err(existing) => {
                                    let existing_span = resolver.symbols.get(existing).span;
                                    resolver.diagnostics.push(
                                        arandu_middle::Diagnostic::error(
                                            arandu_middle::DiagCode::N006ImportConflict,
                                            format!(
                                                "import `{}` conflicts with an existing declaration",
                                                import_name
                                            ),
                                            item.span,
                                        )
                                        .with_label(existing_span, "already defined here"),
                                    );
                                }
                            }
                            // Named import of `Result.expectOrAbort` must also
                            // populate `associated_members` on the local builtin.
                            if matches!(kind, arandu_middle::SymbolKind::AssociatedFunc)
                                && let Some((ty, method)) = item.name.rsplit_once('.')
                            {
                                let local_ty = resolver.symbols.lookup_type(global, ty);
                                let exported_ty = exports
                                    .symbols
                                    .get(ty)
                                    .filter(|&&(_, k)| {
                                        matches!(
                                            k,
                                            arandu_middle::SymbolKind::Struct
                                                | arandu_middle::SymbolKind::Enum
                                                | arandu_middle::SymbolKind::TypeAlias
                                        )
                                    })
                                    .map(|&(sid, _)| sid);
                                for type_sym in [local_ty, exported_ty].into_iter().flatten() {
                                    resolver
                                        .symbols
                                        .associated_members
                                        .insert((type_sym, smol_str::SmolStr::new(method)), id);
                                }
                            } else if matches!(
                                kind,
                                arandu_middle::SymbolKind::Struct
                                    | arandu_middle::SymbolKind::Enum
                                    | arandu_middle::SymbolKind::TypeAlias
                            ) {
                                let prefix = format!("{}.", item.name);
                                for (assoc_name, &(assoc_id, assoc_kind)) in
                                    exports.symbols.iter().chain(
                                        internal
                                            .iter()
                                            .flat_map(|table| table.internal_symbols.iter()),
                                    )
                                {
                                    if matches!(
                                        assoc_kind,
                                        arandu_middle::SymbolKind::AssociatedFunc
                                    ) && let Some(method) = assoc_name.strip_prefix(&prefix)
                                    {
                                        let exported_assoc_name = format!("{import_name}.{method}");
                                        let lang_item =
                                            exports.lang_items.get(&assoc_id).copied().or_else(
                                                || {
                                                    internal.as_ref().and_then(|table| {
                                                        table.lang_items.get(&assoc_id).copied()
                                                    })
                                                },
                                            );
                                        let assoc_sym = arandu_middle::Symbol {
                                            id: assoc_id,
                                            name: exported_assoc_name.into(),
                                            kind: assoc_kind,
                                            span: item.span,
                                            scope: global,
                                            visibility: sym_visibility,
                                            lang_item,
                                        };
                                        resolver.symbols.register_imported_symbol(assoc_sym);
                                        resolver
                                            .symbols
                                            .associated_members
                                            .insert((id, smol_str::SmolStr::new(method)), assoc_id);
                                    }
                                }
                            }
                        } else if !db.same_package(resolver.symbols.file_id, imported_file)
                            && db
                                .internal_symbols(imported_file)
                                .internal_symbols
                                .contains_key(item.name.as_str())
                        {
                            resolver.diagnostics.push(arandu_middle::Diagnostic::error(
                                arandu_middle::DiagCode::N016InternalOutsidePackage,
                                format!(
                                    "'{}' is internal and cannot be imported from outside its package",
                                    item.name
                                ),
                                item.span,
                            ));
                        } else {
                            // Missing or private: not in the export table.
                            let mut diag = arandu_middle::Diagnostic::error(
                                arandu_middle::DiagCode::M001UnresolvedImport,
                                format!(
                                    "cannot import `{}`: not found or not public in module",
                                    item.name
                                ),
                                item.span,
                            )
                            .with_primary_label(format!(
                                "`{}` is unavailable from this module",
                                item.name
                            ));
                            let mut candidates: Vec<&str> =
                                exports.symbols.keys().map(String::as_str).collect();
                            candidates.sort_unstable();
                            let name_str = item.name.as_str();
                            let max_distance = if name_str.len() <= 4 { 2 } else { 3 };
                            let best_match = candidates
                                .into_iter()
                                .map(|cand| {
                                    let dist = if cand.to_lowercase() == name_str.to_lowercase() {
                                        0
                                    } else {
                                        strsim::levenshtein(name_str, cand)
                                    };
                                    (cand, dist)
                                })
                                .filter(|(_, dist)| *dist <= max_distance)
                                .min_by_key(|(_, dist)| *dist)
                                .map(|(cand, _)| cand);
                            if let Some(suggestion) = best_match {
                                diag = diag.with_hint(format!("did you mean '{suggestion}'?"));
                            }
                            resolver.diagnostics.push(diag);
                        }
                    }
                }
            }
        } else if db.missing_import_is_error() {
            if let arandu_parser::ImportDecl::ModuleAlias { alias, .. }
            | arandu_parser::ImportDecl::ExternalAlias { alias, .. } = import
            {
                resolver.failed_import_aliases.insert(alias.clone());
            }
            let import_name = match import {
                arandu_parser::ImportDecl::ModuleAlias { path, .. }
                | arandu_parser::ImportDecl::Named { path, .. }
                | arandu_parser::ImportDecl::ReExport { path, .. } => path.join("."),
                arandu_parser::ImportDecl::ExternalAlias { source, .. }
                | arandu_parser::ImportDecl::ExternalNamed { source, .. } => source.to_string(),
            };
            resolver.diagnostics.push(
                arandu_middle::Diagnostic::error(
                    arandu_middle::DiagCode::M001UnresolvedImport,
                    format!("unresolved import: `{}`", import_name),
                    import.span(),
                )
                .with_primary_label("module could not be found"),
            );
        }
    } else if db.missing_import_is_error() {
        if let arandu_parser::ImportDecl::ModuleAlias { alias, .. }
        | arandu_parser::ImportDecl::ExternalAlias { alias, .. } = import
        {
            resolver.failed_import_aliases.insert(alias.clone());
        }
        let import_name = match import {
            arandu_parser::ImportDecl::ModuleAlias { path, .. }
            | arandu_parser::ImportDecl::Named { path, .. }
            | arandu_parser::ImportDecl::ReExport { path, .. } => path.join("."),
            arandu_parser::ImportDecl::ExternalAlias { source, .. }
            | arandu_parser::ImportDecl::ExternalNamed { source, .. } => source.to_string(),
        };
        resolver.diagnostics.push(
            arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::M001UnresolvedImport,
                format!("unresolved import: `{}`", import_name),
                import.span(),
            )
            .with_primary_label("module could not be found"),
        );
    }
}
