use super::super::graph::InstantiationKey;
use super::concrete::{is_identity_instantiation, type_args_fully_concrete};
use arandu_middle::hir::{
    HirBlockId, HirCatchHandler, HirCondition, HirExprId, HirExprKind, HirLambdaBody,
    HirMatchArmBody, HirProgram, HirStmtKind,
};
use arandu_middle::symbol_table::SymbolId;
use arandu_middle::types::TypeId;
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

/// Walk a specialized body and report concrete instantiation keys for nested
/// free-func / method calls (Generic nodes or inferred mono).
pub(super) fn discover_nested_keys<'bump>(
    hir: &HirProgram,
    block_id: HirBlockId,
    tc: &TypeCheckResult,
    bump: &'bump bumpalo::Bump,
    template_funcs: &FxHashMap<SymbolId, usize>,
    mut enqueue: impl FnMut(InstantiationKey<'bump>),
) {
    fn visit_block<'bump>(
        hir: &HirProgram,
        block_id: HirBlockId,
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        let blk = hir.pool.block(block_id);
        for &sid in hir.pool.stmt_list(blk.statements) {
            visit_stmt(hir, sid, tc, bump, template_funcs, enqueue);
        }
    }

    fn visit_stmt<'bump>(
        hir: &HirProgram,
        stmt_id: arandu_middle::hir::HirStmtId,
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        let kind = &hir.pool.stmt(stmt_id).kind;
        match kind {
            HirStmtKind::VarDecl { value, .. }
            | HirStmtKind::Expr(value)
            | HirStmtKind::Free(value)
            | HirStmtKind::Set { value, .. } => {
                visit_expr(hir, *value, tc, bump, template_funcs, enqueue);
            }
            HirStmtKind::Return { values } => {
                for &e in hir.pool.expr_list(*values) {
                    visit_expr(hir, e, tc, bump, template_funcs, enqueue);
                }
            }
            HirStmtKind::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_condition(hir, condition, tc, bump, template_funcs, enqueue);
                visit_block(hir, *then_block, tc, bump, template_funcs, enqueue);
                if let Some(b) = else_block {
                    visit_block(hir, *b, tc, bump, template_funcs, enqueue);
                }
            }
            HirStmtKind::While { condition, body } => {
                visit_condition(hir, condition, tc, bump, template_funcs, enqueue);
                visit_block(hir, *body, tc, bump, template_funcs, enqueue);
            }
            HirStmtKind::For {
                body,
                comptime_bodies,
                ..
            } => {
                if let Some(bodies) = comptime_bodies {
                    for body in bodies {
                        visit_block(hir, *body, tc, bump, template_funcs, enqueue);
                    }
                } else {
                    visit_block(hir, *body, tc, bump, template_funcs, enqueue);
                }
            }
            HirStmtKind::Match { value, arms } => {
                visit_expr(hir, *value, tc, bump, template_funcs, enqueue);
                for arm in hir.pool.match_arms_list(*arms) {
                    if let Some(g) = arm.guard {
                        visit_expr(hir, g, tc, bump, template_funcs, enqueue);
                    }
                    match &arm.body {
                        HirMatchArmBody::Expr(e) => {
                            visit_expr(hir, *e, tc, bump, template_funcs, enqueue)
                        }
                        HirMatchArmBody::Block(b) => {
                            visit_block(hir, *b, tc, bump, template_funcs, enqueue)
                        }
                    }
                }
            }
            HirStmtKind::Defer(b)
            | HirStmtKind::ErrDefer(b)
            | HirStmtKind::Unsafe(b)
            | HirStmtKind::Scope(b) => {
                visit_block(hir, *b, tc, bump, template_funcs, enqueue);
            }
            HirStmtKind::Break | HirStmtKind::Continue | HirStmtKind::Error => {}
        }
    }

    fn visit_condition<'bump>(
        hir: &HirProgram,
        cond: &HirCondition,
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        match cond {
            HirCondition::Expr(e) | HirCondition::Is { expr: e, .. } => {
                visit_expr(hir, *e, tc, bump, template_funcs, enqueue);
            }
            HirCondition::And(conditions) => {
                for condition in conditions {
                    visit_condition(hir, condition, tc, bump, template_funcs, enqueue);
                }
            }
        }
    }

    fn maybe_enqueue<'bump>(
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        symbol: SymbolId,
        type_args: &[TypeId],
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        if !template_funcs.contains_key(&symbol) {
            return;
        }
        if is_identity_instantiation(tc, symbol, type_args)
            || !type_args_fully_concrete(tc, type_args)
        {
            return;
        }
        let type_args = bump.alloc_slice_copy(type_args);
        enqueue(InstantiationKey { symbol, type_args });
    }

    fn visit_expr<'bump>(
        hir: &HirProgram,
        expr_id: HirExprId,
        tc: &TypeCheckResult,
        bump: &'bump bumpalo::Bump,
        template_funcs: &FxHashMap<SymbolId, usize>,
        enqueue: &mut impl FnMut(InstantiationKey<'bump>),
    ) {
        let expr = hir.pool.expr(expr_id);
        match &expr.kind {
            HirExprKind::Call {
                callee,
                args,
                trailing_block,
            } => {
                visit_expr(hir, *callee, tc, bump, template_funcs, enqueue);
                for &a in hir.pool.expr_list(*args) {
                    visit_expr(hir, a, tc, bump, template_funcs, enqueue);
                }
                if let Some(b) = trailing_block {
                    visit_block(hir, *b, tc, bump, template_funcs, enqueue);
                }
                // Explicit Generic type args on the callee.
                if let HirExprKind::Generic {
                    callee: inner,
                    args: type_args,
                } = &hir.pool.expr(*callee).kind
                {
                    let symbol = match &hir.pool.expr(*inner).kind {
                        HirExprKind::Path { symbol } => Some(*symbol),
                        HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                        _ => None,
                    };
                    if let Some(symbol) = symbol {
                        maybe_enqueue(tc, bump, template_funcs, symbol, type_args, enqueue);
                    }
                } else if let Some((symbol, type_args_vec)) =
                    super::super::collect::instantiation_key_for_call(
                        hir, tc, *callee, *args, expr.ty, expr.span,
                    )
                {
                    maybe_enqueue(tc, bump, template_funcs, symbol, &type_args_vec, enqueue);
                }
            }
            HirExprKind::Generic { callee, args } => {
                visit_expr(hir, *callee, tc, bump, template_funcs, enqueue);
                if let Some(symbol) = match &hir.pool.expr(*callee).kind {
                    HirExprKind::Path { symbol } => Some(*symbol),
                    HirExprKind::TypePath { member_symbol, .. } => Some(*member_symbol),
                    _ => None,
                } {
                    maybe_enqueue(tc, bump, template_funcs, symbol, args, enqueue);
                }
            }
            HirExprKind::Field { base, .. }
            | HirExprKind::SafeField { base, .. }
            | HirExprKind::Alloc { expr: base }
            | HirExprKind::Try { expr: base }
            | HirExprKind::Cast { expr: base, .. }
            | HirExprKind::Unary { expr: base, .. }
            | HirExprKind::ToStr { value: base }
            | HirExprKind::ResultCtor { value: base, .. } => {
                visit_expr(hir, *base, tc, bump, template_funcs, enqueue);
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
                visit_expr(hir, *base, tc, bump, template_funcs, enqueue);
                visit_expr(hir, *index, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_condition(hir, condition, tc, bump, template_funcs, enqueue);
                visit_block(hir, *then_block, tc, bump, template_funcs, enqueue);
                visit_block(hir, *else_block, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::Match { value, arms } => {
                visit_expr(hir, *value, tc, bump, template_funcs, enqueue);
                for arm in hir.pool.match_arms_list(*arms) {
                    if let Some(g) = arm.guard {
                        visit_expr(hir, g, tc, bump, template_funcs, enqueue);
                    }
                    match &arm.body {
                        HirMatchArmBody::Expr(e) => {
                            visit_expr(hir, *e, tc, bump, template_funcs, enqueue)
                        }
                        HirMatchArmBody::Block(b) => {
                            visit_block(hir, *b, tc, bump, template_funcs, enqueue)
                        }
                    }
                }
            }
            HirExprKind::Catch { expr, handler } => {
                visit_expr(hir, *expr, tc, bump, template_funcs, enqueue);
                match handler {
                    HirCatchHandler::Expr(e) => {
                        visit_expr(hir, *e, tc, bump, template_funcs, enqueue)
                    }
                    HirCatchHandler::Block { block, .. } => {
                        visit_block(hir, *block, tc, bump, template_funcs, enqueue)
                    }
                }
            }
            HirExprKind::Lambda { body, .. } => match body {
                HirLambdaBody::Expr(e) => visit_expr(hir, *e, tc, bump, template_funcs, enqueue),
                HirLambdaBody::Block(b) => visit_block(hir, *b, tc, bump, template_funcs, enqueue),
            },
            HirExprKind::AsyncBlock { block }
            | HirExprKind::UnsafeBlock { block }
            | HirExprKind::ValueBlock { block } => {
                visit_block(hir, *block, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::StructLiteral { fields, .. } => {
                for f in hir.pool.field_inits_list(*fields) {
                    visit_expr(hir, f.value, tc, bump, template_funcs, enqueue);
                }
            }
            HirExprKind::ArrayRepeat { value } => {
                visit_expr(hir, *value, tc, bump, template_funcs, enqueue);
            }
            HirExprKind::Array { items } => {
                for &e in hir.pool.expr_list(*items) {
                    visit_expr(hir, e, tc, bump, template_funcs, enqueue);
                }
            }
            HirExprKind::StringInterp { parts } => {
                for p in parts {
                    if let arandu_middle::hir::HirStringPart::Expr(e) = p {
                        visit_expr(hir, *e, tc, bump, template_funcs, enqueue);
                    }
                }
            }
            _ => {}
        }
    }

    visit_block(hir, block_id, tc, bump, template_funcs, &mut enqueue);
}
