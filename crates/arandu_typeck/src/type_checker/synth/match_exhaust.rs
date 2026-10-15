use rustc_hash::FxHashSet;

use arandu_lexer::Span;
use arandu_parser::{
    MatchArm, Pattern,
    ast_pool::{AstPool, PatternId},
};

use super::super::TypeChecker;
use super::super::types::{ArType, TypeId};

fn pattern_covers_all(pool: &AstPool, pat: PatternId) -> bool {
    matches!(
        pool.pattern(pat),
        Pattern::Wildcard { .. } | Pattern::Bind { .. }
    )
}

/// Collect the canonical variant `SymbolId`s for `enum_id`.
///
/// Uses `SymbolId` for set membership to avoid heap-allocating variant name
/// strings on the hot exhaustiveness check path. String names are only
/// materialised in the error message (the cold path).
///
/// Each enum variant is stored under **two** different `SymbolId`s in
/// `enum_variants` (a span-derived one and an associated-member one).
/// We only want **one** representative per variant — we choose the
/// associated-member SymbolId because that is the one `lookup_associated_member`
/// returns, keeping `all_variants` and `covered` in the same coordinate system.
fn enum_variant_symbol_ids(
    checker: &TypeChecker<'_>,
    enum_id: crate::SymbolId,
) -> FxHashSet<crate::SymbolId> {
    let mut ids = FxHashSet::default();
    for (variant_id, (parent_enum, _)) in &checker.type_info.enum_variants {
        if *parent_enum != enum_id {
            continue;
        }
        let Some(sym) = checker.symbols.try_get(*variant_id) else {
            continue;
        };
        if sym.kind == arandu_middle::SymbolKind::AssociatedFunc {
            ids.insert(*variant_id);
        }
    }
    ids
}

/// Resolve a match-arm pattern to the `SymbolId` of the enum variant it covers.
///
/// Returns `None` for wildcards, binds, and any non-variant pattern (those are
/// handled separately via `pattern_covers_all`).
fn collect_pattern_variant_symbols(
    checker: &TypeChecker<'_>,
    enum_id: crate::SymbolId,
    pat: PatternId,
    covered: &mut FxHashSet<crate::SymbolId>,
) {
    match checker.pool.pattern(pat) {
        // `Variant` or `EnumName.Variant`
        Pattern::Enum { variant, .. } => {
            let short = variant
                .rsplit_once('.')
                .map_or(variant.as_str(), |(_, s)| s);
            if let Some(sym) = checker.symbols.lookup_associated_member(enum_id, short) {
                covered.insert(sym);
            }
        }
        // `EnumName.Variant(...)` style
        Pattern::TypeTuple { name, .. } => {
            let short = name.rsplit_once('.').map_or(name.as_str(), |(_, s)| s);
            if let Some(sym) = checker.symbols.lookup_associated_member(enum_id, short) {
                covered.insert(sym);
            }
        }
        // `EnumName.Variant { ... }` or `Variant { ... }`
        Pattern::Struct { type_name, .. } => {
            if let Some(short) = type_name.path.last()
                && let Some(sym) = checker
                    .symbols
                    .lookup_associated_member(enum_id, short.as_str())
            {
                covered.insert(sym);
            }
        }
        Pattern::Or { alts, .. } => {
            for &alt in checker.pool.pattern_list(*alts) {
                collect_pattern_variant_symbols(checker, enum_id, alt, covered);
            }
        }
        _ => {}
    }
}

pub fn check_match_exhaustiveness(
    checker: &mut TypeChecker<'_>,
    value_ty: TypeId,
    arms: &[MatchArm],
    match_span: Span,
) {
    let resolved_ty = checker.type_info.resolve_type_id(value_ty);
    // Error-typed match values must not trigger exhaustiveness diagnostics;
    // the guard has to run *before* the `Named` binding below, otherwise it
    // is unreachable (`Error` is not `Named`).
    if resolved_ty.is_error() {
        return;
    }
    let ArType::Named(enum_id, _) = resolved_ty else {
        return;
    };

    if checker
        .type_info
        .interfaces
        .get(&enum_id)
        .is_some_and(|interface| interface.sealed)
    {
        check_sealed_interface_exhaustiveness(checker, enum_id, arms, match_span);
        return;
    }

    // Collect all variant SymbolIds — O(V) where V = #variants.
    let all_variants = enum_variant_symbol_ids(checker, enum_id);
    if all_variants.is_empty() {
        return;
    }

    // Any wildcard / bind arm covers everything — short-circuit.
    if arms
        .iter()
        .any(|arm| pattern_covers_all(checker.pool, arm.pattern))
    {
        return;
    }

    // Build the covered set using SymbolId comparisons (integer equality,
    // no heap allocations on the hot path).
    let mut covered: FxHashSet<crate::SymbolId> = FxHashSet::default();
    for arm in arms.iter().filter(|arm| arm.guard.is_none()) {
        collect_pattern_variant_symbols(checker, enum_id, arm.pattern, &mut covered);
    }

    // Compute missing variants. String names are only materialised here,
    // which is the cold (error) path.
    let enum_name = checker
        .symbols
        .try_get(enum_id)
        .map(|s| s.name.as_str())
        .unwrap_or("enum");
    let prefix = format!("{}.", enum_name);
    let mut missing: Vec<String> = all_variants
        .difference(&covered)
        .map(|&sym| {
            if let Some(s) = checker.symbols.try_get(sym) {
                let full = &s.name;
                full.strip_prefix(&prefix).unwrap_or(full).to_string()
            } else {
                "variant".to_string()
            }
        })
        .collect();

    if missing.is_empty() {
        return;
    }
    missing.sort();

    checker.diagnostics.push(crate::Diagnostic::error(
        crate::DiagCode::T024NonExhaustiveMatch,
        format!(
            "non-exhaustive match: missing variant(s): {}",
            missing.join(", ")
        ),
        match_span,
    ));
}

fn check_sealed_interface_exhaustiveness(
    checker: &mut TypeChecker<'_>,
    interface: crate::SymbolId,
    arms: &[MatchArm],
    match_span: Span,
) {
    let implementations: FxHashSet<_> = checker
        .symbols
        .interface_implementations
        .iter()
        .filter_map(|&(concrete, implemented)| (implemented == interface).then_some(concrete))
        .collect();
    if implementations.is_empty() {
        return;
    }
    if arms
        .iter()
        .any(|arm| arm.guard.is_none() && pattern_covers_all(checker.pool, arm.pattern))
    {
        return;
    }

    let global = checker.symbols.global_scope();
    let mut covered = FxHashSet::default();
    for arm in arms.iter().filter(|arm| arm.guard.is_none()) {
        let type_name = match checker.pool.pattern(arm.pattern) {
            Pattern::Struct { type_name, .. } => type_name.path.last(),
            Pattern::TypeTuple { name, .. } => Some(name),
            _ => None,
        };
        if let Some(concrete) = type_name.and_then(|name| checker.symbols.lookup_type(global, name))
            && implementations.contains(&concrete)
        {
            covered.insert(concrete);
        }
    }

    let mut missing: Vec<_> = implementations
        .difference(&covered)
        .filter_map(|symbol| checker.symbols.try_get(*symbol))
        .map(|symbol| symbol.name.to_string())
        .collect();
    if missing.is_empty() {
        return;
    }
    missing.sort_unstable();
    checker.diagnostics.push(crate::Diagnostic::error(
        crate::DiagCode::T024NonExhaustiveMatch,
        format!(
            "non-exhaustive match on sealed interface: missing implementer(s): {}",
            missing.join(", ")
        ),
        match_span,
    ));
}
