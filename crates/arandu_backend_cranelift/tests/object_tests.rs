#![cfg(target_pointer_width = "64")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use arandu_backend_cranelift::{CraneliftObjectBackend, DebugSource};
use arandu_semantics::{lower_to_amir, lower_to_hir, resolve_for_test, type_check};
use cranelift_object::object::{self, Object, ObjectSection, ObjectSymbol};
use std::sync::Arc;

fn compile_object(src: &str) -> arandu_backend_cranelift::ObjectArtifact {
    let (amir, symbols, type_info) = compile_amir(src);
    CraneliftObjectBackend::host_baseline()
        .expect("host baseline ISA should be supported")
        .compile(&amir, &symbols, &type_info)
        .expect("object emission should succeed")
}

fn compile_amir(
    src: &str,
) -> (
    arandu_semantics::amir::AmirProgram,
    arandu_semantics::SymbolTable,
    arandu_semantics::TypeInfo,
) {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir, 8).expect("AMIR lowering failed");
    let symbols = Arc::unwrap_or_clone(tc.symbols);
    let type_info = Arc::unwrap_or_clone(tc.type_info);

    (amir, symbols, type_info)
}

fn imports(artifact: &arandu_backend_cranelift::ObjectArtifact) -> Vec<String> {
    object::File::parse(artifact.bytes())
        .unwrap()
        .symbols()
        .filter(|symbol| symbol.is_undefined())
        .filter_map(|symbol| symbol.name().ok())
        .filter(|name| !name.is_empty())
        .map(|name| name.trim_start_matches('_').to_owned())
        .collect()
}

#[test]
fn scalar_object_has_no_runtime_imports() {
    assert!(imports(&compile_object("func main(): int { return 42 }")).is_empty());
}

#[test]
fn printing_object_imports_only_its_actual_host() {
    let names = imports(&compile_object(
        "import io\nfunc main(): int { io.println(\"hello\") return 0 }",
    ));
    assert_eq!(names, ["io.println"]);
}

#[test]
fn callback_and_unknown_foreign_code_retain_cleanup_conservatively() {
    let artifact = compile_object(
        "extern \"C\" { func foreign(): int }\nfunc main(): int { unsafe { return foreign() } }",
    );
    let names = imports(&artifact);
    assert!(names.iter().any(|name| name == "foreign"));
    assert!(names.iter().any(|name| name == "ar_gen_shutdown_raw"));
    assert!(!names.iter().any(|name| name == "ar_rt_tcp_accept"));
}

#[test]
fn jit_retains_private_functions_for_host_lookup() {
    let (program, symbols, type_info) =
        compile_amir("func privateLookup(): int { return 42 } func main(): int { return 0 }");
    let module = arandu_backend_cranelift::CraneliftBackend::try_new()
        .unwrap()
        .compile(&program, &symbols, &type_info)
        .unwrap();
    // SAFETY: the source defines a zero-argument function returning native i32.
    let private = unsafe { module.get_fn::<extern "C" fn() -> i32>("privateLookup") }.unwrap();
    assert_eq!(private(), 42);
}

#[test]
fn reachable_helper_public_root_and_callback_keep_gen_cleanup() {
    use arandu_semantics::amir::{
        AmirOperand, AmirRvalue, AmirStmt, AmirTemp, GenArenaDomain, TempId,
    };
    use arandu_semantics::layout::DenseRange;
    use arandu_semantics::types::ArType;

    for (source, retained) in [
        (
            "func helper(): int { return 42 } func main(): int { return helper() }",
            true,
        ),
        (
            "public func helper(): int { return 42 } func main(): int { return 0 }",
            true,
        ),
        (
            "extern \"C\" { func sink(callback: ptr[u8]): void }\nfunc helper(): int { return 42 }\nfunc main(): int { unsafe { sink(helper as ptr[u8]) } return 0 }",
            true,
        ),
        (
            "func helper(): int { return 42 } func main(): int { return 0 }",
            false,
        ),
    ] {
        let (mut program, symbols, type_info) = compile_amir(source);
        let units_before = arandu_backend_cranelift::partition_program(
            &program,
            &symbols,
            &type_info,
            &target_lexicon::Triple::host(),
            arandu_backend_cranelift::AotOptimization::Baseline,
            "runtime-demand-test",
        );
        let function = program
            .funcs
            .iter_mut()
            .find(|function| symbols.get(function.symbol).name == "helper")
            .unwrap();
        let temp = TempId::from_usize(function.temps.len());
        function.temps.push(AmirTemp {
            id: temp,
            ty: type_info.type_interner.intern(ArType::GenRef),
            is_copy: true,
            is_nullable: false,
            span: arandu_base::span::Span::new(0, 0, 0),
        });
        function.stmts.push(AmirStmt::Assign {
            lhs: temp,
            rhs: AmirRvalue::GenInsert {
                value: AmirOperand::Copy(TempId::from_usize(0)),
                payload_ty: function.return_type,
                arena: GenArenaDomain::CompilerManaged,
                origin: arandu_base::span::Span::new(0, 0, 0),
            },
        });
        function.blocks[0].statements = DenseRange::new(0, function.stmts.len());
        let units_after = arandu_backend_cranelift::partition_program(
            &program,
            &symbols,
            &type_info,
            &target_lexicon::Triple::host(),
            arandu_backend_cranelift::AotOptimization::Baseline,
            "runtime-demand-test",
        );
        let main_before = units_before
            .iter()
            .find(|unit| unit.name == "main")
            .unwrap();
        let main_after = units_after.iter().find(|unit| unit.name == "main").unwrap();
        if retained && !source.contains("callback") {
            assert_ne!(
                main_before.hash, main_after.hash,
                "main's cache must track transitive cleanup policy"
            );
        } else {
            assert_eq!(main_before.hash, main_after.hash);
        }
        let main_bytes = arandu_backend_cranelift::compile_cgu(
            main_after,
            &program,
            &symbols,
            &type_info,
            &target_lexicon::Triple::host(),
            arandu_backend_cranelift::AotOptimization::Baseline,
        )
        .unwrap();
        assert_eq!(
            object::File::parse(main_bytes.as_slice())
                .unwrap()
                .symbols()
                .any(|symbol| {
                    symbol.is_undefined()
                        && symbol
                            .name()
                            .is_ok_and(|name| name.trim_start_matches('_') == "ar_gen_shutdown_raw")
                }),
            retained
        );
        let artifact = CraneliftObjectBackend::host_baseline()
            .unwrap()
            .compile(&program, &symbols, &type_info)
            .unwrap();
        let names = imports(&artifact);
        for name in ["ar_gen_insert_raw", "ar_gen_shutdown_raw"] {
            assert_eq!(
                names.iter().any(|import| import == name),
                retained,
                "{source}: {name}"
            );
        }
        assert!(!names.iter().any(|name| name == "ar_rt_tcp_accept"));
    }
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
    let amir = lower_to_amir(&tc, &hir, 8).expect("AMIR lowering failed");
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
fn executable_object_omits_dead_private_bodies_and_retains_public_roots() {
    let artifact = compile_object(
        "func dead(): int { return 99 } public func api(): int { return 7 } func helper(): int { return 42 } func main(): int { return helper() }",
    );
    let file = object::File::parse(artifact.bytes()).unwrap();
    let definitions: Vec<_> = file
        .symbols()
        .filter(|symbol| symbol.is_definition())
        .filter_map(|symbol| symbol.name().ok())
        .collect();
    for name in ["main", "helper", "api"] {
        assert!(
            definitions
                .iter()
                .any(|defined| defined.trim_start_matches('_') == name)
        );
    }
    assert!(
        !definitions
            .iter()
            .any(|defined| defined.trim_start_matches('_') == "dead")
    );
}

#[test]
fn size_object_emission_is_deterministic() {
    let source = "func main(): int { return 42 }";
    let program = arandu_parser::parse(source).unwrap();
    let mut tc = type_check(
        resolve_for_test(0, &program),
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let hir = lower_to_hir(&mut tc, &program).unwrap();
    let amir = lower_to_amir(&tc, &hir, 8).unwrap();
    let emit = || {
        CraneliftObjectBackend::for_target_with_optimization(
            target_lexicon::Triple::host(),
            arandu_backend_cranelift::AotOptimization::Size,
        )
        .unwrap()
        .compile(&amir, &tc.symbols, &*tc.type_info)
        .unwrap()
    };
    assert_eq!(emit().bytes(), emit().bytes());
}

#[test]
fn executable_object_retains_projected_field_destructors() {
    let artifact = compile_object(
        "struct Inner { p: ptr[u8] }\n@Destructor\nfunc Inner.close(own self): void { free(self.p) }\nstruct Outer { inner: Inner }\npublic func consume(own value: Outer): int { return 0 }\nfunc main(): int { return 0 }",
    );
    let file = object::File::parse(artifact.bytes()).unwrap();
    assert!(
        file.symbols().any(|symbol| {
            symbol.is_definition()
                && symbol
                    .name()
                    .is_ok_and(|name| name.ends_with("Inner.close"))
        }),
        "the exported consumer's projected drop needs its private destructor body"
    );
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
    let amir = lower_to_amir(&tc, &hir, 8).expect("AMIR lowering failed");
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
    let abort_name = if file.format() == object::BinaryFormat::MachO {
        "_abort"
    } else {
        "abort"
    };
    let has_undefined_abort = file
        .symbols()
        .any(|sym| sym.name() == Ok(abort_name) && !sym.is_definition());
    assert!(
        !has_undefined_abort,
        "object must not import an external libc 'abort' symbol; it must inline the CPU trap"
    );
    let imports_host_trap = file.symbols().any(|sym| {
        let Ok(name) = sym.name() else {
            return false;
        };
        (name == "ar_rt_trap" || name == "_ar_rt_trap") && !sym.is_definition()
    });
    if cfg!(windows) {
        assert!(
            imports_host_trap,
            "windows abort must call ar_rt_trap instead of an inline UD2"
        );
    } else {
        assert!(
            !imports_host_trap,
            "unix abort must stay an inline trap with no ar_rt_trap import"
        );
    }
}
