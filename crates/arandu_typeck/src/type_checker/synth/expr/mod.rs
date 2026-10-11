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

    if let ExprKind::Layout { query, ty } = kind {
        let result = match query {
            arandu_parser::LayoutQuery::Size | arandu_parser::LayoutQuery::Align => {
                let Some(ty) = ty else {
                    return checker.intern(ArType::Error);
                };
                if checker.lower_type_expr(*ty, checker.type_scope()) == ArType::Error {
                    return checker.intern(ArType::Error);
                }
                super::super::types::Primitive::USize
            }
            arandu_parser::LayoutQuery::TargetOS | arandu_parser::LayoutQuery::TargetArch => {
                super::super::types::Primitive::Str
            }
            arandu_parser::LayoutQuery::TargetPointerWidth => super::super::types::Primitive::USize,
        };
        return checker.intern(ArType::Primitive(result));
    }

    if let ExprKind::Comptime { body } = kind {
        if !checker.type_info.ctfe_roots.contains(&expr) {
            checker.type_info.ctfe_roots.push(expr);
        }
        // Initial typing only. Evaluation and residualization are owned by the
        // query staging boundary, never by this pure checker.
        let ty = match body {
            arandu_parser::ast_pool::ComptimeBody::Expression(inner) => {
                synth_expr_expected(checker, *inner, expected)
            }
            arandu_parser::ast_pool::ComptimeBody::Block(block) => {
                crate::type_checker::check::check_ctfe_block(checker, pool.block(*block), expected)
                    .return_type
            }
        };
        // A staged value is a typed scalar result, not an unbound literal
        // variable escaping into runtime inference. Explicit context is fed
        // to the inner checker; absent context uses the ordinary default.
        // Freeze numeric leaves of aggregates too. Otherwise a local can keep
        // `[N][M]IntLiteral` while materialization produces `[N][M]int`, making
        // SSA join parameters disagree with their incoming values.
        let interner = &checker.type_info.type_interner;
        let normalized =
            arandu_middle::types::TypeShape::from_id(ty, interner).and_then(|mut shape| {
                shape.default_numeric_literals()?;
                shape.intern(interner)
            });
        return match normalized {
            Ok(ty) => ty,
            Err(_) => {
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T042UnsupportedComptime,
                        "compile-time result exceeds the structural type limits",
                        span,
                    )
                    .with_primary_label("result type is too deeply nested or too large"),
                );
                checker.intern(ArType::Error)
            }
        };
    }

    if let ExprKind::VariantSugar { name, args } = kind {
        return synth_variant_sugar(checker, expr, name, *args, expected, span);
    }

    if let Some(id) = synth_literal_expr(checker, expr, kind, span, expected) {
        return id;
    }
    if let Some(id) = synth_call_expr(checker, expr, kind, span, expected) {
        return id;
    }
    if let ExprKind::Binary { op, left, right } = kind {
        return binary::synth_binary_expr(checker, expr, op, (*left, *right), span, expected)
            .unwrap_or_else(|| checker.intern(ArType::Error));
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
