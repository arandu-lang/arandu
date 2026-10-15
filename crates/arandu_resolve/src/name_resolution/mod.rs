use arandu_parser::{FuncName, Program, TopLevelDecl};
use smol_str::SmolStr;

use crate::{ResolutionResult, ResolvedNames, SymbolKind, SymbolTable};

mod collect;
mod decls;
mod expr;
mod program;
mod staging;
mod stmt;
mod symbols;
mod types;
mod util;

/// Decode canonical core identities at the import boundary; semantic consumers
/// use LangItem/SymbolId, never the imported alias or a bare type spelling.
fn core_lang_item(path: &str, name: &str) -> Option<arandu_middle::symbol_table::LangItem> {
    use arandu_middle::symbol_table::LangItem;
    let (module, file, item) = match name {
        "Poll" => ("std.core.future", "core/future.aru", LangItem::Poll),
        "Result" => ("std.core.result", "core/result.aru", LangItem::Result),
        "Option" => ("std.core.option", "core/option.aru", LangItem::Option),
        "Coroutine" => (
            "std.core.coroutine",
            "core/coroutine.aru",
            LangItem::Coroutine,
        ),
        "Copy" => ("std.core.marker", "core/marker.aru", LangItem::Copy),
        "Send" => ("std.core.marker", "core/marker.aru", LangItem::Send),
        "Sync" => ("std.core.marker", "core/marker.aru", LangItem::Sync),
        "String" => ("std.alloc.string", "alloc/string.aru", LangItem::String),
        "Vec" => ("std.alloc.vec", "alloc/vec.aru", LangItem::Vec),
        "TaskHandle" => (
            "std.runtime.executor",
            "std/runtime/executor.aru",
            LangItem::TaskHandle,
        ),
        "alloc" => ("std.core.mem", "core/mem.aru", LangItem::Alloc),
        "free" => ("std.core.mem", "core/mem.aru", LangItem::Free),
        _ => return None,
    };
    (path == module
        || std::path::Path::new(path).ends_with(file)
        || path == "std.core.prelude"
        || std::path::Path::new(path).ends_with("core/prelude.aru"))
    .then_some(item)
}

/// Builtin prelude modules injected by `define_prelude` / this helper.
/// Kept in one place so Salsa import resolution can short-circuit without
/// requiring on-disk `io.aru` / `err.aru` files.
pub const PRELUDE_MODULES: &[&str] = &["io", "err"];

/// Members registered for each prelude module (must stay in sync with
/// [`super::program::Resolver::define_prelude`]).
const PRELUDE_MODULE_MEMBERS: &[(&str, &[&str])] = &[
    ("io", &["println", "print", "create", "remove", "eprint"]),
    ("err", &["new"]),
];

/// Returns the prelude module name if `path` is a single-segment prelude path.
#[must_use]
pub fn prelude_module_from_path(path: &[SmolStr]) -> Option<&'static str> {
    if path.len() != 1 {
        return None;
    }
    let name = path[0].as_str();
    PRELUDE_MODULES.iter().copied().find(|&m| m == name)
}

#[must_use]
pub fn resolve_local(file_id: u32, program: &Program) -> ResolutionResult {
    Resolver::new(file_id, &program.pool, Some(program)).resolve_local(program)
}

#[must_use]
pub fn resolve_local_with_poll(
    file_id: u32,
    program: &Program,
    poll: impl FnMut(),
) -> ResolutionResult {
    Resolver::new(file_id, &program.pool, Some(program)).resolve_local_with_poll(program, poll)
}

/// Single-file / unit-test resolve that runs the **same** import pipeline as
/// production, with an empty module loader (no multi-file loads).
///
/// Prefer this over hand-rolled import collection so prelude short-circuit and
/// `canonicalize_import_path` stay shared with the CLI (RC-DUAL-RESOLVE).
#[must_use]
pub fn resolve_for_test(file_id: u32, program: &Program) -> ResolutionResult {
    let local = resolve_local(file_id, program);
    resolve_imports_and_bodies(&crate::EmptyModuleLoader, program, local)
}

#[must_use]
pub fn resolve_imports_and_bodies(
    db: &dyn crate::ModuleLoader,
    program: &Program,
    result: ResolutionResult,
) -> ResolutionResult {
    resolve_imports_and_bodies_with_poll(db, program, result, || {})
}

#[must_use]
pub fn resolve_imports_and_bodies_with_poll(
    db: &dyn crate::ModuleLoader,
    program: &Program,
    result: ResolutionResult,
    mut poll: impl FnMut(),
) -> ResolutionResult {
    let headers = resolve_headers_with_poll(db, program, result, &mut poll);
    resolve_bodies_with_poll(program, headers, poll)
}

/// Declaration/import state that can be consumed without resolving function
/// bodies. Continuation maps refer only to this AST revision and are retained
/// so finishing resolution does not reallocate headers or redo imports.
#[derive(Debug, Clone)]
pub struct HeaderResolution {
    pub declarations: ResolutionResult,
    pub body_scopes: rustc_hash::FxHashMap<crate::NodeKey, crate::ScopeId>,
    pub import_aliases: rustc_hash::FxHashMap<SmolStr, SmolStr>,
    pub current_module: Option<String>,
    pub imported_symbols: rustc_hash::FxHashMap<crate::SymbolId, (SmolStr, arandu_lexer::Span)>,
    pub used_symbols: rustc_hash::FxHashSet<crate::SymbolId>,
}

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
        if db.package_mode() {
            match crate::logical_import(import) {
                Some(crate::LogicalImport::LegacyExternal { source }) => {
                    resolver.diagnostics.push(
                        crate::Diagnostic::error(
                            arandu_middle::DiagCode::M005FilesystemImportForbidden,
                            format!(
                                "quoted filesystem import `{source}` is forbidden in package mode"
                            ),
                            import.span(),
                        )
                        .with_hint(
                            "declare a dependency in arandu.toml and import it through its alias",
                        ),
                    );
                    continue;
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
                            .with_hint_replacement(
                                arandu_middle::Hint {
                                    message: "use the explicit `self` import root".into(),
                                    replacement: Some(arandu_middle::CodeReplacement {
                                        span: import.span(),
                                        new_text: replacement,
                                    }),
                                },
                            ),
                        );
                    }
                }
                _ => {}
            }
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
                                    lang_item: None,
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
                continue;
            }
        }

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
                    continue;
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
                            let item_lang = core_lang_item(path, name);
                            let sym = arandu_middle::Symbol {
                                id,
                                name: name.clone().into(),
                                kind,
                                span: import.span(),
                                scope: global,
                                visibility: arandu_parser::Visibility::Public,
                                lang_item: item_lang,
                            };
                            resolver.symbols.register_imported_symbol(sym);
                            if let Some(lang) = item_lang {
                                resolver.symbols.set_lang_item(id, lang);
                            }
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
                                if let arandu_parser::ImportDecl::ReExport { visibility, .. } =
                                    import
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
                                let item_lang = core_lang_item(path, &item.name);
                                let sym = arandu_middle::Symbol {
                                    id,
                                    name: import_name.clone(),
                                    kind,
                                    span: item.span,
                                    scope: global,
                                    visibility: sym_visibility,
                                    lang_item: item_lang,
                                };
                                if let Some(lang) = item_lang {
                                    resolver.symbols.set_lang_item(id, lang);
                                }
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
                                            let exported_assoc_name =
                                                format!("{import_name}.{method}");
                                            let assoc_lang = core_lang_item(path, assoc_name);
                                            let assoc_sym = arandu_middle::Symbol {
                                                id: assoc_id,
                                                name: exported_assoc_name.into(),
                                                kind: assoc_kind,
                                                span: item.span,
                                                scope: global,
                                                visibility: sym_visibility,
                                                lang_item: assoc_lang,
                                            };
                                            resolver.symbols.register_imported_symbol(assoc_sym);
                                            if let Some(lang) = assoc_lang {
                                                resolver.symbols.set_lang_item(assoc_id, lang);
                                            }
                                            resolver.symbols.associated_members.insert(
                                                (id, smol_str::SmolStr::new(method)),
                                                assoc_id,
                                            );
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
                                        let dist = if cand.to_lowercase() == name_str.to_lowercase()
                                        {
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

#[must_use]
pub fn resolve_bodies_with_poll(
    program: &Program,
    headers: HeaderResolution,
    poll: impl FnMut(),
) -> ResolutionResult {
    resolve_selected_body_with_poll(program, headers, BodySelection::All, poll)
}

/// A revision-local selection borrowed from the same AST as the header state.
/// Expression staging sees declaration/parameter scope, not surrounding runtime
/// locals. The orchestration layer must select parents before nested conditions.
#[derive(Debug, Clone, Copy)]
pub enum BodySelection {
    All,
    Function(crate::SymbolId),
    Expression {
        owner: crate::SymbolId,
        expression: arandu_parser::ast_pool::ExprId,
    },
    /// Establish the lexical scope of an enclosing ordinary statement without
    /// resolving its runtime expressions. Shared by conditions and arguments.
    LexicalExpression {
        owner: crate::SymbolId,
        expression: arandu_parser::ast_pool::ExprId,
        statement: arandu_middle::Span,
    },
    LexicalBlock {
        owner: crate::SymbolId,
        block: arandu_parser::ast_pool::BlockId,
        statement: arandu_middle::Span,
    },
    LexicalLoopBody {
        owner: crate::SymbolId,
        statement: arandu_middle::Span,
    },
}

/// Continue the canonical resolver for one body/root without visiting siblings.
/// Unused-import warnings are meaningful only for complete resolution.
#[must_use]
pub fn resolve_selected_body_with_poll(
    program: &Program,
    headers: HeaderResolution,
    selection: BodySelection,
    mut poll: impl FnMut(),
) -> ResolutionResult {
    let HeaderResolution {
        declarations,
        body_scopes,
        import_aliases,
        current_module,
        imported_symbols,
        used_symbols,
    } = headers;
    let failed_import_aliases = declarations
        .symbols
        .unresolved_module_aliases
        .iter()
        .cloned()
        .collect();
    let mut resolver = Resolver {
        reusable_definitions: declarations.resolved.definitions.keys().copied().collect(),
        symbols: std::sync::Arc::unwrap_or_clone(declarations.symbols),
        resolved: std::sync::Arc::unwrap_or_clone(declarations.resolved),
        docs: declarations.docs,
        diagnostics: declarations.diagnostics,
        pool: &program.pool,
        import_aliases,
        failed_import_aliases,
        current_module,
        imported_symbols,
        used_symbols,
    };
    program.for_each_decl_recursive(|_, declaration| {
        poll();
        if let TopLevelDecl::Func(function) = declaration {
            let key = match &function.name {
                FuncName::Free { span, .. } | FuncName::Method { span, .. } => (*span).into(),
            };
            let symbol = resolver.resolved.definitions.get(&key).copied();
            let selected = match selection {
                BodySelection::All => true,
                BodySelection::Function(owner) | BodySelection::Expression { owner, .. } | BodySelection::LexicalExpression { owner, .. } | BodySelection::LexicalBlock { owner, .. } | BodySelection::LexicalLoopBody { owner, .. } => {
                    symbol == Some(owner)
                }
            };
            if !selected {
                return;
            }
            if symbol.is_some_and(|symbol| resolver.resolved.deferred_comptime_functions.contains(&symbol))
                && matches!(selection, BodySelection::All)
            {
                return;
            }
            if let Some(&scope) = body_scopes.get(&key) {
                match selection {
                    BodySelection::All | BodySelection::Function(_) => {
                        resolver.resolve_block_in_scope(scope, &program.pool, &function.body);
                    }
                    BodySelection::Expression { expression, .. } => {
                        resolver.resolve_expr(scope, expression);
                    }
                    BodySelection::LexicalExpression { expression, statement, .. } => {
                        if let Some(scope) = resolver.staging_context(scope, &program.pool, &function.body, statement, 0) {
                            resolver.resolve_expr(scope, expression);
                        } else {
                            resolver.diagnostics.push(crate::Diagnostic::error(
                                crate::DiagCode::T042UnsupportedComptime,
                                "compile-time staging could not establish this expression's lexical scope",
                                statement,
                            ));
                        }
                    }
                    BodySelection::LexicalBlock { block, statement, .. } => {
                        if let Some(scope) = resolver.staging_context(scope, &program.pool, &function.body, statement, 0) {
                            resolver.resolve_block_in_scope(scope, &program.pool, program.pool.block(block));
                        }
                    }
                    BodySelection::LexicalLoopBody { statement, .. } => {
                        if let Some(arandu_parser::Stmt::For { body, .. }) = program.pool.stmts.iter().find(|stmt| stmt.span() == statement)
                            && let Some(scope) = resolver.staging_context(scope, &program.pool, &function.body, body.span, 0) {
                            resolver.resolve_block_in_scope(scope, &program.pool, body);
                        }
                    }
                }
            } else {
                resolver.diagnostics.push(crate::Diagnostic::ice(
                    crate::DiagCode::ICET001,
                    "function body has no declaration resolution scope",
                    function.span,
                ));
            }
        }
    });
    if matches!(selection, BodySelection::All) {
        resolver.check_unused_imports();
    }
    ResolutionResult {
        is_cycle_fallback: declarations.is_cycle_fallback,
        symbols: std::sync::Arc::new(resolver.symbols),
        resolved: std::sync::Arc::new(resolver.resolved),
        docs: resolver.docs,
        diagnostics: resolver.diagnostics,
    }
}

fn lookup_type_name(
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

#[must_use]
#[tracing::instrument(level = "trace", target = "arandu_resolve", skip(program))]
pub fn collect_symbols(
    program: &Program,
) -> (
    SymbolTable,
    ResolvedNames,
    crate::DocCommentMap,
    Vec<crate::Diagnostic>,
) {
    let mut resolver = Resolver {
        reusable_definitions: rustc_hash::FxHashSet::default(),
        symbols: SymbolTable::new(0),
        resolved: ResolvedNames::default(),
        docs: crate::DocCommentMap::default(),
        diagnostics: Vec::new(),
        pool: &program.pool,
        import_aliases: rustc_hash::FxHashMap::default(),
        failed_import_aliases: rustc_hash::FxHashSet::default(),
        current_module: program.module.as_ref().map(|m| m.path.join(".")),
        imported_symbols: rustc_hash::FxHashMap::default(),
        used_symbols: rustc_hash::FxHashSet::default(),
    };

    for doc in &program.docs {
        resolver
            .docs
            .entry(crate::NodeKey::from(doc.target_span))
            .or_default()
            .push(doc.text.to_string());
    }

    let global = resolver.symbols.global_scope();
    if let Some(module) = &program.module
        && let Some(root) = module.path.first()
    {
        resolver.define(global, root, SymbolKind::Module, module.span);
    }

    for import in &program.imports {
        resolver.collect_import(global, import);
    }

    for decl_id in &program.decls {
        let decl = program.pool.decl(*decl_id);
        resolver.collect_top_level(global, decl);
    }

    if let Some(module) = &program.module {
        let module_name = module.path.join(".");
        for decl_id in &program.decls {
            let decl = program.pool.decl(*decl_id);
            match decl {
                TopLevelDecl::Const(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::TypeAlias(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::Func(d) => {
                    if let FuncName::Free { span, name } = &d.name {
                        let _ = resolver
                            .symbols
                            .define_module_member(&module_name, name, *span);
                    }
                }
                TopLevelDecl::Struct(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::Enum(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::Interface(d) => {
                    let _ = resolver
                        .symbols
                        .define_module_member(&module_name, &d.name, d.span);
                }
                TopLevelDecl::Extern(d) => {
                    for member in &d.members {
                        let _ = resolver.symbols.define_module_member(
                            &module_name,
                            &member.name,
                            member.span,
                        );
                    }
                }
                TopLevelDecl::Submodule(_) => {}
                TopLevelDecl::Error(_) => {}
            }
        }
    }

    (
        resolver.symbols,
        resolver.resolved,
        resolver.docs,
        resolver.diagnostics,
    )
}

#[must_use]
pub fn resolve_with_symbols(
    global_symbols: SymbolTable,
    resolved: ResolvedNames,
    docs: crate::DocCommentMap,
    diagnostics: Vec<crate::Diagnostic>,
    program: &Program,
) -> ResolutionResult {
    let mut resolver = Resolver {
        reusable_definitions: rustc_hash::FxHashSet::default(),
        symbols: global_symbols,
        resolved,
        docs,
        diagnostics,
        pool: &program.pool,
        import_aliases: rustc_hash::FxHashMap::default(),
        failed_import_aliases: rustc_hash::FxHashSet::default(),
        current_module: program.module.as_ref().map(|m| m.path.join(".")),
        imported_symbols: rustc_hash::FxHashMap::default(),
        used_symbols: rustc_hash::FxHashSet::default(),
    };

    for import in &program.imports {
        if let arandu_parser::ImportDecl::ExternalAlias { source, alias, .. } = import {
            resolver
                .import_aliases
                .insert(alias.clone(), source.clone());
        }
    }

    let global = resolver.symbols.global_scope();
    for decl_id in &program.decls {
        let decl = program.pool.decl(*decl_id);
        resolver.resolve_top_level(global, decl);
    }

    resolver.check_unused_imports();

    ResolutionResult {
        is_cycle_fallback: false,
        symbols: std::sync::Arc::new(resolver.symbols),
        resolved: std::sync::Arc::new(resolver.resolved),
        docs: resolver.docs,
        diagnostics: resolver.diagnostics,
    }
}

struct Resolver<'a> {
    /// Seed identities may be rebound once by a selected lexical continuation.
    /// Definitions created during this traversal are never eligible for reuse.
    reusable_definitions: rustc_hash::FxHashSet<crate::NodeKey>,
    symbols: SymbolTable,
    resolved: ResolvedNames,
    docs: crate::DocCommentMap,
    diagnostics: Vec<crate::Diagnostic>,
    pool: &'a arandu_parser::ast_pool::AstPool,
    import_aliases: rustc_hash::FxHashMap<SmolStr, SmolStr>,
    failed_import_aliases: rustc_hash::FxHashSet<SmolStr>,
    current_module: Option<String>,
    imported_symbols: rustc_hash::FxHashMap<crate::SymbolId, (SmolStr, arandu_lexer::Span)>,
    used_symbols: rustc_hash::FxHashSet<crate::SymbolId>,
}

impl<'a> Resolver<'a> {
    pub(crate) fn mark_used(&mut self, symbol: crate::SymbolId) {
        self.used_symbols.insert(symbol);
    }

    pub(crate) fn record_expr_ref(
        &mut self,
        expr: arandu_parser::ast_pool::ExprId,
        symbol: crate::SymbolId,
    ) {
        self.resolved.expr_ref(expr, symbol);
        self.mark_used(symbol);
    }

    pub(crate) fn record_value_ref(&mut self, span: arandu_lexer::Span, symbol: crate::SymbolId) {
        self.resolved.value_ref(span, symbol);
        self.mark_used(symbol);
    }

    pub(crate) fn record_type_ref(&mut self, span: arandu_lexer::Span, symbol: crate::SymbolId) {
        self.resolved.type_ref(span, symbol);
        self.mark_used(symbol);
    }

    pub(crate) fn record_import_symbol(
        &mut self,
        symbol: crate::SymbolId,
        name: SmolStr,
        span: arandu_lexer::Span,
    ) {
        self.imported_symbols.insert(symbol, (name, span));
    }

    pub(crate) fn lookup_and_record_module(
        &mut self,
        scope: crate::ScopeId,
        name: &str,
    ) -> Option<crate::SymbolId> {
        let sym = self.symbols.lookup_module(scope, name)?;
        self.mark_used(sym);
        Some(sym)
    }

    pub(crate) fn check_unused_imports(&mut self) {
        for (sym_id, (name, span)) in &self.imported_symbols {
            if !self.used_symbols.contains(sym_id) {
                self.diagnostics.push(crate::Diagnostic::warning(
                    crate::DiagCode::W007UnusedImport,
                    format!("unused import `{name}`"),
                    *span,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests;
