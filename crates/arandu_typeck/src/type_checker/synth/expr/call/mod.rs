//! Call expression synthesis (callee resolution, free/method/generic calls).

mod arg;
mod instantiate;

pub(crate) use arg::check_call_arg;
pub(crate) use instantiate::infer_and_instantiate_func;

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
            // Bare `Result.Ok` / `Result.Err` as paths (not calls): build a
            // polymorphic-looking Func using expected `Result<T,E>` when present.
            // Actual calls are handled by `synth_result_ctor` (bidirectional).
            if types::type_name_base(type_name) == "Result" {
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
            if types::type_name_base(type_name) == "Option" {
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
            if types::type_name_base(type_name) == "Poll" {
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
                        let var_name = &checker.symbols.get(var_id).name;
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
                    let enum_ty =
                        ArType::named(*enum_symbol_id, &[], &checker.type_info.type_interner);
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
            trailing_block: _,
        } => {
            let callee_id = *callee;
            let args_range = *args;
            if let Some(callee_sym) = checker.resolved.expr_symbol(callee_id) {
                if let Some(&eff) = checker.type_info.function_effects.get(&callee_sym) {
                    checker.current_observed_effects = checker.current_observed_effects.union(eff);
                }
                let sym = checker.symbols.get(callee_sym);
                if sym.kind == arandu_middle::SymbolKind::ExternFunc {
                    checker.current_observed_effects = checker
                        .current_observed_effects
                        .union(arandu_middle::EffectFlags::FOREIGN);
                    // Intrinsics that only inspect already-valid fat-pointer descriptors
                    // or produce compile-time constants are safe without `unsafe {}`.
                    let is_safe_intrinsic = arandu_middle::IntrinsicKind::from_name(&sym.name)
                        .is_some_and(|k| k.is_safe());
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
                if Some(callee_sym) == checker.symbols.builtin_alloc {
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
                if Some(callee_sym) == checker.symbols.builtin_free {
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
            }
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
            if let Some(poll_ty) = synth_poll_ctor(checker, callee_id, args_range, span) {
                return Some(checker.intern(poll_ty));
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
                    let arg_ids = checker.pool.expr_list(args_range).to_vec();
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
                                checker, &gp, &params, ret, &arg_tys, expected, span,
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
                            let receiver_ok = checker.unify_ids(receiver_ty_id, actual_base_ty_id)
                                || match checker.resolve(receiver_ty_id) {
                                    ArType::Ref(inner) | ArType::RefMut(inner) => {
                                        checker.unify_ids(inner, actual_base_ty_id)
                                    }
                                    _ => match checker.resolve(actual_base_ty_id) {
                                        ArType::Ref(inner) | ArType::RefMut(inner) => {
                                            checker.unify_ids(receiver_ty_id, inner)
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
                                checker.resolved.value_ref(field_span, sym);
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
                            }
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
                            && let Some(sym_id) =
                                checker.symbols.lookup_module_member(&path[0], field)
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
                                checker.resolved.expr_ref(current_callee, sym_id);
                            }
                        }
                    }
                    _ => {}
                }

                if let Some(sym_id) = callee_func_sym
                    && let Some(&eff) = checker.type_info.function_effects.get(&sym_id)
                {
                    checker.current_observed_effects = checker.current_observed_effects.union(eff);
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
                        checker, &gp, &params, ret, &arg_tys, expected, span,
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
