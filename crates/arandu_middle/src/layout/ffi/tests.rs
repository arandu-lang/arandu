use rustc_hash::FxHashMap;

use super::*;
use crate::hir::pool::IndexRange;
use crate::layout::{EnumPayloadShape, StructFieldInfo, StructFields};

#[derive(Default)]
struct MockProvider {
    fields: FxHashMap<SymbolId, StructFields>,
    repr_c: rustc_hash::FxHashSet<SymbolId>,
}

impl StructLayoutProvider for MockProvider {
    fn get_struct_fields(&self, struct_id: SymbolId) -> Option<&StructFields> {
        self.fields.get(&struct_id)
    }

    fn get_generic_params(&self, _struct_id: SymbolId) -> Option<&[SymbolId]> {
        None
    }

    fn get_enum_variants(&self, _enum_id: SymbolId) -> Option<Vec<EnumPayloadShape>> {
        None
    }

    fn is_repr_c(&self, struct_id: SymbolId) -> bool {
        self.repr_c.contains(&struct_id)
    }
}

fn register(
    provider: &mut MockProvider,
    interner: &TypeInterner,
    id: u32,
    repr_c: bool,
    fields: &[(&str, ArType)],
) -> ArType {
    let symbol = SymbolId::new(0, id);
    let entries = fields
        .iter()
        .enumerate()
        .map(|(index, (name, ty))| StructFieldInfo {
            name: smol_str::SmolStr::new(*name),
            symbol: Some(SymbolId::new(0, 5000 + index as u32)),
            ty: interner.intern(ty.clone()),
            index,
        })
        .collect::<Vec<_>>();
    provider
        .fields
        .insert(symbol, StructFields::from_entries(entries));
    if repr_c {
        provider.repr_c.insert(symbol);
    }
    ArType::Named(symbol, IndexRange::empty())
}

fn map(ty: &ArType, interner: &TypeInterner, provider: &MockProvider) -> Result<FfiType, String> {
    ffi_type_of(ty, interner, provider, FfiPosition::Parameter, 0)
}

#[test]
fn scalars_keep_their_language_width() {
    let interner = TypeInterner::new();
    let provider = MockProvider::default();
    for primitive in [
        Primitive::Bool,
        Primitive::I8,
        Primitive::I16,
        Primitive::I32,
        Primitive::I64,
        Primitive::U8,
        Primitive::U16,
        Primitive::U32,
        Primitive::U64,
        Primitive::F32,
        Primitive::F64,
        Primitive::Int,
        Primitive::Uint,
        Primitive::ISize,
        Primitive::USize,
        Primitive::Byte,
    ] {
        let ty = ArType::Primitive(primitive);
        assert_eq!(
            map(&ty, &interner, &provider),
            Ok(FfiType::Primitive(primitive)),
            "{primitive:?} must cross the boundary as a scalar"
        );
    }
    assert!(
        c_primitive_name(Primitive::USize, 8).as_deref() == Some("size_t"),
        "usize must map to the C pointer-width type"
    );
}

#[test]
fn char_is_not_a_c_character() {
    let interner = TypeInterner::new();
    let provider = MockProvider::default();
    let error = map(&ArType::Primitive(Primitive::Char), &interner, &provider)
        .expect_err("char must be rejected");
    assert!(error.contains("Unicode scalar value"), "{error}");
}

#[test]
fn str_crosses_as_an_opaque_two_word_contract() {
    let interner = TypeInterner::new();
    let provider = MockProvider::default();
    let mapped = map(&ArType::Primitive(Primitive::Str), &interner, &provider).expect("str");
    assert_eq!(mapped, FfiType::Str);
    assert!(mapped.is_aggregate());
}

#[test]
fn pointers_do_not_require_repr_c_but_by_value_structs_do() {
    let interner = TypeInterner::new();
    let mut provider = MockProvider::default();
    let frame = register(
        &mut provider,
        &interner,
        1,
        false,
        &[("id", ArType::Primitive(Primitive::U32))],
    );
    let inner = interner.intern(frame.clone());
    let pointer = map(&ArType::Ptr(inner), &interner, &provider).expect("opaque pointer");
    assert_eq!(
        pointer,
        FfiType::Ptr(Box::new(FfiType::Struct(SymbolId::new(0, 1))))
    );

    let error = map(&frame, &interner, &provider).expect_err("by value needs Repr(C)");
    assert!(error.contains("@Repr(\"C\")"), "{error}");

    provider.repr_c.insert(SymbolId::new(0, 1));
    assert_eq!(
        map(&frame, &interner, &provider),
        Ok(FfiType::Struct(SymbolId::new(0, 1)))
    );
}

#[test]
fn references_are_pointers_with_constness() {
    let interner = TypeInterner::new();
    let provider = MockProvider::default();
    let u32_ty = interner.intern(ArType::Primitive(Primitive::U32));
    assert_eq!(
        map(&ArType::Ref(u32_ty), &interner, &provider),
        Ok(FfiType::Ref {
            pointee: Box::new(FfiType::Primitive(Primitive::U32)),
            mutable: false,
        })
    );
    assert_eq!(
        map(&ArType::RefMut(u32_ty), &interner, &provider),
        Ok(FfiType::Ref {
            pointee: Box::new(FfiType::Primitive(Primitive::U32)),
            mutable: true,
        })
    );
    let reference = map(&ArType::Ref(u32_ty), &interner, &provider).expect("reference");
    assert!(
        !reference.is_aggregate(),
        "a reference is a single register"
    );
}

#[test]
fn slices_carry_element_and_mutability() {
    let interner = TypeInterner::new();
    let provider = MockProvider::default();
    let u8_ty = interner.intern(ArType::Primitive(Primitive::U8));
    let immutable = map(&ArType::Slice(u8_ty), &interner, &provider).expect("slice");
    let mutable = map(&ArType::MutSlice(u8_ty), &interner, &provider).expect("mut slice");
    assert_eq!(
        immutable,
        FfiType::Slice {
            element: Box::new(FfiType::Primitive(Primitive::U8)),
            mutable: false,
        }
    );
    assert_eq!(
        mutable,
        FfiType::Slice {
            element: Box::new(FfiType::Primitive(Primitive::U8)),
            mutable: true,
        }
    );
    assert!(immutable.is_aggregate());
    assert_ne!(
        immutable, mutable,
        "mutability must survive into the contract"
    );
}

#[test]
fn arrays_are_struct_fields_only() {
    let interner = TypeInterner::new();
    let mut provider = MockProvider::default();
    let u8_ty = ArType::Primitive(Primitive::U8);
    let u8_id = interner.intern(u8_ty.clone());
    let array = ArType::Array(16, u8_id);
    let frame = register(
        &mut provider,
        &interner,
        7,
        true,
        &[("name", array.clone())],
    );

    assert_eq!(
        map(&frame, &interner, &provider),
        Ok(FfiType::Struct(SymbolId::new(0, 7)))
    );
    let error = map(&array, &interner, &provider).expect_err("arrays decay in signatures");
    assert!(error.contains("decay"), "{error}");
    let field = ffi_type_of(&array, &interner, &provider, FfiPosition::StructField, 0)
        .expect("array field");
    assert_eq!(
        field,
        FfiType::Array {
            element: Box::new(FfiType::Primitive(Primitive::U8)),
            len: 16,
        }
    );
}

#[test]
fn sum_types_teach_the_status_code_pattern() {
    let interner = TypeInterner::new();
    let provider = MockProvider::default();
    let u32_ty = interner.intern(ArType::Primitive(Primitive::U32));
    let result = ArType::Result(u32_ty, u32_ty);
    let error = map(&result, &interner, &provider).expect_err("Result has no C shape");
    assert!(error.contains("status code"), "{error}");
    assert!(error.contains("mut ref"), "{error}");

    let option = ArType::Option(u32_ty);
    let error = map(&option, &interner, &provider).expect_err("Option has no C shape");
    assert!(error.contains("optional"), "{error}");

    let func = ArType::Func(crate::hir::pool::IndexRange::empty(), u32_ty);
    let error = map(&func, &interner, &provider).expect_err("callbacks are not lowered");
    assert!(error.contains("function pointers"), "{error}");
}
