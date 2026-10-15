//! Public value staging between initial typing and ordinary HIR lowering.
//! This query never requests runtime units or final borrow contracts. Helpers
//! are initially typed by the existing CTFE producer, avoiding query cycles.

use arandu_middle::{
    types::{ArType, Primitive},
    DiagCode, Diagnostic, Span, SymbolId,
};
use arandu_mir::ctfe::{Budget, EvalErrorKind, ScalarEvalError};
use arandu_parser::TopLevelDecl;
use arandu_typeck::TypeCheckResult;

use super::{
    BuildFailure, CtfeRoot, CtfeRootRequest, RootEvalError, RootExpectedType, RootSelector,
};
use crate::{db::HashEq, passes, ArandCompilerDb, SourceFile};

/// Per-obligation-family ceilings, shared by CLI/LSP/web. Explicit roots,
/// static conditions, computed arguments and loop bounds each divide their
/// own envelope among source obligations and generated static occurrences.
/// This is not a compiler-wide or whole-item memory/fuel budget.
/// No wall-clock or host-environment input participates in evaluation.
pub const PUBLIC_BUDGET: Budget = Budget {
    fuel: 1_000_000,
    frames: 128,
    values: 1_000_000,
};
const MAX_PUBLIC_ROOTS: usize = 4096;

/// Independent memoized evaluations in one obligation family share a
/// deterministic phase envelope.
/// Each source obligation can be evaluated at most once per admitted static
/// occurrence; splitting by both dimensions prevents each iteration from
/// restarting the entire allowance. A zero allowance is deliberately rejected
/// by the VM rather than rounded up into an unbounded total.
pub(crate) fn staging_budget(envelope: Budget, obligations: usize, occurrences: u64) -> Budget {
    let obligations = u64::try_from(obligations).unwrap_or(u64::MAX).max(1);
    let obligations = obligations.saturating_mul(occurrences.max(1));
    Budget {
        fuel: envelope.fuel / obligations,
        values: envelope.values / obligations,
        frames: envelope.frames,
    }
}

/// Each ancestor splits its envelope equally among its own frozen domain.
/// This weighting also bounds jagged nested arrays: siblings may have different
/// lengths without each resetting the family's complete allowance.
pub(crate) fn iteration_count(occurrence: &[(u32, super::FrozenConstant, u64)]) -> u64 {
    occurrence.iter().fold(1_u64, |count, (_, _, size)| {
        count.saturating_mul((*size).max(1))
    })
}

#[cfg(test)]
mod budget_tests {
    use super::{staging_budget, PUBLIC_BUDGET};

    #[test]
    fn all_source_obligations_share_the_static_occurrence_envelope() {
        for roots in [1, 2, 100, 4096] {
            let budget = staging_budget(
                PUBLIC_BUDGET,
                roots,
                crate::ctfe::loops::MAX_STATIC_ITERATIONS,
            );
            let evaluations = roots as u64 * crate::ctfe::loops::MAX_STATIC_ITERATIONS;
            assert!(budget.fuel.saturating_mul(evaluations) <= PUBLIC_BUDGET.fuel);
            assert!(budget.values.saturating_mul(evaluations) <= PUBLIC_BUDGET.values);
            assert_eq!(budget.frames, PUBLIC_BUDGET.frames);
        }
        assert_eq!(staging_budget(PUBLIC_BUDGET, 0, 1).fuel, PUBLIC_BUDGET.fuel);
    }

    #[test]
    fn jagged_domains_share_the_envelope_without_the_worst_case_divisor() {
        for roots in [1, 2, 7, 4096] {
            let mut fuel = 0;
            let mut values = 0;
            for length in [1_u64, 3, 5] {
                let budget = staging_budget(PUBLIC_BUDGET, roots, 3 * length);
                let evaluations = u64::try_from(roots).expect("small roots") * length;
                fuel += budget.fuel * evaluations;
                values += budget.values * evaluations;
            }
            assert!(fuel <= PUBLIC_BUDGET.fuel);
            assert!(values <= PUBLIC_BUDGET.values);
        }
        assert_eq!(staging_budget(PUBLIC_BUDGET, 2, u64::MAX).fuel, 0);
        assert_eq!(
            staging_budget(PUBLIC_BUDGET, 1, 2).fuel,
            PUBLIC_BUDGET.fuel / 2
        );
    }
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_staged_typing", file = ?file.file_id(db), item = ?owner,
))]
pub fn item_staged_typing(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
) -> HashEq<TypeCheckResult> {
    let initial = passes::item_typing(db, file, owner);
    if initial.type_info.ctfe_global_values.contains_key(&owner) {
        let mut checked = (**initial).clone();
        let source = passes::item_source_input(db, file, owner);
        super::globals::stage_initializer(
            &mut checked,
            &source.program,
            owner,
            *db.target_config().data_layout(db),
        );
        return HashEq::new(checked);
    }
    stage_public_roots(db, file, owner, initial, None)
}

pub(crate) fn stage_public_roots(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    initial: &HashEq<TypeCheckResult>,
    instance: Option<&arandu_middle::types::FunctionInstance>,
) -> HashEq<TypeCheckResult> {
    stage_public_roots_in_occurrence(db, file, owner, initial, instance, &[])
}

pub(crate) fn stage_public_roots_in_occurrence(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    initial: &HashEq<TypeCheckResult>,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    occurrence: &[(u32, super::FrozenConstant, u64)],
) -> HashEq<TypeCheckResult> {
    stage_public_roots_in_context(
        db,
        file,
        owner,
        initial,
        instance,
        occurrence,
        &super::DependencyContext::default(),
    )
}

pub(crate) fn stage_public_roots_in_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    initial: &HashEq<TypeCheckResult>,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    occurrence: &[(u32, super::FrozenConstant, u64)],
    context: &super::DependencyContext,
) -> HashEq<TypeCheckResult> {
    // Ordinary functions preserve the existing O(1) shared memo path; do not
    // scan the full file's arena for every item in projects without staging.
    if initial.type_info.ctfe_roots.is_empty() {
        return HashEq::share(initial);
    }
    let source = passes::item_source_input(db, file, owner);
    let parsed = (instance.is_some() || !occurrence.is_empty()).then(|| passes::parse(db, file));
    let program = match parsed {
        Some(parsed) => match &**parsed {
            Ok(program) => program.as_ref(),
            Err(_) => return HashEq::share(initial),
        },
        None => source.program.as_ref(),
    };
    let mut span = None;
    let mut function = false;
    program.for_each_decl_recursive(|_, decl| {
        if arandu_semantics::primary_def_key(decl)
            .and_then(|key| initial.resolved.definitions.get(&key))
            == Some(&owner)
        {
            span = Some(arandu_semantics::item_source_span(decl));
            function = matches!(decl, TopLevelDecl::Func(_));
        }
    });
    let Some(span) = span else {
        return HashEq::share(initial);
    };
    // Module/type initializers are already frozen by their declaration query.
    if !function {
        return HashEq::share(initial);
    }
    let roots = super::roots::public_roots(&program.pool, span);
    if roots.is_empty() {
        return HashEq::share(initial);
    }
    let mut checked = (**initial).clone();
    let unsupported = |span, message| {
        Diagnostic::error(DiagCode::T042UnsupportedComptime, message, span)
            .with_primary_label("unsupported compile-time construct")
    };
    if roots.len() > MAX_PUBLIC_ROOTS {
        checked.diagnostics.push(
            Diagnostic::error(
                DiagCode::T045ComptimeLimitExceeded,
                "this item contains too many comptime roots",
                span,
            )
            .with_primary_label("at most 4096 roots per item"),
        );
        return HashEq::new(checked);
    }
    let layout = *db.target_config().data_layout(db);
    // Explicit public roots in an item share their family's finite envelope.
    // Other staging families have separate envelopes. Independent Salsa
    // evaluations cannot restart the full allowance thousands of times.
    let budget = staging_budget(
        super::config::public_budget(db),
        roots.len(),
        super::public::iteration_count(occurrence),
    );
    for (ordinal, &expression) in roots.iter().enumerate() {
        db.unwind_if_revision_cancelled();
        let root_span = program.pool.expr_span(expression);
        let enclosing: Vec<_> = super::roots::static_fors(&program.pool, span)
            .iter()
            .enumerate()
            .filter_map(|(ordinal, statement)| {
                let arandu_parser::Stmt::For { body, .. } = statement else {
                    return None;
                };
                (contains(body.span, root_span) && !contains(root_span, statement.span()))
                    .then_some(ordinal)
            })
            .collect();
        if enclosing.len() != occurrence.len()
            || enclosing
                .iter()
                .zip(occurrence)
                .any(|(ordinal, (selected, _, _))| {
                    usize::try_from(*selected).ok() != Some(*ordinal)
                })
        {
            continue;
        }
        if !initial.type_info.ctfe_roots.contains(&expression) {
            continue; // Roots in discarded static branches are not obligations.
        }
        // Initial semantic errors already explain invalid syntax/types at this
        // root. Do not rerun that checker just to publish duplicate diagnostics.
        if initial.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == arandu_middle::Severity::Error
                && contains(root_span, diagnostic.span)
        }) {
            continue;
        }
        if !function
            || (instance.is_none() && checked.type_info.generic_params.contains_key(&owner))
        {
            checked.diagnostics.push(unsupported(root_span,
                "comptime roots require a concrete function body; constant declarations cannot contain them in this version"));
            continue;
        }
        if roots
            .iter()
            .any(|&other| other != expression && contains(program.pool.expr_span(other), root_span))
        {
            continue;
        }
        let Some(ty) = checked.type_info.expr_type(expression) else {
            continue;
        };
        let ty = ty.default_literal();
        let expected = match ty {
            ArType::Void => RootExpectedType::Unit,
            ArType::Primitive(Primitive::Bool) => RootExpectedType::Primitive(Primitive::Bool),
            ArType::Primitive(primitive) if primitive.is_integer() => {
                RootExpectedType::Primitive(primitive)
            }
            ArType::Error => continue,
            _ => match arandu_middle::types::TypeShape::from_id(
                checked.type_info.type_interner.intern(ty.clone()),
                &checked.type_info.type_interner,
            ) {
                Ok(mut shape) => {
                    if shape.default_numeric_literals().is_err() {
                        checked.diagnostics.push(unsupported(
                            root_span,
                            "this compile-time result has an unresolved or excessive type",
                        ));
                        continue;
                    }
                    RootExpectedType::Structural(shape)
                }
                Err(_) => {
                    checked.diagnostics.push(unsupported(
                        root_span,
                        "this compile-time result has an unresolved or excessive type",
                    ));
                    continue;
                }
            },
        };
        let Ok(ordinal) = u32::try_from(ordinal) else {
            continue;
        };
        let root = CtfeRoot::new_in_context(
            db,
            file,
            owner,
            super::roots::in_occurrence(
                instance.map_or(RootSelector::PublicComptime(ordinal), |instance| {
                    RootSelector::InInstance {
                        instance: instance.clone(),
                        selector: Box::new(RootSelector::PublicComptime(ordinal)),
                    }
                }),
                occurrence,
            ),
            Some(expected),
            context.clone(),
        );
        match super::ctfe_eval_root(db, CtfeRootRequest::new(db, root, budget)) {
            Ok(value) => {
                let info = checked.type_info_mut();
                let ty = info.type_interner.intern(ty);
                info.record_expr_type(expression, ty);
                info.ctfe_values.insert(root_span, (value.clone(), layout));
            }
            Err(error) => append_failure(&mut checked.diagnostics, error.clone(), root_span),
        }
    }
    HashEq::new(checked)
}

fn contains(outer: Span, inner: Span) -> bool {
    outer.file_id == inner.file_id && outer.start <= inner.start && inner.end <= outer.end
}

pub(crate) fn append_failure(output: &mut Vec<Diagnostic>, error: RootEvalError, root: Span) {
    match error {
        RootEvalError::Build(BuildFailure::Diagnostics(diagnostics)) => {
            output.extend(diagnostics);
        }
        RootEvalError::Build(BuildFailure::RuntimeCapture(capture)) => {
            output.push(
                Diagnostic::error(
                    DiagCode::T043ComptimeRuntimeCapture,
                    format!("comptime cannot capture runtime value '{}'", capture.name),
                    capture.use_span,
                )
                .with_primary_label("runtime value used during compile-time evaluation")
                .with_label(capture.declaration_span, "runtime value declared here")
                .with_note("runtime local values and parameters do not exist during compilation")
                .with_hint("declare the value inside the comptime block, or use a global constant"),
            );
        }
        RootEvalError::Evaluation(error) => {
            let mut diagnostic = evaluation_diagnostic(error.kind, root);
            diagnostic = diagnostic.with_label(error.span, "compile-time evaluation stopped here");
            for location in error.trace {
                diagnostic =
                    diagnostic.with_label(location.span, "called during compile-time evaluation");
            }
            if error.trace_truncated {
                diagnostic = diagnostic.with_note("additional compile-time call sites omitted");
            }
            output.push(diagnostic);
        }
        RootEvalError::Build(BuildFailure::Evaluation(kind)) => {
            output.push(evaluation_diagnostic(kind, root))
        }
        RootEvalError::Build(BuildFailure::GenericFunction) => output.push(Diagnostic::error(
            DiagCode::T042UnsupportedComptime,
            "compile-time evaluation requires concrete generic arguments",
            root,
        )),
        RootEvalError::Build(BuildFailure::MissingFunction | BuildFailure::InvalidRoot) => output
            .push(Diagnostic::ice(
                DiagCode::ICET001,
                "public comptime root is missing from its canonical source item",
                root,
            )),
    }
}

fn evaluation_diagnostic(kind: EvalErrorKind, root: Span) -> Diagnostic {
    let (code, message, label) = match kind {
        EvalErrorKind::FuelExhausted
        | EvalErrorKind::FrameLimit
        | EvalErrorKind::ValueLimit
        | EvalErrorKind::AllocationFailed => (
            DiagCode::T045ComptimeLimitExceeded,
            "compile-time evaluation exceeded its resource limit",
            "bounded evaluation stopped",
        ),
        EvalErrorKind::Arithmetic(ScalarEvalError::DivisionByZero) => (
            DiagCode::T040DivisionByZero,
            "division or remainder by zero during compile-time evaluation",
            "invalid compile-time division",
        ),
        EvalErrorKind::Arithmetic(
            ScalarEvalError::Overflow(_) | ScalarEvalError::InvalidShift { .. },
        ) => (
            DiagCode::T046ComptimeArithmetic,
            "compile-time arithmetic overflows or uses an invalid shift",
            "result cannot be represented",
        ),
        EvalErrorKind::UnsupportedType(_) | EvalErrorKind::UnsupportedOperation => (
            DiagCode::T042UnsupportedComptime,
            "this operation or intermediate type is not supported during compile-time evaluation",
            "unsupported compile-time operation",
        ),
        EvalErrorKind::Cancelled
        | EvalErrorKind::MissingFunction(_)
        | EvalErrorKind::UnavailableFunction(_)
        | EvalErrorKind::InvalidIr
        | EvalErrorKind::Uninitialized
        | EvalErrorKind::TypeMismatch
        | EvalErrorKind::InvalidLiteral
        | EvalErrorKind::TargetMismatch
        | EvalErrorKind::Arithmetic(_)
        | EvalErrorKind::Value(_)
        | EvalErrorKind::InvalidLayout(_) => (
            DiagCode::T044ComptimeEvaluationFailed,
            "compile-time evaluation could not evaluate this expression or one of its helpers",
            "compile-time value unavailable",
        ),
    };
    let mut diagnostic = Diagnostic::error(code, message, root).with_primary_label(label);
    match kind {
        EvalErrorKind::FuelExhausted | EvalErrorKind::FrameLimit => {
            diagnostic = diagnostic.with_hint(
                "check for infinite recursion or unbounded loops during compile-time evaluation",
            );
        }
        EvalErrorKind::ValueLimit | EvalErrorKind::AllocationFailed => {
            diagnostic = diagnostic.with_hint(
                "reduce the amount of intermediate data held during compile-time evaluation",
            );
        }
        _ => {}
    }
    diagnostic.with_note("comptime supports pure values admitted by the evaluator; runtime effects and unsupported resources are not allowed")
}
