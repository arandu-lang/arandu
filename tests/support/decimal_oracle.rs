//! Small deterministic decimal corpus using whole Rust u128 arithmetic as oracle.
//! Input limits keep all intermediates in u128; Arandu uses independent u32 limbs.
#![allow(dead_code)]

fn literal(magnitude: u128, scale: u32, negative: bool) -> String {
    let mut digits = magnitude.to_string();
    let fractional = usize::try_from(scale).expect("small scale");
    if fractional > 0 {
        while digits.len() <= fractional {
            digits.insert(0, '0');
        }
        digits.insert(digits.len() - fractional, '.');
    }
    if negative && magnitude != 0 {
        digits.insert(0, '-');
    }
    format!("parse(\"{digits}\")")
}

fn expected(mut magnitude: u128, mut scale: u32, negative: bool) -> String {
    let maximum = (1_u128 << 96) - 1;
    let original = magnitude;
    let mut divisor = 1_u128;
    while magnitude > maximum {
        if scale == 0 {
            return "Option.None".into();
        }
        scale -= 1;
        divisor *= 10;
        // Always round from the exact intermediate, never the previous result.
        let quotient = original / divisor;
        let remainder = original % divisor;
        magnitude = quotient
            + u128::from(
                remainder * 2 > divisor || (remainder * 2 == divisor && quotient % 2 == 1),
            );
    }
    format!("Option.Some({})", literal(magnitude, scale, negative))
}

pub fn source() -> String {
    let mut source = String::from(
        r#"module decimalOracle
import std.core.decimal as dec
import std.core.intrinsics as intrinsics
func parse(s: str): dec.Decimal {
    match dec.Decimal.fromStr(s) {
        Option.Some(value) => { return value }
        Option.None => { unsafe { intrinsics.abort() } }
    }
    return dec.Decimal.zero()
}
func same(actual: Option<dec.Decimal>, expected: Option<dec.Decimal>): bool {
    match actual {
        Option.Some(a) => {
            match expected { Option.Some(b) => { return a.eq(b) } Option.None => { return false } }
        }
        Option.None => {
            match expected { Option.Some(_) => { return false } Option.None => { return true } }
        }
    }
}
"#,
    );
    let mut seed = 0x7246_8aa9_11d5_c3e7_u64;
    for group in 0..16 {
        source.push_str(&format!("func corpus{group}(): int {{\n"));
        for case in 0..4 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let a = u128::from(seed % 10_000_000_000_000_000);
            let sa = u32::try_from((seed >> 48) % 13).expect("small scale");
            let na = seed & 1 != 0;
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let b = u128::from(seed % 10_000_000_000_000_000);
            let sb = u32::try_from((seed >> 48) % 13).expect("small scale");
            let nb = seed & 2 != 0;
            let scale = sa.max(sb);
            let signed_a = i128::try_from(a * 10_u128.pow(scale - sa))
                .expect("bounded intermediate")
                * if na { -1 } else { 1 };
            let signed_b = i128::try_from(b * 10_u128.pow(scale - sb))
                .expect("bounded intermediate")
                * if nb { -1 } else { 1 };
            for (operation, magnitude, operation_scale, negative) in [
                (
                    "checkedAdd",
                    (signed_a + signed_b).unsigned_abs(),
                    scale,
                    signed_a + signed_b < 0,
                ),
                (
                    "checkedSub",
                    (signed_a - signed_b).unsigned_abs(),
                    scale,
                    signed_a - signed_b < 0,
                ),
                ("checkedMul", a * b, sa + sb, na != nb),
            ] {
                source.push_str(&format!(
                    "    if !same({}.{}({}), {}) {{ return {} }}\n",
                    literal(a, sa, na),
                    operation,
                    literal(b, sb, nb),
                    expected(magnitude, operation_scale, negative),
                    case + 1
                ));
            }
        }
        source.push_str("    return 0\n}\n");
    }
    source.push_str("func main(): int {\n");
    for group in 0..16 {
        source.push_str(&format!(
            "    if corpus{group}() != 0 {{ return {} }}\n",
            group + 1
        ));
    }
    source.push_str("    return 0\n}\n");
    source
}
