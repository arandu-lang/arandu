//! Call expression synthesis (callee resolution, free/method/generic calls).

mod arg;
mod instantiate;

pub(crate) use arg::check_call_arg;
pub(crate) use instantiate::infer_and_instantiate_func;

use std::sync::Arc;

use arandu_lexer::Span;
use arandu_parser::CatchHandler;
use arandu_parser::ast_pool::{ExprId, ExprKind};

use super::{synth_expr, synth_expr_expected};
use crate::type_checker::TypeChecker;
use crate::type_checker::constraints::ConstraintOrigin;
use crate::type_checker::synth::{
    resolve_field, resolve_index, resolve_namespace_field, resolve_namespace_member_type,
    synth_method_call, synth_option_ctor, synth_poll_ctor, synth_result_ctor,
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
        ExprKind::Call {
            callee,
            args,
            trailing_block,
        } => {
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
            if let Some(result_ty) =
                synth_result_ctor(checker, callee_id, args_range, span, expected)
            {
                return Some(checker.intern(result_ty));
            }
            if let Some(option_ty) =
                synth_option_ctor(checker, callee_id, args_range, span, expected)
            {
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
                if let Some(&eff) = checker.type_info.function_effects.get(&callee_sym) {
                    checker.current_observed_effects = checker.current_observed_effects.union(eff);
                }
                let sym = checker.symbols.get(callee_sym);
                if sym.kind == arandu_middle::SymbolKind::ExternFunc {
                    let pure_bits = arandu_middle::IntrinsicKind::from_name(&sym.name)
                        .is_some_and(|kind| kind.is_pure_bit_operation())
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
                    let is_safe_intrinsic = arandu_middle::IntrinsicKind::from_name(&sym.name)
                        .is_some_and(|k| k.is_safe() && (!k.is_pure_bit_operation() || pure_bits))
                        || pure_simd;
                    if !is_safe_intrinsic && !checker.ctx.is_in_unsafe() {
                        checker.diagnostics.push(
                            crate::Diagnostic::error(
                                crate::DiagCode::O013ExternRequiresUnsafe,
                                "call to extern function requires an `unsafe` block",
                                span,
                            )
                            .with_label(span, "`extern` functions are unsafe and must be called inside an `unsafe` block"),
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
                        check_target_thread_capabilities(checker, sym_id, span);
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
                        return synth_thread_spawn_closure(
                            checker, expr, sym_id, callee_id, arg_ids[0], span,
                        );
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
                        ) && let Some(gp) =
                            checker.type_info.generic_params.get(&sym_id).cloned()
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
                        return Some(ret);
                    }
                }

                if let Some(ret_id) = synth_method_call(
                    checker, base_id, callee_id, &field_str, field_span, args_range, span,
                ) {
                    return Some(ret_id);
                }
            }
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
                                super::super::method::validate_exclusive_receiver_autoref(
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
                            let func_ty =
                                ArType::func(&params, ret, &checker.type_info.type_interner);
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
                            return Some(ret);
                        }
                    }
                }
            }

            let callee_ty_id = synth_expr(checker, callee_id);
            let arg_ids = checker.pool.expr_list(args_range).to_vec();
            let callee_ty = checker.resolve(callee_ty_id);
            let func_info = if let ArType::Func(ref params, ret) = callee_ty {
                Some((checker.type_info.type_interner.type_args(*params), ret))
            } else {
                None
            };
            let is_error = matches!(callee_ty, ArType::Error);

            Some(if let Some((mut params, mut ret)) = func_info {
                let mut is_direct = false;
                let mut current_callee = callee_id;
                let had_explicit_generic =
                    matches!(checker.pool.expr(callee_id), ExprKind::Generic { .. });
                if let ExprKind::Generic { callee: inner, .. } = checker.pool.expr(current_callee) {
                    current_callee = *inner;
                }
                let mut callee_func_sym = None;
                match checker.pool.expr(current_callee) {
                    ExprKind::Path { .. } => {
                        let mut found_direct = false;
                        if let Some(sym_id) = checker.resolved.expr_symbol(current_callee) {
                            let sym = checker.symbols.get(sym_id);
                            if matches!(
                                sym.kind,
                                arandu_middle::SymbolKind::Func
                                    | arandu_middle::SymbolKind::ExternFunc
                                    | arandu_middle::SymbolKind::EnumVariant
                                    | arandu_middle::SymbolKind::AssociatedFunc
                            ) {
                                is_direct = true;
                                callee_func_sym = Some(sym_id);
                                found_direct = true;
                            }
                        }
                        if !found_direct
                            && let ExprKind::Path { path } = checker.pool.expr(current_callee)
                            && let [module, member] = path.as_slice()
                            && let Some(sym_id) =
                                checker.symbols.lookup_module_member(module, member)
                        {
                            is_direct = true;
                            callee_func_sym = Some(sym_id);
                        }
                    }
                    ExprKind::TypePath { .. } => {
                        // Any TypePath that resolves to a Func type is a static constructor or associated function.
                        // It cannot be a local variable or field. Thus, if it's callable, it's a direct call.
                        is_direct = true;
                        if let Some(sym_id) = checker.resolved.expr_symbol(current_callee) {
                            callee_func_sym = Some(sym_id);
                        }
                    }
                    // `mem.sizeOf` / `io.println` / `rt.spawn` after Generic unwrap:
                    // Field on a module path.
                    ExprKind::Field { base, field } => {
                        if let ExprKind::Path { path } = checker.pool.expr(*base)
                            && path.len() == 1
                            && let Some(sym_id) = checker
                                .resolved
                                .expr_symbol(current_callee)
                                .or_else(|| checker.symbols.lookup_module_member(&path[0], field))
                        {
                            let kind = checker.symbols.get(sym_id).kind;
                            if matches!(
                                kind,
                                arandu_middle::SymbolKind::Func
                                    | arandu_middle::SymbolKind::ExternFunc
                                    | arandu_middle::SymbolKind::AssociatedFunc
                            ) {
                                is_direct = true;
                                callee_func_sym = Some(sym_id);
                                Arc::make_mut(&mut checker.resolved)
                                    .expr_ref(current_callee, sym_id);
                            }
                        }
                    }
                    _ => {}
                }

                if let Some(sym_id) = callee_func_sym {
                    check_target_thread_capabilities(checker, sym_id, span);
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
                                span,
                                "the function's documented preconditions must be upheld by its caller",
                            ),
                        );
                    }
                    if let Some(&eff) = checker.type_info.function_effects.get(&sym_id) {
                        checker.current_observed_effects =
                            checker.current_observed_effects.union(eff);
                    }
                }

                if let Some(sym_id) = callee_func_sym
                    && checker.symbols.get(sym_id).lang_item
                        == Some(arandu_middle::symbol_table::LangItem::ThreadSpawn)
                    && arg_ids.len() == 1
                    && matches!(checker.pool.expr(arg_ids[0]), ExprKind::Lambda { .. })
                {
                    return synth_thread_spawn_closure(
                        checker, expr, sym_id, callee_id, arg_ids[0], span,
                    );
                }

                // Infer type args for bare `id(x)` (no `id<T>`): instantiate formal params
                // before arg checking so `T` becomes the concrete argument type.
                if !had_explicit_generic
                    && let Some(sym_id) = callee_func_sym
                    && let Some(gp) = checker.type_info.generic_params.get(&sym_id).cloned()
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
                        let func_ty = ArType::func(&params, ret, &checker.type_info.type_interner);
                        let inst_func = checker.intern(func_ty);
                        checker.record_expr_type(callee_id, inst_func);
                    }
                }

                if !is_direct {
                    let diag = crate::Diagnostic::error(
                        crate::DiagCode::T033IndirectCallNotSupported,
                        "indirect function calls are not supported",
                        span,
                    )
                    .with_label(
                        checker.pool.expr_span(callee_id),
                        "this expression evaluates to a function, but Arandu only supports direct calls",
                    );
                    checker.diagnostics.push(diag);
                }

                if params.len() != arg_ids.len() {
                    let diag = crate::Diagnostic::error(
                        crate::DiagCode::T012WrongArgCount,
                        format!(
                            "expected {} arguments, found {}",
                            params.len(),
                            arg_ids.len()
                        ),
                        span,
                    )
                    .with_label(checker.pool.expr_span(callee_id), "call target is here")
                    .with_label(span, format!("{} arguments provided", arg_ids.len()));
                    checker.diagnostics.push(diag);
                }
                for (i, arg_id) in arg_ids.iter().copied().enumerate() {
                    let formal = params.get(i).copied();
                    let arg_ty_id = synth_expr_expected(checker, arg_id, formal);
                    if let Some(param_id) = formal {
                        check_call_arg(
                            checker,
                            arg_id,
                            param_id,
                            arg_ty_id,
                            span,
                            checker.pool.expr_span(callee_id),
                            checker.pool.expr_span(arg_id),
                            i,
                        );
                    }
                }

                if !is_direct {
                    checker.intern(ArType::Error)
                } else {
                    ret
                }
            } else if is_error {
                checker.intern(ArType::Error)
            } else {
                let callee_ty = checker.resolve(callee_ty_id);
                let interner = &checker.type_info.type_interner;
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T003IncompatibleCallArg,
                    format!(
                        "cannot call non-function type '{}'",
                        callee_ty.display(&checker.symbols, interner)
                    ),
                    span,
                )
                .with_label(
                    checker.pool.expr_span(callee_id),
                    format!(
                        "this has type '{}'",
                        callee_ty.display(&checker.symbols, interner)
                    ),
                );
                checker.diagnostics.push(diag);
                checker.intern(ArType::Error)
            })
        }
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

pub(crate) fn check_target_thread_capabilities(
    checker: &mut TypeChecker<'_>,
    sym_id: arandu_middle::SymbolId,
    span: Span,
) {
    if !checker.type_info.target_identity.arch.starts_with("wasm") {
        return;
    }
    let Some(lang_item) = checker.symbols.get(sym_id).lang_item else {
        return;
    };
    match lang_item {
        arandu_middle::symbol_table::LangItem::ThreadParkTimeout => {
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::U001FeatureNotSupported,
                    "`parkTimeout` requires target thread support to wake on unpark, which is unavailable on wasm32; use `std.thread.sleep` for a non-interruptible delay",
                    span,
                )
                .with_label(
                    span,
                    "`parkTimeout` cannot be used on WebAssembly without threads",
                ),
            );
        }
        arandu_middle::symbol_table::LangItem::ThreadPark
        | arandu_middle::symbol_table::LangItem::ThreadUnpark
        | arandu_middle::symbol_table::LangItem::ThreadSpawn => {
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::U001FeatureNotSupported,
                    "thread synchronization and spawning are unsupported on single-threaded WebAssembly targets; use cooperative tasks or async execution instead",
                    span,
                )
                .with_label(
                    span,
                    "thread operations are unsupported on this target",
                ),
            );
        }
        _ => {}
    }
}

fn span_contains(outer: Span, inner: Span) -> bool {
    outer.file_id == inner.file_id && outer.start <= inner.start && outer.end >= inner.end
}

fn walk_expr_ids(pool: &arandu_parser::ast_pool::AstPool, expr: ExprId, out: &mut Vec<ExprId>) {
    out.push(expr);
    match pool.expr(expr) {
        ExprKind::Path { .. }
        | ExprKind::TypePath { .. }
        | ExprKind::Int { .. }
        | ExprKind::Float { .. }
        | ExprKind::Bool { .. }
        | ExprKind::Char { .. }
        | ExprKind::Byte { .. }
        | ExprKind::Nil
        | ExprKind::Error => {}
        ExprKind::Field { base, .. }
        | ExprKind::SafeField { base, .. }
        | ExprKind::Unary { expr: base, .. }
        | ExprKind::Alloc { expr: base, .. }
        | ExprKind::Try { expr: base, .. }
        | ExprKind::Cast { expr: base, .. }
        | ExprKind::Group { expr: base }
        | ExprKind::ArrayRepeat { value: base, .. }
        | ExprKind::Generic { callee: base, .. } => {
            walk_expr_ids(pool, *base, out);
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NullCoalesce { left, right, .. }
        | ExprKind::Index {
            base: left,
            index: right,
        }
        | ExprKind::SafeIndex {
            base: left,
            index: right,
        } => {
            walk_expr_ids(pool, *left, out);
            walk_expr_ids(pool, *right, out);
        }
        ExprKind::Call {
            callee,
            args,
            trailing_block,
        } => {
            walk_expr_ids(pool, *callee, out);
            for &arg in pool.expr_list(*args) {
                walk_expr_ids(pool, arg, out);
            }
            if let Some(tb) = trailing_block {
                walk_block(pool, pool.block(*tb), out);
            }
        }
        ExprKind::Array { items } => {
            for &item in pool.expr_list(*items) {
                walk_expr_ids(pool, item, out);
            }
        }
        ExprKind::StructLiteral { fields, .. } => {
            for &field in pool.field_init_list(*fields) {
                walk_expr_ids(pool, pool.field_init(field).value, out);
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            match condition {
                arandu_parser::Condition::Expr { expr: e, .. }
                | arandu_parser::Condition::Is { expr: e, .. } => {
                    walk_expr_ids(pool, *e, out);
                }
                arandu_parser::Condition::And { conditions, .. } => {
                    for c in conditions.iter() {
                        if let arandu_parser::Condition::Expr { expr: e, .. }
                        | arandu_parser::Condition::Is { expr: e, .. } = c
                        {
                            walk_expr_ids(pool, *e, out);
                        }
                    }
                }
            }
            walk_block(pool, pool.block(*then_block), out);
            walk_block(pool, pool.block(*else_block), out);
        }
        ExprKind::Match { value, arms } => {
            walk_expr_ids(pool, *value, out);
            for &arm in pool.match_arm_list(*arms) {
                let arm_data = pool.match_arm(arm);
                match &arm_data.body {
                    arandu_parser::MatchArmBody::Expr { expr: e, .. } => {
                        walk_expr_ids(pool, *e, out)
                    }
                    arandu_parser::MatchArmBody::Block { block: b, .. } => walk_block(pool, b, out),
                }
            }
        }
        ExprKind::Catch {
            expr: base,
            handler,
        } => {
            walk_expr_ids(pool, *base, out);
            match pool.catch_handler(*handler) {
                arandu_parser::CatchHandler::Expr { expr: e, .. } => walk_expr_ids(pool, *e, out),
                arandu_parser::CatchHandler::Block { block: b, .. } => walk_block(pool, b, out),
            }
        }
        ExprKind::Lambda { .. } => {}
        _ => {}
    }
}

fn walk_block(
    pool: &arandu_parser::ast_pool::AstPool,
    block: &arandu_parser::Block,
    out: &mut Vec<ExprId>,
) {
    for &stmt_id in pool.stmt_list(block.statements) {
        match pool.stmt(stmt_id) {
            arandu_parser::Stmt::Expr { expr, .. } | arandu_parser::Stmt::Free { expr, .. } => {
                walk_expr_ids(pool, *expr, out);
            }
            arandu_parser::Stmt::VarDecl { value, .. } | arandu_parser::Stmt::Set { value, .. } => {
                walk_expr_ids(pool, *value, out);
            }
            arandu_parser::Stmt::Return { values, .. } => {
                for v in values {
                    walk_expr_ids(pool, *v, out);
                }
            }
            arandu_parser::Stmt::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                match condition {
                    arandu_parser::Condition::Expr { expr: e, .. }
                    | arandu_parser::Condition::Is { expr: e, .. } => {
                        walk_expr_ids(pool, *e, out);
                    }
                    arandu_parser::Condition::And { conditions, .. } => {
                        for c in conditions.iter() {
                            if let arandu_parser::Condition::Expr { expr: e, .. }
                            | arandu_parser::Condition::Is { expr: e, .. } = c
                            {
                                walk_expr_ids(pool, *e, out);
                            }
                        }
                    }
                }
                walk_block(pool, then_block, out);
                if let Some(eb) = else_block {
                    walk_block(pool, eb, out);
                }
            }
            arandu_parser::Stmt::While {
                condition, body, ..
            } => {
                if let arandu_parser::Condition::Expr { expr: e, .. }
                | arandu_parser::Condition::Is { expr: e, .. } = condition
                {
                    walk_expr_ids(pool, *e, out);
                }
                walk_block(pool, body, out);
            }
            arandu_parser::Stmt::For { clause, body, .. } => {
                match clause {
                    arandu_parser::ForClause::In { iterable, .. } => {
                        walk_expr_ids(pool, *iterable, out);
                    }
                    arandu_parser::ForClause::CStyle { condition, .. } => {
                        if let Some(c) = condition {
                            walk_expr_ids(pool, *c, out);
                        }
                    }
                }
                walk_block(pool, body, out);
            }
            arandu_parser::Stmt::Match { expr, .. } => {
                walk_expr_ids(pool, *expr, out);
            }
            _ => {}
        }
    }
}

pub(crate) fn synth_thread_spawn_closure(
    checker: &mut TypeChecker<'_>,
    call_expr: ExprId,
    spawn_sym: arandu_middle::SymbolId,
    callee_id: ExprId,
    arg_id: ExprId,
    _span: Span,
) -> Option<TypeId> {
    let arg_span = checker.pool.expr_span(arg_id);
    let (params, body, is_move) = match checker.pool.expr(arg_id) {
        ExprKind::Lambda {
            params,
            body,
            is_move,
        } => (*params, body.clone(), *is_move),
        _ => unreachable!(),
    };

    if !is_move {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::U001FeatureNotSupported,
                "thread closure requires explicit 'move || ...' to transfer captured values into the new thread",
                arg_span,
            )
            .with_label(arg_span, "missing 'move' keyword on thread closure"),
        );
        return Some(checker.intern(ArType::Error));
    }

    if !params.is_empty() {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::U001FeatureNotSupported,
                "thread closure cannot take parameters; use 'move || ...'",
                arg_span,
            )
            .with_label(arg_span, "thread closure must take no parameters"),
        );
        return Some(checker.intern(ArType::Error));
    }

    // Inspect captured symbols
    let mut expr_ids = Vec::new();
    match &body {
        arandu_parser::LambdaBody::Expr {
            expr: inner_expr, ..
        } => {
            walk_expr_ids(checker.pool, *inner_expr, &mut expr_ids);
        }
        arandu_parser::LambdaBody::Block { block, .. } => {
            walk_block(checker.pool, block, &mut expr_ids);
        }
    }

    for eid in expr_ids {
        if let Some(sym_id) = checker.resolved.expr_symbol(eid) {
            let (is_captured_var, sym_name) = {
                let sym = checker.symbols.get(sym_id);
                (
                    matches!(
                        sym.kind,
                        arandu_middle::SymbolKind::Local | arandu_middle::SymbolKind::Param
                    ) && !span_contains(arg_span, sym.span),
                    sym.name.clone(),
                )
            };
            if is_captured_var {
                let expr_span = checker.pool.expr_span(eid);
                let cap_ty_id = checker
                    .ctx
                    .lookup(sym_id)
                    .or_else(|| checker.decl_type_id(sym_id))
                    .unwrap_or_else(|| checker.intern(ArType::Error));
                let cap_ty = checker.resolve(cap_ty_id);
                if matches!(cap_ty, ArType::Ref(_) | ArType::RefMut(_)) {
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T025InterfaceNotSatisfied,
                            "cannot move borrowed value into thread closure: references do not satisfy 'marker.Send' and cannot escape to another thread",
                            expr_span,
                        )
                        .with_label(expr_span, "reference cannot cross thread boundaries"),
                    );
                } else if !cap_ty.is_error()
                    && !crate::type_checker::types::transfer::satisfies(
                        checker,
                        &cap_ty,
                        arandu_middle::symbol_table::LangItem::Send,
                    )
                {
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::T025InterfaceNotSatisfied,
                            format!(
                                "captured variable '{}' of type '{}' does not satisfy 'marker.Send'",
                                sym_name,
                                cap_ty.display(&checker.symbols, &checker.type_info.type_interner)
                            ),
                            expr_span,
                        )
                        .with_label(expr_span, "non-Send type cannot cross thread boundaries"),
                    );
                }
            }
        }
    }

    // Synthesize the closure body to determine return type
    let ret_ty_id = match &body {
        arandu_parser::LambdaBody::Expr {
            expr: inner_expr, ..
        } => super::synth_expr(checker, *inner_expr),
        arandu_parser::LambdaBody::Block { block, .. } => {
            let b_ty = crate::type_checker::check::check_block(checker, checker.pool, block);
            checker.intern(b_ty)
        }
    };
    let ret_ty = checker.resolve(ret_ty_id);
    if !ret_ty.is_error()
        && !crate::type_checker::types::transfer::satisfies(
            checker,
            &ret_ty,
            arandu_middle::symbol_table::LangItem::Send,
        )
    {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T025InterfaceNotSatisfied,
                format!(
                    "thread return type '{}' does not satisfy 'marker.Send'",
                    ret_ty.display(&checker.symbols, &checker.type_info.type_interner)
                ),
                arg_span,
            )
            .with_label(arg_span, "thread return type must satisfy 'marker.Send'"),
        );
    }

    let generic_params = checker
        .type_info
        .generic_params
        .get(&spawn_sym)
        .cloned()
        .unwrap_or_default();
    let decl_ty = checker.decl_type(spawn_sym);
    let res_ty = if let (Some(ArType::Func(_, ret_ty)), Some(&first_gp)) =
        (decl_ty, generic_params.first())
    {
        let ret = checker.resolve(ret_ty);
        let subst = arandu_middle::types::build_subst(&[first_gp], &[checker.resolve(ret_ty_id)]);
        let inst =
            arandu_middle::types::substitute_type(&ret, &subst, &checker.type_info.type_interner);
        checker.intern(inst)
    } else {
        checker.intern(ArType::Error)
    };

    let lambda_ty = ArType::func(&[], ret_ty_id, &checker.type_info.type_interner);
    let lambda_ty_id = checker.intern(lambda_ty);
    checker.record_expr_type(arg_id, lambda_ty_id);
    let call_func_ty = ArType::func(&[lambda_ty_id], res_ty, &checker.type_info.type_interner);
    let call_func_ty_id = checker.intern(call_func_ty);
    checker.record_expr_type(callee_id, call_func_ty_id);
    checker.record_expr_type(call_expr, res_ty);

    checker.diagnostics.push(
        crate::Diagnostic::error(
            crate::DiagCode::U001FeatureNotSupported,
            "thread closures ('move || ...') require AMIR ThreadJob lowering, which is not yet available; implement ThreadJob statically for now",
            arg_span,
        )
        .with_label(arg_span, "closure lowering for thread.spawn is pending AMIR support"),
    );
    Some(checker.intern(ArType::Error))
}
