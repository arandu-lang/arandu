use super::capture::import_synthetic_captures;
use super::{declaration_context, declaration_hir, function_hir, Instance, InstanceHir};
use crate::db::HashEq;
use crate::passes::{item_source_input, PreparedHir};
use crate::{ArandCompilerDb, StableHash};
use arandu_middle::{Diagnostic, Severity};

pub(super) fn build_instance_hir<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
    for_ctfe: bool,
    context: &crate::ctfe::DependencyContext,
) -> HashEq<InstanceHir> {
    let key = instance.key(db);
    let source = item_source_input(db, *instance.file(db), key.definition);
    let has_static_loop = if source.may_have_comptime && key.arguments.is_empty() {
        let mut body_span = None;
        source.program.for_each_decl_recursive(|_, declaration| {
            if arandu_semantics::item_source_span(declaration).start == source.item_start {
                if let arandu_parser::TopLevelDecl::Func(function) = declaration {
                    body_span = Some(function.body.span);
                }
            }
        });
        body_span.is_some_and(|body| {
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
        })
    } else {
        false
    };
    let dependent_header = !for_ctfe
        && !key.arguments.is_empty()
        && crate::passes::declaration_signatures(db, *instance.file(db))
            .type_info
            .deferred_headers
            .contains(&key.definition);
    let staged = dependent_header
        || (source.may_have_comptime && (!key.arguments.is_empty() || has_static_loop));
    let template = if for_ctfe {
        crate::ctfe::instances::instance_ctfe_hir(db, instance, context)
    } else if staged {
        crate::ctfe::instance_staged_hir(db, instance)
    } else {
        HashEq::share(function_hir(db, *instance.file(db), key.definition))
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
    if let Some(hir) = &mut hir {
        db.unwind_if_revision_cancelled();
        // Header linking is memoized per module closure, not repeated in the
        // retained function HIR and then copied again for every instance.
        if for_ctfe {
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
            let headers = declaration_context(db, *instance.file(db));
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
            let Some(context_file) = db.source_file_by_id(file_id) else {
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
            let declarations = declaration_hir(db, context_file);
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
            for (_, imported) in crate::passes::module_import_edges(db, context_file)
                .iter()
                .rev()
            {
                if let Some(file_id) = imported {
                    pending.push(*file_id);
                }
            }
        }
        import_synthetic_captures(db, key, &mut checked);
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
    HashEq::new(InstanceHir::new(
        PreparedHir {
            hir,
            type_check: checked,
            diagnostics,
            source_fingerprint: fingerprint.finalize(),
        },
        function,
        instances,
        generated_symbols,
    ))
}
