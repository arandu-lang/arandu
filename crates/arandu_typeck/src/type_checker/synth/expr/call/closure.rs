//! Synthesize the capture struct and method behind an accepted lambda.
//!
//! A `move ||` job gets `run`; a callback argument gets `call`. The struct and
//! method are span-derived symbols. They are not dense locals, so parallel
//! item typeck of the same signature table cannot reuse one id.

use arandu_lexer::Span;
use arandu_middle::layout::{StructFieldInfo, StructFields};
use arandu_middle::{Symbol, SymbolId, SymbolKind, Visibility};
use arandu_parser::ast_pool::{ExprId, ExprKind};
use rustc_hash::FxHashSet;
use smol_str::SmolStr;
use std::sync::Arc;

use crate::type_checker::TypeChecker;
use crate::type_checker::info::{CaptureMode, ClosureCaptureField, ClosureParam, ClosureRecipe};
use crate::type_checker::types::ArType;
use crate::type_checker::types::TypeId;

pub(super) struct Capture {
    pub symbol: SymbolId,
    pub name: SmolStr,
    pub ty: TypeId,
    pub span: Span,
    pub mode: CaptureMode,
}

/// What the closure method looks like: its name, the struct and host name
/// prefixes, and its real parameters. `None` symbols are minted as `argument`.
pub(super) struct ClosureShape<'a> {
    pub method: &'static str,
    pub struct_prefix: &'static str,
    pub host_prefix: &'static str,
    pub params: &'a [(Option<SymbolId>, TypeId)],
    pub return_type: TypeId,
}

/// Registers the struct, its fields and the method; returns the struct type.
pub(super) fn install_closure(
    checker: &mut TypeChecker<'_>,
    lambda_span: Span,
    shape: &ClosureShape<'_>,
    captures: &[Capture],
) -> TypeId {
    let file_id = checker.symbols.file_id;
    let mut reserved = FxHashSet::default();
    let struct_symbol = mint(checker, file_id, lambda_span, 0, &mut reserved);
    let method_symbol = mint(checker, file_id, lambda_span, 1, &mut reserved);
    let self_param = mint(checker, file_id, lambda_span, 2, &mut reserved);
    let mut minted_params = Vec::new();
    let params = shape
        .params
        .iter()
        .enumerate()
        .map(|(index, &(symbol, ty))| {
            let symbol = symbol.unwrap_or_else(|| {
                let role = 3u32.wrapping_add(u32::try_from(index).unwrap_or(u32::MAX) << 8);
                let minted = mint(checker, file_id, lambda_span, role, &mut reserved);
                minted_params.push(minted);
                minted
            });
            ClosureParam { symbol, ty }
        })
        .collect::<Vec<_>>();
    let mut used_names = FxHashSet::default();
    let mut fields = Vec::with_capacity(captures.len());
    for (index, capture) in captures.iter().enumerate() {
        let mut name = capture.name.clone();
        if !used_names.insert(name.clone()) {
            name = SmolStr::new(format!("{name}_{}", capture.symbol.local_id.0));
            used_names.insert(name.clone());
        }
        let symbol = mint(
            checker,
            file_id,
            lambda_span,
            4 + u32::try_from(index).unwrap_or(u32::MAX),
            &mut reserved,
        );
        fields.push(ClosureCaptureField {
            name,
            symbol,
            source: capture.symbol,
            ty: capture.ty,
            span: capture.span,
            mode: capture.mode,
        });
    }

    let struct_name = SmolStr::new(format!(
        "{}_{}_{}",
        shape.struct_prefix, lambda_span.start, lambda_span.end
    ));
    let method_name = SmolStr::new(shape.method);
    let host_name = SmolStr::new(format!(
        "{}_{}_{}_{}_{}",
        shape.host_prefix, file_id, lambda_span.start, lambda_span.end, shape.method
    ));
    let scope = checker.symbols.global_scope();
    let symbols = Arc::make_mut(&mut checker.symbols);
    register(
        symbols,
        struct_symbol,
        &struct_name,
        SymbolKind::Struct,
        lambda_span,
        scope,
    );
    register(
        symbols,
        method_symbol,
        &format!("{struct_name}.{method_name}"),
        SymbolKind::AssociatedFunc,
        lambda_span,
        scope,
    );
    register(
        symbols,
        self_param,
        "self",
        SymbolKind::Param,
        lambda_span,
        scope,
    );
    for &param in &minted_params {
        register(
            symbols,
            param,
            "argument",
            SymbolKind::Param,
            lambda_span,
            scope,
        );
    }
    for field in &fields {
        register(
            symbols,
            field.symbol,
            &field.name,
            SymbolKind::Field,
            field.span,
            scope,
        );
    }
    symbols
        .associated_members_mut()
        .insert((struct_symbol, method_name.clone()), method_symbol);
    symbols
        .host_function_names
        .insert(method_symbol, host_name.clone());

    let struct_named = ArType::named(struct_symbol, &[], &checker.type_info.type_interner);
    let struct_ty = checker.intern(struct_named);
    let mut method_params = Vec::with_capacity(params.len() + 1);
    method_params.push(struct_ty);
    method_params.extend(params.iter().map(|param| param.ty));
    let method_named = ArType::func(
        &method_params,
        shape.return_type,
        &checker.type_info.type_interner,
    );
    let method_ty = checker.intern(method_named);
    checker.record_decl_type(struct_symbol, struct_ty);
    checker.record_decl_type(method_symbol, method_ty);
    checker.record_decl_type(self_param, struct_ty);
    for param in &params {
        checker.record_decl_type(param.symbol, param.ty);
    }
    for field in &fields {
        checker.record_decl_type(field.symbol, field.ty);
    }
    let table = StructFields::from_entries(fields.iter().enumerate().map(|(index, field)| {
        StructFieldInfo {
            name: field.name.clone(),
            symbol: Some(field.symbol),
            ty: field.ty,
            index,
        }
    }));
    checker
        .type_info
        .struct_fields
        .insert(struct_symbol, Arc::new(table));
    checker.type_info.closures.insert(
        lambda_span,
        ClosureRecipe {
            lambda_span,
            struct_symbol,
            struct_name,
            method_symbol,
            method_name,
            self_param,
            params,
            fields,
            return_type: shape.return_type,
            host_name,
        },
    );
    struct_ty
}

/// The callback bound of a formal parameter, if it is an implicit callback
/// type parameter of the callee.
pub(super) fn callback_bound_of(checker: &TypeChecker<'_>, formal: TypeId) -> Option<TypeId> {
    match checker.resolve(formal) {
        ArType::Named(symbol, args) if args.is_empty() => {
            checker.type_info.callback_bounds.get(&symbol).copied()
        }
        _ => None,
    }
}

/// Argument types for generic inference. A lambda in a callback position is
/// typed after the other arguments, against its bound with the type
/// parameters those arguments already fix; it is never typed without one.
pub(super) fn synth_args_with_callbacks(
    checker: &mut TypeChecker<'_>,
    type_params: &[SymbolId],
    formals: &[TypeId],
    arg_ids: &[ExprId],
) -> Vec<TypeId> {
    let deferred = |checker: &TypeChecker<'_>, index: usize, arg: ExprId| {
        matches!(checker.pool.expr(arg), ExprKind::Lambda { .. })
            && formals
                .get(index)
                .is_some_and(|&formal| callback_bound_of(checker, formal).is_some())
    };
    let mut arg_tys = Vec::with_capacity(arg_ids.len());
    let mut bindings = rustc_hash::FxHashMap::default();
    for (index, &arg) in arg_ids.iter().enumerate() {
        if deferred(checker, index, arg) {
            arg_tys.push(None);
            continue;
        }
        let ty = super::synth_expr(checker, arg);
        if let Some(&formal) = formals.get(index) {
            let formal = checker.resolve(formal);
            super::instantiate::bind_type_params(checker, type_params, &formal, ty, &mut bindings);
        }
        arg_tys.push(Some(ty));
    }
    let (params, args): (Vec<_>, Vec<_>) = bindings.into_iter().unzip();
    let concrete = args
        .iter()
        .map(|&ty| checker.resolve(ty))
        .collect::<Vec<_>>();
    let subst = arandu_middle::types::build_subst(&params, &concrete);
    arg_tys
        .into_iter()
        .zip(arg_ids)
        .enumerate()
        .map(|(index, (ty, &arg))| {
            ty.unwrap_or_else(|| {
                let Some(bound) = formals
                    .get(index)
                    .and_then(|&formal| callback_bound_of(checker, formal))
                else {
                    return checker.intern(ArType::Error);
                };
                let bound = arandu_middle::types::substitute_type(
                    &checker.resolve(bound),
                    &subst,
                    &checker.type_info.type_interner,
                );
                let bound = checker.intern(bound);
                check_callback_lambda(checker, arg, bound)
            })
        })
        .collect()
}

/// Every argument bound to a callback parameter must be a closure, or a
/// callback parameter of the caller being passed on.
pub(super) fn check_callback_args(
    checker: &mut TypeChecker<'_>,
    formals: &[TypeId],
    arg_ids: &[ExprId],
    arg_tys: &[TypeId],
) {
    for ((&formal, &arg), &arg_ty) in formals.iter().zip(arg_ids).zip(arg_tys) {
        let Some(bound) = callback_bound_of(checker, formal) else {
            continue;
        };
        let actual = checker.resolve(arg_ty);
        let accepted = match actual {
            ArType::Error => true,
            ArType::Named(symbol, _) => {
                checker.type_info.callback_bounds.contains_key(&symbol)
                    || checker
                        .type_info
                        .closures
                        .values()
                        .any(|recipe| recipe.struct_symbol == symbol)
            }
            _ => false,
        };
        if accepted {
            continue;
        }
        let span = checker.pool.expr_span(arg);
        let interner = &checker.type_info.type_interner;
        let bound_text = checker.resolve(bound).display(&checker.symbols, interner);
        let diagnostic = if matches!(actual, ArType::Func(..)) {
            crate::Diagnostic::error(
                crate::DiagCode::U001FeatureNotSupported,
                "a named function as a callback argument is not supported yet (v0.3 LAMBDA L1)",
                span,
            )
            .with_label(span, format!("expected a closure of type '{bound_text}'"))
            .with_hint("wrap the function in a closure, such as `(x) => f(x)`")
        } else {
            crate::Diagnostic::error(
                crate::DiagCode::T003IncompatibleCallArg,
                format!(
                    "expected a callback of type '{bound_text}', found '{}'",
                    actual.display(&checker.symbols, interner)
                ),
                span,
            )
            .with_label(span, "this value is not callable")
        };
        checker.diagnostics.push(diagnostic);
    }
}

/// Type a lambda passed where a callback `bound` is expected and install its
/// closure struct. Returns the struct type, the argument's type.
pub(super) fn check_callback_lambda(
    checker: &mut TypeChecker<'_>,
    arg_id: ExprId,
    bound: TypeId,
) -> TypeId {
    let lambda_span = checker.pool.expr_span(arg_id);
    if let Some(recipe) = checker.type_info.closure_at(lambda_span) {
        let struct_symbol = recipe.struct_symbol;
        return checker
            .decl_type_id(struct_symbol)
            .unwrap_or_else(|| checker.intern(ArType::Error));
    }
    let error = checker.intern(ArType::Error);
    let ExprKind::Lambda {
        params,
        body,
        is_move,
    } = checker.pool.expr(arg_id).clone()
    else {
        return error;
    };
    let ArType::Func(expected_params, expected_ret) = checker.resolve(bound) else {
        return error;
    };
    let expected_params = checker.type_info.type_interner.type_args(expected_params);
    let lambda_params = checker.pool.lambda_param_list(params).to_vec();
    let bound_text = checker
        .resolve(bound)
        .display(&checker.symbols, &checker.type_info.type_interner);
    if lambda_params.len() != expected_params.len() {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T003IncompatibleCallArg,
                format!(
                    "closure takes {} parameters, but the callback '{bound_text}' takes {}",
                    lambda_params.len(),
                    expected_params.len()
                ),
                lambda_span,
            )
            .with_label(lambda_span, format!("expected '{bound_text}'")),
        );
        checker.record_expr_type(arg_id, error);
        return error;
    }

    let mut rejected = false;
    let mut method_params = Vec::with_capacity(lambda_params.len());
    for (&param_id, &expected) in lambda_params.iter().zip(expected_params.iter()) {
        let param = checker.pool.lambda_param(param_id).clone();
        if let Some(annotation) = param.ty {
            let scope = checker.type_scope();
            let annotated = checker.lower_type_expr(annotation, scope);
            let annotated = checker.intern(annotated);
            if !checker.unify_ids(expected, annotated) {
                rejected = true;
                let interner = &checker.type_info.type_interner;
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T003IncompatibleCallArg,
                        format!(
                            "closure parameter '{}' is annotated '{}', but the callback '{bound_text}' passes '{}'",
                            param.name,
                            checker.resolve(annotated).display(&checker.symbols, interner),
                            checker.resolve(expected).display(&checker.symbols, interner),
                        ),
                        param.span,
                    )
                    .with_label(param.span, "annotation differs from the callback type"),
                );
            }
        }
        let Some(&symbol) = checker
            .resolved
            .definitions
            .get(&crate::NodeKey::from(param.span))
        else {
            rejected = true;
            continue;
        };
        checker.ctx.bind(symbol, expected);
        checker.record_decl_type(symbol, expected);
        method_params.push((Some(symbol), expected));
    }

    let captures = callback_captures(checker, lambda_span, is_move);
    let return_type = expected_ret;
    match &body {
        arandu_parser::LambdaBody::Expr { expr, .. } => {
            let value = super::synth_expr_expected(checker, *expr, Some(return_type));
            if !matches!(checker.resolve(return_type), ArType::Void)
                && !checker.unify_ids(return_type, value)
            {
                let value_span = checker.pool.expr_span(*expr);
                checker.add_constraint(
                    return_type,
                    value,
                    crate::type_checker::constraints::ConstraintOrigin::ClosureResult {
                        value_span,
                        closure_span: lambda_span,
                    },
                );
            }
        }
        arandu_parser::LambdaBody::Block { block, .. } => {
            crate::type_checker::check::check_closure_block(checker, block, Some(return_type));
        }
    }
    let Some(captures) = captures else {
        checker.record_expr_type(arg_id, error);
        return error;
    };
    if rejected {
        checker.record_expr_type(arg_id, error);
        return error;
    }
    let struct_ty = install_closure(
        checker,
        lambda_span,
        &ClosureShape {
            method: "call",
            struct_prefix: "__Closure",
            host_prefix: "ar_closure",
            params: &method_params,
            return_type,
        },
        &captures,
    );
    checker.record_expr_type(arg_id, struct_ty);
    struct_ty
}

/// Deduce how a callback holds each captured local. `own`/`move` moves all;
/// otherwise an unmodified `Copy` value is copied. Borrowed captures are not
/// lowered yet and are rejected here with the reason.
fn callback_captures(
    checker: &mut TypeChecker<'_>,
    lambda_span: Span,
    is_move: bool,
) -> Option<Vec<Capture>> {
    let assigned = checker
        .resolved
        .value_refs
        .iter()
        .filter(|(key, _)| {
            super::thread_target::span_contains(
                lambda_span,
                Span::new(lambda_span.file_id, key.start, key.end),
            )
        })
        .map(|(_, symbol)| *symbol)
        .collect::<FxHashSet<_>>();
    let mut seen = FxHashSet::default();
    let mut captures = Vec::new();
    let mut rejected = false;
    for (use_span, symbol) in super::thread_target::value_uses_within(checker, lambda_span) {
        if seen.contains(&symbol) {
            continue;
        }
        let (captured, name) = {
            let declaration = checker.symbols.get(symbol);
            (
                matches!(declaration.kind, SymbolKind::Local | SymbolKind::Param)
                    && !super::thread_target::span_contains(lambda_span, declaration.span),
                declaration.name.clone(),
            )
        };
        if !captured {
            continue;
        }
        seen.insert(symbol);
        let ty = checker
            .ctx
            .lookup(symbol)
            .or_else(|| checker.decl_type_id(symbol))
            .unwrap_or_else(|| checker.intern(ArType::Error));
        if checker.resolve(ty).is_error() {
            rejected = true;
            continue;
        }
        let mode = if is_move {
            CaptureMode::Move
        } else if assigned.contains(&symbol) {
            rejected = true;
            borrowed_capture(checker, use_span, &name, "mutable borrow");
            continue;
        } else if !matches!(checker.resolve(ty), ArType::Ref(_) | ArType::RefMut(_))
            && checker.type_info.is_copy(ty)
        {
            CaptureMode::Copy
        } else {
            rejected = true;
            borrowed_capture(checker, use_span, &name, "shared borrow");
            continue;
        };
        captures.push(Capture {
            symbol,
            name,
            ty,
            span: use_span,
            mode,
        });
    }
    (!rejected).then_some(captures)
}

fn borrowed_capture(checker: &mut TypeChecker<'_>, span: Span, name: &str, capture: &str) {
    checker.diagnostics.push(
        crate::Diagnostic::error(
            crate::DiagCode::U001FeatureNotSupported,
            format!("capturing '{name}' by {capture} is not supported yet (v0.3 LAMBDA L1)"),
            span,
        )
        .with_label(span, format!("'{name}' is captured here"))
        .with_hint("prefix the closure with `move` to move the captured values into it"),
    );
}

fn register(
    symbols: &mut arandu_middle::SymbolTable,
    id: SymbolId,
    name: &str,
    kind: SymbolKind,
    span: Span,
    scope: arandu_middle::symbol_table::ScopeId,
) {
    symbols.register_synthetic(Symbol {
        id,
        name: name.into(),
        kind,
        span,
        scope,
        visibility: Visibility::Public,
        lang_item: None,
    });
}

fn mint(
    checker: &TypeChecker<'_>,
    file_id: arandu_middle::db::FileId,
    span: Span,
    mut role: u32,
    reserved: &mut FxHashSet<u32>,
) -> SymbolId {
    loop {
        let mixed = span.start.wrapping_mul(0x9E37_79B1)
            ^ span.end.wrapping_mul(0x85EB_CA6B)
            ^ role.wrapping_mul(0xC2B2_AE35);
        let local = SymbolId::SYNTHETIC_LOCAL_BIT | (mixed & 0x7FFF_FFFF);
        let id = SymbolId::new(file_id, local);
        if reserved.insert(local) && checker.symbols.try_get(id).is_none() {
            return id;
        }
        role = role.wrapping_add(0x1_0000);
        if role == 0 {
            return SymbolId::new(file_id, SymbolId::SYNTHETIC_LOCAL_BIT | 1);
        }
    }
}
