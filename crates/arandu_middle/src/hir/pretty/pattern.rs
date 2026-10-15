//! Semantic pattern formatting, independent of incidental pool/symbol IDs.

use super::types::HirPrettyCtx;
use crate::hir::HirPattern;

pub(super) fn format_pattern_ref(pat: &HirPattern, ctx: &HirPrettyCtx<'_>) -> String {
    let symbol_name = |symbol| {
        ctx.symbols
            .try_get(symbol)
            .map_or("<unresolved>", |symbol| symbol.name.as_str())
    };
    let pattern_list = |range, separator: &str| {
        ctx.pool
            .pattern_list(range)
            .iter()
            .map(|&id| format_pattern_ref(ctx.pool.pattern(id), ctx))
            .collect::<Vec<_>>()
            .join(separator)
    };
    let text = match pat {
        HirPattern::Wildcard { .. } => "_".to_string(),
        HirPattern::Bind { name, .. } => name.to_string(),
        HirPattern::Literal { expr, .. } => expr.pretty_print_inline(ctx),
        HirPattern::Enum {
            type_symbol,
            variant,
            payload,
            ..
        } => {
            let name = format!("{}.{}", symbol_name(*type_symbol), variant);
            if ctx.pool.pattern_list(*payload).is_empty() {
                name
            } else {
                format!("{name}({})", pattern_list(*payload, ", "))
            }
        }
        HirPattern::TypeTuple { name, payload, .. } => {
            format!("{name}({})", pattern_list(*payload, ", "))
        }
        HirPattern::Struct {
            struct_symbol,
            fields,
            ..
        } => {
            let fields = ctx
                .pool
                .field_pattern_list(*fields)
                .iter()
                .map(|&id| {
                    let field = ctx.pool.field_pattern(id);
                    match field.pattern {
                        Some(pattern) => format!(
                            "{}: {}",
                            field.name,
                            format_pattern_ref(ctx.pool.pattern(pattern), ctx)
                        ),
                        None => field.name.to_string(),
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {fields} }}", symbol_name(*struct_symbol))
        }
        HirPattern::Tuple { items, .. } => {
            let suffix = if ctx.pool.pattern_list(*items).len() == 1 {
                ","
            } else {
                ""
            };
            format!("({}{suffix})", pattern_list(*items, ", "))
        }
        HirPattern::Range {
            start,
            inclusive,
            end,
            ..
        } => format!(
            "{}{}{}",
            start.pretty_print_inline(ctx),
            if *inclusive { "..=" } else { ".." },
            end.pretty_print_inline(ctx)
        ),
        HirPattern::Or { alts, .. } => pattern_list(*alts, " | "),
    };
    if ctx.show_spans {
        format!("{text} @ {:?}", pat.span())
    } else {
        text
    }
}

impl HirPattern {
    pub(super) fn pretty_print_to(&self, out: &mut String, _indent: usize, ctx: &HirPrettyCtx<'_>) {
        out.push_str(&format_pattern_ref(self, ctx));
    }
}
