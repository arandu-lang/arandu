use std::sync::Arc;

use arandu_middle::Span;

use super::super::{BuildFailure, QueryProvider};
use super::{CtfeRootRequest, RootEvalError, RootSelector};
use crate::ArandCompilerDb;

fn root_cycle<'db>(
    db: &'db dyn ArandCompilerDb,
    _id: salsa::Id,
    request: CtfeRootRequest<'db>,
) -> Result<arandu_middle::ctfe::ConstValue, RootEvalError> {
    let root = request.root(db);
    let file = root.file(db);
    Err(RootEvalError::Build(BuildFailure::Diagnostics(vec![
        arandu_middle::Diagnostic::error(
            arandu_middle::DiagCode::T044ComptimeEvaluationFailed,
            "compile-time evaluation depends on its own static selection",
            Span::new(*file.file_id(db), 0, 0),
        )
        .with_primary_label("cyclic compile-time dependency"),
    ])))
}

#[salsa::tracked(cycle_result = root_cycle)]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db, request),
    fields(query = "ctfe_eval_root")
)]
pub fn ctfe_eval_root<'db>(
    db: &'db dyn ArandCompilerDb,
    request: CtfeRootRequest<'db>,
) -> Result<arandu_middle::ctfe::ConstValue, RootEvalError> {
    let lowered = super::lower::ctfe_root_amir(db, *request.root(db));
    let unit = lowered
        .result
        .as_ref()
        .map(Arc::clone)
        .map_err(|error| RootEvalError::Build(error.clone()))?;
    let provider = QueryProvider {
        db,
        context: request.root(db).dependency(db).clone(),
        failures: Default::default(),
        headers_only: matches!(
            super::select::source_selector(request.root(db).selector(db)),
            RootSelector::IfCondition { .. }
                | RootSelector::StaticIfCondition(_)
                | RootSelector::StaticForBound { .. }
                | RootSelector::StaticForDomain(_)
                | RootSelector::ConstArgument(_)
                | RootSelector::GlobalInitializer
                | RootSelector::HeaderArgument(_)
        ),
    };
    let evaluated =
        arandu_mir::ctfe::evaluate_unit(&provider, unit, &[], *request.budget(db), || {
            db.unwind_if_revision_cancelled();
            false
        });
    if evaluated.is_err() && !provider.failures.borrow().is_empty() {
        return Err(RootEvalError::Build(BuildFailure::Diagnostics(
            provider.failures.into_inner(),
        )));
    }
    evaluated.map_err(RootEvalError::Evaluation)
}
