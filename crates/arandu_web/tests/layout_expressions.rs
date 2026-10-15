use arandu_web::compile_source;

#[test]
fn playground_compiles_layout_roots_and_generic_operand_types_for_wasm32() {
    let output = compile_source(
        "func size<T>(): usize { return @sizeOf(T) }\nfunc main(): int { let word = comptime (@sizeOf(usize)); return (word + @alignOf(i64) + size<[3]u16>()) as int }",
    );
    assert!(output.success, "{:?}", output.diagnostics);
    assert!(output.wasm_bytes.is_some());
}

#[test]
fn playground_keeps_invalid_layout_as_a_structured_user_diagnostic() {
    let output = compile_source(
        "func main(): usize { return comptime (@sizeOf([18446744073709551615]u64)) }",
    );
    assert!(!output.success);
    assert!(output.wasm_bytes.is_none());
    assert!(
        output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("T047"))
    );
    assert!(!output.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .code
            .as_deref()
            .is_some_and(|code| code.starts_with("ICE"))
    }));
}

#[test]
fn playground_uses_wasm_identity_even_when_the_compiler_runs_natively() {
    let result = arandu_web::compile_source(
        "func main(): int { comptime if @targetOS() == \"unknown\" && @targetArch() == \"wasm32\" && @targetPointerWidth() == 32 { return 42 } else { return unavailable_host_branch } }",
    );
    assert!(result.success, "{:?}", result.diagnostics);
    let source = "func main(): int { let arch = comptime @targetArch(); return 0 }";
    let offset = u32::try_from(source.find("comptime").unwrap()).unwrap();
    let hover = arandu_web::hover_source(source, offset).expect("hover on compile-time root");
    assert!(hover.contents.contains("wasm32"), "{hover:?}");
}

#[test]
fn web_explicit_limits_match_cli_and_lsp_resource_diagnostics() {
    let limits = arandu_query::ctfe::CtfeLimits::new(1, 1, 1).expect("positive limits");
    let result = arandu_web::compile_source_with_limits(
        "func main(): int { return comptime (20 + 22) }",
        limits,
    );
    assert!(!result.success);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("T045")),
        "{:?}",
        result.diagnostics
    );
}
