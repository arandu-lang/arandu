use std::sync::Arc;

use arandu_middle::SymbolId;
use arandu_parser::ast_pool::{ExprId, ExprKind, IndexRange};

use super::super::super::TypeChecker;
use super::super::super::constraints::ConstraintOrigin;
use super::super::super::types::{ArType, Primitive, TypeId};
use super::super::expr::synth_expr;
use super::sig::instantiate_method_sig_for_receiver;
use super::{
    contains_generic_params, instantiate_method_sig_for_result_option, is_receiver_type_formal,
    validate_exclusive_receiver_autoref,
};

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker))]
pub(crate) fn synth_method_call(
    checker: &mut TypeChecker<'_>,
    base: ExprId,
    callee: ExprId,
    method: &str,
    field_span: arandu_lexer::Span,
    args: IndexRange,
    call_span: arandu_lexer::Span,
) -> Option<TypeId> {
    // If `base` is a namespace module path (`io.foo`), this is not a method
    // call — let the Call path handle namespace members. Returning `Some(Error)`
    // here previously poisoned `io.println(...)` and skipped argument typing.
    if let ExprKind::Path { path } = checker.pool.expr(base)
        && path.len() == 1
        && checker
            .symbols
            .lookup_module(checker.symbols.global_scope(), path[0].as_str())
            .is_some()
    {
        return None;
    }

    let base_ty_id = synth_expr(checker, base);
    if checker.resolve(base_ty_id).is_error() {
        // Receiver already failed to type; avoid cascading "no method" noise.
        return Some(checker.intern(ArType::Error));
    }

    // Peel Nullable / & / &mut / ptr so `shared value: T` (typed as `&T`) still
    // resolves methods and `T: Interface` constraints (PROMOTE-L1 family).
    let actual_base_ty_id = {
        let mut id = match checker.resolve(base_ty_id) {
            ArType::Nullable(inner) => inner,
            _ => base_ty_id,
        };
        for _ in 0..4 {
            match checker.resolve(id) {
                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                    id = inner;
                }
                _ => break,
            }
        }
        id
    };

    // ToStr v0.1 intrinsic: `receiver.to_str()` with zero args → `str`.
    if method == "to_str" {
        let arg_ids = checker.pool.expr_list(args).to_vec();
        if !arg_ids.is_empty() {
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!(
                        "method 'to_str' expects 0 argument(s), found {}",
                        arg_ids.len()
                    ),
                    call_span,
                )
                .with_label(field_span, "call target is here"),
            );
            return Some(checker.intern(ArType::Error));
        }
        let base_ty = checker.resolve(actual_base_ty_id);
        let str_id = checker.intern(ArType::Primitive(
            crate::type_checker::types::Primitive::Str,
        ));
        if base_ty.is_to_str_v01() {
            let func_ty = ArType::func(
                &[actual_base_ty_id],
                str_id,
                &checker.type_info.type_interner,
            );
            let func_id = checker.intern(func_ty);
            checker.record_expr_type(callee, func_id);
            return Some(str_id);
        }
        let interner = &checker.type_info.type_interner;
        let found = base_ty.display(&checker.symbols, interner);
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T034CannotFormat,
                format!("cannot format value of type `{found}` as `str`"),
                checker.pool.expr_span(base),
            )
            .with_note(
                "only bool, integers, floats, char, and str are supported in v0.1".to_string(),
            )
            .with_label(field_span, "to_str is not available for this type"),
        );
        return Some(checker.intern(ArType::Error));
    }

    let base_resolved = checker.resolve(actual_base_ty_id);

    // Built-in `Result` / `Option` / primitive methods live under the type
    // symbol in `associated_members`. Resolve their SymbolId from the prelude.
    let builtin_id: Option<SymbolId> = match &base_resolved {
        ArType::Vector(shape) => checker.symbols.vector_type_symbol(*shape),
        ArType::Result(_, _) => checker.symbols.builtins.result,
        ArType::Option(_) => checker.symbols.builtins.option,
        ArType::Primitive(Primitive::Str) => checker.symbols.builtins.str_type,
        ArType::Primitive(Primitive::Char) => checker.symbols.builtins.char_type,
        ArType::Primitive(Primitive::U32) => checker.symbols.builtins.u32_type,
        ArType::Primitive(Primitive::U64) => checker.symbols.builtins.u64_type,
        ArType::Primitive(Primitive::Int) | ArType::IntLiteral => checker.symbols.builtins.int_type,
        ArType::Primitive(Primitive::Float) | ArType::FloatLiteral => {
            checker.symbols.builtins.float_type
        }
        ArType::Slice(_) | ArType::MutSlice(_) => checker.symbols.builtins.slice_type,
        ArType::Ptr(_) => checker.symbols.builtins.ptr_type,
        ArType::Nullable(inner) if matches!(checker.resolve(*inner), ArType::Ptr(_)) => {
            checker.symbols.builtins.ptr_type
        }
        _ => None,
    };

    let struct_id = match &base_resolved {
        ArType::Named(id, _) => Some(*id),
        ArType::Ptr(inner) => match checker.resolve(*inner) {
            ArType::Named(id, _) => Some(id),
            _ => None,
        },
        _ => None,
    };

    // Effective parent SymbolId for the method lookup.
    let effective_id = struct_id.or(builtin_id)?;

    let method_sym = checker
        .symbols
        .lookup_associated_member(effective_id, method);

    let mut resolved_method = None;
    let mut method_generic_params = Vec::new();
    if method_sym.is_none()
        && let Some(sid) = struct_id
        && let Some(constraints) = checker.type_info.param_constraints.get(&sid)
    {
        for bound in constraints.iter() {
            if let Some(iface_info) = checker.type_info.interfaces.get(&bound.iface_sym)
                && let Some(m) = iface_info.methods.iter().find(|m| m.name == method)
            {
                let raw_sig = checker.resolve(m.sig_id);
                let inst_sig = crate::type_checker::types::interfaces::instantiate_interface_method(
                    checker,
                    bound.iface_sym,
                    &bound.type_args,
                    &base_resolved,
                    &raw_sig,
                );
                resolved_method = Some(inst_sig);
                method_generic_params = m.generic_params.clone();
                break;
            }
        }
    }

    let (params, ret, method_sym_recorded) = if let Some(method_sig) = resolved_method {
        if let ArType::Func(params, ret) = method_sig {
            let params = checker.type_info.type_interner.type_args(params);
            // Interface methods may declare an explicit `self`/`Self` receiver or
            // only the free-style payload (`Allocator.alloc(size, align)`).
            // Preserve the declared receiver formal (e.g. `ref Self`) if present,
            // or prepend `actual_base_ty_id` if omitted.
            let (receiver_formal, payload) = if let Some(&first) = params.first()
                && is_receiver_type_formal(checker, first, actual_base_ty_id)
            {
                (first, params[1..].to_vec())
            } else {
                (actual_base_ty_id, params)
            };
            let mut new_params = Vec::with_capacity(payload.len() + 1);
            new_params.push(receiver_formal);
            new_params.extend(payload);
            (new_params, ret, None)
        } else {
            return None;
        }
    } else if let Some(sym) = method_sym {
        let method_ty = checker.decl_type(sym)?;
        let (params, ret) = match &method_ty {
            ArType::Func(params, ret) => (checker.type_info.type_interner.type_args(*params), *ret),
            _ => return None,
        };
        if let Some(gp) = checker.type_info.generic_params.get(&sym) {
            if let Some(&first_param) = params.first() {
                let first_ty = checker.resolve(first_param);
                let interner = &checker.type_info.type_interner;
                method_generic_params = gp
                    .iter()
                    .copied()
                    .filter(|&p| !contains_generic_params(&first_ty, &[p], interner))
                    .collect();
            } else {
                method_generic_params = gp.to_vec();
            }
        }
        (params, ret, Some(sym))
    } else {
        // Root fix: missing method (including private methods not present in
        // the import export table) must diagnose here. Returning `None` let the
        // Call path fall through without a reliable diagnostic.
        let interner = &checker.type_info.type_interner;
        let ty_disp = base_resolved.display(&checker.symbols, interner);
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T018UndefinedField,
                format!("no method `{method}` on type `{ty_disp}`"),
                field_span,
            )
            .with_label(
                checker.pool.expr_span(base),
                format!("receiver has type `{ty_disp}`"),
            )
            .with_note(
                "if this is a method from another module, ensure it is declared `public`"
                    .to_string(),
            ),
        );
        return Some(checker.intern(ArType::Error));
    };

    // Instantiate template method type with the receiver's concrete type args
    // so `BoxG<int>.get` sees `Func([BoxG<int>], int)` not `Func([BoxG<T>], T)`.
    // For Result/Option, substitute T/E from the builtin type shape.
    let (mut params, mut ret) = if let Some(sid) = struct_id {
        instantiate_method_sig_for_receiver(
            checker,
            sid,
            actual_base_ty_id,
            params,
            ret,
            method_sym_recorded,
            call_span,
        )
    } else {
        instantiate_method_sig_for_result_option(
            checker,
            actual_base_ty_id,
            params,
            ret,
            method_sym_recorded,
        )
    };

    if params.is_empty() {
        return None;
    }

    let receiver_ty_id = params[0];
    let receiver_ok = checker.is_assignable(base_ty_id, receiver_ty_id)
        || match checker.resolve(receiver_ty_id) {
            // Auto-ref: method/`self` formal is `&T`/`&mut T`, receiver is `T`.
            ArType::Ref(inner) | ArType::RefMut(inner) => checker.is_assignable(base_ty_id, inner),
            _ => match checker.resolve(base_ty_id) {
                // Auto-deref: formal `T`, receiver is `&T`/`&mut T`. Only permitted if `T` is a Copy type
                // to prevent consuming/moving ownership out of a borrowed reference (soundness hole).
                ArType::Ref(inner) | ArType::RefMut(inner) => {
                    checker.type_info.is_copy(inner) && checker.is_assignable(inner, receiver_ty_id)
                }
                _ => false,
            },
        };
    if !receiver_ok {
        checker.add_constraint(
            receiver_ty_id,
            base_ty_id,
            ConstraintOrigin::CallArg {
                call_span,
                param_span: field_span,
                arg_span: checker.pool.expr_span(base),
                arg_index: 0,
            },
        );
    } else {
        validate_exclusive_receiver_autoref(checker, base, receiver_ty_id, base_ty_id);
    }

    let mut explicit_params = params[1..].to_vec();
    let arg_ids = checker.pool.expr_list(args).to_vec();
    if explicit_params.len() != arg_ids.len() {
        let struct_name = checker.symbols.get(effective_id).name.clone();
        let diag = crate::Diagnostic::error(
            crate::DiagCode::T012WrongArgCount,
            format!(
                "method '{struct_name}.{method}' expects {} argument(s), found {}",
                explicit_params.len(),
                arg_ids.len()
            ),
            call_span,
        )
        .with_label(field_span, "call target is here")
        .with_label(call_span, format!("{} arguments provided", arg_ids.len()));
        checker.diagnostics.push(diag);
    }

    if !method_generic_params.is_empty() && explicit_params.len() == arg_ids.len() {
        let arg_tys: Vec<TypeId> = arg_ids
            .iter()
            .copied()
            .map(|aid| super::super::expr::synth_expr(checker, aid))
            .collect();
        if let Some((ip, ir)) = super::super::expr::infer_and_instantiate_func(
            checker,
            (method_sym_recorded, &method_generic_params),
            &explicit_params,
            ret,
            &arg_tys,
            None,
            call_span,
        ) {
            let mut new_params = Vec::with_capacity(ip.len() + 1);
            new_params.push(params[0]);
            new_params.extend(ip);
            params = new_params;
            explicit_params = params[1..].to_vec();
            ret = ir;
        }
    }

    for (i, arg_id) in arg_ids.iter().copied().enumerate() {
        let expected_id = explicit_params.get(i).copied();
        let arg_ty_id = super::super::expr::synth_expr_expected(checker, arg_id, expected_id);
        if let Some(expected_id) = expected_id {
            super::super::expr::check_call_arg(
                checker,
                arg_id,
                expected_id,
                arg_ty_id,
                call_span,
                field_span,
                checker.pool.expr_span(arg_id),
                i + 1,
            );
        }
    }

    if let Some(sym) = method_sym_recorded {
        Arc::make_mut(&mut checker.resolved).value_ref(field_span, sym);
        if super::super::expr::check_target_thread_capabilities(checker, sym, call_span) {
            let err = checker.intern(ArType::Error);
            checker.record_expr_type(callee, err);
            return Some(err);
        }
        if checker.type_info.unsafe_functions.contains(&sym) && !checker.ctx.is_in_unsafe() {
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::O013ExternRequiresUnsafe,
                    "call to an `@Unsafe` function requires an `unsafe` block",
                    call_span,
                )
                .with_label(
                    field_span,
                    "the function's documented preconditions must be upheld by its caller",
                ),
            );
        }
        if let Some(&eff) = checker.type_info.function_effects.get(&sym) {
            checker.current_observed_effects = checker.current_observed_effects.union(eff);
        }
    }
    let func_ty = ArType::func(&params, ret, &checker.type_info.type_interner);
    let func_id = checker.intern(func_ty);
    checker.record_expr_type(callee, func_id);

    Some(ret)
}
