//! Method call synthesis, auto-ref / auto-deref, and receiver generic instantiation.

use arandu_middle::SymbolId;
use arandu_parser::ast_pool::{ExprId, ExprKind, IndexRange};

use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::{ArType, TypeId};
use super::expr::synth_expr;

/// Reject an implicit exclusive borrow of an immutable value receiver.
///
/// A binding whose value is already `mut ref T` does not need to be mutable:
/// mutability belongs to the reference. This check only covers the auto-ref
/// conversion from a bare `T` to a method receiver declared as `mut ref T`.
pub(crate) fn validate_exclusive_receiver_autoref(
    checker: &mut TypeChecker<'_>,
    base: ExprId,
    formal: TypeId,
    bare_actual: TypeId,
) {
    let ArType::RefMut(inner) = checker.resolve(formal) else {
        return;
    };
    if !checker.unify_ids(inner, bare_actual) {
        return;
    }

    let Some(symbol_id) = receiver_root_symbol(checker, base) else {
        return;
    };
    if checker
        .decl_type_id(symbol_id)
        .is_some_and(|ty| matches!(checker.resolve(ty), ArType::RefMut(_)))
    {
        return;
    }
    let symbol = checker.symbols.get(symbol_id);
    if !matches!(
        symbol.kind,
        crate::SymbolKind::Local | crate::SymbolKind::Param
    ) || checker.resolved.mutable_symbols.contains(&symbol_id)
    {
        return;
    }

    let name = &symbol.name;
    checker.diagnostics.push(
        crate::Diagnostic::error(
            crate::DiagCode::T026CannotAssignImmutable,
            format!("cannot mutably borrow immutable variable '{name}'"),
            checker.pool.expr_span(base),
        )
        .with_label(
            checker.pool.expr_span(base),
            "exclusive method receiver is required here",
        )
        .with_label(symbol.span, "variable declared here as immutable")
        .with_hint_replacement(crate::Hint {
            message: format!("consider declaring the variable as mutable: `mut {name} = ...;`"),
            replacement: Some(crate::CodeReplacement {
                span: symbol.span,
                new_text: format!("mut {name}"),
            }),
        }),
    );
}

fn receiver_root_symbol(checker: &TypeChecker<'_>, expr: ExprId) -> Option<SymbolId> {
    match checker.pool.expr(expr) {
        ExprKind::Path { .. } => checker.resolved.expr_symbol(expr),
        ExprKind::Field { base, .. }
        | ExprKind::SafeField { base, .. }
        | ExprKind::Index { base, .. }
        | ExprKind::SafeIndex { base, .. }
        | ExprKind::Group { expr: base } => receiver_root_symbol(checker, *base),
        _ => None,
    }
}

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

    // Built-in `Result` / `Option` methods (`expectOrAbort`) live under the type
    // symbol in `associated_members`. Resolve their SymbolId from the prelude.
    let global_scope = checker.symbols.global_scope();
    let builtin_id: Option<SymbolId> = match &base_resolved {
        ArType::Result(_, _) => checker.symbols.lookup_type(global_scope, "Result"),
        ArType::Option(_) => checker.symbols.lookup_type(global_scope, "Option"),
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
    } else if let Some(sym) = method_sym
        && let Some(gp) = checker.type_info.generic_params.get(&sym)
    {
        if let Some(sid) = struct_id
            && let Some(struct_gp) = checker.type_info.generic_params.get(&sid)
        {
            if gp.len() >= struct_gp.len() {
                method_generic_params = gp[struct_gp.len()..].to_vec();
            } else {
                method_generic_params = gp.to_vec();
            }
        } else {
            method_generic_params = gp.to_vec();
        }
    }

    let (params, ret, method_sym_recorded) = if let Some(method_sig) = resolved_method {
        if let ArType::Func(params, ret) = method_sig {
            let params = checker.type_info.type_interner.type_args(params);
            // Interface methods may declare an explicit `self`/`Self` receiver or
            // only the free-style payload (`Allocator.alloc(size, align)`).
            // Drop a leading `Self` formal if present, then always prepend the
            // concrete receiver so call sites stay uniform (TYP.2).
            let payload = if params
                .first()
                .is_some_and(|&p| is_receiver_type_formal(checker, p, actual_base_ty_id))
            {
                params[1..].to_vec()
            } else {
                params
            };
            let mut new_params = Vec::with_capacity(payload.len() + 1);
            new_params.push(actual_base_ty_id);
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
    let receiver_ok = checker.unify_ids(receiver_ty_id, actual_base_ty_id)
        || match checker.resolve(receiver_ty_id) {
            // Auto-ref: method/`self` formal is `&T`/`&mut T`, receiver is `T`.
            ArType::Ref(inner) | ArType::RefMut(inner) => {
                checker.unify_ids(inner, actual_base_ty_id)
            }
            _ => match checker.resolve(actual_base_ty_id) {
                // Auto-deref: formal `T`, receiver is `&T`/`&mut T`.
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
                call_span,
                param_span: field_span,
                arg_span: checker.pool.expr_span(base),
                arg_index: 0,
            },
        );
    } else {
        validate_exclusive_receiver_autoref(checker, base, receiver_ty_id, actual_base_ty_id);
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
            .map(|aid| super::expr::synth_expr(checker, aid))
            .collect();
        if let Some((ip, ir)) = super::expr::infer_and_instantiate_func(
            checker,
            &method_generic_params,
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
        let arg_ty_id = super::expr::synth_expr_expected(checker, arg_id, expected_id);
        if let Some(expected_id) = expected_id {
            super::expr::check_call_arg(
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
        checker.resolved.value_ref(field_span, sym);
    }
    let func_ty = ArType::func(&params, ret, &checker.type_info.type_interner);
    let func_id = checker.intern(func_ty);
    checker.record_expr_type(callee, func_id);

    Some(ret)
}

/// Instantiate `Result.expectOrAbort` / `Option.expectOrAbort` from a builtin
/// `ArType::Result` / `Option` receiver (not a Named type).
fn instantiate_method_sig_for_result_option(
    checker: &mut TypeChecker<'_>,
    actual_base_ty_id: TypeId,
    params: Vec<TypeId>,
    ret: TypeId,
    method_sym: Option<arandu_middle::SymbolId>,
) -> (Vec<TypeId>, TypeId) {
    use crate::type_checker::types::{build_subst, substitute_type};

    let concrete_args: Vec<ArType> = match checker.resolve(actual_base_ty_id) {
        ArType::Result(ok, err) => vec![checker.resolve(ok), checker.resolve(err)],
        ArType::Option(inner) => vec![checker.resolve(inner)],
        _ => return (params, ret),
    };

    let Some(sym) = method_sym else {
        // No generic map: still force receiver to the concrete Result/Option type.
        if params.is_empty() {
            return (params, ret);
        }
        let mut new_params = params;
        new_params[0] = actual_base_ty_id;
        return (new_params, ret);
    };

    let Some(gp) = checker.type_info.generic_params.get(&sym).cloned() else {
        if params.is_empty() {
            return (params, ret);
        }
        let mut new_params = params;
        new_params[0] = actual_base_ty_id;
        return (new_params, ret);
    };

    let n = gp.len().min(concrete_args.len());
    if n == 0 {
        return (params, ret);
    }
    let subst = build_subst(&gp[..n], &concrete_args[..n]);
    let new_params: Vec<TypeId> = params
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            if i == 0 {
                return actual_base_ty_id;
            }
            let ty = checker.resolve(p);
            let inst = substitute_type(&ty, &subst, &checker.type_info.type_interner);
            checker.intern(inst)
        })
        .collect();
    let ret_ty = checker.resolve(ret);
    let new_ret = checker.intern(substitute_type(
        &ret_ty,
        &subst,
        &checker.type_info.type_interner,
    ));
    (new_params, new_ret)
}

/// Substitute struct type parameters in a method signature using the concrete
/// receiver type (`BoxG<int>` → replace `T` with `int` in params/return).
fn instantiate_method_sig_for_receiver(
    checker: &mut TypeChecker<'_>,
    struct_id: arandu_middle::SymbolId,
    actual_base_ty_id: TypeId,
    params: Vec<TypeId>,
    ret: TypeId,
    method_sym: Option<arandu_middle::SymbolId>,
) -> (Vec<TypeId>, TypeId) {
    use crate::type_checker::types::{build_subst, substitute_type};

    // Peel ptr/ref so specialization works when the receiver is already a ref
    // (or when only the formal is `&T` after auto-ref at the call site).
    let mut base_id = actual_base_ty_id;
    for _ in 0..4 {
        match checker.resolve(base_id) {
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                base_id = inner;
            }
            _ => break,
        }
    }
    let (recv_args_ids, recv_args) = match checker.resolve(base_id) {
        ArType::Named(id, args) if id == struct_id => {
            let arg_vec = checker.type_info.type_interner.type_args(args);
            (
                arg_vec.clone(),
                arg_vec
                    .iter()
                    .map(|&a| checker.resolve(a))
                    .collect::<Vec<_>>(),
            )
        }
        _ => return (params, ret),
    };
    if recv_args.is_empty() {
        return (params, ret);
    }

    let key_sym = method_sym.unwrap_or(struct_id);
    let cache_key = (key_sym, recv_args_ids);
    if let Some(cached) = checker.type_info.variant_instantiations.get(&cache_key) {
        return cached.clone();
    }

    // Prefer method-level generic_params prefix (struct params first), else struct params.
    let param_syms: Vec<arandu_middle::SymbolId> = if let Some(sym) = method_sym
        && let Some(gp) = checker.type_info.generic_params.get(&sym)
    {
        let n = recv_args.len().min(gp.len());
        gp.iter().copied().take(n).collect()
    } else if let Some(gp) = checker.type_info.generic_params.get(&struct_id) {
        gp.iter().copied().take(recv_args.len()).collect()
    } else {
        return (params, ret);
    };
    if param_syms.len() != recv_args.len() {
        return (params, ret);
    }

    let subst = build_subst(&param_syms, &recv_args);
    let new_params: Vec<TypeId> = params
        .iter()
        .map(|&pid| {
            let ty = checker.resolve(pid);
            let inst = substitute_type(&ty, &subst, &checker.type_info.type_interner);
            checker.intern(inst)
        })
        .collect();
    let ret_ty = checker.resolve(ret);
    let ret_inst = substitute_type(&ret_ty, &subst, &checker.type_info.type_interner);
    let new_ret = checker.intern(ret_inst);
    let res = (new_params, new_ret);
    checker
        .type_info
        .variant_instantiations
        .insert(cache_key, res.clone());
    res
}

/// True when a formal matches the receiver type (or a ref/deref of it).
fn is_receiver_type_formal(checker: &TypeChecker<'_>, tid: TypeId, base_id: TypeId) -> bool {
    if checker.unify_ids(tid, base_id) {
        return true;
    }
    match checker.resolve(tid) {
        ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
            checker.unify_ids(inner, base_id) || is_receiver_type_formal(checker, inner, base_id)
        }
        _ => false,
    }
}

pub(super) fn contains_generic_params(
    ty: &arandu_middle::types::ArType,
    gp: &[arandu_middle::SymbolId],
    interner: &arandu_middle::types::TypeInterner,
) -> bool {
    use arandu_middle::types::ArType;
    match ty {
        ArType::Named(id, args) => {
            if gp.contains(id) {
                return true;
            }
            interner
                .type_args(*args)
                .iter()
                .any(|&a| contains_generic_params(&interner.resolve(a), gp, interner))
        }
        ArType::Func(params, ret) => {
            interner
                .type_args(*params)
                .iter()
                .any(|&p| contains_generic_params(&interner.resolve(p), gp, interner))
                || contains_generic_params(&interner.resolve(*ret), gp, interner)
        }
        ArType::Nullable(inner)
        | ArType::Slice(inner)
        | ArType::Ptr(inner)
        | ArType::Ref(inner)
        | ArType::RefMut(inner)
        | ArType::Option(inner)
        | ArType::Coroutine(inner)
        | ArType::Poll(inner)
        | ArType::Range(inner)
        | ArType::Array(_, inner) => {
            contains_generic_params(&interner.resolve(*inner), gp, interner)
        }
        ArType::Result(ok, err) => {
            contains_generic_params(&interner.resolve(*ok), gp, interner)
                || contains_generic_params(&interner.resolve(*err), gp, interner)
        }
        ArType::Tuple(tys) => interner
            .type_args(*tys)
            .iter()
            .any(|&t| contains_generic_params(&interner.resolve(t), gp, interner)),
        _ => false,
    }
}
