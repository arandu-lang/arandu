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
    /// `mut self` for a callback, which may be called many times; a
    /// `ThreadJob` takes `own self` and runs once.
    pub self_by_mut_ref: bool,
    /// The named function an adapter forwards to.
    pub adapts: Option<SymbolId>,
    /// `Closure<F>` or `SendClosure<F>` for an owning closure, whose code
    /// function `F` is synthesized too (RFC 0032, L3).
    pub owning: Option<TypeId>,
}

/// Registers the struct, its fields and the method; returns the struct type.
pub(super) fn install_closure(
    checker: &mut TypeChecker<'_>,
    lambda_span: Span,
    shape: &ClosureShape<'_>,
    captures: &[Capture],
) -> TypeId {
    let file_id = checker.symbols.file_id;
    let identity = ClosureIdentity::of(checker, lambda_span);
    let mut reserved = FxHashSet::default();
    let struct_symbol = mint(checker, file_id, &identity, 0, &mut reserved);
    let method_symbol = mint(checker, file_id, &identity, 1, &mut reserved);
    let self_param = mint(checker, file_id, &identity, 2, &mut reserved);
    let mut minted_params = Vec::new();
    let params = shape
        .params
        .iter()
        .enumerate()
        .map(|(index, &(symbol, ty))| {
            let symbol = symbol.unwrap_or_else(|| {
                let role = 3u32.wrapping_add(u32::try_from(index).unwrap_or(u32::MAX) << 8);
                let minted = mint(checker, file_id, &identity, role, &mut reserved);
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
            &identity,
            4 + u32::try_from(index).unwrap_or(u32::MAX),
            &mut reserved,
        );
        let field_ty = match capture.mode {
            CaptureMode::Borrow => checker.intern(ArType::Ref(capture.ty)),
            CaptureMode::BorrowMut => checker.intern(ArType::RefMut(capture.ty)),
            CaptureMode::Copy | CaptureMode::Move => capture.ty,
        };
        fields.push(ClosureCaptureField {
            name,
            symbol,
            source: capture.symbol,
            ty: capture.ty,
            field_ty,
            span: capture.span,
            mode: capture.mode,
        });
    }

    let struct_name = SmolStr::new(format!("{}{}", shape.struct_prefix, identity.suffix));
    let method_name = SmolStr::new(shape.method);
    let host_name = SmolStr::new(format!(
        "{}_{}{}_{}",
        shape.host_prefix, file_id, identity.suffix, shape.method
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

    let generics = closure_generics(checker, captures, &params, shape.return_type);
    let owning = shape.owning.map(|closure_ty| {
        install_owning_code(
            checker,
            &identity,
            &mut reserved,
            &struct_name,
            lambda_span,
            &params,
            closure_ty,
        )
    });
    let generic_args = generics
        .iter()
        .map(|&param| {
            let named = ArType::named(param, &[], &checker.type_info.type_interner);
            checker.intern(named)
        })
        .collect::<Vec<_>>();
    let struct_named = ArType::named(
        struct_symbol,
        &generic_args,
        &checker.type_info.type_interner,
    );
    let struct_ty = checker.intern(struct_named);
    let self_ty = if shape.self_by_mut_ref {
        checker.intern(ArType::RefMut(struct_ty))
    } else {
        struct_ty
    };
    let mut method_params = Vec::with_capacity(params.len() + 1);
    if shape.method != "fn" {
        method_params.push(self_ty);
    }
    method_params.extend(params.iter().map(|param| param.ty));
    let method_named = ArType::func(
        &method_params,
        shape.return_type,
        &checker.type_info.type_interner,
    );
    let method_ty = checker.intern(method_named);
    checker.record_decl_type(struct_symbol, struct_ty);
    checker.record_decl_type(method_symbol, method_ty);
    checker.record_decl_type(self_param, self_ty);
    for param in &params {
        checker.record_decl_type(param.symbol, param.ty);
    }
    for field in &fields {
        checker.record_decl_type(field.symbol, field.field_ty);
    }
    let table = StructFields::from_entries(fields.iter().enumerate().map(|(index, field)| {
        StructFieldInfo {
            name: field.name.clone(),
            symbol: Some(field.symbol),
            ty: field.field_ty,
            index,
        }
    }));
    checker
        .type_info
        .struct_fields
        .insert(struct_symbol, Arc::new(table));
    if !generics.is_empty() {
        let generics = Arc::new(generics.clone());
        let mut generic_symbols = vec![struct_symbol, method_symbol];
        generic_symbols.extend(owning.as_ref().map(|code| code.symbol));
        for symbol in generic_symbols {
            checker
                .type_info
                .generic_params
                .insert(symbol, generics.clone());
        }
    }
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
            adapts: shape.adapts,
            owning,
        },
    );
    struct_ty
}

/// The code function `(env: ptr[u8], A) -> R` stored in an owning closure,
/// typed as the closure's `F`; `lower_closure_hir` gives it a body.
fn install_owning_code(
    checker: &mut TypeChecker<'_>,
    identity: &ClosureIdentity,
    reserved: &mut FxHashSet<u32>,
    struct_name: &str,
    lambda_span: Span,
    params: &[ClosureParam],
    closure_ty: TypeId,
) -> crate::type_checker::info::OwningCode {
    let file_id = checker.symbols.file_id;
    const CODE_ROLE: u32 = 0x4000_0000;
    let symbol = mint(checker, file_id, identity, CODE_ROLE, reserved);
    let env_param = mint(checker, file_id, identity, CODE_ROLE + 1, reserved);
    let forwarded = (0..params.len())
        .map(|index| {
            let role = CODE_ROLE + 2 + (u32::try_from(index).unwrap_or(u32::MAX) << 8);
            mint(checker, file_id, identity, role, reserved)
        })
        .collect::<Vec<_>>();
    let host_name = SmolStr::new(format!("ar_closure_code_{file_id}{}", identity.suffix));
    let byte = checker.intern(ArType::Primitive(arandu_middle::types::Primitive::U8));
    let env_ty = checker.intern(ArType::Ptr(byte));
    let mut code_params = vec![env_ty];
    code_params.extend(params.iter().map(|param| param.ty));
    let return_type = match checker.resolve(closure_ty) {
        ArType::Named(_, args) => checker
            .type_info
            .type_interner
            .type_args(args)
            .first()
            .and_then(|&code| match checker.resolve(code) {
                ArType::Func(_, ret) => Some(ret),
                _ => None,
            }),
        _ => None,
    }
    .unwrap_or_else(|| checker.intern(ArType::Error));
    let code_ty = ArType::func(&code_params, return_type, &checker.type_info.type_interner);
    let code_ty = checker.intern(code_ty);
    let scope = checker.symbols.global_scope();
    let symbols = Arc::make_mut(&mut checker.symbols);
    register(
        symbols,
        symbol,
        &format!("{struct_name}.code"),
        SymbolKind::AssociatedFunc,
        lambda_span,
        scope,
    );
    register(
        symbols,
        env_param,
        "env",
        SymbolKind::Param,
        lambda_span,
        scope,
    );
    for &param in &forwarded {
        register(
            symbols,
            param,
            "argument",
            SymbolKind::Param,
            lambda_span,
            scope,
        );
    }
    symbols
        .host_function_names
        .insert(symbol, host_name.clone());
    checker.record_decl_type(symbol, code_ty);
    checker.record_decl_type(env_param, env_ty);
    for (&param, original) in forwarded.iter().zip(params) {
        checker.record_decl_type(param, original.ty);
    }
    crate::type_checker::info::OwningCode {
        symbol,
        env_param,
        params: forwarded,
        host_name,
        closure_ty,
    }
}

/// The callback bound of a formal parameter, if it is an implicit callback
/// type parameter of the callee.
pub(super) fn callback_bound_of(checker: &TypeChecker<'_>, formal: TypeId) -> Option<TypeId> {
    let implicit = callback_param_of(checker, formal)?;
    checker.type_info.callback_bounds.get(&implicit).copied()
}

/// The implicit callback type parameter `F` of a `mut ref F` (or bare `F`).
pub(crate) fn callback_param_of(checker: &TypeChecker<'_>, ty: TypeId) -> Option<SymbolId> {
    let ty = match checker.resolve(ty) {
        ArType::Ref(inner) | ArType::RefMut(inner) => checker.resolve(inner),
        other => other,
    };
    match ty {
        ArType::Named(symbol, args)
            if args.is_empty() && checker.type_info.callback_bounds.contains_key(&symbol) =>
        {
            Some(symbol)
        }
        _ => None,
    }
}

/// Argument types for generic inference. A lambda in a callback position is
/// typed after the other arguments, against its bound with the type
/// parameters those arguments already fix; it is never typed without one.
pub(crate) fn synth_args_with_callbacks(
    checker: &mut TypeChecker<'_>,
    type_params: &[SymbolId],
    formals: &[TypeId],
    arg_ids: &[ExprId],
    receiver: &[(SymbolId, TypeId)],
) -> Vec<TypeId> {
    // A generic function waits like a lambda: the bound instantiates it.
    let deferred = |checker: &TypeChecker<'_>, index: usize, arg: ExprId| {
        (matches!(checker.pool.expr(arg), ExprKind::Lambda { .. })
            || is_generic_function(checker, arg))
            && formals
                .get(index)
                .is_some_and(|&formal| callback_bound_of(checker, formal).is_some())
    };
    let mut arg_tys = Vec::with_capacity(arg_ids.len());
    // A method's receiver fixes its type's parameters (`Pair<int>` binds `T`)
    // before any argument, so a callback bound sees them too.
    let mut bindings = receiver
        .iter()
        .copied()
        .collect::<rustc_hash::FxHashMap<_, _>>();
    for (index, &arg) in arg_ids.iter().enumerate() {
        if deferred(checker, index, arg) {
            arg_tys.push(None);
            continue;
        }
        let mut ty = super::synth_expr(checker, arg);
        if let Some(bound) = formals
            .get(index)
            .and_then(|&formal| callback_bound_of(checker, formal))
            && matches!(checker.resolve(ty), ArType::Func(..))
        {
            // A plain function fixes the parameters its callback leaves open
            // (`apply(negate, x)` makes `T` = bool), as its signature is known.
            let open = checker.resolve(bound);
            super::instantiate::bind_type_params(checker, type_params, &open, ty, &mut bindings);
            let bound = substitute_bindings(checker, bound, &bindings);
            ty = adapt_named_function(checker, arg, ty, bound);
            // A function value is itself the callback, called indirectly, as
            // a Rust `fn` pointer implements `FnMut`.
            if matches!(checker.resolve(ty), ArType::Func(..)) && !checker.unify_ids(bound, ty) {
                let interner = &checker.type_info.type_interner;
                let found = checker.resolve(ty).display(&checker.symbols, interner);
                let expected = checker.resolve(bound).display(&checker.symbols, interner);
                let span = checker.pool.expr_span(arg);
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T003IncompatibleCallArg,
                        format!(
                            "function value of type '{found}' cannot be the callback '{expected}'"
                        ),
                        span,
                    )
                    .with_label(span, format!("expected a callback of type '{expected}'")),
                );
                ty = checker.intern(ArType::Error);
                checker.record_expr_type(arg, ty);
            }
        }
        if let Some(bound) = formals
            .get(index)
            .and_then(|&formal| callback_bound_of(checker, formal))
            && let Some(signature) = super::owning::owning_closure_signature(checker, ty)
        {
            let open = checker.resolve(bound);
            super::instantiate::bind_type_params(
                checker,
                type_params,
                &open,
                signature,
                &mut bindings,
            );
            let bound = substitute_bindings(checker, bound, &bindings);
            ty = adapt_owning_closure(checker, arg, ty, signature, bound);
        }
        if let Some(&formal) = formals.get(index) {
            let formal = checker.resolve(formal);
            super::instantiate::bind_type_params(checker, type_params, &formal, ty, &mut bindings);
        }
        arg_tys.push(Some(ty));
    }
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
                let bound = substitute_bindings(checker, bound, &bindings);
                if matches!(checker.pool.expr(arg), ExprKind::Lambda { .. }) {
                    return check_callback_lambda(checker, arg, bound);
                }
                let function_ty = super::synth_expr_expected(checker, arg, Some(bound));
                adapt_named_function(checker, arg, function_ty, bound)
            })
        })
        .collect()
}

fn is_generic_function(checker: &TypeChecker<'_>, arg: ExprId) -> bool {
    matches!(checker.pool.expr(arg), ExprKind::Path { .. })
        && checker.resolved.expr_symbol(arg).is_some_and(|symbol| {
            checker.symbols.try_get(symbol).is_some_and(|entry| {
                matches!(entry.kind, SymbolKind::Func | SymbolKind::AssociatedFunc)
            }) && checker
                .type_info
                .generic_params
                .get(&symbol)
                .is_some_and(|params| !params.is_empty())
        })
}

fn substitute_bindings(
    checker: &mut TypeChecker<'_>,
    ty: TypeId,
    bindings: &rustc_hash::FxHashMap<SymbolId, TypeId>,
) -> TypeId {
    if bindings.is_empty() {
        return ty;
    }
    let (params, args): (Vec<_>, Vec<_>) = bindings.iter().map(|(&p, &a)| (p, a)).unzip();
    let subst =
        arandu_middle::types::build_subst_ids(&params, &args, &checker.type_info.type_interner);
    arandu_middle::types::substitute_type_id(ty, &subst, &checker.type_info.type_interner)
}

/// The type of a call argument after inference. A closure or adapter
/// installed for it keeps its struct type instead of being typed again.
pub(crate) fn synth_call_arg(
    checker: &mut TypeChecker<'_>,
    arg: ExprId,
    formal: Option<TypeId>,
) -> TypeId {
    let span = checker.pool.expr_span(arg);
    if let Some(recipe) = checker.type_info.closure_at(span) {
        if let Some(code) = &recipe.owning {
            let ty = code.closure_ty;
            checker.record_expr_type(arg, ty);
            return ty;
        }
        if recipe.is_callback() {
            let struct_symbol = recipe.struct_symbol;
            if let Some(ty) = checker.decl_type_id(struct_symbol) {
                checker.record_expr_type(arg, ty);
                return ty;
            }
        }
    }
    // A callback argument already rejected keeps its error, no cascade.
    if formal.is_some_and(|formal| callback_bound_of(checker, formal).is_some())
        && let Some(checked) = checker.type_info.expr_type_id(arg)
        && checker.resolve(checked).is_error()
    {
        return checked;
    }
    super::synth_expr_expected(checker, arg, formal)
}

/// A named function passed where a callback is expected becomes a closure
/// without captures whose `call` forwards to it, as Rust passes a `fn` item
/// as an `FnMut`. Its signature must be the callback's.
fn adapt_named_function(
    checker: &mut TypeChecker<'_>,
    arg: ExprId,
    function_ty: TypeId,
    bound: TypeId,
) -> TypeId {
    adapt_function(checker, arg, function_ty, bound, None)
}

/// A named function where `own (A) -> R` is expected: its closure has no
/// environment and its code calls the function (RFC 0032, L3).
pub(crate) fn adapt_function_owning(
    checker: &mut TypeChecker<'_>,
    arg: ExprId,
    function_ty: TypeId,
    closure_ty: TypeId,
    signature: TypeId,
) -> TypeId {
    adapt_function(checker, arg, function_ty, signature, Some(closure_ty))
}

fn adapt_function(
    checker: &mut TypeChecker<'_>,
    arg: ExprId,
    function_ty: TypeId,
    bound: TypeId,
    owning: Option<TypeId>,
) -> TypeId {
    let span = checker.pool.expr_span(arg);
    if let Some(recipe) = checker.type_info.closure_at(span) {
        if let Some(code) = &recipe.owning {
            return code.closure_ty;
        }
        let struct_symbol = recipe.struct_symbol;
        return checker
            .decl_type_id(struct_symbol)
            .unwrap_or_else(|| checker.intern(ArType::Error));
    }
    let Some(function) = checker.resolved.expr_symbol(arg).filter(|&symbol| {
        checker.symbols.try_get(symbol).is_some_and(|entry| {
            matches!(
                entry.kind,
                SymbolKind::Func | SymbolKind::AssociatedFunc | SymbolKind::ExternFunc
            )
        })
    }) else {
        return function_ty;
    };
    let (ArType::Func(params, ret), ArType::Func(bound_params, bound_ret)) =
        (checker.resolve(function_ty), checker.resolve(bound))
    else {
        return function_ty;
    };
    // A generic function is instantiated by the callback it becomes, as Rust
    // infers `identity::<i32>` from the `FnMut(i32) -> i32` it must satisfy.
    let (params, ret) = match checker.type_info.generic_params.get(&function).cloned() {
        Some(generics) if !generics.is_empty() => {
            // Bind against the declaration: the value may arrive instantiated.
            let template = checker.decl_type(function).unwrap_or(ArType::Error);
            let (params, ret) = match template {
                ArType::Func(params, ret) => (params, ret),
                _ => (params, ret),
            };
            let mut bindings = rustc_hash::FxHashMap::default();
            let formals = checker.type_info.type_interner.type_args(params);
            let expected = checker.type_info.type_interner.type_args(bound_params);
            for (&formal, &actual) in formals.iter().zip(&expected) {
                let formal = checker.resolve(formal);
                super::instantiate::bind_type_params(
                    checker,
                    &generics,
                    &formal,
                    actual,
                    &mut bindings,
                );
            }
            let formal = checker.resolve(ret);
            super::instantiate::bind_type_params(
                checker,
                &generics,
                &formal,
                bound_ret,
                &mut bindings,
            );
            let inferred = generics.iter().all(|param| {
                bindings
                    .get(param)
                    .is_some_and(|&ty| !mentions_type_parameter(checker, ty))
            });
            if !inferred {
                let interner = &checker.type_info.type_interner;
                let expected = checker.resolve(bound).display(&checker.symbols, interner);
                let name = checker.symbols.get(function).name.clone();
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T001CannotInferType,
                        format!(
                            "cannot infer the type arguments of `{name}` from the callback '{expected}'"
                        ),
                        span,
                    )
                    .with_label(
                        span,
                        "each of its type parameters must appear in the callback's signature",
                    ),
                );
                let error = checker.intern(ArType::Error);
                checker.record_expr_type(arg, error);
                return error;
            }
            let concrete = generics
                .iter()
                .map(|param| checker.resolve(bindings[param]))
                .collect::<Vec<_>>();
            crate::type_checker::types::interfaces::check_instantiation_constraints(
                checker, &generics, &concrete, span,
            );
            let subst = arandu_middle::types::build_subst(&generics, &concrete);
            let instance = arandu_middle::types::substitute_type(
                &template,
                &subst,
                &checker.type_info.type_interner,
            );
            match instance {
                ArType::Func(params, ret) => (params, ret),
                _ => (params, ret),
            }
        }
        _ => (params, ret),
    };
    let params = checker.type_info.type_interner.type_args(params);
    let bound_params = checker.type_info.type_interner.type_args(bound_params);
    let matches = params.len() == bound_params.len()
        && params
            .iter()
            .zip(&bound_params)
            .all(|(&param, &expected)| checker.unify_ids(expected, param))
        && checker.unify_ids(bound_ret, ret);
    if !matches {
        let interner = &checker.type_info.type_interner;
        let found = checker
            .resolve(function_ty)
            .display(&checker.symbols, interner);
        let expected = checker.resolve(bound).display(&checker.symbols, interner);
        checker.diagnostics.push(if owning.is_some() {
            crate::Diagnostic::error(
                crate::DiagCode::T002IncompatibleAssignment,
                format!("function of type '{found}' cannot be the closure 'own {expected}'"),
                span,
            )
            .with_label(span, format!("expected a closure of type 'own {expected}'"))
        } else {
            crate::Diagnostic::error(
                crate::DiagCode::T003IncompatibleCallArg,
                format!("function of type '{found}' cannot be the callback '{expected}'"),
                span,
            )
            .with_label(span, format!("expected a callback of type '{expected}'"))
        });
        let error = checker.intern(ArType::Error);
        checker.record_expr_type(arg, error);
        return error;
    }
    let method_params = bound_params
        .iter()
        .map(|&ty| (None, ty))
        .collect::<Vec<_>>();
    let struct_ty = install_closure(
        checker,
        span,
        &ClosureShape {
            // An owning adapter is a plain function: it has no environment.
            method: if owning.is_some() { "fn" } else { "call" },
            struct_prefix: "__Adapter",
            host_prefix: "ar_adapter",
            params: &method_params,
            return_type: bound_ret,
            self_by_mut_ref: owning.is_none(),
            adapts: Some(function),
            owning,
        },
        &[],
    );
    let ty = owning.unwrap_or(struct_ty);
    checker.record_expr_type(arg, ty);
    ty
}

/// A function value where `own (A) -> R` is expected: its closure keeps a
/// copy of the code pointer and calls through it (RFC 0032, L3).
pub(crate) fn adapt_value_owning(
    checker: &mut TypeChecker<'_>,
    arg: ExprId,
    local: SymbolId,
    value_ty: TypeId,
    closure_ty: TypeId,
    signature: TypeId,
) -> TypeId {
    let span = checker.pool.expr_span(arg);
    if let Some(code) = checker
        .type_info
        .closure_at(span)
        .and_then(|recipe| recipe.owning.as_ref())
    {
        return code.closure_ty;
    }
    if !checker.unify_ids(signature, value_ty) {
        let interner = &checker.type_info.type_interner;
        let found = checker
            .resolve(value_ty)
            .display(&checker.symbols, interner);
        let expected = checker
            .resolve(signature)
            .display(&checker.symbols, interner);
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T002IncompatibleAssignment,
                format!("function value of type '{found}' cannot be the closure 'own {expected}'"),
                span,
            )
            .with_label(span, format!("expected a closure of type 'own {expected}'")),
        );
        let error = checker.intern(ArType::Error);
        checker.record_expr_type(arg, error);
        return error;
    }
    if super::owning::reject_bare_metal_environment(checker, span) {
        let error = checker.intern(ArType::Error);
        checker.record_expr_type(arg, error);
        return error;
    }
    let ArType::Func(params, ret) = checker.resolve(signature) else {
        return checker.intern(ArType::Error);
    };
    let method_params = checker
        .type_info
        .type_interner
        .type_args(params)
        .iter()
        .map(|&ty| (None, ty))
        .collect::<Vec<_>>();
    let name = checker.symbols.get(local).name.clone();
    let _ = install_closure(
        checker,
        span,
        &ClosureShape {
            method: "call",
            struct_prefix: "__Adapter",
            host_prefix: "ar_adapter",
            params: &method_params,
            return_type: ret,
            self_by_mut_ref: true,
            adapts: Some(local),
            owning: Some(closure_ty),
        },
        &[Capture {
            symbol: local,
            name,
            ty: value_ty,
            span,
            mode: CaptureMode::Copy,
        }],
    );
    checker.record_expr_type(arg, closure_ty);
    closure_ty
}

/// An owning closure passed where a callback is expected is lent for the
/// call: an adapter borrows the local exclusively and forwards each call to
/// it, as `(x) => c(x)` would (RFC 0032, L3).
fn adapt_owning_closure(
    checker: &mut TypeChecker<'_>,
    arg: ExprId,
    closure_ty: TypeId,
    signature: TypeId,
    bound: TypeId,
) -> TypeId {
    let span = checker.pool.expr_span(arg);
    if let Some(recipe) = checker.type_info.closure_at(span) {
        let struct_symbol = recipe.struct_symbol;
        return checker
            .decl_type_id(struct_symbol)
            .unwrap_or_else(|| checker.intern(ArType::Error));
    }
    let error = checker.intern(ArType::Error);
    let interner = &checker.type_info.type_interner;
    let expected = checker.resolve(bound).display(&checker.symbols, interner);
    if !checker.unify_ids(bound, signature) {
        let found = checker
            .resolve(closure_ty)
            .display(&checker.symbols, &checker.type_info.type_interner);
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T003IncompatibleCallArg,
                format!("closure of type '{found}' cannot be the callback '{expected}'"),
                span,
            )
            .with_label(span, format!("expected a callback of type '{expected}'")),
        );
        checker.record_expr_type(arg, error);
        return error;
    }
    let local = matches!(checker.pool.expr(arg), ExprKind::Path { .. })
        .then(|| checker.resolved.expr_symbol(arg))
        .flatten()
        .filter(|&symbol| {
            checker
                .symbols
                .try_get(symbol)
                .is_some_and(|entry| matches!(entry.kind, SymbolKind::Local | SymbolKind::Param))
        });
    let Some(local) = local else {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T003IncompatibleCallArg,
                format!(
                    "this closure must be stored before it is lent as the callback '{expected}'"
                ),
                span,
            )
            .with_label(span, "a callback borrows the closure for the call")
            .with_hint("bind it first: `let f = ...`, then pass `f`"),
        );
        checker.record_expr_type(arg, error);
        return error;
    };
    let ArType::Func(params, ret) = checker.resolve(bound) else {
        return error;
    };
    let method_params = checker
        .type_info
        .type_interner
        .type_args(params)
        .iter()
        .map(|&ty| (None, ty))
        .collect::<Vec<_>>();
    let name = checker.symbols.get(local).name.clone();
    let struct_ty = install_closure(
        checker,
        span,
        &ClosureShape {
            method: "call",
            struct_prefix: "__Adapter",
            host_prefix: "ar_adapter",
            params: &method_params,
            return_type: ret,
            self_by_mut_ref: true,
            adapts: Some(local),
            owning: None,
        },
        &[Capture {
            symbol: local,
            name,
            ty: closure_ty,
            span,
            mode: CaptureMode::BorrowMut,
        }],
    );
    checker.record_expr_type(arg, struct_ty);
    struct_ty
}

/// Every argument bound to a callback parameter must be a closure, or a
/// callback parameter of the caller being passed on.
pub(crate) fn check_callback_args(
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
            // Checked against the bound where its type was synthesized.
            ArType::Error | ArType::Func(..) => true,
            _ if callback_param_of(checker, arg_ty).is_some() => true,
            ArType::Named(symbol, _) => checker
                .type_info
                .closures
                .values()
                .any(|recipe| recipe.struct_symbol == symbol),
            _ => false,
        };
        if accepted {
            continue;
        }
        let span = checker.pool.expr_span(arg);
        let interner = &checker.type_info.type_interner;
        let bound_text = checker.resolve(bound).display(&checker.symbols, interner);
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T003IncompatibleCallArg,
                format!(
                    "expected a callback of type '{bound_text}', found '{}'",
                    actual.display(&checker.symbols, interner)
                ),
                span,
            )
            .with_label(span, "this value is not callable"),
        );
    }
}

/// Type a lambda passed where a callback `bound` is expected and install its
/// closure struct. Returns the struct type, the argument's type.
/// What a lambda becomes: a borrowed callback environment (L1), a function
/// value without captures (L2), or an owning closure (L3).
#[derive(Clone, Copy)]
enum LambdaUse {
    Callback,
    Value,
    /// `Closure<F>` or `SendClosure<F>`.
    Owning(TypeId),
}

pub(super) fn check_callback_lambda(
    checker: &mut TypeChecker<'_>,
    arg_id: ExprId,
    bound: TypeId,
) -> TypeId {
    check_lambda(checker, arg_id, bound, LambdaUse::Callback)
}

/// A lambda whose expected type is `(A) -> R` outside a callback position
/// is a function value. Without captures it becomes a function of its own;
/// a function value holds no environment, so a capture is rejected until
/// `own (A) -> R` (L3).
pub(crate) fn check_lambda_value(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    func_ty: TypeId,
) -> TypeId {
    check_lambda(checker, expr, func_ty, LambdaUse::Value)
}

/// A lambda whose expected type is `own (A) -> R [+ Send]`: its captures
/// move into a heap environment the closure owns (RFC 0032, L3).
pub(crate) fn check_lambda_owning(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    closure_ty: TypeId,
    signature: TypeId,
) -> TypeId {
    check_lambda(checker, expr, signature, LambdaUse::Owning(closure_ty))
}

fn check_lambda(
    checker: &mut TypeChecker<'_>,
    arg_id: ExprId,
    bound: TypeId,
    usage: LambdaUse,
) -> TypeId {
    let lambda_span = checker.pool.expr_span(arg_id);
    if let Some(recipe) = checker.type_info.closure_at(lambda_span) {
        if let Some(code) = &recipe.owning {
            return code.closure_ty;
        }
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
    // Modes need the typed body: a call through a `mut` receiver mutates.
    let owning = match usage {
        LambdaUse::Owning(closure_ty) => Some(closure_ty),
        LambdaUse::Callback | LambdaUse::Value => None,
    };
    let Some(captures) = callback_captures(checker, lambda_span, is_move || owning.is_some())
    else {
        checker.record_expr_type(arg_id, error);
        return error;
    };
    if rejected
        || owning.is_some_and(|closure_ty| {
            !super::owning::admit_owning_captures(checker, closure_ty, &captures)
        })
    {
        checker.record_expr_type(arg_id, error);
        return error;
    }
    if let Some(closure_ty) = owning {
        // Without captures the closure's code calls the lambda's own function
        // and needs no environment.
        let (method, struct_prefix, host_prefix, self_by_mut_ref) = if captures.is_empty() {
            ("fn", "__Lambda", "ar_lambda", false)
        } else {
            ("call", "__Closure", "ar_closure", true)
        };
        let _ = install_closure(
            checker,
            lambda_span,
            &ClosureShape {
                method,
                struct_prefix,
                host_prefix,
                params: &method_params,
                return_type,
                self_by_mut_ref,
                adapts: None,
                owning: Some(closure_ty),
            },
            &captures,
        );
        checker.record_expr_type(arg_id, closure_ty);
        return closure_ty;
    }
    if matches!(usage, LambdaUse::Value) {
        if let Some(capture) = captures.first() {
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::T002IncompatibleAssignment,
                    format!(
                        "a closure that captures '{}' is not a function value of type '{bound_text}'",
                        capture.name
                    ),
                    capture.span,
                )
                .with_label(lambda_span, "a function value holds no environment")
                .with_hint(format!(
                    "use the owning closure type `own {bound_text}` to keep the captures"
                )),
            );
            checker.record_expr_type(arg_id, error);
            return error;
        }
        let _ = install_closure(
            checker,
            lambda_span,
            &ClosureShape {
                method: "fn",
                struct_prefix: "__Lambda",
                host_prefix: "ar_lambda",
                params: &method_params,
                return_type,
                self_by_mut_ref: false,
                adapts: None,
                owning: None,
            },
            &[],
        );
        checker.record_expr_type(arg_id, bound);
        return bound;
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
            self_by_mut_ref: true,
            adapts: None,
            owning: None,
        },
        &captures,
    );
    checker.record_expr_type(arg_id, struct_ty);
    struct_ty
}

/// Deduce how a callback holds each captured local (RFC 0032, L1):
/// `move` moves every capture; otherwise a local that is assigned, mutably
/// borrowed or used as a `mut` receiver is a `BorrowMut`, a `Copy` value of
/// an immutable binding is copied, and anything else is a `Borrow`. The
/// borrows are real: the environment holds references to the caller's
/// locals, and the borrow checker sees them live for the whole call.
fn callback_captures(
    checker: &mut TypeChecker<'_>,
    lambda_span: Span,
    is_move: bool,
) -> Option<Vec<Capture>> {
    let mutated = mutated_within(checker, lambda_span);
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
        // The environment inherits the enclosing item's type parameters; a
        // parameter from anywhere else has no instance to bind it.
        if mentions_foreign_type_parameter(checker, ty) {
            rejected = true;
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::U001FeatureNotSupported,
                    format!(
                        "capturing '{name}', whose type names a type parameter outside the enclosing function, is not supported"
                    ),
                    use_span,
                )
                .with_label(use_span, format!("'{name}' is captured here"))
                .with_hint("pass it to the callee directly instead of capturing it"),
            );
            continue;
        }
        // An exclusive reference is borrowed uniquely, as Rust captures
        // `&mut`: the closure reaches it through the environment and the
        // caller keeps it after the call.
        let exclusive = matches!(checker.resolve(ty), ArType::RefMut(_));
        let mode = if is_move {
            CaptureMode::Move
        } else if exclusive || mutated.contains(&symbol) {
            CaptureMode::BorrowMut
        } else if checker.type_info.is_copy(ty)
            && !checker.resolved.mutable_symbols.contains(&symbol)
        {
            CaptureMode::Copy
        } else {
            CaptureMode::Borrow
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

/// Locals written inside the lambda: assignment roots, `mut ref` borrows
/// and receivers of methods that take `mut self`, by the root of the place.
fn mutated_within(checker: &TypeChecker<'_>, lambda_span: Span) -> FxHashSet<SymbolId> {
    let inside = |span: Span| super::thread_target::span_contains(lambda_span, span);
    let mut mutated = checker
        .resolved
        .value_refs
        .iter()
        .filter(|(key, _)| inside(Span::new(lambda_span.file_id, key.start, key.end)))
        .map(|(_, symbol)| *symbol)
        .collect::<FxHashSet<_>>();
    for (index, &span) in checker.pool.expr_spans.iter().enumerate() {
        if span == lambda_span || !inside(span) {
            continue;
        }
        let place = match checker.pool.expr(ExprId::new(index)) {
            ExprKind::Unary {
                op: arandu_parser::UnaryOp::RefMut,
                expr,
            } => Some(*expr),
            ExprKind::Call { callee, .. } => match checker.pool.expr(*callee) {
                ExprKind::Field { base, .. }
                    if checker
                        .type_info
                        .expr_type(*callee)
                        .and_then(|method| match method {
                            ArType::Func(params, _) => checker
                                .type_info
                                .type_interner
                                .type_args(params)
                                .first()
                                .copied(),
                            _ => None,
                        })
                        .is_some_and(|receiver| {
                            matches!(checker.resolve(receiver), ArType::RefMut(_))
                        }) =>
                {
                    Some(*base)
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(root) = place.and_then(|place| place_root(checker, place)) {
            mutated.insert(root);
        }
    }
    mutated
}

fn place_root(checker: &TypeChecker<'_>, expr: ExprId) -> Option<SymbolId> {
    match checker.pool.expr(expr) {
        ExprKind::Path { .. } => checker.resolved.expr_symbol(expr),
        ExprKind::Field { base, .. } | ExprKind::Index { base, .. } => place_root(checker, *base),
        ExprKind::Group { expr, .. } => place_root(checker, *expr),
        _ => None,
    }
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

/// A closure is identified by the item that encloses it and its offset in
/// that item, never by its absolute position: an edit elsewhere in the file
/// keeps its symbols and names, and so the early cutoff of what uses them.
struct ClosureIdentity {
    owner_hash: u32,
    start: u32,
    end: u32,
    /// `_<owner>_<start>_<end>`, or `_<start>_<end>` outside any item.
    suffix: String,
}

impl ClosureIdentity {
    fn of(checker: &TypeChecker<'_>, span: Span) -> Self {
        let Some(owner) = &checker.closure_owner else {
            return Self {
                owner_hash: 0,
                start: span.start,
                end: span.end,
                suffix: format!("_{}_{}", span.start, span.end),
            };
        };
        let start = span.start.saturating_sub(owner.start);
        let end = span.end.saturating_sub(owner.start);
        // FNV-1a: stable across runs and platforms.
        let owner_hash = owner.name.bytes().fold(0x811C_9DC5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
        Self {
            owner_hash,
            start,
            end,
            suffix: format!("_{}_{start}_{end}", owner.name),
        }
    }
}

fn mint(
    checker: &TypeChecker<'_>,
    file_id: arandu_middle::db::FileId,
    identity: &ClosureIdentity,
    mut role: u32,
    reserved: &mut FxHashSet<u32>,
) -> SymbolId {
    loop {
        let mixed = identity.start.wrapping_mul(0x9E37_79B1)
            ^ identity.end.wrapping_mul(0x85EB_CA6B)
            ^ identity.owner_hash.wrapping_mul(0x27D4_EB2F)
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

/// A type parameter that is not one of the enclosing item's, which a
/// closure environment could not inherit.
fn mentions_foreign_type_parameter(checker: &TypeChecker<'_>, ty: TypeId) -> bool {
    let owned = checker
        .closure_owner
        .as_ref()
        .map(|owner| owner.generics.as_slice())
        .unwrap_or_default();
    let Ok(shape) = arandu_middle::types::TypeShape::from_id(ty, &checker.type_info.type_interner)
    else {
        return false;
    };
    let mut found = false;
    let _ = shape.for_each_symbol(|symbol| {
        found |= !owned.contains(&symbol)
            && checker
                .symbols
                .try_get(symbol)
                .is_some_and(|entry| entry.kind == SymbolKind::TypeParam);
    });
    found
}

/// The enclosing item's type parameters when the closure's captures or
/// signature mention any: its environment, method and code are generic over
/// them, and each use instantiates them with the item's own (RFC 0032, L3).
fn closure_generics(
    checker: &TypeChecker<'_>,
    captures: &[Capture],
    params: &[ClosureParam],
    return_type: TypeId,
) -> Vec<SymbolId> {
    let generic = captures
        .iter()
        .map(|capture| capture.ty)
        .chain(params.iter().map(|param| param.ty))
        .chain(std::iter::once(return_type))
        .any(|ty| mentions_type_parameter(checker, ty));
    if !generic {
        return Vec::new();
    }
    checker
        .closure_owner
        .as_ref()
        .map(|owner| owner.generics.clone())
        .unwrap_or_default()
}

pub(super) fn mentions_type_parameter(checker: &TypeChecker<'_>, ty: TypeId) -> bool {
    let Ok(shape) = arandu_middle::types::TypeShape::from_id(ty, &checker.type_info.type_interner)
    else {
        return false;
    };
    let mut found = false;
    let _ = shape.for_each_symbol(|symbol| {
        found |= checker
            .symbols
            .try_get(symbol)
            .is_some_and(|entry| entry.kind == SymbolKind::TypeParam);
    });
    found
}
