//! Call expression synthesis (callee resolution, free/method/generic calls).

mod arg;
mod instantiate;
mod invoke;
mod invoke_direct;
mod thread_capture;
mod thread_target;

pub(crate) use arg::check_call_arg;
pub(crate) use instantiate::infer_and_instantiate_func;
pub(crate) use thread_target::check_target_thread_capabilities;

use arandu_lexer::Span;
use arandu_parser::CatchHandler;
use arandu_parser::ast_pool::{ExprId, ExprKind};

use super::{synth_expr, synth_expr_expected};
use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::synth::{
    resolve_field, resolve_index, resolve_namespace_field, resolve_namespace_member_type,
};
use crate::type_checker::types::{self, ArType};

use arandu_middle::types::type_interner::TypeId;

pub(super) fn synth_call_expr(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    kind: &ExprKind,
    span: Span,
    expected: Option<TypeId>,
) -> Option<TypeId> {
    match kind {
        ExprKind::Path { path: _ } => {
            if let Some(symbol_id) = checker.resolved.expr_symbol(expr) {
                if let Some(var_id) = checker.literal_table.var_for_symbol(symbol_id) {
                    checker.literal_table.bind_expr(expr, var_id);
                }
                if !checker
                    .type_info
                    .generic_params
                    .get(&symbol_id)
                    .is_some_and(|parameters| !parameters.is_empty())
                    && let Some(ty_id) = checker.type_info.demand_header(symbol_id, &[], span)
                {
                    return Some(ty_id);
                }
                if let Some(ty_id) = checker.ctx.lookup(symbol_id) {
                    return Some(ty_id);
                }
                if let Some(ty_id) = checker.decl_type_id(symbol_id) {
                    return Some(ty_id);
                }
            }
            Some(checker.intern(ArType::Error))
        }
        ExprKind::TypePath { type_name, member } => {
            if let Some(symbol) = checker.resolved.expr_symbol(expr)
                && checker.symbols.try_get(symbol).is_some_and(|entry| {
                    matches!(
                        entry.kind,
                        arandu_middle::SymbolKind::ConstParam | arandu_middle::SymbolKind::Const
                    )
                })
                && let Some(base_ty) = checker.decl_type_id(symbol)
            {
                return Some(crate::type_checker::synth::resolve_field_type(
                    checker,
                    base_ty,
                    type_name.span,
                    member,
                    span,
                    false,
                ));
            }
            // Bare `Result.Ok` / `Result.Err` as paths (not calls): build a
            // polymorphic-looking Func using expected `Result<T,E>` when present.
            // Actual calls are handled by `synth_result_ctor` (bidirectional).
            let type_symbol = checker
                .resolved
                .type_refs
                .get(&type_name.span.into())
                .copied();
            if type_symbol.is_some_and(|symbol| checker.symbols.is_result_type(symbol)) {
                return Some(match member.as_str() {
                    "Ok" => {
                        let (ok_id, err_id) = match expected.map(|e| checker.resolve(e)) {
                            Some(ArType::Result(ok, err)) => (ok, err),
                            _ => {
                                let hole = checker.intern(ArType::Error);
                                let err_lit = checker.intern(ArType::Err);
                                (hole, err_lit)
                            }
                        };
                        let res_id = checker.intern(ArType::Result(ok_id, err_id));
                        let func_ty =
                            ArType::func(&[ok_id], res_id, &checker.type_info.type_interner);
                        checker.intern(func_ty)
                    }
                    "Err" => {
                        let (ok_id, err_id) = match expected.map(|e| checker.resolve(e)) {
                            Some(ArType::Result(ok, err)) => (ok, err),
                            _ => {
                                let hole = checker.intern(ArType::Error);
                                let err_lit = checker.intern(ArType::Err);
                                (hole, err_lit)
                            }
                        };
                        let res_id = checker.intern(ArType::Result(ok_id, err_id));
                        let func_ty =
                            ArType::func(&[err_id], res_id, &checker.type_info.type_interner);
                        checker.intern(func_ty)
                    }
                    _ => checker.intern(ArType::Error),
                });
            }
            if type_symbol.is_some_and(|symbol| checker.symbols.is_option_type(symbol)) {
                let inner_id = match expected.map(|e| checker.resolve(e)) {
                    Some(ArType::Option(inner)) => inner,
                    _ => checker.intern(ArType::Error),
                };
                return Some(match member.as_str() {
                    "Some" => {
                        let opt_ty = ArType::Option(inner_id);
                        let opt_id = checker.intern(opt_ty);
                        let func_ty =
                            ArType::func(&[inner_id], opt_id, &checker.type_info.type_interner);
                        checker.intern(func_ty)
                    }
                    "None" => checker.intern(ArType::Option(inner_id)),
                    _ => checker.intern(ArType::Error),
                });
            }
            if type_symbol.is_some_and(|symbol| checker.symbols.is_poll_type(symbol)) {
                let inner = match expected.map(|e| checker.resolve(e)) {
                    Some(ArType::Poll(inner)) => inner,
                    _ => checker.intern(ArType::Error),
                };
                return Some(match member.as_str() {
                    "Ready" => {
                        let poll_id = checker.intern(ArType::Poll(inner));
                        let func_ty =
                            ArType::func(&[inner], poll_id, &checker.type_info.type_interner);
                        checker.intern(func_ty)
                    }
                    "Pending" => checker.intern(ArType::Poll(inner)),
                    _ => checker.intern(ArType::Error),
                });
            }

            let type_key = crate::NodeKey::from(type_name.span);
            if let Some(enum_symbol_id) = checker.resolved.type_refs.get(&type_key) {
                let mut variant_symbol_opt = None;
                for (&var_id, &(parent_id, _)) in &checker.type_info.enum_variants {
                    if parent_id == *enum_symbol_id {
                        let Some(var_sym) = checker.symbols.try_get(var_id) else {
                            continue;
                        };
                        let var_name = &var_sym.name;
                        if var_name == member || var_name.ends_with(&format!(".{}", member)) {
                            variant_symbol_opt = Some(var_id);
                            break;
                        }
                    }
                }
                if let Some(variant_symbol_id) = variant_symbol_opt
                    && let Some((_, shape)) = checker
                        .type_info
                        .enum_variants
                        .get(&variant_symbol_id)
                        .cloned()
                {
                    let enum_ty = match expected.map(|id| checker.resolve(id)) {
                        Some(ArType::Named(expected_enum, args))
                            if expected_enum == *enum_symbol_id =>
                        {
                            ArType::Named(expected_enum, args)
                        }
                        _ => ArType::named(*enum_symbol_id, &[], &checker.type_info.type_interner),
                    };
                    match shape {
                        crate::type_checker::EnumPayloadShape::Unit => {
                            return Some(checker.intern(enum_ty));
                        }
                        crate::type_checker::EnumPayloadShape::Tuple(tids) => {
                            let enum_id = checker.intern(enum_ty);
                            let func_ty =
                                ArType::func(&tids, enum_id, &checker.type_info.type_interner);
                            return Some(checker.intern(func_ty));
                        }
                    }
                }
            }

            if let Some(symbol_id) = checker.resolved.expr_symbol(expr)
                && let Some(ty_id) = checker.decl_type_id(symbol_id)
            {
                return Some(ty_id);
            }
            Some(checker.intern(ArType::Error))
        }
        ExprKind::Generic { callee, args } => {
            let callee_id = *callee;
            let args_range = *args;
            let ty = types::synth_generic_instantiation(checker, callee_id, args_range, span);
            Some(checker.intern(ty))
        }
        ExprKind::Field { base, field } => {
            let base_id = *base;
            let field_str = field.clone();
            let ty_id = if let Some(ty_id) =
                resolve_namespace_field(checker, base_id, expr, &field_str, span)
            {
                ty_id
            } else if let Some(ty_id) = resolve_namespace_member_type(checker, expr) {
                ty_id
            } else {
                resolve_field(checker, base_id, &field_str, span, false)
            };
            Some(ty_id)
        }
        ExprKind::SafeField { base, field } => {
            let base_id = *base;
            let field_str = field.clone();
            let ty_id = if let Some(ty_id) =
                resolve_namespace_field(checker, base_id, expr, &field_str, span)
            {
                ty_id
            } else if let Some(ty_id) = resolve_namespace_member_type(checker, expr) {
                ty_id
            } else {
                resolve_field(checker, base_id, &field_str, span, true)
            };
            Some(ty_id)
        }
        ExprKind::Index { base, index } => {
            let base_id = *base;
            let index_id = *index;
            let ty_id = resolve_index(checker, base_id, index_id, false);
            Some(ty_id)
        }
        ExprKind::SafeIndex { base, index } => {
            let base_id = *base;
            let index_id = *index;
            let ty_id = resolve_index(checker, base_id, index_id, true);
            Some(ty_id)
        }
        ExprKind::Try { expr: inner_expr } => {
            let inner_id = *inner_expr;
            let inner_ty_id = synth_expr(checker, inner_id);
            let inner_ty = checker.resolve(inner_ty_id);
            Some(if let Some(ok_ty) = checker.try_ok_type(&inner_ty) {
                let return_ty_id = checker.ctx.current_return();
                let return_ty = return_ty_id.map(|id| checker.resolve(id));
                // An unannotated isolated block learns its result from its
                // explicit returns/tail. Validate `?` after that inference;
                // using void here rejects valid nested helpers prematurely.
                let deferred = return_ty_id.is_none() && checker.ctx.is_ctfe_return();
                if deferred {
                    checker.ctx.record_ctfe_propagation(
                        crate::type_checker::context::ReturnValue {
                            ty: inner_ty_id,
                            span,
                            expression: None,
                        },
                    );
                }
                let can_propagate = deferred
                    || return_ty
                        .as_ref()
                        .is_some_and(|target| checker.can_propagate_try(&inner_ty, target));
                if !can_propagate {
                    let found = return_ty_id.unwrap_or_else(|| checker.intern(ArType::Void));
                    let return_span = checker.ctx.current_return_decl_span().unwrap_or(span);
                    checker.add_constraint(
                        ArType::Error,
                        found,
                        ConstraintOrigin::TryReturnInvalid { span, return_span },
                    );
                }
                checker.intern(ok_ty)
            } else if inner_ty.is_error() {
                checker.intern(ArType::Error)
            } else {
                checker.add_constraint(
                    ArType::Error,
                    inner_ty_id,
                    ConstraintOrigin::TryInvalid { span },
                );
                checker.intern(ArType::Error)
            })
        }
        ExprKind::NullCoalesce { left, right } => {
            let left_id = *left;
            let right_id = *right;
            let left_ty_id = synth_expr(checker, left_id);
            let left_ty = checker.resolve(left_ty_id);
            let unwrapped_ty = match left_ty {
                ArType::Nullable(inner) => Some(checker.resolve(inner)),
                ArType::Option(inner) => Some(checker.resolve(inner)),
                _ => None,
            };
            let right_expected = unwrapped_ty.as_ref().map(|t| checker.intern(t.clone()));
            let right_ty_id = synth_expr_expected(checker, right_id, right_expected);
            let right_ty = checker.resolve(right_ty_id);

            if let Some(target_ty) = unwrapped_ty {
                let target_id = checker.intern(target_ty);
                if !checker.unify_ids(target_id, right_ty_id)
                    && !checker.is_assignable(right_ty_id, target_id)
                {
                    checker.add_constraint(
                        target_id,
                        right_ty_id,
                        ConstraintOrigin::NullCoalesce {
                            left_span: checker.pool.expr_span(left_id),
                            right_span: checker.pool.expr_span(right_id),
                        },
                    );
                }
                Some(target_id)
            } else if left_ty.is_error() || right_ty.is_error() {
                Some(checker.intern(ArType::Error))
            } else {
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T005OperatorNotApplicable,
                        format!(
                            "left operand of `??` must be nullable or `Option`, found '{}'",
                            left_ty.display(&checker.symbols, &checker.type_info.type_interner)
                        ),
                        span,
                    )
                    .with_label(
                        checker.pool.expr_span(left_id),
                        format!(
                            "this has type '{}'",
                            left_ty.display(&checker.symbols, &checker.type_info.type_interner)
                        ),
                    ),
                );
                Some(checker.intern(ArType::Error))
            }
        }
        ExprKind::Call { .. } => invoke::synth_invoked_call(checker, expr, kind, span, expected),
        ExprKind::Catch {
            expr: inner_expr,
            handler,
        } => {
            let inner_id = *inner_expr;
            let handler_id = *handler;
            let inner_ty_id = synth_expr(checker, inner_id);
            let handler_def = checker.pool.catch_handler(handler_id);
            let handler_ty_id = match handler_def {
                CatchHandler::Expr {
                    expr: h,
                    span: h_span,
                } => {
                    let ty_id = synth_expr(checker, *h);
                    (*h_span, ty_id)
                }
                CatchHandler::Block {
                    block,
                    span: h_span,
                    ..
                } => {
                    // Bind `|e|` to the Result error type before type-checking the body.
                    if let Some((_, err_ty_id)) = checker.result_ok_err_ids(inner_ty_id) {
                        let err_key = crate::NodeKey::from(*h_span);
                        if let Some(symbol_id) = checker.resolved.definitions.get(&err_key).copied()
                        {
                            checker.ctx.bind(symbol_id, err_ty_id);
                            checker.record_decl_type(symbol_id, err_ty_id);
                        }
                    }
                    let ty = crate::type_checker::check::check_block(checker, checker.pool, block);
                    (*h_span, checker.intern(ty))
                }
            };
            Some(
                if let Some((ok_ty_id, _)) = checker.result_ok_err_ids(inner_ty_id) {
                    if !checker.unify_ids(ok_ty_id, handler_ty_id.1) {
                        checker.add_constraint(
                            ok_ty_id,
                            handler_ty_id.1,
                            ConstraintOrigin::CatchHandler {
                                expr_span: checker.pool.expr_span(inner_id),
                                handler_span: handler_ty_id.0,
                            },
                        );
                    }
                    ok_ty_id
                } else if checker.resolve(inner_ty_id).is_error() {
                    checker.intern(ArType::Error)
                } else {
                    let inner_ty = checker.resolve(inner_ty_id);
                    let interner = &checker.type_info.type_interner;
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T005OperatorNotApplicable,
                            format!(
                                "operator `catch` requires a `Result` type, found '{}'",
                                inner_ty.display(&checker.symbols, interner)
                            ),
                            span,
                        )
                        .with_label(
                            checker.pool.expr_span(inner_id),
                            format!("type is '{}'", inner_ty.display(&checker.symbols, interner)),
                        ),
                    );
                    checker.intern(ArType::Error)
                },
            )
        }
        _ => None,
    }
}
