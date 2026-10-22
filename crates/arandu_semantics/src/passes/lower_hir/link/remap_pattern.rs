use super::PoolOffsets;
use arandu_middle::hir::HirPattern;
use arandu_middle::types::{TypeId, TypeInterner};
use arandu_typeck::TypeCheckResult;
use rustc_hash::FxHashMap;

pub(super) fn remap_pattern(
    pat: &HirPattern,
    offs: &PoolOffsets,
    _from: &TypeInterner,
    _dest_tc: &mut TypeCheckResult,
    _ty_cache: &mut FxHashMap<TypeId, TypeId>,
) -> HirPattern {
    match pat {
        HirPattern::Wildcard { span } => HirPattern::Wildcard { span: *span },
        HirPattern::Bind { span, name, symbol } => HirPattern::Bind {
            span: *span,
            name: name.clone(),
            symbol: *symbol,
        },
        HirPattern::Literal { span, expr } => HirPattern::Literal {
            span: *span,
            expr: offs.expr_id(*expr),
        },
        HirPattern::Enum {
            span,
            type_symbol,
            variant,
            variant_symbol,
            payload,
        } => HirPattern::Enum {
            span: *span,
            type_symbol: *type_symbol,
            variant: variant.clone(),
            variant_symbol: *variant_symbol,
            payload: offs.range(*payload, offs.pattern_ids),
        },
        HirPattern::TypeTuple {
            span,
            name,
            payload,
        } => HirPattern::TypeTuple {
            span: *span,
            name: name.clone(),
            payload: offs.range(*payload, offs.pattern_ids),
        },
        HirPattern::Struct {
            span,
            struct_symbol,
            fields,
        } => HirPattern::Struct {
            span: *span,
            struct_symbol: *struct_symbol,
            fields: offs.range(*fields, offs.field_pattern_ids),
        },
        HirPattern::Tuple { span, items } => HirPattern::Tuple {
            span: *span,
            items: offs.range(*items, offs.pattern_ids),
        },
        HirPattern::Range {
            span,
            start,
            inclusive,
            end,
        } => HirPattern::Range {
            span: *span,
            start: offs.expr_id(*start),
            inclusive: *inclusive,
            end: offs.expr_id(*end),
        },
        HirPattern::Or { span, alts } => HirPattern::Or {
            span: *span,
            alts: offs.range(*alts, offs.pattern_ids),
        },
    }
}
