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

/// Memory synchronization ordering for atomic operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AtomicOrdering {
    /// No ordering constraints; only atomicity is guaranteed.
    Relaxed,
    /// Acquires a release sequence, ensuring subsequent accesses see prior writes.
    Acquire,
    /// Releases prior writes to acquiring threads.
    Release,
    /// Combines Acquire and Release on read-modify-write operations.
    AcqRel,
    /// Sequentially consistent total global ordering across all threads.
    #[default]
    SeqCst,
}

/// Overflow-checked integer arithmetic operations (`*WithOverflow*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverflowOp {
    Add,
    Sub,
    Mul,
}

/// Saturating integer arithmetic operations (`saturatingAdd*` / `saturatingSub*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SaturatingOp {
    Add,
    Sub,
}

/// Closed fixed-width integer variants for overflow and saturating intrinsics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArithmeticInt {
    I32,
    U32,
    I64,
    U64,
}

impl ArithmeticInt {
    #[must_use]
    pub const fn primitive(self) -> crate::types::Primitive {
        use crate::types::Primitive;
        match self {
            Self::I32 => Primitive::I32,
            Self::U32 => Primitive::U32,
            Self::I64 => Primitive::I64,
            Self::U64 => Primitive::U64,
        }
    }

    #[must_use]
    pub const fn is_signed(self) -> bool {
        matches!(self, Self::I32 | Self::I64)
    }

    #[must_use]
    pub const fn is_64_bit(self) -> bool {
        matches!(self, Self::I64 | Self::U64)
    }

    #[must_use]
    pub const fn bit_width(self) -> u32 {
        if self.is_64_bit() { 64 } else { 32 }
    }

    #[must_use]
    pub const fn ar_type_name(self) -> &'static str {
        match self {
            Self::I32 => "i32",
            Self::U32 => "u32",
            Self::I64 => "i64",
            Self::U64 => "u64",
        }
    }

    #[must_use]
    pub fn from_suffix(suffix: &str) -> Option<Self> {
        match suffix {
            "I32" => Some(Self::I32),
            "U32" => Some(Self::U32),
            "I64" => Some(Self::I64),
            "U64" => Some(Self::U64),
            _ => None,
        }
    }
}

/// Intrinsic operations provided by `std.core` or runtime environments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicKind {
    /// Scalar atomic memory operation with specified memory ordering.
    Atomic(AtomicOp, AtomicWidth, AtomicOrdering),
    /// Hardware fence with specified memory ordering.
    AtomicFence(AtomicOrdering),
    /// Compiler fence with specified memory ordering.
    AtomicCompilerFence(AtomicOrdering),
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
    /// Rotate a 32- or 64-bit integer left; the `u32` amount is taken modulo the width.
    RotateLeft,
    /// Rotate a 32- or 64-bit integer right; the `u32` amount is taken modulo the width.
    RotateRight,
    /// Reverse the byte order of a 32- or 64-bit integer.
    SwapBytes,
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
    /// Compile-time description of a type (`info`, `std.core.types`).
    TypeInfo,
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
    /// Spin-loop pause / yield hint to the processor (`spinLoopHint`).
    SpinLoopHint,
    /// Branch-prediction hint expecting `cond == true` (`likely`).
    Likely,
    /// Branch-prediction hint expecting `cond == false` (`unlikely`).
    Unlikely,
    /// Fused multiply-add `(a * b) + c` with a single rounding step (`fma32` / `fma64`).
    Fma,
    /// Overflow-checked integer arithmetic (`addWithOverflow*`, `subWithOverflow*`, `mulWithOverflow*`).
    OverflowArithmetic(OverflowOp, ArithmeticInt),
    /// Saturating integer arithmetic (`saturatingAdd*`, `saturatingSub*`).
    SaturatingArithmetic(SaturatingOp, ArithmeticInt),
    /// Bitwise reinterpretation of float as unsigned integer (`f64ToBits`, `f32ToBits`).
    FloatToBits(FloatWidth),
    /// Bitwise reinterpretation of unsigned integer as float (`f64FromBits`, `f32FromBits`).
    FloatFromBits(FloatWidth),
}

/// Floating-point scalar width for bitcast intrinsics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatWidth {
    F32,
    F64,
}

impl FloatWidth {
    #[must_use]
    pub const fn bit_width(self) -> u32 {
        match self {
            Self::F32 => 32,
            Self::F64 => 64,
        }
    }
}

impl IntrinsicKind {
    /// ABI identity required for compiler intrinsics.
    #[must_use]
    pub const fn declaration_marker(self) -> Option<crate::symbol_table::LangItem> {
        use crate::symbol_table::LangItem;
        match self {
            Self::Atomic(..) | Self::AtomicFence(..) | Self::AtomicCompilerFence(..) => {
                Some(LangItem::AtomicIntrinsic)
            }
            Self::CountOnes
            | Self::LeadingZeros
            | Self::TrailingZeros
            | Self::ReverseBits
            | Self::RotateLeft
            | Self::RotateRight
            | Self::SwapBytes
            | Self::FloatToBits(..)
            | Self::FloatFromBits(..) => Some(LangItem::BitIntrinsic),
            _ => Some(LangItem::CompilerIntrinsic),
        }
    }

    /// Pure integer bit operations with no memory, runtime or foreign-code effects.
    #[must_use]
    pub const fn is_pure_integer_bit_operation(self) -> bool {
        matches!(
            self,
            Self::CountOnes
                | Self::LeadingZeros
                | Self::TrailingZeros
                | Self::ReverseBits
                | Self::RotateLeft
                | Self::RotateRight
                | Self::SwapBytes
        )
    }

    /// Integer operations and float bitcasts with no memory, runtime or foreign-code effects.
    #[must_use]
    pub const fn is_pure_bit_operation(self) -> bool {
        self.is_pure_integer_bit_operation()
            || matches!(self, Self::FloatToBits(..) | Self::FloatFromBits(..))
    }

    /// Pure compiler intrinsics admitted inside compile-time evaluation.
    #[must_use]
    pub const fn is_pure_ctfe_intrinsic(self) -> bool {
        self.is_pure_bit_operation()
            || matches!(
                self,
                Self::Abort
                    | Self::Fma
                    | Self::SpinLoopHint
                    | Self::Likely
                    | Self::Unlikely
                    | Self::OverflowArithmetic(..)
                    | Self::SaturatingArithmetic(..)
            )
    }

    /// Intrinsics lowered inline by codegen rather than emitted as calls.
    /// Excludes `Abort` so cold trap wrappers do not bloat hot callers.
    #[must_use]
    pub const fn is_inline_codegen_intrinsic(self) -> bool {
        !matches!(self, Self::Abort)
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
                | Self::RotateLeft
                | Self::RotateRight
                | Self::SwapBytes
                | Self::FloatToBits(..)
                | Self::FloatFromBits(..)
                | Self::Fma
                | Self::Likely
                | Self::Unlikely
                | Self::OverflowArithmetic(..)
                | Self::SaturatingArithmetic(..)
                | Self::PtrOffset
                | Self::SizeOf
                | Self::AlignOf
                | Self::TypeInfo
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
                | Self::TypeInfo
                | Self::StrBytes
                | Self::CountOnes
                | Self::LeadingZeros
                | Self::TrailingZeros
                | Self::ReverseBits
                | Self::RotateLeft
                | Self::RotateRight
                | Self::SwapBytes
                | Self::FloatToBits(..)
                | Self::FloatFromBits(..)
                | Self::SpinLoopHint
                | Self::Likely
                | Self::Unlikely
                | Self::Fma
                | Self::OverflowArithmetic(..)
                | Self::SaturatingArithmetic(..)
        )
    }

    /// Number of operands a pure bit intrinsic takes (`value`, plus `amount`
    /// for rotations). `None` for every other intrinsic.
    #[must_use]
    pub const fn bit_operand_count(self) -> Option<usize> {
        match self {
            Self::RotateLeft | Self::RotateRight => Some(2),
            Self::CountOnes
            | Self::LeadingZeros
            | Self::TrailingZeros
            | Self::ReverseBits
            | Self::SwapBytes => Some(1),
            _ => None,
        }
    }

    /// Whether a pure bit intrinsic returns the input width (`u32`/`u64`)
    /// rather than a `u32` count.
    #[must_use]
    pub const fn bit_result_is_input_width(self) -> bool {
        matches!(
            self,
            Self::ReverseBits | Self::RotateLeft | Self::RotateRight | Self::SwapBytes
        )
    }

    /// Classifies an identifier or qualified name into an intrinsic kind.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let bare = name.rsplit(['.', '$']).next().unwrap_or(name);
        if let Some(rest) = bare.strip_prefix("atomicFence") {
            let ordering = match rest {
                "" | "SeqCst" => AtomicOrdering::SeqCst,
                "Acquire" => AtomicOrdering::Acquire,
                "Release" => AtomicOrdering::Release,
                "AcqRel" => AtomicOrdering::AcqRel,
                _ => return None,
            };
            return Some(Self::AtomicFence(ordering));
        }
        if let Some(rest) = bare.strip_prefix("atomicCompilerFence") {
            let ordering = match rest {
                "" | "SeqCst" => AtomicOrdering::SeqCst,
                "Acquire" => AtomicOrdering::Acquire,
                "Release" => AtomicOrdering::Release,
                "AcqRel" => AtomicOrdering::AcqRel,
                _ => return None,
            };
            return Some(Self::AtomicCompilerFence(ordering));
        }
        for (suffix, width) in [
            ("Usize", AtomicWidth::Usize),
            ("Ptr", AtomicWidth::Pointer),
            ("8", AtomicWidth::U8),
            ("32", AtomicWidth::U32),
            ("64", AtomicWidth::U64),
        ] {
            if let Some(operation) = bare.strip_suffix(suffix) {
                let (op_name, ordering) = if let Some(base) = operation.strip_suffix("Relaxed") {
                    (base, AtomicOrdering::Relaxed)
                } else if let Some(base) = operation.strip_suffix("Acquire") {
                    (base, AtomicOrdering::Acquire)
                } else if let Some(base) = operation.strip_suffix("Release") {
                    (base, AtomicOrdering::Release)
                } else if let Some(base) = operation.strip_suffix("AcqRel") {
                    (base, AtomicOrdering::AcqRel)
                } else if let Some(base) = operation.strip_suffix("SeqCst") {
                    (base, AtomicOrdering::SeqCst)
                } else {
                    (operation, AtomicOrdering::SeqCst)
                };

                let op = match op_name {
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
                return Some(Self::Atomic(op, width, ordering));
            }
        }
        for (prefix, op) in [
            ("addWithOverflow", OverflowOp::Add),
            ("subWithOverflow", OverflowOp::Sub),
            ("mulWithOverflow", OverflowOp::Mul),
        ] {
            if let Some(suffix) = bare.strip_prefix(prefix)
                && let Some(int_ty) = ArithmeticInt::from_suffix(suffix)
            {
                return Some(Self::OverflowArithmetic(op, int_ty));
            }
        }
        for (prefix, op) in [
            ("saturatingAdd", SaturatingOp::Add),
            ("saturatingSub", SaturatingOp::Sub),
        ] {
            if let Some(suffix) = bare.strip_prefix(prefix)
                && let Some(int_ty) = ArithmeticInt::from_suffix(suffix)
            {
                return Some(Self::SaturatingArithmetic(op, int_ty));
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
            "info" => Some(Self::TypeInfo),
            "abort" | "abort_generational_mismatch" | "abortGenerationalMismatch" => {
                Some(Self::Abort)
            }
            "blackBox" => Some(Self::BlackBox),
            "bitCountOnes32" | "bitCountOnes64" => Some(Self::CountOnes),
            "bitLeadingZeros32" | "bitLeadingZeros64" => Some(Self::LeadingZeros),
            "bitTrailingZeros32" | "bitTrailingZeros64" => Some(Self::TrailingZeros),
            "bitReverse32" | "bitReverse64" => Some(Self::ReverseBits),
            "bitRotateLeft32" | "bitRotateLeft64" => Some(Self::RotateLeft),
            "bitRotateRight32" | "bitRotateRight64" => Some(Self::RotateRight),
            "bitSwapBytes32" | "bitSwapBytes64" => Some(Self::SwapBytes),
            "spinLoopHint" | "spin_loop_hint" => Some(Self::SpinLoopHint),
            "likely" => Some(Self::Likely),
            "unlikely" => Some(Self::Unlikely),
            "fma" | "fma32" | "fma64" => Some(Self::Fma),
            "f64ToBits" | "f64_to_bits" | "floatToBits64" | "float_to_bits64" => {
                Some(Self::FloatToBits(FloatWidth::F64))
            }
            "f32ToBits" | "f32_to_bits" | "floatToBits32" | "float_to_bits32" => {
                Some(Self::FloatToBits(FloatWidth::F32))
            }
            "f64FromBits" | "f64_from_bits" | "floatFromBits64" | "float_from_bits64" => {
                Some(Self::FloatFromBits(FloatWidth::F64))
            }
            "f32FromBits" | "f32_from_bits" | "floatFromBits32" | "float_from_bits32" => {
                Some(Self::FloatFromBits(FloatWidth::F32))
            }
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
    Bitmask,
    LoadUnaligned,
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
                "Bitmask" if shape == crate::types::VectorType::U8x16 => SimdOperation::Bitmask,
                "LoadUnaligned" if shape == crate::types::VectorType::U8x16 => {
                    SimdOperation::LoadUnaligned
                }
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

    #[must_use]
    pub const fn is_pure(self) -> bool {
        !matches!(self.operation, SimdOperation::LoadUnaligned)
    }

    /// Exact signature; no target-sized lane or foreign nominal substitutes.
    #[must_use]
    pub fn signature(
        self,
        interner: &crate::types::TypeInterner,
    ) -> (Vec<crate::types::ArType>, crate::types::ArType) {
        use crate::types::{ArType, Primitive, VectorType};
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
            SimdOperation::Bitmask => (vec![mask], ArType::Primitive(Primitive::U32)),
            SimdOperation::LoadUnaligned => {
                let byte_ty = interner.intern(ArType::Primitive(Primitive::U8));
                (vec![ArType::Slice(byte_ty)], mask)
            }
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
            "simdF32x4Bitmask",
            "simdI32x4LoadUnaligned",
        ] {
            assert!(SimdIntrinsic::from_name(name).is_none(), "{name}");
        }
        assert_eq!(
            SimdIntrinsic::from_name("simdU8x16Bitmask").map(|s| (s.operation, s.is_pure())),
            Some((SimdOperation::Bitmask, true))
        );
        assert_eq!(
            SimdIntrinsic::from_name("simdU8x16LoadUnaligned").map(|s| (s.operation, s.is_pure())),
            Some((SimdOperation::LoadUnaligned, false))
        );
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
        assert_eq!(
            IntrinsicKind::from_name("atomicFenceAcquire").unwrap(),
            IntrinsicKind::AtomicFence(AtomicOrdering::Acquire)
        );
        assert_eq!(
            IntrinsicKind::from_name("atomicLoadRelaxed32").unwrap(),
            IntrinsicKind::Atomic(AtomicOp::Load, AtomicWidth::U32, AtomicOrdering::Relaxed)
        );
        assert_eq!(
            IntrinsicKind::from_name("atomicStoreRelease64").unwrap(),
            IntrinsicKind::Atomic(AtomicOp::Store, AtomicWidth::U64, AtomicOrdering::Release)
        );
        assert_eq!(
            IntrinsicKind::from_name("atomicFetchAddAcqRelUsize").unwrap(),
            IntrinsicKind::Atomic(AtomicOp::Add, AtomicWidth::Usize, AtomicOrdering::AcqRel)
        );
        assert_eq!(
            IntrinsicKind::from_name("addWithOverflowI32").unwrap(),
            IntrinsicKind::OverflowArithmetic(OverflowOp::Add, ArithmeticInt::I32)
        );
        assert_eq!(
            IntrinsicKind::from_name("mulWithOverflowU64").unwrap(),
            IntrinsicKind::OverflowArithmetic(OverflowOp::Mul, ArithmeticInt::U64)
        );
        assert_eq!(
            IntrinsicKind::from_name("saturatingSubI64").unwrap(),
            IntrinsicKind::SaturatingArithmetic(SaturatingOp::Sub, ArithmeticInt::I64)
        );
        assert_eq!(IntrinsicKind::from_name("saturatingAdd"), None);
    }
}
