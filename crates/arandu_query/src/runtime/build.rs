use super::capture::import_synthetic_captures;
use super::{
    declaration_context, declaration_surface, function_hir, local_declaration_context, Instance,
    InstanceHir,
};
use crate::db::FingerprintEq;
use crate::passes::{item_source_input, PreparedHir};
use crate::{ArandCompilerDb, StableHash};
use arandu_middle::{Diagnostic, Severity};

pub(super) fn build_instance_hir<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
    for_ctfe: bool,
    context: &crate::ctfe::DependencyContext,
) -> FingerprintEq<InstanceHir> {
    let key = instance.key(db);
    let (may_have_comptime, has_static_loop) =
        *item_staging(db, *instance.file(db), key.definition);
    let dependent_header = !for_ctfe
        && !key.arguments.is_empty()
        && crate::passes::declaration_signatures(db, *instance.file(db))
            .type_info
            .deferred_headers
            .contains(&key.definition);
    let staged =
        dependent_header || (may_have_comptime && (!key.arguments.is_empty() || has_static_loop));
    let local =
        !for_ctfe && !staged && super::lowers_locally(db, *instance.file(db), key.definition);
    let template = if for_ctfe {
        FingerprintEq::from_content(&crate::ctfe::instances::instance_ctfe_hir(
            db, instance, context,
        ))
    } else if staged {
        FingerprintEq::from_content(&crate::ctfe::instance_staged_hir(db, instance))
    } else {
        function_hir(db, *instance.file(db), key.definition).clone()
    };
    let mut checked = template.type_check.clone();
    let mut hir = template.hir.as_ref().map(|source| {
        let mut hir = arandu_middle::hir::HirProgram {
            span: source.span,
            module: source.module.clone(),
            decls: Vec::new(),
            pool: arandu_middle::hir::HirPool::new(),
        };
        arandu_semantics::link_hir_module(&mut checked, &mut hir, &template.type_check, source);
        hir
    });
    let mut diagnostics = template.diagnostics.clone();
    let mut function = None;
    let mut instances = Vec::new();
    let mut generated_symbols = Vec::new();
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(template.stable_hash().as_bytes());
    fingerprint.update(key.stable_hash().as_bytes());
    fingerprint.update(&[u8::from(local)]);
    if let Some(hir) = &mut hir {
        db.unwind_if_revision_cancelled();
        // Header linking is memoized per module closure, not repeated in the
        // retained function HIR and then copied again for every instance.
        if for_ctfe {
            let source = item_source_input(db, *instance.file(db), key.definition);
            match crate::ctfe::globals::declaration_context(
                db,
                *instance.file(db),
                &source.program,
                &mut checked,
            ) {
                Ok(context) => {
                    let metadata = checked.clone();
                    arandu_semantics::link_hir_module(&mut checked, hir, &metadata, &context);
                    let program = crate::passes::parse(db, *instance.file(db));
                    if let Ok(program) = &**program {
                        let mut span = None;
                        program.for_each_decl_recursive(|_, declaration| {
                            if arandu_semantics::primary_def_key(declaration)
                                .and_then(|node| checked.resolved.definitions.get(&node))
                                == Some(&key.definition)
                            {
                                span = Some(arandu_semantics::item_source_span(declaration));
                            }
                        });
                        if let Some(span) = span {
                            if let Err(error) = crate::ctfe::globals::link_referenced_globals(
                                db,
                                program,
                                span,
                                &mut checked,
                                hir,
                            ) {
                                crate::ctfe::public::append_failure(
                                    &mut diagnostics,
                                    crate::ctfe::RootEvalError::Build(error),
                                    span,
                                );
                            }
                        }
                    }
                }
                Err(error) => crate::ctfe::public::append_failure(
                    &mut diagnostics,
                    crate::ctfe::RootEvalError::Build(error),
                    hir.span,
                ),
            }
        } else {
            let headers = if local {
                local_declaration_context(db, *instance.file(db))
            } else {
                declaration_context(db, *instance.file(db))
            };
            fingerprint.update(headers.stable_hash().as_bytes());
            if let Some(context) = &headers.hir {
                arandu_semantics::link_hir_module(&mut checked, hir, &headers.type_check, context);
            } else {
                diagnostics.extend(headers.diagnostics.iter().cloned());
            }
        }
        // A concrete argument may come from the caller's module, not from the
        // generic definition's imports (e.g. Vec<String>). Bring only its
        // declaration context, never that module's sibling bodies.
        let mut files = std::collections::BTreeSet::new();
        for argument in &key.arguments {
            if argument
                .for_each_symbol(|symbol| {
                    if checked.symbols.try_get(symbol).is_none() {
                        files.insert(symbol.file_id);
                    }
                })
                .is_err()
            {
                diagnostics.push(Diagnostic::error(
                    arandu_middle::DiagCode::G002GenericInstantiationLimit,
                    "instance arguments exceed the structural type bounds",
                    hir.span,
                ));
            }
        }
        let mut pending = files.into_iter().collect::<Vec<_>>();
        let mut visited = rustc_hash::FxHashSet::default();
        while let Some(file_id) = pending.pop() {
            db.unwind_if_revision_cancelled();
            if !visited.insert(file_id) {
                continue;
            }
            let registered = db.source_file_by_id(file_id);
            fingerprint.update(&file_id.to_le_bytes());
            fingerprint.update(&[u8::from(registered.is_some())]);
            let Some(context_file) = registered else {
                continue;
            };
            if for_ctfe {
                let parsed = crate::passes::parse(db, context_file);
                let Ok(program) = &**parsed else {
                    continue;
                };
                match crate::ctfe::globals::declaration_context(
                    db,
                    context_file,
                    program,
                    &mut checked,
                ) {
                    Ok(context) => {
                        let metadata = checked.clone();
                        arandu_semantics::link_hir_module(&mut checked, hir, &metadata, &context);
                    }
                    Err(error) => crate::ctfe::public::append_failure(
                        &mut diagnostics,
                        crate::ctfe::RootEvalError::Build(error),
                        hir.span,
                    ),
                }
                continue;
            }
            let declarations = declaration_surface(db, context_file);
            fingerprint.update(declarations.stable_hash().as_bytes());
            if let Some(context) = &declarations.artifacts.hir {
                arandu_semantics::link_hir_module(
                    &mut checked,
                    hir,
                    &declarations.artifacts.type_check,
                    context,
                );
            } else {
                diagnostics.extend(declarations.artifacts.diagnostics.iter().cloned());
            }
            let edges = crate::passes::module_import_edges(db, context_file);
            fingerprint.update(&(edges.len() as u64).to_le_bytes());
            for (path, imported) in edges {
                fingerprint.update(path.as_bytes());
                fingerprint.update(&[0]);
                fingerprint.update(&imported.map_or(u64::MAX, u64::from).to_le_bytes());
            }
            for (_, imported) in edges.iter().rev() {
                if let Some(file_id) = imported {
                    pending.push(*file_id);
                }
            }
        }
        import_synthetic_captures(db, key, &mut checked, &mut fingerprint);
        // Narrow CTFE declaration contexts contain local headers only. Retain
        // demanded imported intrinsic declarations before monomorphization;
        // otherwise a layout query becomes an unavailable external call.
        if for_ctfe {
            if let Err(error) = crate::ctfe::link_extern_headers(db, &mut checked, hir) {
                crate::ctfe::public::append_failure(
                    &mut diagnostics,
                    crate::ctfe::RootEvalError::Build(error),
                    hir.span,
                );
            }
        }
        match arandu_semantics::passes::monomorphize::instantiate_function(&mut checked, hir, key) {
            Ok(concrete) => {
                function = Some(concrete.function);
                instances = concrete.instances;
                generated_symbols = concrete.generated_symbols;
                if staged {
                    generated_symbols.extend(if for_ctfe {
                        crate::ctfe::instances::instance_ctfe_symbols(db, instance, context)
                    } else {
                        crate::ctfe::instance_staged_symbols(db, instance)
                    });
                    generated_symbols
                        .sort_unstable_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
                    generated_symbols.dedup();
                }
            }
            Err(errors) => diagnostics.extend(errors),
        }
    }
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        hir = None;
    }
    let source_fingerprint = fingerprint.finalize();
    let output = InstanceHir {
        artifacts: PreparedHir {
            hir,
            type_check: checked,
            diagnostics,
            source_fingerprint,
        },
        function,
        instances,
        generated_symbols,
        local,
    };
    if for_ctfe || staged {
        return FingerprintEq::exact(output);
    }
    // Outside staging and CTFE, the output is a deterministic function of the
    // digests folded into `source_fingerprint`: the template, the instance,
    // the declaration contexts, import edges and captures.
    let mut identity = blake3::Hasher::new();
    identity.update(b"InstanceHir/fingerprint/v1");
    identity.update(source_fingerprint.as_bytes());
    FingerprintEq::fingerprinted(db, "instance_hir", output, identity.finalize())
}

/// Whether the item's text may stage (`comptime`), and whether its body holds
/// a static loop: the position-free facts that choose the staged lowering.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_staging", file = ?file.file_id(db), item = ?symbol,
))]
pub(crate) fn item_staging(
    db: &dyn ArandCompilerDb,
    file: crate::SourceFile,
    symbol: arandu_middle::SymbolId,
) -> (bool, bool) {
    local_staging(db, file, symbol).unwrap_or_else(|| file_staging(db, file, symbol))
}

fn file_staging(
    db: &dyn ArandCompilerDb,
    file: crate::SourceFile,
    symbol: arandu_middle::SymbolId,
) -> (bool, bool) {
    let source = item_source_input(db, file, symbol);
    if !source.may_have_comptime {
        return (false, false);
    }
    let mut body_span = None;
    source.program.for_each_decl_recursive(|_, declaration| {
        if arandu_semantics::item_source_span(declaration).start == source.item_start {
            if let arandu_parser::TopLevelDecl::Func(function) = declaration {
                body_span = Some(function.body.span);
            }
        }
    });
    let has_static_loop = body_span.is_some_and(|body| {
        source.program.pool.stmts.iter().any(|stmt| {
            matches!(
                stmt,
                arandu_parser::Stmt::For {
                    is_comptime: true,
                    ..
                }
            ) && body.start <= stmt.span().start
                && stmt.span().end <= body.end
        })
    });
    (true, has_static_loop)
}

/// [`item_staging`] from the item AST and the item text, without the file
/// AST, so that it costs the item and not its file (RFC 0042).
fn local_staging(
    db: &dyn ArandCompilerDb,
    file: crate::SourceFile,
    symbol: arandu_middle::SymbolId,
) -> Option<(bool, bool)> {
    let id = *crate::passes::item_ast_ids(db, file).get(&symbol)?;
    let extent = crate::passes::item_positions(db, file).item_span(id)?;
    let item = crate::passes::item_ast(db, file, id);
    let Ok(item) = &*item.value else {
        return None;
    };
    // Header names only: CTFE stages items while the signatures are built.
    let headers = crate::passes::resolved_headers(db, file);
    let definitions = &headers.declarations.resolved.definitions;
    let defines = |span: arandu_middle::Span| {
        let key = arandu_middle::NodeKey {
            start: span.start + extent.start,
            end: span.end + extent.start,
        };
        definitions.get(&key) == Some(&symbol)
    };
    let mut selected = None;
    item.for_each_decl_recursive(|_, declaration| {
        let defines = arandu_semantics::primary_def_key(declaration).is_some_and(|key| {
            defines(arandu_middle::Span::new(extent.file_id, key.start, key.end))
        }) || matches!(
            declaration,
            arandu_parser::TopLevelDecl::Extern(block)
                if block.members.iter().any(|member| defines(member.span))
        );
        if defines && selected.is_none() {
            selected = Some(declaration);
        }
    });
    let declaration = selected?;
    let span = arandu_semantics::item_source_span(declaration);
    let text = file.text(db);
    let source = text.get(extent.start as usize..extent.end as usize)?;
    let source = source.get(span.start as usize..span.end as usize)?;
    if !source.contains("comptime") {
        return Some((false, false));
    }
    let arandu_parser::TopLevelDecl::Func(function) = declaration else {
        return Some((true, false));
    };
    let body = function.body.span;
    let has_static_loop = item.pool.stmts.iter().any(|stmt| {
        matches!(
            stmt,
            arandu_parser::Stmt::For {
                is_comptime: true,
                ..
            }
        ) && body.start <= stmt.span().start
            && stmt.span().end <= body.end
    });
    Some((true, has_static_loop))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::DatabaseImpl;
    use std::path::{Path, PathBuf};

    fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                collect(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "aru") {
                files.push(path);
            }
        }
    }

    /// The staging facts read from the item AST equal those of the file AST.
    #[test]
    fn local_staging_equals_the_file_staging() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for dir in ["examples", "stdlib", "tests"] {
            collect(&root.join(dir), &mut files);
        }
        files.sort();
        let (mut items, mut local, mut staged) = (0, 0, 0);
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut db = DatabaseImpl::new();
            let file = db.new_file(path.display().to_string(), text);
            let parsed = crate::passes::parse(&db, file);
            let Ok(program) = &**parsed else { continue };
            let signatures = crate::passes::declaration_signatures(&db, file);
            for symbol in arandu_semantics::body_item_symbols(program, &signatures.resolved) {
                items += 1;
                let Some(facts) = super::local_staging(&db, file, symbol) else {
                    continue;
                };
                local += 1;
                staged += usize::from(facts.0);
                assert_eq!(
                    facts,
                    super::file_staging(&db, file, symbol),
                    "{} {symbol:?}",
                    path.display()
                );
            }
        }
        assert!(
            items > 500 && local * 100 >= items * 80 && staged > 0,
            "{local} of {items} items staged locally, {staged} may stage"
        );
    }
}
