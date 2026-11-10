//! Owning closures, `own (A) -> R [+ Send]` (RFC 0032, L3).
//!
//! The type is `Closure<F>` or `SendClosure<F>` from `std.alloc.closure`,
//! where `F` is the code's signature `(ptr[u8], A) -> R`. A call `c(args)`
//! lowers to `c.code(c.env, args)`: an indirect call that may change the
//! captures behind `env`. Only the owner or a `mut ref` reaches them, so
//! either may call; a shared `ref` may not, which rules out a second call
//! running while the first one changes the captures.

use arandu_middle::symbol_table::LangItem;
use arandu_parser::ast_pool::{ExprId, ExprKind};

use crate::type_checker::TypeChecker;
use crate::type_checker::types::{ArType, TypeId};

/// `Closure` or `SendClosure`, with the code signature it stores.
pub(crate) fn owning_closure_code(checker: &TypeChecker<'_>, ty: TypeId) -> Option<TypeId> {
    let ArType::Named(symbol, args) = checker.resolve(ty) else {
        return None;
    };
    let item = checker.symbols.try_get(symbol)?.lang_item?;
    if !matches!(item, LangItem::Closure | LangItem::SendClosure) {
        return None;
    }
    match checker.type_info.type_interner.type_args(args).as_slice() {
        &[code] => Some(code),
        _ => None,
    }
}

/// The parameters (without the environment) and result of a call through an
/// owning closure, after checking that the callee may call it.
pub(super) fn closure_call_signature(
    checker: &mut TypeChecker<'_>,
    callee: ExprId,
    callee_ty: TypeId,
) -> Option<(Vec<TypeId>, TypeId)> {
    let (closure_ty, through) = match checker.resolve(callee_ty) {
        ArType::Ref(inner) => (inner, Some(false)),
        ArType::RefMut(inner) => (inner, Some(true)),
        _ => (callee_ty, None),
    };
    let code = owning_closure_code(checker, closure_ty)?;
    let ArType::Func(params, ret) = checker.resolve(code) else {
        return None;
    };
    let params = checker.type_info.type_interner.type_args(params);
    let params = params.get(1..).unwrap_or_default().to_vec();
    let span = checker.pool.expr_span(callee);
    match through {
        Some(true) => {}
        Some(false) => checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T026CannotAssignImmutable,
                "cannot call an owning closure through a shared reference",
                span,
            )
            .with_primary_label("calling may change the closure's captures")
            .with_hint("pass the closure as `mut ref`"),
        ),
        None if !is_place(checker, callee) => checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::T026CannotAssignImmutable,
                "an owning closure must be stored before it is called",
                span,
            )
            .with_primary_label("this closure is a temporary value")
            .with_hint("bind it first: `let f = ...; f(...)`"),
        ),
        None => {}
    }
    Some((params, ret))
}

/// A local or parameter, or a field or group of one: the call reads it twice
/// (`code` and `env`), which must not evaluate anything twice.
fn is_place(checker: &TypeChecker<'_>, expr: ExprId) -> bool {
    match checker.pool.expr(expr) {
        ExprKind::Path { .. } => true,
        ExprKind::Field { base, .. } | ExprKind::Group { expr: base } => is_place(checker, *base),
        _ => false,
    }
}

/// `(A) -> R` for `own (A) -> R`: the code signature without its environment.
pub(crate) fn owning_closure_signature(
    checker: &mut TypeChecker<'_>,
    ty: TypeId,
) -> Option<TypeId> {
    let code = owning_closure_code(checker, ty)?;
    let ArType::Func(params, ret) = checker.resolve(code) else {
        return None;
    };
    let params = checker.type_info.type_interner.type_args(params);
    let signature = ArType::func(params.get(1..)?, ret, &checker.type_info.type_interner);
    Some(checker.intern(signature))
}

/// An owning closure outlives the scope that created it, so no capture may
/// borrow; a `Send` closure must also be able to move each capture to another
/// thread. Reports every rejected capture.
pub(super) fn admit_owning_captures(
    checker: &mut TypeChecker<'_>,
    closure_ty: TypeId,
    captures: &[super::closure::Capture],
) -> bool {
    let send = matches!(
        checker.resolve(closure_ty),
        ArType::Named(symbol, _)
            if checker.symbols.try_get(symbol).and_then(|symbol| symbol.lang_item)
                == Some(LangItem::SendClosure)
    );
    let mut admitted = true;
    for capture in captures {
        let shown = checker
            .resolve(capture.ty)
            .display(&checker.symbols, &checker.type_info.type_interner);
        if checker.type_info.contains_borrow(capture.ty) {
            admitted = false;
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::O010EscapeOfBorrowedValue,
                    format!(
                        "an owning closure cannot capture '{}' of type '{shown}': it borrows a value the closure may outlive",
                        capture.name
                    ),
                    capture.span,
                )
                .with_primary_label(format!("'{}' is captured here", capture.name))
                .with_hint("capture an owned value instead, such as a `String` or a `Vec`"),
            );
            continue;
        }
        let ty = checker.resolve(capture.ty);
        if send && !crate::type_checker::types::transfer::satisfies(checker, &ty, LangItem::Send) {
            admitted = false;
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::T025InterfaceNotSatisfied,
                    format!(
                        "captured variable '{}' of type '{shown}' does not satisfy 'marker.Send'",
                        capture.name
                    ),
                    capture.span,
                )
                .with_primary_label("a `Send` closure may move to another thread")
                .with_hint("drop `+ Send` from the closure type, or capture a `Send` value"),
            );
        }
    }
    admitted
}
