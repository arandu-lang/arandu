//! Target data layout — size and ABI alignment per type class.
//!
//! Separates **target identity** (pointer width, optional triple later) from
//! rules that `pointer_width` alone cannot express (e.g. i686: i64 size=8,
//! abi_align=4). Backends and [`super::LayoutEngine`] consume this struct only;
//! no magic `+8` / hard-coded host assumptions at use sites.
//!
//! ## Platform `float`
//!
//! Language `float` / `FloatLiteral` are always IEEE **f64** (size 8), matching
//! [`docs/arandu-abi-layout-v0.1.md`](../../../../docs/arandu-abi-layout-v0.1.md).
//! They do **not** shrink to 4 bytes on 32-bit targets.

/// Size and ABI alignment of a type class, in bytes.
///
/// `abi_align` is always a power of two. On some ABIs (i686 SysV) it may be
/// **strictly less** than `size` (e.g. i64: size 8, align 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SizeAlign {
    pub size: u64,
    pub abi_align: u64,
}

impl SizeAlign {
    #[must_use]
    pub const fn new(size: u64, abi_align: u64) -> Self {
        Self { size, abi_align }
    }

    /// Natural layout: size == abi_align == `width`.
    #[must_use]
    pub const fn natural(width: u64) -> Self {
        Self {
            size: width,
            abi_align: width,
        }
    }
}

/// Canonical data-layout rules for one compilation target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataLayout {
    /// Pointer / `usize` / `isize` (size and ABI alignment).
    pub pointer: SizeAlign,
    /// Language `float` and `FloatLiteral` (always f64 semantics).
    pub float: SizeAlign,
    /// Fixed-width `i64` / `u64` (size always 8; align may be 4 on i686).
    pub i64: SizeAlign,
    /// Fixed-width `f64` (size always 8; align may be 4 on i686).
    pub f64: SizeAlign,
}

/// Invalid target data is rejected before layout arithmetic or CTFE admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataLayoutError {
    PointerWidth,
    ScalarWidth,
    Alignment,
    FloatLayout,
}

impl DataLayout {
    /// Validate the layouts supported by the current scalar ABI contract.
    /// An alignment may be smaller than its size (notably i686 SysV), but
    /// cannot be zero, non-power-of-two, or larger than the scalar itself.
    pub fn validate(self) -> Result<(), DataLayoutError> {
        if !matches!(self.pointer.size, 4 | 8) {
            return Err(DataLayoutError::PointerWidth);
        }
        if self.float.size != 8 || self.i64.size != 8 || self.f64.size != 8 {
            return Err(DataLayoutError::ScalarWidth);
        }
        for class in [self.pointer, self.float, self.i64, self.f64] {
            if !class.abi_align.is_power_of_two() || class.abi_align > class.size {
                return Err(DataLayoutError::Alignment);
            }
        }
        if self.float != self.f64 {
            return Err(DataLayoutError::FloatLayout);
        }
        Ok(())
    }
    /// Standard LP64 / ILP32-style layout for pointer width `w` (4 or 8).
    ///
    /// - `int`/`uint`/`ptr` = `w`
    /// - `float` = f64 (8/8)
    /// - `i64`/`u64`/`f64` = 8/8
    #[must_use]
    pub fn ptr_width(w: u64) -> Self {
        assert!(
            w == 4 || w == 8,
            "DataLayout::ptr_width expects 4 or 8, got {w}"
        );
        Self {
            pointer: SizeAlign::natural(w),
            float: SizeAlign::new(8, 8),
            i64: SizeAlign::new(8, 8),
            f64: SizeAlign::new(8, 8),
        }
    }

    /// Host process layout (pointer width = `size_of::<usize>()`).
    ///
    /// Used by Cranelift JIT and host C parity; **not** a 32-bit Cranelift.
    #[must_use]
    pub fn host() -> Self {
        Self::ptr_width(std::mem::size_of::<usize>() as u64)
    }

    /// i686 System V ABI: 32-bit pointers; i64/f64 have size 8 but abi_align 4.
    ///
    /// Used for portable/embedded C targeting 32-bit x86 — not Cranelift.
    #[must_use]
    pub const fn i686_sysv() -> Self {
        Self {
            pointer: SizeAlign::natural(4),
            // Language float stays IEEE f64; abi_align 4 matches SysV long double packing neighbors.
            float: SizeAlign::new(8, 4),
            i64: SizeAlign::new(8, 4),
            f64: SizeAlign::new(8, 4),
        }
    }

    #[must_use]
    pub const fn pointer_width(self) -> u64 {
        self.pointer.size
    }

    #[must_use]
    pub const fn pointer_align(self) -> u64 {
        self.pointer.abi_align
    }

    /// Exclusive upper bound for an object in the target's default address
    /// space. Keeping objects below `isize::MAX + 1` preserves pointer
    /// differences and one-past-the-end addressing.
    #[must_use]
    pub const fn object_size_bound(self) -> u64 {
        1_u64 << (self.pointer.size * 8 - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_matches_usize() {
        let dl = DataLayout::host();
        assert_eq!(dl.pointer_width(), std::mem::size_of::<usize>() as u64);
    }

    #[test]
    fn validation_accepts_supported_abis_without_equating_size_and_alignment() {
        for layout in [
            DataLayout::ptr_width(4),
            DataLayout::ptr_width(8),
            DataLayout::i686_sysv(),
        ] {
            assert_eq!(layout.validate(), Ok(()));
        }
    }

    #[test]
    fn validation_rejects_inconsistent_or_unsafe_scalar_layouts() {
        let valid = DataLayout::ptr_width(8);
        assert_eq!(
            DataLayout {
                pointer: SizeAlign::natural(2),
                ..valid
            }
            .validate(),
            Err(DataLayoutError::PointerWidth)
        );
        assert_eq!(
            DataLayout {
                i64: SizeAlign::new(4, 4),
                ..valid
            }
            .validate(),
            Err(DataLayoutError::ScalarWidth)
        );
        for alignment in [0, 3, 16] {
            assert_eq!(
                DataLayout {
                    i64: SizeAlign::new(8, alignment),
                    ..valid
                }
                .validate(),
                Err(DataLayoutError::Alignment)
            );
        }
        assert_eq!(
            DataLayout {
                float: SizeAlign::new(8, 4),
                ..valid
            }
            .validate(),
            Err(DataLayoutError::FloatLayout)
        );
    }

    #[test]
    fn ptr_width_64_natural_i64() {
        let dl = DataLayout::ptr_width(8);
        assert_eq!(dl.pointer, SizeAlign::natural(8));
        assert_eq!(dl.i64, SizeAlign::new(8, 8));
        assert_eq!(dl.float, SizeAlign::new(8, 8));
    }

    #[test]
    fn ptr_width_32_float_still_f64() {
        let dl = DataLayout::ptr_width(4);
        assert_eq!(dl.pointer_width(), 4);
        // Language float is always 8 bytes (ABI), not shrink-to-pointer.
        assert_eq!(dl.float.size, 8);
        assert_eq!(dl.float.abi_align, 8);
    }

    #[test]
    fn i686_sysv_i64_size_8_align_4() {
        let dl = DataLayout::i686_sysv();
        assert_eq!(dl.pointer_width(), 4);
        assert_eq!(dl.i64.size, 8);
        assert_eq!(dl.i64.abi_align, 4);
        assert_eq!(dl.f64.abi_align, 4);
    }

    #[test]
    fn object_size_bound_follows_target_pointer_width() {
        assert_eq!(DataLayout::ptr_width(4).object_size_bound(), 1_u64 << 31);
        assert_eq!(DataLayout::ptr_width(8).object_size_bound(), 1_u64 << 63);
    }

    #[test]
    #[should_panic(expected = "DataLayout::ptr_width expects 4 or 8, got 2")]
    fn ptr_width_rejects_unsupported_widths() {
        let _ = DataLayout::ptr_width(2);
    }
}
