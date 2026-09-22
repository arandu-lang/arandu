use std::arch::aarch64::*;

#[inline]
#[must_use]
/// Simulates movemask behavior on Neon ARM architectures.
///
/// # Safety
/// The caller must ensure that the CPU supports ARM Neon instructions.
pub unsafe fn neon_movemask(cmp_mask: uint8x16_t) -> u16 {
    // SAFETY: The caller guarantees Neon support — exactly the `# Safety`
    // precondition documented above. `vld1q_u8` reads only the 16 bytes of the
    // local `weights_data` array. The `std::mem::transmute` reinterprets a 128-bit
    // NEON vector register as the 128-bit vector type expected by
    // `vgetq_lane_u64`; both sides are 128-bit NEON vector types of identical
    // size, so every bit is preserved and no memory outside the register is
    // accessed. The lane indices `0` and `1` are compile-time constants within the
    // two-lane range of `uint64x2_t`.
    unsafe {
        let weights_data: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
        let weights = vld1q_u8(weights_data.as_ptr());
        let masked = vandq_u8(cmp_mask, weights);

        let v16 = vpaddlq_u8(masked);
        let v32 = vpaddlq_u16(v16);
        let v64 = vpaddlq_u32(v32);

        let low_byte = vgetq_lane_u64(std::mem::transmute(v64), 0);
        let high_byte = vgetq_lane_u64(std::mem::transmute(v64), 1);

        (low_byte | (high_byte << 8)) as u16
    }
}

#[target_feature(enable = "neon")]
#[must_use]
/// Skips whitespace characters using Neon ARM instructions.
///
/// # Safety
/// The caller must ensure that the CPU supports the Neon ARM instruction set.
pub unsafe fn skip_whitespace(bytes: &[u8]) -> (usize, usize, Option<usize>) {
    // SAFETY: The caller guarantees Neon support — exactly the `# Safety`
    // precondition documented above, which `#[target_feature(enable = "neon")]`
    // gates this body to; that guarantee also upholds the precondition of the
    // `neon_movemask` calls below. Every 16-byte read uses `vld1q_u8` and only
    // executes while `i + 16 <= bytes.len()`, so it never reads past the end of
    // the slice. The tail `&bytes[i..]` handed to the scalar backend is a valid
    // subslice because the loop maintains `i <= bytes.len()`.
    unsafe {
        let mut i = 0;
        let mut newlines = 0;
        let mut last_nl = None;

        let spaces = vdupq_n_u8(b' ');
        let tabs = vdupq_n_u8(b'\t');
        let crs = vdupq_n_u8(b'\r');
        let nls = vdupq_n_u8(b'\n');

        while i + 16 <= bytes.len() {
            let chunk = vld1q_u8(bytes[i..].as_ptr());

            let eq_space = vceqq_u8(chunk, spaces);
            let eq_tab = vceqq_u8(chunk, tabs);
            let eq_cr = vceqq_u8(chunk, crs);
            let eq_nl = vceqq_u8(chunk, nls);

            let is_whitespace = vorrq_u8(vorrq_u8(eq_space, eq_tab), vorrq_u8(eq_cr, eq_nl));

            let mask = neon_movemask(is_whitespace) as u32;

            let non_ws_bit = (!mask) & 0xFFFF;
            if non_ws_bit != 0 {
                let skip = non_ws_bit.trailing_zeros() as usize;

                let nl_mask = neon_movemask(eq_nl) as u32;
                let skipped_nl_mask = nl_mask & ((1 << skip) - 1);
                newlines += skipped_nl_mask.count_ones() as usize;
                if skipped_nl_mask != 0 {
                    let last_nl_offset = 31 - skipped_nl_mask.leading_zeros() as usize;
                    last_nl = Some(i + last_nl_offset);
                }

                i += skip;
                return (newlines, i, last_nl);
            }

            let nl_mask = neon_movemask(eq_nl) as u32;
            newlines += nl_mask.count_ones() as usize;
            if nl_mask != 0 {
                let last_nl_offset = 31 - nl_mask.leading_zeros() as usize;
                last_nl = Some(i + last_nl_offset);
            }
            i += 16;
        }

        let (rem_nl, rem_skip, rem_last_nl) = crate::simd::scalar::skip_whitespace(&bytes[i..]);
        newlines += rem_nl;
        if rem_skip > 0 {
            if let Some(offset) = rem_last_nl {
                last_nl = Some(i + offset);
            }
            i += rem_skip;
        }

        (newlines, i, last_nl)
    }
}

#[target_feature(enable = "neon")]
#[must_use]
/// Scans an identifier name prefix using Neon ARM instructions.
///
/// # Safety
/// The caller must ensure that the CPU supports the Neon ARM instruction set.
pub unsafe fn scan_identifier(bytes: &[u8]) -> usize {
    // SAFETY: The caller guarantees Neon support — exactly the `# Safety`
    // precondition documented above, which `#[target_feature(enable = "neon")]`
    // gates this body to; that guarantee also upholds the precondition of the
    // `neon_movemask` calls below. Every 16-byte read uses `vld1q_u8` and only
    // executes while `i + 16 <= bytes.len()`, so it never reads past the end of
    // the slice. The tail `&bytes[i..]` handed to the scalar backend is a valid
    // subslice because the loop maintains `i <= bytes.len()`.
    unsafe {
        let mut i = 0;

        let lc_low = vdupq_n_u8(b'a');
        let lc_high = vdupq_n_u8(b'z');
        let uc_low = vdupq_n_u8(b'A');
        let uc_high = vdupq_n_u8(b'Z');
        let dig_low = vdupq_n_u8(b'0');
        let dig_high = vdupq_n_u8(b'9');
        let under = vdupq_n_u8(b'_');

        while i + 16 <= bytes.len() {
            let chunk = vld1q_u8(bytes[i..].as_ptr());

            let is_lc = vandq_u8(vcgeq_u8(chunk, lc_low), vcleq_u8(chunk, lc_high));
            let is_uc = vandq_u8(vcgeq_u8(chunk, uc_low), vcleq_u8(chunk, uc_high));
            let is_dig = vandq_u8(vcgeq_u8(chunk, dig_low), vcleq_u8(chunk, dig_high));
            let is_under = vceqq_u8(chunk, under);

            let is_ident = vorrq_u8(vorrq_u8(is_lc, is_uc), vorrq_u8(is_dig, is_under));

            let mask = neon_movemask(is_ident) as u32;

            let non_ident_bit = (!mask) & 0xFFFF;
            if non_ident_bit != 0 {
                let skip = non_ident_bit.trailing_zeros() as usize;
                i += skip;
                return i;
            }

            i += 16;
        }

        i += crate::simd::scalar::scan_identifier(&bytes[i..]);
        i
    }
}
