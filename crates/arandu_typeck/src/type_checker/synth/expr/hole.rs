//! Typed holes: `todo`, `todo()` and `todo("reason")` (RFC 0036, H1).
//!
//! A hole is an expression not written yet. It takes the type expected where
//! it stands (its goal), or an inference variable that the rest of the body
//! binds, so the program around it type checks without cascading errors. At
//! the end of the body each hole is `T102` with its goal, its reason and the
//! locals in scope that would fill it; being an error, it stops build, run,
//! tests and compile-time evaluation before anything executes.

use arandu_middle::symbol_table::LangItem;
use arandu_parser::ast_pool::{ExprId, ExprKind, IndexRange};
use smol_str::SmolStr;

use super::synth_expr_expected;
use crate::type_checker::TypeChecker;
use crate::type_checker::types::{ArType, Primitive, TypeId};

/// The `todo` name of a hole expression, and its arguments when called.
pub(super) fn hole_parts(
    checker: &TypeChecker<'_>,
    expr: ExprId,
) -> Option<(ExprId, Option<IndexRange>)> {
    let is_hole = |name: ExprId| {
        matches!(checker.pool.expr(name), ExprKind::Path { .. })
            && checker
                .resolved
                .expr_symbol(name)
                .and_then(|symbol| checker.symbols.try_get(symbol))
                .is_some_and(|symbol| symbol.lang_item == Some(LangItem::Hole))
    };
    match checker.pool.expr(expr) {
        ExprKind::Path { .. } if is_hole(expr) => Some((expr, None)),
        ExprKind::Call { callee, args, .. } if is_hole(*callee) => Some((*callee, Some(*args))),
        _ => None,
    }
}

/// Checks a hole and records it for its `T102`.
pub(super) fn synth_hole(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    (name, args): (ExprId, Option<IndexRange>),
    expected: Option<TypeId>,
) -> TypeId {
    let span = checker.pool.expr_span(expr);
    let name_span = checker.pool.expr_span(name);
    let hole_symbol = checker.resolved.expr_symbol(name);
    let goal = match (expected, hole_symbol) {
        (Some(expected), _) => expected,
        (None, Some(symbol)) => checker.fresh_type_var(symbol, None, span),
        (None, None) => checker.intern(ArType::Error),
    };
    let mut reason = None;
    let str_ty = checker.intern(ArType::Primitive(Primitive::Str));
    let arg_ids = args
        .map(|args| checker.pool.expr_list(args).to_vec())
        .unwrap_or_default();
    for &arg in &arg_ids {
        synth_expr_expected(checker, arg, Some(str_ty));
    }
    match arg_ids.as_slice() {
        [] => {}
        [arg] => reason = literal_text(checker, *arg),
        _ => checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T012WrongArgCount,
                format!("`todo` takes at most one reason, found {}", arg_ids.len()),
                span,
            )
            .with_hint("write `todo` or `todo(\"what is missing\")`"),
        ),
    }
    let params: &[TypeId] = if args.is_some() && !arg_ids.is_empty() {
        &[str_ty]
    } else {
        &[]
    };
    let callee_ty = ArType::func(params, goal, &checker.type_info.type_interner);
    let callee_ty = checker.intern(callee_ty);
    if args.is_some() {
        checker.record_expr_type(name, callee_ty);
    }
    let scope = checker.resolved.hole_scopes.get(&name_span.into()).copied();
    checker
        .infer
        .holes
        .push(crate::type_checker::infer::PendingHole {
            span,
            goal,
            reason,
            scope,
        });
    goal
}

/// The text of a plain string literal reason.
fn literal_text(checker: &TypeChecker<'_>, arg: ExprId) -> Option<SmolStr> {
    let ExprKind::InterpolatedString { parts } = checker.pool.expr(arg) else {
        return None;
    };
    let mut text = String::new();
    for &part in checker.pool.string_part_list(*parts) {
        match checker.pool.string_part(part) {
            arandu_parser::StringPart::Text { text: piece, .. } => text.push_str(piece),
            arandu_parser::StringPart::Expr { .. } => return None,
        }
    }
    Some(text.into())
}
