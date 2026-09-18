use rustc_hash::FxHashMap;
use smol_str::SmolStr;

use super::*;
use crate::SymbolId;
use crate::hir::pool::IndexRange;
use crate::layout::{EnumPayloadShape, StructFieldInfo, StructFields};

#[derive(Default)]
struct TestMockProvider {
    fields: FxHashMap<SymbolId, StructFields>,
    generic_params: FxHashMap<SymbolId, Vec<SymbolId>>,
    enum_variants: FxHashMap<SymbolId, Vec<EnumPayloadShape>>,
    repr_c: rustc_hash::FxHashSet<SymbolId>,
}

impl StructLayoutProvider for TestMockProvider {
    fn get_struct_fields(&self, struct_id: SymbolId) -> Option<&StructFields> {
        self.fields.get(&struct_id)
    }
    fn get_generic_params(&self, struct_id: SymbolId) -> Option<&[SymbolId]> {
        self.generic_params.get(&struct_id).map(|v| v.as_slice())
    }
    fn get_enum_variants(&self, enum_id: SymbolId) -> Option<Vec<EnumPayloadShape>> {
        self.enum_variants.get(&enum_id).cloned()
    }
    fn is_repr_c(&self, struct_id: SymbolId) -> bool {
        self.repr_c.contains(&struct_id)
    }
}

fn register_struct(
    provider: &mut TestMockProvider,
    interner: &TypeInterner,
    sym_id: SymbolId,
    field_tys: &[(&str, ArType)],
) -> ArType {
    let mut fields = Vec::new();
    for (i, &(name, ref ty)) in field_tys.iter().enumerate() {
        let ty_id = interner.intern(ty.clone());
        fields.push(StructFieldInfo {
            name: SmolStr::new(name),
            symbol: Some(SymbolId::new(0, 1000 + i as u32)),
            ty: ty_id,
            index: i,
        });
    }
    provider
        .fields
        .insert(sym_id, StructFields::from_entries(fields));
    ArType::Named(sym_id, IndexRange::empty())
}

#[test]
fn sysv_amd64_point_two_i64() {
    let mut provider = TestMockProvider::default();
    let interner = TypeInterner::new();
    let sym = SymbolId::new(0, 100);
    let ty = register_struct(
        &mut provider,
        &interner,
        sym,
        &[
            ("x", ArType::Primitive(Primitive::I64)),
            ("y", ArType::Primitive(Primitive::I64)),
        ],
    );

    let classifier = TargetAbiClassifier::new(TargetAbi::SystemVAmd64, 8);
    let abi = classifier.classify_type(&ty, &interner, &provider);
    match abi {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 2);
            assert_eq!(direct.slots[0].scalar, AbiScalar::I64);
            assert_eq!(direct.slots[0].offset, 0);
            assert_eq!(direct.slots[1].scalar, AbiScalar::I64);
            assert_eq!(direct.slots[1].offset, 8);
        }
        other => panic!("expected direct, got {other:?}"),
    }
}

#[test]
fn sysv_amd64_floats() {
    let mut provider = TestMockProvider::default();
    let interner = TypeInterner::new();
    let sym = SymbolId::new(0, 101);
    let ty = register_struct(
        &mut provider,
        &interner,
        sym,
        &[
            ("x", ArType::Primitive(Primitive::F64)),
            ("y", ArType::Primitive(Primitive::F64)),
        ],
    );

    let classifier = TargetAbiClassifier::new(TargetAbi::SystemVAmd64, 8);
    let abi = classifier.classify_type(&ty, &interner, &provider);
    match abi {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 2);
            assert_eq!(direct.slots[0].scalar, AbiScalar::F64);
            assert_eq!(direct.slots[0].offset, 0);
            assert_eq!(direct.slots[1].scalar, AbiScalar::F64);
            assert_eq!(direct.slots[1].offset, 8);
        }
        other => panic!("expected direct, got {other:?}"),
    }
}

#[test]
fn sysv_amd64_mixed_int_and_float() {
    let mut provider = TestMockProvider::default();
    let interner = TypeInterner::new();
    let sym = SymbolId::new(0, 102);
    let ty = register_struct(
        &mut provider,
        &interner,
        sym,
        &[
            ("id", ArType::Primitive(Primitive::I64)),
            ("ratio", ArType::Primitive(Primitive::F64)),
        ],
    );

    let classifier = TargetAbiClassifier::new(TargetAbi::SystemVAmd64, 8);
    let abi = classifier.classify_type(&ty, &interner, &provider);
    match abi {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 2);
            assert_eq!(direct.slots[0].scalar, AbiScalar::I64);
            assert_eq!(direct.slots[0].offset, 0);
            assert_eq!(direct.slots[1].scalar, AbiScalar::F64);
            assert_eq!(direct.slots[1].offset, 8);
        }
        other => panic!("expected direct, got {other:?}"),
    }
}

#[test]
fn sysv_amd64_single_fields() {
    let mut provider = TestMockProvider::default();
    let interner = TypeInterner::new();
    let sym_i32 = SymbolId::new(0, 103);
    let ty_i32 = register_struct(
        &mut provider,
        &interner,
        sym_i32,
        &[("val", ArType::Primitive(Primitive::I32))],
    );

    let classifier = TargetAbiClassifier::new(TargetAbi::SystemVAmd64, 8);
    let abi = classifier.classify_type(&ty_i32, &interner, &provider);
    match abi {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 1);
            assert_eq!(direct.slots[0].scalar, AbiScalar::I32);
            assert_eq!(direct.slots[0].offset, 0);
        }
        other => panic!("expected direct, got {other:?}"),
    }

    let sym_f32 = SymbolId::new(0, 104);
    let ty_f32 = register_struct(
        &mut provider,
        &interner,
        sym_f32,
        &[("val", ArType::Primitive(Primitive::F32))],
    );
    let abi_f32 = classifier.classify_type(&ty_f32, &interner, &provider);
    match abi_f32 {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 1);
            assert_eq!(direct.slots[0].scalar, AbiScalar::F32);
            assert_eq!(direct.slots[0].offset, 0);
        }
        other => panic!("expected direct, got {other:?}"),
    }
}

#[test]
fn sysv_amd64_large_struct_is_indirect() {
    let mut provider = TestMockProvider::default();
    let interner = TypeInterner::new();
    let sym = SymbolId::new(0, 105);
    let ty = register_struct(
        &mut provider,
        &interner,
        sym,
        &[
            ("a", ArType::Primitive(Primitive::I64)),
            ("b", ArType::Primitive(Primitive::I64)),
            ("c", ArType::Primitive(Primitive::I64)),
        ],
    );

    let classifier = TargetAbiClassifier::new(TargetAbi::SystemVAmd64, 8);
    let abi = classifier.classify_type(&ty, &interner, &provider);
    assert_eq!(abi, ArgAbi::Indirect);
}

#[test]
fn sysv_amd64_empty_struct_is_zero_sized() {
    let mut provider = TestMockProvider::default();
    let interner = TypeInterner::new();
    let sym = SymbolId::new(0, 106);
    let ty = register_struct(&mut provider, &interner, sym, &[]);

    let classifier = TargetAbiClassifier::new(TargetAbi::SystemVAmd64, 8);
    let abi = classifier.classify_type(&ty, &interner, &provider);
    assert_eq!(abi, ArgAbi::ZeroSized);
}

#[test]
fn windows_x64_size_rules() {
    let mut provider = TestMockProvider::default();
    let interner = TypeInterner::new();

    let classifier = TargetAbiClassifier::new(TargetAbi::WindowsX64, 8);

    // 16 bytes: Point -> Indirect on Windows!
    let sym_point = SymbolId::new(0, 200);
    let ty_point = register_struct(
        &mut provider,
        &interner,
        sym_point,
        &[
            ("x", ArType::Primitive(Primitive::I64)),
            ("y", ArType::Primitive(Primitive::I64)),
        ],
    );
    assert_eq!(
        classifier.classify_type(&ty_point, &interner, &provider),
        ArgAbi::Indirect
    );

    // 8 bytes: Pair i32 -> Direct([I64])
    let sym_pair = SymbolId::new(0, 201);
    let ty_pair = register_struct(
        &mut provider,
        &interner,
        sym_pair,
        &[
            ("a", ArType::Primitive(Primitive::I32)),
            ("b", ArType::Primitive(Primitive::I32)),
        ],
    );
    match classifier.classify_type(&ty_pair, &interner, &provider) {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 1);
            assert_eq!(direct.slots[0].scalar, AbiScalar::I64);
        }
        other => panic!("expected direct, got {other:?}"),
    }

    // 4 bytes: Single i32 -> Direct([I32])
    let sym_i32 = SymbolId::new(0, 202);
    let ty_i32 = register_struct(
        &mut provider,
        &interner,
        sym_i32,
        &[("a", ArType::Primitive(Primitive::I32))],
    );
    match classifier.classify_type(&ty_i32, &interner, &provider) {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 1);
            assert_eq!(direct.slots[0].scalar, AbiScalar::I32);
        }
        other => panic!("expected direct, got {other:?}"),
    }
}

#[test]
fn aapcs64_hfa_and_general() {
    let mut provider = TestMockProvider::default();
    let interner = TypeInterner::new();

    let classifier = TargetAbiClassifier::new(TargetAbi::Aapcs64, 8);

    // HFA with 3 f32s -> Direct([F32, F32, F32])
    let sym_hfa = SymbolId::new(0, 300);
    let ty_hfa = register_struct(
        &mut provider,
        &interner,
        sym_hfa,
        &[
            ("x", ArType::Primitive(Primitive::F32)),
            ("y", ArType::Primitive(Primitive::F32)),
            ("z", ArType::Primitive(Primitive::F32)),
        ],
    );
    match classifier.classify_type(&ty_hfa, &interner, &provider) {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 3);
            assert_eq!(direct.slots[0].scalar, AbiScalar::F32);
            assert_eq!(direct.slots[1].scalar, AbiScalar::F32);
            assert_eq!(direct.slots[2].scalar, AbiScalar::F32);
        }
        other => panic!("expected direct HFA, got {other:?}"),
    }

    // General 16 bytes: Point -> Direct([I64, I64])
    let sym_point = SymbolId::new(0, 301);
    let ty_point = register_struct(
        &mut provider,
        &interner,
        sym_point,
        &[
            ("x", ArType::Primitive(Primitive::I64)),
            ("y", ArType::Primitive(Primitive::I64)),
        ],
    );
    match classifier.classify_type(&ty_point, &interner, &provider) {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 2);
            assert_eq!(direct.slots[0].scalar, AbiScalar::I64);
            assert_eq!(direct.slots[1].scalar, AbiScalar::I64);
        }
        other => panic!("expected direct, got {other:?}"),
    }
}

#[test]
fn sysv_amd64_tuples() {
    let provider = TestMockProvider::default();
    let interner = TypeInterner::new();

    let i64_id = interner.intern(ArType::Primitive(Primitive::I64));
    let f64_id = interner.intern(ArType::Primitive(Primitive::F64));
    let tuple_ty = ArType::tuple(&[i64_id, f64_id], &interner);

    let classifier = TargetAbiClassifier::new(TargetAbi::SystemVAmd64, 8);
    match classifier.classify_type(&tuple_ty, &interner, &provider) {
        ArgAbi::Direct(direct) => {
            assert_eq!(direct.slots.len(), 2);
            assert_eq!(direct.slots[0].scalar, AbiScalar::I64);
            assert_eq!(direct.slots[0].offset, 0);
            assert_eq!(direct.slots[1].scalar, AbiScalar::F64);
            assert_eq!(direct.slots[1].offset, 8);
        }
        other => panic!("expected direct, got {other:?}"),
    }
}
