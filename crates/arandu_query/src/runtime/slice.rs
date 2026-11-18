//! Declaration headers per symbol (INC.3b).
//!
//! A unit links the headers of the declarations its typing reaches instead of
//! whole files, so a signature it never reaches leaves its context, and the
//! unit, equal. The stdlib still enters by whole files: lowering also reaches
//! its lang items without naming them.

use std::collections::{BTreeMap, BTreeSet};

use arandu_middle::hir::HirDecl;
use arandu_middle::{Span, SymbolId, SymbolKind};

use super::declaration_surface;
use crate::db::HashEq;
use crate::passes::PreparedHir;
use crate::{ArandCompilerDb, SourceFile, StableHash};

/// The declaration-only HIR of the item `item` of `file`, lowered from its
/// own AST against its item-local resolution, with the positions of `file`
/// as placeholders. A function header needs no body typing; a constant's
/// value does, so it lowers against the constant's typing context. `None`
/// when the item has no local context or does not lower cleanly.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "declaration_slice", file = ?file.file_id(db), item = ?item,
))]
pub(crate) fn declaration_slice(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item: SymbolId,
) -> Option<HashEq<PreparedHir>> {
    let signatures = crate::passes::declaration_signatures(db, file);
    let is_const = signatures
        .symbols
        .try_get(item)
        .is_some_and(|symbol| symbol.kind == SymbolKind::Const);
    let id = *crate::passes::item_ast_ids(db, file).get(&item)?;
    let context = if is_const {
        crate::passes::item_context(db, file, item)
            .as_ref()
            .map(|(context, _)| HashEq::share(context))?
    } else {
        crate::passes::item_typing_input(db, file, item)
            .as_ref()
            .map(HashEq::share)?
    };
    let ast = crate::passes::item_ast(db, file, id);
    let Ok(program) = &*ast.value else {
        return None;
    };
    let mut checked = (*context).clone();
    let mut hir = arandu_semantics::lower_declarations_to_hir(&mut checked, program).ok()?;
    let file_id = *file.file_id(db);
    hir.map_spans(&mut |span| {
        if span.file_id == file_id {
            Span::new(0, 0, 0)
        } else {
            span
        }
    });
    let mut type_check = crate::passes::without_positions(&checked);
    // Only what the header declares and reaches: another declaration of the
    // file, or a symbol it adds, must leave this header equal.
    // A function header reaches what its signature names, not its body; a
    // constant's value is part of its declaration.
    let extent = crate::passes::item_positions(db, file).item_span(id)?;
    let mut declared = BTreeSet::from([item]);
    declared.extend(
        signatures
            .symbols
            .iter_own()
            .filter(|symbol| {
                symbol.span.file_id == extent.file_id
                    && extent.start <= symbol.span.start
                    && symbol.span.end <= extent.end
            })
            .map(|symbol| symbol.id),
    );
    let mut keep = if is_const {
        crate::passes::item_reach(db, file, item).as_ref()?.clone()
    } else {
        let context = crate::passes::signature_context(db, file);
        context
            .type_info
            .reachable_from(&context.symbols, declared.iter().copied())?
            .into_iter()
            .collect()
    };
    keep.extend(declared);
    type_check.symbols = std::sync::Arc::new(restricted_symbols(&type_check.symbols, &keep));
    type_check.type_info =
        std::sync::Arc::new(type_check.type_info.view_of(|symbol| keep.contains(symbol)));
    let mut identity = blake3::Hasher::new();
    identity.update(b"DeclarationSlice/v1");
    identity.update(type_check.stable_hash().as_bytes());
    let output = PreparedHir {
        hir: Some(hir),
        type_check,
        diagnostics: Vec::new(),
        source_fingerprint: identity.finalize(),
    };
    Some(HashEq::new(output))
}

/// The function symbols of `symbols` that carry a host name.
pub(super) fn function_symbols(symbols: &arandu_middle::SymbolTable) -> BTreeSet<SymbolId> {
    symbols
        .host_function_names
        .keys()
        .copied()
        .filter(|&symbol| {
            symbols.try_get(symbol).is_some_and(|symbol| {
                matches!(
                    symbol.kind,
                    SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc
                )
            })
        })
        .collect()
}

/// The symbols of `keep` in `symbols`, all as imported symbols of a table
/// that owns none, with the tables that describe them.
fn restricted_symbols(
    symbols: &arandu_middle::SymbolTable,
    keep: &BTreeSet<SymbolId>,
) -> arandu_middle::SymbolTable {
    let kept = |symbol: &SymbolId| keep.contains(symbol);
    let mut table = arandu_middle::SymbolTable::new(arandu_middle::db::FileId::MAX);
    for symbol in symbols.iter().filter(|symbol| kept(&symbol.id)) {
        table.register_imported_symbol(symbol.clone());
    }
    table.host_function_names = symbols
        .host_function_names
        .iter()
        .filter(|(symbol, _)| kept(symbol))
        .map(|(symbol, name)| (*symbol, name.clone()))
        .collect();
    table.exported_functions = symbols
        .exported_functions
        .iter()
        .copied()
        .filter(kept)
        .collect();
    table.cold_functions = symbols
        .cold_functions
        .iter()
        .copied()
        .filter(kept)
        .collect();
    table.tooling_entrypoints = symbols
        .tooling_entrypoints
        .iter()
        .copied()
        .filter(kept)
        .collect();
    table.sealed_interfaces = symbols
        .sealed_interfaces
        .iter()
        .copied()
        .filter(kept)
        .collect();
    table.interface_implementations = symbols
        .interface_implementations
        .iter()
        .copied()
        .filter(|(ty, interface)| kept(ty) && kept(interface))
        .collect();
    table.associated_members = std::sync::Arc::new(
        symbols
            .associated_members
            .iter()
            .filter(|((owner, _), member)| kept(owner) && kept(member))
            .map(|(key, member)| (key.clone(), *member))
            .collect(),
    );
    table.type_params = std::sync::Arc::new(
        symbols
            .type_params
            .iter()
            .filter(|(owner, _)| kept(owner))
            .map(|(owner, params)| (*owner, params.clone()))
            .collect(),
    );
    table
}

/// The top-level item of `file` that declares `symbol`: the item itself, or
/// the item whose extent holds the symbol (a field, variant, member or type
/// parameter).
fn owner_item(db: &dyn ArandCompilerDb, file: SourceFile, symbol: SymbolId) -> Option<SymbolId> {
    let items = crate::passes::item_ast_ids(db, file);
    if items.contains_key(&symbol) {
        return Some(symbol);
    }
    let span = crate::passes::declaration_signatures(db, file)
        .symbols
        .try_get(symbol)?
        .span;
    let id = crate::passes::item_positions(db, file).ast_id_containing(span)?;
    items
        .iter()
        .find_map(|(&item, &item_id)| (item_id == id).then_some(item))
}

fn trusted_stdlib(db: &dyn ArandCompilerDb, file: SourceFile) -> bool {
    db.as_db_impl()
        .is_some_and(|db| db.is_trusted_stdlib_source(file))
}

/// The header context of the local unit of `item`: its own typing context,
/// the stdlib files its file imports, and the header of every user
/// declaration its typing reaches. `None` falls back to the file context:
/// the item reaches what cannot be bounded, a header does not lower, or a
/// callee, constant or intrinsic it names has no declaration in the result.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "unit_headers", file = ?file.file_id(db), item = ?item,
))]
pub(crate) fn unit_headers(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item: SymbolId,
) -> Option<HashEq<PreparedHir>> {
    if trusted_stdlib(db, file) {
        return None;
    }
    let reach = crate::passes::item_reach(db, file, item).as_ref()?;
    let (context, _) = crate::passes::item_context(db, file, item).as_ref()?;
    let mut checked = crate::passes::without_positions(context);
    let mut hir = arandu_middle::hir::HirProgram {
        span: Span::new(0, 0, 0),
        module: None,
        decls: Vec::new(),
        pool: arandu_middle::hir::HirPool::new(),
    };

    // The stdlib closure of the file's imports, by path for a stable order.
    let mut stdlib = BTreeMap::new();
    let mut visited = BTreeSet::new();
    let mut pending = vec![file];
    while let Some(current) = pending.pop() {
        if !visited.insert(*current.file_id(db)) {
            continue;
        }
        if trusted_stdlib(db, current) {
            stdlib.insert(current.path(db).clone(), current);
        }
        for (_, imported) in crate::passes::module_import_edges(db, current) {
            if let Some(imported) = imported.and_then(|id| db.source_file_by_id(id)) {
                pending.push(imported);
            }
        }
    }
    let mut diagnostics = Vec::new();
    for source in stdlib.values() {
        let surface = declaration_surface(db, *source);
        diagnostics.extend(surface.artifacts.diagnostics.iter().cloned());
        let linked = surface.artifacts.hir.as_ref()?;
        arandu_semantics::link_hir_module(
            &mut checked,
            &mut hir,
            &surface.artifacts.type_check,
            linked,
        );
    }

    // One header per reached user declaration, the unit's own included (its
    // declaration lowering records its `@Export` marks), ordered by file
    // path. Symbols
    // the compiler provides (empty span, no item) have no header in any
    // context.
    let mut headers = BTreeMap::new();
    let mut builtin = BTreeSet::new();
    for &symbol in reach {
        let source = db.source_file_by_id(symbol.file_id)?;
        if trusted_stdlib(db, source) {
            continue;
        }
        let declared = crate::passes::declaration_signatures(db, source)
            .symbols
            .try_get(symbol);
        if declared.is_some_and(|symbol| symbol.span.start == symbol.span.end) {
            builtin.insert(symbol);
            continue;
        }
        let kind = declared.map(|symbol| symbol.kind);
        if matches!(
            kind,
            Some(
                SymbolKind::Module
                    | SymbolKind::ImportValue
                    | SymbolKind::ImportType
                    | SymbolKind::NamespaceMember
                    | SymbolKind::Local
                    | SymbolKind::Param
            )
        ) {
            continue;
        }
        let owner = owner_item(db, source, symbol)?;
        headers.insert((source.path(db).clone(), owner.local_id.0), (source, owner));
    }
    for &(source, owner) in headers.values() {
        let slice = declaration_slice(db, source, owner).as_ref()?;
        let linked = slice.hir.as_ref()?;
        arandu_semantics::link_hir_module(&mut checked, &mut hir, &slice.type_check, linked);
        // Linking carries host names but not the `@Export` set.
        checked
            .symbols_mut()
            .exported_functions
            .extend(slice.type_check.symbols.exported_functions.iter().copied());
        checked
            .symbols_mut()
            .cold_functions
            .extend(slice.type_check.symbols.cold_functions.iter().copied());
    }

    // The typing context holds the signature-level host names of every
    // function of the file; only the headers lowered here carry the final
    // ones (`@Export`). Marks of symbols the unit does not reach belong to
    // their own units.
    // Nominal types keep their qualified backend names.
    let functions = function_symbols(&checked.symbols);
    let symbols = checked.symbols_mut();
    symbols
        .host_function_names
        .retain(|symbol, _| !functions.contains(symbol) || reach.contains(symbol));
    symbols
        .exported_functions
        .retain(|symbol| reach.contains(symbol));
    symbols
        .cold_functions
        .retain(|symbol| reach.contains(symbol));
    symbols
        .tooling_entrypoints
        .retain(|symbol| reach.contains(symbol));

    // A callee missing from the headers would lower with default argument
    // modes; never use such a context.
    let mut declared = BTreeSet::new();
    for &id in &hir.decls {
        match hir.pool.decl(id) {
            HirDecl::Func(function) => {
                declared.insert(function.symbol);
            }
            HirDecl::Const(value) => {
                declared.insert(value.symbol);
            }
            HirDecl::Extern(block) => {
                declared.extend(
                    hir.pool
                        .func_signatures_list(block.members)
                        .iter()
                        .map(|member| member.symbol),
                );
            }
            _ => {}
        }
    }
    for &symbol in reach {
        if symbol == item || builtin.contains(&symbol) {
            continue;
        }
        let kind = checked.symbols.try_get(symbol).map(|symbol| symbol.kind);
        if matches!(
            kind,
            Some(
                SymbolKind::Func
                    | SymbolKind::AssociatedFunc
                    | SymbolKind::Const
                    | SymbolKind::ExternFunc
            )
        ) && !declared.contains(&symbol)
        {
            return None;
        }
    }
    Some(HashEq::new(PreparedHir {
        hir: Some(hir),
        type_check: checked,
        diagnostics,
        source_fingerprint: blake3::Hash::from([0; 32]),
    }))
}
