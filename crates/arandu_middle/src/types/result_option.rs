use arandu_parser::{ResultType, TypeExprId, TypeName};

use crate::SymbolTable;

use super::ar_type::ArType;
use super::lower::LowerCtx;
use super::type_interner::{TypeId, TypeInterner};

#[must_use]
pub fn result_type_decl_span(result: &ResultType) -> arandu_lexer::Span {
    match result {
        ResultType::Single { span, .. } | ResultType::Multi { span, .. } => *span,
    }
}

#[must_use]
pub fn type_name_base(name: &TypeName) -> &str {
    name.path.last().map_or("", |s| s.as_str())
}

#[must_use]
pub fn is_err_type(ty: &ArType, interner: &TypeInterner) -> bool {
    matches!(ty, ArType::Err)
        || matches!(
            ty,
            ArType::Nullable(inner) if matches!(interner.resolve(*inner), ArType::Err)
        )
}

/// Extract ok/err from `Result<T,E>`.
#[must_use]
pub fn result_ok_err(ty: &ArType, interner: &TypeInterner) -> Option<(ArType, ArType)> {
    match ty {
        ArType::Result(ok, err) => {
            let ok_ty = interner.resolve(*ok);
            let err_ty = interner.resolve(*err);
            Some((ok_ty, err_ty))
        }
        _ => None,
    }
}

/// Extract ok/err from an interned `Result<T,E>` (avoids an extra outer clone when the
/// caller only has a `TypeId`).
#[must_use]
pub fn result_ok_err_id(id: TypeId, interner: &TypeInterner) -> Option<(ArType, ArType)> {
    match interner.resolve(id) {
        ArType::Result(ok, err) => Some((interner.resolve(ok), interner.resolve(err))),
        _ => None,
    }
}

#[must_use]
pub fn result_ok_err_ids(ty: &ArType) -> Option<(TypeId, TypeId)> {
    match ty {
        ArType::Result(ok, err) => Some((*ok, *err)),
        _ => None,
    }
}

/// Extract ok/err TypeIds from an interned `Result<T,E>` without allocating or cloning ArType trees.
#[must_use]
pub fn result_ok_err_id_fast(id: TypeId, interner: &TypeInterner) -> Option<(TypeId, TypeId)> {
    interner.with_type(id, |ty| match ty {
        ArType::Result(ok, err) => Some((*ok, *err)),
        _ => None,
    })
}

#[must_use]
pub fn is_result_type(ty: &ArType, _interner: &TypeInterner) -> bool {
    matches!(ty, ArType::Result(_, _))
}

#[must_use]
pub fn is_option_type(ty: &ArType) -> bool {
    matches!(ty, ArType::Option(_))
}

/// Is this a `Vec<T>` nominal type? Matches both the bare `Vec` and the
/// qualified `std.*.Vec` surfaces, requiring exactly one type argument.
///
/// Centralized so typeck, AMIR lowering and both backends agree on what
/// "is a vector" means (previously copy-pasted at 7 call sites).
#[must_use]
pub fn is_vec_type(ty: &ArType, symbols: &SymbolTable) -> bool {
    match ty {
        ArType::Named(sym_id, args) => {
            (symbols.get(*sym_id).name == "Vec" || symbols.get(*sym_id).name.ends_with(".Vec"))
                && args.len == 1
        }
        _ => false,
    }
}

/// Element type of an indexable collection (`[T]`, `[]T`, `Vec<T>`, `ptr[T]`)
/// when the resolved base type is one of those; otherwise `None`.
#[must_use]
pub fn index_elem_type(
    ty: &ArType,
    symbols: &SymbolTable,
    interner: &TypeInterner,
) -> Option<TypeId> {
    match ty {
        ArType::Array(_, inner) | ArType::Slice(inner) | ArType::Ptr(inner) => Some(*inner),
        ArType::Named(_, args) if is_vec_type(ty, symbols) => Some(interner.type_args(*args)[0]),
        ArType::Ref(inner) | ArType::RefMut(inner) => Some(*inner),
        _ => None,
    }
}

/// Types that support the `?` operator.
#[must_use]
pub fn try_ok_type(ty: &ArType, interner: &TypeInterner) -> Option<ArType> {
    if let Some((ok, _)) = result_ok_err(ty, interner) {
        return Some(ok);
    }
    match ty {
        ArType::Option(inner) => Some(interner.resolve(*inner)),
        ArType::Nullable(inner) if !is_err_type(ty, interner) => Some(interner.resolve(*inner)),
        _ => None,
    }
}

#[must_use]
pub fn is_tryable_type(ty: &ArType, interner: &TypeInterner) -> bool {
    matches!(ty, ArType::Result(_, _) | ArType::Option(_))
        || (matches!(ty, ArType::Nullable(_)) && !is_err_type(ty, interner))
}

pub(crate) fn lower_builtin_generic(
    name: &TypeName,
    args: &[TypeExprId],
    ctx: &LowerCtx<'_>,
    interner: &mut TypeInterner,
) -> Option<ArType> {
    let resolved_sym = ctx
        .resolved
        .type_refs
        .get(&name.span.into())
        .copied()
        .or_else(|| {
            let base = name.path.last().map(|s| s.as_str())?;
            ctx.symbols.lookup_type(ctx.symbols.global_scope(), base)
        })?;
    let lowered: Vec<ArType> = args
        .iter()
        .map(|&a| super::lower::lower_type_expr_ctx(a, ctx, interner))
        .collect();

    if ctx.symbols.is_result_type(resolved_sym) && lowered.len() == 2 {
        let mut it = lowered.into_iter();
        if let (Some(ok), Some(err)) = (it.next(), it.next()) {
            let ok_id = interner.intern(ok);
            let err_id = interner.intern(err);
            Some(ArType::Result(ok_id, err_id))
        } else {
            None
        }
    } else if ctx.symbols.is_option_type(resolved_sym) && lowered.len() == 1 {
        let mut it = lowered.into_iter();
        if let Some(inner) = it.next() {
            let id = interner.intern(inner);
            Some(ArType::Option(id))
        } else {
            None
        }
    } else if ctx.symbols.is_coroutine_type(resolved_sym) && lowered.len() == 1 {
        let mut it = lowered.into_iter();
        if let Some(inner) = it.next() {
            let id = interner.intern(inner);
            Some(ArType::Coroutine(id))
        } else {
            None
        }
    } else if ctx.symbols.is_poll_type(resolved_sym) && lowered.len() == 1 {
        let mut it = lowered.into_iter();
        if let Some(inner) = it.next() {
            let id = interner.intern(inner);
            Some(ArType::Poll(id))
        } else {
            None
        }
    } else {
        None
    }
}

/// `Poll[T]` → `T` when Ready.
#[must_use]
pub fn is_poll_type(ty: &ArType) -> bool {
    matches!(ty, ArType::Poll(_))
}

#[must_use]
pub fn poll_ready_type(ty: &ArType, interner: &TypeInterner) -> Option<ArType> {
    match ty {
        ArType::Poll(inner) => Some(interner.resolve(*inner)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Primitive;

    fn interner() -> TypeInterner {
        TypeInterner::new()
    }

    fn int_id(i: &mut TypeInterner) -> super::super::type_interner::TypeId {
        i.intern(ArType::Primitive(Primitive::Int))
    }
    fn str_id(i: &mut TypeInterner) -> super::super::type_interner::TypeId {
        i.intern(ArType::Primitive(Primitive::Str))
    }
    // ── is_err_type ──

    #[test]
    fn err_is_err_type() {
        let i = interner();
        assert!(is_err_type(&ArType::Err, &i));
    }

    #[test]
    fn nullable_err_is_err_type() {
        let i = interner();
        let inner = i.intern(ArType::Err);
        assert!(is_err_type(&ArType::Nullable(inner), &i));
    }

    #[test]
    fn int_is_not_err() {
        let i = interner();
        assert!(!is_err_type(&ArType::Primitive(Primitive::Int), &i));
    }

    // ── result_ok_err ──

    #[test]
    fn result_ok_err_extracts() {
        let mut i = interner();
        let ok = int_id(&mut i);
        let err = str_id(&mut i);
        let r = ArType::Result(ok, err);
        let (got_ok, got_err) = result_ok_err(&r, &i).unwrap();
        assert_eq!(got_ok, ArType::Primitive(Primitive::Int));
        assert_eq!(got_err, ArType::Primitive(Primitive::Str));
    }

    #[test]
    fn non_result_returns_none() {
        let i = interner();
        assert!(result_ok_err(&ArType::Primitive(Primitive::Int), &i).is_none());
        assert!(result_ok_err(&ArType::Void, &i).is_none());
    }

    // ── is_result_type ──

    #[test]
    fn is_result_type_true() {
        let mut i = interner();
        let r = ArType::Result(int_id(&mut i), str_id(&mut i));
        assert!(is_result_type(&r, &i));
    }

    #[test]
    fn is_result_type_false() {
        let i = interner();
        assert!(!is_result_type(&ArType::Primitive(Primitive::Int), &i));
        assert!(!is_result_type(
            &ArType::Option(int_id(&mut interner())),
            &i
        ));
    }

    // ── is_option_type ──

    #[test]
    fn option_type_recognized() {
        assert!(is_option_type(&ArType::Option(int_id(&mut interner()))));
    }

    #[test]
    fn non_option_not_recognized() {
        assert!(!is_option_type(&ArType::Primitive(Primitive::Int)));
        assert!(!is_option_type(&ArType::Result(
            int_id(&mut interner()),
            str_id(&mut interner())
        )));
    }

    // ── try_ok_type ──

    #[test]
    fn try_ok_from_result() {
        let mut i = interner();
        let r = ArType::Result(int_id(&mut i), str_id(&mut i));
        assert_eq!(try_ok_type(&r, &i), Some(ArType::Primitive(Primitive::Int)));
    }

    #[test]
    fn try_ok_from_option() {
        let mut i = interner();
        let opt = ArType::Option(int_id(&mut i));
        assert_eq!(
            try_ok_type(&opt, &i),
            Some(ArType::Primitive(Primitive::Int))
        );
    }

    #[test]
    fn try_ok_from_nullable_non_err() {
        let mut i = interner();
        let null = ArType::Nullable(int_id(&mut i));
        assert_eq!(
            try_ok_type(&null, &i),
            Some(ArType::Primitive(Primitive::Int))
        );
    }

    #[test]
    fn try_ok_from_nullable_err_returns_none() {
        let i = interner();
        let inner = i.intern(ArType::Err);
        let null_err = ArType::Nullable(inner);
        assert_eq!(try_ok_type(&null_err, &i), None);
    }

    #[test]
    fn try_ok_non_tryable_returns_none() {
        let i = interner();
        assert_eq!(try_ok_type(&ArType::Primitive(Primitive::Int), &i), None);
        assert_eq!(try_ok_type(&ArType::Void, &i), None);
    }

    // ── is_tryable_type ──

    #[test]
    fn result_option_and_non_err_nullable_are_tryable() {
        let mut i = interner();
        assert!(is_tryable_type(
            &ArType::Result(int_id(&mut i), str_id(&mut i)),
            &i
        ));
        assert!(is_tryable_type(&ArType::Option(int_id(&mut i)), &i));
        assert!(is_tryable_type(&ArType::Nullable(int_id(&mut i)), &i));
    }

    #[test]
    fn nullable_err_is_not_tryable() {
        let i = interner();
        let inner = i.intern(ArType::Err);
        assert!(!is_tryable_type(&ArType::Nullable(inner), &i));
    }

    #[test]
    fn plain_type_not_tryable() {
        let i = interner();
        assert!(!is_tryable_type(&ArType::Primitive(Primitive::Int), &i));
    }

    // ── type_name_base ──

    #[test]
    fn type_name_base_single_path() {
        let name = arandu_parser::TypeName {
            span: arandu_lexer::Span::new(0, 0, 0),
            path: vec![smol_str::SmolStr::new("Result")].into(),
        };
        assert_eq!(type_name_base(&name), "Result");
    }

    #[test]
    fn type_name_base_multi_path() {
        let name = arandu_parser::TypeName {
            span: arandu_lexer::Span::new(0, 0, 0),
            path: vec![
                smol_str::SmolStr::new("std"),
                smol_str::SmolStr::new("core"),
                smol_str::SmolStr::new("String"),
            ]
            .into(),
        };
        assert_eq!(type_name_base(&name), "String");
    }

    #[test]
    fn type_name_base_empty_path() {
        let name = arandu_parser::TypeName {
            span: arandu_lexer::Span::new(0, 0, 0),
            path: smallvec::SmallVec::new(),
        };
        assert_eq!(type_name_base(&name), "");
    }

    // ── lower_builtin_generic ──

    #[test]
    fn lower_builtin_wrong_name_returns_none() {
        let mut i = interner();
        let name = arandu_parser::TypeName {
            span: arandu_lexer::Span::new(0, 0, 0),
            path: vec![smol_str::SmolStr::new("NonExistent")].into(),
        };
        let result = lower_builtin_generic(&name, &[], &create_dummy_ctx(), &mut i);
        assert!(result.is_none());
    }

    fn create_dummy_ctx() -> LowerCtx<'static> {
        use crate::ResolvedNames;
        let pool = Box::new(arandu_parser::ast_pool::AstPool::new());
        let symbols = Box::new(crate::SymbolTable::new(0));
        let resolved = Box::new(ResolvedNames::default());
        LowerCtx {
            pool: Box::leak(pool),
            symbols: Box::leak(symbols),
            scope: crate::ScopeId(0),
            resolved: Box::leak(resolved),
        }
    }

    // ── is_vec_type ──

    #[test]
    fn vec_type_with_one_arg_recognized() {
        let i = interner();
        let mut symbols = crate::SymbolTable::new(0);
        let vec_sym = symbols
            .define(
                crate::ScopeId(0),
                "Vec",
                crate::SymbolKind::Struct,
                arandu_lexer::Span::new(0, 0, 0),
            )
            .unwrap();
        let ty = ArType::named(vec_sym, &[i.intern(ArType::Primitive(Primitive::Int))], &i);
        assert!(is_vec_type(&ty, &symbols));
    }

    #[test]
    fn qualified_vec_type_recognized() {
        let i = interner();
        let mut symbols = crate::SymbolTable::new(0);
        let vec_sym = symbols
            .define(
                crate::ScopeId(0),
                "std.alloc.vec.Vec",
                crate::SymbolKind::Struct,
                arandu_lexer::Span::new(0, 0, 0),
            )
            .unwrap();
        let ty = ArType::named(vec_sym, &[i.intern(ArType::Primitive(Primitive::Int))], &i);
        assert!(is_vec_type(&ty, &symbols));
    }

    #[test]
    fn non_vec_named_not_recognized() {
        let i = interner();
        let mut symbols = crate::SymbolTable::new(0);
        let other_sym = symbols
            .define(
                crate::ScopeId(0),
                "Widget",
                crate::SymbolKind::Struct,
                arandu_lexer::Span::new(0, 0, 0),
            )
            .unwrap();
        let ty = ArType::named(
            other_sym,
            &[i.intern(ArType::Primitive(Primitive::Int))],
            &i,
        );
        assert!(!is_vec_type(&ty, &symbols));
    }

    #[test]
    fn vec_with_wrong_arity_not_recognized() {
        let i = interner();
        let mut symbols = crate::SymbolTable::new(0);
        let vec_sym = symbols
            .define(
                crate::ScopeId(0),
                "Vec",
                crate::SymbolKind::Struct,
                arandu_lexer::Span::new(0, 0, 0),
            )
            .unwrap();
        let int_id = i.intern(ArType::Primitive(Primitive::Int));
        let ty = ArType::named(vec_sym, &[int_id, int_id], &i);
        assert!(!is_vec_type(&ty, &symbols));
    }

    #[test]
    fn non_named_not_recognized() {
        let i = interner();
        let symbols = crate::SymbolTable::new(0);
        let int_id = i.intern(ArType::Primitive(Primitive::Int));
        assert!(!is_vec_type(&ArType::Slice(int_id), &symbols));
        assert!(!is_vec_type(&ArType::Primitive(Primitive::Int), &symbols));
    }

    // ── index_elem_type ──

    #[test]
    fn index_elem_arrays_slices_vecs() {
        let i = interner();
        let mut symbols = crate::SymbolTable::new(0);
        let int_id = i.intern(ArType::Primitive(Primitive::Int));

        let arr = ArType::Array(4, int_id);
        assert_eq!(index_elem_type(&arr, &symbols, &i), Some(int_id));

        let slice = ArType::Slice(int_id);
        assert_eq!(index_elem_type(&slice, &symbols, &i), Some(int_id));

        let vec_sym = symbols
            .define(
                crate::ScopeId(0),
                "Vec",
                crate::SymbolKind::Struct,
                arandu_lexer::Span::new(0, 0, 0),
            )
            .unwrap();
        let vec = ArType::named(vec_sym, &[int_id], &i);
        assert_eq!(index_elem_type(&vec, &symbols, &i), Some(int_id));
    }
}
