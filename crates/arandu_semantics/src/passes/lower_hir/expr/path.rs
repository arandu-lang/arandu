use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExpr, HirExprKind};
use crate::passes::lowering::{require_type_symbol, require_value_symbol};
use crate::passes::type_checker::types::{ArType, Primitive};
use arandu_middle::types::TypeInterner;
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};

pub(super) fn lower_path(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Path { .. } = pool.expr(expr) else {
        unreachable!();
    };
    let symbol = require_value_symbol(&type_check.resolved, expr, span)?;
    if type_check
        .symbols
        .try_get(symbol)
        .and_then(|entry| entry.lang_item)
        == Some(arandu_middle::symbol_table::LangItem::OptionNone)
    {
        return Ok(super::dummy_unit_ctor_kind(
            hir_pool,
            crate::hir::ResultCtorVariant::None,
            span,
        ));
    }
    if let Some(kind) = generic_function_value(type_check, hir_pool, expr, symbol, span) {
        return Ok(kind);
    }
    let kind = captured_value(hir_pool, symbol, span);
    if let HirExprKind::Path { symbol: sym } = kind {
        let sym = maybe_wrap_exported_func_value(type_check, hir_pool, sym, span);
        return Ok(HirExprKind::Path { symbol: sym });
    }
    Ok(kind)
}

fn alloc_synthetic_symbol(
    type_check: &mut TypeCheckResult,
    span: arandu_middle::Span,
    name: &str,
    kind: crate::SymbolKind,
) -> crate::SymbolId {
    use arandu_middle::{Symbol, SymbolId, Visibility};
    let file_id = type_check.symbols.file_id;
    let mut salt = name.bytes().fold(0x811C_9DC5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    loop {
        let mixed = span.start.wrapping_mul(0x9E37_79B1)
            ^ span.end.wrapping_mul(0x85EB_CA6B)
            ^ salt.wrapping_mul(0xC2B2_AE35);
        let id = SymbolId::new(
            file_id,
            SymbolId::SYNTHETIC_LOCAL_BIT | (mixed & 0x3FFF_FFFF),
        );
        if type_check.symbols.try_get(id).is_none() {
            let scope = type_check.symbols.global_scope();
            type_check.symbols_mut().register_synthetic(Symbol {
                id,
                name: name.into(),
                kind,
                span,
                scope,
                visibility: Visibility::Private,
                lang_item: None,
            });
            return id;
        }
        salt = salt.wrapping_add(0x1_0000);
    }
}

/// Synthesizes an internal non-exported wrapper function when an `@Export`
/// function is referenced as a value so indirect calls always use the internal
/// ABI across every backend (even for aggregate parameters or returns).
pub(super) fn maybe_wrap_exported_func_value(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    symbol: crate::SymbolId,
    span: arandu_middle::Span,
) -> crate::SymbolId {
    if hir_pool.suppress_export_thunk_span == Some(span) {
        return symbol;
    }
    if let Some(&cached) = hir_pool.exported_thunks.get(&symbol) {
        return cached;
    }
    let Some((thunk_sym, decl_id)) = build_export_thunk_decl(type_check, hir_pool, symbol) else {
        return symbol;
    };
    hir_pool.exported_thunks.insert(symbol, thunk_sym);
    hir_pool.synthetic_decls.push(decl_id);
    thunk_sym
}

/// Lowers a standalone [`crate::hir::HirProgram`] containing only the synthetic internal ABI
/// wrapper thunk for an `@Export` function taking or returning aggregates.
pub fn lower_export_thunk_hir(
    type_check: &mut TypeCheckResult,
    target: crate::SymbolId,
) -> Result<Option<crate::hir::HirProgram>, Vec<Diagnostic>> {
    if type_check
        .diagnostics
        .iter()
        .any(|d| d.severity == crate::Severity::Error)
    {
        return Err(type_check.diagnostics.clone());
    }
    let mut pool = crate::hir::HirPool::new();
    let Some((_, decl_id)) = build_export_thunk_decl(type_check, &mut pool, target) else {
        return Ok(None);
    };
    let span = type_check.symbols.try_get(target).map_or_else(
        || arandu_middle::Span::new(target.file_id, 0, 0),
        |entry| entry.span,
    );
    Ok(Some(crate::hir::HirProgram {
        span,
        module: None,
        decls: vec![decl_id],
        pool,
    }))
}

fn build_export_thunk_decl(
    type_check: &mut TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    symbol: crate::SymbolId,
) -> Option<(crate::SymbolId, crate::hir::HirDeclId)> {
    let entry = type_check.symbols.try_get(symbol)?.clone();
    let is_exported_func = entry.kind == crate::SymbolKind::Func
        && (type_check.type_info.exported_functions.contains(&symbol)
            || type_check.symbols.is_exported(symbol));
    if !is_exported_func {
        return None;
    }
    let decl_ty_id = type_check.type_info.decl_type_id(symbol)?;
    let (param_tys, ret_ty) = match type_check.type_info.type_interner.resolve(decl_ty_id) {
        ArType::Func(params, ret) => (type_check.type_info.type_interner.type_args(params), ret),
        _ => return None,
    };
    let is_direct_abi_scalar = |ty_id: arandu_middle::types::TypeId| -> bool {
        match type_check.type_info.type_interner.resolve(ty_id) {
            ArType::Void | ArType::Never | ArType::Ptr(_) | ArType::Func(..) => true,
            ArType::Primitive(p) => !matches!(p, Primitive::Str),
            _ => false,
        }
    };
    if param_tys.iter().all(|&p| is_direct_abi_scalar(p)) && is_direct_abi_scalar(ret_ty) {
        return None;
    }
    let thunk_span = entry.span;
    let thunk_sym = crate::SymbolId::export_thunk_for(symbol);
    let thunk_name = format!("__ar_export_thunk_{}_{}", symbol.file_id, symbol.local_id.0);
    let scope = type_check.symbols.global_scope();
    type_check
        .symbols_mut()
        .ensure_visible(arandu_middle::Symbol {
            id: thunk_sym,
            name: thunk_name.into(),
            kind: crate::SymbolKind::Func,
            span: thunk_span,
            scope,
            visibility: arandu_middle::Visibility::Private,
            lang_item: None,
        });
    type_check
        .type_info_mut()
        .decl_types
        .insert(thunk_sym, decl_ty_id);

    let mut hir_params = Vec::with_capacity(param_tys.len());
    let mut call_args = Vec::with_capacity(param_tys.len());
    for (idx, param_ty) in param_tys.into_iter().enumerate() {
        let param_name = format!(
            "__ar_export_thunk_{}_{}_arg_{idx}",
            symbol.file_id, symbol.local_id.0
        );
        let param_sym = alloc_synthetic_symbol(
            type_check,
            thunk_span,
            &param_name,
            crate::SymbolKind::Param,
        );
        type_check
            .type_info_mut()
            .decl_types
            .insert(param_sym, param_ty);
        hir_params.push(crate::hir::HirParam {
            symbol: param_sym,
            ty: param_ty,
            span: thunk_span,
            is_receiver: false,
            receiver_kind: None,
        });
        call_args.push(hir_pool.alloc_expr(HirExpr {
            kind: HirExprKind::Path { symbol: param_sym },
            ty: param_ty,
            span: thunk_span,
        }));
    }

    let callee_expr = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol },
        ty: decl_ty_id,
        span: thunk_span,
    });
    let args_range = hir_pool.alloc_expr_list(&call_args);
    let call_expr = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Call {
            callee: callee_expr,
            args: args_range,
        },
        ty: ret_ty,
        span: thunk_span,
    });
    let stmt_kind = if matches!(
        type_check.type_info.type_interner.resolve(ret_ty),
        ArType::Void
    ) {
        crate::hir::HirStmtKind::Expr(call_expr)
    } else {
        let ret_values = hir_pool.alloc_expr_list(&[call_expr]);
        crate::hir::HirStmtKind::Return { values: ret_values }
    };
    let stmt_id = hir_pool.alloc_stmt(crate::hir::HirStmt {
        kind: stmt_kind,
        span: thunk_span,
    });
    let stmts_range = hir_pool.alloc_stmt_list(&[stmt_id]);
    let block_id = hir_pool.alloc_block(crate::hir::HirBlock {
        statements: stmts_range,
        span: thunk_span,
    });
    let params_range = hir_pool.alloc_param_list(&hir_params);
    let decl_id = hir_pool.alloc_decl(crate::hir::HirDecl::Func(crate::hir::HirFunc {
        symbol: thunk_sym,
        params: params_range,
        return_type: ret_ty,
        body: Some(block_id),
        span: thunk_span,
        is_async: false,
        no_fallback: false,
    }));
    Some((thunk_sym, decl_id))
}

/// A generic function used as a value names one instance: typeck recorded
/// its instantiated type, and monomorphization rewrites the `Generic` node to
/// that instance's path. A direct callee keeps its plain path (see `call.rs`).
fn generic_function_value(
    type_check: &TypeCheckResult,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    symbol: crate::SymbolId,
    span: arandu_middle::Span,
) -> Option<HirExprKind> {
    let info = &type_check.type_info;
    info.generic_params
        .get(&symbol)
        .filter(|params| !params.is_empty())?;
    let instance = info.expr_type_id(expr)?;
    if Some(instance) == info.decl_type_id(symbol)
        || !matches!(info.type_interner.resolve(instance), ArType::Func(..))
    {
        return None;
    }
    let args = crate::passes::monomorphize::function_value_type_args(type_check, symbol, instance)?;
    let callee = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol },
        ty: instance,
        span,
    });
    Some(HirExprKind::Generic { callee, args })
}

/// A use of `symbol`. Inside a closure body a captured local is its
/// environment field, read through the stored reference for a borrow.
pub(crate) fn captured_value(
    hir_pool: &mut crate::hir::HirPool,
    symbol: crate::SymbolId,
    span: arandu_middle::Span,
) -> HirExprKind {
    let Some(alias) = hir_pool.capture_aliases.get(&symbol).cloned() else {
        return HirExprKind::Path { symbol };
    };
    let env = hir_pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol: alias.env },
        ty: alias.env_ty,
        span,
    });
    let field = HirExprKind::Field {
        base: env,
        field: alias.field,
    };
    if !alias.through_ref {
        return field;
    }
    let reference = hir_pool.alloc_expr(HirExpr {
        kind: field,
        ty: alias.field_ty,
        span,
    });
    HirExprKind::Unary {
        op: arandu_middle::ops::UnaryOp::Deref,
        expr: reference,
    }
}

pub(super) fn lower_variant_sugar(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::VariantSugar { name, args, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            // T2.2: lower like Result/Option/Poll ctors from the recorded expr type.
            let arg_ids = pool.expr_list(*args);
            let variant = match (type_check.type_info.expr_type(expr), name.as_str()) {
                (Some(ArType::Result(_, _)), "Ok") => Some(crate::hir::ResultCtorVariant::Ok),
                (Some(ArType::Result(_, _)), "Err") => Some(crate::hir::ResultCtorVariant::Err),
                (Some(ArType::Option(_)), "Some") => Some(crate::hir::ResultCtorVariant::Some),
                (Some(ArType::Option(_)), "None") => Some(crate::hir::ResultCtorVariant::None),
                (Some(ArType::Poll(_)), "Ready") => Some(crate::hir::ResultCtorVariant::PollReady),
                (Some(ArType::Poll(_)), "Pending") => {
                    Some(crate::hir::ResultCtorVariant::PollPending)
                }
                _ => None,
            };
            if let Some(variant) = variant {
                if arg_ids.len() == 1 {
                    let value_id = super::lower_expr(type_check, pool, hir_pool, arg_ids[0])?;
                    HirExprKind::ResultCtor {
                        variant,
                        value: value_id,
                    }
                } else if arg_ids.is_empty()
                    && matches!(
                        variant,
                        crate::hir::ResultCtorVariant::None
                            | crate::hir::ResultCtorVariant::PollPending
                    )
                {
                    let dummy = hir_pool.alloc_expr(HirExpr {
                        kind: HirExprKind::Bool(false),
                        ty: TypeInterner::preinterned_primitive(Primitive::Bool),
                        span,
                    });
                    HirExprKind::ResultCtor {
                        variant,
                        value: dummy,
                    }
                } else {
                    return Err(Diagnostic::error(
                        crate::DiagCode::ICET001,
                        format!(
                            "variant sugar `.{}` has unexpected arity {}",
                            name,
                            arg_ids.len()
                        ),
                        span,
                    ));
                }
            } else if let Some(member_symbol) = type_check
                .resolved
                .value_refs
                .get(&crate::NodeKey::from(span))
                .copied()
                .or_else(|| type_check.resolved.expr_symbol(expr))
            {
                let type_symbol = match type_check.type_info.expr_type(expr) {
                    Some(ArType::Named(id, _)) => id,
                    _ => {
                        return Err(Diagnostic::error(
                            crate::DiagCode::ICET001,
                            format!("variant sugar `.{}` missing resolved enum type", name),
                            span,
                        ));
                    }
                };
                let path_kind = HirExprKind::TypePath {
                    type_symbol,
                    member_symbol,
                };
                if arg_ids.is_empty() {
                    path_kind
                } else {
                    let mut lowered_args = Vec::with_capacity(arg_ids.len());
                    for &arg_id in arg_ids {
                        lowered_args.push(super::lower_expr(type_check, pool, hir_pool, arg_id)?);
                    }
                    let callee_ty = type_check
                        .type_info
                        .decl_type_id(member_symbol)
                        .unwrap_or(fallback_ty);
                    let callee = hir_pool.alloc_expr(HirExpr {
                        kind: path_kind,
                        ty: callee_ty,
                        span,
                    });
                    let args_range = hir_pool.alloc_expr_list(&lowered_args);
                    HirExprKind::Call {
                        callee,
                        args: args_range,
                    }
                }
            } else {
                return Err(Diagnostic::error(
                    crate::DiagCode::ICET001,
                    format!("unresolved variant sugar `.{}`", name),
                    span,
                ));
            }
        }
    })
}

pub(super) fn lower_type_path(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: arandu_middle::Span,
    fallback_ty: crate::types::TypeId,
) -> Result<HirExpr, Diagnostic> {
    let ExprKind::TypePath {
        type_name, member, ..
    } = pool.expr(expr)
    else {
        unreachable!();
    };
    let kind = {
        {
            if let Some(symbol) = type_check.resolved.expr_symbol(expr)
                && type_check.symbols.try_get(symbol).is_some_and(|entry| {
                    matches!(
                        entry.kind,
                        arandu_middle::SymbolKind::ConstParam
                            | arandu_middle::SymbolKind::Const
                            | arandu_middle::SymbolKind::StaticMut
                    )
                })
            {
                let base_ty = type_check
                    .type_info
                    .decl_type_id(symbol)
                    .unwrap_or_else(super::error_ty);
                let base = hir_pool.alloc_expr(HirExpr {
                    kind: HirExprKind::Path { symbol },
                    ty: base_ty,
                    span: type_name.span,
                });
                return Ok(HirExpr {
                    kind: HirExprKind::Field {
                        base,
                        field: member.clone(),
                    },
                    ty: fallback_ty,
                    span,
                });
            }
            // Builtin unit ctors as bare TypePath (not Call): `Option.None`, `Poll.Pending`.
            let variant = super::builtin_ctor_variant_for_type_path(type_check, type_name, member);
            if let Some(variant) = variant.filter(|variant| {
                matches!(
                    variant,
                    crate::hir::ResultCtorVariant::None
                        | crate::hir::ResultCtorVariant::PollPending
                )
            }) {
                super::dummy_unit_ctor_kind(hir_pool, variant, span)
            } else {
                let type_symbol = require_type_symbol(&type_check.resolved, type_name.span)?;
                let member_symbol = require_value_symbol(&type_check.resolved, expr, span)?;
                HirExprKind::TypePath {
                    type_symbol,
                    member_symbol,
                }
            }
        }
    };
    let ty = super::expr_type_for_kind(type_check, hir_pool, &kind, fallback_ty);
    Ok(HirExpr { kind, ty, span })
}

/// `f<int>(xs, lambda)` names only the declared parameters; the implicit
/// callback parameters typeck inferred are read back from the instantiated
/// callee so the instance key is complete.
fn complete_callback_type_args(
    type_check: &TypeCheckResult,
    hir_pool: &crate::hir::HirPool,
    expr: ExprId,
    callee: crate::hir::HirExprId,
    args: &mut Vec<arandu_middle::types::TypeId>,
) {
    let symbol = match &hir_pool.expr(callee).kind {
        HirExprKind::Path { symbol } => *symbol,
        HirExprKind::TypePath { member_symbol, .. } => *member_symbol,
        _ => return,
    };
    let info = &type_check.type_info;
    let Some(params) = info.generic_params.get(&symbol) else {
        return;
    };
    let declared = params
        .iter()
        .filter(|param| !info.callback_bounds.contains_key(param))
        .count();
    if declared == params.len() || args.len() < declared {
        return;
    }
    args.truncate(declared);
    let interner = &info.type_interner;
    for &param in &params[declared..] {
        args.push(interner.intern(ArType::named(param, &[], interner)));
    }
    let (Some(ArType::Func(template, _)), Some(ArType::Func(instance, _))) =
        (info.decl_type(symbol), info.expr_type(expr))
    else {
        return;
    };
    let template = interner.type_args(template);
    let instance = interner.type_args(instance);
    for (&formal, &actual) in template.iter().zip(&instance) {
        let (ArType::RefMut(formal), ArType::RefMut(actual)) =
            (interner.resolve(formal), interner.resolve(actual))
        else {
            continue;
        };
        if let ArType::Named(implicit, _) = interner.resolve(formal)
            && let Some(index) = params[declared..].iter().position(|&p| p == implicit)
        {
            args[declared + index] = actual;
        }
    }
}

pub(super) fn lower_generic(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
) -> Result<HirExprKind, Diagnostic> {
    let ExprKind::Generic { callee, args, .. } = pool.expr(expr) else {
        unreachable!();
    };
    Ok({
        {
            let mut callee_id = super::lower_expr(type_check, pool, hir_pool, *callee)?;
            // The explicit arguments name the instance; keep the plain path.
            if let HirExprKind::Generic { callee: path, .. } = hir_pool.expr(callee_id).kind {
                callee_id = path;
            }
            let mut hir_args = Vec::new();
            let arg_ids = pool.type_expr_list(*args);
            let interner = &mut std::sync::Arc::make_mut(&mut type_check.type_info).type_interner;
            for &arg_id in arg_ids {
                let ar = crate::passes::type_checker::types::lower_type_expr(
                    arg_id,
                    pool,
                    &type_check.symbols,
                    crate::ScopeId(0),
                    &type_check.resolved,
                    interner,
                );
                hir_args.push(interner.intern(ar));
            }
            complete_callback_type_args(type_check, hir_pool, expr, callee_id, &mut hir_args);
            HirExprKind::Generic {
                callee: callee_id,
                args: hir_args,
            }
        }
    })
}
