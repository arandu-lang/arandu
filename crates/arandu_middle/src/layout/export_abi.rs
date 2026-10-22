//! C ABI shapes for `@Export`: the callee side of the classification in
//! [`super::abi`].
//!
//! The caller-side classifier answers "which register slots carry this value".
//! An exported definition needs the inverse answer in LLVM terms: the exact
//! parameter and return *types* the C compiler will use, plus the byte offsets
//! needed to rebuild the aggregate in memory for the Arandu body.
//!
//! The rules below are derived from `clang -S -emit-llvm` for each target; the
//! unit tests record the probe that produced each expectation. Emitting a
//! shape that merely "looks right" is how C++ `extern "C"` silently corrupts
//! aggregate boundaries, so anything not proven here stays rejected by
//! `T049`.

use crate::SymbolId;
use crate::layout::{LayoutEngine, StructLayoutProvider, TypeLayout};
use crate::types::{ArType, Primitive, TypeId, TypeInterner};

/// C ABI family of the target the object is emitted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportAbi {
    /// System V AMD64 (Linux, macOS, BSD on x86_64).
    SysVAmd64,
    /// Microsoft x64 (Windows on x86_64).
    WindowsX64,
    /// AAPCS64 (AArch64).
    Aapcs64,
    /// AAPCS32 (Cortex-M and other 32-bit ARM targets).
    Aapcs32 { hard_float: bool },
    /// RISC-V ILP32.
    RiscvIlp32,
}

impl ExportAbi {
    /// Maps a target identity to the convention that C compilers use there.
    ///
    /// Returns `None` for targets whose aggregate rules are not implemented;
    /// callers must reject exported aggregates instead of guessing.
    #[must_use]
    pub fn for_target(arch: &str, os: &str, triple: &str) -> Option<Self> {
        match arch {
            "x86_64" if os == "windows" => Some(Self::WindowsX64),
            "x86_64" => Some(Self::SysVAmd64),
            "aarch64" => Some(Self::Aapcs64),
            arch if arch.starts_with("thumb") || arch.starts_with("arm") => Some(Self::Aapcs32 {
                hard_float: triple.ends_with("eabihf"),
            }),
            arch if arch.starts_with("riscv32") => Some(Self::RiscvIlp32),
            _ => None,
        }
    }
}

/// One LLVM first-class value that carries part of an aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiPart {
    /// Integer of this bit width: `i8`, `i16`, `i24`, `i32`, `i64`.
    Int(u16),
    /// IEEE float of this bit width.
    Float(u16),
    /// `SysV` eightbyte holding two `f32`: `<2 x float>`.
    FloatPair,
    /// One LLVM array value: `[count x i{word}]`.
    IntWords { word: u16, count: u32 },
    /// Homogeneous floating aggregate passed in vector registers.
    FloatWords { float: u16, count: u32 },
}

impl AbiPart {
    /// Bytes this part occupies in memory (excluding ABI padding).
    #[must_use]
    pub fn byte_len(self) -> u64 {
        match self {
            Self::Int(bits) | Self::Float(bits) => u64::from(bits).div_ceil(8),
            Self::FloatPair => 8,
            Self::IntWords { word, count } | Self::FloatWords { float: word, count } => {
                u64::from(word).div_ceil(8) * u64::from(count)
            }
        }
    }
}

/// One part plus the byte offset where it starts inside the aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiLane {
    pub part: AbiPart,
    pub offset: u64,
}

/// How a C caller passes this argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamAbi {
    /// Zero-sized: the parameter does not exist at the ABI level.
    Ignored,
    /// One LLVM parameter per lane.
    Direct(Vec<AbiLane>),
    /// Pointer to a caller-owned copy (`byval` on System V).
    Indirect,
}

/// How a C caller receives this return value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnAbi {
    /// No value.
    Void,
    /// One LLVM return value per lane.
    Direct(Vec<AbiLane>),
    /// Hidden first pointer parameter.
    Sret,
}

/// ABI shape and memory layout of one exported aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportAbiInfo {
    pub size: u64,
    pub align: u64,
    pub param: ParamAbi,
    pub ret: ReturnAbi,
}

/// Types whose boundary representation must be proven by this module.
#[must_use]
pub fn is_export_aggregate(ty: &ArType) -> bool {
    matches!(
        ty,
        ArType::Named(..) | ArType::Slice(_) | ArType::MutSlice(_)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeafKind {
    Int,
    F32,
    F64,
}

#[derive(Debug, Clone, Copy)]
struct Leaf {
    offset: u64,
    size: u64,
    kind: LeafKind,
}

/// Classifies the boundary of an exported aggregate.
///
/// # Errors
///
/// Returns a message when the type is not an export aggregate or its layout
/// cannot be computed.
pub fn classify(
    abi: ExportAbi,
    ty: &ArType,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    pointer_width: u64,
) -> Result<ExportAbiInfo, String> {
    let (layout, leaves) = layout_and_leaves(ty, interner, provider, pointer_width)?;
    let size = layout.size;
    let align = layout.align.max(1);
    if size == 0 {
        return Ok(ExportAbiInfo {
            size,
            align,
            param: ParamAbi::Ignored,
            ret: ReturnAbi::Void,
        });
    }
    let (param, ret) = match abi {
        ExportAbi::SysVAmd64 => sysv(&leaves, size),
        ExportAbi::WindowsX64 => windows_x64(size),
        ExportAbi::Aapcs64 => aapcs64(&leaves, size),
        ExportAbi::Aapcs32 { hard_float } => aapcs32(&leaves, size, align, hard_float),
        ExportAbi::RiscvIlp32 => riscv_ilp32(size, align),
    };
    Ok(ExportAbiInfo {
        size,
        align,
        param,
        ret,
    })
}

fn sysv(leaves: &[Leaf], size: u64) -> (ParamAbi, ReturnAbi) {
    if size > 16 {
        return (ParamAbi::Indirect, ReturnAbi::Sret);
    }
    let eightbytes = usize::try_from(size.div_ceil(8)).unwrap_or(2);
    let mut lanes = Vec::with_capacity(eightbytes);
    for index in 0..eightbytes {
        let start = index as u64 * 8;
        let end = (start + 8).min(size);
        let mut integer = false;
        let mut float32 = true;
        let mut seen = false;
        for leaf in leaves {
            let leaf_end = leaf.offset + leaf.size;
            if leaf.offset < end && leaf_end > start {
                seen = true;
                match leaf.kind {
                    LeafKind::Int => integer = true,
                    LeafKind::F32 => {}
                    LeafKind::F64 => float32 = false,
                }
            }
        }
        let remaining = end - start;
        let part = if integer || !seen {
            AbiPart::Int(exact_int_bits(remaining))
        } else if remaining == 8 && float32 {
            AbiPart::FloatPair
        } else if remaining == 4 {
            AbiPart::Float(32)
        } else {
            AbiPart::Float(64)
        };
        lanes.push(AbiLane {
            part,
            offset: start,
        });
    }
    (ParamAbi::Direct(lanes.clone()), ReturnAbi::Direct(lanes))
}

fn windows_x64(size: u64) -> (ParamAbi, ReturnAbi) {
    match size {
        1 | 2 | 4 | 8 => {
            let lane = AbiLane {
                part: AbiPart::Int(exact_int_bits(size)),
                offset: 0,
            };
            (ParamAbi::Direct(vec![lane]), ReturnAbi::Direct(vec![lane]))
        }
        _ => (ParamAbi::Indirect, ReturnAbi::Sret),
    }
}

fn aapcs64(leaves: &[Leaf], size: u64) -> (ParamAbi, ReturnAbi) {
    if let Some(hfa) = homogeneous_float(leaves, size) {
        let lane = AbiLane {
            part: hfa,
            offset: 0,
        };
        return (ParamAbi::Direct(vec![lane]), ReturnAbi::Direct(vec![lane]));
    }
    if size > 16 {
        return (ParamAbi::Indirect, ReturnAbi::Sret);
    }
    let lanes = if size <= 8 {
        vec![AbiLane {
            part: AbiPart::Int(exact_int_bits(size)),
            offset: 0,
        }]
    } else {
        vec![AbiLane {
            part: AbiPart::IntWords { word: 64, count: 2 },
            offset: 0,
        }]
    };
    (ParamAbi::Direct(lanes.clone()), ReturnAbi::Direct(lanes))
}

fn aapcs32(leaves: &[Leaf], size: u64, align: u64, hard_float: bool) -> (ParamAbi, ReturnAbi) {
    let hfa = hard_float
        .then(|| homogeneous_float(leaves, size))
        .flatten();
    if let Some(hfa) = hfa {
        let lane = AbiLane {
            part: hfa,
            offset: 0,
        };
        return (ParamAbi::Direct(vec![lane]), ReturnAbi::Direct(vec![lane]));
    }
    let param = if align >= 8 {
        AbiPart::IntWords {
            word: 64,
            count: u64_to_count(size.div_ceil(8)),
        }
    } else {
        AbiPart::IntWords {
            word: 32,
            count: u64_to_count(size.div_ceil(4)),
        }
    };
    // AAPCS32 returns only up to one word in r0; everything larger uses the
    // hidden pointer (clang: 8-byte structs still use sret).
    let ret = if size <= 2 {
        ReturnAbi::Direct(vec![AbiLane {
            part: AbiPart::Int(exact_int_bits(size)),
            offset: 0,
        }])
    } else if size == 3 {
        ReturnAbi::Direct(vec![AbiLane {
            part: AbiPart::IntWords { word: 32, count: 1 },
            offset: 0,
        }])
    } else if size == 4 {
        ReturnAbi::Direct(vec![AbiLane {
            part: AbiPart::Int(32),
            offset: 0,
        }])
    } else {
        ReturnAbi::Sret
    };
    (
        ParamAbi::Direct(vec![AbiLane {
            part: param,
            offset: 0,
        }]),
        ret,
    )
}

fn riscv_ilp32(size: u64, align: u64) -> (ParamAbi, ReturnAbi) {
    if size > 8 {
        return (ParamAbi::Indirect, ReturnAbi::Sret);
    }
    let lane = if size <= 4 {
        AbiLane {
            part: AbiPart::Int(exact_int_bits(size)),
            offset: 0,
        }
    } else if align >= 8 {
        AbiLane {
            part: AbiPart::Int(64),
            offset: 0,
        }
    } else {
        AbiLane {
            part: AbiPart::IntWords { word: 32, count: 2 },
            offset: 0,
        }
    };
    (ParamAbi::Direct(vec![lane]), ReturnAbi::Direct(vec![lane]))
}

/// Exact bit width for a scalar of `size` bytes, matching clang's `iN` types.
fn exact_int_bits(size: u64) -> u16 {
    u16::try_from(size.min(8) * 8).unwrap_or(64)
}

fn u64_to_count(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// AAPCS homogeneous floating aggregate: 1..=4 members, one float type, no
/// padding between members.
fn homogeneous_float(leaves: &[Leaf], size: u64) -> Option<AbiPart> {
    if !(1..=4).contains(&leaves.len()) {
        return None;
    }
    let first = leaves.first()?;
    let float = match first.kind {
        LeafKind::F32 => 32,
        LeafKind::F64 => 64,
        LeafKind::Int => return None,
    };
    let mut expected = 0;
    for leaf in leaves {
        if leaf.offset != expected || leaf.kind != first.kind || leaf.size != u64::from(float) / 8 {
            return None;
        }
        expected += leaf.size;
    }
    if expected != size {
        return None;
    }
    Some(AbiPart::FloatWords {
        float,
        count: u64_to_count(leaves.len() as u64),
    })
}

fn layout_and_leaves(
    ty: &ArType,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    pointer_width: u64,
) -> Result<(TypeLayout, Vec<Leaf>), String> {
    let engine = LayoutEngine::new(pointer_width);
    let layout = match ty {
        ArType::Slice(_) | ArType::MutSlice(_) => {
            TypeLayout::simple(pointer_width * 2, pointer_width)
        }
        other => engine
            .layout_of_type(other, interner, provider)
            .map_err(|error| format!("layout of {other:?} failed: {error}"))?,
    };
    let mut leaves = Vec::new();
    collect_leaves(
        ty,
        0,
        &layout,
        interner,
        provider,
        pointer_width,
        &mut leaves,
    )?;
    Ok((layout, leaves))
}

fn collect_leaves(
    ty: &ArType,
    base: u64,
    layout: &TypeLayout,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    pointer_width: u64,
    out: &mut Vec<Leaf>,
) -> Result<(), String> {
    match ty {
        ArType::Named(..) => {
            let Some(fields) = provider.get_struct_fields_for_type(ty, interner) else {
                out.push(Leaf {
                    offset: base,
                    size: layout.size,
                    kind: LeafKind::Int,
                });
                return Ok(());
            };
            let offsets = layout.field_offsets.clone();
            let fields = fields.clone();
            for (field, offset) in fields.iter().zip(offsets) {
                let field_ty = interner.resolve(field.ty);
                collect_leaf_type(
                    &field_ty,
                    base + offset,
                    interner,
                    provider,
                    pointer_width,
                    out,
                )?;
            }
            Ok(())
        }
        ArType::Array(count, element) => {
            let element_ty = interner.resolve(*element);
            let engine = LayoutEngine::new(pointer_width);
            let element_layout = engine
                .layout_of_type(&element_ty, interner, provider)
                .map_err(|error| format!("array element layout failed: {error}"))?;
            let stride = element_layout.size.max(1);
            if *count > 8192 {
                return Err("aggregate has too many elements to classify".to_owned());
            }
            for index in 0..*count {
                collect_leaf_type(
                    &element_ty,
                    base + index * stride,
                    interner,
                    provider,
                    pointer_width,
                    out,
                )?;
            }
            Ok(())
        }
        other => {
            collect_leaf_type(other, base, interner, provider, pointer_width, out)?;
            Ok(())
        }
    }
}

fn collect_leaf_type(
    ty: &ArType,
    base: u64,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    pointer_width: u64,
    out: &mut Vec<Leaf>,
) -> Result<(), String> {
    match ty {
        ArType::Primitive(Primitive::Str) => {
            out.push(Leaf {
                offset: base,
                size: pointer_width,
                kind: LeafKind::Int,
            });
            out.push(Leaf {
                offset: base + pointer_width,
                size: pointer_width,
                kind: LeafKind::Int,
            });
            Ok(())
        }
        ArType::Primitive(primitive) => {
            let (size, kind) = primitive_leaf(*primitive, pointer_width)?;
            out.push(Leaf {
                offset: base,
                size,
                kind,
            });
            Ok(())
        }
        ArType::Ptr(_)
        | ArType::Ref(_)
        | ArType::RefMut(_)
        | ArType::Nullable(_)
        | ArType::GenRef
        | ArType::Err => {
            out.push(Leaf {
                offset: base,
                size: pointer_width,
                kind: LeafKind::Int,
            });
            Ok(())
        }
        ArType::Slice(_) | ArType::MutSlice(_) => {
            out.push(Leaf {
                offset: base,
                size: pointer_width,
                kind: LeafKind::Int,
            });
            out.push(Leaf {
                offset: base + pointer_width,
                size: pointer_width,
                kind: LeafKind::Int,
            });
            Ok(())
        }
        ArType::Named(..) | ArType::Array(..) => {
            let engine = LayoutEngine::new(pointer_width);
            let layout = engine
                .layout_of_type(ty, interner, provider)
                .map_err(|error| format!("nested layout failed: {error}"))?;
            collect_leaves(ty, base, &layout, interner, provider, pointer_width, out)?;
            Ok(())
        }
        other => Err(format!("{other:?} has no C leaf representation")),
    }
}

fn primitive_leaf(primitive: Primitive, pointer_width: u64) -> Result<(u64, LeafKind), String> {
    let leaf = match primitive {
        Primitive::Bool | Primitive::I8 | Primitive::U8 | Primitive::Byte => (1, LeafKind::Int),
        Primitive::I16 | Primitive::U16 => (2, LeafKind::Int),
        Primitive::I32 | Primitive::U32 | Primitive::Char => (4, LeafKind::Int),
        Primitive::I64 | Primitive::U64 => (8, LeafKind::Int),
        Primitive::ISize | Primitive::USize => (pointer_width, LeafKind::Int),
        Primitive::F32 => (4, LeafKind::F32),
        Primitive::F64 | Primitive::Float => (8, LeafKind::F64),
        Primitive::Str => (pointer_width, LeafKind::Int),
        other => return Err(format!("{other:?} has no C leaf representation")),
    };
    Ok(leaf)
}

/// The C spelling of a primitive type, used by the generated header.
#[must_use]
pub fn c_primitive_name(primitive: Primitive, pointer_width: u64) -> Option<String> {
    let name = match primitive {
        Primitive::Bool => "bool",
        Primitive::I8 => "int8_t",
        Primitive::I16 => "int16_t",
        Primitive::I32 => "int32_t",
        Primitive::I64 => "int64_t",
        Primitive::U8 | Primitive::Byte => "uint8_t",
        Primitive::U16 => "uint16_t",
        Primitive::U32 | Primitive::Char => "uint32_t",
        Primitive::U64 => "uint64_t",
        Primitive::ISize => {
            if pointer_width == 4 {
                "int32_t"
            } else {
                "int64_t"
            }
        }
        Primitive::USize => {
            if pointer_width == 4 {
                "uint32_t"
            } else {
                "uint64_t"
            }
        }
        Primitive::F32 => "float",
        Primitive::F64 | Primitive::Float => "double",
        _ => return None,
    };
    Some(name.to_owned())
}

/// Symbol of the struct behind a nominal export type.
#[must_use]
pub fn nominal_symbol(ty: &ArType) -> Option<SymbolId> {
    match ty {
        ArType::Named(symbol, _) => Some(*symbol),
        _ => None,
    }
}

/// Interned type id helper for callers that need the element of a slice.
#[must_use]
pub fn slice_element(ty: &ArType) -> Option<TypeId> {
    match ty {
        ArType::Slice(element) | ArType::MutSlice(element) => Some(*element),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
