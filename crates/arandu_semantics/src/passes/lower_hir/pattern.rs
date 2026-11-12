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
            if let Some(&(enum_symbol, _)) = type_check.type_info.enum_variants.get(&struct_symbol)
                && let Some(variant_fields) = type_check
                    .type_info
                    .struct_fields
                    .get(&struct_symbol)
                    .cloned()
            {
                let field_list = pool.field_pattern_list(*fields);
                let mut hir_payload = Vec::with_capacity(variant_fields.len());
                for vf in variant_fields.iter() {
                    if let Some(&f_id) = field_list
                        .iter()
                        .find(|&&f_id| pool.field_pattern(f_id).name == vf.name)
                    {
                        let f = pool.field_pattern(f_id);
                        let pat_id = match f.pattern {
                            Some(p) => lower_pattern_to_id(type_check, pool, hir_pool, p)?,
                            None => {
                                let symbol = require_def_symbol(&type_check.resolved, f.span)?;
                                hir_pool.alloc_pattern(HirPattern::Bind {
                                    span: f.span,
                                    name: f.name.clone(),
                                    symbol,
                                })
                            }
                        };
                        hir_payload.push(pat_id);
                    } else {
                        hir_payload
                            .push(hir_pool.alloc_pattern(HirPattern::Wildcard { span: *span }));
                    }
                }
                let payload_range = hir_pool.alloc_pattern_list(&hir_payload);
                let variant_name = type_name.path.last().cloned().unwrap_or_else(|| {
                    type_check
                        .symbols
                        .get(struct_symbol)
                        .name
                        .rsplit('.')
                        .next()
                        .unwrap_or("")
                        .into()
                });
                return Ok(HirPattern::Enum {
                    span: *span,
                    type_symbol: enum_symbol,
                    variant: variant_name,
                    variant_symbol: Some(struct_symbol),
                    payload: payload_range,
                });
            }
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
        let mut pattern_id = lower_pattern_to_id(type_check, pool, hir_pool, arm.pattern)?;
        // Alternatives that bind names are selected one at a time, so they
        // are kept at the top of the arm's pattern.
        if alternatives_bind(hir_pool, pattern_id, false) {
            pattern_id = hoist_alternatives(hir_pool, pattern_id);
        }
        hir_arms.push(HirMatchArm {
            span: arm.span,
            pattern: pattern_id,
            guard,
            body,
        });
    }
    Ok(hir_pool.alloc_match_arm_list(&hir_arms))
}

/// Most alternatives a pattern is expanded into; beyond it the pattern is
/// left as written.
const MAX_GUARDED_ALTERNATIVES: usize = 64;

/// Whether `pattern` binds a name inside a `p | q` (`inside` once within
/// one).
fn alternatives_bind(
    hir_pool: &crate::hir::HirPool,
    pattern: crate::hir::HirPatternId,
    inside: bool,
) -> bool {
    let any = |children: &[crate::hir::HirPatternId], inside: bool| {
        children
            .iter()
            .any(|&child| alternatives_bind(hir_pool, child, inside))
    };
    match hir_pool.pattern(pattern) {
        HirPattern::Bind { .. } => inside,
        HirPattern::Or { alts, .. } => any(hir_pool.pattern_list(*alts), true),
        HirPattern::Enum { payload, .. } | HirPattern::TypeTuple { payload, .. } => {
            any(hir_pool.pattern_list(*payload), inside)
        }
        HirPattern::Tuple { items, .. } => any(hir_pool.pattern_list(*items), inside),
        HirPattern::Struct { fields, .. } => {
            hir_pool.field_pattern_list(*fields).iter().any(|&field| {
                match hir_pool.field_pattern(field).pattern {
                    Some(nested) => alternatives_bind(hir_pool, nested, inside),
                    // A field written alone binds its own name.
                    None => inside,
                }
            })
        }
        HirPattern::Wildcard { .. } | HirPattern::Literal { .. } | HirPattern::Range { .. } => {
            false
        }
    }
}

/// `pattern` with every nested `p | q` distributed to the top:
/// `Some(A(x) | B(x))` becomes `Some(A(x)) | Some(B(x))`.
fn hoist_alternatives(
    hir_pool: &mut crate::hir::HirPool,
    pattern: crate::hir::HirPatternId,
) -> crate::hir::HirPatternId {
    let span = hir_pool.pattern(pattern).span();
    match alternatives(hir_pool, pattern) {
        Some(alts) if alts.len() > 1 => {
            let alts = hir_pool.alloc_pattern_list(&alts);
            hir_pool.alloc_pattern(HirPattern::Or { span, alts })
        }
        _ => pattern,
    }
}

/// The or-free patterns `pattern` stands for, in source order, or `None`
/// past [`MAX_GUARDED_ALTERNATIVES`].
fn alternatives(
    hir_pool: &mut crate::hir::HirPool,
    pattern: crate::hir::HirPatternId,
) -> Option<Vec<crate::hir::HirPatternId>> {
    // Every combination of the alternatives of `children`.
    fn product(
        hir_pool: &mut crate::hir::HirPool,
        children: &[crate::hir::HirPatternId],
    ) -> Option<Vec<Vec<crate::hir::HirPatternId>>> {
        let mut combinations = vec![Vec::with_capacity(children.len())];
        for &child in children {
            let choices = alternatives(hir_pool, child)?;
            if combinations.len() * choices.len() > MAX_GUARDED_ALTERNATIVES {
                return None;
            }
            combinations = combinations
                .iter()
                .flat_map(|prefix| {
                    choices.iter().map(move |&choice| {
                        let mut next = prefix.clone();
                        next.push(choice);
                        next
                    })
                })
                .collect();
        }
        Some(combinations)
    }

    let node = hir_pool.pattern(pattern).clone();
    let rebuilt = match node {
        HirPattern::Or { alts, .. } => {
            let mut all = Vec::new();
            for alt in hir_pool.pattern_list(alts).to_vec() {
                all.extend(alternatives(hir_pool, alt)?);
                if all.len() > MAX_GUARDED_ALTERNATIVES {
                    return None;
                }
            }
            return Some(all);
        }
        HirPattern::Enum {
            span,
            type_symbol,
            variant,
            variant_symbol,
            payload,
        } => {
            let children = hir_pool.pattern_list(payload).to_vec();
            let combinations = product(hir_pool, &children)?;
            if combinations.len() == 1 {
                return Some(vec![pattern]);
            }
            combinations
                .into_iter()
                .map(|items| HirPattern::Enum {
                    span,
                    type_symbol,
                    variant: variant.clone(),
                    variant_symbol,
                    payload: hir_pool.alloc_pattern_list(&items),
                })
                .collect::<Vec<_>>()
        }
        HirPattern::TypeTuple {
            span,
            name,
            payload,
        } => {
            let children = hir_pool.pattern_list(payload).to_vec();
            let combinations = product(hir_pool, &children)?;
            if combinations.len() == 1 {
                return Some(vec![pattern]);
            }
            combinations
                .into_iter()
                .map(|items| HirPattern::TypeTuple {
                    span,
                    name: name.clone(),
                    payload: hir_pool.alloc_pattern_list(&items),
                })
                .collect()
        }
        HirPattern::Tuple { span, items } => {
            let children = hir_pool.pattern_list(items).to_vec();
            let combinations = product(hir_pool, &children)?;
            if combinations.len() == 1 {
                return Some(vec![pattern]);
            }
            combinations
                .into_iter()
                .map(|items| HirPattern::Tuple {
                    span,
                    items: hir_pool.alloc_pattern_list(&items),
                })
                .collect()
        }
        HirPattern::Struct {
            span,
            struct_symbol,
            fields,
        } => {
            let field_ids = hir_pool.field_pattern_list(fields).to_vec();
            let fields: Vec<crate::hir::HirFieldPattern> = field_ids
                .iter()
                .map(|&id| hir_pool.field_pattern(id).clone())
                .collect();
            // Fields written without a pattern bind their own name and have
            // no alternatives; only the others are combined.
            let nested: Vec<crate::hir::HirPatternId> =
                fields.iter().filter_map(|field| field.pattern).collect();
            let combinations = product(hir_pool, &nested)?;
            if combinations.len() == 1 {
                return Some(vec![pattern]);
            }
            let mut rebuilt = Vec::with_capacity(combinations.len());
            for items in combinations {
                let mut items = items.into_iter();
                let ids: Vec<_> = fields
                    .iter()
                    .map(|field| {
                        let mut field = field.clone();
                        if field.pattern.is_some() {
                            field.pattern = items.next();
                        }
                        hir_pool.alloc_field_pattern(field)
                    })
                    .collect();
                rebuilt.push(HirPattern::Struct {
                    span,
                    struct_symbol,
                    fields: hir_pool.alloc_field_pattern_list(&ids),
                });
            }
            rebuilt
        }
        HirPattern::Wildcard { .. }
        | HirPattern::Bind { .. }
        | HirPattern::Literal { .. }
        | HirPattern::Range { .. } => return Some(vec![pattern]),
    };
    Some(
        rebuilt
            .into_iter()
            .map(|pattern| hir_pool.alloc_pattern(pattern))
            .collect(),
    )
}
