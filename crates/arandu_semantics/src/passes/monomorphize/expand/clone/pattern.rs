use arandu_diagnostics::Diagnostic;
use arandu_middle::hir::{HirCondition, HirPattern, HirProgram};
use arandu_middle::symbol_table::{SymbolId, SymbolKind};
use arandu_middle::types::{GenericSubst, substitute_type_id};
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

pub(in crate::passes::monomorphize::expand) fn clone_pattern_id(
    hir: &mut HirProgram,
    pat_id: arandu_middle::hir::HirPatternId,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<arandu_middle::hir::HirPatternId, Diagnostic> {
    let pat = hir.pool.pattern(pat_id).clone();
    let new_pat = clone_pattern(hir, &pat, subst, symbol_map, tc, name_prefix)?;
    Ok(hir.pool.alloc_pattern(new_pat))
}

pub(in crate::passes::monomorphize::expand) fn clone_pattern(
    hir: &mut HirProgram,
    pat: &HirPattern,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirPattern, Diagnostic> {
    Ok(match pat {
        HirPattern::Wildcard { span } => HirPattern::Wildcard { span: *span },
        HirPattern::Bind { span, name, symbol } => {
            let new_sym = super::fresh_symbol(
                tc,
                &format!("{name_prefix}$pat_{name}"),
                SymbolKind::Local,
                *span,
            );
            symbol_map.insert(*symbol, new_sym);
            // Type of binding comes from typeck at use; leave unbound in decl_types if unknown.
            if let Some(tid) = tc.type_info.decl_type_id(*symbol) {
                let new_ty = substitute_type_id(tid, subst, &tc.type_info.type_interner);
                tc.type_info_mut().record_decl_type(new_sym, new_ty);
            }
            HirPattern::Bind {
                span: *span,
                name: name.clone(),
                symbol: new_sym,
            }
        }
        HirPattern::Literal { span, expr } => HirPattern::Literal {
            span: *span,
            expr: super::clone_expr(hir, *expr, subst, symbol_map, tc, name_prefix)?,
        },
        HirPattern::Enum {
            span,
            type_symbol,
            variant,
            variant_symbol,
            payload,
        } => {
            let old_p: Vec<_> = hir.pool.pattern_list(*payload).to_vec();
            let mut new_p = Vec::with_capacity(old_p.len());
            for &p in &old_p {
                new_p.push(clone_pattern_id(
                    hir,
                    p,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            HirPattern::Enum {
                span: *span,
                type_symbol: *type_symbol,
                variant: variant.clone(),
                variant_symbol: *variant_symbol,
                payload: hir.pool.alloc_pattern_list(&new_p),
            }
        }
        HirPattern::TypeTuple {
            span,
            name,
            payload,
        } => {
            let old_p: Vec<_> = hir.pool.pattern_list(*payload).to_vec();
            let mut new_p = Vec::with_capacity(old_p.len());
            for &p in &old_p {
                new_p.push(clone_pattern_id(
                    hir,
                    p,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            HirPattern::TypeTuple {
                span: *span,
                name: name.clone(),
                payload: hir.pool.alloc_pattern_list(&new_p),
            }
        }
        HirPattern::Struct {
            span,
            struct_symbol,
            fields,
        } => {
            let old_f: Vec<_> = hir.pool.field_pattern_list(*fields).to_vec();
            let mut new_f = Vec::with_capacity(old_f.len());
            for &fid in &old_f {
                let fp = hir.pool.field_pattern(fid).clone();
                let pattern = fp
                    .pattern
                    .map(|p| clone_pattern_id(hir, p, subst, symbol_map, tc, name_prefix))
                    .transpose()?;
                new_f.push(
                    hir.pool
                        .alloc_field_pattern(arandu_middle::hir::HirFieldPattern {
                            span: fp.span,
                            name: fp.name.clone(),
                            pattern,
                        }),
                );
            }
            HirPattern::Struct {
                span: *span,
                struct_symbol: *struct_symbol,
                fields: hir.pool.alloc_field_pattern_list(&new_f),
            }
        }
        HirPattern::Tuple { span, items } => {
            let old_p: Vec<_> = hir.pool.pattern_list(*items).to_vec();
            let mut new_p = Vec::with_capacity(old_p.len());
            for &p in &old_p {
                new_p.push(clone_pattern_id(
                    hir,
                    p,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            HirPattern::Tuple {
                span: *span,
                items: hir.pool.alloc_pattern_list(&new_p),
            }
        }
        HirPattern::Range {
            span,
            start,
            inclusive,
            end,
        } => HirPattern::Range {
            span: *span,
            start: super::clone_expr(hir, *start, subst, symbol_map, tc, name_prefix)?,
            inclusive: *inclusive,
            end: super::clone_expr(hir, *end, subst, symbol_map, tc, name_prefix)?,
        },
        HirPattern::Or { span, alts } => {
            let old_p: Vec<_> = hir.pool.pattern_list(*alts).to_vec();
            let mut new_p = Vec::with_capacity(old_p.len());
            for &p in &old_p {
                new_p.push(clone_pattern_id(
                    hir,
                    p,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            HirPattern::Or {
                span: *span,
                alts: hir.pool.alloc_pattern_list(&new_p),
            }
        }
    })
}

pub(in crate::passes::monomorphize::expand) fn clone_condition(
    hir: &mut HirProgram,
    cond: &HirCondition,
    subst: &GenericSubst,
    symbol_map: &mut FxHashMap<SymbolId, SymbolId>,
    tc: &mut TypeCheckResult,
    name_prefix: &str,
) -> Result<HirCondition, Diagnostic> {
    Ok(match cond {
        HirCondition::Expr(e) => HirCondition::Expr(super::clone_expr(
            hir,
            *e,
            subst,
            symbol_map,
            tc,
            name_prefix,
        )?),
        HirCondition::Is { expr, pattern } => HirCondition::Is {
            expr: super::clone_expr(hir, *expr, subst, symbol_map, tc, name_prefix)?,
            pattern: clone_pattern_id(hir, *pattern, subst, symbol_map, tc, name_prefix)?,
        },
        HirCondition::And(conditions) => {
            let mut cloned = Vec::with_capacity(conditions.len());
            for condition in conditions {
                cloned.push(clone_condition(
                    hir,
                    condition,
                    subst,
                    symbol_map,
                    tc,
                    name_prefix,
                )?);
            }
            HirCondition::And(cloned.into_boxed_slice())
        }
    })
}
