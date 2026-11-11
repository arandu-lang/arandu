//! Temporaries a call reads after evaluating them (RFC 0032, L3).
//!
//! An owning closure called while still a temporary (`make()(x)`) is read
//! twice, for its code and its environment; one lent to a callback is
//! borrowed by the adapter for the duration of the call. The call becomes a
//! scope that first evaluates the temporary into a local, then makes the
//! call; the local drops when the scope ends, right after the call.
//! Arguments evaluated before a hoisted one are hoisted with it, so the
//! source's left-to-right evaluation order is kept.

use crate::TypeCheckResult;
use crate::diagnostics::Diagnostic;
use crate::hir::{HirBindingItem, HirBlock, HirExpr, HirExprKind, HirStmt, HirStmtKind};
use arandu_middle::{Span, Symbol, SymbolId, SymbolKind, Visibility};
use arandu_parser::ast_pool::{AstPool, ExprId, ExprKind};
use arandu_typeck::type_checker::info::ClosureTemporary;

/// Lowers `expr`, a call, as a scope when it reads a temporary the type
/// checker bound to a local; `None` when it reads none.
pub(super) fn lower_call_with_temporaries(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    expr: ExprId,
    span: Span,
) -> Result<Option<HirExpr>, Diagnostic> {
    if type_check.type_info.closure_temporaries.is_empty() {
        return Ok(None);
    }
    let ExprKind::Call { callee, args, .. } = pool.expr(expr) else {
        return Ok(None);
    };
    let callee = *callee;
    let arg_ids = pool.expr_list(*args).to_vec();
    let pending = |hir_pool: &crate::hir::HirPool, value: ExprId| {
        let span = pool.expr_span(value);
        type_check
            .type_info
            .closure_temporaries
            .get(&span)
            .copied()
            .filter(|_| !hir_pool.hoisted_values.contains_key(&span))
    };
    let called = pending(hir_pool, callee);
    let last_lent = arg_ids
        .iter()
        .rposition(|&arg| pending(hir_pool, arg).is_some());
    if called.is_none() && last_lent.is_none() {
        return Ok(None);
    }

    // Evaluation order: the callee (or a method's receiver), then arguments.
    let mut hoisted: Vec<(ExprId, Option<ClosureTemporary>)> = Vec::new();
    if let Some(local) = called {
        hoisted.push((callee, Some(local)));
    } else if let Some(receiver) = method_receiver(pool, callee)
        && !is_pure(pool, receiver)
    {
        hoisted.push((receiver, None));
    }
    if let Some(last) = last_lent {
        for &arg in &arg_ids[..=last] {
            match pending(hir_pool, arg) {
                Some(local) => hoisted.push((arg, Some(local))),
                None if !is_pure(pool, arg) => hoisted.push((arg, None)),
                None => {}
            }
        }
    }

    let mut statements = Vec::with_capacity(hoisted.len() + 1);
    let mut spans = Vec::with_capacity(hoisted.len());
    for (value_expr, local) in hoisted {
        let value_span = pool.expr_span(value_expr);
        let previous = hir_pool.lowering_temporary.replace(value_span);
        let value = super::lower_expr(type_check, pool, hir_pool, value_expr);
        hir_pool.lowering_temporary = previous;
        let value = value?;
        // A temporary lent to a callback is checked as its adapter; the
        // local holds the closure itself.
        let (local, ty) = match local {
            Some(temporary) => {
                hir_pool.exprs[value].ty = temporary.ty;
                (temporary.local, temporary.ty)
            }
            None => (
                synthetic_local(type_check, value_span, "value"),
                hir_pool.expr(value).ty,
            ),
        };
        let bindings = hir_pool.alloc_binding_list(&[HirBindingItem {
            symbol: local,
            ty,
            span: value_span,
        }]);
        statements.push(hir_pool.alloc_stmt(HirStmt {
            kind: HirStmtKind::VarDecl { bindings, value },
            span: value_span,
        }));
        hir_pool.hoisted_values.insert(value_span, (local, ty));
        spans.push(value_span);
    }
    let call = super::lower_expr_raw(type_check, pool, hir_pool, expr);
    for value_span in spans {
        hir_pool.hoisted_values.remove(&value_span);
    }
    let call = call?;
    let ty = call.ty;
    let call = hir_pool.alloc_expr(call);
    statements.push(hir_pool.alloc_stmt(HirStmt {
        kind: HirStmtKind::Expr(call),
        span,
    }));
    let statements = hir_pool.alloc_stmt_list(&statements);
    let block = hir_pool.alloc_block(HirBlock { statements, span });
    Ok(Some(HirExpr {
        kind: HirExprKind::Scope { block },
        ty,
        span,
    }))
}

/// The receiver of `base.method(..)`, which the call evaluates first.
fn method_receiver(pool: &AstPool, callee: ExprId) -> Option<ExprId> {
    let field = match pool.expr(callee) {
        ExprKind::Generic { callee, .. } => *callee,
        _ => callee,
    };
    match pool.expr(field) {
        ExprKind::Field { base, .. } | ExprKind::SafeField { base, .. } => Some(*base),
        _ => None,
    }
}

/// Evaluating it has no effect, so moving it after a hoisted argument is
/// unobservable: a name, a literal or a field of one.
fn is_pure(pool: &AstPool, expr: ExprId) -> bool {
    match pool.expr(expr) {
        ExprKind::Path { .. }
        | ExprKind::TypePath { .. }
        | ExprKind::Int { .. }
        | ExprKind::Float { .. }
        | ExprKind::Bool { .. }
        | ExprKind::Char { .. }
        | ExprKind::Byte { .. }
        | ExprKind::ByteString { .. }
        | ExprKind::CString { .. }
        | ExprKind::Nil => true,
        ExprKind::Field { base, .. } | ExprKind::Group { expr: base } => is_pure(pool, *base),
        _ => false,
    }
}

/// A local lowering introduces at `span`, named `name` (a hoisted argument,
/// the parts of a `once` call). Its id derives from the span and the name,
/// like the type checker's temporaries, so lowering the same item twice
/// yields the same HIR.
pub(super) fn synthetic_local(
    type_check: &mut TypeCheckResult,
    span: Span,
    name: &str,
) -> SymbolId {
    let file_id = type_check.symbols.file_id;
    // FNV-1a of the name keeps distinct locals of one span apart.
    let mut role = name.bytes().fold(0x811C_9DC5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    loop {
        let mixed = span.start.wrapping_mul(0x9E37_79B1)
            ^ span.end.wrapping_mul(0x85EB_CA6B)
            ^ role.wrapping_mul(0xC2B2_AE35);
        let id = SymbolId::new(
            file_id,
            SymbolId::SYNTHETIC_LOCAL_BIT | (mixed & 0x7FFF_FFFF),
        );
        match type_check.symbols.try_get(id) {
            None => {
                let scope = type_check.symbols.global_scope();
                type_check.symbols_mut().register_synthetic(Symbol {
                    id,
                    name: name.into(),
                    kind: SymbolKind::Local,
                    span,
                    scope,
                    visibility: Visibility::Public,
                    lang_item: None,
                });
                return id;
            }
            Some(existing)
                if existing.span == span
                    && existing.kind == SymbolKind::Local
                    && existing.name == name =>
            {
                return id;
            }
            Some(_) => {}
        }
        role = role.wrapping_add(0x1_0000);
    }
}
