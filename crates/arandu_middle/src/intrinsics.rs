//! Intrinsics and compiler-recognized built-in runtime operations.

/// Scalar storage widths admitted by the atomic intrinsic ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AtomicWidth {
    U8,
    U32,
    U64,
    Usize,
    Pointer,
}

impl AtomicWidth {
    #[must_use]
    pub const fn primitive(self) -> crate::types::Primitive {
        use crate::types::Primitive;
        match self {
            Self::U8 => Primitive::U8,
            Self::U32 => Primitive::U32,
            Self::U64 => Primitive::U64,
            Self::Usize | Self::Pointer => Primitive::USize,
        }
    }
}

/// Atomic operations return the value observed before any write, except store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AtomicOp {
    Load,
    Store,
    Swap,
    Add,
    Sub,
    And,
    Or,
    Xor,
    CompareExchange,
}

impl AtomicOp {
    #[must_use]
    pub const fn argument_count(self) -> usize {
        match self {
            Self::Load => 1,
            Self::CompareExchange => 3,
            _ => 2,
        }
    }
}

/// Intrinsic operations provided by `std.core` or runtime environments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicKind {
    /// Sequentially consistent scalar atomic memory operation.
    Atomic(AtomicOp, AtomicWidth),
    /// Sequentially consistent hardware fence.
    AtomicFence,
    /// Compiler fence; a backend may conservatively emit a hardware fence.
    AtomicCompilerFence,
    /// Read value from raw pointer (`ptrRead` / `ptr_read`).
    PtrRead,
    /// Population count of a 32- or 64-bit integer.
    CountOnes,
    /// Leading zero count; zero returns the integer width.
    LeadingZeros,
    /// Trailing zero count; zero returns the integer width.
    TrailingZeros,
    /// Reverse all bits of a 32- or 64-bit integer.
    ReverseBits,
    /// Write value to raw pointer (`ptrWrite` / `ptr_write`).
    PtrWrite,
    /// Erase an exclusive reference to a raw address (`addressOf`).
    AddressOf,
    /// Offset raw pointer by elements (`ptrOffset` / `ptr_offset`).
    PtrOffset,
    /// Size of type in bytes (`sizeOf` / `size_of`).
    SizeOf,
    /// Alignment of type in bytes (`alignOf` / `align_of`).
    AlignOf,
    /// Abort execution (`abort`).
    Abort,
    /// Compiler barrier preventing optimization (`blackBox`).
    BlackBox,
    /// Construct slice from raw parts (`sliceFromRaw`).
    SliceFromRaw,
    /// Subslice a slice (`sliceSubslice`).
    SliceSubslice,
    /// Length of slice (`sliceLen`).
    SliceLen,
    /// Extract data pointer of slice (`sliceData` / `slicePtr`).
    SliceData,
    /// Borrow UTF-8 storage as a byte slice (`strBytes`).
    StrBytes,
    /// View an owned string prefix as a borrowed string (`strView`).
    StrView,
    /// Runs the destructor of the value pointed to by raw pointer (`dropInPlace` / `drop_in_place`).
    DropInPlace,
}

impl IntrinsicKind {
    /// ABI identity required for compiler intrinsics.
    #[must_use]
    pub const fn declaration_marker(self) -> Option<crate::symbol_table::LangItem> {
        use crate::symbol_table::LangItem;
        match self {
            Self::Atomic(..) | Self::AtomicFence | Self::AtomicCompilerFence => {
                Some(LangItem::AtomicIntrinsic)
            }
            Self::CountOnes | Self::LeadingZeros | Self::TrailingZeros | Self::ReverseBits => {
                Some(LangItem::BitIntrinsic)
            }
            _ => Some(LangItem::CompilerIntrinsic),
        }
    }

    /// Integer operations with no memory, runtime or foreign-code effects.
    #[must_use]
    pub const fn is_pure_bit_operation(self) -> bool {
        matches!(
            self,
            Self::CountOnes | Self::LeadingZeros | Self::TrailingZeros | Self::ReverseBits
        )
    }

    /// Calls whose result may be dropped when unused: they read no memory,
    /// write nothing, cannot trap and always return (LLVM's `memory(none)`,
    /// `nounwind` and `willreturn` together).
    #[must_use]
    pub const fn is_removable_when_unused(self) -> bool {
        matches!(
            self,
            Self::CountOnes
                | Self::LeadingZeros
                | Self::TrailingZeros
                | Self::ReverseBits
                | Self::PtrOffset
                | Self::SizeOf
                | Self::AlignOf
                | Self::SliceLen
                | Self::SliceData
                | Self::StrBytes
        )
    }

    /// Returns `true` for intrinsics that are safe to call without an `unsafe` block.
    ///
    /// ## Criteria
    ///
    /// An intrinsic qualifies as safe when **all** of the following hold:
    ///
    /// 1. It only reads plain integer values or a fat-pointer descriptor
    ///    (`ptr` + `len`) that the type-checker has already validated — it never
    ///    dereferences element data, nor writes to or exposes raw pointers.
    /// 2. It produces no side-effects observable by the Arandu memory model
    ///    (no I/O, no heap allocation, no mutation through a foreign pointer).
    /// 3. It is either a **compile-time constant** (`SizeOf`, `AlignOf`) or a
    ///    **pure value operation** (fat-pointer projection or integer bits).
    ///
    /// `SliceSubslice`, `SliceData`, `SliceFromRaw`, `StrView`, `PtrRead`,
    /// `PtrWrite`, `PtrOffset`, `AddressOf`, `Abort` and `BlackBox` remain `unsafe`.
    #[must_use]
    pub fn is_safe(self) -> bool {
        matches!(
            self,
            Self::SliceLen
                | Self::SizeOf
                | Self::AlignOf
                | Self::StrBytes
                | Self::CountOnes
                | Self::LeadingZeros
                | Self::TrailingZeros
                | Self::ReverseBits
        )
    }

    /// Classifies an identifier or qualified name into an intrinsic kind.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let bare = name.rsplit(['.', '$']).next().unwrap_or(name);
        if bare == "atomicFence" {
            return Some(Self::AtomicFence);
        }
        if bare == "atomicCompilerFence" {
            return Some(Self::AtomicCompilerFence);
        }
        for (suffix, width) in [
            ("Usize", AtomicWidth::Usize),
            ("Ptr", AtomicWidth::Pointer),
            ("8", AtomicWidth::U8),
            ("32", AtomicWidth::U32),
            ("64", AtomicWidth::U64),
        ] {
            if let Some(operation) = bare.strip_suffix(suffix) {
                let op = match operation {
                    "atomicLoad" => AtomicOp::Load,
                    "atomicStore" => AtomicOp::Store,
                    "atomicSwap" => AtomicOp::Swap,
                    "atomicFetchAdd" => AtomicOp::Add,
                    "atomicFetchSub" => AtomicOp::Sub,
                    "atomicFetchAnd" => AtomicOp::And,
                    "atomicFetchOr" => AtomicOp::Or,
                    "atomicFetchXor" => AtomicOp::Xor,
                    "atomicCompareExchange" => AtomicOp::CompareExchange,
                    _ => continue,
                };
                return Some(Self::Atomic(op, width));
            }
        }
        match bare {
            "ptrRead" | "ptr_read" | "refRead" | "ref_read" => Some(Self::PtrRead),
            "ptrWrite" | "ptr_write" | "refWrite" | "ref_write" => Some(Self::PtrWrite),
            "addressOf" | "address_of" | "addressOfShared" | "address_of_shared" => {
                Some(Self::AddressOf)
            }
            "ptrOffset" | "ptr_offset" => Some(Self::PtrOffset),
            "sizeOf" | "size_of" => Some(Self::SizeOf),
            "alignOf" | "align_of" => Some(Self::AlignOf),
            "abort" | "abort_generational_mismatch" | "abortGenerationalMismatch" => {
                Some(Self::Abort)
            }
            "blackBox" => Some(Self::BlackBox),
            "bitCountOnes32" | "bitCountOnes64" => Some(Self::CountOnes),
            "bitLeadingZeros32" | "bitLeadingZeros64" => Some(Self::LeadingZeros),
            "bitTrailingZeros32" | "bitTrailingZeros64" => Some(Self::TrailingZeros),
            "bitReverse32" | "bitReverse64" => Some(Self::ReverseBits),
            s if s.starts_with("sliceFromRaw") || s.starts_with("mutSliceFromRaw") => {
                Some(Self::SliceFromRaw)
            }
            s if s.starts_with("sliceSubslice") => Some(Self::SliceSubslice),
            s if s.starts_with("sliceLen") || s.starts_with("mutSliceLen") => Some(Self::SliceLen),
            s if s.starts_with("sliceData") || s.starts_with("slicePtr") => Some(Self::SliceData),
            s if s.starts_with("strBytes") => Some(Self::StrBytes),
            s if s.starts_with("strView") => Some(Self::StrView),
            "dropInPlace" | "drop_in_place" => Some(Self::DropInPlace),
            _ => None,
        }
    }

    /// Classifies a function only when its retained ABI marker declares it as
    /// this compiler intrinsic. Names alone are never sufficient identity.
    #[must_use]
    pub fn from_declared_name(
        name: &str,
        marker: Option<crate::symbol_table::LangItem>,
    ) -> Option<Self> {
        let kind = Self::from_name(name)?;
        (kind.declaration_marker() == marker).then_some(kind)
    }
}

/// Closed SIMD operation admitted by an explicit intrinsic declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SimdOperation {
    Splat,
    Binary(crate::amir::VectorOp),
    Extract(u8),
    Insert(u8),
    Compare(crate::amir::VectorCompareOp),
    Select,
}

/// A SIMD name is only an ABI selector. Callers must additionally require
/// `LangItem::SimdIntrinsic` and validate the exact, non-generic signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SimdIntrinsic {
    pub shape: crate::types::VectorType,
    pub operation: SimdOperation,
}

impl SimdIntrinsic {
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        use crate::amir::{VectorCompareOp, VectorOp};
        let bare = name.rsplit(['.', '$']).next().unwrap_or(name);
        let name = bare.strip_prefix("simd")?;
        for shape in crate::types::VectorType::ALL {
            let Some(suffix) = name.strip_prefix(shape.name()) else {
                continue;
            };
            let operation = match suffix {
                "Splat" => SimdOperation::Splat,
                "Add" => SimdOperation::Binary(VectorOp::Add),
                "Sub" => SimdOperation::Binary(VectorOp::Sub),
                "Mul" => SimdOperation::Binary(VectorOp::Mul),
                "Div" => SimdOperation::Binary(VectorOp::Div),
                "BitAnd" => SimdOperation::Binary(VectorOp::BitAnd),
                "BitOr" => SimdOperation::Binary(VectorOp::BitOr),
                "BitXor" => SimdOperation::Binary(VectorOp::BitXor),
                "Eq" => SimdOperation::Compare(VectorCompareOp::Eq),
                "Ne" => SimdOperation::Compare(VectorCompareOp::Ne),
                "Lt" => SimdOperation::Compare(VectorCompareOp::Lt),
                "Le" => SimdOperation::Compare(VectorCompareOp::Le),
                "Gt" => SimdOperation::Compare(VectorCompareOp::Gt),
                "Ge" => SimdOperation::Compare(VectorCompareOp::Ge),
                "Select" => SimdOperation::Select,
                suffix => {
                    let (insert, lane) = if let Some(lane) = suffix.strip_prefix("Extract") {
                        (false, lane)
                    } else {
                        (true, suffix.strip_prefix("Insert")?)
                    };
                    let index: u8 = lane.parse().ok()?;
                    if index >= shape.lanes() || index.to_string() != lane {
                        return None;
                    }
                    if insert {
                        SimdOperation::Insert(index)
                    } else {
                        SimdOperation::Extract(index)
                    }
                }
            };
            if let SimdOperation::Binary(op) = operation
                && !op.supports(shape)
            {
                return None;
            }
            return Some(Self { shape, operation });
        }
        None
    }

    /// Exact signature; no target-sized lane or foreign nominal substitutes.
    #[must_use]
    pub fn signature(self) -> (Vec<crate::types::ArType>, crate::types::ArType) {
        use crate::types::{ArType, VectorType};
        let vector = ArType::Vector(self.shape);
        let lane = ArType::Primitive(self.shape.lane());
        let mask = ArType::Vector(VectorType::U8x16);
        match self.operation {
            SimdOperation::Splat => (vec![lane], vector),
            SimdOperation::Binary(_) => (vec![vector.clone(), vector.clone()], vector),
            SimdOperation::Extract(_) => (vec![vector], lane),
            SimdOperation::Insert(_) => (vec![vector.clone(), lane], vector),
            SimdOperation::Compare(_) => (vec![vector.clone(), vector], mask),
            SimdOperation::Select => (vec![mask, vector.clone(), vector.clone()], vector),
        }
    }
}

#[cfg(test)]
mod simd_tests {
    use super::*;

    #[test]
    fn closed_simd_names_reject_invalid_shapes_lanes_and_operations() {
        for shape in crate::types::VectorType::ALL {
            assert_eq!(
                SimdIntrinsic::from_name(&format!("simd{}Splat", shape.name())).map(|op| op.shape),
                Some(shape)
            );
            assert!(
                SimdIntrinsic::from_name(&format!(
                    "simd{}Extract{}",
                    shape.name(),
                    shape.lanes() - 1
                ))
                .is_some()
            );
            assert!(
                SimdIntrinsic::from_name(&format!("simd{}Extract{}", shape.name(), shape.lanes()))
                    .is_none()
            );
        }
        for name in [
            "simdF32x3Splat",
            "simdF32x4BitAnd",
            "simdU8x16Mul",
            "simdI32x4Div",
            "simdF32x4Extract01",
            "simdF32x4Insert255",
        ] {
            assert!(SimdIntrinsic::from_name(name).is_none(), "{name}");
        }
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::*;

    #[test]
    fn every_intrinsic_spelling_requires_an_explicit_abi_marker() {
        let ordinary =
            IntrinsicKind::from_name("std.core.mem.sliceLen").expect("known intrinsic spelling");
        assert_eq!(
            ordinary.declaration_marker(),
            Some(crate::symbol_table::LangItem::CompilerIntrinsic)
        );
        assert_eq!(
            IntrinsicKind::from_name("bitCountOnes32")
                .unwrap()
                .declaration_marker(),
            Some(crate::symbol_table::LangItem::BitIntrinsic)
        );
        assert_eq!(
            IntrinsicKind::from_name("atomicFence")
                .unwrap()
                .declaration_marker(),
            Some(crate::symbol_table::LangItem::AtomicIntrinsic)
        );
    }
}
