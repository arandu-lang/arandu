//! Inference variables of the type checker (TYP.NEXT S1).
//!
//! The design follows rustc's inference context, not a global constraint
//! solver: checking stays bidirectional and eager, and an [`ArType::Infer`]
//! stands for a type that is not known yet where a generic parameter has no
//! argument to bind it (`Vec.new()`). Unification binds it when it meets a
//! type; the interner then resolves the variable to that type everywhere.
//! A body's variables are written back into its recorded types before the
//! body's result leaves the type checker, so no later phase sees one.
//!
//! Compatibility checks treat an unbound variable as accepting any type of
//! its kind; only a check that succeeds as a whole binds, so a failed probe
//! leaves no partial bindings behind.

use super::ar_type::ArType;
use super::primitive::Primitive;
use super::type_interner::{InferKind, TypeId, TypeInterner};

/// Nesting at which binding and resolution stop; deeper types are left as
/// they are and reported unresolved instead of overflowing the stack.
const MAX_DEPTH: usize = 64;

/// Whether the unbound variable `var` may stand for `other`.
#[must_use]
pub fn infer_accepts(var: u32, other: &ArType, interner: &TypeInterner) -> bool {
    let kind = interner.infer_kind(var);
    match other {
        ArType::Error | ArType::Primitive(Primitive::Any) => true,
        ArType::Infer(other) => {
            let other = interner.infer_kind(*other);
            !matches!(
                (kind, other),
                (InferKind::Int, InferKind::Float) | (InferKind::Float, InferKind::Int)
            )
        }
        _ => match kind {
            InferKind::General => true,
            InferKind::Int => match other {
                ArType::IntLiteral => true,
                ArType::Primitive(primitive) => primitive.is_integer(),
                _ => false,
            },
            InferKind::Float => match other {
                ArType::IntLiteral | ArType::FloatLiteral => true,
                ArType::Primitive(primitive) => primitive.is_float(),
                _ => false,
            },
        },
    }
}

/// Binds the unbound variables of `a` and `b` to what they meet in the
/// other type, after a compatibility check over the pair succeeded.
pub fn bind_unified(a: &ArType, b: &ArType, interner: &TypeInterner) {
    if interner.has_infer_vars() {
        bind(a, b, interner, 0);
    }
}

fn bind(a: &ArType, b: &ArType, interner: &TypeInterner, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let pair = |x: TypeId, y: TypeId| {
        if x != y {
            bind(
                &interner.resolve(x),
                &interner.resolve(y),
                interner,
                depth + 1,
            );
        }
    };
    let pairs = |xs: Vec<TypeId>, ys: Vec<TypeId>| {
        if xs.len() == ys.len() {
            for (x, y) in xs.into_iter().zip(ys) {
                pair(x, y);
            }
        }
    };
    match (a, b) {
        (ArType::Infer(var), other) | (other, ArType::Infer(var)) => {
            bind_var(*var, other, interner);
        }
        (ArType::Named(x, xs), ArType::Named(y, ys)) if x == y => {
            pairs(interner.type_args(*xs), interner.type_args(*ys));
        }
        (ArType::Func(xs, x), ArType::Func(ys, y)) => {
            pairs(interner.type_args(*xs), interner.type_args(*ys));
            pair(*x, *y);
        }
        (ArType::Tuple(xs), ArType::Tuple(ys)) => {
            pairs(interner.type_args(*xs), interner.type_args(*ys));
        }
        (ArType::Result(x_ok, x_err), ArType::Result(y_ok, y_err)) => {
            pair(*x_ok, *y_ok);
            pair(*x_err, *y_err);
        }
        (ArType::Array(_, x), ArType::Array(_, y))
        | (ArType::ConstArray(_, x), ArType::ConstArray(_, y))
        | (
            ArType::Slice(x) | ArType::MutSlice(x),
            ArType::Slice(y) | ArType::MutSlice(y) | ArType::Array(_, y),
        )
        | (ArType::Array(_, x), ArType::Slice(y) | ArType::MutSlice(y))
        | (ArType::Ptr(x), ArType::Ptr(y))
        | (ArType::Ref(x) | ArType::RefMut(x), ArType::Ref(y) | ArType::RefMut(y))
        | (ArType::Option(x), ArType::Option(y))
        | (ArType::Nullable(x), ArType::Nullable(y))
        | (ArType::Coroutine(x), ArType::Coroutine(y))
        | (ArType::Poll(x), ArType::Poll(y))
        | (ArType::Range(x), ArType::Range(y)) => pair(*x, *y),
        // Coercions the checks accept: `T` into `T?`, and a value into the
        // reference that borrows it.
        (ArType::Nullable(x) | ArType::Ref(x) | ArType::RefMut(x), other)
        | (other, ArType::Nullable(x) | ArType::Ref(x) | ArType::RefMut(x)) => {
            bind(&interner.resolve(*x), other, interner, depth + 1);
        }
        _ => {}
    }
}

fn bind_var(var: u32, other: &ArType, interner: &TypeInterner) {
    if interner.infer_binding(var).is_some() {
        return;
    }
    match other {
        ArType::Infer(other_var) if *other_var == var => {}
        ArType::Infer(other_var) => {
            let kind = interner.infer_kind(var);
            if kind != InferKind::General {
                interner
                    .set_infer_kind(*other_var, narrower(kind, interner.infer_kind(*other_var)));
            }
            interner.bind_infer(var, interner.intern(ArType::Infer(*other_var)));
        }
        // A literal narrows the variable; the literal's own group fixes the
        // concrete width later.
        ArType::IntLiteral => {
            if interner.infer_kind(var) == InferKind::General {
                interner.set_infer_kind(var, InferKind::Int);
            }
        }
        ArType::FloatLiteral => interner.set_infer_kind(var, InferKind::Float),
        other => {
            let id = interner.intern(other.clone());
            if !occurs(var, id, interner, 0) {
                interner.bind_infer(var, id);
            }
        }
    }
}

fn narrower(a: InferKind, b: InferKind) -> InferKind {
    match (a, b) {
        (InferKind::General, other) | (other, InferKind::General) => other,
        (InferKind::Float, _) | (_, InferKind::Float) => InferKind::Float,
        _ => InferKind::Int,
    }
}

/// Whether the variable `var` occurs in `id` (a binding to it would make an
/// infinite type).
#[must_use]
pub fn occurs(var: u32, id: TypeId, interner: &TypeInterner, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let ty = interner.resolve(id);
    if let ArType::Infer(other) = ty {
        return other == var;
    }
    children(&ty, interner)
        .into_iter()
        .any(|child| occurs(var, child, interner, depth + 1))
}

/// Whether `id`, after following bindings, still mentions a variable.
#[must_use]
pub fn contains_infer(id: TypeId, interner: &TypeInterner) -> bool {
    contains(id, interner, 0)
}

fn contains(id: TypeId, interner: &TypeInterner, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let ty = interner.resolve(id);
    matches!(ty, ArType::Infer(_))
        || children(&ty, interner)
            .into_iter()
            .any(|child| contains(child, interner, depth + 1))
}

/// The unbound variables `id` mentions, in first-occurrence order.
#[must_use]
pub fn unbound_vars(id: TypeId, interner: &TypeInterner) -> Vec<u32> {
    let mut found = Vec::new();
    collect_vars(id, interner, 0, &mut found);
    found
}

fn collect_vars(id: TypeId, interner: &TypeInterner, depth: usize, found: &mut Vec<u32>) {
    if depth > MAX_DEPTH {
        return;
    }
    let ty = interner.resolve(id);
    if let ArType::Infer(var) = ty {
        if !found.contains(&var) {
            found.push(var);
        }
        return;
    }
    for child in children(&ty, interner) {
        collect_vars(child, interner, depth + 1, found);
    }
}

/// `id` with every bound variable replaced by its binding, recursively. An
/// unbound variable stays; the caller reports it.
#[must_use]
pub fn resolve_deep(id: TypeId, interner: &TypeInterner) -> TypeId {
    if !interner.has_infer_vars() {
        return id;
    }
    deep(id, interner, 0)
}

fn deep(id: TypeId, interner: &TypeInterner, depth: usize) -> TypeId {
    if depth > MAX_DEPTH {
        return id;
    }
    let ty = interner.resolve(id);
    let one = |inner: TypeId| deep(inner, interner, depth + 1);
    let many = |args: Vec<TypeId>| {
        args.into_iter()
            .map(|arg| deep(arg, interner, depth + 1))
            .collect::<Vec<_>>()
    };
    let rebuilt = match &ty {
        ArType::Infer(_) => return interner.intern(ty),
        ArType::Named(symbol, args) => {
            ArType::named(*symbol, &many(interner.type_args(*args)), interner)
        }
        ArType::Func(params, ret) => {
            ArType::func(&many(interner.type_args(*params)), one(*ret), interner)
        }
        ArType::Tuple(items) => ArType::tuple(&many(interner.type_args(*items)), interner),
        ArType::Result(ok, err) => ArType::Result(one(*ok), one(*err)),
        ArType::Nullable(inner) => ArType::Nullable(one(*inner)),
        ArType::Slice(inner) => ArType::Slice(one(*inner)),
        ArType::MutSlice(inner) => ArType::MutSlice(one(*inner)),
        ArType::Array(len, inner) => ArType::Array(*len, one(*inner)),
        ArType::ConstArray(len, inner) => ArType::ConstArray(*len, one(*inner)),
        ArType::Ptr(inner) => ArType::Ptr(one(*inner)),
        ArType::Ref(inner) => ArType::Ref(one(*inner)),
        ArType::RefMut(inner) => ArType::RefMut(one(*inner)),
        ArType::Option(inner) => ArType::Option(one(*inner)),
        ArType::Coroutine(inner) => ArType::Coroutine(one(*inner)),
        ArType::Poll(inner) => ArType::Poll(one(*inner)),
        ArType::Range(inner) => ArType::Range(one(*inner)),
        ArType::Primitive(_)
        | ArType::Vector(_)
        | ArType::Const(_)
        | ArType::FrozenConst(_)
        | ArType::ConstParam(_)
        | ArType::GenRef
        | ArType::Err
        | ArType::Void
        | ArType::IntLiteral
        | ArType::FloatLiteral
        | ArType::Error => return interner.intern(ty),
    };
    interner.intern(rebuilt)
}

fn children(ty: &ArType, interner: &TypeInterner) -> Vec<TypeId> {
    match ty {
        ArType::Named(_, args) | ArType::Tuple(args) => interner.type_args(*args),
        ArType::Func(params, ret) => {
            let mut all = interner.type_args(*params);
            all.push(*ret);
            all
        }
        ArType::Result(ok, err) => vec![*ok, *err],
        ArType::Nullable(inner)
        | ArType::Slice(inner)
        | ArType::MutSlice(inner)
        | ArType::Array(_, inner)
        | ArType::ConstArray(_, inner)
        | ArType::Ptr(inner)
        | ArType::Ref(inner)
        | ArType::RefMut(inner)
        | ArType::Option(inner)
        | ArType::Coroutine(inner)
        | ArType::Poll(inner)
        | ArType::Range(inner) => vec![*inner],
        ArType::Primitive(_)
        | ArType::Vector(_)
        | ArType::Const(_)
        | ArType::FrozenConst(_)
        | ArType::ConstParam(_)
        | ArType::GenRef
        | ArType::Err
        | ArType::Void
        | ArType::IntLiteral
        | ArType::FloatLiteral
        | ArType::Infer(_)
        | ArType::Error => Vec::new(),
    }
}
