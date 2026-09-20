//! Call-site rewrite for monomorphized specializations.
//!
//! `Call(Generic(Path(F), [T…]), args)` → `Call(Path(F_spec), args)`.

use arandu_middle::hir::{
    HirBlockId, HirCatchHandler, HirCondition, HirExpr, HirExprId, HirExprKind, HirForClause,
    HirLambdaBody, HirMatchArmBody, HirPlaceSuffix, HirProgram, HirStmtKind, HirStringPart,
};
use arandu_middle::symbol_table::SymbolId;
use arandu_middle::types::ArType;
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

use super::super::graph::InstantiationKey;

// ── Call-site rewrite ───────────────────────────────────────────────────────

pub(super) fn rewrite_block_calls<'bump>(
    hir: &mut HirProgram,
    block_id: HirBlockId,
    specialized: &FxHashMap<InstantiationKey<'bump>, SymbolId>,
    tc: &TypeCheckResult,
    bump: &'bump bumpalo::Bump,
) {
    let stmt_ids: Vec<_> = hir
        .pool
        .stmt_list(hir.pool.block(block_id).statements)
        .to_vec();
    for sid in stmt_ids {
        rewrite_stmt_calls(hir, sid, specialized, tc, bump);
    }
}

pub(super) fn rewrite_stmt_calls<'bump>(
    hir: &mut HirProgram,
    stmt_id: arandu_middle::hir::HirStmtId,
    specialized: &FxHashMap<InstantiationKey<'bump>, SymbolId>,
    tc: &TypeCheckResult,
    bump: &'bump bumpalo::Bump,
) {
    let kind = hir.pool.stmt(stmt_id).kind.clone();
    match kind {
        HirStmtKind::VarDecl { value, .. }
        | HirStmtKind::Expr(value)
        | HirStmtKind::Free(value) => {
            rewrite_expr_calls(hir, value, specialized, tc, bump);
        }
        HirStmtKind::Set { places, value, .. } => {
            rewrite_expr_calls(hir, value, specialized, tc, bump);
            let index_exprs: Vec<_> = hir
                .pool
                .places_list(places)
                .iter()
                .flat_map(|p| p.suffixes.iter())
                .filter_map(|s| match s {
                    HirPlaceSuffix::Index { expr, .. } => Some(*expr),
                    _ => None,
                })
                .collect();
            for e in index_exprs {
                rewrite_expr_calls(hir, e, specialized, tc, bump);
            }
        }
        HirStmtKind::Return { values } => {
            let es: Vec<_> = hir.pool.expr_list(values).to_vec();
            for e in es {
                rewrite_expr_calls(hir, e, specialized, tc, bump);
            }
        }
        HirStmtKind::If {
            condition,
            then_block,
            else_block,
        } => {
            rewrite_condition_calls(hir, &condition, specialized, tc, bump);
            rewrite_block_calls(hir, then_block, specialized, tc, bump);
            if let Some(eb) = else_block {
                rewrite_block_calls(hir, eb, specialized, tc, bump);
            }
        }
        HirStmtKind::While { condition, body } => {
            rewrite_condition_calls(hir, &condition, specialized, tc, bump);
            rewrite_block_calls(hir, body, specialized, tc, bump);
        }
        HirStmtKind::For { clause, body } => {
            match clause {
                HirForClause::In { iterable, .. } => {
                    rewrite_expr_calls(hir, iterable, specialized, tc, bump);
                }
                HirForClause::CStyle { condition, .. } => {
                    if let Some(c) = condition {
                        rewrite_expr_calls(hir, c, specialized, tc, bump);
                    }
                }
            }
            rewrite_block_calls(hir, body, specialized, tc, bump);
        }
        HirStmtKind::Match { value, arms } => {
            rewrite_expr_calls(hir, value, specialized, tc, bump);
            let arms_snap: Vec<_> = hir.pool.match_arms_list(arms).to_vec();
            for arm in arms_snap {
                if let Some(g) = arm.guard {
                    rewrite_expr_calls(hir, g, specialized, tc, bump);
                }
                match arm.body {
                    HirMatchArmBody::Expr(e) => rewrite_expr_calls(hir, e, specialized, tc, bump),
                    HirMatchArmBody::Block(b) => rewrite_block_calls(hir, b, specialized, tc, bump),
                }
            }
        }
        HirStmtKind::Defer(b) | HirStmtKind::ErrDefer(b) | HirStmtKind::Unsafe(b) => {
            rewrite_block_calls(hir, b, specialized, tc, bump);
        }
        HirStmtKind::Break | HirStmtKind::Continue | HirStmtKind::Error => {}
    }
}

pub(super) fn rewrite_condition_calls<'bump>(
    hir: &mut HirProgram,
    cond: &HirCondition,
    specialized: &FxHashMap<InstantiationKey<'bump>, SymbolId>,
    tc: &TypeCheckResult,
    bump: &'bump bumpalo::Bump,
) {
    match cond {
        HirCondition::Expr(e) => rewrite_expr_calls(hir, *e, specialized, tc, bump),
        HirCondition::Is { expr, .. } => rewrite_expr_calls(hir, *expr, specialized, tc, bump),
        HirCondition::And(conditions) => {
            for condition in conditions {
                rewrite_condition_calls(hir, condition, specialized, tc, bump);
            }
        }
    }
}

pub(super) fn rewrite_expr_calls<'bump>(
    hir: &mut HirProgram,
    expr_id: HirExprId,
    specialized: &FxHashMap<InstantiationKey<'bump>, SymbolId>,
    tc: &TypeCheckResult,
    bump: &'bump bumpalo::Bump,
) {
    // First recurse into children, then rewrite this node if it is Call(Generic(...)).
    let kind = hir.pool.expr(expr_id).kind.clone();
    match &kind {
        HirExprKind::Generic { callee, args } => {
            rewrite_expr_calls(hir, *callee, specialized, tc, bump);
            if let Some(symbol) = super::super::collect::generic_callee_symbol(*callee, hir, tc) {
                let type_args = bump.alloc_slice_copy(args);
                let key = InstantiationKey { symbol, type_args };
                if let Some(&spec_sym) = specialized.get(&key) {
                    let call_ty = hir.pool.expr(expr_id).ty;
                    let path_ty = tc.type_info.decl_type_id(spec_sym).unwrap_or(call_ty);
                    let span = hir.pool.expr(expr_id).span;
                    *hir.pool.expr_mut(expr_id) = HirExpr {
                        kind: HirExprKind::Path { symbol: spec_sym },
                        ty: path_ty,
                        span,
                    };
                }
            }
        }
        HirExprKind::Field { base: callee, .. }
        | HirExprKind::SafeField { base: callee, .. }
        | HirExprKind::Alloc { expr: callee }
        | HirExprKind::Try { expr: callee }
        | HirExprKind::Cast { expr: callee, .. }
        | HirExprKind::Unary { expr: callee, .. }
        | HirExprKind::ToStr { value: callee }
        | HirExprKind::ResultCtor { value: callee, .. } => {
            rewrite_expr_calls(hir, *callee, specialized, tc, bump);
        }
        HirExprKind::Index { base, index }
        | HirExprKind::SafeIndex { base, index }
        | HirExprKind::Binary {
            left: base,
            right: index,
            ..
        }
        | HirExprKind::NullCoalesce {
            left: base,
            right: index,
        } => {
            rewrite_expr_calls(hir, *base, specialized, tc, bump);
            rewrite_expr_calls(hir, *index, specialized, tc, bump);
        }
        HirExprKind::Call {
            callee,
            args,
            trailing_block,
        } => {
            rewrite_expr_calls(hir, *callee, specialized, tc, bump);
            let args_snap: Vec<_> = hir.pool.expr_list(*args).to_vec();
            for a in args_snap {
                rewrite_expr_calls(hir, a, specialized, tc, bump);
            }
            if let Some(b) = trailing_block {
                rewrite_block_calls(hir, *b, specialized, tc, bump);
            }
            // Rewrite Call(Generic(...), …) or Call(Field/Path template, …) → Path(spec).
            try_rewrite_generic_call(hir, expr_id, *callee, *args, specialized, tc, bump);
        }
        HirExprKind::StructLiteral { fields, .. } => {
            let vals: Vec<_> = hir
                .pool
                .field_inits_list(*fields)
                .iter()
                .map(|f| f.value)
                .collect();
            for e in vals {
                rewrite_expr_calls(hir, e, specialized, tc, bump);
            }
        }
        HirExprKind::Array { items } => {
            let es: Vec<_> = hir.pool.expr_list(*items).to_vec();
            for e in es {
                rewrite_expr_calls(hir, e, specialized, tc, bump);
            }
        }
        HirExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            rewrite_condition_calls(hir, condition, specialized, tc, bump);
            rewrite_block_calls(hir, *then_block, specialized, tc, bump);
            rewrite_block_calls(hir, *else_block, specialized, tc, bump);
        }
        HirExprKind::Match { value, arms } => {
            rewrite_expr_calls(hir, *value, specialized, tc, bump);
            let arms_snap: Vec<_> = hir.pool.match_arms_list(*arms).to_vec();
            for arm in arms_snap {
                if let Some(g) = arm.guard {
                    rewrite_expr_calls(hir, g, specialized, tc, bump);
                }
                match arm.body {
                    HirMatchArmBody::Expr(e) => rewrite_expr_calls(hir, e, specialized, tc, bump),
                    HirMatchArmBody::Block(b) => rewrite_block_calls(hir, b, specialized, tc, bump),
                }
            }
        }
        HirExprKind::Catch { expr, handler } => {
            rewrite_expr_calls(hir, *expr, specialized, tc, bump);
            match handler {
                HirCatchHandler::Expr(e) => rewrite_expr_calls(hir, *e, specialized, tc, bump),
                HirCatchHandler::Block { block, .. } => {
                    rewrite_block_calls(hir, *block, specialized, tc, bump)
                }
            }
        }
        HirExprKind::Lambda { body, .. } => match body {
            HirLambdaBody::Expr(e) => rewrite_expr_calls(hir, *e, specialized, tc, bump),
            HirLambdaBody::Block(b) => rewrite_block_calls(hir, *b, specialized, tc, bump),
        },
        HirExprKind::AsyncBlock { block } | HirExprKind::UnsafeBlock { block } => {
            rewrite_block_calls(hir, *block, specialized, tc, bump);
        }
        HirExprKind::StringInterp { parts } => {
            for p in parts {
                if let HirStringPart::Expr(e) = p {
                    rewrite_expr_calls(hir, *e, specialized, tc, bump);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn try_rewrite_generic_call<'bump>(
    hir: &mut HirProgram,
    call_expr_id: HirExprId,
    callee_id: HirExprId,
    args: arandu_middle::hir::IndexRange,
    specialized: &FxHashMap<InstantiationKey<'bump>, SymbolId>,
    tc: &TypeCheckResult,
    bump: &'bump bumpalo::Bump,
) {
    let key = match hir.pool.expr(callee_id).kind.clone() {
        HirExprKind::Generic {
            callee: inner_callee,
            args: type_args_vec,
        } => {
            let symbol = match &hir.pool.expr(inner_callee).kind {
                HirExprKind::Path { symbol } => *symbol,
                HirExprKind::TypePath { member_symbol, .. } => *member_symbol,
                HirExprKind::Field { base, field } | HirExprKind::SafeField { base, field } => {
                    let base_ty = tc.type_info.type_interner.resolve(hir.pool.expr(*base).ty);
                    let mut actual = match base_ty {
                        ArType::Nullable(inner) => tc.type_info.type_interner.resolve(inner),
                        other => other,
                    };
                    // Peel & / &mut / ptr (shared self receivers).
                    for _ in 0..4 {
                        actual = match actual {
                            ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                                tc.type_info.type_interner.resolve(inner)
                            }
                            other => other,
                        };
                        if matches!(
                            actual,
                            ArType::Named(_, _) | ArType::Result(_, _) | ArType::Option(_)
                        ) {
                            break;
                        }
                    }
                    let type_id = match actual {
                        ArType::Named(id, _) => Some(id),
                        ArType::Ptr(inner) => match tc.type_info.type_interner.resolve(inner) {
                            ArType::Named(id, _) => Some(id),
                            _ => None,
                        },
                        ArType::Result(_, _) => {
                            tc.symbols.lookup_type(tc.symbols.global_scope(), "Result")
                        }
                        ArType::Option(_) => {
                            tc.symbols.lookup_type(tc.symbols.global_scope(), "Option")
                        }
                        _ => None,
                    };
                    let Some(type_id) = type_id else {
                        return;
                    };
                    let Some(sym) = tc.symbols.lookup_associated_member(type_id, field.as_str())
                    else {
                        return;
                    };
                    sym
                }
                _ => return,
            };
            let type_args = bump.alloc_slice_copy(&type_args_vec);
            InstantiationKey { symbol, type_args }
        }
        // Receiver-driven method mono or free-func inferred mono (no Generic node).
        _ => {
            let call_ty = hir.pool.expr(call_expr_id).ty;
            let Some((symbol, type_args_vec)) = super::super::collect::instantiation_key_for_call(
                hir,
                tc,
                callee_id,
                args,
                call_ty,
                hir.pool.expr(call_expr_id).span,
            ) else {
                return;
            };
            let type_args = bump.alloc_slice_copy(&type_args_vec);
            InstantiationKey { symbol, type_args }
        }
    };
    let Some(&spec_sym) = specialized.get(&key) else {
        return;
    };
    // Overwrite callee → Path(specialized). Method calls already include receiver in args.
    let call_ty = hir.pool.expr(call_expr_id).ty;
    let path_ty = tc.type_info.decl_type_id(spec_sym).unwrap_or(call_ty);
    let span = hir.pool.expr(callee_id).span;
    *hir.pool.expr_mut(callee_id) = HirExpr {
        kind: HirExprKind::Path { symbol: spec_sym },
        ty: path_ty,
        span,
    };
}
