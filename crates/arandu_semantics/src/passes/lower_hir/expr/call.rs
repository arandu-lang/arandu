use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprKind};
use crate::passes::type_checker::types::Primitive;
use arandu_middle::types::ArType;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

pub(super) fn lower_call(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExpr, Diagnostic> {
    let ExprKind::Call { callee, args, .. } = pool.expr(expr) else {
        unreachable!();
    };
    if let Some(scope) =
        super::temporary::lower_call_with_temporaries(type_check, pool, hir_pool, expr, span)?
    {
        return Ok(scope);
    }
    let kind = {
        {
            let callee_id = *callee;
            let arg_ids = pool.expr_list(*args);
            if let Some(callee_sym) = super::get_resolved_value_ref(type_check, callee_id)
                && type_check.symbols.is_alloc_func(callee_sym)
                && let Some(&arg) = arg_ids.first()
            {
                let inner_id = super::lower_expr(type_check, pool, hir_pool, arg)?;
                let kind = HirExprKind::Alloc { expr: inner_id };
                let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                return Ok(HirExpr { kind, ty, span });
            }
            if let Some(variant) = super::builtin_ctor_variant(type_check, pool, callee_id) {
                if arg_ids.len() == 1 {
                    let value_id = super::lower_expr(type_check, pool, hir_pool, arg_ids[0])?;
                    let kind = HirExprKind::ResultCtor {
                        variant,
                        value: value_id,
                    };
                    let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                    return Ok(HirExpr { kind, ty, span });
                }
                // Unit ctors: `Poll.Pending` / `Option.None` — dummy value ignored by AMIR.
                if arg_ids.is_empty()
                    && matches!(
                        variant,
                        crate::hir::ResultCtorVariant::PollPending
                            | crate::hir::ResultCtorVariant::None
                    )
                {
                    let kind = super::dummy_unit_ctor_kind(hir_pool, variant, span);
                    let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                    return Ok(HirExpr { kind, ty, span });
                }
            }
            // ToStr intrinsic: `receiver.to_str()` or `receiver.toStr()` → HirExprKind::ToStr
            // unless the receiver struct defines its own associated method.
            if arg_ids.is_empty()
                && let ExprKind::Field { base, field, .. } | ExprKind::SafeField { base, field, .. } =
                    pool.expr(callee_id)
                && (field == "to_str" || field == "toStr")
                && !type_check
                    .type_info
                    .expr_type_id(*base)
                    .is_some_and(|ty| super::text::has_user_to_str_method(type_check, ty, field))
            {
                let value_id = super::lower_expr(type_check, pool, hir_pool, *base)?;
                let to_str_kind = HirExprKind::ToStr { value: value_id };
                let owned = super::text::own_type(type_check, pool, expr)
                    .is_some_and(|ty| super::text::is_string(type_check, ty));
                let kind = if owned {
                    super::text::string_from(type_check, hir_pool, to_str_kind, span).ok_or_else(
                        || {
                            Diagnostic::ice(
                                crate::DiagCode::ICEL001,
                                "`String.from` is missing",
                                span,
                            )
                        },
                    )?
                } else {
                    to_str_kind
                };
                let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                return Ok(HirExpr { kind, ty, span });
            }
            if let Some(kind) =
                lower_owning_closure_call(type_check, pool, hir_pool, callee_id, arg_ids)?
            {
                let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                return Ok(HirExpr { kind, ty, span });
            }
            if let Some(kind) = lower_callback_call(type_check, pool, hir_pool, callee_id, arg_ids)?
            {
                let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
                return Ok(HirExpr { kind, ty, span });
            }
            // `GREETING.len()`: a `static` or `const` named like a type is
            // the receiver of the method the type checker resolved.
            if let Some(kind) = value_receiver_call(type_check, pool, hir_pool, callee_id, arg_ids)?
            {
                return Ok(HirExpr {
                    ty: super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty),
                    kind,
                    span,
                });
            }
            // Method receiver is not in the AST arg list. Prepend it for
            // `obj.m(...)` and generic `obj.m<T>(...)` so AMIR/mono see
            // (self, …) matching the method Func type.
            let method_base = {
                let field_callee = match pool.expr(callee_id) {
                    ExprKind::Field { .. } | ExprKind::SafeField { .. } => Some(callee_id),
                    ExprKind::Generic { callee: inner, .. } => match pool.expr(*inner) {
                        ExprKind::Field { .. } | ExprKind::SafeField { .. } => Some(*inner),
                        _ => None,
                    },
                    _ => None,
                };
                // `job.run(x)` calling a field holds no receiver.
                let calls_field = type_check
                    .resolved
                    .expr_symbol(callee_id)
                    .and_then(|symbol| type_check.symbols.try_get(symbol))
                    .is_some_and(|symbol| symbol.kind == arandu_middle::SymbolKind::Field);
                field_callee
                    .filter(|_| !calls_field)
                    .and_then(|fc| match pool.expr(fc) {
                        ExprKind::Field { base, field, .. }
                        | ExprKind::SafeField { base, field, .. } => {
                            if super::lookup_namespace_field(pool, *base, field, type_check)
                                .is_some()
                            {
                                None
                            } else {
                                Some(*base)
                            }
                        }
                        _ => None,
                    })
            };
            let direct_callee_span = match pool.expr(callee_id) {
                ExprKind::Generic { callee: inner, .. } => pool.expr_span(*inner),
                _ => pool.expr_span(callee_id),
            };
            let expects_mut_self = method_base.is_some()
                && type_check
                    .type_info
                    .expr_type_id(callee_id)
                    .is_some_and(|tid| {
                        let interner = &type_check.type_info.type_interner;
                        match interner.resolve(tid) {
                            ArType::Func(params, _) => interner
                                .type_args(params)
                                .first()
                                .is_some_and(|&p| matches!(interner.resolve(p), ArType::RefMut(_))),
                            _ => false,
                        }
                    });
            let peel_group = |mut e: ExprId| {
                while let ExprKind::Group { expr: inner, .. } = pool.expr(e) {
                    e = *inner;
                }
                e
            };
            let mut_trait_index_base = method_base.filter(|&b| {
                expects_mut_self && matches!(pool.expr(peel_group(b)), ExprKind::Index { .. })
            });
            let prev_suppress = hir_pool
                .suppress_export_thunk_span
                .replace(direct_callee_span);
            let callee_res = if let Some(mut_base) = mut_trait_index_base
                && let ExprKind::Field { base: _, field } = pool.expr(callee_id)
                && let Some(field_expr_ty) = type_check.type_info.expr_type_id(callee_id)
            {
                let base_expr_id = peel_group(mut_base);
                let kind = super::access::lower_index_with_mut(
                    type_check,
                    pool,
                    hir_pool,
                    base_expr_id,
                    true,
                )?;
                let base_ty = type_check
                    .type_info
                    .expr_type_id(base_expr_id)
                    .unwrap_or(fallback_ty);
                let base_vid = hir_pool.alloc_expr(HirExpr {
                    kind,
                    ty: base_ty,
                    span: pool.expr_span(base_expr_id),
                });
                Ok(hir_pool.alloc_expr(HirExpr {
                    kind: HirExprKind::Field {
                        base: base_vid,
                        field: field.clone(),
                    },
                    ty: field_expr_ty,
                    span: direct_callee_span,
                }))
            } else {
                super::lower_expr(type_check, pool, hir_pool, callee_id)
            };
            hir_pool.suppress_export_thunk_span = prev_suppress;
            let mut callee_vid = callee_res?;
            // A called generic path stays a path: the call instantiates it.
            if matches!(pool.expr(callee_id), ExprKind::Path { .. })
                && let HirExprKind::Generic { callee: path, .. } = hir_pool.expr(callee_vid).kind
            {
                callee_vid = path;
            }
            let mut hir_args = Vec::new();
            if let Some(base_id) = method_base {
                if let Some(mut_base) = mut_trait_index_base {
                    let base_expr_id = peel_group(mut_base);
                    let kind = super::access::lower_index_with_mut(
                        type_check,
                        pool,
                        hir_pool,
                        base_expr_id,
                        true,
                    )?;
                    let base_ty = type_check
                        .type_info
                        .expr_type_id(base_expr_id)
                        .unwrap_or(fallback_ty);
                    hir_args.push(hir_pool.alloc_expr(HirExpr {
                        kind,
                        ty: base_ty,
                        span: pool.expr_span(base_expr_id),
                    }));
                } else {
                    hir_args.push(super::lower_expr(type_check, pool, hir_pool, base_id)?);
                }
            }
            for &arg_id in arg_ids {
                hir_args.push(super::lower_expr(type_check, pool, hir_pool, arg_id)?);
            }
            if let Some(callee_sym) =
                resolve_call_symbol(type_check, pool, hir_pool, callee_id, callee_vid)
            {
                let formal_params =
                    callee_param_types(type_check, hir_pool, callee_sym, callee_vid);
                while hir_args.len() < formal_params.len() {
                    let param_idx = hir_args.len();
                    let Some(default) = type_check
                        .type_info
                        .func_param_defaults
                        .get(&(callee_sym, param_idx))
                        .cloned()
                    else {
                        break;
                    };
                    let formal_ty = formal_params[param_idx];
                    let def_id = materialize_param_default(
                        type_check, pool, hir_pool, callee_sym, default, formal_ty, span,
                    )?;
                    hir_args.push(def_id);
                }
            }
            let args_range = hir_pool.alloc_expr_list(&hir_args);
            HirExprKind::Call {
                callee: callee_vid,
                args: args_range,
            }
        }
    };
    let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
    Ok(HirExpr { kind, ty, span })
}

/// `f(args)` on an implicit callback parameter `f: F` is `F.call(f, args)`.
/// Monomorphization replaces `F` with the closure struct, so the call is direct.
fn lower_callback_call(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    callee_id: ExprId,
    arg_ids: &[ExprId],
) -> Result<Option<HirExprKind>, Diagnostic> {
    let Some(callee_ty) = type_check.type_info.expr_type_id(callee_id) else {
        return Ok(None);
    };
    let interner = &type_check.type_info.type_interner;
    let callee = match interner.resolve(callee_ty) {
        ArType::Ref(inner) | ArType::RefMut(inner) => interner.resolve(inner),
        other => other,
    };
    let ArType::Named(implicit, _) = callee else {
        return Ok(None);
    };
    let Some(&bound) = type_check.type_info.callback_bounds.get(&implicit) else {
        return Ok(None);
    };
    let ArType::Func(params, ret) = interner.resolve(bound) else {
        return Err(Diagnostic::ice(
            crate::DiagCode::ICEL001,
            "callback bound is not a function type",
            pool.expr_span(callee_id),
        ));
    };
    let mut method_params = vec![callee_ty];
    method_params.extend(interner.type_args(params));
    let method = ArType::func(&method_params, ret, interner);
    let method_ty = type_check.type_info_mut().type_interner.intern(method);
    let base = super::lower_expr(type_check, pool, hir_pool, callee_id)?;
    let callee = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Field {
            base,
            field: "call".into(),
        },
        ty: method_ty,
        span: pool.expr_span(callee_id),
    });
    let mut args = vec![super::lower_expr(type_check, pool, hir_pool, callee_id)?];
    for &arg in arg_ids {
        args.push(super::lower_expr(type_check, pool, hir_pool, arg)?);
    }
    Ok(Some(HirExprKind::Call {
        callee,
        args: hir_pool.alloc_expr_list(&args),
    }))
}

/// `c(args)` on an owning closure `c` is `c.code(c.env, args)`: its code takes
/// the environment first (RFC 0032, L3). The type checker only admits a place
/// as `c`, so reading it twice evaluates nothing twice.
fn lower_owning_closure_call(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    callee_id: ExprId,
    arg_ids: &[ExprId],
) -> Result<Option<HirExprKind>, Diagnostic> {
    use arandu_middle::symbol_table::LangItem;
    let Some(callee_ty) = type_check.type_info.expr_type_id(callee_id) else {
        return Ok(None);
    };
    let interner = &type_check.type_info.type_interner;
    let closure = match interner.resolve(callee_ty) {
        ArType::Ref(inner) | ArType::RefMut(inner) => interner.resolve(inner),
        other => other,
    };
    let ArType::Named(symbol, type_args) = closure else {
        return Ok(None);
    };
    let Some(kind) = type_check
        .symbols
        .try_get(symbol)
        .and_then(|symbol| symbol.lang_item)
        .and_then(LangItem::owning_closure)
    else {
        return Ok(None);
    };
    let &[code_ty] = interner.type_args(type_args).as_slice() else {
        return Err(Diagnostic::ice(
            crate::DiagCode::ICEL001,
            "owning closure type has no code signature",
            pool.expr_span(callee_id),
        ));
    };
    if kind.once {
        return lower_once_closure_call(
            type_check,
            pool,
            hir_pool,
            (callee_id, symbol, code_ty),
            arg_ids,
        )
        .map(Some);
    }
    let byte = interner.intern(ArType::Primitive(Primitive::U8));
    let env_ty = interner.intern(ArType::Ptr(byte));
    let span = pool.expr_span(callee_id);
    let mut field = |hir_pool: &mut crate::hir::HirPool, name: &str, ty| {
        super::lower_expr(type_check, pool, hir_pool, callee_id).map(|base| {
            hir_pool.alloc_expr(HirExpr {
                kind: HirExprKind::Field {
                    base,
                    field: name.into(),
                },
                ty,
                span,
            })
        })
    };
    let callee = field(hir_pool, "code", code_ty)?;
    let mut args = vec![field(hir_pool, "env", env_ty)?];
    for &arg in arg_ids {
        args.push(super::lower_expr(type_check, pool, hir_pool, arg)?);
    }
    Ok(Some(HirExprKind::Call {
        callee,
        args: hir_pool.alloc_expr_list(&args),
    }))
}

/// `c(args)` on an `own once` closure consumes `c` (RFC 0032, L3):
///
/// ```text
/// { let call = OnceClosure.release(c); call.code(call.env, args) }
/// ```
///
/// `release` takes the environment from `c`, which is then destroyed empty;
/// the code moves the captures out or borrows them, and destroying `call`
/// at the end of the scope frees what remains of the environment.
fn lower_once_closure_call(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    (callee_id, closure_symbol, code_ty): (ExprId, arandu_middle::SymbolId, crate::types::TypeId),
    arg_ids: &[ExprId],
) -> Result<HirExprKind, Diagnostic> {
    use crate::hir::{HirBindingItem, HirBlock, HirStmt, HirStmtKind};
    let span = pool.expr_span(callee_id);
    let ice = |message: &str| Diagnostic::ice(crate::DiagCode::ICEL001, message.to_string(), span);
    let release = type_check
        .symbols
        .associated_members
        .get(&(closure_symbol, "release".into()))
        .copied()
        .ok_or_else(|| ice("`once` closure has no release"))?;
    let interner = &type_check.type_info.type_interner;
    let Some(ArType::Func(_, release_ret)) = type_check.type_info.decl_type(release) else {
        return Err(ice("`once` closure release has no signature"));
    };
    let ArType::Named(call_symbol, _) = interner.resolve(release_ret) else {
        return Err(ice("`once` closure release does not return its call"));
    };
    let ArType::Func(_, result_ty) = interner.resolve(code_ty) else {
        return Err(ice("`once` closure code is not a function"));
    };
    let call_ty = interner.intern(ArType::named(call_symbol, &[code_ty], interner));
    let closure_ty = type_check
        .type_info
        .expr_type_id(callee_id)
        .ok_or_else(|| ice("`once` closure has no type"))?;
    let byte = interner.intern(ArType::Primitive(Primitive::U8));
    let env_ty = interner.intern(ArType::Ptr(byte));
    let release_ty = interner.intern(ArType::func(&[closure_ty], call_ty, interner));

    // let call = release<F>(c)
    let closure = super::lower_expr(type_check, pool, hir_pool, callee_id)?;
    let release_path = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol: release },
        ty: release_ty,
        span,
    });
    let release_callee = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Generic {
            callee: release_path,
            args: vec![code_ty],
        },
        ty: release_ty,
        span,
    });
    let release_args = hir_pool.alloc_expr_list(&[closure]);
    let released = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Call {
            callee: release_callee,
            args: release_args,
        },
        ty: call_ty,
        span,
    });
    let call_local = super::temporary::synthetic_local(type_check, span, "call");
    let bindings = hir_pool.alloc_binding_list(&[HirBindingItem {
        symbol: call_local,
        ty: call_ty,
        span,
    }]);
    let declared = hir_pool.alloc_stmt(HirStmt {
        kind: HirStmtKind::VarDecl {
            bindings,
            value: released,
        },
        span,
    });
    let part = |hir_pool: &mut crate::hir::HirPool, name: &str, ty| {
        let base = hir_pool.alloc_expr(HirExpr {
            kind: HirExprKind::Path { symbol: call_local },
            ty: call_ty,
            span,
        });
        hir_pool.alloc_expr(HirExpr {
            kind: HirExprKind::Field {
                base,
                field: name.into(),
            },
            ty,
            span,
        })
    };

    // call.code(call.env, args)
    let code = part(hir_pool, "code", code_ty);
    let mut args = vec![part(hir_pool, "env", env_ty)];
    for &arg in arg_ids {
        args.push(super::lower_expr(type_check, pool, hir_pool, arg)?);
    }
    let args = hir_pool.alloc_expr_list(&args);
    let invoked = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Call { callee: code, args },
        ty: result_ty,
        span,
    });
    let value = hir_pool.alloc_stmt(HirStmt {
        kind: HirStmtKind::Expr(invoked),
        span,
    });
    let statements = hir_pool.alloc_stmt_list(&[declared, value]);
    let block = hir_pool.alloc_block(HirBlock { statements, span });
    Ok(HirExprKind::Scope { block })
}

/// `NAME.method(args)` where `NAME` is a `static` or `const`: the type
/// checker resolved `method` on the value, so the call is
/// `method(NAME, args)`. `None` for any other callee.
fn value_receiver_call(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    callee_id: ExprId,
    arg_ids: &[ExprId],
) -> Result<Option<HirExprKind>, Diagnostic> {
    let ExprKind::TypePath { type_name, .. } = pool.expr(callee_id) else {
        return Ok(None);
    };
    let Some(receiver) = type_check
        .resolved
        .expr_symbol(callee_id)
        .filter(|&symbol| {
            type_check.symbols.try_get(symbol).is_some_and(|entry| {
                matches!(
                    entry.kind,
                    arandu_middle::SymbolKind::ConstParam
                        | arandu_middle::SymbolKind::Const
                        | arandu_middle::SymbolKind::StaticMut
                )
            })
        })
    else {
        return Ok(None);
    };
    let callee_span = pool.expr_span(callee_id);
    let Some(method) = type_check
        .resolved
        .value_refs
        .get(&callee_span.into())
        .copied()
        .filter(|&method| method != receiver)
    else {
        return Ok(None);
    };
    let error = arandu_middle::types::TypeInterner::preinterned_error_id();
    let method_ty = type_check
        .type_info
        .expr_type_id(callee_id)
        .unwrap_or(error);
    let receiver_ty = type_check.type_info.decl_type_id(receiver).unwrap_or(error);
    let callee = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol: method },
        ty: method_ty,
        span: callee_span,
    });
    let mut args = vec![hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol: receiver },
        ty: receiver_ty,
        span: type_name.span,
    })];
    for &arg in arg_ids {
        args.push(super::lower_expr(type_check, pool, hir_pool, arg)?);
    }
    let args = hir_pool.alloc_expr_list(&args);
    Ok(Some(HirExprKind::Call { callee, args }))
}

fn resolve_call_symbol(
    type_check: &TypeCheckResult,
    pool: &AstPool,
    hir_pool: &crate::hir::HirPool,
    callee_id: ExprId,
    callee_vid: crate::hir::HirExprId,
) -> Option<arandu_middle::SymbolId> {
    let mut inner_vid = callee_vid;
    while let HirExprKind::Generic { callee, .. } = hir_pool.expr(inner_vid).kind {
        inner_vid = callee;
    }
    match hir_pool.expr(inner_vid).kind {
        HirExprKind::Path { symbol }
        | HirExprKind::TypePath {
            member_symbol: symbol,
            ..
        } => return Some(symbol),
        _ => {}
    }
    let bare = match pool.expr(callee_id) {
        ExprKind::Generic { callee, .. } => *callee,
        _ => callee_id,
    };
    if let ExprKind::Field { .. } | ExprKind::SafeField { .. } = pool.expr(bare) {
        let field_span = pool.expr_span(bare);
        if let Some(&sym) = type_check.resolved.value_refs.get(&field_span.into()) {
            return Some(sym);
        }
    }
    type_check.resolved.expr_symbol(bare)
}

fn callee_param_types(
    type_check: &TypeCheckResult,
    hir_pool: &crate::hir::HirPool,
    callee_sym: arandu_middle::SymbolId,
    callee_vid: crate::hir::HirExprId,
) -> Vec<crate::types::TypeId> {
    let interner = &type_check.type_info.type_interner;
    let ty = hir_pool.expr(callee_vid).ty;
    if let ArType::Func(params, _) = interner.resolve(ty) {
        return interner.type_args(params);
    }
    if let Some(decl_ty) = type_check.type_info.decl_type_id(callee_sym)
        && let ArType::Func(params, _) = interner.resolve(decl_ty)
    {
        return interner.type_args(params);
    }
    Vec::new()
}

fn materialize_param_default(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    callee_sym: arandu_middle::SymbolId,
    default: arandu_typeck::ParamDefault,
    formal_ty: crate::types::TypeId,
    span: arandu_middle::Span,
) -> Result<crate::hir::HirExprId, Diagnostic> {
    let error_ty = arandu_middle::types::TypeInterner::preinterned_error_id();
    let ty_for = |fallback: crate::types::TypeId| {
        if formal_ty != error_ty {
            formal_ty
        } else {
            fallback
        }
    };
    let expr = match default {
        arandu_typeck::ParamDefault::LocalExpr { expr, ty } => {
            if callee_sym.file_id == span.file_id && expr.as_usize() < pool.exprs.len() {
                return super::lower_expr(type_check, pool, hir_pool, expr);
            }
            return Err(Diagnostic::error(
                crate::DiagCode::T012WrongArgCount,
                "cross-module default parameter requires a constant literal or `nil`",
                span,
            )
            .with_hint(format!(
                "pass this argument explicitly or use a literal default (expected `{}`)",
                type_check
                    .type_info
                    .type_interner
                    .resolve(ty_for(ty))
                    .display(&type_check.symbols, &type_check.type_info.type_interner)
            )));
        }
        arandu_typeck::ParamDefault::Int { raw, ty } => {
            let kind = HirExprKind::Int(raw);
            let expr_ty = super::expr_type_for_kind(type_check, hir_pool, &kind, ty_for(ty));
            HirExpr {
                kind,
                ty: expr_ty,
                span,
            }
        }
        arandu_typeck::ParamDefault::Float { raw, ty } => {
            let kind = HirExprKind::Float(raw);
            let expr_ty = super::expr_type_for_kind(type_check, hir_pool, &kind, ty_for(ty));
            HirExpr {
                kind,
                ty: expr_ty,
                span,
            }
        }
        arandu_typeck::ParamDefault::Bool(v) => HirExpr {
            kind: HirExprKind::Bool(v),
            ty: arandu_middle::types::TypeInterner::preinterned_primitive(Primitive::Bool),
            span,
        },
        arandu_typeck::ParamDefault::Str(raw) => HirExpr {
            kind: HirExprKind::Str(raw),
            ty: arandu_middle::types::TypeInterner::preinterned_primitive(Primitive::Str),
            span,
        },
        arandu_typeck::ParamDefault::Char(raw) => HirExpr {
            kind: HirExprKind::Char(raw),
            ty: arandu_middle::types::TypeInterner::preinterned_primitive(Primitive::Char),
            span,
        },
        arandu_typeck::ParamDefault::Nil { ty } => HirExpr {
            kind: HirExprKind::Nil,
            ty: ty_for(ty),
            span,
        },
    };
    Ok(hir_pool.alloc_expr(expr))
}
