#![cfg(target_pointer_width = "64")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use arandu_backend_cranelift::{AotOptimization, compile_cgu, partition_program};
use arandu_semantics::{lower_to_amir, lower_to_hir, resolve_for_test, type_check};
use cranelift_object::object::{self, Object, ObjectSymbol};
use std::sync::Arc;
use target_lexicon::Triple;

const TEST_TOOLCHAIN: &str = "test-toolchain-v1";

fn has_undefined_symbol(object: &object::File<'_>, name: &str) -> bool {
    let name = if object.format() == object::BinaryFormat::MachO {
        format!("_{name}")
    } else {
        name.to_owned()
    };
    object
        .symbols()
        .any(|symbol| symbol.name() == Ok(name.as_str()) && symbol.is_undefined())
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
    let mut hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    arandu_semantics::passes::monomorphize::monomorphize_program(&mut tc, &mut hir)
        .expect("specialization failed");
    let amir = lower_to_amir(&tc, &hir, 8).expect("AMIR lowering failed");
    let symbols = Arc::unwrap_or_clone(tc.symbols);
    let type_info = Arc::unwrap_or_clone(tc.type_info);
    (amir, symbols, type_info)
}

#[test]
fn partitions_functions_into_discrete_cgus() {
    let src = r#"
func alpha(): int {
    return 10;
}

func beta(): int {
    return 20;
}
"#;
    let (amir, symbols, type_info) = compile_amir(src);
    let target = Triple::host();
    let units = partition_program(
        &amir,
        &symbols,
        &type_info,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );

    assert_eq!(units.len(), 2, "expected 2 CGUs for 2 functions");
    assert!(units.iter().any(|u| u.name == "alpha"));
    assert!(units.iter().any(|u| u.name == "beta"));
    assert_ne!(
        units[0].hash, units[1].hash,
        "different functions must have distinct hashes"
    );
}

#[test]
fn editing_one_function_preserves_sibling_cgu_hash() {
    let src_v1 = r#"
func helper(): int {
    return 100;
}

func calculate(): int {
    return helper() + 1;
}

"#;
    let (amir_v1, syms_v1, ti_v1) = compile_amir(src_v1);
    let target = Triple::host();
    let units_v1 = partition_program(
        &amir_v1,
        &syms_v1,
        &ti_v1,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let helper_v1 = units_v1.iter().find(|u| u.name == "helper").unwrap();
    let calc_v1 = units_v1.iter().find(|u| u.name == "calculate").unwrap();

    // Now edit the body of `calculate()`, keeping `helper()` untouched
    let src_v2 = r#"
func helper(): int {
    return 100;
}

func calculate(): int {
    return helper() + 999;
}
"#;
    let (amir_v2, syms_v2, ti_v2) = compile_amir(src_v2);
    let units_v2 = partition_program(
        &amir_v2,
        &syms_v2,
        &ti_v2,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let helper_v2 = units_v2.iter().find(|u| u.name == "helper").unwrap();
    let calc_v2 = units_v2.iter().find(|u| u.name == "calculate").unwrap();

    // `helper` was NOT modified -> its hash MUST be identical!
    assert_eq!(
        helper_v1.hash, helper_v2.hash,
        "untouched sibling function must have identical CGU hash"
    );

    // `calculate` was modified -> its hash MUST differ!
    assert_ne!(
        calc_v1.hash, calc_v2.hash,
        "modified function must have different CGU hash"
    );
}

#[test]
fn removing_an_unreferenced_declaration_preserves_hash_and_object_bytes() {
    let with_unused = "func main(): int { return 1; }\nfunc unused(): int { return 9; }\n";
    let without_unused = "func main(): int { return 1; }\n";
    let (first_program, first_symbols, first_types) = compile_amir(with_unused);
    let (second_program, second_symbols, second_types) = compile_amir(without_unused);
    let target = Triple::host();

    let first = partition_program(
        &first_program,
        &first_symbols,
        &first_types,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let second = partition_program(
        &second_program,
        &second_symbols,
        &second_types,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let first_main = first.iter().find(|unit| unit.name == "main").unwrap();
    let second_main = second.iter().find(|unit| unit.name == "main").unwrap();

    assert_eq!(
        first_main.hash, second_main.hash,
        "an unrelated declaration is not part of this object's closure"
    );
    let emit = |unit, program, symbols, types| {
        compile_cgu(
            unit,
            program,
            symbols,
            types,
            &target,
            AotOptimization::Baseline,
        )
        .expect("object")
    };
    assert_eq!(
        emit(first_main, &first_program, &first_symbols, &first_types),
        emit(second_main, &second_program, &second_symbols, &second_types)
    );
}

fn main_object(source: &str) -> (String, Vec<u8>) {
    main_object_for_target(source, &Triple::host())
}

fn main_object_for_target(source: &str, target: &Triple) -> (String, Vec<u8>) {
    let (program, symbols, types) = compile_amir(source);
    let units = partition_program(
        &program,
        &symbols,
        &types,
        target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let main = units
        .iter()
        .find(|unit| unit.name == "main")
        .expect("main CGU");
    (
        main.hash.clone(),
        compile_cgu(
            main,
            &program,
            &symbols,
            &types,
            target,
            AotOptimization::Baseline,
        )
        .expect("object"),
    )
}

#[test]
fn sibling_literals_and_source_symbol_reallocation_do_not_change_an_object() {
    let first =
        main_object("func helper(): int { return 1 }\nfunc main(): int { return helper() + 42 }");
    let second = main_object(
        "func unrelated(): int { return 3 + 4 + 5 }\nfunc helper(): int { return 6 + 7 }\nfunc main(): int { return helper() + 42 }",
    );
    assert_eq!(
        first, second,
        "only callee signature, not body/pool/synthetic offsets, belongs to the caller"
    );
}

#[test]
fn unreferenced_signature_and_nominal_layout_do_not_change_an_object() {
    let first = main_object(
        "struct Unused { x: int }\nfunc unused(x: int): int { return x }\nfunc main(): int { return 42 }",
    );
    let second = main_object(
        "struct Unused { x: int, y: int }\nfunc unused(x: bool): bool { return x }\nfunc main(): int { return 42 }",
    );
    assert_eq!(first, second);
}

#[test]
fn referenced_signature_changes_invalidate_the_caller() {
    let (mut program, symbols, types) =
        compile_amir("func helper(): int { return 1 }\nfunc main(): int { return helper() }");
    let target = Triple::host();
    let first = partition_program(
        &program,
        &symbols,
        &types,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let integer = program.funcs[0].return_type;
    let boolean = types
        .type_interner
        .intern(arandu_semantics::types::ArType::Primitive(
            arandu_semantics::types::Primitive::Bool,
        ));
    assert_ne!(integer, boolean);
    program.funcs[0].return_type = boolean;
    let second = partition_program(
        &program,
        &symbols,
        &types,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    assert_ne!(first[1].hash, second[1].hash);
}

#[test]
fn enum_payload_metadata_is_part_of_the_cgu_key() {
    use arandu_semantics::amir::{AmirRvalue, AmirStmt};

    let (mut program, symbols, types) = compile_amir(
        "func main(): int { let value: Option<int> = Option.Some(42); match value { Option.Some(x) => { return x; } Option.None => { return 0; } } }",
    );
    let target = Triple::host();
    let hash = |program: &arandu_semantics::amir::AmirProgram| {
        partition_program(
            program,
            &symbols,
            &types,
            &target,
            AotOptimization::Baseline,
            TEST_TOOLCHAIN,
        )[0]
        .hash
        .clone()
    };
    let original = hash(&program);
    let function = &mut program.funcs[0];
    let statement = function
        .stmts
        .iter_ids()
        .find(|&id| {
            matches!(
                function.stmts.get(id),
                Some(AmirStmt::Assign {
                    rhs: AmirRvalue::EnumPayload { .. },
                    ..
                })
            )
        })
        .expect("payload extraction");
    let Some(AmirStmt::Assign {
        rhs: AmirRvalue::EnumPayload { variant_tag, .. },
        ..
    }) = function.stmts.get_mut(statement)
    else {
        panic!("payload extraction changed");
    };
    *variant_tag += 1;
    assert_ne!(
        original,
        hash(&program),
        "a changed payload tag invalidates the object"
    );
}

#[test]
fn referenced_struct_layout_changes_invalidate_the_caller() {
    let first = main_object(
        "struct Point { x: int }\nfunc read(p: Point): int { return p.x }\nfunc main(): int { return read(Point { x: 42 }) }",
    );
    let second = main_object(
        "struct Point { x: int, y: int }\nfunc read(p: Point): int { return p.x }\nfunc main(): int { return read(Point { x: 42, y: 0 }) }",
    );
    assert_ne!(first.0, second.0);
}

#[test]
fn function_references_outside_calls_keep_their_declaration() {
    let source = "extern \"C\" { func sink(callback: ptr[u8]): void }\nfunc callback(): int { return 42 }\nfunc main(): int { unsafe { sink(callback as ptr[u8]) } return 42 }";
    for target in [
        Triple::host(),
        format!("{}-apple-darwin", Triple::host().architecture)
            .parse()
            .expect("Darwin target"),
    ] {
        let (_, bytes) = main_object_for_target(source, &target);
        let object = object::File::parse(bytes.as_slice()).expect("object");
        assert!(has_undefined_symbol(&object, "callback"), "{target}");
    }
}

#[test]
fn implicit_destructor_is_declared_without_a_source_call() {
    let source = "struct Owner { value: int }\n@Destructor\nfunc Owner.destroy(own self: Owner): void {}\nfunc main(): int { let owner = Owner { value: 42 } return owner.value }";
    for target in [
        Triple::host(),
        format!("{}-apple-darwin", Triple::host().architecture)
            .parse()
            .expect("Darwin target"),
    ] {
        let (_, bytes) = main_object_for_target(source, &target);
        let object = object::File::parse(bytes.as_slice()).expect("object");
        assert!(has_undefined_symbol(&object, "Owner.destroy"), "{target}");
    }
}

#[test]
fn compile_cgu_emits_valid_relocatable_objects() {
    let src = r#"
func add(a: int, b: int): int {
    return a + b;
}

func sub(a: int, b: int): int {
    return a - b;
}
"#;
    let (amir, symbols, type_info) = compile_amir(src);
    let target = Triple::host();

    let units = partition_program(
        &amir,
        &symbols,
        &type_info,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    assert_eq!(units.len(), 2);

    for unit in &units {
        let bytes = compile_cgu(
            unit,
            &amir,
            &symbols,
            &type_info,
            &target,
            AotOptimization::Baseline,
        )
        .expect("compile_cgu should succeed");

        let file = object::File::parse(bytes.as_slice()).expect("valid object file");
        assert!(
            file.symbols().any(|s| s.is_definition() && s.is_global()),
            "object must contain at least one defined global symbol"
        );
    }
}

#[test]
fn cgu_hash_ignores_source_spans_but_tracks_toolchain_identity() {
    let compact = "func main(): int { return 7; }\n";
    let shifted = "\n\nfunc main(): int {\n    return 7;\n}\n";
    let (first_program, first_symbols, first_types) = compile_amir(compact);
    let (second_program, second_symbols, second_types) = compile_amir(shifted);
    let target = Triple::host();
    let first = partition_program(
        &first_program,
        &first_symbols,
        &first_types,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let shifted = partition_program(
        &second_program,
        &second_symbols,
        &second_types,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let different_toolchain = partition_program(
        &first_program,
        &first_symbols,
        &first_types,
        &target,
        AotOptimization::Baseline,
        "test-toolchain-v2",
    );

    assert_eq!(first[0].hash, shifted[0].hash);
    assert_ne!(first[0].hash, different_toolchain[0].hash);
}
