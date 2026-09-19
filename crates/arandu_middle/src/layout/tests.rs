use super::*;
use crate::hir::pool::IndexRange;
use crate::newtype_index;

struct LayoutEngine(super::LayoutEngine);

impl LayoutEngine {
    fn new(pointer_width: u64) -> Self {
        Self(super::LayoutEngine::new(pointer_width))
    }

    fn from_data_layout(data_layout: DataLayout) -> Self {
        Self(super::LayoutEngine::from_data_layout(data_layout))
    }

    #[allow(clippy::panic)]
    fn layout_of(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> TypeLayout {
        match self.0.layout_of(type_id, interner, provider) {
            Ok(layout) => layout,
            Err(error) => panic!("expected valid test layout, got {error}"),
        }
    }

    fn try_layout_of(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<TypeLayout, LayoutError> {
        self.0.layout_of(type_id, interner, provider)
    }

    fn gen_payload_layout(
        &self,
        type_id: TypeId,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<GenPayloadLayout, LayoutError> {
        self.0.gen_payload_layout(type_id, interner, provider)
    }

    fn fat_ptr_len_offset(&self) -> u64 {
        self.0.fat_ptr_len_offset()
    }

    fn soo_layout_for_type(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> Result<SooLayout, LayoutError> {
        self.0.soo_layout_for_type(ty, interner, provider)
    }

    fn is_soo_eligible(
        &self,
        ty: &ArType,
        interner: &TypeInterner,
        provider: &dyn StructLayoutProvider,
    ) -> bool {
        self.0.is_soo_eligible(ty, interner, provider)
    }
}

newtype_index!(TestId);

#[test]
fn empty_range_has_no_ids() {
    let range = DenseRange::empty();
    assert!(range.is_empty());
    assert_eq!(range.as_range(), 0..0);
    assert!(range.iter_ids::<TestId>().next().is_none());
}

#[test]
fn typed_iteration_returns_dense_ids() {
    assert_eq!(TestId::from_usize(9).as_usize(), 9);
    let ids: Vec<_> = DenseRange::new(2, 3).iter_ids::<TestId>().collect();
    assert_eq!(ids, vec![TestId(2), TestId(3), TestId(4)]);
}

struct MockProvider;
impl StructLayoutProvider for MockProvider {
    fn get_struct_fields(&self, _struct_id: SymbolId) -> Option<&StructFields> {
        None
    }
    fn get_generic_params(&self, _struct_id: SymbolId) -> Option<&[SymbolId]> {
        None
    }
    fn get_enum_variants(&self, _enum_id: SymbolId) -> Option<Vec<EnumPayloadShape>> {
        None
    }
}

#[test]
fn test_primitive_layouts_64bit() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;

    let u8_id = interner.intern(ArType::Primitive(Primitive::U8));
    let layout_u8 = engine.layout_of(u8_id, &interner, &provider);
    assert_eq!(layout_u8.size, 1);
    assert_eq!(layout_u8.align, 1);

    let i32_id = interner.intern(ArType::Primitive(Primitive::I32));
    let layout_i32 = engine.layout_of(i32_id, &interner, &provider);
    assert_eq!(layout_i32.size, 4);
    assert_eq!(layout_i32.align, 4);

    let str_id = interner.intern(ArType::Primitive(Primitive::Str));
    let layout_str = engine.layout_of(str_id, &interner, &provider);
    assert_eq!(layout_str.size, 16);
    assert_eq!(layout_str.align, 8);
    assert_eq!(layout_str.field_offsets, vec![0, 8]);
}

#[test]
fn test_primitive_layouts_32bit() {
    let engine = LayoutEngine::new(4);
    let interner = TypeInterner::new();
    let provider = MockProvider;

    let str_id = interner.intern(ArType::Primitive(Primitive::Str));
    let layout_str = engine.layout_of(str_id, &interner, &provider);
    // Fat pointer: ptr(4) + len as usize(4) → size 8 on 32-bit.
    assert_eq!(layout_str.size, 8);
    assert_eq!(layout_str.align, 4);
    assert_eq!(layout_str.field_offsets, vec![0, 4]);
    assert_eq!(engine.fat_ptr_len_offset(), 4);

    let int_id = interner.intern(ArType::Primitive(Primitive::Int));
    let layout_int = engine.layout_of(int_id, &interner, &provider);
    assert_eq!(layout_int.size, 4);
    assert_eq!(layout_int.align, 4);

    let uint_id = interner.intern(ArType::Primitive(Primitive::Uint));
    let layout_uint = engine.layout_of(uint_id, &interner, &provider);
    assert_eq!(layout_uint.size, 4);
    assert_eq!(layout_uint.align, 4);

    let int_lit_id = interner.intern(ArType::IntLiteral);
    let layout_int_lit = engine.layout_of(int_lit_id, &interner, &provider);
    assert_eq!(layout_int_lit.size, 4);
    assert_eq!(layout_int_lit.align, 4);
}

#[test]
fn gen_payload_layout_uses_target_not_host_width() {
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let string = interner.intern(ArType::Primitive(Primitive::Str));

    let layout_64 = LayoutEngine::new(8)
        .gen_payload_layout(string, &interner, &provider)
        .unwrap();
    let layout_32 = LayoutEngine::from_data_layout(DataLayout::i686_sysv())
        .gen_payload_layout(string, &interner, &provider)
        .unwrap();

    assert_eq!(layout_64, GenPayloadLayout { size: 16, align: 8 });
    assert_eq!(layout_32, GenPayloadLayout { size: 8, align: 4 });
}

#[test]
fn gen_payload_layout_preserves_zero_sized_void_contract() {
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let void = interner.intern(ArType::Void);
    let layout = LayoutEngine::new(8)
        .gen_payload_layout(void, &interner, &provider)
        .unwrap();

    assert_eq!(layout, GenPayloadLayout { size: 0, align: 1 });
}

#[derive(Default)]
struct StructMockProvider {
    fields: FxHashMap<SymbolId, StructFields>,
    generic_params: FxHashMap<SymbolId, Vec<SymbolId>>,
    enum_variants: FxHashMap<SymbolId, Vec<EnumPayloadShape>>,
    repr_c: rustc_hash::FxHashSet<SymbolId>,
}

impl StructLayoutProvider for StructMockProvider {
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

#[test]
fn test_all_primitive_layouts() {
    for &ptr_width in &[4u64, 8] {
        let engine = LayoutEngine::new(ptr_width);
        let interner = TypeInterner::new();
        let provider = MockProvider;

        let cases = [
            (Primitive::I8, 1u64, 1u64),
            (Primitive::U8, 1, 1),
            (Primitive::Byte, 1, 1),
            (Primitive::Bool, 1, 1),
            (Primitive::Char, 4, 4),
            (Primitive::I16, 2, 2),
            (Primitive::U16, 2, 2),
            (Primitive::I32, 4, 4),
            (Primitive::U32, 4, 4),
            (Primitive::F32, 4, 4),
            // Language float is always IEEE f64 (DataLayout), not pointer-width.
            (Primitive::Float, 8, 8),
            (Primitive::Int, ptr_width, ptr_width),
            (Primitive::Uint, ptr_width, ptr_width),
            (Primitive::I64, 8, 8),
            (Primitive::U64, 8, 8),
            (Primitive::F64, 8, 8),
            (Primitive::Any, ptr_width, ptr_width),
        ];
        for (prim, size, align) in cases {
            let tid = interner.intern(ArType::Primitive(prim));
            let layout = engine.layout_of(tid, &interner, &provider);
            assert_eq!(layout.size, size, "{prim:?} size at ptr_width={ptr_width}");
            assert_eq!(
                layout.align, align,
                "{prim:?} align at ptr_width={ptr_width}"
            );
            assert!(layout.field_offsets.is_empty());
        }
    }
}

#[test]
fn test_i686_sysv_i64_align_4() {
    let engine = LayoutEngine::from_data_layout(DataLayout::i686_sysv());
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let i64_id = interner.intern(ArType::Primitive(Primitive::I64));
    let layout = engine.layout_of(i64_id, &interner, &provider);
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 4);
    let f64_id = interner.intern(ArType::Primitive(Primitive::F64));
    let fl = engine.layout_of(f64_id, &interner, &provider);
    assert_eq!(fl.size, 8);
    assert_eq!(fl.align, 4);
    // Fat pointer still 8 bytes on 32-bit pointer width.
    let str_id = interner.intern(ArType::Primitive(Primitive::Str));
    let sl = engine.layout_of(str_id, &interner, &provider);
    assert_eq!(sl.size, 8);
    assert_eq!(sl.field_offsets, vec![0, 4]);
}

#[test]
fn test_ptr_layout() {
    for &ptr_width in &[4u64, 8] {
        let engine = LayoutEngine::new(ptr_width);
        let interner = TypeInterner::new();
        let provider = MockProvider;
        let inner = interner.intern(ArType::Primitive(Primitive::I32));
        let ptr_ty = ArType::Ptr(inner);
        let tid = interner.intern(ptr_ty);
        let layout = engine.layout_of(tid, &interner, &provider);
        assert_eq!(layout.size, ptr_width);
        assert_eq!(layout.align, ptr_width);
    }
}

#[test]
fn test_slice_layout() {
    for &ptr_width in &[4u64, 8] {
        let engine = LayoutEngine::new(ptr_width);
        let interner = TypeInterner::new();
        let provider = MockProvider;
        let inner = interner.intern(ArType::Primitive(Primitive::I32));
        let slice_ty = ArType::Slice(inner);
        let tid = interner.intern(slice_ty);
        let layout = engine.layout_of(tid, &interner, &provider);
        // Fat pointer: ptr(W) + len as usize(W)
        assert_eq!(layout.field_offsets, vec![0, ptr_width]);
        assert_eq!(layout.size, ptr_width * 2);
        assert_eq!(layout.align, ptr_width);
    }
}

#[test]
fn test_array_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let elem = interner.intern(ArType::Primitive(Primitive::I32));
    let arr_ty = ArType::Array(5, elem);
    let tid = interner.intern(arr_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    assert_eq!(layout.size, 20); // 5 * 4
    assert_eq!(layout.align, 4);
}

#[test]
fn array_layout_rejects_target_size_bound_on_32_and_64_bit() {
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let byte = interner.intern(ArType::Primitive(Primitive::U8));

    for (pointer_width, bound) in [(4, 1_u64 << 31), (8, 1_u64 << 63)] {
        let engine = LayoutEngine::new(pointer_width);
        let last_valid = interner.intern(ArType::Array(bound - 1, byte));
        assert_eq!(
            engine
                .try_layout_of(last_valid, &interner, &provider)
                .map(|layout| layout.size),
            Ok(bound - 1)
        );

        let too_large = interner.intern(ArType::Array(bound, byte));
        assert_eq!(
            engine.try_layout_of(too_large, &interner, &provider),
            Err(LayoutError::SizeOverflow {
                operation: LayoutOperation::ArrayRepeat,
                limit: bound,
            })
        );
    }
}

#[test]
fn aggregate_layout_rejects_offset_that_crosses_target_bound() {
    let engine = LayoutEngine::new(4);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let byte = interner.intern(ArType::Primitive(Primitive::U8));
    let almost_full = interner.intern(ArType::Array((1_u64 << 31) - 1, byte));
    let tuple = interner.intern(ArType::tuple(&[almost_full, byte], &interner));

    assert_eq!(
        engine.try_layout_of(tuple, &interner, &provider),
        Err(LayoutError::SizeOverflow {
            operation: LayoutOperation::FieldOffset,
            limit: 1_u64 << 31,
        })
    );
}

#[test]
fn test_void_error_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    for ty in [ArType::Void, ArType::Error] {
        let tid = interner.intern(ty);
        let layout = engine.layout_of(tid, &interner, &provider);
        assert_eq!(layout.size, 0);
        assert_eq!(layout.align, 1);
    }
    // `Err` is a message handle (pointer-sized), not a ZST.
    let err_tid = interner.intern(ArType::Err);
    let err_layout = engine.layout_of(err_tid, &interner, &provider);
    assert_eq!(err_layout.size, 8);
    assert_eq!(err_layout.align, 8);
}

#[test]
fn test_func_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let int_id = interner.intern(ArType::Primitive(Primitive::Int));
    let func_ty = ArType::func(&[int_id, int_id], int_id, &interner);
    let tid = interner.intern(func_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
}

#[test]
fn test_nullable_layout_is_pointer_handle() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let inner = interner.intern(ArType::Primitive(Primitive::I32));
    let nullable = ArType::Nullable(inner);
    let tid = interner.intern(nullable);
    let layout = engine.layout_of(tid, &interner, &provider);
    // Handle ABI: always pointer-sized (null vs box/object ptr).
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
}

#[test]
fn test_tuple_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let u8_id = interner.intern(ArType::Primitive(Primitive::U8));
    let i32_id = interner.intern(ArType::Primitive(Primitive::I32));
    let u8_2 = interner.intern(ArType::Primitive(Primitive::U8));
    let tuple_ty = ArType::tuple(&[u8_id, i32_id, u8_2], &interner);
    let tid = interner.intern(tuple_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    // u8 at 0, i32 at 4 (align 4), u8 at 8, total = 12 (aligned to 4)
    assert_eq!(layout.align, 4);
    assert_eq!(layout.field_offsets, vec![0, 4, 8]);
    assert_eq!(layout.size, 12);
}

#[test]
fn test_result_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let ok = interner.intern(ArType::Primitive(Primitive::I32));
    let err = interner.intern(ArType::Primitive(Primitive::U8));
    let result_ty = ArType::Result(ok, err);
    let tid = interner.intern(result_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    // tag at 0, payload at 8 (ptr_width), max payload = max(4,1) = 4, total = 12 aligned to max(4,1,8)=8 => 16
    assert_eq!(layout.field_offsets, vec![0, 8]);
    assert_eq!(layout.align, 8);
    assert_eq!(layout.size, 16);
}

#[test]
fn test_option_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let inner = interner.intern(ArType::Primitive(Primitive::I32));
    let opt_ty = ArType::Option(inner);
    let tid = interner.intern(opt_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    // tag at 0, payload at 8 (ptr_width), payload size 4, total = 12 aligned to max(4,8)=8 => 16
    assert_eq!(layout.field_offsets, vec![0, 8]);
    assert_eq!(layout.align, 8);
    assert_eq!(layout.size, 16);
}

#[test]
fn test_range_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let inner = interner.intern(ArType::Primitive(Primitive::I32));
    let range_ty = ArType::Range(inner);
    let tid = interner.intern(range_ty);
    let layout = engine.layout_of(tid, &interner, &provider);
    // start at 0, end at 4 (padding to align 4), total = 8
    assert_eq!(layout.field_offsets, vec![0, 4]);
    assert_eq!(layout.align, 4);
    assert_eq!(layout.size, 8);
}

#[test]
fn test_zst_allocator_field_adds_no_size() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();

    // Setup mock provider for ZST
    let mut provider = StructMockProvider {
        fields: FxHashMap::<SymbolId, StructFields>::default(),
        generic_params: FxHashMap::<SymbolId, Vec<SymbolId>>::default(),
        enum_variants: FxHashMap::<SymbolId, Vec<EnumPayloadShape>>::default(),
        repr_c: rustc_hash::FxHashSet::default(),
    };

    // Create ZST struct "GlobalAllocator"
    let zst_id = SymbolId::new(0, 100);
    provider.fields.insert(zst_id, StructFields::new()); // No fields = ZST

    let zst_ty = ArType::Named(zst_id, IndexRange::empty());
    let zst_tid = interner.intern(zst_ty);
    let zst_layout = engine.layout_of(zst_tid, &interner, &provider);
    assert_eq!(zst_layout.size, 0); // ZST has 0 size
    assert_eq!(zst_layout.align, 1);

    // Create Vec struct with ZST field
    let vec_id = SymbolId::new(0, 101);
    let vec_fields = StructFields::from_entries([
        StructFieldInfo {
            name: "data".into(),
            symbol: None,
            ty: interner.intern(ArType::Primitive(Primitive::I64)),
            index: 0,
        },
        StructFieldInfo {
            name: "len".into(),
            symbol: None,
            ty: interner.intern(ArType::Primitive(Primitive::U64)),
            index: 1,
        },
        StructFieldInfo {
            name: "capacity".into(),
            symbol: None,
            ty: interner.intern(ArType::Primitive(Primitive::U64)),
            index: 2,
        },
        StructFieldInfo {
            name: "allocator".into(),
            symbol: None,
            ty: interner.intern(ArType::Named(zst_id, IndexRange::empty())),
            index: 3,
        },
    ]);

    provider.fields.insert(vec_id, vec_fields);

    let vec_ty = ArType::Named(vec_id, IndexRange::empty());
    let vec_tid = interner.intern(vec_ty);
    let vec_layout = engine.layout_of(vec_tid, &interner, &provider);

    // 3 u64 fields + 1 ZST field = 3 * 8 = 24 bytes
    assert_eq!(vec_layout.size, 24);
    assert_eq!(vec_layout.align, 8);
}

#[test]
fn test_int_literal_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let tid = interner.intern(ArType::IntLiteral);
    let layout = engine.layout_of(tid, &interner, &provider);
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
}

#[test]
fn test_float_literal_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;
    let tid = interner.intern(ArType::FloatLiteral);
    let layout = engine.layout_of(tid, &interner, &provider);
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
}

#[test]
fn test_struct_layout_and_padding() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();

    let struct_sym = SymbolId::new(0, 1234);

    let fields = StructFields::from_entries([
        StructFieldInfo {
            name: "a".into(),
            symbol: None,
            ty: interner.intern(ArType::Primitive(Primitive::U8)),
            index: 0,
        },
        StructFieldInfo {
            name: "b".into(),
            symbol: None,
            ty: interner.intern(ArType::Primitive(Primitive::I32)),
            index: 1,
        },
        StructFieldInfo {
            name: "c".into(),
            symbol: None,
            ty: interner.intern(ArType::Primitive(Primitive::U8)),
            index: 2,
        },
    ]);

    let mut fields_map = FxHashMap::<SymbolId, StructFields>::default();
    fields_map.insert(struct_sym, fields);

    let mut provider = StructMockProvider {
        fields: fields_map,
        generic_params: FxHashMap::<SymbolId, Vec<SymbolId>>::default(),
        enum_variants: FxHashMap::<SymbolId, Vec<EnumPayloadShape>>::default(),
        repr_c: rustc_hash::FxHashSet::default(),
    };

    let struct_ty = ArType::Named(struct_sym, IndexRange::empty());
    let struct_id = interner.intern(struct_ty);

    // A4.0: Default Arandu struct reorders fields by descending alignment:
    // b (I32, align 4): offset 0
    // a (U8, align 1): offset 4
    // c (U8, align 1): offset 5
    // total size: 8 (reduced from 12 bytes!)
    let layout = engine.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout.align, 4);
    assert_eq!(layout.field_offsets, vec![4, 0, 5]);
    assert_eq!(layout.size, 8);

    // With repr(C), declaration order is preserved:
    // a: offset 0, b: offset 4, c: offset 8 -> size 12
    provider.repr_c.insert(struct_sym);
    let layout_c = engine.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout_c.align, 4);
    assert_eq!(layout_c.field_offsets, vec![0, 4, 8]);
    assert_eq!(layout_c.size, 12);
}

#[test]
fn test_struct_missing_fields_fallback() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let struct_sym = SymbolId::new(0, 9999);
    let struct_ty = ArType::Named(struct_sym, IndexRange::empty());
    let struct_id = interner.intern(struct_ty);
    let provider = MockProvider;
    let layout = engine.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout.size, 0);
    assert_eq!(layout.align, 1);
    assert!(layout.field_offsets.is_empty());
}

#[test]
fn test_struct_generic_substitution() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();

    let struct_sym = SymbolId::new(0, 42);
    let param_sym = SymbolId::new(0, 1);

    let param_ty = interner.intern(ArType::Named(param_sym, IndexRange::empty()));

    let fields = StructFields::from_entries([StructFieldInfo {
        name: "value".into(),
        symbol: None,
        ty: param_ty,
        index: 0,
    }]);

    let mut fields_map = FxHashMap::<SymbolId, StructFields>::default();
    fields_map.insert(struct_sym, fields);

    let mut generic_params = FxHashMap::<SymbolId, Vec<SymbolId>>::default();
    generic_params.insert(struct_sym, vec![param_sym]);

    let provider = StructMockProvider {
        fields: fields_map,
        generic_params,
        enum_variants: FxHashMap::<SymbolId, Vec<EnumPayloadShape>>::default(),
        repr_c: rustc_hash::FxHashSet::default(),
    };

    let concrete_int = interner.intern(ArType::Primitive(Primitive::I32));
    let struct_ty = ArType::named(struct_sym, &[concrete_int], &interner);
    let struct_id = interner.intern(struct_ty);

    let layout = engine.layout_of(struct_id, &interner, &provider);
    // value field substituted to I32: size 4, align 4
    assert_eq!(layout.align, 4);
    assert_eq!(layout.field_offsets, vec![0]);
    assert_eq!(layout.size, 4);
}

#[test]
fn test_target_32bit_vec_and_string_evidence() {
    let interner = TypeInterner::new();

    // Struct mimicking Vec[T] / String: { ptr, len: uint, cap: uint }
    let struct_sym = SymbolId::new(0, 100);
    let u8_tid = interner.intern(ArType::Primitive(Primitive::U8));
    let ptr_u8_tid = interner.intern(ArType::Ptr(u8_tid));
    let uint_tid = interner.intern(ArType::Primitive(Primitive::Uint));

    let fields = StructFields::from_entries([
        StructFieldInfo {
            name: "data".into(),
            symbol: None,
            ty: ptr_u8_tid,
            index: 0,
        },
        StructFieldInfo {
            name: "len".into(),
            symbol: None,
            ty: uint_tid,
            index: 1,
        },
        StructFieldInfo {
            name: "capacity".into(),
            symbol: None,
            ty: uint_tid,
            index: 2,
        },
    ]);

    let mut fields_map = FxHashMap::<SymbolId, StructFields>::default();
    fields_map.insert(struct_sym, fields);

    let provider = StructMockProvider {
        fields: fields_map,
        generic_params: FxHashMap::default(),
        enum_variants: FxHashMap::default(),
        repr_c: rustc_hash::FxHashSet::default(),
    };

    let struct_ty = ArType::Named(struct_sym, IndexRange::empty());
    let struct_id = interner.intern(struct_ty);

    // 64-bit target: ptr=8, uint=8, cap=8 -> size=24, align=8, offsets=[0, 8, 16]
    let engine_64 = LayoutEngine::new(8);
    let layout_64 = engine_64.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout_64.size, 24);
    assert_eq!(layout_64.align, 8);
    assert_eq!(layout_64.field_offsets, vec![0, 8, 16]);

    // 32-bit generic target: ptr=4, uint=4, cap=4 -> size=12, align=4, offsets=[0, 4, 8]
    let engine_32 = LayoutEngine::new(4);
    let layout_32 = engine_32.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout_32.size, 12);
    assert_eq!(layout_32.align, 4);
    assert_eq!(layout_32.field_offsets, vec![0, 4, 8]);

    // 32-bit i686 SysV target: size=12, align=4, offsets=[0, 4, 8]
    let engine_i686 = LayoutEngine::from_data_layout(DataLayout::i686_sysv());
    let layout_i686 = engine_i686.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout_i686.size, 12);
    assert_eq!(layout_i686.align, 4);
    assert_eq!(layout_i686.field_offsets, vec![0, 4, 8]);
}

#[test]
fn test_target_32bit_mixed_alignment_evidence() {
    let interner = TypeInterner::new();

    // Struct with mixed types: { a: u8, b: u64, c: u32 }
    let struct_sym = SymbolId::new(0, 101);
    let u8_tid = interner.intern(ArType::Primitive(Primitive::U8));
    let u64_tid = interner.intern(ArType::Primitive(Primitive::U64));
    let u32_tid = interner.intern(ArType::Primitive(Primitive::U32));

    let fields = StructFields::from_entries([
        StructFieldInfo {
            name: "a".into(),
            symbol: None,
            ty: u8_tid,
            index: 0,
        },
        StructFieldInfo {
            name: "b".into(),
            symbol: None,
            ty: u64_tid,
            index: 1,
        },
        StructFieldInfo {
            name: "c".into(),
            symbol: None,
            ty: u32_tid,
            index: 2,
        },
    ]);

    let mut fields_map = FxHashMap::<SymbolId, StructFields>::default();
    fields_map.insert(struct_sym, fields);

    let mut provider = StructMockProvider {
        fields: fields_map,
        generic_params: FxHashMap::default(),
        enum_variants: FxHashMap::default(),
        repr_c: rustc_hash::FxHashSet::default(),
    };

    let struct_ty = ArType::Named(struct_sym, IndexRange::empty());
    let struct_id = interner.intern(struct_ty);

    // In 64-bit with default Arandu reordering (descending alignment):
    // b (align 8, size 8) -> offset 0
    // c (align 4, size 4) -> offset 8
    // a (align 1, size 1) -> offset 12
    // total size = align_up(13, 8) = 16 (padding eliminated from 24 down to 16!)
    // field_offsets indexed by orig_index: a@12, b@0, c@8 -> [12, 0, 8]
    let engine_64 = LayoutEngine::new(8);
    let layout_64 = engine_64.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout_64.size, 16);
    assert_eq!(layout_64.align, 8);
    assert_eq!(layout_64.field_offsets, vec![12, 0, 8]);

    // In 64-bit with repr(C): a@0, pad 7, b@8, c@16, pad 4 -> size=24, align=8
    provider.repr_c.insert(struct_sym);
    let layout_64_c = engine_64.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout_64_c.size, 24);
    assert_eq!(layout_64_c.align, 8);
    assert_eq!(layout_64_c.field_offsets, vec![0, 8, 16]);
    provider.repr_c.remove(&struct_sym);

    // In i686 SysV (where u64 align is 4!):
    // b: align 4, size 8 -> offset 0
    // c: align 4, size 4 -> offset 8
    // a: align 1, size 1 -> offset 12
    // size = align_up(13, 4) = 16, align 4, field_offsets: [12, 0, 8]
    let engine_i686 = LayoutEngine::from_data_layout(DataLayout::i686_sysv());
    let layout_i686 = engine_i686.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout_i686.size, 16);
    assert_eq!(layout_i686.align, 4);
    assert_eq!(layout_i686.field_offsets, vec![12, 0, 8]);

    // In i686 SysV with repr(C): a@0, pad 3, b@4, c@12 -> size=16, align=4, field_offsets: [0, 4, 12]
    provider.repr_c.insert(struct_sym);
    let layout_i686_c = engine_i686.layout_of(struct_id, &interner, &provider);
    assert_eq!(layout_i686_c.size, 16);
    assert_eq!(layout_i686_c.align, 4);
    assert_eq!(layout_i686_c.field_offsets, vec![0, 4, 12]);
}

#[test]
fn test_enum_layouts_32bit_and_64bit() {
    let interner = TypeInterner::new();
    let enum_sym = SymbolId::new(0, 200);

    let i32_tid = interner.intern(ArType::Primitive(Primitive::I32));
    let i64_tid = interner.intern(ArType::Primitive(Primitive::I64));

    // Enum with 2 variants:
    // Variant 0: None (unit)
    // Variant 1: Some(i32)
    let variants_i32 = vec![
        EnumPayloadShape { payload_ty: None },
        EnumPayloadShape {
            payload_ty: Some(i32_tid),
        },
    ];

    let mut enum_map = FxHashMap::<SymbolId, Vec<EnumPayloadShape>>::default();
    enum_map.insert(enum_sym, variants_i32);

    let provider = StructMockProvider {
        fields: FxHashMap::default(),
        generic_params: FxHashMap::default(),
        enum_variants: enum_map,
        repr_c: rustc_hash::FxHashSet::default(),
    };

    let enum_ty = ArType::Named(enum_sym, IndexRange::empty());
    let enum_id = interner.intern(enum_ty);

    // 64-bit target: tag_size=8, payload i32 (size 4, align 4)
    // max_align = max(4, 8) = 8.
    // payload_end = checked_add(8, 4) = 12.
    // align_up(12, 8) = 16.
    let engine_64 = LayoutEngine::new(8);
    let layout_64 = engine_64.layout_of(enum_id, &interner, &provider);
    assert_eq!(layout_64.size, 16);
    assert_eq!(layout_64.align, 8);
    assert_eq!(layout_64.field_offsets, vec![0, 8]);

    // 32-bit generic target: tag_size=4, payload i32 (size 4, align 4)
    // max_align = max(4, 4) = 4.
    // payload_end = checked_add(4, 4) = 8.
    // align_up(8, 4) = 8.
    let engine_32 = LayoutEngine::new(4);
    let layout_32 = engine_32.layout_of(enum_id, &interner, &provider);
    assert_eq!(layout_32.size, 8);
    assert_eq!(layout_32.align, 4);
    assert_eq!(layout_32.field_offsets, vec![0, 4]);

    // Now test with i64 payload:
    let enum_sym_i64 = SymbolId::new(0, 201);
    let variants_i64 = vec![
        EnumPayloadShape { payload_ty: None },
        EnumPayloadShape {
            payload_ty: Some(i64_tid),
        },
    ];
    let mut enum_map_i64 = FxHashMap::<SymbolId, Vec<EnumPayloadShape>>::default();
    enum_map_i64.insert(enum_sym_i64, variants_i64);
    let provider_i64 = StructMockProvider {
        fields: FxHashMap::default(),
        generic_params: FxHashMap::default(),
        enum_variants: enum_map_i64,
        repr_c: rustc_hash::FxHashSet::default(),
    };
    let enum_id_i64 = interner.intern(ArType::Named(enum_sym_i64, IndexRange::empty()));

    // 32-bit i686 SysV target: tag_size=4, payload i64 (size 8, align 4 on i686!)
    // max_align = max(4, 4) = 4.
    // payload_end = checked_add(4, 8) = 12.
    // align_up(12, 4) = 12.
    let engine_i686 = LayoutEngine::from_data_layout(DataLayout::i686_sysv());
    let layout_i686 = engine_i686.layout_of(enum_id_i64, &interner, &provider_i64);
    assert_eq!(layout_i686.size, 12);
    assert_eq!(layout_i686.align, 4);
    assert_eq!(layout_i686.field_offsets, vec![0, 4]);

    // 32-bit standard ILP32 (DataLayout::ptr_width(4)): i64 has natural align 8
    // max_align = max(8, 4) = 8.
    // payload_end = checked_add(4, 8) = 12.
    // align_up(12, 8) = 16.
    let layout_ilp32 = engine_32.layout_of(enum_id_i64, &interner, &provider_i64);
    assert_eq!(layout_ilp32.size, 16);
    assert_eq!(layout_ilp32.align, 8);
    assert_eq!(layout_ilp32.field_offsets, vec![0, 4]);
}

#[test]
fn test_niche_layout_option_ref_vs_val() {
    let engine_64 = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = StructMockProvider::default();

    let i32_tid = interner.intern(ArType::Primitive(Primitive::I32));
    let ref_i32_tid = interner.intern(ArType::Ref(i32_tid));

    // Option[ref i32]: has null niche!
    let opt_ref_tid = interner.intern(ArType::Option(ref_i32_tid));
    let layout_ref = engine_64.layout_of(opt_ref_tid, &interner, &provider);
    assert_eq!(layout_ref.size, 8);
    assert_eq!(layout_ref.align, 8);
    assert_eq!(layout_ref.field_offsets, vec![0, 0]);
    assert_eq!(
        layout_ref.tag_encoding,
        Some(TagEncoding::Niche {
            niche_offset: 0,
            niche_size: 8,
            niche_value: 0,
            untagged_variant: 1,
            tagged_variant: 0,
        })
    );

    // Option[i32]: no niche! Direct tag (tag_size=8, payload_offset=8)
    let opt_i32_tid = interner.intern(ArType::Option(i32_tid));
    let layout_val = engine_64.layout_of(opt_i32_tid, &interner, &provider);
    assert_eq!(layout_val.size, 16);
    assert_eq!(layout_val.align, 8);
    assert_eq!(layout_val.field_offsets, vec![0, 8]);
    assert_eq!(
        layout_val.tag_encoding,
        Some(TagEncoding::Direct {
            tag_size: 8,
            payload_offset: 8,
        })
    );

    // Single-field struct wrapping ref i32: struct Wrapper { r: ref i32 }
    let wrapper_sym = SymbolId::new(0, 300);
    let wrapper_fields = StructFields::from_entries([StructFieldInfo {
        name: "r".into(),
        symbol: None,
        ty: ref_i32_tid,
        index: 0,
    }]);
    let mut fields_map = FxHashMap::<SymbolId, StructFields>::default();
    fields_map.insert(wrapper_sym, wrapper_fields);
    let wrapper_provider = StructMockProvider {
        fields: fields_map,
        generic_params: FxHashMap::default(),
        enum_variants: FxHashMap::default(),
        repr_c: rustc_hash::FxHashSet::default(),
    };
    let wrapper_tid = interner.intern(ArType::Named(wrapper_sym, IndexRange::empty()));
    let opt_wrapper_tid = interner.intern(ArType::Option(wrapper_tid));
    let layout_wrapper = engine_64.layout_of(opt_wrapper_tid, &interner, &wrapper_provider);
    assert_eq!(layout_wrapper.size, 8);
    assert_eq!(layout_wrapper.align, 8);
    assert_eq!(layout_wrapper.field_offsets, vec![0, 0]);
    assert_eq!(
        layout_wrapper.tag_encoding,
        Some(TagEncoding::Niche {
            niche_offset: 0,
            niche_size: 8,
            niche_value: 0,
            untagged_variant: 1,
            tagged_variant: 0,
        })
    );
}

#[test]
fn test_pointer_tagging_8_byte_align() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();

    // Struct with align 8: struct Node8 { a: i64 }
    let node_sym = SymbolId::new(0, 500);
    let i64_tid = interner.intern(ArType::Primitive(Primitive::I64));
    let node_fields = StructFields::from_entries([StructFieldInfo {
        name: "a".into(),
        symbol: None,
        ty: i64_tid,
        index: 0,
    }]);
    let mut fields_map = FxHashMap::default();
    fields_map.insert(node_sym, node_fields);

    let node_tid = interner.intern(ArType::Named(node_sym, IndexRange::empty()));
    let ref_node_tid = interner.intern(ArType::Ref(node_tid));

    // Enum Tree { Nil, Leaf(ref Node8), Branch(ref Node8) } -> 3 variants <= 2^3 = 8
    let enum_sym = SymbolId::new(0, 501);
    let variants = vec![
        EnumPayloadShape { payload_ty: None },
        EnumPayloadShape {
            payload_ty: Some(ref_node_tid),
        },
        EnumPayloadShape {
            payload_ty: Some(ref_node_tid),
        },
    ];
    let mut enum_map = FxHashMap::default();
    enum_map.insert(enum_sym, variants);

    let provider = StructMockProvider {
        fields: fields_map,
        generic_params: FxHashMap::default(),
        enum_variants: enum_map,
        repr_c: rustc_hash::FxHashSet::default(),
    };

    let enum_tid = interner.intern(ArType::Named(enum_sym, IndexRange::empty()));
    let layout = engine.layout_of(enum_tid, &interner, &provider);

    // A4.2: Pointer tagging reduces size from 16 to 8 bytes!
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
    assert_eq!(layout.field_offsets, vec![0]);
    assert_eq!(
        layout.tag_encoding,
        Some(TagEncoding::PointerTag {
            tag_bits: 3,
            tag_mask: 7,
            pointer_offset: 0,
        })
    );
}

#[test]
fn test_pointer_tagging_4_byte_align() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();

    // Struct with align 4: struct Node4 { a: i32 }
    let node_sym = SymbolId::new(0, 510);
    let i32_tid = interner.intern(ArType::Primitive(Primitive::I32));
    let node_fields = StructFields::from_entries([StructFieldInfo {
        name: "a".into(),
        symbol: None,
        ty: i32_tid,
        index: 0,
    }]);
    let mut fields_map = FxHashMap::default();
    fields_map.insert(node_sym, node_fields);

    let node_tid = interner.intern(ArType::Named(node_sym, IndexRange::empty()));
    let ref_node_tid = interner.intern(ArType::Ref(node_tid));

    // Enum Quad { V0, V1(ref Node4), V2(ref Node4), V3 } -> 4 variants <= 2^2 = 4
    let enum_sym = SymbolId::new(0, 511);
    let variants = vec![
        EnumPayloadShape { payload_ty: None },
        EnumPayloadShape {
            payload_ty: Some(ref_node_tid),
        },
        EnumPayloadShape {
            payload_ty: Some(ref_node_tid),
        },
        EnumPayloadShape { payload_ty: None },
    ];
    let mut enum_map = FxHashMap::default();
    enum_map.insert(enum_sym, variants);

    let provider = StructMockProvider {
        fields: fields_map,
        generic_params: FxHashMap::default(),
        enum_variants: enum_map,
        repr_c: rustc_hash::FxHashSet::default(),
    };

    let enum_tid = interner.intern(ArType::Named(enum_sym, IndexRange::empty()));
    let layout = engine.layout_of(enum_tid, &interner, &provider);

    // A4.2: Pointer tagging with 2 bits (mask 3) fits 4 variants in 8 bytes:
    assert_eq!(layout.size, 8);
    assert_eq!(layout.align, 8);
    assert_eq!(layout.field_offsets, vec![0]);
    assert_eq!(
        layout.tag_encoding,
        Some(TagEncoding::PointerTag {
            tag_bits: 2,
            tag_mask: 3,
            pointer_offset: 0,
        })
    );
}

#[test]
fn test_pointer_tagging_fallback_when_variants_exceed_capacity() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();

    // Struct with align 4: struct Node4 { a: i32 } -> max 4 variants for pointer tagging
    let node_sym = SymbolId::new(0, 520);
    let i32_tid = interner.intern(ArType::Primitive(Primitive::I32));
    let node_fields = StructFields::from_entries([StructFieldInfo {
        name: "a".into(),
        symbol: None,
        ty: i32_tid,
        index: 0,
    }]);
    let mut fields_map = FxHashMap::default();
    fields_map.insert(node_sym, node_fields);

    let node_tid = interner.intern(ArType::Named(node_sym, IndexRange::empty()));
    let ref_node_tid = interner.intern(ArType::Ref(node_tid));

    // 5 variants > 4 -> must fall back to Direct encoding (16 bytes)
    let enum_sym = SymbolId::new(0, 521);
    let variants = vec![
        EnumPayloadShape { payload_ty: None },
        EnumPayloadShape {
            payload_ty: Some(ref_node_tid),
        },
        EnumPayloadShape {
            payload_ty: Some(ref_node_tid),
        },
        EnumPayloadShape { payload_ty: None },
        EnumPayloadShape { payload_ty: None },
    ];
    let mut enum_map = FxHashMap::default();
    enum_map.insert(enum_sym, variants);

    let provider = StructMockProvider {
        fields: fields_map,
        generic_params: FxHashMap::default(),
        enum_variants: enum_map,
        repr_c: rustc_hash::FxHashSet::default(),
    };

    let enum_tid = interner.intern(ArType::Named(enum_sym, IndexRange::empty()));
    let layout = engine.layout_of(enum_tid, &interner, &provider);

    assert_eq!(layout.size, 16);
    assert_eq!(layout.align, 8);
    assert_eq!(
        layout.tag_encoding,
        Some(TagEncoding::Direct {
            tag_size: 8,
            payload_offset: 8,
        })
    );
}

#[test]
fn test_small_object_optimization_soo_layout() {
    let engine = LayoutEngine::new(8);
    let interner = TypeInterner::new();
    let provider = MockProvider;

    // Primitives
    let u8_ty = ArType::Primitive(Primitive::U8);
    let soo_u8 = engine
        .soo_layout_for_type(&u8_ty, &interner, &provider)
        .unwrap();
    assert_eq!(soo_u8.max_inline_bytes, 24);
    assert_eq!(soo_u8.inline_capacity, 24);
    assert!(soo_u8.is_inline_eligible);

    let i16_ty = ArType::Primitive(Primitive::I16);
    let soo_i16 = engine
        .soo_layout_for_type(&i16_ty, &interner, &provider)
        .unwrap();
    assert_eq!(soo_i16.inline_capacity, 12);
    assert!(soo_i16.is_inline_eligible);

    let i32_ty = ArType::Primitive(Primitive::I32);
    let soo_i32 = engine
        .soo_layout_for_type(&i32_ty, &interner, &provider)
        .unwrap();
    assert_eq!(soo_i32.inline_capacity, 6);
    assert!(soo_i32.is_inline_eligible);

    let i64_ty = ArType::Primitive(Primitive::I64);
    let soo_i64 = engine
        .soo_layout_for_type(&i64_ty, &interner, &provider)
        .unwrap();
    assert_eq!(soo_i64.inline_capacity, 3);
    assert!(soo_i64.is_inline_eligible);

    // Struct of 24 bytes (3x i64)
    let s24_sym = SymbolId::new(0, 600);
    let i64_tid = interner.intern(i64_ty);
    let s24_fields = StructFields::from_entries([
        StructFieldInfo {
            name: "a".into(),
            symbol: None,
            ty: i64_tid,
            index: 0,
        },
        StructFieldInfo {
            name: "b".into(),
            symbol: None,
            ty: i64_tid,
            index: 1,
        },
        StructFieldInfo {
            name: "c".into(),
            symbol: None,
            ty: i64_tid,
            index: 2,
        },
    ]);
    let mut fields_map = FxHashMap::default();
    fields_map.insert(s24_sym, s24_fields);

    // Struct of 32 bytes (4x i64)
    let s32_sym = SymbolId::new(0, 601);
    let s32_fields = StructFields::from_entries([
        StructFieldInfo {
            name: "a".into(),
            symbol: None,
            ty: i64_tid,
            index: 0,
        },
        StructFieldInfo {
            name: "b".into(),
            symbol: None,
            ty: i64_tid,
            index: 1,
        },
        StructFieldInfo {
            name: "c".into(),
            symbol: None,
            ty: i64_tid,
            index: 2,
        },
        StructFieldInfo {
            name: "d".into(),
            symbol: None,
            ty: i64_tid,
            index: 3,
        },
    ]);
    fields_map.insert(s32_sym, s32_fields);

    let struct_provider = StructMockProvider {
        fields: fields_map,
        generic_params: FxHashMap::default(),
        enum_variants: FxHashMap::default(),
        repr_c: rustc_hash::FxHashSet::default(),
    };

    let s24_ty = ArType::Named(s24_sym, IndexRange::empty());
    let soo_s24 = engine
        .soo_layout_for_type(&s24_ty, &interner, &struct_provider)
        .unwrap();
    assert_eq!(soo_s24.inline_capacity, 1);
    assert!(soo_s24.is_inline_eligible);
    assert!(engine.is_soo_eligible(&s24_ty, &interner, &struct_provider));

    let s32_ty = ArType::Named(s32_sym, IndexRange::empty());
    let soo_s32 = engine
        .soo_layout_for_type(&s32_ty, &interner, &struct_provider)
        .unwrap();
    assert_eq!(soo_s32.inline_capacity, 0);
    assert!(!soo_s32.is_inline_eligible);
    assert!(!engine.is_soo_eligible(&s32_ty, &interner, &struct_provider));
}
