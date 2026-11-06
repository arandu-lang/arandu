use std::sync::Arc;

use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::SymbolId;
use arandu_mir::ctfe::{CtfeFunction, EvalErrorKind};

use super::{
    arguments, branches, globals, headers, link_extern_headers, loops, public, roots,
    scalar_resolution_diagnostics, BuildFailure, CtfeLowering, DependencyContext,
};

pub(super) fn lower_scalar_function(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
    headers_only: bool,
    context: &DependencyContext,
) -> HashEq<CtfeLowering> {
    if symbol.file_id != *file.file_id(db) {
        return HashEq::new(CtfeLowering {
            result: Err(BuildFailure::MissingFunction),
        });
    }
    let source = crate::passes::item_source_input(db, file, symbol);
    let parsed = headers_only.then(|| crate::passes::parse(db, file));
    let program = if let Some(parsed) = parsed {
        match &**parsed {
            Ok(program) => program.as_ref(),
            Err(_) => {
                return HashEq::new(CtfeLowering {
                    result: Err(BuildFailure::MissingFunction),
                });
            }
        }
    } else {
        source.program.as_ref()
    };
    let layout = *db.target_config().data_layout(db);
    if let Err(error) = layout.validate() {
        return HashEq::new(CtfeLowering {
            result: Err(BuildFailure::Evaluation(EvalErrorKind::InvalidLayout(
                error,
            ))),
        });
    }
    if let Err(error) =
        arandu_middle::ctfe::IntegerType::new(arandu_middle::types::Primitive::USize, layout)
    {
        return HashEq::new(CtfeLowering {
            result: Err(BuildFailure::Evaluation(EvalErrorKind::Value(error))),
        });
    }
    let signatures = headers::owner_signatures_in_context(db, file, symbol, context);
    let import_errors = signatures
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.severity == arandu_middle::Severity::Error
                && matches!(
                    diagnostic.code,
                    arandu_middle::DiagCode::M001UnresolvedImport
                        | arandu_middle::DiagCode::N006ImportConflict
                        | arandu_middle::DiagCode::N019CyclicReExport
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    if !import_errors.is_empty() {
        return HashEq::new(CtfeLowering {
            result: Err(BuildFailure::Diagnostics(import_errors)),
        });
    }
    let mut has_global_constants = false;
    program.for_each_decl_recursive(|_, declaration| {
        if matches!(declaration, arandu_parser::TopLevelDecl::Const(_)) {
            has_global_constants = true;
        }
    });
    let ordinary_body = !headers_only && !source.may_have_comptime && !has_global_constants;
    let resolution_diagnostics = if ordinary_body {
        Some(scalar_resolution_diagnostics(db, file, symbol))
    } else {
        None
    };
    let item = if ordinary_body {
        crate::passes::item_typing(db, file, symbol).clone()
    } else {
        let mut headers = (**crate::passes::resolved_headers(db, file)).clone();
        // Keep imported semantic identities added by the signature checker.
        headers.declarations.symbols = Arc::clone(&signatures.symbols);
        headers.declarations.resolved = Arc::clone(&signatures.resolved);
        let selected = branches::select_branches_in_context(db, file, symbol, None, &[], context);
        let loops = loops::select_loops_in_context(db, file, symbol, None, &selected, &[], context);
        Arc::make_mut(&mut headers.declarations.resolved)
            .comptime_loops
            .extend(loops.domains.iter().map(|(&key, &value)| (key, value)));
        headers
            .declarations
            .diagnostics
            .extend(loops.diagnostics.iter().cloned());
        Arc::make_mut(&mut headers.declarations.resolved)
            .comptime_branches
            .extend(selected.decisions.iter().map(|(&key, &value)| (key, value)));
        headers
            .declarations
            .diagnostics
            .extend(selected.diagnostics.iter().cloned());
        let arguments =
            arguments::select_arguments_in_context(db, file, symbol, None, &selected, &[], context);
        Arc::make_mut(&mut headers.declarations.resolved)
            .typed_comptime_arguments
            .extend(
                arguments
                    .typed_values
                    .iter()
                    .map(|(&key, value)| (key, value.clone())),
            );
        Arc::make_mut(&mut headers.declarations.resolved)
            .comptime_arguments
            .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
        headers
            .declarations
            .diagnostics
            .extend(arguments.diagnostics.iter().cloned());
        let resolved = arandu_resolve::resolve_selected_body_with_poll(
            program,
            headers,
            arandu_resolve::BodySelection::Function(symbol),
            || db.unwind_if_revision_cancelled(),
        );
        let mut initial = signatures.clone();
        initial.symbols = resolved.symbols;
        initial.resolved = resolved.resolved;
        let mut source_span = None;
        let mut prerequisite_diagnostics = Vec::new();
        program.for_each_decl_recursive(|_, declaration| {
            if arandu_semantics::primary_def_key(declaration)
                .and_then(|key| initial.resolved.definitions.get(&key))
                == Some(&symbol)
            {
                source_span = Some(arandu_semantics::item_source_span(declaration));
            }
        });
        if let Some(span) = source_span {
            if let Err(error) = headers::install_referenced_headers_in_context(
                db,
                &program.pool,
                span,
                &mut initial,
                context,
            ) {
                public::append_failure(
                    &mut prerequisite_diagnostics,
                    roots::RootEvalError::Build(error),
                    span,
                );
            }
            if let Err(error) = globals::install_referenced_globals_in_context(
                db,
                program,
                span,
                &mut initial,
                context,
            ) {
                public::append_failure(
                    &mut prerequisite_diagnostics,
                    roots::RootEvalError::Build(error),
                    span,
                );
            }
        }
        let mut checked = arandu_semantics::check_item_body_only(
            &initial,
            program,
            symbol,
            crate::passes::database_target_info(db),
        );
        checked.diagnostics.extend(resolved.diagnostics);
        checked.diagnostics.extend(prerequisite_diagnostics);
        crate::db::FingerprintEq::exact(checked)
    };
    let build = || -> Result<Arc<CtfeFunction>, BuildFailure> {
        if let Some(diagnostics) = &resolution_diagnostics {
            if diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == arandu_middle::Severity::Error)
            {
                return Err(BuildFailure::Diagnostics(
                    diagnostics.iter().cloned().collect(),
                ));
            }
        }
        if item.type_info.generic_params.contains_key(&symbol) {
            return Err(BuildFailure::GenericFunction);
        }
        // Arc-backed fields are shared; only the selected body contributes to
        // HIR/AMIR. There is deliberately no monomorphizer or imported-body walk.
        let mut checked = (*item).clone();
        let mut span = None;
        program.for_each_decl_recursive(|_, declaration| {
            if arandu_semantics::primary_def_key(declaration)
                .and_then(|key| checked.resolved.definitions.get(&key))
                == Some(&symbol)
            {
                span = Some(arandu_semantics::item_source_span(declaration));
            }
        });
        let span = span.ok_or(BuildFailure::MissingFunction)?;
        // Resolution diagnostics include all bodies; do not reject this unit
        // because a sibling has an unresolved name. This evaluation is not a
        // certificate that the rest of the file is a valid runtime program.
        checked.diagnostics.extend(
            signatures
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.span.file_id == span.file_id
                        && ((diagnostic.span.start >= span.start
                            && diagnostic.span.end <= span.end)
                            || program.imports.iter().any(|import| {
                                diagnostic.span.start >= import.span().start
                                    && diagnostic.span.end <= import.span().end
                            }))
                })
                .cloned(),
        );
        let mut hir = arandu_semantics::lower_ctfe_function_to_hir(&mut checked, program, symbol)
            .map_err(BuildFailure::Diagnostics)?
            .ok_or(BuildFailure::MissingFunction)?;
        let declarations = globals::declaration_context(db, file, program, &mut checked)?;
        let declaration_info = checked.clone();
        arandu_semantics::link_hir_module(&mut checked, &mut hir, &declaration_info, &declarations);
        globals::link_referenced_globals(db, program, span, &mut checked, &mut hir)?;
        link_extern_headers(db, &mut checked, &mut hir)?;
        db.unwind_if_revision_cancelled();
        let (mut program, diagnostics) =
            arandu_semantics::lower_to_amir_with_layout(&mut checked, &hir, layout)
                .map_err(BuildFailure::Diagnostics)?;
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == arandu_middle::Severity::Error)
        {
            return Err(BuildFailure::Diagnostics(diagnostics));
        }
        let function = program.funcs.pop().ok_or(BuildFailure::MissingFunction)?;
        if !program.funcs.is_empty() || function.symbol != symbol {
            return Err(BuildFailure::MissingFunction);
        }
        CtfeFunction::new_with_provider(
            function,
            program.literal_pool,
            &checked.type_info.type_interner,
            layout,
            checked.type_info.as_ref(),
        )
        .map(Arc::new)
        .map_err(BuildFailure::Evaluation)
    };
    HashEq::new(CtfeLowering { result: build() })
}
