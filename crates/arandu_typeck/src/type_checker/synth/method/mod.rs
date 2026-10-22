//! Method call synthesis, auto-ref / auto-deref, and receiver generic instantiation.

mod call;
mod sig;

use arandu_middle::SymbolId;
use arandu_parser::ast_pool::{ExprId, ExprKind};

use super::super::TypeChecker;
use super::super::types::{ArType, TypeId};

pub(crate) use call::synth_method_call;

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
        ArType::Slice(elem) | ArType::MutSlice(elem) => vec![checker.resolve(elem)],
        ArType::Ptr(inner) => vec![checker.resolve(inner)],
        ArType::Nullable(inner) => match checker.resolve(inner) {
            ArType::Ptr(elem) => vec![checker.resolve(elem)],
            _ => return (params, ret),
        },
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
        .map(|&p| {
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

fn bind_receiver_type_arg(
    checker: &TypeChecker<'_>,
    formal_id: TypeId,
    actual_id: TypeId,
    candidate_gp: &[arandu_middle::SymbolId],
    param_syms: &mut Vec<arandu_middle::SymbolId>,
    bound_args: &mut Vec<ArType>,
) {
    let formal_ty = checker.resolve(formal_id);
    let actual_ty = checker.resolve(actual_id);
    match (&formal_ty, &actual_ty) {
        (ArType::Named(sym, a), _) if a.is_empty() && candidate_gp.contains(sym) => {
            if !param_syms.contains(sym) {
                param_syms.push(*sym);
                bound_args.push(actual_ty);
            }
        }
        (ArType::ConstParam(sym), _) if candidate_gp.contains(sym) => {
            if !param_syms.contains(sym) {
                param_syms.push(*sym);
                bound_args.push(actual_ty);
            }
        }
        (ArType::Option(f_inner), ArType::Option(a_inner))
        | (ArType::Slice(f_inner), ArType::Slice(a_inner))
        | (ArType::MutSlice(f_inner), ArType::MutSlice(a_inner))
        | (ArType::Ref(f_inner), ArType::Ref(a_inner))
        | (ArType::RefMut(f_inner), ArType::RefMut(a_inner))
        | (ArType::Ptr(f_inner), ArType::Ptr(a_inner))
        | (ArType::Nullable(f_inner), ArType::Nullable(a_inner)) => {
            bind_receiver_type_arg(
                checker,
                *f_inner,
                *a_inner,
                candidate_gp,
                param_syms,
                bound_args,
            );
        }
        (ArType::Result(f_ok, f_err), ArType::Result(a_ok, a_err)) => {
            bind_receiver_type_arg(checker, *f_ok, *a_ok, candidate_gp, param_syms, bound_args);
            bind_receiver_type_arg(
                checker,
                *f_err,
                *a_err,
                candidate_gp,
                param_syms,
                bound_args,
            );
        }
        (ArType::Named(f_sym, f_args), ArType::Named(a_sym, a_args))
            if f_sym == a_sym && f_args.len == a_args.len =>
        {
            let f_ids = checker.type_info.type_interner.type_args(*f_args);
            let a_ids = checker.type_info.type_interner.type_args(*a_args);
            for (&f, &a) in f_ids.iter().zip(a_ids.iter()) {
                bind_receiver_type_arg(checker, f, a, candidate_gp, param_syms, bound_args);
            }
        }
        _ => {}
    }
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
        ArType::ConstParam(id) => gp.contains(id),
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
        | ArType::MutSlice(inner)
        | ArType::Ptr(inner)
        | ArType::Ref(inner)
        | ArType::RefMut(inner)
        | ArType::Option(inner)
        | ArType::Coroutine(inner)
        | ArType::Poll(inner)
        | ArType::Range(inner)
        | ArType::Array(_, inner)
        | ArType::ConstArray(_, inner) => {
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
