#[cfg(target_arch = "x86")]
use std::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[target_feature(enable = "sse2")]
#[must_use]
/// Skips whitespace characters using SSE2 instructions.
///
/// # Safety
/// The caller must ensure that the CPU supports the SSE2 instruction set.
pub unsafe fn skip_whitespace(bytes: &[u8]) -> (usize, usize, Option<usize>) {
    // SAFETY: The caller guarantees SSE2 support — exactly the `# Safety`
    // precondition documented above, which `#[target_feature(enable = "sse2")]`
    // gates this body to. Every 16-byte read uses the unaligned `_mm_loadu_si128`
    // and only executes while `i + 16 <= bytes.len()`, so it never reads past the
    // end of the slice. The tail `&bytes[i..]` handed to the scalar backend is a
    // valid subslice because the loop maintains `i <= bytes.len()`.
    unsafe {
        let mut i = 0;
        let mut newlines = 0;
        let mut last_nl = None;

        let spaces = _mm_set1_epi8(b' ' as i8);
        let tabs = _mm_set1_epi8(b'\t' as i8);
        let crs = _mm_set1_epi8(b'\r' as i8);
        let nls = _mm_set1_epi8(b'\n' as i8);

        while i + 16 <= bytes.len() {
            let chunk = _mm_loadu_si128(bytes[i..].as_ptr() as *const __m128i);

            let eq_space = _mm_cmpeq_epi8(chunk, spaces);
            let eq_tab = _mm_cmpeq_epi8(chunk, tabs);
            let eq_cr = _mm_cmpeq_epi8(chunk, crs);
            let eq_nl = _mm_cmpeq_epi8(chunk, nls);

            let is_whitespace =
                _mm_or_si128(_mm_or_si128(eq_space, eq_tab), _mm_or_si128(eq_cr, eq_nl));

            let mask = _mm_movemask_epi8(is_whitespace) as u32;

            let non_ws_bit = (!mask) & 0xFFFF;
            if non_ws_bit != 0 {
                let skip = non_ws_bit.trailing_zeros() as usize;

                let nl_mask = _mm_movemask_epi8(eq_nl) as u32;
                let skipped_nl_mask = nl_mask & ((1 << skip) - 1);
                newlines += skipped_nl_mask.count_ones() as usize;
                if skipped_nl_mask != 0 {
                    let last_nl_offset = 31 - skipped_nl_mask.leading_zeros() as usize;
                    last_nl = Some(i + last_nl_offset);
                }

                i += skip;
                return (newlines, i, last_nl);
            }

            let nl_mask = _mm_movemask_epi8(eq_nl) as u32;
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

#[target_feature(enable = "sse2")]
#[must_use]
/// Scans an identifier name prefix using SSE2 instructions.
///
/// # Safety
/// The caller must ensure that the CPU supports the SSE2 instruction set.
pub unsafe fn scan_identifier(bytes: &[u8]) -> usize {
    // SAFETY: The caller guarantees SSE2 support — exactly the `# Safety`
    // precondition documented above, which `#[target_feature(enable = "sse2")]`
    // gates this body to. Every 16-byte read uses the unaligned `_mm_loadu_si128`
    // and only executes while `i + 16 <= bytes.len()`, so it never reads past the
    // end of the slice. The tail `&bytes[i..]` handed to the scalar backend is a
    // valid subslice because the loop maintains `i <= bytes.len()`.
    unsafe {
        let mut i = 0;

        let lc_low = _mm_set1_epi8(b'a' as i8 - 1);
        let lc_high = _mm_set1_epi8(b'z' as i8 + 1);
        let uc_low = _mm_set1_epi8(b'A' as i8 - 1);
        let uc_high = _mm_set1_epi8(b'Z' as i8 + 1);
        let dig_low = _mm_set1_epi8(b'0' as i8 - 1);
        let dig_high = _mm_set1_epi8(b'9' as i8 + 1);
        let under = _mm_set1_epi8(b'_' as i8);

        while i + 16 <= bytes.len() {
            let chunk = _mm_loadu_si128(bytes[i..].as_ptr() as *const __m128i);

            let is_lc = _mm_and_si128(
                _mm_cmpgt_epi8(chunk, lc_low),
                _mm_cmpgt_epi8(lc_high, chunk),
            );
            let is_uc = _mm_and_si128(
                _mm_cmpgt_epi8(chunk, uc_low),
                _mm_cmpgt_epi8(uc_high, chunk),
            );
            let is_dig = _mm_and_si128(
                _mm_cmpgt_epi8(chunk, dig_low),
                _mm_cmpgt_epi8(dig_high, chunk),
            );
            let is_under = _mm_cmpeq_epi8(chunk, under);

            let is_ident = _mm_or_si128(_mm_or_si128(is_lc, is_uc), _mm_or_si128(is_dig, is_under));

            let mask = _mm_movemask_epi8(is_ident) as u32;

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
