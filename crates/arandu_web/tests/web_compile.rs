use arandu_web::{arandu_alloc, arandu_compile, arandu_free_response, compile_source};

#[test]
fn compiles_valid_program_to_wasm_bytes() {
    let source = "public func main(): i32 { return 42 }";
    let res = compile_source(source);
    assert!(
        res.success,
        "valid program must compile: {:?}",
        res.diagnostics
    );
    let bytes = res.wasm_bytes.expect("wasm bytes must be present");
    assert!(
        bytes.starts_with(b"\0asm"),
        "emitted bytes must start with wasm magic"
    );
    assert!(res.diagnostics.is_empty(), "diagnostics must be empty");
}

#[test]
fn test_compile_io_println() {
    let source = r#"
import io

public func main(): i32 {
    io.println("ola do io.println!")
    return 42
}
"#;
    let res = compile_source(source);
    assert!(
        res.success,
        "must compile io.println: {:?}",
        res.diagnostics
    );
    let bytes = res.wasm_bytes.expect("wasm bytes must be present");
    assert!(bytes.starts_with(b"\0asm"));
}

#[test]
fn reports_syntax_error_with_line_and_column() {
    let source = "func (invalid syntax here";
    let res = compile_source(source);
    assert!(!res.success, "syntax error must fail compilation");
    assert!(res.wasm_bytes.is_none(), "wasm bytes must not be emitted");
    assert!(!res.diagnostics.is_empty(), "diagnostics must be produced");
    let diag = &res.diagnostics[0];
    assert!(diag.line >= 1);
    assert!(diag.column >= 1);
}

#[test]
fn reports_type_error_with_diagnostic_code() {
    let source = r#"
public func main(): i32 {
    return "not an integer"
}
"#;
    let res = compile_source(source);
    assert!(!res.success, "type mismatch must fail compilation");
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("T004")),
        "must emit T004 type mismatch: {:?}",
        res.diagnostics
    );
}

#[test]
fn raw_c_abi_roundtrip_executes_safely() {
    let source = b"public func main(): i32 { return 99 }";
    let ptr = arandu_alloc(source.len());
    assert!(!ptr.is_null());

    unsafe {
        std::ptr::copy_nonoverlapping(source.as_ptr(), ptr, source.len());
    }

    let resp_ptr = unsafe { arandu_compile(ptr, source.len()) };
    assert!(!resp_ptr.is_null());

    unsafe {
        let resp = &*resp_ptr;
        assert_eq!(resp.success, 1);
        assert!(resp.wasm_ptr != 0);
        assert!(resp.wasm_len > 0);
        assert!(resp.json_ptr != 0);

        let wasm_slice =
            std::slice::from_raw_parts(resp.wasm_ptr as *const u8, resp.wasm_len as usize);
        assert!(wasm_slice.starts_with(b"\0asm"));

        let json_slice =
            std::slice::from_raw_parts(resp.json_ptr as *const u8, resp.json_len as usize);
        let json_str = std::str::from_utf8(json_slice).expect("json must be valid utf8");
        assert_eq!(json_str, "[]");

        arandu_free_response(resp_ptr);
        arandu_web::arandu_free(ptr, source.len());
    }
}

#[test]
fn compiles_program_with_std_core_fixed() {
    let source = r#"
import std.core.fixed as fixed

public func main(): i32 {
    let one = fixed.fromInt(1 as i16)
    let two = fixed.fromInt(2 as i16)
    let sum = one.add(two)
    return sum.toInt()
}
"#;
    let res = compile_source(source);
    assert!(
        res.success,
        "program with std.core.fixed must compile: {:?}",
        res.diagnostics
    );
    let bytes = res.wasm_bytes.expect("wasm bytes must be present");
    assert!(bytes.starts_with(b"\0asm"));
}

#[test]
fn compiles_program_with_std_core_str() {
    let source = r#"
import std.core.str as strings

public func main(): i32 {
    if strings.contains("arandu", "and") {
        return 1
    }
    return 0
}
"#;
    let res = compile_source(source);
    assert!(
        res.success,
        "program with std.core.str must compile: {:?}",
        res.diagnostics
    );
    let bytes = res.wasm_bytes.expect("wasm bytes must be present");
    assert!(bytes.starts_with(b"\0asm"));
}

#[test]
fn compiles_program_with_std_core_slice() {
    let source = r#"
import std.core.intrinsics as intrinsics
import std.core.slice as slice

public func main(): i32 {
    let bytes = unsafe { intrinsics.strBytes("hello") }
    let l = slice.len<u8>(bytes)
    return l as i32
}
"#;
    let res = compile_source(source);
    assert!(
        res.success,
        "program with std.core.slice must compile: {:?}",
        res.diagnostics
    );
    let bytes = res.wasm_bytes.expect("wasm bytes must be present");
    assert!(bytes.starts_with(b"\0asm"));
}

#[test]
fn compiles_program_with_std_core_io() {
    let source = r#"
import std.core.intrinsics as intrinsics
import std.core.io as io

public func main(): i32 {
    let storage = unsafe { intrinsics.strBytes("arandu") }
    let reader = io.newSliceReader(storage)
    let rem = reader.remaining()
    return rem as i32
}
"#;
    let res = compile_source(source);
    assert!(
        res.success,
        "program with std.core.io must compile: {:?}",
        res.diagnostics
    );
    let bytes = res.wasm_bytes.expect("wasm bytes must be present");
    assert!(bytes.starts_with(b"\0asm"));
}

#[test]
fn formats_source_code_cleanly() {
    let unformatted = "public func main():i32{return 42;}";
    let formatted = arandu_web::format_source(unformatted);
    assert!(formatted.contains("public func main(): i32"));
    assert!(formatted.ends_with('\n'));
}

#[test]
fn raw_c_abi_format_roundtrip() {
    let source = b"public func main():i32{return 42;}";
    let ptr = arandu_alloc(source.len());
    assert!(!ptr.is_null());

    unsafe {
        std::ptr::copy_nonoverlapping(source.as_ptr(), ptr, source.len());
        let resp_ptr = arandu_web::arandu_format(ptr, source.len());
        assert!(!resp_ptr.is_null());

        let resp = &*resp_ptr;
        assert!(resp.ptr != 0);
        assert!(resp.len > 0);

        let json_slice = std::slice::from_raw_parts(resp.ptr as *const u8, resp.len);
        let json_str = std::str::from_utf8(json_slice).expect("json must be valid utf8");
        let formatted: String = serde_json::from_str(json_str).expect("must parse json string");
        assert!(formatted.contains("public func main(): i32"));

        arandu_web::arandu_free_json(resp_ptr);
        arandu_web::arandu_free(ptr, source.len());
    }
}

#[test]
fn completions_query_runs_safely() {
    let source = "public func main(): i32 { return 42 }\n";
    let items = arandu_web::completion_source(source, 26);
    // Ensure completions engine runs and returns a valid vector without crashing
    let _ = items;
}
