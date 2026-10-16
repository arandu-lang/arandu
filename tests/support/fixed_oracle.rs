//! Mathematical i128 oracle shared by native/C and Wasm fixed-point tests.

use std::fmt::Write as _;

pub fn source() -> String {
    let mut source = String::from("module fixed_oracle\nimport std.core.fixed as fixed\n");
    let mut calls = String::new();
    for (name, bits) in [("Q8_8", 16_u32), ("Q16_16", 32), ("Q32_32", 64)] {
        let fraction = bits / 2;
        let min = -(1_i128 << (bits - 1));
        let max = (1_i128 << (bits - 1)) - 1;
        let scale = 1_i128 << fraction;
        let raw_type = format!("i{bits}");
        writeln!(source, "func check{name}(a: {raw_type}, b: {raw_type}, sum: {raw_type}, difference: {raw_type}, product: {raw_type}, quotient: {raw_type}): bool {{
    let x = fixed.{name}.fromRaw(a)
    let y = fixed.{name}.fromRaw(b)
    if x.add(y).toRaw() != sum {{ return false }}
    if x.sub(y).toRaw() != difference {{ return false }}
    if x.mul(y).toRaw() != product {{ return false }}
    match x.div(y) {{
        Option.Some(value) => {{ if b == 0 || value.toRaw() != quotient {{ return false }} }}
        Option.None => {{ if b != 0 {{ return false }} }}
    }}
    return true
}}").unwrap();
        let boundary = [min, min + 1, -scale - 1, -1, 0, 1, scale + 1, max];
        let mut pairs = Vec::new();
        for a in boundary {
            for b in boundary {
                pairs.push((a, b));
            }
        }
        let mut seed = 0xc0de_1234_7654_3210_u64;
        for _ in 0..32 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let a = signed_bits(seed, bits);
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            pairs.push((a, signed_bits(seed, bits)));
            // Keep a second corpus within the unsaturated product range so
            // limb carries and discarded fractional bits remain observable.
            pairs.push((
                signed_bits(seed.rotate_left(17), bits * 3 / 4),
                signed_bits(seed.rotate_right(9), bits * 3 / 4),
            ));
        }
        for (a, b) in pairs {
            // Rust's arithmetic shift is floor division; signed / truncates to zero.
            let expected = [
                a,
                b,
                (a + b).clamp(min, max),
                (a - b).clamp(min, max),
                ((a * b) >> fraction).clamp(min, max),
                if b == 0 {
                    0
                } else {
                    ((a * scale) / b).clamp(min, max)
                },
            ];
            let arguments = expected.map(|value| literal(value, &raw_type)).join(", ");
            writeln!(calls, "    if !check{name}({arguments}) {{ return 1 }}").unwrap();
        }
        let half_type = format!("i{fraction}");
        writeln!(calls, "    let low{name} = fixed.{name}.fromInt({})\n    if low{name}.toRaw() != {} {{ return 2 }}\n    if low{name}.abs().toRaw() != {} {{ return 3 }}\n    let negative{name} = fixed.{name}.fromRaw({})\n    if negative{name}.toInt() != -1 {{ return 4 }}\n    if negative{name}.fractionPart() != 1 {{ return 5 }}\n    if !negative{name}.isNegative() {{ return 6 }}",
            literal(-(1_i128 << (fraction - 1)), &half_type),
            literal(min, &raw_type), literal(max, &raw_type), literal(-1, &raw_type)).unwrap();
    }
    writeln!(
        source,
        "public func main(): int {{\n{calls}    return 0\n}}"
    )
    .unwrap();
    source
}

fn literal(value: i128, ty: &str) -> String {
    if value == i128::from(i64::MIN) {
        "((-(9223372036854775807 as i64)) - (1 as i64))".into()
    } else {
        format!("({value} as {ty})")
    }
}

fn signed_bits(value: u64, bits: u32) -> i128 {
    let value = i128::from(value) & ((1_i128 << bits) - 1);
    if value & (1_i128 << (bits - 1)) == 0 {
        value
    } else {
        value - (1_i128 << bits)
    }
}
