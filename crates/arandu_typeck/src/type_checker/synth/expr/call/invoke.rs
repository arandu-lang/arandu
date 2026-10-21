use std::sync::Arc;

use arandu_lexer::Span;
use arandu_parser::ast_pool::{ExprId, ExprKind, IndexRange};

use super::thread_target::synth_thread_spawn_closure;
use super::{
    check_call_arg, check_target_thread_capabilities, infer_and_instantiate_func, synth_expr,
    synth_expr_expected,
};
use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::synth::{
    resolve_namespace_field, synth_method_call, synth_option_ctor, synth_poll_ctor,
    synth_result_ctor,
};
use crate::type_checker::types::{self, ArType};
use arandu_middle::types::type_interner::TypeId;

pub(super) fn synth_invoked_call(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    kind: &ExprKind,
    span: Span,
    expected: Option<TypeId>,
) -> Option<TypeId> {
    let ExprKind::Call {
        callee,
        args,
        trailing_block,
    } = kind
    else {
        return None;
    };
    if let Some(block_id) = trailing_block {
        let block = checker.pool.block(*block_id);
        let _ = crate::type_checker::check::check_block(checker, checker.pool, block);
    }
    let callee_id = *callee;
    let args_range = *args;
    // Canonical enum families have dedicated IR types. Select them by
    // resolved identity before nominal generic variant inference, which
    // would otherwise return Named(Option/Poll/Result, ...) for aliases
    // and re-exports of the same standard-library declaration.
    if let Some(result_ty) = synth_result_ctor(checker, callee_id, args_range, span, expected) {
        return Some(checker.intern(result_ty));
    }
    if let Some(option_ty) = synth_option_ctor(checker, callee_id, args_range, span, expected) {
        return Some(checker.intern(option_ty));
    }
    if let Some(poll_ty) = synth_poll_ctor(checker, callee_id, args_range, span, expected) {
        return Some(checker.intern(poll_ty));
    }
    // Enum variant constructors inherit their generic parameters from
    // the parent enum. They are not generic functions in the symbol
    // table, so instantiate the variant signature against its parent
    // before checking payload arguments.
    if let Some(variant_id) = checker.resolved.expr_symbol(callee_id)
        && let Some((enum_id, _)) = checker.type_info.enum_variants.get(&variant_id)
        && let Some(type_params) = checker.type_info.generic_params.get(enum_id).cloned()
        && !type_params.is_empty()
        && let Some(ArType::Func(formals, ret)) = checker.decl_type(variant_id)
    {
        let formals = checker.type_info.type_interner.type_args(formals);
        let arg_ids = checker.pool.expr_list(args_range).to_vec();
        let arg_tys: Vec<TypeId> = arg_ids
            .iter()
            .copied()
            .map(|arg| synth_expr(checker, arg))
            .collect();
        if let Some((params, ret)) = infer_and_instantiate_func(
            checker,
            (Some(variant_id), &type_params),
            &formals,
            ret,
            &arg_tys,
            expected,
            span,
        ) {
            let func_ty = ArType::func(&params, ret, &checker.type_info.type_interner);
            let func_ty_id = checker.intern(func_ty);
            checker.record_expr_type(callee_id, func_ty_id);
            for (index, arg) in arg_ids.iter().copied().enumerate() {
                if let Some(&param) = params.get(index) {
                    let got = synth_expr_expected(checker, arg, Some(param));
                    check_call_arg(
                        checker,
                        arg,
                        param,
                        got,
                        span,
                        checker.pool.expr_span(callee_id),
                        checker.pool.expr_span(arg),
                        index,
                    );
                }
            }
            return Some(ret);
        }
    }
    if let Some(callee_sym) = checker.resolved.expr_symbol(callee_id) {
        // Bare-metal rejection happens before namespace typing. Wasm
        // keeps its U001 on the original call sites so `park`/`unpark`
        // are not diagnosed twice.
        if checker.type_info.target_identity.is_bare_metal()
            && check_target_thread_capabilities(checker, callee_sym, span)
        {
            let err = checker.intern(ArType::Error);
            checker.record_expr_type(expr, err);
            return Some(err);
        }
        if let Some(&eff) = checker.type_info.function_effects.get(&callee_sym) {
            checker.current_observed_effects = checker.current_observed_effects.union(eff);
        }
        let sym = checker.symbols.get(callee_sym);
        if sym.kind == arandu_middle::SymbolKind::ExternFunc {
            let declared_intrinsic =
                arandu_middle::IntrinsicKind::from_declared_name(&sym.name, sym.lang_item);
            let pure_bits = declared_intrinsic.is_some_and(|kind| kind.is_pure_bit_operation())
                && checker.type_info.function_effects.get(&callee_sym)
                    == Some(&arandu_middle::EffectFlags::NONE);
            let pure_simd = sym.lang_item
                == Some(arandu_middle::symbol_table::LangItem::SimdIntrinsic)
                && arandu_middle::intrinsics::SimdIntrinsic::from_name(&sym.name).is_some()
                && checker.type_info.function_effects.get(&callee_sym)
                    == Some(&arandu_middle::EffectFlags::NONE);
            if !pure_bits && !pure_simd {
                checker.current_observed_effects = checker
                    .current_observed_effects
                    .union(arandu_middle::EffectFlags::FOREIGN);
            }
            // Intrinsics that only inspect already-valid fat-pointer descriptors
            // or produce compile-time constants are safe without `unsafe {}`.
            let is_safe_intrinsic = declared_intrinsic
                .is_some_and(|k| k.is_safe() && (!k.is_pure_bit_operation() || pure_bits))
                || pure_simd;
            if !is_safe_intrinsic && !checker.ctx.is_in_unsafe() {
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::O013ExternRequiresUnsafe,
                        "call to extern function requires an `unsafe` block",
                        span,
                    )
                    .with_label(
                        span,
                        "`extern` functions are unsafe and must be called inside an `unsafe` block",
                    ),
                );
            }
        }
        if checker.symbols.is_alloc_func(callee_sym) {
            checker.current_observed_effects = checker
                .current_observed_effects
                .union(arandu_middle::EffectFlags::HEAP);
            let arg_ids = checker.pool.expr_list(args_range).to_vec();
            let arg_ty = if let Some(first) = arg_ids.first() {
                super::synth_expr(checker, *first)
            } else {
                checker.intern(ArType::Error)
            };
            let ptr_ty = checker.intern(ArType::Ptr(arg_ty));
            checker.record_expr_type(expr, ptr_ty);
            return Some(ptr_ty);
        }
        if checker.symbols.is_free_func(callee_sym) {
            checker.current_observed_effects = checker
                .current_observed_effects
                .union(arandu_middle::EffectFlags::HEAP);
            let arg_ids = checker.pool.expr_list(args_range).to_vec();
            if let Some(first) = arg_ids.first() {
                let arg_ty_id = super::synth_expr(checker, *first);
                let arg_ty = checker.resolve(arg_ty_id);
                if !arg_ty.is_error() && !matches!(arg_ty, ArType::Ptr(_)) {
                    let interner = &checker.type_info.type_interner;
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::O011FreeRequiresPtr,
                            format!(
                                "`free` requires a pointer type (`ptr[T]`), found '{}'",
                                arg_ty.display(&checker.symbols, interner)
                            ),
                            span,
                        )
                        .with_label(
                            checker.pool.expr_span(*first),
                            format!(
                                "expression has type '{}'",
                                arg_ty.display(&checker.symbols, interner)
                            ),
                        ),
                    );
                }
            }
            let void_ty = checker.intern(ArType::Void);
            checker.record_expr_type(expr, void_ty);
            return Some(void_ty);
        }
        if checker.symbols.get(callee_sym).lang_item
            == Some(arandu_middle::symbol_table::LangItem::ThreadSpawn)
        {
            check_target_thread_capabilities(checker, callee_sym, span);
            let arg_ids = checker.pool.expr_list(args_range).to_vec();
            if arg_ids.len() == 1
                && matches!(checker.pool.expr(arg_ids[0]), ExprKind::Lambda { .. })
            {
                return synth_thread_spawn_closure(
                    checker, expr, callee_sym, callee_id, arg_ids[0], span,
                );
            }
        }
    }
    if let std::ops::ControlFlow::Break(result) =
        synth_namespace_field_call(checker, expr, span, callee_id, args_range, expected)
    {
        return result;
    }
    if let std::ops::ControlFlow::Break(result) =
        synth_generic_callee_call(checker, expr, span, callee_id, args_range, expected)
    {
        return result;
    }
    super::invoke_direct::synth_direct_symbol_call(
        checker, expr, span, callee_id, args_range, expected,
    )
}

fn synth_namespace_field_call(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    span: Span,
    callee_id: ExprId,
    args_range: IndexRange,
    expected: Option<TypeId>,
) -> std::ops::ControlFlow<Option<TypeId>> {
    if let ExprKind::Field { base, field } = checker.pool.expr(callee_id) {
        let base_id = *base;
        let field_str = field.clone();
        let field_span = checker.pool.expr_span(callee_id);

        // Root cause fix (RC-NEST / namespace calls):
        // `io.println(x)` is a Field callee. Trying method dispatch first
        // synthesizes `io` as a value Path → Error, then returns
        // `Some(Error)` and never types the arguments (including nested
        // method calls). Resolve namespace members before methods.
        if let Some(ns_ty_id) =
            resolve_namespace_field(checker, base_id, callee_id, &field_str, field_span)
        {
            let member_sym = checker.resolved.expr_symbol(callee_id).or_else(|| {
                if let ExprKind::Path { path } = checker.pool.expr(base_id)
                    && path.len() == 1
                {
                    checker.symbols.lookup_module_member(&path[0], &field_str)
                } else {
                    None
                }
            });
            if let Some(sym_id) = member_sym {
                if check_target_thread_capabilities(checker, sym_id, span) {
                    let err = checker.intern(ArType::Error);
                    checker.record_expr_type(expr, err);
                    return std::ops::ControlFlow::Break(Some(err));
                }
                if checker.type_info.unsafe_functions.contains(&sym_id)
                    && !checker.ctx.is_in_unsafe()
                {
                    checker.diagnostics.push(
                        arandu_middle::Diagnostic::error(
                            arandu_middle::DiagCode::O013ExternRequiresUnsafe,
                            "call to an `@Unsafe` function requires an `unsafe` block",
                            span,
                        )
                        .with_label(
                            field_span,
                            "the function's documented preconditions must be upheld by its caller",
                        ),
                    );
                }
            }
            let arg_ids = checker.pool.expr_list(args_range).to_vec();
            if let Some(sym_id) = member_sym
                && checker.symbols.get(sym_id).lang_item
                    == Some(arandu_middle::symbol_table::LangItem::ThreadSpawn)
                && arg_ids.len() == 1
                && matches!(checker.pool.expr(arg_ids[0]), ExprKind::Lambda { .. })
            {
                return std::ops::ControlFlow::Break(synth_thread_spawn_closure(
                    checker, expr, sym_id, callee_id, arg_ids[0], span,
                ));
            }
            let ns_ty = checker.resolve(ns_ty_id);
            if let ArType::Func(params, ret) = ns_ty {
                let mut params = checker.type_info.type_interner.type_args(params);
                let mut ret = ret;
                // Multi-file generic free funcs (`rt.spawn(ex, job)`):
                // same inference as Path callees — instantiate T from
                // Coroutine[T] args / expected return before arg checks.
                if let Some(sym_id) = checker.symbols.lookup_module_member(
                    match checker.pool.expr(base_id) {
                        ExprKind::Path { path } if path.len() == 1 => &path[0],
                        _ => "",
                    },
                    &field_str,
                ) && let Some(gp) = checker.type_info.generic_params.get(&sym_id).cloned()
                    && !gp.is_empty()
                {
                    let arg_tys: Vec<TypeId> = arg_ids
                        .iter()
                        .copied()
                        .map(|aid| synth_expr(checker, aid))
                        .collect();
                    if let Some((ip, ir)) = infer_and_instantiate_func(
                        checker,
                        (Some(sym_id), &gp),
                        &params,
                        ret,
                        &arg_tys,
                        expected,
                        span,
                    ) {
                        params = ip;
                        ret = ir;
                    }
                }
                if params.len() != arg_ids.len() {
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T012WrongArgCount,
                            format!(
                                "expected {} arguments, found {}",
                                params.len(),
                                arg_ids.len()
                            ),
                            span,
                        )
                        .with_label(field_span, "call target is here")
                        .with_label(span, format!("{} arguments provided", arg_ids.len())),
                    );
                }
                for (i, arg_id) in arg_ids.iter().copied().enumerate() {
                    let param_id = params.get(i).copied();
                    let arg_ty_id = synth_expr_expected(checker, arg_id, param_id);
                    if let Some(param_id) = param_id {
                        check_call_arg(
                            checker,
                            arg_id,
                            param_id,
                            arg_ty_id,
                            span,
                            field_span,
                            checker.pool.expr_span(arg_id),
                            i,
                        );
                    }
                }
                // Mark as a direct call target for HIR/codegen.
                let func_ty = ArType::func(&params, ret, &checker.type_info.type_interner);
                let func_ty_id = checker.intern(func_ty);
                checker.record_expr_type(callee_id, func_ty_id);
                return std::ops::ControlFlow::Break(Some(ret));
            }
        }

        if let Some(ret_id) = synth_method_call(
            checker, base_id, callee_id, &field_str, field_span, args_range, span,
        ) {
            return std::ops::ControlFlow::Break(Some(ret_id));
        }
    }
    std::ops::ControlFlow::Continue(())
}

fn synth_generic_callee_call(
    checker: &mut TypeChecker<'_>,
    _expr: ExprId,
    span: Span,
    callee_id: ExprId,
    args_range: IndexRange,
    _expected: Option<TypeId>,
) -> std::ops::ControlFlow<Option<TypeId>> {
    if let ExprKind::Generic {
        callee: gen_callee,
        args: gen_args,
    } = checker.pool.expr(callee_id)
    {
        let gen_callee_id = *gen_callee;
        let gen_args_range = *gen_args;
        if let ExprKind::Field { base, field } = checker.pool.expr(gen_callee_id) {
            let base_id = *base;
            let field_name = field.clone();
            let field_span = checker.pool.expr_span(gen_callee_id);

            let base_ty_id = synth_expr(checker, base_id);
            if !checker.resolve(base_ty_id).is_error() {
                let instantiated_method_ty = types::synth_generic_instantiation(
                    checker,
                    gen_callee_id,
                    gen_args_range,
                    field_span,
                );
                if let ArType::Func(params, ret) = instantiated_method_ty
                    && !params.is_empty()
                {
                    let params = checker.type_info.type_interner.type_args(params);
                    let actual_base_ty_id = match checker.resolve(base_ty_id) {
                        ArType::Nullable(inner) => inner,
                        _ => base_ty_id,
                    };
                    let receiver_ty_id = params[0];
                    // Same auto-ref/auto-deref as synth_method_call: formal
                    // `shared`/`mut self` is `&T`/`&mut T`, receiver value is `T`.
                    let receiver_ok = checker.is_assignable(base_ty_id, receiver_ty_id)
                        || match checker.resolve(receiver_ty_id) {
                            ArType::Ref(inner) | ArType::RefMut(inner) => {
                                checker.is_assignable(base_ty_id, inner)
                            }
                            _ => match checker.resolve(base_ty_id) {
                                ArType::Ref(inner) | ArType::RefMut(inner) => {
                                    checker.type_info.is_copy(inner)
                                        && checker.is_assignable(inner, receiver_ty_id)
                                }
                                _ => false,
                            },
                        };
                    if !receiver_ok {
                        checker.add_constraint(
                            receiver_ty_id,
                            actual_base_ty_id,
                            ConstraintOrigin::CallArg {
                                call_span: span,
                                param_span: field_span,
                                arg_span: checker.pool.expr_span(base_id),
                                arg_index: 0,
                            },
                        );
                    } else {
                        super::super::super::method::validate_exclusive_receiver_autoref(
                            checker,
                            base_id,
                            receiver_ty_id,
                            actual_base_ty_id,
                        );
                    }
                    let explicit_params = &params[1..];
                    let arg_ids = checker.pool.expr_list(args_range).to_vec();
                    if explicit_params.len() != arg_ids.len() {
                        let struct_id = match checker.resolve(actual_base_ty_id) {
                            ArType::Named(id, _) => Some(id),
                            _ => None,
                        };
                        let struct_name = struct_id.map_or_else(
                            || "Struct".to_string(),
                            |id| checker.symbols.get(id).name.to_string(),
                        );
                        let diag = crate::Diagnostic::error(
                            crate::DiagCode::T012WrongArgCount,
                            format!(
                                "method '{struct_name}.{field_name}' expects {} argument(s), found {}",
                                explicit_params.len(),
                                arg_ids.len()
                            ),
                            span,
                        )
                        .with_label(field_span, "call target is here")
                        .with_label(span, format!("{} arguments provided", arg_ids.len()));
                        checker.diagnostics.push(diag);
                    }
                    for (i, arg_id) in arg_ids.iter().copied().enumerate() {
                        let expected_id = explicit_params.get(i).copied();
                        let arg_ty_id = synth_expr_expected(checker, arg_id, expected_id);
                        if let Some(expected_id) = expected_id {
                            check_call_arg(
                                checker,
                                arg_id,
                                expected_id,
                                arg_ty_id,
                                span,
                                field_span,
                                checker.pool.expr_span(arg_id),
                                i + 1,
                            );
                        }
                    }
                    // Instantiated method Func type on both the Generic node
                    // and the Field selector (`obj.m`). Without typing the
                    // Field, HIR lower falls back to Error and fails
                    // validate_invariants (mono method path).
                    let func_ty = ArType::func(&params, ret, &checker.type_info.type_interner);
                    let params_id = checker.intern(func_ty);
                    checker.record_expr_type(gen_callee_id, params_id);
                    checker.record_expr_type(callee_id, params_id);
                    // Bind method symbol for HIR (namespace Path rewrite / mono).
                    let struct_id = match checker.resolve(actual_base_ty_id) {
                        ArType::Vector(shape) => checker.symbols.vector_type_symbol(shape),
                        ArType::Named(id, _) => Some(id),
                        ArType::Ptr(inner) => match checker.resolve(inner) {
                            ArType::Named(id, _) => Some(id),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(struct_id) = struct_id
                        && let Some(sym) = checker
                            .symbols
                            .lookup_associated_member(struct_id, &field_name)
                    {
                        Arc::make_mut(&mut checker.resolved).value_ref(field_span, sym);
                    }
                    return std::ops::ControlFlow::Break(Some(ret));
                }
            }
        }
    }
    std::ops::ControlFlow::Continue(())
}
