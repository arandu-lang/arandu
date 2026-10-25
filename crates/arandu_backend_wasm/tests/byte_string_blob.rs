//! Non-UTF-8 byte strings must reach wasm as raw rodata, not as a `[]u8` array.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod common;

#[test]
fn non_utf8_byte_string_blob_lands_in_rodata_and_runs() {
    let source = r#"
public func main(): int {
    let raw = b"\xFF\x00AB"
    if raw.len != 4 { return 1 }
    if (raw[0] as int) != 255 { return 2 }
    if raw[1] != b'\0' { return 3 }
    if (raw[2] as int) != 65 { return 4 }
    if (raw[3] as int) != 66 { return 5 }
    let tail = raw[2..4]
    if tail.len != 2 { return 6 }
    if tail[0] != b'A' { return 7 }
    return 42
}
"#;
    let bytes = common::compile_source(source);
    assert_eq!(common::run_main_i32(&bytes), 42);
    assert!(
        bytes
            .windows(4)
            .any(|window| window == [0xFF, 0x00, 0x41, 0x42]),
        "the blob must be emitted as raw data bytes"
    );
}
