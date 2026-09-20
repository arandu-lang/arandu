use super::ar_type::ArType;
use super::type_interner::{TypeId, TypeInterner};
use crate::SymbolId;

/// Map from type-parameter `SymbolId` to concrete type.
pub type GenericSubst = smallvec::SmallVec<[(SymbolId, ArType); 2]>;

#[must_use]
pub fn build_subst(param_symbols: &[SymbolId], args: &[ArType]) -> GenericSubst {
    let mut subst = GenericSubst::new();
    for (param, arg) in param_symbols.iter().zip(args.iter()) {
        subst.push((*param, arg.clone()));
    }
    subst
}

/// Build a subst from interned type-argument ids.
#[must_use]
pub fn build_subst_ids(
    param_symbols: &[SymbolId],
    arg_ids: &[TypeId],
    interner: &TypeInterner,
) -> GenericSubst {
    let args: smallvec::SmallVec<[ArType; 2]> =
        arg_ids.iter().map(|&id| interner.resolve(id)).collect();
    build_subst(param_symbols, &args)
}

/// Substitute and re-intern a type id.
#[must_use]
pub fn substitute_type_id(id: TypeId, subst: &GenericSubst, interner: &TypeInterner) -> TypeId {
    let ty = interner.resolve(id);
    let new_ty = substitute_type(&ty, subst, interner);
    interner.intern(new_ty)
}

pub fn substitute_type(ty: &ArType, subst: &GenericSubst, interner: &TypeInterner) -> ArType {
    match ty {
        ArType::ConstParam(symbol) => subst
            .iter()
            .find_map(|(param, concrete)| (param == symbol).then(|| concrete.clone()))
            .unwrap_or_else(|| ty.clone()),
        ArType::Named(id, args) => {
            if let Some((_, concrete)) = subst.iter().find(|(param, _)| param == id) {
                return concrete.clone();
            }
            let old_args = interner.type_args(*args);
            let new_args: Vec<TypeId> = old_args
                .iter()
                .map(|&a| {
                    let resolved = interner.resolve(a);
                    let substituted = substitute_type(&resolved, subst, interner);
                    interner.intern(substituted)
                })
                .collect();
            ArType::named(*id, &new_args, interner)
        }
        ArType::Nullable(inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            ArType::Nullable(id)
        }
        ArType::Option(inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            ArType::Option(id)
        }
        ArType::Coroutine(inner) | ArType::Poll(inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            match ty {
                ArType::Coroutine(_) => ArType::Coroutine(id),
                _ => ArType::Poll(id),
            }
        }
        ArType::Range(inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            ArType::Range(id)
        }
        ArType::Result(ok, err) => {
            let resolved_ok = interner.resolve(*ok);
            let resolved_err = interner.resolve(*err);
            let subst_ok = substitute_type(&resolved_ok, subst, interner);
            let ok_id = interner.intern(subst_ok);
            let subst_err = substitute_type(&resolved_err, subst, interner);
            let err_id = interner.intern(subst_err);
            ArType::Result(ok_id, err_id)
        }
        ArType::Ptr(inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            ArType::Ptr(id)
        }
        ArType::Ref(inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            ArType::Ref(id)
        }
        ArType::RefMut(inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            ArType::RefMut(id)
        }
        ArType::GenRef => ArType::GenRef,
        ArType::Slice(inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            ArType::Slice(id)
        }
        ArType::Array(n, inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            ArType::Array(*n, id)
        }
        ArType::ConstArray(param, inner) => {
            let resolved = interner.resolve(*inner);
            let substituted = substitute_type(&resolved, subst, interner);
            let id = interner.intern(substituted);
            match subst
                .iter()
                .find_map(|(symbol, value)| (symbol == param).then_some(value))
            {
                Some(ArType::Const(length)) => ArType::Array(*length, id),
                Some(ArType::ConstParam(replacement)) => ArType::ConstArray(*replacement, id),
                _ => ArType::ConstArray(*param, id),
            }
        }
        ArType::Tuple(items) => {
            let old_items = interner.type_args(*items);
            let new_items: Vec<TypeId> = old_items
                .iter()
                .map(|&t| {
                    let resolved = interner.resolve(t);
                    let substituted = substitute_type(&resolved, subst, interner);
                    interner.intern(substituted)
                })
                .collect();
            ArType::tuple(&new_items, interner)
        }
        ArType::Func(params, ret) => {
            let old_params = interner.type_args(*params);
            let new_params: Vec<TypeId> = old_params
                .iter()
                .map(|&p| {
                    let resolved = interner.resolve(p);
                    let substituted = substitute_type(&resolved, subst, interner);
                    interner.intern(substituted)
                })
                .collect();
            let resolved_ret = interner.resolve(*ret);
            let subst_ret = substitute_type(&resolved_ret, subst, interner);
            let ret_id = interner.intern(subst_ret);
            ArType::func(&new_params, ret_id, interner)
        }
        _ => ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_interner() -> TypeInterner {
        TypeInterner::new()
    }

    // ── build_subst ──

    #[test]
    fn build_subst_empty() {
        let subst = build_subst(&[], &[]);
        assert!(subst.is_empty());
    }

    #[test]
    fn build_subst_single() {
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        assert_eq!(subst.len(), 1);
        assert_eq!(subst[0].0, SymbolId::new(0, 1));
        assert_eq!(subst[0].1, ArType::Primitive(super::super::Primitive::Int));
    }

    #[test]
    fn build_subst_multiple() {
        let subst = build_subst(
            &[SymbolId::new(0, 1), SymbolId::new(0, 2)],
            &[
                ArType::Primitive(super::super::Primitive::Int),
                ArType::Primitive(super::super::Primitive::Bool),
            ],
        );
        assert_eq!(subst.len(), 2);
    }

    #[test]
    fn build_subst_ignores_extra_params() {
        let subst = build_subst(
            &[SymbolId::new(0, 1), SymbolId::new(0, 2)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        assert_eq!(subst.len(), 1);
    }

    // ── substitute_type ──

    #[test]
    fn substitute_simple_named() {
        let i = new_interner();
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        let ty = ArType::named(SymbolId::new(0, 1), &[], &i);
        let result = substitute_type(&ty, &subst, &i);
        assert_eq!(result, ArType::Primitive(super::super::Primitive::Int));
    }

    #[test]
    fn substitute_non_param_named_unchanged() {
        let i = new_interner();
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        let ty = ArType::named(SymbolId::new(0, 2), &[], &i);
        let result = substitute_type(&ty, &subst, &i);
        assert_eq!(result, ArType::named(SymbolId::new(0, 2), &[], &i));
    }

    #[test]
    fn substitute_named_with_generic_args() {
        let i = new_interner();
        let inner = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let ty = ArType::named(SymbolId::new(0, 3), &[inner], &i);
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_inner = i.intern(ArType::Primitive(super::super::Primitive::Int));
        assert_eq!(
            result,
            ArType::named(SymbolId::new(0, 3), &[expected_inner], &i)
        );
    }

    #[test]
    fn substitute_nullable() {
        let i = new_interner();
        let inner = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let ty = ArType::Nullable(inner);
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_inner = i.intern(ArType::Primitive(super::super::Primitive::Int));
        assert_eq!(result, ArType::Nullable(expected_inner));
    }

    #[test]
    fn substitute_option() {
        let i = new_interner();
        let inner = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let ty = ArType::Option(inner);
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_inner = i.intern(ArType::Primitive(super::super::Primitive::Int));
        assert_eq!(result, ArType::Option(expected_inner));
    }

    #[test]
    fn substitute_result() {
        let i = new_interner();
        let ok_inner = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let err_inner = i.intern(ArType::named(SymbolId::new(0, 2), &[], &i));
        let ty = ArType::Result(ok_inner, err_inner);
        let subst = build_subst(
            &[SymbolId::new(0, 1), SymbolId::new(0, 2)],
            &[
                ArType::Primitive(super::super::Primitive::Int),
                ArType::Primitive(super::super::Primitive::Str),
            ],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_ok = i.intern(ArType::Primitive(super::super::Primitive::Int));
        let expected_err = i.intern(ArType::Primitive(super::super::Primitive::Str));
        assert_eq!(result, ArType::Result(expected_ok, expected_err));
    }

    #[test]
    fn substitute_slice() {
        let i = new_interner();
        let inner = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let ty = ArType::Slice(inner);
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_inner = i.intern(ArType::Primitive(super::super::Primitive::Int));
        assert_eq!(result, ArType::Slice(expected_inner));
    }

    #[test]
    fn substitute_array() {
        let i = new_interner();
        let inner = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let ty = ArType::Array(4, inner);
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_inner = i.intern(ArType::Primitive(super::super::Primitive::Int));
        assert_eq!(result, ArType::Array(4, expected_inner));
    }

    #[test]
    fn substitute_const_array_parameter_with_another_parameter() {
        let i = new_interner();
        let declared = SymbolId::new(0, 1);
        let forwarded = SymbolId::new(0, 2);
        let elem = i.intern(ArType::Primitive(super::super::Primitive::Int));
        let ty = ArType::ConstArray(declared, elem);
        let subst = build_subst(&[declared], &[ArType::ConstParam(forwarded)]);

        assert_eq!(
            substitute_type(&ty, &subst, &i),
            ArType::ConstArray(forwarded, elem)
        );
    }

    #[test]
    fn substitute_ptr() {
        let i = new_interner();
        let inner = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let ty = ArType::Ptr(inner);
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_inner = i.intern(ArType::Primitive(super::super::Primitive::Int));
        assert_eq!(result, ArType::Ptr(expected_inner));
    }

    #[test]
    fn substitute_tuple() {
        let i = new_interner();
        let a = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let b = i.intern(ArType::named(SymbolId::new(0, 2), &[], &i));
        let ty = ArType::tuple(&[a, b], &i);
        let subst = build_subst(
            &[SymbolId::new(0, 1), SymbolId::new(0, 2)],
            &[
                ArType::Primitive(super::super::Primitive::Int),
                ArType::Primitive(super::super::Primitive::Bool),
            ],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_a = i.intern(ArType::Primitive(super::super::Primitive::Int));
        let expected_b = i.intern(ArType::Primitive(super::super::Primitive::Bool));
        assert_eq!(result, ArType::tuple(&[expected_a, expected_b], &i));
    }

    #[test]
    fn substitute_func() {
        let i = new_interner();
        let param = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let ret = i.intern(ArType::named(SymbolId::new(0, 2), &[], &i));
        let ty = ArType::func(&[param], ret, &i);
        let subst = build_subst(
            &[SymbolId::new(0, 1), SymbolId::new(0, 2)],
            &[
                ArType::Primitive(super::super::Primitive::Int),
                ArType::Primitive(super::super::Primitive::Bool),
            ],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_param = i.intern(ArType::Primitive(super::super::Primitive::Int));
        let expected_ret = i.intern(ArType::Primitive(super::super::Primitive::Bool));
        assert_eq!(result, ArType::func(&[expected_param], expected_ret, &i));
    }

    #[test]
    fn substitute_primitive_unchanged() {
        let i = new_interner();
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        assert_eq!(
            substitute_type(
                &ArType::Primitive(super::super::Primitive::Bool),
                &subst,
                &i
            ),
            ArType::Primitive(super::super::Primitive::Bool)
        );
    }

    #[test]
    fn substitute_range() {
        let i = new_interner();
        let inner = i.intern(ArType::named(SymbolId::new(0, 1), &[], &i));
        let ty = ArType::Range(inner);
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        let result = substitute_type(&ty, &subst, &i);
        let expected_inner = i.intern(ArType::Primitive(super::super::Primitive::Int));
        assert_eq!(result, ArType::Range(expected_inner));
    }

    #[test]
    fn substitute_void_err_literals_unchanged() {
        let i = new_interner();
        let subst = build_subst(
            &[SymbolId::new(0, 1)],
            &[ArType::Primitive(super::super::Primitive::Int)],
        );
        assert_eq!(substitute_type(&ArType::Void, &subst, &i), ArType::Void);
        assert_eq!(substitute_type(&ArType::Err, &subst, &i), ArType::Err);
        assert_eq!(
            substitute_type(&ArType::IntLiteral, &subst, &i),
            ArType::IntLiteral
        );
        assert_eq!(substitute_type(&ArType::Error, &subst, &i), ArType::Error);
    }

    #[test]
    fn substitution_does_not_expand_a_concrete_replacement_recursively() {
        let interner = new_interner();
        let parameter = SymbolId::new(0, 1);
        let parameter_ty = ArType::named(parameter, &[], &interner);
        let parameter_id = interner.intern(parameter_ty.clone());
        let replacement = ArType::Option(parameter_id);
        let subst = build_subst(&[parameter], std::slice::from_ref(&replacement));

        assert_eq!(
            substitute_type(&parameter_ty, &subst, &interner),
            replacement,
            "substitution must insert a finite replacement without alias expansion"
        );
    }
}
