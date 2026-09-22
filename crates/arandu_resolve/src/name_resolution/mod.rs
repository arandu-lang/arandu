use arandu_parser::{FuncName, Program, TopLevelDecl};
use smol_str::SmolStr;

use crate::{ResolutionResult, ResolvedNames, SymbolKind, SymbolTable};

mod collect;
mod decls;
mod expr;
mod program;
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
        _ => return None,
    };
    (path == module || std::path::Path::new(path).ends_with(file)).then_some(item)
}

/// Builtin prelude modules injected by `define_prelude` / this helper.
/// Kept in one place so Salsa import resolution can short-circuit without
/// requiring on-disk `io.aru` / `err.aru` files.
pub const PRELUDE_MODULES: &[&str] = &["io", "err"];

/// Members registered for each prelude module (must stay in sync with
/// [`super::program::Resolver::define_prelude`]).
const PRELUDE_MODULE_MEMBERS: &[(&str, &[&str])] =
    &[("io", &["println", "create", "remove"]), ("err", &["new"])];

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
    // The resolver mutates the seed tables. `unwrap_or_clone` keeps the
    // single-owner case zero-copy (fresh `resolve_local` seed) and copies
    // exactly once when the seed is still shared with a memoized query.
    let mut resolver = Resolver {
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
            | arandu_parser::ImportDecl::Named { path, .. } => prelude_module_from_path(path),
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
                    arandu_parser::ImportDecl::Named { items, .. } => {
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
                                    is_public: true,
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
                        for (name, &(id, kind)) in &exports.symbols {
                            let item_lang = core_lang_item(path, name);
                            let sym = arandu_middle::Symbol {
                                id,
                                name: name.clone().into(),
                                kind,
                                span: import.span(),
                                scope: global,
                                is_public: true, // only public symbols appear in exports
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
                    | arandu_parser::ImportDecl::ExternalNamed { items, .. } => {
                        for item in items {
                            if let Some(&(id, kind)) = exports.symbols.get(item.name.as_str()) {
                                let import_name = item.alias.as_ref().unwrap_or(&item.name).clone();
                                let item_lang = core_lang_item(path, &item.name);
                                let sym = arandu_middle::Symbol {
                                    id,
                                    name: import_name.clone(),
                                    kind,
                                    span: item.span,
                                    scope: global,
                                    is_public: true, // only public symbols appear in exports
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
                                }
                            } else {
                                // Missing or private: not in the export table.
                                let mut diag = arandu_middle::Diagnostic::error(
                                    arandu_middle::DiagCode::M001UnresolvedImport,
                                    format!(
                                        "cannot import `{}`: not found or not public in module",
                                        item.name
                                    ),
                                    item.span,
                                );
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
                    | arandu_parser::ImportDecl::Named { path, .. } => path.join("."),
                    arandu_parser::ImportDecl::ExternalAlias { source, .. }
                    | arandu_parser::ImportDecl::ExternalNamed { source, .. } => source.to_string(),
                };
                resolver.diagnostics.push(arandu_middle::Diagnostic::error(
                    arandu_middle::DiagCode::M001UnresolvedImport,
                    format!("unresolved import: `{}`", import_name),
                    import.span(),
                ));
            }
        } else if db.missing_import_is_error() {
            if let arandu_parser::ImportDecl::ModuleAlias { alias, .. }
            | arandu_parser::ImportDecl::ExternalAlias { alias, .. } = import
            {
                resolver.failed_import_aliases.insert(alias.clone());
            }
            let import_name = match import {
                arandu_parser::ImportDecl::ModuleAlias { path, .. }
                | arandu_parser::ImportDecl::Named { path, .. } => path.join("."),
                arandu_parser::ImportDecl::ExternalAlias { source, .. }
                | arandu_parser::ImportDecl::ExternalNamed { source, .. } => source.to_string(),
            };
            resolver.diagnostics.push(arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::M001UnresolvedImport,
                format!("unresolved import: `{}`", import_name),
                import.span(),
            ));
        }
    }

    poll();
    resolver.resolve_method_receivers(program);

    for decl_id in &program.decls {
        poll();
        let decl = resolver.pool.decl(*decl_id);
        resolver.resolve_top_level(global, decl);
    }

    resolver.check_unused_imports();

    resolver.symbols.unresolved_module_aliases =
        resolver.failed_import_aliases.into_iter().collect();

    ResolutionResult {
        is_cycle_fallback: false,
        symbols: std::sync::Arc::new(resolver.symbols),
        resolved: std::sync::Arc::new(resolver.resolved),
        docs: resolver.docs,
        diagnostics: resolver.diagnostics,
    }
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
