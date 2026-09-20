#![cfg(target_pointer_width = "64")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use arandu_backend_cranelift::{CraneliftObjectBackend, DebugSource};
use arandu_semantics::{lower_to_amir, lower_to_hir, resolve_for_test, type_check};
use cranelift_object::object::{self, Object, ObjectSection, ObjectSymbol};
use std::sync::Arc;

fn compile_object(src: &str) -> arandu_backend_cranelift::ObjectArtifact {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir, 64).expect("AMIR lowering failed");
    let symbols = Arc::unwrap_or_clone(tc.symbols);
    let type_info = Arc::unwrap_or_clone(tc.type_info);

    CraneliftObjectBackend::host_baseline()
        .expect("host baseline ISA should be supported")
        .compile(&amir, &symbols, &type_info)
        .expect("object emission should succeed")
}

fn compile_debug_object(src: &str) -> arandu_backend_cranelift::ObjectArtifact {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir, 64).expect("AMIR lowering failed");
    let symbols = Arc::unwrap_or_clone(tc.symbols);
    let type_info = Arc::unwrap_or_clone(tc.type_info);
    let source = DebugSource {
        file_id: 0,
        path: Arc::new(std::path::PathBuf::from("/workspace/main.aru")),
        text: Arc::from(src),
    };

    CraneliftObjectBackend::host_baseline()
        .expect("host baseline ISA should be supported")
        .compile_with_debug_sources(&amir, &symbols, &type_info, &[source])
        .expect("debug object emission should succeed")
}

#[test]
fn emits_parseable_host_object_with_defined_function() {
    let artifact = compile_object("func main(): int { return 42; }");
    let file = object::File::parse(artifact.bytes()).expect("valid native object");

    #[cfg(target_os = "windows")]
    assert_eq!(file.format(), object::BinaryFormat::Coff);
    #[cfg(target_os = "linux")]
    assert_eq!(file.format(), object::BinaryFormat::Elf);
    #[cfg(target_os = "macos")]
    assert_eq!(file.format(), object::BinaryFormat::MachO);

    #[cfg(target_arch = "x86_64")]
    assert_eq!(file.architecture(), object::Architecture::X86_64);
    #[cfg(target_arch = "aarch64")]
    assert_eq!(file.architecture(), object::Architecture::Aarch64);

    let main = file
        .symbols()
        .find(|symbol| symbol.name() == Ok("main") || symbol.name() == Ok("_main"))
        .expect("object must define the source function");
    assert!(main.is_definition());
    assert!(main.is_global());
    assert!(
        file.sections()
            .any(|section| section.kind() == object::SectionKind::Text && section.size() > 0),
        "object must contain native code"
    );
}

#[test]
fn emits_dwarf_v5_line_and_info_sections() {
    let artifact = compile_debug_object(
        "func add(a: int, b: int): int {\n    let total = a + b;\n    return total;\n}\n",
    );
    let file = object::File::parse(artifact.bytes()).expect("valid native object");
    for name in [".debug_line", ".debug_info", ".debug_loclists"] {
        let section = file.section_by_name(name).expect("DWARF section");
        let data = section.data().expect("DWARF bytes");
        assert!(data.len() >= 6, "{name} must contain a DWARF header");
        let version = if file.is_little_endian() {
            u16::from_le_bytes([data[4], data[5]])
        } else {
            u16::from_be_bytes([data[4], data[5]])
        };
        assert_eq!(version, 5, "{name} must use DWARF v5");
    }
    let debug_strings = file
        .section_by_name(".debug_str")
        .expect("DWARF string section")
        .data()
        .expect("DWARF strings");
    for variable in [b"a".as_slice(), b"b".as_slice(), b"total".as_slice()] {
        assert!(
            debug_strings
                .windows(variable.len())
                .any(|window| window == variable),
            "source variable must be represented in DWARF"
        );
    }
}

#[test]
fn baseline_object_emission_is_byte_deterministic() {
    let src = "func add(a: int, b: int): int { return a + b; }";
    let first = compile_object(src);
    let second = compile_object(src);

    assert_eq!(first.target(), second.target());
    assert_eq!(first.bytes(), second.bytes());
}

#[test]
fn release_object_emission_is_byte_deterministic() {
    let src = "func add(a: int, b: int): int { return a + b; }";
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir, 64).expect("AMIR lowering failed");
    let symbols = Arc::unwrap_or_clone(tc.symbols);
    let type_info = Arc::unwrap_or_clone(tc.type_info);

    let emit = || {
        CraneliftObjectBackend::host_release()
            .expect("host release ISA should be supported")
            .compile(&amir, &symbols, &type_info)
            .expect("release object emission should succeed")
    };
    let first = emit();
    let second = emit();
    assert_eq!(first.target(), second.target());
    assert_eq!(first.bytes(), second.bytes());
}

#[test]
fn emits_zero_unwinding_metadata_sections() {
    let src = "func compute(x: int): int { if x > 10 { return x * 2; } else { return x + 1; } }";
    let artifact = compile_object(src);
    let file = object::File::parse(artifact.bytes()).expect("valid native object");

    // PAN / Invariant 5: abort nativo sem unwinding — zero tabelas de stack unwinding (.eh_frame / .pdata / __compact_unwind).
    for section in file.sections() {
        if let Ok(name) = section.name() {
            assert!(
                !name.contains("eh_frame")
                    && !name.contains("pdata")
                    && !name.contains("xdata")
                    && !name.contains("compact_unwind"),
                "found unwinding metadata section '{name}', but PAN / Invariant 5 mandates zero-metadata runtime"
            );
        }
    }
}

#[test]
fn abort_intrinsic_lowers_to_native_trap_without_external_symbol() {
    let src = r#"
module std.core.abort_test

extern "arandu-intrinsic" {
    func abort(): void
}

func trigger(cond: bool): int {
    if cond {
        unsafe {
            abort();
        }
    }
    return 42;
}
"#;
    let artifact = compile_object(src);
    let file = object::File::parse(artifact.bytes()).expect("valid native object");

    // PAN: abort lowers to native CPU trap instruction (UD2/BRK), NOT to an external function call to libc `abort`.
    let has_undefined_abort = file
        .symbols()
        .any(|sym| sym.name() == Ok("abort") && !sym.is_definition());
    assert!(
        !has_undefined_abort,
        "object must not import an external libc 'abort' symbol; it must inline the CPU trap"
    );
}
