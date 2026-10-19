//! Differential testing oracle for BigInt and BigDecimal against num-bigint.
#![allow(dead_code)]

use num_bigint::{BigInt as NBig, Sign};
use std::fmt::Write as _;

fn to_limbs_padded(val: &NBig, max_limbs: usize) -> (bool, usize, Vec<u64>) {
    let is_neg = val.sign() == Sign::Minus;
    let (_, mut digits) = val.to_u64_digits();
    let len = digits.len();
    while digits.len() < max_limbs {
        digits.push(0);
    }
    (is_neg, len, digits)
}

fn emit_make(val: &NBig) -> String {
    let (is_neg, len, digits) = to_limbs_padded(val, 8);
    match len {
        0 => "big.BigInt.zero()".to_string(),
        1 => format!("makeBig1({is_neg}, {} as u64)", digits[0]),
        2 => format!(
            "makeBig2({is_neg}, {} as u64, {} as u64)",
            digits[0], digits[1]
        ),
        3 => format!(
            "makeBig3({is_neg}, {} as u64, {} as u64, {} as u64)",
            digits[0], digits[1], digits[2]
        ),
        4 => format!(
            "makeBig4({is_neg}, {} as u64, {} as u64, {} as u64, {} as u64)",
            digits[0], digits[1], digits[2], digits[3]
        ),
        _ => format!(
            "makeBig8({is_neg}, {} as u64, {} as u64, {} as u64, {} as u64, {} as u64, {} as u64, {} as u64, {} as u64)",
            digits[0], digits[1], digits[2], digits[3], digits[4], digits[5], digits[6], digits[7]
        ),
    }
}

fn emit_assert(val: &NBig, expr_name: &str) -> String {
    let (is_neg, len, digits) = to_limbs_padded(val, 8);
    format!(
        "if !eqBig(ref {expr_name}, {is_neg}, {len} as usize, {} as u64, {} as u64, {} as u64, {} as u64, {} as u64, {} as u64, {} as u64, {} as u64) {{ return false }}",
        digits[0], digits[1], digits[2], digits[3], digits[4], digits[5], digits[6], digits[7]
    )
}

pub fn source() -> String {
    let mut source = String::from(
        r#"module bigintOracle

import std.math.big as big
import std.core.cmp as cmp

func makeBig1(is_neg: bool, l0: u64): big.BigInt {
    let mut b = big.BigInt.zero()
    b.pushLimb(l0)
    b.trim()
    if is_neg { b.negate() }
    return b
}

func makeBig2(is_neg: bool, l0: u64, l1: u64): big.BigInt {
    let mut b = big.BigInt.zero()
    b.pushLimb(l0)
    b.pushLimb(l1)
    b.trim()
    if is_neg { b.negate() }
    return b
}

func makeBig3(is_neg: bool, l0: u64, l1: u64, l2: u64): big.BigInt {
    let mut b = big.BigInt.zero()
    b.pushLimb(l0)
    b.pushLimb(l1)
    b.pushLimb(l2)
    b.trim()
    if is_neg { b.negate() }
    return b
}

func makeBig4(is_neg: bool, l0: u64, l1: u64, l2: u64, l3: u64): big.BigInt {
    let mut b = big.BigInt.zero()
    b.pushLimb(l0)
    b.pushLimb(l1)
    b.pushLimb(l2)
    b.pushLimb(l3)
    b.trim()
    if is_neg { b.negate() }
    return b
}

func makeBig8(is_neg: bool, l0: u64, l1: u64, l2: u64, l3: u64, l4: u64, l5: u64, l6: u64, l7: u64): big.BigInt {
    let mut b = big.BigInt.zero()
    b.pushLimb(l0)
    b.pushLimb(l1)
    b.pushLimb(l2)
    b.pushLimb(l3)
    b.pushLimb(l4)
    b.pushLimb(l5)
    b.pushLimb(l6)
    b.pushLimb(l7)
    b.trim()
    if is_neg { b.negate() }
    return b
}

func eqBig(actual: ref big.BigInt, is_neg: bool, expected_len: usize, l0: u64, l1: u64, l2: u64, l3: u64, l4: u64, l5: u64, l6: u64, l7: u64): bool {
    if actual.isNegative() != is_neg { return false }
    if actual.limbCount() != expected_len { return false }
    if expected_len > (0 as usize) && actual.getLimb(0 as usize) != l0 { return false }
    if expected_len > (1 as usize) && actual.getLimb(1 as usize) != l1 { return false }
    if expected_len > (2 as usize) && actual.getLimb(2 as usize) != l2 { return false }
    if expected_len > (3 as usize) && actual.getLimb(3 as usize) != l3 { return false }
    if expected_len > (4 as usize) && actual.getLimb(4 as usize) != l4 { return false }
    if expected_len > (5 as usize) && actual.getLimb(5 as usize) != l5 { return false }
    if expected_len > (6 as usize) && actual.getLimb(6 as usize) != l6 { return false }
    if expected_len > (7 as usize) && actual.getLimb(7 as usize) != l7 { return false }
    return true
}
"#,
    );

    let num_0 = NBig::from(0);
    let num_1 = NBig::from(1);
    let num_neg1 = NBig::from(-1);
    let num_42 = NBig::from(42_u64);
    let num_58 = NBig::from(58_u64);
    let num_100 = NBig::from(100_u64);
    let num_neg100 = -num_100.clone();
    let num_250 = NBig::from(250_u64);
    let num_neg250 = -num_250.clone();
    let num_prime = NBig::from(1000000007_u64);
    let num_u64_max = NBig::from(u64::MAX);
    let num_neg_u64_max = -num_u64_max.clone();
    let num_2_pow_64 = &num_u64_max + &num_1;
    let num_128 =
        (NBig::from(0xDEADBEEF_CAFE_BABE_u64) << 64_usize) | NBig::from(0x1337C0DE_1234_5678_u64);
    let num_128_b =
        (NBig::from(0x11223344_55667788_u64) << 64_usize) | NBig::from(0x99AABBCC_DDEEFF00_u64);
    let num_192 = (&num_128 << 64_usize) | NBig::from(0x987654321_u64);
    let num_256 = (NBig::from(0x1234_5678_9ABC_DEF0_u64) << 192_usize)
        | (NBig::from(0x2345_6789_ABCD_EF01_u64) << 128_usize)
        | (NBig::from(0x3456_789A_BCDE_F012_u64) << 64_usize)
        | NBig::from(0x4567_89AB_CDEF_0123_u64);
    let num_512 = (&num_256 << 256_usize) | (&num_256 ^ (&num_128 << 64_usize));

    let mut test_calls = Vec::new();

    // 1. Addition & Subtraction Tests (24 curated pairs)
    let add_sub_pairs: Vec<(NBig, NBig)> = vec![
        (num_0.clone(), num_0.clone()),
        (num_0.clone(), num_100.clone()),
        (num_100.clone(), num_0.clone()),
        (num_0.clone(), num_neg100.clone()),
        (num_neg100.clone(), num_0.clone()),
        (num_42.clone(), num_58.clone()),
        (num_58.clone(), num_42.clone()),
        (num_42.clone(), -num_58.clone()),
        (-num_58.clone(), num_42.clone()),
        (num_prime.clone(), -num_prime.clone()),
        (num_u64_max.clone(), num_1.clone()),
        (num_1.clone(), num_u64_max.clone()),
        (num_u64_max.clone(), num_u64_max.clone()),
        (num_2_pow_64.clone(), -num_1.clone()),
        (num_2_pow_64.clone(), num_2_pow_64.clone()),
        (num_128.clone(), num_128_b.clone()),
        (num_128.clone(), -num_128.clone()),
        (num_192.clone(), num_42.clone()),
        (num_256.clone(), num_256.clone()),
        (num_256.clone(), -num_256.clone()),
        (num_256.clone(), -num_1.clone()),
        (-num_256.clone(), -num_256.clone()),
        (num_128.clone(), -num_128_b.clone()),
        (num_192.clone(), -num_128.clone()),
    ];

    for (idx, (a, b)) in add_sub_pairs.iter().enumerate() {
        let sum = a + b;
        let diff = a - b;

        let fn_name = format!("test_add_sub_{idx}");
        writeln!(source, "\nfunc {fn_name}(): bool {{").unwrap();
        writeln!(source, "    let a = {}", emit_make(a)).unwrap();
        writeln!(source, "    let b = {}", emit_make(b)).unwrap();
        writeln!(source, "    let sum = a.add(ref b)").unwrap();
        writeln!(source, "    {}", emit_assert(&sum, "sum")).unwrap();
        writeln!(source, "    let diff = a.sub(ref b)").unwrap();
        writeln!(source, "    {}", emit_assert(&diff, "diff")).unwrap();
        writeln!(source, "    return true").unwrap();
        writeln!(source, "}}").unwrap();
        test_calls.push(fn_name);
    }

    // 2. Multiplication Tests (12 curated pairs)
    let mul_pairs: Vec<(NBig, NBig)> = vec![
        (num_0.clone(), num_0.clone()),
        (num_0.clone(), num_128.clone()),
        (num_128.clone(), num_0.clone()),
        (num_1.clone(), num_256.clone()),
        (num_neg1.clone(), num_256.clone()),
        (num_neg1.clone(), -num_256.clone()),
        (num_100.clone(), num_neg250.clone()),
        (num_u64_max.clone(), NBig::from(2)),
        (num_u64_max.clone(), num_u64_max.clone()),
        (num_u64_max.clone(), num_neg_u64_max.clone()),
        (num_128.clone(), num_128_b.clone()),
        (num_256.clone(), num_100.clone()),
    ];

    for (idx, (a, b)) in mul_pairs.iter().enumerate() {
        let prod = a * b;
        let fn_name = format!("test_mul_{idx}");
        writeln!(source, "\nfunc {fn_name}(): bool {{").unwrap();
        writeln!(source, "    let a = {}", emit_make(a)).unwrap();
        writeln!(source, "    let b = {}", emit_make(b)).unwrap();
        writeln!(source, "    let prod = a.mul(ref b)").unwrap();
        writeln!(source, "    {}", emit_assert(&prod, "prod")).unwrap();
        writeln!(source, "    return true").unwrap();
        writeln!(source, "}}").unwrap();
        test_calls.push(fn_name);
    }

    // 3. Division and Remainder Tests (22 curated pairs)
    let div_pairs: Vec<(NBig, NBig)> = vec![
        (num_0.clone(), num_1.clone()),
        (num_0.clone(), num_100.clone()),
        (num_42.clone(), num_42.clone()),
        (num_42.clone(), -num_42.clone()),
        (-num_42.clone(), num_42.clone()),
        (-num_42.clone(), -num_42.clone()),
        (NBig::from(10), NBig::from(20)),
        (NBig::from(-10), NBig::from(20)),
        (NBig::from(10), NBig::from(-20)),
        (NBig::from(-10), NBig::from(-20)),
        (NBig::from(100), NBig::from(7)),
        (NBig::from(-100), NBig::from(7)),
        (NBig::from(100), NBig::from(-7)),
        (NBig::from(-100), NBig::from(-7)),
        (num_u64_max.clone(), num_1.clone()),
        (num_u64_max.clone(), NBig::from(2)),
        (num_u64_max.clone(), num_u64_max.clone()),
        (num_128.clone(), NBig::from(10)),
        (num_128.clone(), num_u64_max.clone()),
        (num_256.clone(), num_128.clone()),
        (num_512.clone(), num_256.clone()),
        (num_512.clone(), NBig::from(1000)),
    ];

    for (idx, (a, b)) in div_pairs.iter().enumerate() {
        let quot = a / b;
        let rem = a % b;

        let fn_name = format!("test_div_rem_{idx}");
        writeln!(source, "\nfunc {fn_name}(): bool {{").unwrap();
        writeln!(source, "    let a = {}", emit_make(a)).unwrap();
        writeln!(source, "    let b = {}", emit_make(b)).unwrap();
        writeln!(source, "    let res = a.divRem(ref b)").unwrap();
        writeln!(source, "    {}", emit_assert(&quot, "res.quotient")).unwrap();
        writeln!(source, "    {}", emit_assert(&rem, "res.remainder")).unwrap();
        writeln!(source, "    let q = a.div(ref b)").unwrap();
        writeln!(source, "    {}", emit_assert(&quot, "q")).unwrap();
        writeln!(source, "    let r = a.rem(ref b)").unwrap();
        writeln!(source, "    {}", emit_assert(&rem, "r")).unwrap();
        writeln!(source, "    return true").unwrap();
        writeln!(source, "}}").unwrap();
        test_calls.push(fn_name);
    }

    // 4. BigDecimal Tests (add, sub, mul with scale)
    writeln!(source, "\nfunc test_big_decimal(): bool {{").unwrap();
    writeln!(source, "    let a_int = big.BigInt.fromInt(125)").unwrap();
    writeln!(source, "    let a = big.BigDecimal.new(a_int, 2 as i64)").unwrap(); // 1.25
    writeln!(source, "    let b_int = big.BigInt.fromInt(75)").unwrap();
    writeln!(source, "    let b = big.BigDecimal.new(b_int, 4 as i64)").unwrap(); // 0.0075
    writeln!(
        source,
        "    // 1.25 + 0.0075 = 1.2575 (scale 4, mantissa 12575)"
    )
    .unwrap();
    writeln!(source, "    let sum = a.add(ref b)").unwrap();
    writeln!(
        source,
        "    if sum.scale() != (4 as i64) {{ return false }}"
    )
    .unwrap();
    writeln!(source, "    let sum_mant = sum.mantissa().clone()").unwrap();
    writeln!(source, "    if !eqBig(ref sum_mant, false, 1 as usize, 12575 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64) {{ return false }}").unwrap();
    writeln!(
        source,
        "    // 1.25 - 0.0075 = 1.2425 (scale 4, mantissa 12425)"
    )
    .unwrap();
    writeln!(source, "    let diff = a.sub(ref b)").unwrap();
    writeln!(
        source,
        "    if diff.scale() != (4 as i64) {{ return false }}"
    )
    .unwrap();
    writeln!(source, "    let diff_mant = diff.mantissa().clone()").unwrap();
    writeln!(source, "    if !eqBig(ref diff_mant, false, 1 as usize, 12425 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64) {{ return false }}").unwrap();
    writeln!(
        source,
        "    // 1.25 * 0.0075 = 0.009375 (scale 6, mantissa 9375)"
    )
    .unwrap();
    writeln!(source, "    let prod = a.mul(ref b)").unwrap();
    writeln!(
        source,
        "    if prod.scale() != (6 as i64) {{ return false }}"
    )
    .unwrap();
    writeln!(source, "    let prod_mant = prod.mantissa().clone()").unwrap();
    writeln!(source, "    if !eqBig(ref prod_mant, false, 1 as usize, 9375 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64, 0 as u64) {{ return false }}").unwrap();
    writeln!(source, "    return true").unwrap();
    writeln!(source, "}}").unwrap();
    test_calls.push("test_big_decimal".to_string());

    // Main entry point orchestrating all oracle checks
    writeln!(source, "\npublic func main(): int {{").unwrap();
    for (idx, call) in test_calls.iter().enumerate() {
        // Let's run all and return
        writeln!(source, "    if !{call}() {{ return {} as int }}", idx + 1).unwrap();
    }
    writeln!(source, "    return 0").unwrap();
    writeln!(source, "}}").unwrap();

    source
}
