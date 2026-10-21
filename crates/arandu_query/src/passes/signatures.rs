use crate::{ArandCompilerDb, SourceFile};
use arandu_semantics::TypeCheckResult;

use super::ModuleSignatures;

use std::sync::Arc;

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

    let program_res = super::resolution::parse(db, file);
    let Ok(program) = &**program_res else {
        return Vec::new();
    };
    let mut edges: Vec<_> = program
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
        .collect();

    let current_mod = program.module.as_ref().map(|m| m.path.join("."));
    if !current_mod
        .as_deref()
        .is_some_and(|m| m.starts_with("std.core."))
    {
        for &(path, mod_name) in arandu_resolve::AMBIENT_PRIMITIVE_MODULES {
            if current_mod.as_deref() == Some(mod_name) {
                continue;
            }
            if let Some(imported) = db.as_source_db().resolve_module_path(path) {
                if imported != file && !edges.iter().any(|(p, _)| p == path) {
                    edges.push((path.to_string(), Some(*imported.file_id(db))));
                }
            }
        }
    }
    edges
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
    let program_res = super::resolution::parse(db, file);
    let resolved_arc = super::resolution::resolve(db, file);
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
        Ok(program) => {
            super::resolution::signatures_from_program(db, file, program, &resolved, false)
        }
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
    let parsed = super::resolution::parse(db, file);
    let headers = super::resolution::resolved_headers(db, file);
    let mut checked = match &**parsed {
        Ok(program) => super::resolution::signatures_with_imports(
            db,
            file,
            program,
            &headers.declarations,
            true,
            false,
        ),
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
    let program = super::resolution::parse(db, file);
    let headers = super::resolution::resolved_headers(db, file);
    let result = match &**program {
        Ok(program) => super::resolution::signatures_from_program(
            db,
            file,
            program,
            &headers.declarations,
            true,
        ),
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
    let program = super::resolution::parse(db, file);
    let Ok(program) = &**program else {
        return declarations.clone();
    };
    let mut checked = Arc::clone(&declarations.value);
    super::resolution::merge_imported_borrow_interfaces(db, program, &mut checked);
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
        return super::resolution::resolve(db, file)
            .symbols
            .interface_implementations
            .iter()
            .copied()
            .collect();
    };
    let Some(map) = database.package_module_map() else {
        return super::resolution::resolve(db, file)
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
        return super::resolution::resolve(db, file)
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
            super::resolution::resolve(db, binding.file)
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
