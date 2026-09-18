use super::method::contains_generic_params;
use arandu_lexer::Span;
use arandu_parser::TypeName;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind, IndexRange};

use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::{ArType, TypeId};
use super::expr::synth_expr;

fn type_path_member(pool: &AstPool, callee: ExprId) -> Option<(&TypeName, &str)> {
    match pool.expr(callee) {
        ExprKind::TypePath { type_name, member } => Some((type_name, member.as_str())),
        _ => None,
    }
}

pub(crate) fn synth_result_ctor(
    checker: &mut TypeChecker<'_>,
    callee: ExprId,
    args: IndexRange,
    span: Span,
    expected: Option<TypeId>,
) -> Option<ArType> {
    let (type_name, member) = type_path_member(checker.pool, callee)?;
    let global_scope = checker.symbols.global_scope();
    let is_result = if let Some(result_sym) = checker.symbols.lookup_type(global_scope, "Result") {
        if let Some(resolved_sym) = checker
            .resolved
            .type_refs
            .get(&type_name.span.into())
            .copied()
        {
            resolved_sym == result_sym
        } else {
            super::super::types::type_name_base(type_name) == "Result"
        }
    } else {
        super::super::types::type_name_base(type_name) == "Result"
    };
    if !is_result {
        return None;
    }
    let arg_ids = checker.pool.expr_list(args).to_vec();

    // Bidirectional: when the expected type is `Result<T, E>`, pin both sides
    // (same family as `.Ok` / `.Err` sugar). Bare `Result.Ok(x)` without context
    // still defaults `E = Err` for the canonical error path.
    let expected_result = expected.and_then(|id| match checker.resolve(id) {
        ArType::Result(ok, err) => Some((id, ok, err)),
        _ => None,
    });

    match member {
        "Ok" => {
            if arg_ids.len() != 1 {
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!("Result.Ok expects 1 argument, found {}", arg_ids.len()),
                    span,
                )
                .with_label(checker.pool.expr_span(callee), "call target is here")
                .with_label(span, format!("{} arguments provided", arg_ids.len()));
                checker.diagnostics.push(diag);
                return Some(ArType::Error);
            }
            if let Some((exp_id, ok_id, _err_id)) = expected_result {
                let got = synth_expr(checker, arg_ids[0]);
                if !checker.unify_ids(ok_id, got) {
                    checker.add_constraint(
                        ok_id,
                        got,
                        ConstraintOrigin::CallArg {
                            call_span: span,
                            param_span: span,
                            arg_span: checker.pool.expr_span(arg_ids[0]),
                            arg_index: 0,
                        },
                    );
                }
                // Return the expected Result so return-type check is exact.
                return Some(checker.resolve(exp_id));
            }
            let ok_ty_id = synth_expr(checker, arg_ids[0]);
            let err_literal_id = checker.intern(ArType::Err);
            Some(ArType::Result(ok_ty_id, err_literal_id))
        }
        "Err" => {
            if arg_ids.len() != 1 {
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!("Result.Err expects 1 argument, found {}", arg_ids.len()),
                    span,
                )
                .with_label(checker.pool.expr_span(callee), "call target is here")
                .with_label(span, format!("{} arguments provided", arg_ids.len()));
                checker.diagnostics.push(diag);
                return Some(ArType::Error);
            }
            if let Some((exp_id, _ok_id, err_id)) = expected_result {
                let got = synth_expr(checker, arg_ids[0]);
                if !checker.unify_ids(err_id, got) {
                    checker.add_constraint(
                        err_id,
                        got,
                        ConstraintOrigin::CallArg {
                            call_span: span,
                            param_span: span,
                            arg_span: checker.pool.expr_span(arg_ids[0]),
                            arg_index: 0,
                        },
                    );
                }
                return Some(checker.resolve(exp_id));
            }
            // No expected type: Ok-side is unknown — use a fresh hole (Error) so
            // later annotation/return can refine; E comes from the argument.
            let err_ty_id = synth_expr(checker, arg_ids[0]);
            let ok_hole = checker.intern(ArType::Error);
            Some(ArType::Result(ok_hole, err_ty_id))
        }
        _ => None,
    }
}

pub(crate) fn synth_option_ctor(
    checker: &mut TypeChecker<'_>,
    callee: ExprId,
    args: IndexRange,
    span: Span,
    expected: Option<TypeId>,
) -> Option<ArType> {
    let (type_name, member) = type_path_member(checker.pool, callee)?;
    let global_scope = checker.symbols.global_scope();
    let is_option = if let Some(option_sym) = checker.symbols.lookup_type(global_scope, "Option") {
        if let Some(resolved_sym) = checker
            .resolved
            .type_refs
            .get(&type_name.span.into())
            .copied()
        {
            resolved_sym == option_sym
        } else {
            super::super::types::type_name_base(type_name) == "Option"
        }
    } else {
        super::super::types::type_name_base(type_name) == "Option"
    };
    if !is_option {
        return None;
    }
    let arg_ids = checker.pool.expr_list(args).to_vec();
    let expected_inner = expected.and_then(|id| match checker.resolve(id) {
        ArType::Option(inner) => Some(inner),
        _ => None,
    });

    match member {
        "Some" => {
            if arg_ids.len() != 1 {
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!("Option.Some expects 1 argument, found {}", arg_ids.len()),
                    span,
                )
                .with_label(checker.pool.expr_span(callee), "call target is here")
                .with_label(span, format!("{} arguments provided", arg_ids.len()));
                checker.diagnostics.push(diag);
                return Some(ArType::Error);
            }
            if let Some(exp_inner) = expected_inner {
                let got = synth_expr(checker, arg_ids[0]);
                if !checker.unify_ids(exp_inner, got) {
                    checker.add_constraint(
                        exp_inner,
                        got,
                        ConstraintOrigin::CallArg {
                            call_span: span,
                            param_span: span,
                            arg_span: checker.pool.expr_span(arg_ids[0]),
                            arg_index: 0,
                        },
                    );
                }
                Some(ArType::Option(exp_inner))
            } else {
                let inner_id = synth_expr(checker, arg_ids[0]);
                Some(ArType::Option(inner_id))
            }
        }
        "None" => {
            if !arg_ids.is_empty() {
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!("Option.None expects 0 arguments, found {}", arg_ids.len()),
                    span,
                )
                .with_label(checker.pool.expr_span(callee), "call target is here")
                .with_label(span, format!("{} arguments provided", arg_ids.len()));
                checker.diagnostics.push(diag);
                return Some(ArType::Error);
            }
            let inner_id = expected_inner.unwrap_or_else(|| checker.intern(ArType::Error));
            Some(ArType::Option(inner_id))
        }
        _ => None,
    }
}

/// T2.2: `.Ok(x)` / `.None` / `.Some(v)` / `.Pending` with expected type context.
pub(crate) fn synth_variant_sugar(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    name: &str,
    args: IndexRange,
    expected: Option<TypeId>,
    span: Span,
) -> TypeId {
    let arg_ids = checker.pool.expr_list(args).to_vec();
    let Some(expected_id) = expected.filter(|id| !checker.resolve(*id).is_error()) else {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T003IncompatibleCallArg,
                format!(
                    "variant sugar `.{name}` requires an expected type (e.g. return type or annotation)"
                ),
                span,
            )
            .with_note(
                "write `Result.Ok(...)` / `Option.None` explicitly, or use in a typed context"
                    .to_string(),
            ),
        );
        return checker.intern(ArType::Error);
    };
    let expected_ty = checker.resolve(expected_id);

    match expected_ty {
        ArType::Result(ok_id, err_id) => match name {
            "Ok" => {
                if arg_ids.len() != 1 {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T012WrongArgCount,
                        format!(".Ok expects 1 argument, found {}", arg_ids.len()),
                        span,
                    ));
                    return checker.intern(ArType::Error);
                }
                let got = synth_expr(checker, arg_ids[0]);
                if !checker.unify_ids(ok_id, got) {
                    checker.add_constraint(
                        ok_id,
                        got,
                        ConstraintOrigin::CallArg {
                            call_span: span,
                            param_span: span,
                            arg_span: checker.pool.expr_span(arg_ids[0]),
                            arg_index: 0,
                        },
                    );
                }
                expected_id
            }
            "Err" => {
                if arg_ids.len() != 1 {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T012WrongArgCount,
                        format!(".Err expects 1 argument, found {}", arg_ids.len()),
                        span,
                    ));
                    return checker.intern(ArType::Error);
                }
                let got = synth_expr(checker, arg_ids[0]);
                if !checker.unify_ids(err_id, got) {
                    checker.add_constraint(
                        err_id,
                        got,
                        ConstraintOrigin::CallArg {
                            call_span: span,
                            param_span: span,
                            arg_span: checker.pool.expr_span(arg_ids[0]),
                            arg_index: 0,
                        },
                    );
                }
                expected_id
            }
            _ => {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T018UndefinedField,
                    format!("`.{name}` is not a Result variant (expected Ok or Err)"),
                    span,
                ));
                checker.intern(ArType::Error)
            }
        },
        ArType::Option(inner_id) => match name {
            "Some" => {
                if arg_ids.len() != 1 {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T012WrongArgCount,
                        format!(".Some expects 1 argument, found {}", arg_ids.len()),
                        span,
                    ));
                    return checker.intern(ArType::Error);
                }
                let got = synth_expr(checker, arg_ids[0]);
                if !checker.unify_ids(inner_id, got) {
                    checker.add_constraint(
                        inner_id,
                        got,
                        ConstraintOrigin::CallArg {
                            call_span: span,
                            param_span: span,
                            arg_span: checker.pool.expr_span(arg_ids[0]),
                            arg_index: 0,
                        },
                    );
                }
                expected_id
            }
            "None" => {
                if !arg_ids.is_empty() {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T012WrongArgCount,
                        format!(".None expects 0 arguments, found {}", arg_ids.len()),
                        span,
                    ));
                    return checker.intern(ArType::Error);
                }
                expected_id
            }
            _ => {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T018UndefinedField,
                    format!("`.{name}` is not an Option variant (expected Some or None)"),
                    span,
                ));
                checker.intern(ArType::Error)
            }
        },
        ArType::Poll(inner_id) => match name {
            "Ready" => {
                if arg_ids.len() != 1 {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T012WrongArgCount,
                        format!(".Ready expects 1 argument, found {}", arg_ids.len()),
                        span,
                    ));
                    return checker.intern(ArType::Error);
                }
                let got = synth_expr(checker, arg_ids[0]);
                if !checker.unify_ids(inner_id, got) {
                    checker.add_constraint(
                        inner_id,
                        got,
                        ConstraintOrigin::CallArg {
                            call_span: span,
                            param_span: span,
                            arg_span: checker.pool.expr_span(arg_ids[0]),
                            arg_index: 0,
                        },
                    );
                }
                expected_id
            }
            "Pending" => {
                if !arg_ids.is_empty() {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T012WrongArgCount,
                        format!(".Pending expects 0 arguments, found {}", arg_ids.len()),
                        span,
                    ));
                    return checker.intern(ArType::Error);
                }
                expected_id
            }
            _ => {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T018UndefinedField,
                    format!("`.{name}` is not a Poll variant (expected Ready or Pending)"),
                    span,
                ));
                checker.intern(ArType::Error)
            }
        },
        ArType::Named(enum_id, expected_args) => {
            let expected_args = checker.type_info.type_interner.type_args(expected_args);
            let enum_name = checker.symbols.get(enum_id).name.clone();
            let Some(variant_sym) = checker.symbols.lookup_associated_member(enum_id, name) else {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T018UndefinedField,
                    format!("`{name}` is not a variant of `{enum_name}`"),
                    span,
                ));
                return checker.intern(ArType::Error);
            };
            // Record resolution for HIR (same as TypePath member).
            checker.resolved.value_ref(span, variant_sym);
            checker.resolved.expr_ref(expr, variant_sym);

            // Get variant constructor signature with expected generic parameters substituted.
            let cache_key = (variant_sym, expected_args.clone());
            let (params, ret) = if let Some(cached) =
                checker.type_info.variant_instantiations.get(&cache_key)
            {
                cached.clone()
            } else {
                let res = if let Some(ArType::Func(params, ret)) = checker.decl_type(variant_sym) {
                    let params = checker.type_info.type_interner.type_args(params);
                    let mut inst_params = params.clone();
                    let mut inst_ret = ret;
                    if !expected_args.is_empty()
                        && let Some(gp) = checker.type_info.generic_params.get(&enum_id)
                    {
                        let interner = &checker.type_info.type_interner;
                        let has_params = params
                            .iter()
                            .any(|&p| contains_generic_params(&interner.resolve(p), gp, interner))
                            || contains_generic_params(&interner.resolve(ret), gp, interner);
                        if has_params {
                            use crate::type_checker::types::{build_subst, substitute_type};
                            let concrete_args: Vec<ArType> =
                                expected_args.iter().map(|&a| checker.resolve(a)).collect();
                            let n = gp.len().min(concrete_args.len());
                            if n > 0 {
                                let subst = build_subst(&gp[..n], &concrete_args[..n]);
                                inst_params = params
                                    .iter()
                                    .map(|&p| {
                                        let ty = checker.resolve(p);
                                        let inst = substitute_type(
                                            &ty,
                                            &subst,
                                            &checker.type_info.type_interner,
                                        );
                                        checker.intern(inst)
                                    })
                                    .collect();
                                let ret_ty = checker.resolve(ret);
                                let ret_inst = substitute_type(
                                    &ret_ty,
                                    &subst,
                                    &checker.type_info.type_interner,
                                );
                                inst_ret = checker.intern(ret_inst);
                            }
                        }
                    }
                    (inst_params, inst_ret)
                } else {
                    (Vec::new(), checker.intern(ArType::Error))
                };
                checker
                    .type_info
                    .variant_instantiations
                    .insert(cache_key, res.clone());
                res
            };

            // Type args of payload: use variant decl type if Func-like, else unit.
            if let Some(ArType::Func(_, _)) = checker.decl_type(variant_sym) {
                if params.len() != arg_ids.len() {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T012WrongArgCount,
                        format!(
                            ".{name} expects {} argument(s), found {}",
                            params.len(),
                            arg_ids.len()
                        ),
                        span,
                    ));
                    return checker.intern(ArType::Error);
                }
                for (i, &arg) in arg_ids.iter().enumerate() {
                    let got = synth_expr(checker, arg);
                    if let Some(&param) = params.get(i)
                        && !checker.unify_ids(param, got)
                    {
                        checker.add_constraint(
                            param,
                            got,
                            ConstraintOrigin::CallArg {
                                call_span: span,
                                param_span: span,
                                arg_span: checker.pool.expr_span(arg),
                                arg_index: i,
                            },
                        );
                    }
                }
                let _ = ret;
            } else if !arg_ids.is_empty() {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!(".{name} expects 0 arguments, found {}", arg_ids.len()),
                    span,
                ));
                return checker.intern(ArType::Error);
            }
            expected_id
        }
        other => {
            let interner = &checker.type_info.type_interner;
            let disp = other.display(&checker.symbols, interner);
            checker.diagnostics.push(crate::Diagnostic::error(
                crate::DiagCode::T003IncompatibleCallArg,
                format!("variant sugar `.{name}` cannot target type `{disp}`"),
                span,
            ));
            checker.intern(ArType::Error)
        }
    }
}

/// A3.6: `Poll.Ready(v)` / `Poll.Pending` (builtin generic like Option).
pub(crate) fn synth_poll_ctor(
    checker: &mut TypeChecker<'_>,
    callee: ExprId,
    args: IndexRange,
    span: Span,
) -> Option<ArType> {
    let (type_name, member) = type_path_member(checker.pool, callee)?;
    let global_scope = checker.symbols.global_scope();
    let poll_sym = checker.symbols.lookup_type(global_scope, "Poll")?;
    let resolved_sym = checker
        .resolved
        .type_refs
        .get(&type_name.span.into())
        .copied()?;
    if resolved_sym != poll_sym {
        return None;
    }
    let arg_ids = checker.pool.expr_list(args).to_vec();
    match member {
        "Ready" => {
            if arg_ids.len() != 1 {
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!("Poll.Ready expects 1 argument, found {}", arg_ids.len()),
                    span,
                )
                .with_label(checker.pool.expr_span(callee), "call target is here")
                .with_label(span, format!("{} arguments provided", arg_ids.len()));
                checker.diagnostics.push(diag);
                return Some(ArType::Error);
            }
            let inner_id = synth_expr(checker, arg_ids[0]);
            Some(ArType::Poll(inner_id))
        }
        "Pending" => {
            if !arg_ids.is_empty() {
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!("Poll.Pending expects 0 arguments, found {}", arg_ids.len()),
                    span,
                )
                .with_label(checker.pool.expr_span(callee), "call target is here");
                checker.diagnostics.push(diag);
                return Some(ArType::Error);
            }
            // Inner type from expected context; Error placeholder if unknown.
            let placeholder = checker.intern(ArType::Error);
            Some(ArType::Poll(placeholder))
        }
        _ => None,
    }
}
