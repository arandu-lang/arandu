#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use arandu_middle::{ctfe::ConstValue, DataLayout, DiagCode, Severity};
use arandu_query::{
    passes::{lower_amir, type_check},
    DatabaseImpl,
};

fn values(source: &str, layout: DataLayout) -> Vec<ConstValue> {
    let mut db = DatabaseImpl::new();
    db.set_target_config(layout);
    let file = db.new_file("frozen.aru".into(), source.into());
    let typed = type_check(&db, file);
    let errors: Vec<_> = typed
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let lowered = lower_amir(&db, file);
    let errors: Vec<_> = lowered
        .type_check
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "residual errors: {errors:?}");
    let mut values: Vec<_> = typed
        .type_info
        .ctfe_values
        .iter()
        .map(|(span, (value, _))| (span.start, value.clone()))
        .collect();
    values.sort_by_key(|(start, _)| *start);
    values.into_iter().map(|(_, value)| value).collect()
}

#[test]
fn public_float_bits_are_identical_for_ptr4_ptr8_and_i686() {
    let source = "func main(): int {\nlet a: f64 = comptime (0.1 + 0.2)\nlet b: f64 = comptime (0.0 / 0.0)\nlet c: f64 = comptime (1.0 / -0.0)\nlet d: f64 = comptime (5e-324 + 5e-324)\nlet e: f32 = comptime (1.000000059604644775390625000000000000000000000000000001 as f32)\nreturn 0\n}";
    let expected = values(source, DataLayout::ptr_width(8));
    for layout in [DataLayout::ptr_width(4), DataLayout::i686_sysv()] {
        assert_eq!(values(source, layout), expected);
    }
    let bits: Vec<_> = expected
        .iter()
        .map(|value| {
            let ConstValue::Float(value) = value else {
                panic!("float")
            };
            value.bits()
        })
        .collect();
    assert_eq!(
        bits,
        [
            0x3fd3333333333334,
            0x7ff8000000000000,
            0xfff0000000000000,
            2,
            0x3f800001
        ]
    );
}

#[test]
fn public_strings_preserve_utf8_empty_nul_and_helper_pool_content() {
    let source = "func text(): str { return \"Olá\0🦀\" }\nfunc main(): int {\nlet a = comptime text()\nlet b = comptime \"\"\nlet c = comptime (\"Olá\" == \"Olá\")\nreturn 0\n}";
    let result = values(source, DataLayout::ptr_width(8));
    assert_eq!(result.len(), 3);
    let ConstValue::String(text) = &result[0] else {
        panic!("string")
    };
    assert_eq!(text.as_str(), "Olá\0🦀");
    let ConstValue::String(empty) = &result[1] else {
        panic!("string")
    };
    assert!(empty.is_empty());
    assert_eq!(result[2], ConstValue::Bool(true));
}

#[test]
fn immutable_byte_views_freeze_static_backing_even_at_unicode_interior() {
    let source = "module std.core.frozen\nextern \"arandu-intrinsic\" {\nfunc strBytes(source: str): []u8\nfunc sliceSubslice<T>(source: []T, start: usize, len: usize): []T\nfunc sliceLen<T>(source: []T): usize\n}\nfunc main(): int {\nlet bytes = comptime { let all = strBytes(\"Olá\")\nunsafe { return sliceSubslice<u8>(all, 3, 1) } }\nreturn bytes[0] as int\n}";
    let result = values(source, DataLayout::ptr_width(8));
    let ConstValue::Bytes(bytes) = &result[0] else {
        panic!("bytes")
    };
    assert_eq!(bytes.as_bytes(), &[0xa1]);
}

#[test]
fn escaped_source_literals_decode_before_freezing_strings_and_byte_views() {
    let source = r#"module std.core.frozen
extern "arandu-intrinsic" {
func strBytes(source: str): []u8
func sliceSubslice<T>(source: []T, start: usize, len: usize): []T
}
func main(): int {
let text = comptime "Olá\0🦀"
let bytes = comptime {
    let all = strBytes("Olá\0🦀")
    unsafe { return sliceSubslice<u8>(all, 3, 2) }
}
return bytes[1] as int
}"#;
    for layout in [DataLayout::ptr_width(4), DataLayout::ptr_width(8)] {
        let result = values(source, layout);
        assert_eq!(result.len(), 2);
        let ConstValue::String(text) = &result[0] else {
            panic!("expected decoded string");
        };
        assert_eq!(text.as_str(), "Olá\0🦀");
        let ConstValue::Bytes(bytes) = &result[1] else {
            panic!("expected frozen byte view");
        };
        assert_eq!(bytes.as_bytes(), &[0xa1, 0]);
    }
}

#[test]
fn float_remainder_and_runtime_string_captures_fail_closed() {
    for source in [
        "func main(): int { let x = comptime (1.0 % 2.0)\nreturn 0 }",
        "func main(input: str): int { let x = comptime input\nreturn 0 }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("invalid.aru".into(), source.into());
        let typed = type_check(&db, file);
        assert!(
            typed.diagnostics.iter().any(|d| matches!(
                d.code,
                DiagCode::T005OperatorNotApplicable
                    | DiagCode::T042UnsupportedComptime
                    | DiagCode::T043ComptimeRuntimeCapture
            )),
            "{:?}",
            typed.diagnostics
        );
        assert!(!typed
            .diagnostics
            .iter()
            .any(|d| d.code.as_str().starts_with("ICE")));
    }
}
