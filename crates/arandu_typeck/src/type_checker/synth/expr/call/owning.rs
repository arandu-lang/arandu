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
        // The call reads `code` and `env`: a temporary is evaluated once into
        // a local that lives until the end of the statement.
        None if !is_place(checker, callee) => {
            super::closure::bind_closure_temporary(checker, callee, callee_ty);
        }
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
    if !captures.is_empty() && reject_bare_metal_environment(checker, captures[0].span) {
        return false;
    }
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

/// Records that the expression's value is passed to `function<type_args>` to
/// become `to`, and returns `to`.
fn record_adjustment(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    function: arandu_middle::SymbolId,
    type_args: Vec<TypeId>,
    from: TypeId,
    to: TypeId,
) -> TypeId {
    let span = checker.pool.expr_span(expr);
    checker.type_info.adjustments.insert(
        span,
        crate::type_checker::info::Adjustment {
            function,
            type_args,
            from,
            to,
        },
    );
    to
}

/// The coercion of an owning closure to the type expected where it is used:
/// `own (A) -> R + Send` forgets `Send` and becomes `own (A) -> R`.
pub(crate) fn adjust_to_expected(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    actual: TypeId,
    expected: Option<TypeId>,
) -> TypeId {
    let span = checker.pool.expr_span(expr);
    if let Some(adjustment) = checker.type_info.adjustments.get(&span) {
        return adjustment.to;
    }
    let Some(expected) = expected else {
        return actual;
    };
    let (ArType::Named(sent, _), ArType::Named(local, _)) =
        (checker.resolve(actual), checker.resolve(expected))
    else {
        return actual;
    };
    let item = |symbol| {
        checker
            .symbols
            .try_get(symbol)
            .and_then(|symbol| symbol.lang_item)
    };
    if item(sent) != Some(LangItem::SendClosure) || item(local) != Some(LangItem::Closure) {
        return actual;
    }
    let (Some(from_code), Some(to_code)) = (
        owning_closure_code(checker, actual),
        owning_closure_code(checker, expected),
    ) else {
        return actual;
    };
    let Some(&function) = checker
        .symbols
        .associated_members
        .get(&(sent, "local".into()))
    else {
        return actual;
    };
    if !checker.unify_ids(from_code, to_code) {
        return actual;
    }
    record_adjustment(checker, expr, function, vec![to_code], actual, expected)
}

/// `thread.spawn(job)` with an owning `Send` closure `job` without
/// parameters runs it as a `ClosureJob<R>`, the thread job for closures.
pub(super) fn adjust_thread_job(checker: &mut TypeChecker<'_>, arg: ExprId) {
    if matches!(checker.pool.expr(arg), ExprKind::Lambda { .. }) {
        return;
    }
    let actual = super::synth_expr(checker, arg);
    let ArType::Named(symbol, _) = checker.resolve(actual) else {
        return;
    };
    if checker
        .symbols
        .try_get(symbol)
        .and_then(|symbol| symbol.lang_item)
        != Some(LangItem::SendClosure)
    {
        return;
    }
    let Some(code) = owning_closure_code(checker, actual) else {
        return;
    };
    let ArType::Func(params, ret) = checker.resolve(code) else {
        return;
    };
    if checker.type_info.type_interner.type_args(params).len() != 1 {
        return;
    }
    let Some(job) = checker.symbols.lang_item_symbol(LangItem::ClosureJob) else {
        return;
    };
    let Some(&function) = checker.symbols.associated_members.get(&(job, "new".into())) else {
        return;
    };
    let job_ty = ArType::named(job, &[ret], &checker.type_info.type_interner);
    let job_ty = checker.intern(job_ty);
    let _ = record_adjustment(checker, arg, function, vec![ret], actual, job_ty);
}

/// A closure with captures allocates its environment through the host
/// allocator, which a bare-metal target does not have. Reports and returns
/// true there.
pub(super) fn reject_bare_metal_environment(
    checker: &mut TypeChecker<'_>,
    span: arandu_lexer::Span,
) -> bool {
    let identity = &checker.type_info.target_identity;
    if !identity.is_bare_metal() {
        return false;
    }
    let target = if identity.triple.is_empty() {
        format!("{}-{}", identity.arch, identity.os)
    } else {
        identity.triple.clone()
    };
    checker.diagnostics.push(
        crate::Diagnostic::error(
            crate::DiagCode::T099RequiresOperatingSystem,
            format!(
                "an owning closure with captures allocates its environment, but target '{target}' is bare-metal and has no host allocator"
            ),
            span,
        )
        .with_primary_label("this capture needs a heap environment")
        .with_hint("capture nothing: a closure without captures allocates nothing"),
    );
    true
}
