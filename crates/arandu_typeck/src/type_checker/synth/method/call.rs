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

/// The receiver of a method call: an expression (`x.len()`), or a named
/// value written where a type could stand (`GREETING.len()` on a `static`
/// or `const`), which has no expression of its own.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Receiver {
    Expr(ExprId),
    Value {
        symbol: arandu_middle::SymbolId,
        span: arandu_lexer::Span,
    },
}

impl Receiver {
    fn span(self, checker: &TypeChecker<'_>) -> arandu_lexer::Span {
        match self {
            Self::Expr(expr) => checker.pool.expr_span(expr),
            Self::Value { span, .. } => span,
        }
    }
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker))]
pub(crate) fn synth_method_call(
    checker: &mut TypeChecker<'_>,
    receiver: Receiver,
    callee: ExprId,
    method: &str,
    field_span: arandu_lexer::Span,
    args: IndexRange,
    call_span: arandu_lexer::Span,
) -> Option<TypeId> {
    // If `base` is a namespace module path (`io.foo`), this is not a method
    // call — let the Call path handle namespace members. Returning `Some(Error)`
    // here previously poisoned `io.println(...)` and skipped argument typing.
    if let Receiver::Expr(base) = receiver
        && let ExprKind::Path { path } = checker.pool.expr(base)
        && path.len() == 1
        && checker
            .symbols
            .lookup_module(checker.symbols.global_scope(), path[0].as_str())
            .is_some()
    {
        return None;
    }

    let mut base_ty_id = match receiver {
        Receiver::Expr(base) => synth_expr(checker, base),
        Receiver::Value { symbol, span } => {
            if checker.symbols.get(symbol).kind == arandu_middle::SymbolKind::StaticMut
                && !checker.ctx.is_in_unsafe()
            {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::O017StaticMutRequiresUnsafe,
                    "access to `static mut` requires an `unsafe` block",
                    span,
                ));
            }
            checker
                .decl_type_id(symbol)
                .unwrap_or_else(|| checker.intern(ArType::Error))
        }
    };
    if checker.resolve(base_ty_id).is_error() {
        // Receiver already failed to type; avoid cascading "no method" noise.
        return Some(checker.intern(ArType::Error));
    }
    // An array receiver has the methods of its slice (`arr.len()`), viewed
    // as `[]T` the way Rust auto-unsizes a method receiver.
    if let ArType::Array(_, element) = checker.resolve(base_ty_id)
        && checker.symbols.builtins.slice_type.is_some_and(|slice| {
            checker
                .symbols
                .lookup_associated_member(slice, method)
                .is_some()
        })
    {
        let slice = checker.intern(ArType::Slice(element));
        if let Receiver::Expr(base) = receiver
            && let Some(adjusted) =
                super::super::expr::adjust_unsize(checker, base, base_ty_id, slice)
        {
            base_ty_id = adjusted;
        }
    }

    // Peel `&` / `&mut` / `ptr` so `ref value: T` still resolves methods and
    // `T: Interface` constraints. Do NOT silently peel `Nullable` (`T?`):
    // calling a pointee method on `T?` without `?.` is rejected below with T006
    // (only inherent `ptr` methods on `ptr[T]?`, such as `.isNull()`, remain allowed).
    let (actual_base_ty_id, peeled_ptr) = {
        let mut id = base_ty_id;
        let mut peeled_ptr = false;
        for _ in 0..4 {
            match checker.resolve(id) {
                ArType::Ref(inner) | ArType::RefMut(inner) => {
                    id = inner;
                }
                ArType::Ptr(inner) => {
                    peeled_ptr = true;
                    id = inner;
                }
                _ => break,
            }
        }
        (id, peeled_ptr)
    };

    let has_user_to_str = {
        let base_resolved = checker.resolve(actual_base_ty_id);
        let struct_id = match &base_resolved {
            ArType::Named(id, _) => Some(*id),
            ArType::Ptr(inner) => match checker.resolve(*inner) {
                ArType::Named(id, _) => Some(id),
                _ => None,
            },
            _ => None,
        };
        struct_id.is_some_and(|id| {
            checker
                .symbols
                .lookup_associated_member(id, method)
                .is_some()
        })
    };

    // ToStr intrinsic: `receiver.to_str()` or `receiver.toStr()` with zero args → `str`.
    // Only intercepts when the receiver does not define its own associated method.
    if (method == "to_str" || method == "toStr") && !has_user_to_str {
        let arg_ids = checker.pool.expr_list(args).to_vec();
        if !arg_ids.is_empty() {
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::T012WrongArgCount,
                    format!(
                        "method '{method}' expects 0 argument(s), found {}",
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
        let ret_id = super::super::expr::text::string_type(checker).unwrap_or(str_id);
        if base_ty.is_to_str_v01() {
            let func_ty = ArType::func(
                &[actual_base_ty_id],
                ret_id,
                &checker.type_info.type_interner,
            );
            let func_id = checker.intern(func_ty);
            checker.record_expr_type(callee, func_id);
            return Some(ret_id);
        }
        let interner = &checker.type_info.type_interner;
        let found = base_ty.display(&checker.symbols, interner);
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T034CannotFormat,
                format!("cannot format value of type `{found}` as `str`"),
                receiver.span(checker),
            )
            .with_note(
                "only bool, integers, floats, char, str, String, and types implementing `toStr` are supported".to_string(),
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
        ArType::Primitive(Primitive::I32) => checker.symbols.builtins.i32_type,
        ArType::Primitive(Primitive::I64) => checker.symbols.builtins.i64_type,
        ArType::Primitive(Primitive::Int) | ArType::IntLiteral => checker.symbols.builtins.int_type,
        ArType::Primitive(Primitive::Float) | ArType::FloatLiteral => {
            checker.symbols.builtins.float_type
        }
        ArType::Slice(_) | ArType::MutSlice(_) => checker.symbols.builtins.slice_type,
        ArType::Ptr(_) => checker.symbols.builtins.ptr_type,
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
    // A pointer reaches the methods of what it points at first, and then
    // the inherent methods of `ptr` itself (`p.offsetFrom(q)`).
    let lookup = |id: arandu_middle::SymbolId| checker.symbols.lookup_associated_member(id, method);
    let pointer_method = struct_id
        .or(builtin_id)
        .is_none_or(|owner| lookup(owner).is_none())
        && checker
            .symbols
            .pointer_method(
                &checker.resolve(base_ty_id),
                &checker.type_info.type_interner,
                method,
            )
            .is_some();
    let (struct_id, effective_id) = if pointer_method {
        (None, checker.symbols.builtins.ptr_type)
    } else {
        (struct_id, struct_id.or(builtin_id))
    };

    let method_sym = effective_id.and_then(lookup);
    let is_inherent_ptr_method = method_sym.is_some()
        && effective_id.is_some()
        && effective_id == checker.symbols.builtins.ptr_type;
    if peeled_ptr
        && !is_inherent_ptr_method
        && !checker.ctx.is_in_unsafe()
        && (method_sym.is_some() || callable_field(checker, actual_base_ty_id, method).is_some())
    {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::O019RawPtrRequiresUnsafe,
                format!(
                    "calling method '{method}' through a raw pointer requires an `unsafe` block"
                ),
                call_span,
            )
            .with_label(
                field_span,
                "dereferencing `ptr[T]` for a method call is unsafe",
            ),
        );
    }

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
    if resolved_method.is_none()
        && method_sym.is_none()
        && method == "cmp"
        && base_resolved.is_orderable()
        && let Some(ord_enum) = checker
            .symbols
            .lang_item_symbol(arandu_middle::symbol_table::LangItem::Ordering)
    {
        let base_id = checker.intern(base_resolved.clone());
        let ref_base = checker.intern(ArType::Ref(base_id));
        let ord_ret = checker.intern(ArType::named(
            ord_enum,
            &[],
            &checker.type_info.type_interner,
        ));
        resolved_method = Some(ArType::func(
            &[ref_base, ref_base],
            ord_ret,
            &checker.type_info.type_interner,
        ));
    }
    if resolved_method.is_none() && effective_id.is_none() {
        return None;
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
    } else if let Some(field) = callable_field(checker, actual_base_ty_id, method) {
        // `job.run(x)` with no method `run` calls the field `run` when it
        // holds a function or a closure, as Swift and Kotlin do. Naming the
        // field on the callee tells lowering this is not a method call.
        Arc::make_mut(&mut checker.resolved).expr_ref(callee, field);
        return None;
    } else {
        // If `base` is `Option<T>` (`T?`), check whether the missing method belongs
        // to `T` so we emit a clear `T006` suggesting `?.` instead of `T018`.
        if let ArType::Option(inner_id) = base_resolved {
            let mut inner_peeled = inner_id;
            for _ in 0..4 {
                match checker.resolve(inner_peeled) {
                    ArType::Ref(i) | ArType::RefMut(i) | ArType::Ptr(i) => inner_peeled = i,
                    _ => break,
                }
            }
            let inner_has_member = match checker.resolve(inner_peeled) {
                ArType::Named(id, _) => checker
                    .symbols
                    .lookup_associated_member(id, method)
                    .is_some(),
                _ => false,
            };
            if inner_has_member {
                let interner = &checker.type_info.type_interner;
                let ty_disp = base_resolved.display(&checker.symbols, interner);
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T006NotNullable,
                        format!("cannot call method '{method}' on nullable type '{ty_disp}'"),
                        field_span,
                    )
                    .with_label(receiver.span(checker), format!("this has type '{ty_disp}'"))
                    .with_hint(format!(
                        "use safe access `?.{method}(...)` or make the value non-nullable"
                    )),
                );
                return Some(checker.intern(ArType::Error));
            }
        }
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
                receiver.span(checker),
                format!("receiver has type `{ty_disp}`"),
            )
            .with_note(
                "if this is a method from another module, ensure it is declared `public`"
                    .to_string(),
            ),
        );
        return Some(checker.intern(ArType::Error));
    };

    // Preserve the uninstantiated receiver formal so methods that redeclare receiver
    // type parameters (`func Vec.filter<T>(...) where T: Copy`) can bind their own
    // `SymbolId` for `T` (distinct from the struct's `T`) when typing callbacks.
    let orig_receiver_formal = params.first().copied();

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
            if pointer_method {
                base_ty_id
            } else {
                actual_base_ty_id
            },
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
                arg_span: receiver.span(checker),
                arg_index: 0,
            },
        );
    } else {
        if let Receiver::Expr(base) = receiver {
            validate_exclusive_receiver_autoref(checker, base, receiver_ty_id, base_ty_id);
        }
    }

    let mut explicit_params = params[1..].to_vec();
    let arg_ids = checker.pool.expr_list(args).to_vec();
    if explicit_params.len() != arg_ids.len() {
        let struct_name = effective_id
            .map(|id| checker.symbols.get(id).name.clone())
            .unwrap_or_else(|| {
                base_resolved
                    .display(&checker.symbols, &checker.type_info.type_interner)
                    .into()
            });
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
        let diag = crate::type_checker::synth::expr::note_trailing_block(
            checker,
            diag,
            &arg_ids,
            explicit_params.len(),
        );
        checker.diagnostics.push(diag);
    }

    if !method_generic_params.is_empty() && explicit_params.len() == arg_ids.len() {
        let mut receiver_bindings = (|| {
            let params = checker
                .type_info
                .generic_params
                .get(&effective_id?)?
                .clone();
            let actual = checker.resolve(actual_base_ty_id);
            let args: Vec<crate::type_checker::types::TypeId> = match actual {
                ArType::Named(_, args) => checker.type_info.type_interner.type_args(args).to_vec(),
                ArType::Option(inner) => vec![inner],
                ArType::Result(ok, err) => vec![ok, err],
                ArType::Range(inner) => vec![inner],
                ArType::Slice(inner) | ArType::MutSlice(inner) => vec![inner],
                _ => return None,
            };
            (args.len() == params.len())
                .then(|| params.iter().copied().zip(args).collect::<Vec<_>>())
        })()
        .unwrap_or_default();

        if let Some(orig_recv) = orig_receiver_formal {
            let mut peeled_formal = orig_recv;
            let mut peeled_actual = actual_base_ty_id;
            for _ in 0..4 {
                match checker.resolve(peeled_formal) {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        peeled_formal = inner;
                    }
                    _ => break,
                }
            }
            for _ in 0..4 {
                match checker.resolve(peeled_actual) {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        peeled_actual = inner;
                    }
                    _ => break,
                }
            }
            let mut candidate_gp = Vec::new();
            if let Some(sym) = method_sym_recorded
                && let Some(gp) = checker.type_info.generic_params.get(&sym)
            {
                candidate_gp.extend(gp.iter().copied());
            }
            if let Some(gp) = effective_id.and_then(|id| checker.type_info.generic_params.get(&id))
            {
                for &p in gp.iter() {
                    if !candidate_gp.contains(&p) {
                        candidate_gp.push(p);
                    }
                }
            }
            let mut bound_syms = Vec::new();
            let mut bound_tys = Vec::new();
            super::bind_receiver_type_arg(
                checker,
                peeled_formal,
                peeled_actual,
                &candidate_gp,
                &mut bound_syms,
                &mut bound_tys,
            );
            for (sym, ty) in bound_syms.into_iter().zip(bound_tys) {
                if !receiver_bindings
                    .iter()
                    .any(|(existing, _)| *existing == sym)
                {
                    let tid = checker.intern(ty);
                    receiver_bindings.push((sym, tid));
                }
            }
        }

        let concrete_args: Vec<crate::type_checker::types::TypeId> =
            match checker.resolve(actual_base_ty_id) {
                ArType::Result(ok, err) => vec![ok, err],
                ArType::Option(inner) => vec![inner],
                ArType::Slice(elem) | ArType::MutSlice(elem) | ArType::Array(_, elem) => vec![elem],
                ArType::Ptr(inner) => vec![inner],
                _ => Vec::new(),
            };
        if let Some(sym) = method_sym_recorded
            && let Some(gp) = checker.type_info.generic_params.get(&sym)
        {
            let n = gp.len().min(concrete_args.len());
            for i in 0..n {
                if !receiver_bindings.iter().any(|(s, _)| *s == gp[i]) {
                    receiver_bindings.push((gp[i], concrete_args[i]));
                }
            }
        }
        let arg_tys = super::super::expr::synth_args_with_callbacks(
            checker,
            &method_generic_params,
            &explicit_params,
            &arg_ids,
            &receiver_bindings,
        );
        super::super::expr::check_callback_args(checker, &explicit_params, &arg_ids, &arg_tys);
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
        let arg_ty_id = super::super::expr::synth_call_arg(checker, arg_id, expected_id);
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

/// The symbol of the field `name` of the struct `owner_ty` when the field is
/// callable: a function value or an owning closure.
fn callable_field(checker: &TypeChecker<'_>, owner_ty: TypeId, name: &str) -> Option<SymbolId> {
    let ArType::Named(owner, args) = checker.resolve(owner_ty) else {
        return None;
    };
    let args = checker.type_info.type_interner.type_args(args);
    let field = checker.type_info.fields_for(owner, &args)?.get(name)?;
    let symbol = field.symbol?;
    let ty = checker.type_info.field_type_for(owner, &args, name)?;
    let callable = matches!(checker.resolve(ty), ArType::Func(..))
        || super::super::expr::owning_closure_code(checker, ty).is_some();
    callable.then_some(symbol)
}
