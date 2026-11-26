//! Text values: `str` views and the owned `String`.
//!
//! As in Rust, text built at run time is owned: an interpolation with
//! expressions (`"n=${n}"`) and `a + b` on text are `String`, which frees its
//! buffer once. A literal stays a `str`. Unlike Rust, a `String` is used
//! where a `str` is expected without `&` or `.asStr()`, but only where the
//! view cannot outlive it: a call argument (the value lives until the call
//! ends) and a `let` initializer (it lives as long as the binding). Returning
//! or storing a view of a `String` is a type error that asks for `String`.

use arandu_middle::symbol_table::LangItem;
use arandu_parser::ast_pool::ExprId;

use crate::type_checker::TypeChecker;
use crate::type_checker::types::{ArType, Primitive, TypeId};

/// The `String` type, when the standard library provides it.
pub(crate) fn string_type(checker: &mut TypeChecker<'_>) -> Option<TypeId> {
    let symbol = checker.symbols.lang_item_symbol(LangItem::String)?;
    let ty = ArType::named(symbol, &[], &checker.type_info.type_interner);
    Some(checker.intern(ty))
}

/// `String`, or a reference to one.
pub(crate) fn is_string(checker: &TypeChecker<'_>, ty: &ArType) -> bool {
    let ty = match ty {
        ArType::Ref(inner) | ArType::RefMut(inner) => checker.resolve(*inner),
        other => other.clone(),
    };
    matches!(ty, ArType::Named(symbol, _)
        if checker.symbols.try_get(symbol).and_then(|s| s.lang_item) == Some(LangItem::String))
}

/// `String` itself, not a reference to one.
pub(crate) fn is_owned_string(checker: &TypeChecker<'_>, ty: &ArType) -> bool {
    !matches!(ty, ArType::Ref(_) | ArType::RefMut(_)) && is_string(checker, ty)
}

/// Text of either kind: `str`, `String`, or a reference to one.
pub(crate) fn is_text(checker: &TypeChecker<'_>, ty: &ArType) -> bool {
    ty.str_reference_depth(&checker.type_info.type_interner)
        .is_some()
        || is_string(checker, ty)
}

/// The view of a `String` where a `str` or `ref str` is expected, at a site
/// where the view cannot outlive the value.
pub(crate) fn adjust_text_view(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    actual: TypeId,
    expected: TypeId,
) -> Option<TypeId> {
    let view = match checker.resolve(expected) {
        ArType::Ref(inner) => checker.resolve(inner),
        other => other,
    };
    if !matches!(view, ArType::Primitive(Primitive::Str))
        || !is_string(checker, &checker.resolve(actual))
        || !checker.text_view_sites.contains(&expr)
    {
        return None;
    }
    let span = checker.pool.expr_span(expr);
    checker.type_info.adjustments.insert(
        span,
        crate::type_checker::info::Adjustment {
            kind: crate::type_checker::info::AdjustmentKind::TextView,
            from: actual,
            to: expected,
        },
    );
    Some(expected)
}

/// The slice view of a `Vec<T>` (or `ref Vec<T>` / `mut ref Vec<T>`) where `[]T`
/// is expected, at a site where the borrowed slice cannot outlive the vector.
pub(crate) fn adjust_vec_view(
    checker: &mut TypeChecker<'_>,
    expr: ExprId,
    actual: TypeId,
    expected: TypeId,
) -> Option<TypeId> {
    if !checker.text_view_sites.contains(&expr) {
        return None;
    }
    let ArType::Slice(slice_elem) = checker.resolve(expected) else {
        return None;
    };
    let unwrapped = match checker.resolve(actual) {
        ArType::Ref(inner) | ArType::RefMut(inner) => checker.resolve(inner),
        other => other,
    };
    let ArType::Named(symbol, args) = unwrapped else {
        return None;
    };
    let is_vec = checker.symbols.lang_item_symbol(LangItem::Vec) == Some(symbol)
        || checker.symbols.try_get(symbol).and_then(|s| s.lang_item) == Some(LangItem::Vec);
    if !is_vec {
        return None;
    }
    let type_args = checker.type_info.type_interner.type_args(args);
    let &[vec_elem] = type_args.as_slice() else {
        return None;
    };
    if vec_elem != slice_elem && !checker.unify_ids(slice_elem, vec_elem) {
        return None;
    }
    let span = checker.pool.expr_span(expr);
    checker.type_info.adjustments.insert(
        span,
        crate::type_checker::info::Adjustment {
            kind: crate::type_checker::info::AdjustmentKind::VecView,
            from: actual,
            to: expected,
        },
    );
    Some(expected)
}
