use super::*;
use crate::SymbolId;
use crate::hir::pool::IndexRange;
use crate::types::Primitive;

fn new_interner() -> TypeInterner {
    TypeInterner::new()
}

fn int_t(interner: &mut TypeInterner) -> super::super::type_interner::TypeId {
    interner.intern(ArType::Primitive(Primitive::Int))
}
fn bool_t(interner: &mut TypeInterner) -> super::super::type_interner::TypeId {
    interner.intern(ArType::Primitive(Primitive::Bool))
}
fn str_t(interner: &mut TypeInterner) -> super::super::type_interner::TypeId {
    interner.intern(ArType::Primitive(Primitive::Str))
}

// ── Primitive unification ──

#[test]
fn unify_same_primitives() {
    let i = new_interner();
    assert!(unify(
        &ArType::Primitive(Primitive::Int),
        &ArType::Primitive(Primitive::Int),
        &i
    ));
    assert!(unify(
        &ArType::Primitive(Primitive::Bool),
        &ArType::Primitive(Primitive::Bool),
        &i
    ));
    assert!(unify(
        &ArType::Primitive(Primitive::Str),
        &ArType::Primitive(Primitive::Str),
        &i
    ));
}

#[test]
fn unify_different_primitives() {
    let i = new_interner();
    assert!(!unify(
        &ArType::Primitive(Primitive::Int),
        &ArType::Primitive(Primitive::Bool),
        &i
    ));
    assert!(!unify(
        &ArType::Primitive(Primitive::Str),
        &ArType::Primitive(Primitive::Int),
        &i
    ));
    assert!(!unify(
        &ArType::Primitive(Primitive::F32),
        &ArType::Primitive(Primitive::F64),
        &i
    ));
}

// ── Poison (Error) — unifies with everything ──

#[test]
fn error_unifies_with_everything() {
    let mut i = new_interner();
    let cases = [
        ArType::Primitive(Primitive::Int),
        ArType::Primitive(Primitive::Bool),
        ArType::Void,
        ArType::Err,
        ArType::Named(SymbolId::new(0, 42), IndexRange::empty()),
        ArType::Nullable(int_t(&mut i)),
        ArType::Slice(int_t(&mut i)),
        ArType::Array(3, int_t(&mut i)),
        ArType::Ptr(int_t(&mut i)),
        ArType::tuple(&[int_t(&mut i), bool_t(&mut i)], &i),
        ArType::Result(int_t(&mut i), int_t(&mut i)),
        ArType::Option(int_t(&mut i)),
        ArType::Coroutine(int_t(&mut i)),
        ArType::Range(int_t(&mut i)),
        ArType::IntLiteral,
        ArType::FloatLiteral,
        ArType::func(&[], int_t(&mut i), &i),
    ];
    for case in &cases {
        assert!(
            unify(&ArType::Error, case, &i),
            "Error should unify with {case:?}"
        );
        assert!(
            unify(case, &ArType::Error, &i),
            "{case:?} should unify with Error"
        );
    }
}

// ── Any unifies with everything ──

#[test]
fn any_unifies_with_everything() {
    let mut i = new_interner();
    let cases = [
        ArType::Primitive(Primitive::Int),
        ArType::Primitive(Primitive::Bool),
        ArType::Void,
        ArType::Err,
        ArType::Named(SymbolId::new(0, 1), IndexRange::empty()),
        ArType::Nullable(int_t(&mut i)),
        ArType::Slice(int_t(&mut i)),
        ArType::Result(int_t(&mut i), int_t(&mut i)),
        ArType::IntLiteral,
        ArType::Error,
    ];
    for case in &cases {
        assert!(
            unify(&ArType::Primitive(Primitive::Any), case, &i),
            "Any should unify with {case:?}"
        );
        assert!(
            unify(case, &ArType::Primitive(Primitive::Any), &i),
            "{case:?} should unify with Any"
        );
    }
}

// ── Literal absorption ──

#[test]
fn int_literal_absorbs_numeric_primitives() {
    let i = new_interner();
    let numerics = [
        Primitive::Int,
        Primitive::Uint,
        Primitive::I8,
        Primitive::I16,
        Primitive::I32,
        Primitive::I64,
        Primitive::U8,
        Primitive::U16,
        Primitive::U32,
        Primitive::U64,
        Primitive::Byte,
    ];
    for numeric in &numerics {
        assert!(unify(&ArType::IntLiteral, &ArType::Primitive(*numeric), &i));
        assert!(unify(&ArType::Primitive(*numeric), &ArType::IntLiteral, &i));
    }
}

#[test]
fn int_literal_does_not_absorb_non_numeric() {
    let i = new_interner();
    assert!(!unify(
        &ArType::IntLiteral,
        &ArType::Primitive(Primitive::Bool),
        &i
    ));
    assert!(!unify(
        &ArType::IntLiteral,
        &ArType::Primitive(Primitive::Str),
        &i
    ));
    assert!(!unify(
        &ArType::IntLiteral,
        &ArType::Primitive(Primitive::Char),
        &i
    ));
}

#[test]
fn float_literal_absorbs_float_primitives() {
    let i = new_interner();
    let floats = [Primitive::Float, Primitive::F32, Primitive::F64];
    for flt in &floats {
        assert!(unify(&ArType::FloatLiteral, &ArType::Primitive(*flt), &i));
        assert!(unify(&ArType::Primitive(*flt), &ArType::FloatLiteral, &i));
    }
}

#[test]
fn float_literal_does_not_absorb_int() {
    let i = new_interner();
    assert!(!unify(
        &ArType::FloatLiteral,
        &ArType::Primitive(Primitive::Int),
        &i
    ));
    assert!(!unify(
        &ArType::FloatLiteral,
        &ArType::Primitive(Primitive::Bool),
        &i
    ));
}

#[test]
fn literal_literal_unification() {
    let i = new_interner();
    assert!(unify(&ArType::IntLiteral, &ArType::IntLiteral, &i));
    assert!(unify(&ArType::FloatLiteral, &ArType::FloatLiteral, &i));
    assert!(unify(&ArType::IntLiteral, &ArType::FloatLiteral, &i));
    assert!(unify(&ArType::FloatLiteral, &ArType::IntLiteral, &i));
}

// ── Named type unification ──

#[test]
fn named_same_id_no_args() {
    let i = new_interner();
    assert!(unify(
        &ArType::Named(SymbolId::new(0, 1), IndexRange::empty()),
        &ArType::Named(SymbolId::new(0, 1), IndexRange::empty()),
        &i
    ));
}

#[test]
fn named_different_id_no_args() {
    let i = new_interner();
    assert!(!unify(
        &ArType::Named(SymbolId::new(0, 1), IndexRange::empty()),
        &ArType::Named(SymbolId::new(0, 2), IndexRange::empty()),
        &i
    ));
}

#[test]
fn named_with_args_same() {
    let mut i = new_interner();
    let int = int_t(&mut i);
    let a = ArType::named(SymbolId::new(0, 1), &[int], &i);
    assert!(unify(&a.clone(), &a, &i));
}

#[test]
fn named_with_args_different_length() {
    let mut i = new_interner();
    let a = ArType::named(SymbolId::new(0, 1), &[int_t(&mut i)], &i);
    let b = ArType::Named(SymbolId::new(0, 1), IndexRange::empty());
    assert!(!unify(&a, &b, &i));
}

#[test]
fn named_with_args_different_inner_type() {
    let mut i = new_interner();
    let a = ArType::named(SymbolId::new(0, 1), &[int_t(&mut i)], &i);
    let b = ArType::named(SymbolId::new(0, 1), &[bool_t(&mut i)], &i);
    assert!(!unify(&a, &b, &i));
}

// ── Func type unification ──

#[test]
fn func_same_params_and_return() {
    let mut i = new_interner();
    let ret = int_t(&mut i);
    let f = ArType::func(&[int_t(&mut i), bool_t(&mut i)], ret, &i);
    assert!(unify(&f, &f, &i));
}

#[test]
fn func_different_param_count() {
    let mut i = new_interner();
    let ret = int_t(&mut i);
    let a = ArType::func(&[int_t(&mut i)], ret, &i);
    let b = ArType::func(&[int_t(&mut i), bool_t(&mut i)], ret, &i);
    assert!(!unify(&a, &b, &i));
}

#[test]
fn func_different_return() {
    let mut i = new_interner();
    let a = ArType::func(&[int_t(&mut i)], int_t(&mut i), &i);
    let b = ArType::func(&[int_t(&mut i)], bool_t(&mut i), &i);
    assert!(!unify(&a, &b, &i));
}

// ── Nullable unification ──

#[test]
fn nullable_same_inner() {
    let mut i = new_interner();
    let inner = int_t(&mut i);
    assert!(unify(
        &ArType::Nullable(inner),
        &ArType::Nullable(inner),
        &i
    ));
}

#[test]
fn nullable_different_inner() {
    let mut i = new_interner();
    assert!(!unify(
        &ArType::Nullable(int_t(&mut i)),
        &ArType::Nullable(bool_t(&mut i)),
        &i
    ));
}

#[test]
fn nullable_unifies_with_inner_type() {
    let mut i = new_interner();
    let int_id = int_t(&mut i);
    assert!(!unify(
        &ArType::Nullable(int_id),
        &ArType::Primitive(Primitive::Int),
        &i
    ));
    assert!(is_assignable(
        &ArType::Primitive(Primitive::Int),
        &ArType::Nullable(int_id),
        &i
    ));
    assert!(!is_assignable(
        &ArType::Nullable(int_id),
        &ArType::Primitive(Primitive::Int),
        &i
    ));
}

// ── Slice unification ──

#[test]
fn slice_same_element() {
    let mut i = new_interner();
    let elem = int_t(&mut i);
    assert!(unify(&ArType::Slice(elem), &ArType::Slice(elem), &i));
}

#[test]
fn slice_different_element() {
    let mut i = new_interner();
    assert!(!unify(
        &ArType::Slice(int_t(&mut i)),
        &ArType::Slice(bool_t(&mut i)),
        &i
    ));
}

// ── Array unification ──

#[test]
fn array_same_size_and_element() {
    let mut i = new_interner();
    let elem = int_t(&mut i);
    assert!(unify(&ArType::Array(3, elem), &ArType::Array(3, elem), &i));
}

#[test]
fn array_different_size() {
    let mut i = new_interner();
    let elem = int_t(&mut i);
    assert!(!unify(&ArType::Array(3, elem), &ArType::Array(4, elem), &i));
}

#[test]
fn array_different_element() {
    let mut i = new_interner();
    assert!(!unify(
        &ArType::Array(3, int_t(&mut i)),
        &ArType::Array(3, bool_t(&mut i)),
        &i
    ));
}

// ── Ptr unification ──

#[test]
fn ptr_same_inner() {
    let mut i = new_interner();
    let inner = int_t(&mut i);
    assert!(unify(&ArType::Ptr(inner), &ArType::Ptr(inner), &i));
}

#[test]
fn ptr_different_inner() {
    let mut i = new_interner();
    assert!(!unify(
        &ArType::Ptr(int_t(&mut i)),
        &ArType::Ptr(bool_t(&mut i)),
        &i
    ));
}

// ── F2.0 Ref / RefMut unification ──

#[test]
fn ref_same_inner() {
    let mut i = new_interner();
    let inner = int_t(&mut i);
    assert!(unify(&ArType::Ref(inner), &ArType::Ref(inner), &i));
}

#[test]
fn refmut_decays_to_ref() {
    let mut i = new_interner();
    let inner = int_t(&mut i);
    // unify is strict: Ref and RefMut do not unify
    assert!(!unify(&ArType::Ref(inner), &ArType::RefMut(inner), &i));
    // RefMut (exclusive) is assignable to Ref (shared)
    assert!(is_assignable(
        &ArType::RefMut(inner),
        &ArType::Ref(inner),
        &i
    ));
    // Ref (shared) is not assignable to RefMut (exclusive)
    assert!(!is_assignable(
        &ArType::Ref(inner),
        &ArType::RefMut(inner),
        &i
    ));
}

#[test]
fn ref_does_not_unify_with_different_inner() {
    let mut i = new_interner();
    assert!(!unify(
        &ArType::Ref(int_t(&mut i)),
        &ArType::Ref(bool_t(&mut i)),
        &i
    ));
}

// ── Tuple unification ──

#[test]
fn tuple_same_types() {
    let mut i = new_interner();
    let t = ArType::tuple(&[int_t(&mut i), bool_t(&mut i)], &i);
    assert!(unify(&t, &t, &i));
}

#[test]
fn tuple_different_length() {
    let mut i = new_interner();
    let a = ArType::tuple(&[int_t(&mut i)], &i);
    let b = ArType::tuple(&[int_t(&mut i), bool_t(&mut i)], &i);
    assert!(!unify(&a, &b, &i));
}

#[test]
fn tuple_different_element_type() {
    let mut i = new_interner();
    let a = ArType::tuple(&[int_t(&mut i), int_t(&mut i)], &i);
    let b = ArType::tuple(&[int_t(&mut i), bool_t(&mut i)], &i);
    assert!(!unify(&a, &b, &i));
}

// ── Result unification ──

#[test]
fn result_same_ok_err() {
    let mut i = new_interner();
    let r = ArType::Result(int_t(&mut i), str_t(&mut i));
    assert!(unify(&r, &r, &i));
}

#[test]
fn result_different_ok() {
    let mut i = new_interner();
    let s = str_t(&mut i);
    let a = ArType::Result(int_t(&mut i), s);
    let b = ArType::Result(bool_t(&mut i), s);
    assert!(!unify(&a, &b, &i));
}

#[test]
fn result_different_err() {
    let mut i = new_interner();
    let int_id = int_t(&mut i);
    let a = ArType::Result(int_id, str_t(&mut i));
    let b = ArType::Result(int_id, bool_t(&mut i));
    assert!(!unify(&a, &b, &i));
}

// ── Option / Coroutine / Range unification ──

#[test]
fn option_same_inner() {
    let mut i = new_interner();
    let inner = int_t(&mut i);
    assert!(unify(&ArType::Option(inner), &ArType::Option(inner), &i));
}

#[test]
fn option_different_inner() {
    let mut i = new_interner();
    assert!(!unify(
        &ArType::Option(int_t(&mut i)),
        &ArType::Option(bool_t(&mut i)),
        &i
    ));
}

#[test]
fn coroutine_same_inner() {
    let mut i = new_interner();
    let inner = int_t(&mut i);
    assert!(unify(
        &ArType::Coroutine(inner),
        &ArType::Coroutine(inner),
        &i
    ));
}

#[test]
fn range_same_inner() {
    let mut i = new_interner();
    let inner = int_t(&mut i);
    assert!(unify(&ArType::Range(inner), &ArType::Range(inner), &i));
}

// ── Err / Void unification ──

#[test]
fn err_with_err() {
    let i = new_interner();
    assert!(unify(&ArType::Err, &ArType::Err, &i));
}

#[test]
fn err_does_not_unify_with_void() {
    let i = new_interner();
    assert!(!unify(&ArType::Err, &ArType::Void, &i));
    assert!(!unify(&ArType::Void, &ArType::Err, &i));
}

#[test]
fn void_with_void() {
    let i = new_interner();
    assert!(unify(&ArType::Void, &ArType::Void, &i));
}

// ── Cross-category mismatches ──

#[test]
fn primitive_does_not_unify_with_named() {
    let i = new_interner();
    assert!(!unify(
        &ArType::Primitive(Primitive::Int),
        &ArType::Named(SymbolId::new(0, 1), IndexRange::empty()),
        &i
    ));
}

#[test]
fn nullablity_unwraps_once_only() {
    let mut i = new_interner();
    let int_id = int_t(&mut i);
    let null_int = ArType::Nullable(int_id);
    assert!(!unify(&null_int, &ArType::Primitive(Primitive::Int), &i));
    assert!(is_assignable(
        &ArType::Primitive(Primitive::Int),
        &null_int,
        &i
    ));
    assert!(!is_assignable(
        &null_int,
        &ArType::Primitive(Primitive::Int),
        &i
    ));
    // Nullable<Int> and plain Named do NOT unify or assign
    assert!(!unify(
        &null_int,
        &ArType::Named(SymbolId::new(0, 1), IndexRange::empty()),
        &i
    ));
    assert!(!is_assignable(
        &null_int,
        &ArType::Named(SymbolId::new(0, 1), IndexRange::empty()),
        &i
    ));
}

// ── unify_return ──

#[test]
fn return_unifies_direct_match() {
    let i = new_interner();
    assert!(unify_return_type(
        &ArType::Primitive(Primitive::Int),
        &ArType::Primitive(Primitive::Int),
        &i
    ));
}

#[test]
fn return_unifies_result_ok_err() {
    let mut i = new_interner();
    let ok_i = int_t(&mut i);
    let err_i = str_t(&mut i);
    let expected = ArType::Result(ok_i, err_i);
    let actual = ArType::Result(int_t(&mut i), str_t(&mut i));
    assert!(unify_return_type(&expected, &actual, &i));
}

#[test]
fn return_unifies_result_void_err_with_nil() {
    let i = new_interner();
    let void_id = i.intern(ArType::Void);
    let err_type_id = i.intern(ArType::Error);
    let expected = ArType::Result(void_id, i.intern(ArType::Err));
    let actual = ArType::Nullable(err_type_id);
    assert!(unify_return_type(&expected, &actual, &i));
}

#[test]
fn return_unifies_both_err() {
    let i = new_interner();
    let void_id = i.intern(ArType::Void);
    let err_id = i.intern(ArType::Err);
    let expected = ArType::Result(void_id, err_id);
    assert!(unify_return_type(&expected, &ArType::Err, &i));
    assert!(unify_return_type(
        &expected,
        &ArType::Nullable(i.intern(ArType::Error)),
        &i
    ));
}

#[test]
fn return_rejects_mismatch() {
    let i = new_interner();
    assert!(!unify_return_type(
        &ArType::Primitive(Primitive::Int),
        &ArType::Primitive(Primitive::Bool),
        &i
    ));
}

// ── resolve_literal_pair ──

#[test]
fn literal_pair_int_literal_with_concrete() {
    let result = resolve_literal_pair(&ArType::IntLiteral, &ArType::Primitive(Primitive::I32));
    assert_eq!(result, ArType::Primitive(Primitive::I32));

    let result = resolve_literal_pair(&ArType::Primitive(Primitive::I32), &ArType::IntLiteral);
    assert_eq!(result, ArType::Primitive(Primitive::I32));
}

#[test]
fn literal_pair_float_literal_with_concrete() {
    let result = resolve_literal_pair(&ArType::FloatLiteral, &ArType::Primitive(Primitive::F64));
    assert_eq!(result, ArType::Primitive(Primitive::F64));
}

#[test]
fn literal_pair_two_int_literals() {
    let result = resolve_literal_pair(&ArType::IntLiteral, &ArType::IntLiteral);
    assert_eq!(result, ArType::IntLiteral);
}

#[test]
fn literal_pair_two_float_literals() {
    let result = resolve_literal_pair(&ArType::FloatLiteral, &ArType::FloatLiteral);
    assert_eq!(result, ArType::FloatLiteral);
}

#[test]
fn literal_pair_int_and_float_literals() {
    let result = resolve_literal_pair(&ArType::IntLiteral, &ArType::FloatLiteral);
    assert_eq!(result, ArType::FloatLiteral);

    let result = resolve_literal_pair(&ArType::FloatLiteral, &ArType::IntLiteral);
    assert_eq!(result, ArType::FloatLiteral);
}

#[test]
fn literal_pair_both_concrete_returns_first() {
    let result = resolve_literal_pair(
        &ArType::Primitive(Primitive::Int),
        &ArType::Primitive(Primitive::Bool),
    );
    assert_eq!(result, ArType::Primitive(Primitive::Int));
}

#[test]
fn literal_pair_literal_absorbs_u32() {
    let result = resolve_literal_pair(&ArType::IntLiteral, &ArType::Primitive(Primitive::U32));
    assert_eq!(result, ArType::Primitive(Primitive::U32));
}

#[test]
fn literal_pair_float_literal_with_int_concrete_promotes() {
    // IntLiteral + Float concrete -> float should win
    let result = resolve_literal_pair(&ArType::IntLiteral, &ArType::Primitive(Primitive::Float));
    assert_eq!(result, ArType::Primitive(Primitive::Float));
}
