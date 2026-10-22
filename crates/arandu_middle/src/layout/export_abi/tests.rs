use rustc_hash::FxHashMap;
use smol_str::SmolStr;

use super::*;
use crate::SymbolId;
use crate::hir::pool::IndexRange;
use crate::layout::export_abi::{AbiLane, AbiPart, ExportAbi, ParamAbi, ReturnAbi, classify};
use crate::layout::{EnumPayloadShape, StructFieldInfo, StructFields};

/// Expected shapes below were produced by `clang -S -emit-llvm -O0` (clang 23)
/// for the equivalent C struct. Keep the probe in the test so the rule stays
/// auditable instead of becoming folklore.
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
    fields: &[(&str, ArType)],
) -> ArType {
    let symbol = SymbolId::new(0, id);
    let entries = fields
        .iter()
        .enumerate()
        .map(|(index, (name, ty))| StructFieldInfo {
            name: SmolStr::new(*name),
            symbol: Some(SymbolId::new(0, 5000 + index as u32)),
            ty: interner.intern(ty.clone()),
            index,
        })
        .collect::<Vec<_>>();
    provider
        .fields
        .insert(symbol, StructFields::from_entries(entries));
    provider.repr_c.insert(symbol);
    ArType::Named(symbol, IndexRange::empty())
}

fn slice_of(interner: &TypeInterner, primitive: Primitive) -> ArType {
    let element = interner.intern(ArType::Primitive(primitive));
    ArType::Slice(element)
}

fn lane(part: AbiPart, offset: u64) -> AbiLane {
    AbiLane { part, offset }
}

fn classify_with(
    abi: ExportAbi,
    ty: &ArType,
    interner: &TypeInterner,
    provider: &MockProvider,
    pointer_width: u64,
) -> ExportAbiInfo {
    classify(abi, ty, interner, provider, pointer_width).expect("classification")
}

#[test]
fn sysv_matches_clang_for_small_structs() {
    let interner = TypeInterner::new();
    let mut provider = MockProvider::default();
    let u32_ty = ArType::Primitive(Primitive::U32);
    let u8_ty = ArType::Primitive(Primitive::U8);
    let f32_ty = ArType::Primitive(Primitive::F32);

    // struct { unsigned a; unsigned char b; };  -> i64
    let mixed = register(
        &mut provider,
        &interner,
        1,
        &[("a", u32_ty.clone()), ("b", u8_ty.clone())],
    );
    let info = classify_with(ExportAbi::SysVAmd64, &mixed, &interner, &provider, 8);
    assert_eq!(
        info.param,
        ParamAbi::Direct(vec![lane(AbiPart::Int(64), 0)])
    );
    assert_eq!(info.ret, ReturnAbi::Direct(vec![lane(AbiPart::Int(64), 0)]));

    // struct { unsigned a, b, c; }; -> { i64, i32 }
    let three = register(
        &mut provider,
        &interner,
        2,
        &[
            ("a", u32_ty.clone()),
            ("b", u32_ty.clone()),
            ("c", u32_ty.clone()),
        ],
    );
    let info = classify_with(ExportAbi::SysVAmd64, &three, &interner, &provider, 8);
    let expected = vec![lane(AbiPart::Int(64), 0), lane(AbiPart::Int(32), 8)];
    assert_eq!(info.param, ParamAbi::Direct(expected.clone()));
    assert_eq!(info.ret, ReturnAbi::Direct(expected));

    // struct { float a, b; }; -> <2 x float>
    let pair = register(
        &mut provider,
        &interner,
        3,
        &[("a", f32_ty.clone()), ("b", f32_ty)],
    );
    let info = classify_with(ExportAbi::SysVAmd64, &pair, &interner, &provider, 8);
    assert_eq!(
        info.param,
        ParamAbi::Direct(vec![lane(AbiPart::FloatPair, 0)])
    );

    // struct { unsigned a; float b; }; -> i64 (eightbyte merged to INTEGER)
    let merged = register(
        &mut provider,
        &interner,
        4,
        &[("a", u32_ty), ("b", ArType::Primitive(Primitive::F32))],
    );
    let info = classify_with(ExportAbi::SysVAmd64, &merged, &interner, &provider, 8);
    assert_eq!(
        info.param,
        ParamAbi::Direct(vec![lane(AbiPart::Int(64), 0)])
    );
}

#[test]
fn sysv_keeps_exact_bit_widths_and_indirection_at_sixteen_bytes() {
    let interner = TypeInterner::new();
    let provider = MockProvider::default();
    let bytes = interner.intern(ArType::Primitive(Primitive::U8));
    let five = ArType::Array(5, bytes);
    let info = classify_with(ExportAbi::SysVAmd64, &five, &interner, &provider, 8);
    assert_eq!(
        info.param,
        ParamAbi::Direct(vec![lane(AbiPart::Int(40), 0)])
    );

    let wide = interner.intern(ArType::Primitive(Primitive::U32));
    let big = ArType::Array(8, wide);
    let info = classify_with(ExportAbi::SysVAmd64, &big, &interner, &provider, 8);
    assert_eq!(info.param, ParamAbi::Indirect);
    assert_eq!(info.ret, ReturnAbi::Sret);
}

#[test]
fn slices_are_a_two_word_struct_everywhere() {
    let interner = TypeInterner::new();
    let provider = MockProvider::default();
    let slice = slice_of(&interner, Primitive::U32);

    let sysv = classify_with(ExportAbi::SysVAmd64, &slice, &interner, &provider, 8);
    let expected = vec![lane(AbiPart::Int(64), 0), lane(AbiPart::Int(64), 8)];
    assert_eq!(sysv.param, ParamAbi::Direct(expected.clone()));
    assert_eq!(sysv.ret, ReturnAbi::Direct(expected));

    let arm = classify_with(
        ExportAbi::Aapcs32 { hard_float: false },
        &slice,
        &interner,
        &provider,
        4,
    );
    assert_eq!(
        arm.param,
        ParamAbi::Direct(vec![lane(AbiPart::IntWords { word: 32, count: 2 }, 0)])
    );
    // clang: struct { unsigned *p; unsigned len; } returns through sret.
    assert_eq!(arm.ret, ReturnAbi::Sret);

    let riscv = classify_with(ExportAbi::RiscvIlp32, &slice, &interner, &provider, 4);
    assert_eq!(
        riscv.param,
        ParamAbi::Direct(vec![lane(AbiPart::IntWords { word: 32, count: 2 }, 0)])
    );
    assert_eq!(
        riscv.ret,
        ReturnAbi::Direct(vec![lane(AbiPart::IntWords { word: 32, count: 2 }, 0)])
    );

    let windows = classify_with(ExportAbi::WindowsX64, &slice, &interner, &provider, 8);
    assert_eq!(windows.param, ParamAbi::Indirect);
    assert_eq!(windows.ret, ReturnAbi::Sret);

    let aarch64 = classify_with(ExportAbi::Aapcs64, &slice, &interner, &provider, 8);
    assert_eq!(
        aarch64.param,
        ParamAbi::Direct(vec![lane(AbiPart::IntWords { word: 64, count: 2 }, 0)])
    );
}

#[test]
fn aapcs32_splits_composites_into_words_and_returns_through_sret() {
    let interner = TypeInterner::new();
    let mut provider = MockProvider::default();
    let u32_ty = ArType::Primitive(Primitive::U32);
    let u8_ty = ArType::Primitive(Primitive::U8);
    let u16_ty = ArType::Primitive(Primitive::U16);

    // clang: struct { unsigned short a, b; } -> i32 return, [1 x i32] param.
    let half = register(
        &mut provider,
        &interner,
        10,
        &[("a", u16_ty.clone()), ("b", u16_ty)],
    );
    let info = classify_with(
        ExportAbi::Aapcs32 { hard_float: false },
        &half,
        &interner,
        &provider,
        4,
    );
    assert_eq!(
        info.param,
        ParamAbi::Direct(vec![lane(AbiPart::IntWords { word: 32, count: 1 }, 0)])
    );
    assert_eq!(info.ret, ReturnAbi::Direct(vec![lane(AbiPart::Int(32), 0)]));

    // clang: struct { unsigned char a; } -> i8 return.
    let byte = register(&mut provider, &interner, 11, &[("a", u8_ty.clone())]);
    let info = classify_with(
        ExportAbi::Aapcs32 { hard_float: false },
        &byte,
        &interner,
        &provider,
        4,
    );
    assert_eq!(info.ret, ReturnAbi::Direct(vec![lane(AbiPart::Int(8), 0)]));

    // clang: struct { unsigned a; unsigned char b; } -> [2 x i32] + sret.
    let header = register(
        &mut provider,
        &interner,
        12,
        &[("a", u32_ty.clone()), ("b", u8_ty)],
    );
    let info = classify_with(
        ExportAbi::Aapcs32 { hard_float: false },
        &header,
        &interner,
        &provider,
        4,
    );
    assert_eq!(
        info.param,
        ParamAbi::Direct(vec![lane(AbiPart::IntWords { word: 32, count: 2 }, 0)])
    );
    assert_eq!(info.ret, ReturnAbi::Sret);

    // clang: struct { unsigned long long a; } -> [1 x i64].
    let wide = register(
        &mut provider,
        &interner,
        13,
        &[("a", ArType::Primitive(Primitive::U64))],
    );
    let info = classify_with(
        ExportAbi::Aapcs32 { hard_float: false },
        &wide,
        &interner,
        &provider,
        4,
    );
    assert_eq!(
        info.param,
        ParamAbi::Direct(vec![lane(AbiPart::IntWords { word: 64, count: 1 }, 0)])
    );
}

#[test]
fn hard_float_arm_passes_homogeneous_floats_by_value() {
    let interner = TypeInterner::new();
    let mut provider = MockProvider::default();
    let f32_ty = ArType::Primitive(Primitive::F32);
    let pair = register(
        &mut provider,
        &interner,
        20,
        &[("a", f32_ty.clone()), ("b", f32_ty)],
    );
    let info = classify_with(
        ExportAbi::Aapcs32 { hard_float: true },
        &pair,
        &interner,
        &provider,
        4,
    );
    let expected = vec![lane(
        AbiPart::FloatWords {
            float: 32,
            count: 2,
        },
        0,
    )];
    assert_eq!(info.param, ParamAbi::Direct(expected.clone()));
    assert_eq!(info.ret, ReturnAbi::Direct(expected));

    // Soft float keeps the same struct in two integer words.
    let soft = classify_with(
        ExportAbi::Aapcs32 { hard_float: false },
        &pair,
        &interner,
        &provider,
        4,
    );
    assert_eq!(
        soft.param,
        ParamAbi::Direct(vec![lane(AbiPart::IntWords { word: 32, count: 2 }, 0)])
    );
}

#[test]
fn aarch64_and_riscv_follow_their_word_rules() {
    let interner = TypeInterner::new();
    let mut provider = MockProvider::default();
    let u32_ty = ArType::Primitive(Primitive::U32);

    let pair = register(
        &mut provider,
        &interner,
        30,
        &[("a", u32_ty.clone()), ("b", u32_ty.clone())],
    );
    // clang aarch64: i64; clang riscv32: [2 x i32].
    let arm = classify_with(ExportAbi::Aapcs64, &pair, &interner, &provider, 8);
    assert_eq!(arm.param, ParamAbi::Direct(vec![lane(AbiPart::Int(64), 0)]));
    let riscv = classify_with(ExportAbi::RiscvIlp32, &pair, &interner, &provider, 4);
    assert_eq!(
        riscv.param,
        ParamAbi::Direct(vec![lane(AbiPart::IntWords { word: 32, count: 2 }, 0)])
    );

    let quad = register(
        &mut provider,
        &interner,
        31,
        &[
            ("a", u32_ty.clone()),
            ("b", u32_ty.clone()),
            ("c", u32_ty.clone()),
            ("d", u32_ty),
        ],
    );
    // clang aarch64: [2 x i64] param and return.
    let info = classify_with(ExportAbi::Aapcs64, &quad, &interner, &provider, 8);
    let expected = vec![lane(AbiPart::IntWords { word: 64, count: 2 }, 0)];
    assert_eq!(info.param, ParamAbi::Direct(expected.clone()));
    assert_eq!(info.ret, ReturnAbi::Direct(expected));

    // clang riscv32: 16-byte structs pass by reference and return through sret.
    let info = classify_with(ExportAbi::RiscvIlp32, &quad, &interner, &provider, 4);
    assert_eq!(info.param, ParamAbi::Indirect);
    assert_eq!(info.ret, ReturnAbi::Sret);

    let wide = register(
        &mut provider,
        &interner,
        32,
        &[("a", ArType::Primitive(Primitive::U64))],
    );
    let info = classify_with(ExportAbi::RiscvIlp32, &wide, &interner, &provider, 4);
    assert_eq!(
        info.param,
        ParamAbi::Direct(vec![lane(AbiPart::Int(64), 0)])
    );
}

#[test]
fn windows_x64_passes_only_one_two_four_or_eight_byte_aggregates() {
    let interner = TypeInterner::new();
    let mut provider = MockProvider::default();
    let u32_ty = ArType::Primitive(Primitive::U32);
    let pair = register(
        &mut provider,
        &interner,
        40,
        &[("a", u32_ty.clone()), ("b", u32_ty.clone())],
    );
    let info = classify_with(ExportAbi::WindowsX64, &pair, &interner, &provider, 8);
    assert_eq!(
        info.param,
        ParamAbi::Direct(vec![lane(AbiPart::Int(64), 0)])
    );

    let triple = register(
        &mut provider,
        &interner,
        41,
        &[("a", u32_ty.clone()), ("b", u32_ty.clone()), ("c", u32_ty)],
    );
    let info = classify_with(ExportAbi::WindowsX64, &triple, &interner, &provider, 8);
    assert_eq!(info.param, ParamAbi::Indirect);
    assert_eq!(info.ret, ReturnAbi::Sret);
}

#[test]
fn target_identity_maps_to_the_right_convention() {
    assert_eq!(
        ExportAbi::for_target("x86_64", "windows", "x86_64-pc-windows-msvc"),
        Some(ExportAbi::WindowsX64)
    );
    assert_eq!(
        ExportAbi::for_target("x86_64", "linux", "x86_64-unknown-linux-gnu"),
        Some(ExportAbi::SysVAmd64)
    );
    assert_eq!(
        ExportAbi::for_target("aarch64", "macos", "aarch64-apple-darwin"),
        Some(ExportAbi::Aapcs64)
    );
    assert_eq!(
        ExportAbi::for_target("thumbv6m", "none", "thumbv6m-none-eabi"),
        Some(ExportAbi::Aapcs32 { hard_float: false })
    );
    assert_eq!(
        ExportAbi::for_target("thumbv7em", "none", "thumbv7em-none-eabihf"),
        Some(ExportAbi::Aapcs32 { hard_float: true })
    );
    assert_eq!(
        ExportAbi::for_target("riscv32imc", "none", "riscv32imc-unknown-none-elf"),
        Some(ExportAbi::RiscvIlp32)
    );
    assert_eq!(ExportAbi::for_target("mips", "none", "mips-none-elf"), None);
}
