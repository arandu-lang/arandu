//! Implicit type parameters of callback parameters (RFC 0032, L1).
//!
//! A `(A) -> R` parameter of a free function is a borrowed, non-escaping
//! callback. Its argument is a closure struct, so the parameter becomes an
//! implicit type parameter `F` bounded by the function type and the callee is
//! monomorphized per closure; `f(args)` then calls `F.call` directly.

use arandu_middle::{Symbol, SymbolId, SymbolKind, Visibility};
use arandu_parser::Param;

use crate::type_checker::TypeChecker;
use crate::type_checker::types::{ArType, TypeId};

/// Replaces each callback parameter type with its implicit type parameter,
/// registers the bound, and returns the new parameters in source order.
pub(super) fn implicit_callback_params(
    checker: &mut TypeChecker<'_>,
    function: SymbolId,
    params: &[Param],
    param_types: &mut [TypeId],
) -> Vec<SymbolId> {
    let mut implicit = Vec::new();
    for (index, (param, ty)) in params.iter().zip(param_types.iter_mut()).enumerate() {
        if param.is_receiver || !matches!(checker.resolve(*ty), ArType::Func(..)) {
            continue;
        }
        let symbol = callback_param_symbol(function, index);
        let name = checker
            .resolve(*ty)
            .display(&checker.symbols, &checker.type_info.type_interner);
        let scope = checker.symbols.global_scope();
        std::sync::Arc::make_mut(&mut checker.symbols).register_synthetic(Symbol {
            id: symbol,
            name: name.into(),
            kind: SymbolKind::TypeParam,
            span: param.span,
            scope,
            visibility: Visibility::Public,
            lang_item: None,
        });
        checker.type_info.callback_bounds.insert(symbol, *ty);
        let named = ArType::named(symbol, &[], &checker.type_info.type_interner);
        *ty = checker.intern(named);
        implicit.push(symbol);
    }
    implicit
}

/// Derived from the function and the parameter position only, so the
/// signature pass and every body check name the same parameter.
fn callback_param_symbol(function: SymbolId, index: usize) -> SymbolId {
    let index = u32::try_from(index).unwrap_or(u32::MAX);
    let mixed = function.local_id.0.wrapping_mul(0x9E37_79B1)
        ^ index.wrapping_add(1).wrapping_mul(0x85EB_CA6B)
        ^ 0x00C0_FFEE;
    SymbolId::new(
        function.file_id,
        SymbolId::SYNTHETIC_LOCAL_BIT | (mixed & 0x7FFF_FFFF),
    )
}
