use crate::diagnostics::Diagnostic;
use crate::hir::{HirFieldPattern, HirMatchArm, HirMatchArmBody, HirPattern};
use crate::passes::lowering::{require_def_symbol, require_type_symbol};
use crate::{NodeKey, TypeCheckResult};
use arandu_parser::MatchArmBody;
use arandu_parser::Pattern;
use arandu_parser::ast_pool::{AstPool, MatchArmId, PatternId};

pub(crate) fn lower_pattern_to_id(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    pattern: PatternId,
) -> Result<crate::hir::HirPatternId, Diagnostic> {
    let hir = lower_pattern(type_check, pool, hir_pool, pattern)?;
    Ok(hir_pool.alloc_pattern(hir))
}

pub(crate) fn lower_pattern(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    pattern: PatternId,
) -> Result<HirPattern, Diagnostic> {
    let pat = pool.pattern(pattern);
    match pat {
        Pattern::Wildcard { span } => Ok(HirPattern::Wildcard { span: *span }),
        Pattern::Bind { span, name, .. } => {
            let symbol = require_def_symbol(&type_check.resolved, *span)?;
            Ok(HirPattern::Bind {
                span: *span,
                name: name.clone(),
                symbol,
            })
        }
        Pattern::Literal { span, expr } => {
            let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
            Ok(HirPattern::Literal {
                span: *span,
                expr: eid,
            })
        }
        Pattern::Enum {
            span,
            type_name,
            variant,
            payload,
        } => {
            let type_symbol = require_type_symbol(&type_check.resolved, type_name.span)?;
            let mut hir_payload = Vec::new();
            for &p in pool.pattern_list(*payload) {
                hir_payload.push(lower_pattern_to_id(type_check, pool, hir_pool, p)?);
            }
            let payload_range = hir_pool.alloc_pattern_list(&hir_payload);
            if type_check.symbols.is_option_type(type_symbol)
                || type_check.symbols.is_result_type(type_symbol)
            {
                Ok(HirPattern::TypeTuple {
                    span: *span,
                    name: variant.clone(),
                    payload: payload_range,
                })
            } else {
                let variant_symbol = type_check
                    .resolved
                    .definitions
                    .get(&NodeKey::from(*span))
                    .copied();
                Ok(HirPattern::Enum {
                    span: *span,
                    type_symbol,
                    variant: variant.clone(),
                    variant_symbol,
                    payload: payload_range,
                })
            }
        }
        Pattern::TypeTuple {
            span,
            name,
            payload,
        } => {
            let mut hir_payload = Vec::new();
            for &p in pool.pattern_list(*payload) {
                hir_payload.push(lower_pattern_to_id(type_check, pool, hir_pool, p)?);
            }
            let payload_range = hir_pool.alloc_pattern_list(&hir_payload);
            Ok(HirPattern::TypeTuple {
                span: *span,
                name: name.clone(),
                payload: payload_range,
            })
        }
        Pattern::Struct {
            span,
            type_name,
            fields,
        } => {
            let struct_symbol = require_type_symbol(&type_check.resolved, type_name.span)?;
            let mut hir_fields = Vec::new();
            for &f_id in pool.field_pattern_list(*fields) {
                let f = pool.field_pattern(f_id);
                let pat_id = match f.pattern {
                    Some(p) => Some(lower_pattern_to_id(type_check, pool, hir_pool, p)?),
                    None => None,
                };
                let field_pat = HirFieldPattern {
                    span: f.span,
                    name: f.name.clone(),
                    pattern: pat_id,
                };
                let field_pat_id = hir_pool.alloc_field_pattern(field_pat);
                hir_fields.push(field_pat_id);
            }
            let fields_range = hir_pool.alloc_field_pattern_list(&hir_fields);
            Ok(HirPattern::Struct {
                span: *span,
                struct_symbol,
                fields: fields_range,
            })
        }
        Pattern::Tuple { span, items } => {
            let mut hir_items = Vec::new();
            for &p in pool.pattern_list(*items) {
                hir_items.push(lower_pattern_to_id(type_check, pool, hir_pool, p)?);
            }
            let items_range = hir_pool.alloc_pattern_list(&hir_items);
            Ok(HirPattern::Tuple {
                span: *span,
                items: items_range,
            })
        }
        Pattern::Range {
            span,
            start,
            inclusive,
            end,
        } => {
            let sid = super::expr::lower_expr(type_check, pool, hir_pool, *start)?;
            let eid = super::expr::lower_expr(type_check, pool, hir_pool, *end)?;
            Ok(HirPattern::Range {
                span: *span,
                start: sid,
                inclusive: *inclusive,
                end: eid,
            })
        }
        Pattern::Or { span, alts } => {
            let mut hir_alts = Vec::new();
            for &p in pool.pattern_list(*alts) {
                hir_alts.push(lower_pattern_to_id(type_check, pool, hir_pool, p)?);
            }
            let alts_range = hir_pool.alloc_pattern_list(&hir_alts);
            Ok(HirPattern::Or {
                span: *span,
                alts: alts_range,
            })
        }
    }
}

pub(crate) fn lower_match_arms(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    arms: &[MatchArmId],
) -> Result<crate::hir::IndexRange, Diagnostic> {
    let mut hir_arms = Vec::new();
    for arm_id in arms {
        let arm = pool.match_arm(*arm_id);
        let guard = arm
            .guard
            .as_ref()
            .map(|g| super::expr::lower_expr(type_check, pool, hir_pool, *g))
            .transpose()?;
        let body = match &arm.body {
            MatchArmBody::Expr { expr, .. } => {
                let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
                HirMatchArmBody::Expr(eid)
            }
            MatchArmBody::Block { block, .. } => {
                HirMatchArmBody::Block(super::stmt::lower_block(type_check, pool, hir_pool, block)?)
            }
        };
        let pattern_id = lower_pattern_to_id(type_check, pool, hir_pool, arm.pattern)?;
        hir_arms.push(HirMatchArm {
            span: arm.span,
            pattern: pattern_id,
            guard,
            body,
        });
    }
    Ok(hir_pool.alloc_match_arm_list(&hir_arms))
}
