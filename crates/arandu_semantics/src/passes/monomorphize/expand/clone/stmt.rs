use arandu_diagnostics::Diagnostic;
use arandu_lexer::Span;
use arandu_middle::hir::{
    HirBindingItem, HirBlock, HirBlockId, HirCatchHandler, HirForBinding, HirForClause,
    HirMatchArm, HirMatchArmBody, HirProgram, HirSimpleStmt, HirStmt, HirStmtKind,
};
use arandu_middle::symbol_table::{SymbolId, SymbolKind};
use arandu_middle::types::{GenericSubst, substitute_type_id};
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

pub(in crate::passes::monomorphize::expand) fn clone_block(
    hir: &mut HirProgram,
    block_id: HirBlockId,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirBlockId, Diagnostic> {
    let block = hir.pool.block(block_id).clone();
    let old_stmts: Vec<_> = hir.pool.stmt_list(block.statements).to_vec();
    let mut new_stmt_ids = Vec::with_capacity(old_stmts.len());
    for &sid in &old_stmts {
        let stmt = hir.pool.stmt(sid).clone();
        let new_kind = clone_stmt_kind(hir, &stmt.kind, subst, symbol_map, tc, name_prefix)?;
        new_stmt_ids.push(hir.pool.alloc_stmt(HirStmt {
            kind: new_kind,
            span: stmt.span,
        }));
    }
    let range = hir.pool.alloc_stmt_list(&new_stmt_ids);
    Ok(hir.pool.alloc_block(HirBlock {
        statements: range,
        span: block.span,
    }))
}

pub(in crate::passes::monomorphize::expand) fn clone_stmt_kind(
    hir: &mut HirProgram,
    kind: &HirStmtKind,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirStmtKind, Diagnostic> {
    Ok(match kind {
        HirStmtKind::VarDecl { bindings, value } => {
            let old_b: Vec<_> = hir.pool.bindings_list(*bindings).to_vec();
            let mut new_b = Vec::with_capacity(old_b.len());
            for b in &old_b {
                let new_ty = substitute_type_id(b.ty, subst, &tc.type_info.type_interner);
                let bname = tc.symbols.get(b.symbol).name.clone();
                let new_sym = super::fresh_symbol(
                    tc,
                    &format!("{name_prefix}${bname}"),
                    SymbolKind::Local,
                    b.span,
                );
                symbol_map.insert(b.symbol, new_sym);
                tc.type_info_mut().record_decl_type(new_sym, new_ty);
                new_b.push(HirBindingItem {
                    symbol: new_sym,
                    ty: new_ty,
                    span: b.span,
                });
            }
            let br = hir.pool.alloc_binding_list(&new_b);
            let value = super::clone_expr(hir, *value, subst, symbol_map, tc, name_prefix)?;
            HirStmtKind::VarDecl {
                bindings: br,
                value,
            }
        }
        HirStmtKind::Set { places, op, value } => {
            let old_p: Vec<_> = hir.pool.places_list(*places).to_vec();
            let mut new_p = Vec::with_capacity(old_p.len());
            for p in &old_p {
                new_p.push(super::clone_place(
                    hir,
                    p,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            let pr = hir.pool.alloc_place_list(&new_p);
            let value = super::clone_expr(hir, *value, subst, symbol_map, tc, name_prefix)?;
            HirStmtKind::Set {
                places: pr,
                op: *op,
                value,
            }
        }
        HirStmtKind::Return { values } => {
            let old_v: Vec<_> = hir.pool.expr_list(*values).to_vec();
            let mut new_v = Vec::with_capacity(old_v.len());
            for &e in &old_v {
                new_v.push(super::clone_expr(
                    hir,
                    e,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            HirStmtKind::Return {
                values: hir.pool.alloc_expr_list(&new_v),
            }
        }
        HirStmtKind::Break => HirStmtKind::Break,
        HirStmtKind::Continue => HirStmtKind::Continue,
        HirStmtKind::Free(e) => HirStmtKind::Free(super::clone_expr(
            hir,
            *e,
            subst,
            symbol_map,
            tc,
            name_prefix,
        )?),
        HirStmtKind::Expr(e) => HirStmtKind::Expr(super::clone_expr(
            hir,
            *e,
            subst,
            symbol_map,
            tc,
            name_prefix,
        )?),
        HirStmtKind::If {
            condition,
            then_block,
            else_block,
        } => HirStmtKind::If {
            condition: super::clone_condition(hir, condition, subst, symbol_map, tc, name_prefix)?,
            then_block: clone_block(hir, *then_block, subst, symbol_map, tc, name_prefix)?,
            else_block: else_block
                .map(|b| clone_block(hir, b, subst, symbol_map, tc, name_prefix))
                .transpose()?,
        },
        HirStmtKind::For {
            clause,
            body,
            comptime_bounds,
            comptime_bodies,
        } => HirStmtKind::For {
            comptime_bounds: *comptime_bounds,
            comptime_bodies: comptime_bodies
                .as_ref()
                .map(|bodies| {
                    bodies
                        .iter()
                        .map(|&body| clone_block(hir, body, subst, symbol_map, tc, name_prefix))
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
            clause: clone_for_clause(hir, clause, subst, symbol_map, tc, name_prefix)?,
            body: clone_block(hir, *body, subst, symbol_map, tc, name_prefix)?,
        },
        HirStmtKind::While { condition, body } => HirStmtKind::While {
            condition: super::clone_condition(hir, condition, subst, symbol_map, tc, name_prefix)?,
            body: clone_block(hir, *body, subst, symbol_map, tc, name_prefix)?,
        },
        HirStmtKind::Match { value, arms } => {
            let value = super::clone_expr(hir, *value, subst, symbol_map, tc, name_prefix)?;
            let old_arms: Vec<_> = hir.pool.match_arms_list(*arms).to_vec();
            let mut new_arms = Vec::with_capacity(old_arms.len());
            for arm in &old_arms {
                new_arms.push(clone_match_arm(
                    hir,
                    arm,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            HirStmtKind::Match {
                value,
                arms: hir.pool.alloc_match_arm_list(&new_arms),
            }
        }
        HirStmtKind::Defer(b) => {
            HirStmtKind::Defer(clone_block(hir, *b, subst, symbol_map, tc, name_prefix)?)
        }
        HirStmtKind::ErrDefer(b) => {
            HirStmtKind::ErrDefer(clone_block(hir, *b, subst, symbol_map, tc, name_prefix)?)
        }
        HirStmtKind::Unsafe(b) => {
            HirStmtKind::Unsafe(clone_block(hir, *b, subst, symbol_map, tc, name_prefix)?)
        }
        HirStmtKind::Error => HirStmtKind::Error,
        HirStmtKind::Scope(b) => {
            HirStmtKind::Scope(clone_block(hir, *b, subst, symbol_map, tc, name_prefix)?)
        }
    })
}

pub(in crate::passes::monomorphize::expand) fn clone_for_clause(
    hir: &mut HirProgram,
    clause: &HirForClause,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirForClause, Diagnostic> {
    Ok(match clause {
        HirForClause::In {
            span,
            bindings,
            iterable,
        } => {
            let old_b: Vec<_> = hir.pool.for_bindings_list(*bindings).to_vec();
            let mut new_b = Vec::with_capacity(old_b.len());
            for b in &old_b {
                let new_ty = substitute_type_id(b.ty, subst, &tc.type_info.type_interner);
                let bname = tc.symbols.get(b.symbol).name.clone();
                let new_sym = super::fresh_symbol(
                    tc,
                    &format!("{name_prefix}$for_{bname}"),
                    SymbolKind::Local,
                    b.span,
                );
                symbol_map.insert(b.symbol, new_sym);
                tc.type_info_mut().record_decl_type(new_sym, new_ty);
                new_b.push(HirForBinding {
                    symbol: new_sym,
                    ty: new_ty,
                    span: b.span,
                });
            }
            HirForClause::In {
                span: *span,
                bindings: hir.pool.alloc_for_binding_list(&new_b),
                iterable: super::clone_expr(hir, *iterable, subst, symbol_map, tc, name_prefix)?,
            }
        }
        HirForClause::CStyle {
            span,
            init,
            condition,
            step,
        } => HirForClause::CStyle {
            span: *span,
            init: init
                .as_ref()
                .map(|s| clone_simple_stmt(hir, s, subst, symbol_map, tc, name_prefix))
                .transpose()?,
            condition: condition
                .map(|e| super::clone_expr(hir, e, subst, symbol_map, tc, name_prefix))
                .transpose()?,
            step: step
                .as_ref()
                .map(|s| clone_simple_stmt(hir, s, subst, symbol_map, tc, name_prefix))
                .transpose()?,
        },
    })
}

pub(in crate::passes::monomorphize::expand) fn clone_simple_stmt(
    hir: &mut HirProgram,
    stmt: &HirSimpleStmt,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirSimpleStmt, Diagnostic> {
    Ok(match stmt {
        HirSimpleStmt::VarDecl { bindings, value } => {
            let kind = clone_stmt_kind(
                hir,
                &HirStmtKind::VarDecl {
                    bindings: *bindings,
                    value: *value,
                },
                subst,
                symbol_map,
                tc,
                name_prefix,
            )?;
            match kind {
                HirStmtKind::VarDecl { bindings, value } => {
                    HirSimpleStmt::VarDecl { bindings, value }
                }
                _ => unreachable!(),
            }
        }
        HirSimpleStmt::Set { places, op, value } => {
            let kind = clone_stmt_kind(
                hir,
                &HirStmtKind::Set {
                    places: *places,
                    op: *op,
                    value: *value,
                },
                subst,
                symbol_map,
                tc,
                name_prefix,
            )?;
            match kind {
                HirStmtKind::Set { places, op, value } => HirSimpleStmt::Set { places, op, value },
                _ => unreachable!(),
            }
        }
        HirSimpleStmt::Expr(e) => HirSimpleStmt::Expr(super::clone_expr(
            hir,
            *e,
            subst,
            symbol_map,
            tc,
            name_prefix,
        )?),
    })
}

pub(in crate::passes::monomorphize::expand) fn clone_match_arm(
    hir: &mut HirProgram,
    arm: &HirMatchArm,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirMatchArm, Diagnostic> {
    let pattern = super::clone_pattern_id(hir, arm.pattern, subst, symbol_map, tc, name_prefix)?;
    let guard = arm
        .guard
        .map(|g| super::clone_expr(hir, g, subst, symbol_map, tc, name_prefix))
        .transpose()?;
    let body = match &arm.body {
        HirMatchArmBody::Expr(e) => HirMatchArmBody::Expr(super::clone_expr(
            hir,
            *e,
            subst,
            symbol_map,
            tc,
            name_prefix,
        )?),
        HirMatchArmBody::Block(b) => {
            HirMatchArmBody::Block(clone_block(hir, *b, subst, symbol_map, tc, name_prefix)?)
        }
    };
    Ok(HirMatchArm {
        span: arm.span,
        pattern,
        guard,
        body,
    })
}

pub(in crate::passes::monomorphize::expand) fn clone_catch_handler(
    hir: &mut HirProgram,
    handler: &HirCatchHandler,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirCatchHandler, Diagnostic> {
    Ok(match handler {
        HirCatchHandler::Expr(e) => HirCatchHandler::Expr(super::clone_expr(
            hir,
            *e,
            subst,
            symbol_map,
            tc,
            name_prefix,
        )?),
        HirCatchHandler::Block {
            error_symbol,
            error_name,
            block,
        } => {
            let error_symbol = if let Some(sym) = error_symbol {
                let new_sym = super::fresh_symbol(
                    tc,
                    &format!("{name_prefix}$catch"),
                    SymbolKind::Local,
                    Span::new(0, 0, 0),
                );
                symbol_map.insert(*sym, new_sym);
                if let Some(tid) = tc.type_info.decl_type_id(*sym) {
                    let new_ty = substitute_type_id(tid, subst, &tc.type_info.type_interner);
                    tc.type_info_mut().record_decl_type(new_sym, new_ty);
                }
                Some(new_sym)
            } else {
                None
            };
            HirCatchHandler::Block {
                error_symbol,
                error_name: error_name.clone(),
                block: clone_block(hir, *block, subst, symbol_map, tc, name_prefix)?,
            }
        }
    })
}
