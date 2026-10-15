#![allow(clippy::unwrap_used, clippy::expect_used)]
//! TargetConfig drives pointer-width type checking (T038 usize literal bounds)
//! through the Salsa input instead of a host hardcode.

use arandu_middle::layout::DataLayout;
use arandu_middle::literal_pool::{parse_int_literal, AmirLiteralEntry};
use arandu_middle::DiagCode;
use arandu_middle::Severity;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{lower_amir, type_check};

const SRC: &str = "func main(): int {\n    let x: usize = 4294967296\n    return 0\n}\n";

fn t038_count(db: &DatabaseImpl, file: arandu_query::SourceFile) -> usize {
    type_check(db, file)
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::T038IntegerLiteralOutOfRange)
        .count()
}

#[test]
fn host_target_config_accepts_u64_literal() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("target_64.aru".into(), SRC.into());
    assert_eq!(
        t038_count(&db, file),
        0,
        "usize literal must fit on the host 64-bit layout"
    );
}

#[test]
fn target_config_32bit_flags_u64_literal() {
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(4));
    let file = db.new_file("target_32.aru".into(), SRC.into());
    assert_eq!(
        t038_count(&db, file),
        1,
        "usize literal beyond u32::MAX must be rejected on a 32-bit layout"
    );
}

#[test]
fn target_config_32bit_update_revalidates() {
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(4));
    let file = db.new_file("target_revalid.aru".into(), SRC.into());
    assert_eq!(t038_count(&db, file), 1);

    db.set_target_config(DataLayout::ptr_width(8));
    assert_eq!(
        t038_count(&db, file),
        0,
        "edit of TargetConfig must invalidate target-dependent diagnostics"
    );
}

// `std.core.mem` resolvable in a bare query DB: registered under its import key.
const MEM_MODULE: &str = "module std.core.mem\n\nextern \"arandu-intrinsic\" {\n    func sizeOf<T>() : usize\n    func alignOf<T>() : usize\n}\n";

const MAIN_WITH_MEM_INTRINSICS: &str =
    "import std.core.mem as mem\nfunc main(): usize {\n    let a = mem.sizeOf<usize>()\n    let b = mem.sizeOf<u64>()\n    let c = mem.alignOf<usize>()\n    return a + b + c\n}\n";

fn folded_int_literals(db: &DatabaseImpl, file: arandu_query::SourceFile) -> Vec<i128> {
    let tc = type_check(db, file);
    assert!(
        !tc.diagnostics.iter().any(|d| d.severity == Severity::Error),
        "type check failed: {:?}",
        tc.diagnostics
    );
    let artifacts = lower_amir(db, file);
    assert!(
        !artifacts.amir.funcs.is_empty(),
        "surface program did not reach AMIR: {:?}",
        artifacts.type_check.diagnostics
    );
    let mut values: Vec<i128> = artifacts
        .amir
        .literal_pool
        .entries
        .iter()
        .filter_map(|e| match e {
            AmirLiteralEntry::Int(s) => parse_int_literal(s.as_str()),
            _ => None,
        })
        .collect();
    values.sort_unstable();
    values
}

#[test]
fn mem_sizeof_alignof_fold_with_target_pointer_width() {
    let mut db = DatabaseImpl::new();
    db.new_file("stdlib/core/mem.aru".into(), MEM_MODULE.into());
    let file = db.new_file("main_mem.aru".into(), MAIN_WITH_MEM_INTRINSICS.into());

    // ILP32 natural layout: usize=4, u64=8, usize align=4.
    db.set_target_config(DataLayout::ptr_width(4));
    assert_eq!(
        folded_int_literals(&db, file),
        vec![4, 8],
        "mem.sizeOf/alignOf must fold to 32-bit layout constants"
    );

    db.set_target_config(DataLayout::ptr_width(8));
    assert_eq!(
        folded_int_literals(&db, file),
        vec![8],
        "mem.sizeOf/alignOf must fold to 64-bit layout constants"
    );
}

#[test]
fn mem_intrinsics_preserve_non_natural_abi_alignments_in_runtime_and_ctfe() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_middle::types::FunctionInstance;
    use arandu_mir::ctfe::Budget;
    use arandu_query::ctfe::{ctfe_eval, ctfe_eval_instance, CtfeInstanceRequest, CtfeRequest};
    use arandu_query::passes::declaration_signatures;
    use arandu_query::runtime::Instance;

    let mut db = DatabaseImpl::new();
    db.new_file("stdlib/core/mem.aru".into(), MEM_MODULE.into());
    let source = "import std.core.mem as mem\nstruct Pair { tag: u8, value: u64 }\nfunc main(): usize { return mem.alignOf<Pair>() }\n";
    let file = db.new_file("main.aru".into(), source.into());
    let public_file = db.new_file("public_layout.aru".into(), "import std.core.mem as mem\nstruct Pair { tag: u8, value: u64 }\nfunc main(): usize { return comptime mem.alignOf<Pair>() }".into());
    let definition = declaration_signatures(&db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "main")
        .expect("main definition")
        .id;
    let budget = Budget {
        fuel: 10_000,
        frames: 16,
        values: 1_000,
    };
    for (layout, alignment) in [
        (DataLayout::ptr_width(4), 8),
        (DataLayout::i686_sysv(), 4),
        (DataLayout::ptr_width(8), 8),
        (DataLayout::i686_sysv(), 4),
    ] {
        db.set_target_config(layout);
        assert_eq!(folded_int_literals(&db, file), vec![alignment]);
        assert_eq!(folded_int_literals(&db, public_file), vec![alignment]);
        let source_result = ctfe_eval(&db, CtfeRequest::new(&db, file, definition, vec![], budget));
        let instance = Instance::new(
            &db,
            file,
            FunctionInstance {
                definition,
                arguments: vec![],
            },
        );
        let concrete_result =
            ctfe_eval_instance(&db, CtfeInstanceRequest::new(&db, instance, vec![], budget));
        for (mode, result) in [("source", source_result), ("concrete", concrete_result)] {
            let ConstValue::Integer(value) = result
                .as_ref()
                .unwrap_or_else(|error| panic!("{mode} layout CTFE: {error:?}"))
            else {
                panic!("expected an alignment integer");
            };
            assert_eq!(value.value(), alignment);
        }
    }
}

#[test]
fn imported_layout_intrinsics_do_not_depend_on_sibling_bodies() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_query::ctfe::{ctfe_eval, CtfeRequest};
    use salsa::Setter;

    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let module = format!("{MEM_MODULE}\nfunc sibling(): int {{ return 1 }}");
    let memory = db.new_file("stdlib/core/mem.aru".into(), module.clone());
    let file = db.new_file(
        "main.aru".into(),
        "import std.core.mem as mem\nfunc main(): usize { return mem.sizeOf<u64>() }".into(),
    );
    let symbol = arandu_query::passes::declaration_signatures(&db, file)
        .symbols
        .iter()
        .find(|symbol| symbol.name == "main")
        .expect("main")
        .id;
    let budget = arandu_mir::ctfe::Budget {
        fuel: 10_000,
        frames: 16,
        values: 1_000,
    };
    let first = ctfe_eval(&db, CtfeRequest::new(&db, file, symbol, vec![], budget)).clone();
    assert!(matches!(first, Ok(ConstValue::Integer(value)) if value.value() == 8));
    log.clear();
    memory
        .set_text(&mut db)
        .to(module.replace("return 1", "return 999").into());
    let next = ctfe_eval(&db, CtfeRequest::new(&db, file, symbol, vec![], budget));
    assert_eq!(&first, next);
    for query in ["ctfe_func_amir", "ctfe_extern_hir", "ctfe_eval"] {
        assert_eq!(
            log.count_executions_matching(query),
            0,
            "{query}: {}",
            log.format_chain(true)
        );
    }
}
