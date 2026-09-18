mod binary;
mod call;
mod control_flow;
mod literal;

use arandu_parser::ast_pool::{ExprId, ExprKind};

use super::super::TypeChecker;
use super::super::types::ArType;

use binary::synth_binary_unary_expr;
use call::synth_call_expr;
use control_flow::synth_control_flow_expr;
use literal::synth_literal_expr;

pub(crate) use call::{check_call_arg, infer_and_instantiate_func};

use super::ctor::synth_variant_sugar;
use arandu_middle::types::type_interner::TypeId;

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker))]
pub fn synth_expr(checker: &mut TypeChecker<'_>, expr: ExprId) -> TypeId {
    synth_expr_expected(checker, expr, None)
}

/// Like [`synth_expr`], but with an optional expected type (T2.2 variant sugar, nil, …).
pub fn synth_expr_expected(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    expected: Option<TypeId>,
) -> TypeId {
    let id = synth_expr_inner(checker, expr, expected);
    checker.record_expr_type(expr, id);
    id
}

fn synth_expr_inner(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    expected: Option<TypeId>,
) -> TypeId {
    let span = checker.pool.expr_span(expr);
    // Copy the immutable pool reference, not the expression payload. Its
    // lifetime is independent of the mutable checker borrow used below.
    let pool = checker.pool;
    let kind = pool.expr(expr);

    if let ExprKind::VariantSugar { name, args } = kind {
        return synth_variant_sugar(checker, expr, name, *args, expected, span);
    }

    if let Some(id) = synth_literal_expr(checker, expr, kind, span, expected) {
        return id;
    }
    if let Some(id) = synth_call_expr(checker, expr, kind, span, expected) {
        return id;
    }
    if let Some(id) = synth_binary_unary_expr(checker, expr, kind, span, expected) {
        return id;
    }
    if let Some(id) = synth_control_flow_expr(checker, expr, kind, span, expected) {
        return id;
    }

    match kind {
        ExprKind::Group { expr: inner_expr } => synth_expr_expected(checker, *inner_expr, expected),
        ExprKind::Error => checker.intern(ArType::Error),
        _ => checker.intern(ArType::Error),
    }
}
