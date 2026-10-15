//! Declaration-scoped frozen constants and demand-driven CTFE dependencies.
//! Seed signatures never request this query: evaluating a constant cannot ask
//! for the final declaration context which contains that same constant.

use super::{BuildFailure, CtfeRoot, CtfeRootRequest, RootExpectedType, RootSelector};
use crate::{db::HashEq, ArandCompilerDb, SourceFile, StableHash};
use arandu_middle::{ctfe::ConstValue, types::TypeShape, DiagCode, Diagnostic, Span, SymbolId};
use arandu_typeck::TypeCheckResult;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrozenConstant {
    pub value: ConstValue,
    pub shape: TypeShape,
}

#[derive(Debug, Clone)]
pub struct GlobalConstant {
    pub result: Result<FrozenConstant, Vec<Diagnostic>>,
}

impl StableHash for GlobalConstant {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hash = blake3::Hasher::new();
        hash.update(b"GlobalConstant/v1");
        match &self.result {
            Ok(constant) => {
                hash.update(&[1]);
                hash.update(&constant.value.canonical_bytes());
                if let Ok(bytes) = arandu_middle::ctfe::canonical_type_bytes(&constant.shape) {
                    hash.update(&bytes);
                }
            }
            Err(diagnostics) => {
                hash.update(&[0]);
                hash.update(diagnostics.stable_hash().as_bytes());
            }
        }
        hash.finalize()
    }
}

fn cycle(
    db: &dyn ArandCompilerDb,
    _id: salsa::Id,
    file: SourceFile,
    _symbol: SymbolId,
) -> HashEq<GlobalConstant> {
    HashEq::new(GlobalConstant {
        result: Err(vec![Diagnostic::error(
            DiagCode::T044ComptimeEvaluationFailed,
            "cyclic compile-time constant dependency",
            Span::new(*file.file_id(db), 0, 0),
        )
        .with_primary_label("constant depends on itself")]),
    })
}

#[salsa::tracked(cycle_result = cycle)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db, file), fields(query = "global_const_value", item = ?symbol))]
pub fn global_const_value(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
) -> HashEq<GlobalConstant> {
    HashEq::share(global_const_value_in_context(
        db,
        file,
        symbol,
        super::DependencyContext::default(),
    ))
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db, file, context), fields(query = "global_const_value_in_context", item = ?symbol))]
pub(crate) fn global_const_value_in_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    symbol: SymbolId,
    context: super::DependencyContext,
) -> HashEq<GlobalConstant> {
    let source = crate::passes::item_source_input(db, file, symbol);
    let seed = crate::passes::seed_header_signatures(db, file);
    let mut initializer = None;
    source.program.for_each_decl_recursive(|_, declaration| {
        if let arandu_parser::TopLevelDecl::Const(constant) = declaration {
            if seed.resolved.definitions.get(&constant.span.into()) == Some(&symbol) {
                initializer = Some(constant.value);
            }
        }
    });
    let span = initializer
        .map(|id| source.program.pool.expr_span(id))
        .unwrap_or(Span::new(*file.file_id(db), 0, 0));
    let context = match context.enter(super::dependency::DependencyKey::Global(symbol), span) {
        Ok(context) => context,
        Err(diagnostic) => {
            return HashEq::new(GlobalConstant {
                result: Err(vec![diagnostic]),
            })
        }
    };
    let build = || {
        if initializer.is_none() || symbol.file_id != *file.file_id(db) {
            return Err(super::RootEvalError::Build(BuildFailure::InvalidRoot));
        }
        let expected = seed
            .type_info
            .decl_type_id(symbol)
            .filter(|&id| !seed.type_info.type_interner.resolve(id).is_error())
            .and_then(|id| TypeShape::from_id(id, &seed.type_info.type_interner).ok())
            .map(RootExpectedType::Structural);
        let root = CtfeRoot::new_in_context(
            db,
            file,
            symbol,
            RootSelector::GlobalInitializer,
            expected,
            context.clone(),
        );
        let lowering = super::ctfe_root_amir(db, root);
        let unit = lowering
            .result
            .as_ref()
            .map_err(|error| super::RootEvalError::Build(error.clone()))?;
        let value = super::ctfe_eval_root(
            db,
            CtfeRootRequest::new(db, root, super::config::public_budget(db)),
        )
        .as_ref()
        .map_err(Clone::clone)?
        .clone();
        Ok(FrozenConstant {
            value,
            shape: unit.result_type_shape(),
        })
    };
    let result = build().map_err(|error| {
        let mut diagnostics = Vec::new();
        super::public::append_failure(&mut diagnostics, error, span);
        diagnostics
    });
    HashEq::new(GlobalConstant { result })
}

fn needs_staging(
    program: &arandu_parser::Program,
    expression: arandu_parser::ast_pool::ExprId,
) -> bool {
    let span = program.pool.expr_span(expression);
    if matches!(
        program.pool.expr(expression),
        arandu_parser::ExprKind::Int { .. }
            | arandu_parser::ExprKind::Bool { .. }
            | arandu_parser::ExprKind::Float { .. }
            | arandu_parser::ExprKind::Byte { .. }
            | arandu_parser::ExprKind::Unary { .. }
            | arandu_parser::ExprKind::Binary { .. }
            | arandu_parser::ExprKind::Group { .. }
            | arandu_parser::ExprKind::Cast { .. }
            | arandu_parser::ExprKind::Array { .. }
            | arandu_parser::ExprKind::InterpolatedString { .. }
    ) {
        return true;
    }
    program
        .pool
        .exprs
        .iter()
        .zip(&program.pool.expr_spans)
        .any(|(kind, child)| {
            child.start >= span.start
                && child.end <= span.end
                && matches!(
                    kind,
                    arandu_parser::ExprKind::Comptime { .. } | arandu_parser::ExprKind::Call { .. }
                )
        })
}

pub(crate) fn install_module_globals(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    checked: &mut TypeCheckResult,
) {
    let parsed = crate::passes::parse(db, file);
    let Ok(program) = &**parsed else {
        return;
    };
    program.for_each_decl_recursive(|_, declaration| {
        let arandu_parser::TopLevelDecl::Const(constant) = declaration else {
            return;
        };
        if !needs_staging(program, constant.value) {
            return;
        }
        let Some(&symbol) = checked.resolved.definitions.get(&constant.span.into()) else {
            return;
        };
        match &global_const_value(db, file, symbol).result {
            Ok(frozen) => {
                if let Ok(ty) = frozen.shape.intern(&checked.type_info.type_interner) {
                    let info = checked.type_info_mut();
                    info.record_decl_type(symbol, ty);
                    info.ctfe_global_values.insert(symbol, frozen.value.clone());
                    if let Err(errors) = install_type_contract(
                        db,
                        checked,
                        ty,
                        program.pool.expr_span(constant.value),
                        &super::DependencyContext::default(),
                    ) {
                        checked.diagnostics.extend(errors);
                    }
                }
            }
            Err(diagnostics) => checked.diagnostics.extend(diagnostics.iter().cloned()),
        }
    });
}

// Freeze only the installed value's metadata. Seed signatures can contain
// demands from unrelated declarations; evaluating them in this root's ancestry
// would introduce false cycles before the selected root is checked.
fn install_type_contract(
    db: &dyn ArandCompilerDb,
    checked: &mut TypeCheckResult,
    ty: arandu_middle::types::type_interner::TypeId,
    span: Span,
    context: &super::DependencyContext,
) -> Result<(), Vec<Diagnostic>> {
    let pending = std::mem::take(&mut checked.type_info_mut().header_requests);
    checked.type_info_mut().demand_type(ty, span);
    let result = super::contracts::freeze_requests(db, checked, context);
    checked.type_info_mut().header_requests.extend(pending);
    result.map(|_| ())
}

fn references(
    program: &arandu_parser::Program,
    span: Span,
    checked: &TypeCheckResult,
) -> Vec<SymbolId> {
    let mut symbols: Vec<_> = program
        .pool
        .expr_spans
        .iter()
        .enumerate()
        .filter_map(|(index, child)| {
            if child.start < span.start || child.end > span.end {
                return None;
            }
            let symbol = checked
                .resolved
                .expr_symbols
                .get(index)
                .copied()
                .flatten()?;
            checked
                .symbols
                .try_get(symbol)
                .is_some_and(|entry| entry.kind == arandu_middle::SymbolKind::Const)
                .then_some(symbol)
        })
        .collect();
    symbols.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    symbols.dedup();
    symbols
}

pub(crate) fn install_referenced_globals_in_context(
    db: &dyn ArandCompilerDb,
    program: &arandu_parser::Program,
    span: Span,
    checked: &mut TypeCheckResult,
    context: &super::DependencyContext,
) -> Result<(), BuildFailure> {
    for symbol in references(program, span, checked) {
        let file = db
            .as_source_db()
            .source_file_by_id(symbol.file_id)
            .ok_or(BuildFailure::MissingFunction)?;
        let frozen = global_const_value_in_context(db, file, symbol, context.clone());
        let constant = frozen
            .result
            .as_ref()
            .map_err(|errors| BuildFailure::Diagnostics(errors.clone()))?;
        let ty = constant
            .shape
            .intern(&checked.type_info.type_interner)
            .map_err(|_| BuildFailure::InvalidRoot)?;
        let info = checked.type_info_mut();
        info.record_decl_type(symbol, ty);
        info.ctfe_global_values
            .insert(symbol, constant.value.clone());
        install_type_contract(db, checked, ty, span, context).map_err(BuildFailure::Diagnostics)?;
    }
    Ok(())
}

pub(crate) fn declaration_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    program: &arandu_parser::Program,
    checked: &mut TypeCheckResult,
) -> Result<arandu_middle::hir::HirProgram, BuildFailure> {
    let seed = crate::passes::seed_header_signatures(db, file);
    let mut context = (**seed).clone();
    context.type_info_mut().merge_from(&checked.type_info);
    context.resolved = std::sync::Arc::clone(&checked.resolved);
    // The caller has checked the selected root and demanded dependencies.
    // Raw signature diagnostics also contain unrelated, unstaged headers.
    context.diagnostics = checked.diagnostics.clone();
    // Initializer diagnostics belong to their own demanded constant queries.
    let mut constants = Vec::new();
    program.for_each_decl_recursive(|_, declaration| {
        if let arandu_parser::TopLevelDecl::Const(constant) = declaration {
            constants.push(program.pool.expr_span(constant.value));
        }
    });
    context.diagnostics.retain(|diagnostic| {
        let staged_header = diagnostic.code == arandu_middle::DiagCode::T042UnsupportedComptime
            && context
                .resolved
                .comptime_arguments
                .keys()
                .chain(context.resolved.typed_comptime_arguments.keys())
                .any(|key| {
                    diagnostic.span.file_id == *file.file_id(db)
                        && key.start <= diagnostic.span.start
                        && diagnostic.span.end <= key.end
                });
        !staged_header
            && !constants.iter().any(|span| {
                diagnostic.span.file_id == span.file_id
                    && diagnostic.span.start >= span.start
                    && diagnostic.span.end <= span.end
            })
    });
    let source = arandu_semantics::lower_ctfe_declarations_to_hir(&mut context, program)
        .map_err(BuildFailure::Diagnostics)?;
    let mut hir = arandu_middle::hir::HirProgram {
        span: source.span,
        module: source.module.clone(),
        decls: Vec::new(),
        pool: arandu_middle::hir::HirPool::new(),
    };
    arandu_semantics::link_hir_module(checked, &mut hir, &context, &source);
    Ok(hir)
}

pub(crate) fn link_referenced_globals(
    db: &dyn ArandCompilerDb,
    program: &arandu_parser::Program,
    span: Span,
    checked: &mut TypeCheckResult,
    hir: &mut arandu_middle::hir::HirProgram,
) -> Result<(), BuildFailure> {
    for symbol in references(program, span, checked) {
        // Linking consumes the value admitted in this request's dependency
        // context. Re-evaluating here would reset its ancestry and budget.
        let frozen = checked
            .type_info
            .ctfe_global_values
            .get(&symbol)
            .ok_or(BuildFailure::InvalidRoot)?;
        let ty = checked
            .type_info
            .decl_type_id(symbol)
            .ok_or(BuildFailure::InvalidRoot)?;
        let value = arandu_semantics::materialize_ctfe_value(
            frozen,
            ty,
            &checked.type_info,
            &mut hir.pool,
            *db.target_config().data_layout(db),
            span,
        )
        .map_err(|_| BuildFailure::InvalidRoot)?;
        let value = hir.pool.alloc_expr(value);
        let declaration = hir.pool.alloc_decl(arandu_middle::hir::HirDecl::Const(
            arandu_middle::hir::HirConst {
                symbol,
                ty,
                value,
                span,
            },
        ));
        hir.decls.push(declaration);
    }
    Ok(())
}

/// Attach an already admitted declaration value to the local initializer only.
/// Span changes stay out of the exported signature product.
pub(crate) fn stage_initializer(
    checked: &mut TypeCheckResult,
    program: &arandu_parser::Program,
    symbol: SymbolId,
    layout: arandu_middle::DataLayout,
) {
    let Some(value) = checked.type_info.ctfe_global_values.get(&symbol).cloned() else {
        return;
    };
    let Some(ty) = checked.type_info.decl_type_id(symbol) else {
        return;
    };
    let mut expression = None;
    program.for_each_decl_recursive(|_, declaration| {
        if let arandu_parser::TopLevelDecl::Const(constant) = declaration {
            if checked.resolved.definitions.get(&constant.span.into()) == Some(&symbol) {
                expression = Some(constant.value);
            }
        }
    });
    if let Some(expression) = expression {
        let info = checked.type_info_mut();
        info.record_expr_type(expression, ty);
        info.ctfe_values
            .insert(program.pool.expr_span(expression), (value, layout));
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn declaration_context_preserves_foreign_diagnostics_at_overlapping_offsets() {
        let mut db = crate::DatabaseImpl::new();
        let file = db.new_file("local.aru".into(), "const N = 42".into());
        let foreign = db.new_file("foreign.aru".into(), "const N = 42".into());
        let parsed = crate::passes::parse(&db, file);
        let Ok(program) = &**parsed else {
            panic!("valid source");
        };
        let mut span = None;
        program.for_each_decl_recursive(|_, declaration| {
            if let arandu_parser::TopLevelDecl::Const(constant) = declaration {
                span = Some(program.pool.expr_span(constant.value));
            }
        });
        let Some(span) = span else {
            panic!("constant initializer");
        };
        let diagnostic = Diagnostic::error(
            DiagCode::T044ComptimeEvaluationFailed,
            "foreign constant failed",
            Span::new(*foreign.file_id(&db), span.start, span.end),
        );
        let mut checked = (**crate::passes::seed_header_signatures(&db, file)).clone();
        checked.diagnostics.push(diagnostic.clone());
        let result = declaration_context(&db, file, program, &mut checked);
        assert!(
            matches!(result, Err(BuildFailure::Diagnostics(errors)) if errors.contains(&diagnostic))
        );
    }
}
