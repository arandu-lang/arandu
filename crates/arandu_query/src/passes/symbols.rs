use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::Diagnostic;
use arandu_resolve::ResolutionResult;

use std::sync::Arc;

#[salsa::tracked]
pub fn local_symbols(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<ResolutionResult> {
    let program_res = super::resolution::parse(db, file);
    let mut resolved = match &**program_res {
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

    if !db
        .as_db_impl()
        .is_some_and(|db| db.is_trusted_stdlib_source(file))
    {
        Arc::make_mut(&mut resolved.symbols).clear_source_lang_items();
        if let Ok(program) = &**program_res {
            program.for_each_decl_recursive(|_, declaration| {
                if let arandu_parser::TopLevelDecl::Extern(external) = declaration {
                    if arandu_parser::AbiKind::from_abi_str(&external.abi)
                        == arandu_parser::AbiKind::AranduIntrinsic
                    {
                        resolved.diagnostics.push(Diagnostic::error(
                            arandu_middle::DiagCode::U001FeatureNotSupported,
                            "the 'arandu-intrinsic' ABI requires a trusted standard-library source",
                            external.span,
                        ));
                    }
                }
            });
        }
    }
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

fn index_associated_methods<'a>(
    tables: impl IntoIterator<
        Item = &'a std::collections::BTreeMap<
            String,
            (arandu_middle::SymbolId, arandu_middle::SymbolKind),
        >,
    >,
) -> std::collections::BTreeMap<
    &'a str,
    Vec<(&'a str, arandu_middle::SymbolId, arandu_middle::SymbolKind)>,
> {
    let mut methods = std::collections::BTreeMap::new();
    for table in tables {
        for (key, &(id, kind)) in table {
            if kind != arandu_middle::SymbolKind::AssociatedFunc {
                continue;
            }
            let Some((owner, method)) = key.split_once('.') else {
                continue;
            };
            methods
                .entry(owner)
                .or_insert_with(Vec::new)
                .push((method, id, kind));
        }
    }
    methods
}

fn insert_reexported_methods(
    methods: &std::collections::BTreeMap<
        &str,
        Vec<(&str, arandu_middle::SymbolId, arandu_middle::SymbolKind)>,
    >,
    type_name: &str,
    export_name: &str,
    map: &mut std::collections::BTreeMap<
        String,
        (arandu_middle::SymbolId, arandu_middle::SymbolKind),
    >,
) {
    if let Some(found) = methods.get(type_name) {
        for &(method, id, kind) in found {
            map.insert(format!("{export_name}.{method}"), (id, kind));
        }
    }
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
    let mut lang_items = std::collections::BTreeMap::new();
    let mut sealed_symbols = std::collections::BTreeSet::new();
    let mut sealed_ids = rustc_hash::FxHashSet::default();

    let global_scope = locals.symbols.global_scope();
    for symbol in locals.symbols.iter() {
        if symbol.scope == global_scope && symbol.visibility == arandu_parser::Visibility::Public {
            if locals.symbols.sealed_interfaces.contains(&symbol.id) {
                sealed_symbols.insert(symbol.name.to_string());
                sealed_ids.insert(symbol.id);
            }
            if let Some(li) = symbol.lang_item {
                lang_items.insert(symbol.id, li);
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

    let program_res = super::resolution::parse(db, file);
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
                        let associated = index_associated_methods([&target_exports.symbols]);
                        for item in items {
                            let name = item.name.as_str();
                            if let Some(&(id, kind)) = target_exports.symbols.get(name) {
                                let export_name =
                                    item.alias.as_ref().unwrap_or(&item.name).to_string();
                                if let Some(&li) = target_exports.lang_items.get(&id) {
                                    lang_items.insert(id, li);
                                }
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
                                    insert_reexported_methods(
                                        &associated,
                                        name,
                                        &export_name,
                                        &mut map,
                                    );
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
        lang_items,
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
    let mut lang_items = std::collections::BTreeMap::new();
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
            if let Some(li) = symbol.lang_item {
                lang_items.insert(symbol.id, li);
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

    let program_res = super::resolution::parse(db, file);
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
                        let associated = index_associated_methods(
                            std::iter::once(&target_exports.symbols).chain(
                                target_internal
                                    .as_ref()
                                    .map(|table| &table.internal_symbols),
                            ),
                        );
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
                                if let Some(&li) =
                                    target_exports.lang_items.get(&id).or_else(|| {
                                        target_internal.as_ref().and_then(|t| t.lang_items.get(&id))
                                    })
                                {
                                    lang_items.insert(id, li);
                                }
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
                                    insert_reexported_methods(
                                        &associated,
                                        name,
                                        &export_name,
                                        &mut map,
                                    );
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
        lang_items,
        sealed_symbols,
        sealed_implementations,
        is_cycle: false,
    })
}
