use super::ar_type::ArType;
use super::primitive::Primitive;
use super::result_option::{is_err_type, result_ok_err};
use super::type_interner::TypeInterner;

/// Unify return value type against declared `Result` return.
#[must_use]
#[tracing::instrument(
    level = "trace",
    target = "arandu_middle::types::unify",
    skip(interner)
)]
pub fn unify_return_type(expected: &ArType, actual: &ArType, interner: &TypeInterner) -> bool {
    if unify(expected, actual, interner) {
        return true;
    }
    if let Some((ok_exp, err_exp)) = result_ok_err(expected, interner) {
        if let Some((ok_act, err_act)) = result_ok_err(actual, interner) {
            return unify(&ok_exp, &ok_act, interner) && unify(&err_exp, &err_act, interner);
        }
        // `return nil` on `Result<void, Err>`
        if matches!(ok_exp, ArType::Void)
            && matches!(actual, ArType::Nullable(inner) if matches!(&interner.resolve(*inner), ArType::Error))
        {
            return true;
        }
        if is_err_type(actual, interner) && is_err_type(&err_exp, interner) {
            return true;
        }
        if matches!(actual, ArType::Err) && matches!(err_exp, ArType::Err) {
            return true;
        }
    }
    false
}

// ── Unification ─────────────────────────────────────────────────────

/// Structural type equality check using the interner for deep resolution.
///
/// - `Error` unifies with anything (poison propagation)
/// - `Any` unifies with anything (FFI/variadic)
/// - `IntLiteral` unifies with any numeric type
/// - `FloatLiteral` unifies with any float type
/// - Named types compare `SymbolId` + generic args
/// - Func types compare param count, params, and return
#[must_use]
#[tracing::instrument(
    level = "trace",
    target = "arandu_middle::types::unify",
    skip(interner)
)]
pub fn unify(a: &ArType, b: &ArType, interner: &TypeInterner) -> bool {
    // Poison and Any always unify
    if a.is_error() || b.is_error() {
        return true;
    }
    if matches!(a, ArType::Primitive(Primitive::Any))
        || matches!(b, ArType::Primitive(Primitive::Any))
    {
        return true;
    }

    // Literal absorption
    if a.is_literal() && a.literal_absorbs(b) {
        return true;
    }
    if b.is_literal() && b.literal_absorbs(a) {
        return true;
    }
    // Two int literals or two float literals unify
    if matches!((a, b), (ArType::IntLiteral, ArType::IntLiteral)) {
        return true;
    }
    if matches!((a, b), (ArType::FloatLiteral, ArType::FloatLiteral)) {
        return true;
    }
    // IntLiteral and FloatLiteral: the int absorbs float context
    if matches!(
        (a, b),
        (ArType::IntLiteral, ArType::FloatLiteral) | (ArType::FloatLiteral, ArType::IntLiteral)
    ) {
        return true;
    }

    match (a, b) {
        (ArType::Primitive(pa), ArType::Primitive(pb)) => pa == pb,
        (ArType::Named(id_a, args_a), ArType::Named(id_b, args_b)) => {
            id_a == id_b
                && args_a.len == args_b.len
                && interner
                    .type_args(*args_a)
                    .iter()
                    .zip(interner.type_args(*args_b).iter())
                    .all(|(&x, &y)| {
                        if x == y {
                            return true;
                        }
                        unify(&interner.resolve(x), &interner.resolve(y), interner)
                    })
        }
        (ArType::Func(params_a, ret_a), ArType::Func(params_b, ret_b)) => {
            params_a.len == params_b.len
                && interner
                    .type_args(*params_a)
                    .iter()
                    .zip(interner.type_args(*params_b).iter())
                    .all(|(&x, &y)| {
                        if x == y {
                            return true;
                        }
                        unify(&interner.resolve(x), &interner.resolve(y), interner)
                    })
                && (*ret_a == *ret_b
                    || unify(
                        &interner.resolve(*ret_a),
                        &interner.resolve(*ret_b),
                        interner,
                    ))
        }
        (ArType::Nullable(inner_a), ArType::Nullable(inner_b)) => {
            *inner_a == *inner_b
                || unify(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }

        (ArType::Slice(inner_a), ArType::Slice(inner_b)) => {
            *inner_a == *inner_b
                || unify(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Array(n_a, elem_a), ArType::Array(n_b, elem_b)) => {
            n_a == n_b
                && (*elem_a == *elem_b
                    || unify(
                        &interner.resolve(*elem_a),
                        &interner.resolve(*elem_b),
                        interner,
                    ))
        }
        (ArType::Ptr(inner_a), ArType::Ptr(inner_b)) => {
            *inner_a == *inner_b
                || unify(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        // Shared refs unify structurally.
        (ArType::Ref(inner_a), ArType::Ref(inner_b)) => {
            *inner_a == *inner_b
                || unify(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        // Exclusive refs unify with each other.
        (ArType::RefMut(inner_a), ArType::RefMut(inner_b)) => {
            *inner_a == *inner_b
                || unify(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }

        (ArType::GenRef, ArType::GenRef) => true,
        (ArType::Tuple(types_a), ArType::Tuple(types_b)) => {
            types_a.len == types_b.len
                && interner
                    .type_args(*types_a)
                    .iter()
                    .zip(interner.type_args(*types_b).iter())
                    .all(|(&x, &y)| {
                        if x == y {
                            return true;
                        }
                        unify(&interner.resolve(x), &interner.resolve(y), interner)
                    })
        }
        (ArType::Result(ok_a, err_a), ArType::Result(ok_b, err_b)) => {
            (*ok_a == *ok_b || unify(&interner.resolve(*ok_a), &interner.resolve(*ok_b), interner))
                && (*err_a == *err_b
                    || unify(
                        &interner.resolve(*err_a),
                        &interner.resolve(*err_b),
                        interner,
                    ))
        }
        (ArType::Option(inner_a), ArType::Option(inner_b)) => {
            *inner_a == *inner_b
                || unify(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Coroutine(inner_a), ArType::Coroutine(inner_b)) => {
            *inner_a == *inner_b
                || unify(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Poll(inner_a), ArType::Poll(inner_b)) => {
            *inner_a == *inner_b
                || unify(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Range(inner_a), ArType::Range(inner_b)) => {
            *inner_a == *inner_b
                || unify(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Err, ArType::Err) => true,
        (ArType::Void, ArType::Void) => true,
        _ => false,
    }
}

/// Given two types where at least one may be a literal, resolve to the
/// concrete type. This is used to determine the result type of binary
/// operations where literals are involved.
///
/// A literal paired with a concrete non-literal takes the concrete type.
/// Two literals of the same kind stay literal, and an `int`/`float` literal
/// pair stays a literal (float-leaning), keeping the result deferred so it
/// can still be constrained by the surrounding context (TYP.3.3).
#[must_use]
pub fn resolve_literal_pair(a: &ArType, b: &ArType) -> ArType {
    match (a, b) {
        (ArType::IntLiteral, other) | (other, ArType::IntLiteral) if !other.is_literal() => {
            other.clone()
        }
        (ArType::FloatLiteral, other) | (other, ArType::FloatLiteral) if !other.is_literal() => {
            other.clone()
        }
        (ArType::IntLiteral, ArType::IntLiteral) => ArType::IntLiteral,
        (ArType::FloatLiteral, ArType::FloatLiteral) => ArType::FloatLiteral,
        (ArType::IntLiteral, ArType::FloatLiteral) | (ArType::FloatLiteral, ArType::IntLiteral) => {
            ArType::FloatLiteral
        }
        _ => a.clone(),
    }
}

/// Check if type `actual` is assignable to `expected` (asymmetric, directional compatibility).
///
/// - Error/Poison propagates
/// - Any unifies with anything
/// - Literal types can be assigned to concrete numeric types
/// - T is assignable to T?
/// - &mut T is assignable to &T
/// - Structs, tuples, and other composite types check their fields/components recursively and covariantly
#[must_use]
#[tracing::instrument(
    level = "trace",
    target = "arandu_middle::types::unify",
    skip(interner)
)]
pub fn is_assignable(actual: &ArType, expected: &ArType, interner: &TypeInterner) -> bool {
    if actual.is_error() || expected.is_error() {
        return true;
    }
    if matches!(actual, ArType::Primitive(Primitive::Any))
        || matches!(expected, ArType::Primitive(Primitive::Any))
    {
        return true;
    }
    if actual == expected {
        return true;
    }

    // T -> T? coercion
    if let ArType::Nullable(inner_expected_id) = expected {
        let inner_expected = interner.resolve(*inner_expected_id);
        if let ArType::Nullable(inner_actual_id) = actual {
            let inner_actual = interner.resolve(*inner_actual_id);
            return is_assignable(&inner_actual, &inner_expected, interner);
        }
        if is_assignable(actual, &inner_expected, interner) {
            return true;
        }
    }

    // &mut T -> &T coercion
    if let ArType::Ref(inner_expected_id) = expected {
        let inner_expected = interner.resolve(*inner_expected_id);
        if let ArType::RefMut(inner_actual_id) = actual {
            let inner_actual = interner.resolve(*inner_actual_id);
            return is_assignable(&inner_actual, &inner_expected, interner);
        }
    }

    // Literal absorption
    if actual.is_literal() && actual.literal_absorbs(expected) {
        return true;
    }
    if expected.is_literal() && expected.literal_absorbs(actual) {
        return true;
    }
    if matches!(
        (actual, expected),
        (ArType::IntLiteral, ArType::IntLiteral)
            | (ArType::FloatLiteral, ArType::FloatLiteral)
            | (ArType::IntLiteral, ArType::FloatLiteral)
            | (ArType::FloatLiteral, ArType::IntLiteral)
    ) {
        return true;
    }

    match (actual, expected) {
        (ArType::Primitive(pa), ArType::Primitive(pb)) => pa == pb,
        (ArType::Named(id_a, args_a), ArType::Named(id_b, args_b)) => {
            id_a == id_b
                && args_a.len == args_b.len
                && interner
                    .type_args(*args_a)
                    .iter()
                    .zip(interner.type_args(*args_b).iter())
                    .all(|(&x, &y)| {
                        if x == y {
                            return true;
                        }
                        is_assignable(&interner.resolve(x), &interner.resolve(y), interner)
                    })
        }
        (ArType::Func(params_a, ret_a), ArType::Func(params_b, ret_b)) => {
            params_a.len == params_b.len
                && interner
                    .type_args(*params_a)
                    .iter()
                    .zip(interner.type_args(*params_b).iter())
                    .all(|(&x, &y)| unify(&interner.resolve(x), &interner.resolve(y), interner))
                && is_assignable(
                    &interner.resolve(*ret_a),
                    &interner.resolve(*ret_b),
                    interner,
                )
        }
        (ArType::Slice(inner_a), ArType::Slice(inner_b)) => {
            *inner_a == *inner_b
                || is_assignable(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Array(n_a, elem_a), ArType::Array(n_b, elem_b)) => {
            n_a == n_b
                && (*elem_a == *elem_b
                    || is_assignable(
                        &interner.resolve(*elem_a),
                        &interner.resolve(*elem_b),
                        interner,
                    ))
        }
        (ArType::Ptr(inner_a), ArType::Ptr(inner_b)) => {
            *inner_a == *inner_b
                || is_assignable(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Ref(inner_a), ArType::Ref(inner_b)) => {
            *inner_a == *inner_b
                || is_assignable(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::RefMut(inner_a), ArType::RefMut(inner_b)) => {
            *inner_a == *inner_b
                || is_assignable(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Tuple(types_a), ArType::Tuple(types_b)) => {
            types_a.len == types_b.len
                && interner
                    .type_args(*types_a)
                    .iter()
                    .zip(interner.type_args(*types_b).iter())
                    .all(|(&x, &y)| {
                        if x == y {
                            return true;
                        }
                        is_assignable(&interner.resolve(x), &interner.resolve(y), interner)
                    })
        }
        (ArType::Result(ok_a, err_a), ArType::Result(ok_b, err_b)) => {
            (*ok_a == *ok_b
                || is_assignable(&interner.resolve(*ok_a), &interner.resolve(*ok_b), interner))
                && (*err_a == *err_b
                    || is_assignable(
                        &interner.resolve(*err_a),
                        &interner.resolve(*err_b),
                        interner,
                    ))
        }
        (ArType::Option(inner_a), ArType::Option(inner_b)) => {
            *inner_a == *inner_b
                || is_assignable(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Coroutine(inner_a), ArType::Coroutine(inner_b)) => {
            *inner_a == *inner_b
                || is_assignable(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Poll(inner_a), ArType::Poll(inner_b)) => {
            *inner_a == *inner_b
                || is_assignable(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        (ArType::Range(inner_a), ArType::Range(inner_b)) => {
            *inner_a == *inner_b
                || is_assignable(
                    &interner.resolve(*inner_a),
                    &interner.resolve(*inner_b),
                    interner,
                )
        }
        _ => false,
    }
}

/// Asymmetric check for return value compatibility.
#[must_use]
#[tracing::instrument(
    level = "trace",
    target = "arandu_middle::types::unify",
    skip(interner)
)]
pub fn is_assignable_return_type(
    expected: &ArType,
    actual: &ArType,
    interner: &TypeInterner,
) -> bool {
    if is_assignable(actual, expected, interner) {
        return true;
    }
    if let Some((ok_exp, err_exp)) = result_ok_err(expected, interner) {
        if let Some((ok_act, err_act)) = result_ok_err(actual, interner) {
            return is_assignable(&ok_act, &ok_exp, interner)
                && is_assignable(&err_act, &err_exp, interner);
        }
        // `return nil` on `Result<void, Err>`
        if matches!(ok_exp, ArType::Void)
            && matches!(actual, ArType::Nullable(inner) if matches!(&interner.resolve(*inner), ArType::Error))
        {
            return true;
        }
        if is_err_type(actual, interner) && is_err_type(&err_exp, interner) {
            return true;
        }
        if matches!(actual, ArType::Err) && matches!(err_exp, ArType::Err) {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[path = "unify_tests.rs"]
mod tests;
