//! Call expression synthesis (callee resolution, free/method/generic calls).

mod arg;
mod closure;
mod instantiate;
mod invoke;
mod invoke_direct;
mod owning;
mod thread_target;

pub(crate) use arg::check_call_arg;
pub(crate) use closure::{
    check_callback_args, check_lambda_owning, check_lambda_value, synth_args_with_callbacks,
    synth_call_arg,
};
pub(crate) use instantiate::{bind_type_params, infer_and_instantiate_func};
pub(crate) use owning::{adjust_to_expected, owning_closure_signature};
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

/// A generic function used as a value is one instance, fixed by the type the
/// value must have (`let f: (int) -> int = identity`), as Rust infers the
/// arguments of a `fn` item it coerces to a `fn` pointer.
fn instantiate_function_value(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    symbol: arandu_middle::SymbolId,
    span: Span,
    expected: Option<TypeId>,
) -> Option<TypeId> {
    if !checker.symbols.try_get(symbol).is_some_and(|entry| {
        matches!(
            entry.kind,
            arandu_middle::SymbolKind::Func | arandu_middle::SymbolKind::AssociatedFunc
        )
    }) {
        return None;
    }
    let generics = checker
        .type_info
        .generic_params
        .get(&symbol)
        .filter(|params| !params.is_empty())?
        .clone();
    let template = checker.decl_type(symbol)?;
    let mut bindings = rustc_hash::FxHashMap::default();
    if let Some(expected) = expected
        && matches!(checker.resolve(expected), ArType::Func(..))
    {
        bind_type_params(checker, &generics, &template, expected, &mut bindings);
    }
    let concrete = generics
        .iter()
        .map(|param| {
            bindings
                .get(param)
                .copied()
                .filter(|&ty| !closure::mentions_type_parameter(checker, ty))
        })
        .collect::<Option<Vec<_>>>();
    let Some(concrete) = concrete else {
        let name = checker.symbols.get(symbol).name.clone();
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T001CannotInferType,
                format!("cannot infer the type arguments of `{name}` used as a function value"),
                span,
            )
            .with_label(span, "this value's type must be known here")
            .with_note("annotate it, as in `let f: (int) -> int = identity`"),
        );
        let error = checker.intern(ArType::Error);
        checker.record_expr_type(expr, error);
        return Some(error);
    };
    let concrete = concrete
        .into_iter()
        .map(|ty| checker.resolve(ty))
        .collect::<Vec<_>>();
    types::interfaces::check_instantiation_constraints(checker, &generics, &concrete, span);
    let subst = arandu_middle::types::build_subst(&generics, &concrete);
    let instance =
        arandu_middle::types::substitute_type(&template, &subst, &checker.type_info.type_interner);
    let instance = checker.intern(instance);
    checker.record_expr_type(expr, instance);
    Some(instance)
}

/// A function used as a value is a plain code pointer called with the
/// internal convention, by code that cannot see its obligations. An
/// `@Unsafe` or `extern` function would lose its `unsafe` requirement, an
/// `@Export` function has a C entry point, and a thread-restricted function
/// its capability check (RFC 0032, L2).
fn check_function_value(
    checker: &mut TypeChecker<'_>,
    symbol: arandu_middle::SymbolId,
    span: Span,
) {
    let Some(entry) = checker.symbols.try_get(symbol) else {
        return;
    };
    let kind = entry.kind;
    if !matches!(
        kind,
        arandu_middle::SymbolKind::Func
            | arandu_middle::SymbolKind::ExternFunc
            | arandu_middle::SymbolKind::AssociatedFunc
    ) {
        return;
    }
    if kind == arandu_middle::SymbolKind::ExternFunc
        || checker.type_info.unsafe_functions.contains(&symbol)
    {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::O013ExternRequiresUnsafe,
                "an `@Unsafe` or `extern` function cannot be used as a function value",
                span,
            )
            .with_label(span, "a call through the value would not require `unsafe`")
            .with_note("call it directly inside an `unsafe` block"),
        );
        return;
    }
    if checker.type_info.exported_functions.contains(&symbol) {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::U001FeatureNotSupported,
                "an `@Export` function cannot be used as a function value yet",
                span,
            )
            .with_label(
                span,
                "its exported entry point uses the C calling convention",
            )
            .with_note("wrap it in a plain function and use that as the value"),
        );
        return;
    }
    let _ = check_target_thread_capabilities(checker, symbol, span);
}

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
                if checker
                    .symbols
                    .try_get(symbol_id)
                    .is_some_and(|s| s.kind == arandu_middle::SymbolKind::StaticMut)
                    && !checker.ctx.is_in_unsafe()
                {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::O017StaticMutRequiresUnsafe,
                        "access to `static mut` requires an `unsafe` block",
                        span,
                    ));
                }
                if let Some(var_id) = checker.literal_table.var_for_symbol(symbol_id) {
                    checker.literal_table.bind_expr(expr, var_id);
                }
                if checker.direct_callee != Some(expr) {
                    check_function_value(checker, symbol_id, span);
                    // `own (A) -> R` expected: the function is instantiated at
                    // `(A) -> R` and wrapped in a closure without environment.
                    let owning = expected.and_then(|expected| {
                        owning_closure_signature(checker, expected)
                            .map(|signature| (expected, signature))
                    });
                    let value_expected = owning.map(|(_, signature)| signature).or(expected);
                    let instance =
                        instantiate_function_value(checker, expr, symbol_id, span, value_expected);
                    if let Some((closure_ty, signature)) = owning
                        && checker.symbols.try_get(symbol_id).is_some_and(|entry| {
                            matches!(
                                entry.kind,
                                arandu_middle::SymbolKind::Func
                                    | arandu_middle::SymbolKind::AssociatedFunc
                            )
                        })
                        && let Some(function_ty) =
                            instance.or_else(|| checker.decl_type_id(symbol_id))
                    {
                        return Some(closure::adapt_function_owning(
                            checker,
                            expr,
                            function_ty,
                            closure_ty,
                            signature,
                        ));
                    }
                    if let Some((closure_ty, signature)) = owning
                        && checker.symbols.try_get(symbol_id).is_some_and(|entry| {
                            matches!(
                                entry.kind,
                                arandu_middle::SymbolKind::Local | arandu_middle::SymbolKind::Param
                            )
                        })
                        && let Some(value_ty) = checker
                            .ctx
                            .lookup(symbol_id)
                            .or_else(|| checker.decl_type_id(symbol_id))
                        && matches!(checker.resolve(value_ty), ArType::Func(..))
                    {
                        return Some(closure::adapt_value_owning(
                            checker, expr, symbol_id, value_ty, closure_ty, signature,
                        ));
                    }
                    if let Some(instance) = instance {
                        return Some(instance);
                    }
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
                        arandu_middle::SymbolKind::ConstParam
                            | arandu_middle::SymbolKind::Const
                            | arandu_middle::SymbolKind::StaticMut
                    )
                })
                && let Some(base_ty) = checker.decl_type_id(symbol)
            {
                if checker.symbols.get(symbol).kind == arandu_middle::SymbolKind::StaticMut
                    && !checker.ctx.is_in_unsafe()
                {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::O017StaticMutRequiresUnsafe,
                        "access to `static mut` requires an `unsafe` block",
                        span,
                    ));
                }
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
