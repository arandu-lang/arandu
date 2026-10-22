//! Independent host-f64 oracle for scalar edge cases, shared across backends.
use std::fmt::Write as _;

pub fn source() -> String {
    let mut source = String::from("module mathOracle\nimport std.core.math as math\n");
    let mut cases = String::new();
    let mut index = 0;
    let mut check = |expression: String, expected: f64, tolerance: f64| {
        index += 1;
        let assertion = if expected.is_nan() {
            format!("math.isNan({expression})")
        } else if expected.is_infinite() {
            format!("{expression} == {}", literal(expected))
        } else if expected == 0.0 {
            // A zero equality alone would hide lost negative zero.
            format!(
                "({expression} == 0.0 && 1.0 / ({expression}) == {})",
                literal(1.0 / expected)
            )
        } else {
            let absolute = (expected.abs() * tolerance).max(1.0e-14);
            format!(
                "math.abs(({expression}) - {}) <= {}",
                literal(expected),
                literal(absolute)
            )
        };
        writeln!(cases, "    if !({assertion}) {{ return {index} }}").unwrap();
    };
    for value in [
        -0.0,
        0.0,
        -f64::from_bits(1),
        f64::from_bits(1),
        -0.7,
        0.7,
        0.49999999999999994,
        -0.49999999999999994,
        2147483648.75,
        -2147483648.75,
        1099511627776.75,
        -1099511627776.75,
        4503599627370495.5,
        -4503599627370495.5,
        4503599627370497.0,
        -4503599627370497.0,
        f64::MAX,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ] {
        for (name, expected) in [
            ("trunc", value.trunc()),
            ("floor", value.floor()),
            ("ceil", value.ceil()),
            ("round", value.round()),
        ] {
            // Rounding operations are exact, including subnormals and zeros.
            check(format!("math.{name}({})", literal(value)), expected, 0.0);
        }
    }
    for value in [
        -0.0,
        0.0,
        f64::from_bits(1),
        f64::MIN_POSITIVE,
        1.0e-200,
        0.5,
        2.0,
        1.0e200,
        f64::MAX,
        f64::INFINITY,
        -1.0,
        f64::NAN,
    ] {
        check(
            format!("math.sqrt({})", literal(value)),
            value.sqrt(),
            2.0e-14,
        );
        check(
            format!("math.cbrt({})", literal(value)),
            value.cbrt(),
            2.0e-14,
        );
    }
    for value in [
        -746.0,
        -745.0,
        -744.0,
        -740.0,
        -710.0,
        -709.0,
        -1.0,
        0.0,
        1.0,
        100.0,
        709.0,
        709.5,
        709.78,
        710.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ] {
        check(
            format!("math.exp({})", literal(value)),
            value.exp(),
            2.0e-13,
        );
    }
    // Host powi can overflow its reciprocal intermediate. Use exact IEEE
    // representations for powers of two, including the minimum subnormal.
    for (base, exponent, expected) in [
        (2.0_f64, -1074_i64, f64::from_bits(1)),
        (2.0, -1024, f64::from_bits(1 << 50)),
        (0.5, 1074, f64::from_bits(1)),
        (2.0, 1023, f64::from_bits(2046 << 52)),
        (-1.0, 3000000000, 1.0),
        (-1.0, 3000000001, -1.0),
        (-2.0, 33, -8589934592.0),
        (-2.0, 32, 4294967296.0),
    ] {
        check(
            format!("math.pow({}, {}.0)", literal(base), exponent),
            expected,
            0.0,
        );
    }
    for value in [
        -10.0,
        -std::f64::consts::PI,
        -1.0,
        -0.5,
        -0.0,
        0.0,
        0.5,
        1.0,
        std::f64::consts::FRAC_PI_4,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
        10.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ] {
        check(
            format!("math.sin({})", literal(value)),
            value.sin(),
            1.0e-12,
        );
        check(
            format!("math.cos({})", literal(value)),
            value.cos(),
            1.0e-12,
        );
    }
    for value in [
        -1.0,
        -0.0,
        0.0,
        f64::from_bits(1),
        f64::MIN_POSITIVE,
        0.5,
        1.0,
        std::f64::consts::E,
        2.0,
        10.0,
        100.0,
        1.0e100,
        f64::MAX,
        f64::INFINITY,
        f64::NAN,
    ] {
        check(format!("math.ln({})", literal(value)), value.ln(), 1.0e-12);
    }
    writeln!(
        source,
        "public func main(): int {{\n{cases}    return 0\n}}"
    )
    .unwrap();
    source
}

fn literal(value: f64) -> String {
    if value.is_nan() {
        "math.nan()".into()
    } else if value == f64::INFINITY {
        "math.infinity()".into()
    } else if value == f64::NEG_INFINITY {
        "math.negInfinity()".into()
    } else {
        format!("{value:.17e}")
    }
}
