//! Intrinsics and compiler-recognized built-in runtime operations.

/// Intrinsic operations provided by `std.core` or runtime environments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicKind {
    /// Read value from raw pointer (`ptrRead` / `ptr_read`).
    PtrRead,
    /// Write value to raw pointer (`ptrWrite` / `ptr_write`).
    PtrWrite,
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
}

impl IntrinsicKind {
    /// Classifies an identifier or qualified name into an intrinsic kind.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let bare = name.rsplit(['.', '$']).next().unwrap_or(name);
        match bare {
            "ptrRead" | "ptr_read" | "refRead" | "ref_read" => Some(Self::PtrRead),
            "ptrWrite" | "ptr_write" | "refWrite" | "ref_write" => Some(Self::PtrWrite),
            "ptrOffset" | "ptr_offset" => Some(Self::PtrOffset),
            "sizeOf" | "size_of" => Some(Self::SizeOf),
            "alignOf" | "align_of" => Some(Self::AlignOf),
            "abort" | "abort_generational_mismatch" | "abortGenerationalMismatch" => {
                Some(Self::Abort)
            }
            "blackBox" => Some(Self::BlackBox),
            s if s.starts_with("sliceFromRaw") => Some(Self::SliceFromRaw),
            s if s.starts_with("sliceSubslice") => Some(Self::SliceSubslice),
            s if s.starts_with("sliceLen") => Some(Self::SliceLen),
            s if s.starts_with("sliceData") || s.starts_with("slicePtr") => Some(Self::SliceData),
            s if s.starts_with("strBytes") => Some(Self::StrBytes),
            s if s.starts_with("strView") => Some(Self::StrView),
            _ => None,
        }
    }
}
