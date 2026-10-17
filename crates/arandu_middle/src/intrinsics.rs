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
    /// ABI identity required for newly admitted compiler intrinsics.
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
            _ => None,
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
            "addressOf" | "address_of" => Some(Self::AddressOf),
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
            s if s.starts_with("sliceFromRaw") => Some(Self::SliceFromRaw),
            s if s.starts_with("sliceSubslice") => Some(Self::SliceSubslice),
            s if s.starts_with("sliceLen") => Some(Self::SliceLen),
            s if s.starts_with("sliceData") || s.starts_with("slicePtr") => Some(Self::SliceData),
            s if s.starts_with("strBytes") => Some(Self::StrBytes),
            s if s.starts_with("strView") => Some(Self::StrView),
            "dropInPlace" | "drop_in_place" => Some(Self::DropInPlace),
            _ => None,
        }
    }
}
