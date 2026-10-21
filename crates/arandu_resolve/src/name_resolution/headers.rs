use super::HeaderResolution;
use super::Resolver;
use super::import_merge::merge_import_exports;
use super::prelude_module_from_path;
use crate::ResolutionResult;
use arandu_parser::Program;
use smol_str::SmolStr;

#[must_use]
pub fn resolve_headers_with_poll(
    db: &dyn crate::ModuleLoader,
    program: &Program,
    result: ResolutionResult,
    mut poll: impl FnMut(),
) -> HeaderResolution {
    // The resolver mutates the seed tables. `unwrap_or_clone` keeps the
    // single-owner case zero-copy (fresh `resolve_local` seed) and copies
    // exactly once when the seed is still shared with a memoized query.
    let mut resolver = Resolver {
        reusable_definitions: rustc_hash::FxHashSet::default(),
        symbols: std::sync::Arc::unwrap_or_clone(result.symbols),
        resolved: std::sync::Arc::unwrap_or_clone(result.resolved),
        docs: result.docs,
        diagnostics: result.diagnostics,
        pool: &program.pool,
        import_aliases: rustc_hash::FxHashMap::default(),
        failed_import_aliases: rustc_hash::FxHashSet::default(),
        current_module: program.module.as_ref().map(|m| m.path.join(".")),
        imported_symbols: rustc_hash::FxHashMap::default(),
        used_symbols: rustc_hash::FxHashSet::default(),
    };

    let global = resolver.symbols.global_scope();

    for import in &program.imports {
        poll();
        if reject_package_filesystem_import(db, &mut resolver, import) {
            continue;
        }
        // Collect aliases only after package policy accepts the import. A rejected
        // filesystem import must not leave a partially usable namespace behind.
        if let arandu_parser::ImportDecl::ExternalAlias { source, alias, .. } = import {
            // SmolStr::clone is O(1)
            resolver
                .import_aliases
                .insert(alias.clone(), source.clone());
        }

        resolver.collect_import(global, import);
        if short_circuit_prelude_import(db, &mut resolver, import, global) {
            continue;
        }
        merge_import_exports(db, &mut resolver, import, global);
    }

    // Ambient linkage for inherent methods of primitive types (str, char, int, float, ptr).
    // Allows methods like `s.len()` or `x.abs()` without requiring an explicit import.
    // Core library modules (`std.core.*`) are excluded to avoid circular dependency cycles.
    if !resolver
        .current_module
        .as_deref()
        .is_some_and(|m| m.starts_with("std.core."))
    {
        for &(path, mod_name) in crate::AMBIENT_PRIMITIVE_MODULES {
            poll();
            if resolver.current_module.as_deref() == Some(mod_name) {
                continue;
            }
            let Some(imported_file) = db.resolve_module_path(path) else {
                continue;
            };
            if db.source_file_by_id(resolver.symbols.file_id) == Some(imported_file) {
                continue;
            }
            let exports = db.exported_symbols(imported_file);
            for (name, &(id, kind)) in &exports.symbols {
                if matches!(kind, arandu_middle::SymbolKind::AssociatedFunc)
                    && let Some((ty, method)) = name.rsplit_once('.')
                    && let Some(type_sym) = resolver.symbols.lookup_type(global, ty)
                {
                    let lang_item = exports.lang_items.get(&id).copied().or_else(|| {
                        let internal = db
                            .same_package(resolver.symbols.file_id, imported_file)
                            .then(|| db.internal_symbols(imported_file));
                        internal
                            .as_ref()
                            .and_then(|table| table.lang_items.get(&id).copied())
                    });
                    let sym = arandu_middle::Symbol {
                        id,
                        name: name.clone().into(),
                        kind,
                        span: arandu_lexer::Span::new(id.file_id, 0, 0),
                        scope: global,
                        visibility: arandu_parser::Visibility::Public,
                        lang_item,
                    };
                    resolver.symbols.register_imported_symbol(sym);
                    resolver
                        .symbols
                        .associated_members
                        .insert((type_sym, smol_str::SmolStr::new(method)), id);
                }
            }
        }
    }

    poll();
    resolver.resolve_method_receivers(program);

    for implementation in &program.interface_impls {
        poll();
        let for_type = lookup_type_name(&resolver.symbols, &implementation.for_type);
        let interface = lookup_type_name(&resolver.symbols, &implementation.interface);
        let (Some(for_type), Some(interface)) = (for_type, interface) else {
            continue;
        };
        if resolver.symbols.sealed_interfaces.contains(&interface)
            && !db.same_package_files(resolver.symbols.file_id, interface.file_id)
        {
            resolver.diagnostics.push(arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::N017SealedImplOutsidePackage,
                format!(
                    "cannot implement sealed interface '{}' outside its package",
                    implementation
                        .interface
                        .path
                        .last()
                        .map_or("<unknown>", |name| name.as_str())
                ),
                implementation.span,
            ));
        } else {
            resolver
                .symbols
                .interface_implementations
                .insert((for_type, interface));
        }
    }

    let mut body_scopes = rustc_hash::FxHashMap::default();
    for decl_id in &program.decls {
        poll();
        let decl = resolver.pool.decl(*decl_id);
        resolver.resolve_top_level_headers(global, decl, &mut body_scopes);
    }

    resolver.symbols.unresolved_module_aliases =
        resolver.failed_import_aliases.into_iter().collect();

    HeaderResolution {
        declarations: ResolutionResult {
            is_cycle_fallback: false,
            symbols: std::sync::Arc::new(resolver.symbols),
            resolved: std::sync::Arc::new(resolver.resolved),
            docs: resolver.docs,
            diagnostics: resolver.diagnostics,
        },
        body_scopes,
        import_aliases: resolver.import_aliases,
        current_module: resolver.current_module,
        imported_symbols: resolver.imported_symbols,
        used_symbols: resolver.used_symbols,
    }
}

fn reject_package_filesystem_import(
    db: &dyn crate::ModuleLoader,
    resolver: &mut Resolver<'_>,
    import: &arandu_parser::ImportDecl,
) -> bool {
    if db.package_mode() {
        match crate::logical_import(import) {
            Some(crate::LogicalImport::LegacyExternal { source }) => {
                resolver.diagnostics.push(
                    crate::Diagnostic::error(
                        arandu_middle::DiagCode::M005FilesystemImportForbidden,
                        format!("quoted filesystem import `{source}` is forbidden in package mode"),
                        import.span(),
                    )
                    .with_hint(
                        "declare a dependency in arandu.toml and import it through its alias",
                    ),
                );
                return true;
            }
            Some(crate::LogicalImport::LegacyLocal { ref module }) => {
                let path = format!("{module}.aru");
                if db.resolve_module_path(&path).is_some()
                    && let Some(replacement) = explicit_self_import(import)
                {
                    resolver.diagnostics.push(
                        crate::Diagnostic::warning(
                            arandu_middle::DiagCode::M004LegacyLocalImport,
                            format!(
                                "implicit local import `{module}` is deprecated in package mode"
                            ),
                            import.span(),
                        )
                        .with_hint_replacement(arandu_middle::Hint {
                            message: "use the explicit `self` import root".into(),
                            replacement: Some(arandu_middle::CodeReplacement {
                                span: import.span(),
                                new_text: replacement,
                            }),
                        }),
                    );
                }
            }
            _ => {}
        }
    }
    false
}

fn short_circuit_prelude_import(
    db: &dyn crate::ModuleLoader,
    resolver: &mut Resolver<'_>,
    import: &arandu_parser::ImportDecl,
    global: crate::ScopeId,
) -> bool {
    // Builtin prelude (`import io`, `import err`, `from io import ...`): members already live in
    // the symbol table from `define_prelude`. Do not require on-disk files.
    // Prefer a real file if one is registered; otherwise short-circuit.
    if let Some(prelude_name) = match import {
        arandu_parser::ImportDecl::ModuleAlias { path, .. }
        | arandu_parser::ImportDecl::Named { path, .. }
        | arandu_parser::ImportDecl::ReExport { path, .. } => prelude_module_from_path(path),
        _ => None,
    } {
        let file_key = format!("{prelude_name}.aru");
        if db.resolve_module_path(&file_key).is_none() {
            // It is indeed a built-in prelude module import!
            match import {
                arandu_parser::ImportDecl::ModuleAlias { alias, .. } => {
                    if alias.as_str() != prelude_name {
                        resolver
                            .import_aliases
                            .insert(alias.clone(), SmolStr::new(prelude_name));
                        let prelude_str = SmolStr::new(prelude_name);
                        let alias_members: Vec<_> = resolver
                            .symbols
                            .module_members
                            .iter()
                            .filter(|((m, _), _)| m == &prelude_str)
                            .map(|((_, member), &id)| (member.clone(), id))
                            .collect();
                        for (member, id) in alias_members {
                            resolver
                                .symbols
                                .module_members
                                .insert((alias.clone(), member), id);
                        }
                    }
                }
                arandu_parser::ImportDecl::Named { items, .. }
                | arandu_parser::ImportDecl::ReExport { items, .. } => {
                    for item in items {
                        let member_name = &item.name;
                        if let Some(&id) = resolver
                            .symbols
                            .module_members
                            .get(&(SmolStr::new(prelude_name), member_name.clone()))
                        {
                            let import_name = item.alias.as_ref().unwrap_or(&item.name).clone();
                            let sym = arandu_middle::Symbol {
                                id,
                                name: import_name.clone(),
                                kind: arandu_middle::SymbolKind::NamespaceMember,
                                span: item.span,
                                scope: global,
                                visibility: arandu_parser::Visibility::Public,
                                lang_item: resolver.symbols.try_get(id).and_then(|s| s.lang_item),
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
                        }
                    }
                }
                _ => {}
            }
            return true;
        }
    }
    false
}

pub(super) fn lookup_type_name(
    symbols: &arandu_middle::SymbolTable,
    name: &arandu_parser::TypeName,
) -> Option<arandu_middle::SymbolId> {
    let global = symbols.global_scope();
    if name.path.len() == 1 {
        return symbols.lookup_type(global, name.path[0].as_str());
    }
    let (member_path, member) = name.path.split_at(name.path.len().saturating_sub(1));
    let namespace = member_path.join(".");
    symbols.lookup_module_member(&namespace, member.first()?.as_str())
}

fn explicit_self_import(import: &arandu_parser::ImportDecl) -> Option<String> {
    match import {
        arandu_parser::ImportDecl::ModuleAlias { path, alias, .. } if path.len() == 1 => {
            Some(format!("import self.{} as {alias}", path[0]))
        }
        arandu_parser::ImportDecl::Named { path, items, .. } if path.len() == 1 => {
            let items = items
                .iter()
                .map(|item| match &item.alias {
                    Some(alias) => format!("{} as {alias}", item.name),
                    None => item.name.to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("from self.{} import {{ {items} }}", path[0]))
        }
        _ => None,
    }
}
