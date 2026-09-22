pub mod scalar;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod avx2;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod sse2;

#[cfg(target_arch = "aarch64")]
pub mod neon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimdBackendKind {
    Scalar,
    Sse2,
    Avx2,
    Neon,
}

impl SimdBackendKind {
    #[must_use]
    pub fn detect() -> Self {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if is_x86_feature_detected!("avx2") {
                return SimdBackendKind::Avx2;
            }
            if is_x86_feature_detected!("sse2") {
                return SimdBackendKind::Sse2;
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // Neon is standard/guaranteed on aarch64, but we check using standard APIs for safety.
            if std::arch::is_aarch64_feature_detected!("neon") {
                return SimdBackendKind::Neon;
            }
        }
        SimdBackendKind::Scalar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_whitespace_equivalence() {
        let inputs = vec![
            "",
            "   ",
            "\n\n\n",
            "\r\n\r\n",
            " \t\r\n \t\r\n",
            "   \n  \t  x",
            "x   \n",
            " \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n \t\r\n", // > 32 bytes
            "                                                  \n", // > 32 bytes
        ];

        for input in inputs {
            let bytes = input.as_bytes();
            let scalar_res = scalar::skip_whitespace(bytes);

            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            {
                if is_x86_feature_detected!("sse2") {
                    // SAFETY: the `is_x86_feature_detected!("sse2")` guard above is
                    // exactly the SSE2 support required by `sse2::skip_whitespace`.
                    let sse2_res = unsafe { sse2::skip_whitespace(bytes) };
                    assert_eq!(scalar_res, sse2_res, "SSE2 mismatch for input {:?}", input);
                }
                if is_x86_feature_detected!("avx2") {
                    // SAFETY: the `is_x86_feature_detected!("avx2")` guard above is
                    // exactly the AVX2 support required by `avx2::skip_whitespace`.
                    let avx2_res = unsafe { avx2::skip_whitespace(bytes) };
                    assert_eq!(scalar_res, avx2_res, "AVX2 mismatch for input {:?}", input);
                }
            }

            #[cfg(target_arch = "aarch64")]
            {
                if std::arch::is_aarch64_feature_detected!("neon") {
                    // SAFETY: the `is_aarch64_feature_detected!("neon")` guard above
                    // is exactly the Neon support required by `neon::skip_whitespace`.
                    let neon_res = unsafe { neon::skip_whitespace(bytes) };
                    assert_eq!(scalar_res, neon_res, "NEON mismatch for input {:?}", input);
                }
            }

            let _ = scalar_res; // Prevent unused warning on archs without SIMD (like ARMv7)
        }
    }

    #[test]
    fn test_identifier_equivalence() {
        let inputs = vec![
            "",
            "abc",
            "a_b_c_1_2_3",
            "123", // starts with digit, but scanned as ident continue
            "abc def",
            "abc\ndef",
            "abc_á_def", // stops at á
            "a_very_long_identifier_that_spans_more_than_sixteen_characters", // > 16
            "a_very_long_identifier_that_spans_more_than_thirty_two_characters_to_trigger_avx2_loop", // > 32
        ];

        for input in inputs {
            let bytes = input.as_bytes();
            let scalar_res = scalar::scan_identifier(bytes);

            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            {
                if is_x86_feature_detected!("sse2") {
                    // SAFETY: the `is_x86_feature_detected!("sse2")` guard above is
                    // exactly the SSE2 support required by `sse2::scan_identifier`.
                    let sse2_res = unsafe { sse2::scan_identifier(bytes) };
                    assert_eq!(scalar_res, sse2_res, "SSE2 mismatch for input {:?}", input);
                }
                if is_x86_feature_detected!("avx2") {
                    // SAFETY: the `is_x86_feature_detected!("avx2")` guard above is
                    // exactly the AVX2 support required by `avx2::scan_identifier`.
                    let avx2_res = unsafe { avx2::scan_identifier(bytes) };
                    assert_eq!(scalar_res, avx2_res, "AVX2 mismatch for input {:?}", input);
                }
            }

            #[cfg(target_arch = "aarch64")]
            {
                if std::arch::is_aarch64_feature_detected!("neon") {
                    // SAFETY: the `is_aarch64_feature_detected!("neon")` guard above
                    // is exactly the Neon support required by `neon::scan_identifier`.
                    let neon_res = unsafe { neon::scan_identifier(bytes) };
                    assert_eq!(scalar_res, neon_res, "NEON mismatch for input {:?}", input);
                }
            }

            let _ = scalar_res; // Prevent unused warning on archs without SIMD (like ARMv7)
        }
    }

    /// Deterministic xorshift64* PRNG with a fixed seed, so the fuzz corpus is
    /// reproducible across runs and platforms without external dependencies.
    struct XorShift64Star(u64);

    impl XorShift64Star {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// Asserts that every available SIMD backend agrees with the scalar reference
    /// for both `skip_whitespace` and `scan_identifier` on the given bytes.
    fn assert_simd_matches_scalar(bytes: &[u8]) {
        let scalar_ws = scalar::skip_whitespace(bytes);
        let scalar_id = scalar::scan_identifier(bytes);

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if is_x86_feature_detected!("sse2") {
                // SAFETY: the `is_x86_feature_detected!("sse2")` guard is exactly the
                // SSE2 support required by `sse2::skip_whitespace`'s `# Safety`.
                let sse2_ws = unsafe { sse2::skip_whitespace(bytes) };
                assert_eq!(
                    scalar_ws, sse2_ws,
                    "SSE2 skip_whitespace mismatch {:?}",
                    bytes
                );
                // SAFETY: same SSE2 feature guard, for `sse2::scan_identifier`.
                let sse2_id = unsafe { sse2::scan_identifier(bytes) };
                assert_eq!(
                    scalar_id, sse2_id,
                    "SSE2 scan_identifier mismatch {:?}",
                    bytes
                );
            }
            if is_x86_feature_detected!("avx2") {
                // SAFETY: the `is_x86_feature_detected!("avx2")` guard is exactly the
                // AVX2 support required by `avx2::skip_whitespace`'s `# Safety`.
                let avx2_ws = unsafe { avx2::skip_whitespace(bytes) };
                assert_eq!(
                    scalar_ws, avx2_ws,
                    "AVX2 skip_whitespace mismatch {:?}",
                    bytes
                );
                // SAFETY: same AVX2 feature guard, for `avx2::scan_identifier`.
                let avx2_id = unsafe { avx2::scan_identifier(bytes) };
                assert_eq!(
                    scalar_id, avx2_id,
                    "AVX2 scan_identifier mismatch {:?}",
                    bytes
                );
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            if std::arch::is_aarch64_feature_detected!("neon") {
                // SAFETY: the `is_aarch64_feature_detected!("neon")` guard is exactly
                // the Neon support required by `neon::skip_whitespace`'s `# Safety`.
                let neon_ws = unsafe { neon::skip_whitespace(bytes) };
                assert_eq!(
                    scalar_ws, neon_ws,
                    "NEON skip_whitespace mismatch {:?}",
                    bytes
                );
                // SAFETY: same Neon feature guard, for `neon::scan_identifier`.
                let neon_id = unsafe { neon::scan_identifier(bytes) };
                assert_eq!(
                    scalar_id, neon_id,
                    "NEON scan_identifier mismatch {:?}",
                    bytes
                );
            }
        }

        let _ = (scalar_ws, scalar_id); // Prevent unused warning on archs without SIMD
    }

    /// Deterministic fuzz: 257 lengths (0..=256) x 8 buffers = 2056 iterations
    /// comparing every available SIMD backend against the scalar reference.
    #[test]
    fn test_simd_scalar_equivalence_fuzz() {
        let mut rng = XorShift64Star(0x0DDB_A11C_5EED_00D5);
        let ws_alphabet = *b" \t\r\n";
        let ident_alphabet = *b"abcXYZ0149_";

        for len in 0..=256usize {
            for round in 0..8u32 {
                let buf: Vec<u8> = match round {
                    // Covers every byte value 0..=255: the `len == 256` buffer holds
                    // the full range in order, shifted per length.
                    0 => (0..len).map(|i| (i + len) as u8).collect(),
                    // Whitespace and CRLF runs at every offset, including the tails
                    // of the 16/32-byte vector lanes.
                    1 => (0..len)
                        .map(|_| ws_alphabet[rng.below(ws_alphabet.len())])
                        .collect(),
                    // Identifier runs crossing vector-lane boundaries.
                    2 => (0..len)
                        .map(|_| ident_alphabet[rng.below(ident_alphabet.len())])
                        .collect(),
                    // Arbitrary bytes: whitespace, identifiers, UTF-8 multibyte and
                    // incomplete sequences all occur.
                    _ => (0..len).map(|_| rng.below(256) as u8).collect(),
                };
                assert_simd_matches_scalar(&buf);
            }
        }
    }
}
