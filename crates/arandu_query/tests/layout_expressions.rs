#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{ctfe::ConstValue, layout::DataLayout, DiagCode, Severity};
use arandu_query::{ctfe, passes, runtime, DatabaseImpl, SourceFile};
use salsa::Setter;

fn frozen(db: &DatabaseImpl, file: SourceFile) -> Vec<u64> {
    let symbol = passes::resolved_headers(db, file)
        .declarations
        .symbols
        .iter()
        .find(|s| s.name == "main")
        .expect("main")
        .id;
    let typed = ctfe::item_staged_typing(db, file, symbol);
    assert!(
        !typed
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error),
        "{:?}",
        typed.diagnostics
    );
    let mut values: Vec<_> = typed.type_info.ctfe_values.iter().collect();
    values.sort_by_key(|(span, _)| span.start);
    values
        .into_iter()
        .map(|(_, (value, _))| match value {
            ConstValue::Integer(n) => u64::try_from(n.value()).expect("layout integer"),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

#[test]
fn layout_uses_the_full_target_abi_not_the_compiler_host() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("layout.aru".into(), "func main(): usize { let size = comptime (@sizeOf(i64)); let align = comptime (@alignOf(i64)); let word = comptime (@sizeOf(usize)); return size + align + word }".into());
    db.set_target_config(DataLayout::ptr_width(8));
    assert_eq!(frozen(&db, file), [8, 8, 8]);
    db.set_target_config(DataLayout::i686_sysv());
    assert_eq!(frozen(&db, file), [8, 4, 4]);
    assert!(runtime::runtime_program(&db, file).diagnostics.is_empty());
}

#[test]
fn layout_materializes_generic_operands_before_amir() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("layout.aru".into(), "func size<T>(): usize { return @sizeOf(T) }\nfunc main(): usize { return size<int>() + size<[3]u16>() }".into());
    let output = runtime::runtime_program(&db, file);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
}

#[test]
fn layout_is_available_to_pre_body_selection_and_const_arguments() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("layout.aru".into(), "func count<comptime N: uint>(): uint { return N }\nfunc main(): uint { comptime if @alignOf(i64) == 4 { return count<comptime (@sizeOf(usize))>() } else { return count<comptime (@sizeOf([3]u16))>() } }".into());
    for layout in [DataLayout::ptr_width(8), DataLayout::i686_sysv()] {
        db.set_target_config(layout);
        let typed = passes::type_check(&db, file);
        assert!(
            !typed
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error),
            "{:?}",
            typed.diagnostics
        );
        let output = runtime::runtime_program(&db, file);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    }
}

#[test]
fn ordinary_generic_functions_named_like_intrinsics_keep_their_body() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("layout.aru".into(), "func sizeOf<T>(): usize { return 99 }\nfunc alignOf<T>(): usize { return 77 }\nfunc main(): usize { return sizeOf<int>() + alignOf<int>() + @sizeOf(int) }".into());
    let output = runtime::runtime_program(&db, file);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(
        output
            .artifacts
            .amir
            .funcs
            .iter()
            .find(|f| output.artifacts.type_check.symbols.get(f.symbol).name == "main")
            .expect("main")
            .stmts
            .payloads
            .iter()
            .filter(|s| matches!(s, arandu_middle::amir::AmirStmt::Call { .. }))
            .count()
            == 2
    );
}

#[test]
fn nominal_layout_edits_match_clean_evaluation() {
    let mut db = DatabaseImpl::new();
    let source =
        "struct Pair { a: u16, b: byte }\nfunc main(): usize { return comptime (@sizeOf(Pair)) }";
    let file = db.new_file("layout.aru".into(), source.into());
    assert_eq!(frozen(&db, file), [4]);
    let edited = source.replace("a: u16", "a: u64");
    file.set_text(&mut db).to(edited.clone().into());
    assert_eq!(frozen(&db, file), [16]);
    let mut clean = DatabaseImpl::new();
    let clean_file = clean.new_file("layout.aru".into(), edited);
    assert_eq!(frozen(&db, file), frozen(&clean, clean_file));
}

#[test]
fn invalid_layout_is_a_user_diagnostic_not_an_ice() {
    for (source, expected) in [
        ("func main(): usize { return @sizeOf([18446744073709551615]u64) }", DiagCode::T047InvalidTypeLayout),
        ("struct Recursive { child: Recursive }\nfunc main(): usize { return @sizeOf(Recursive) }", DiagCode::T029RecursiveStructInfiniteSize),
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("layout.aru".into(), source.into());
        let output = runtime::runtime_program(&db, file);
        assert!(output.diagnostics.iter().any(|d| d.code == expected), "{:?}", output.diagnostics);
        assert!(!output.diagnostics.iter().any(|d| d.code.as_str().starts_with("ICE")), "{:?}", output.diagnostics);
    }
}

#[test]
fn layout_expressions_are_function_tokens_not_declaration_annotations() {
    let mut db = DatabaseImpl::new();
    let source = "@test\nfunc sample(): void { let n = @sizeOf(int); let a = @alignOf(i64) }";
    let file = db.new_file("layout.aru".into(), source.into());
    let highlights = arandu_query::file_highlights(&db, file);
    for name in ["sizeOf", "alignOf"] {
        let start = u32::try_from(source.find(name).expect("intrinsic name")).expect("offset");
        assert!(highlights
            .iter()
            .any(|token| token.start == start && token.kind == arandu_query::HlKind::Function));
    }
    assert!(highlights
        .iter()
        .any(|token| token.start == 1 && token.kind == arandu_query::HlKind::Decorator));
}

#[test]
fn layout_constants_are_available_to_a_scalar_root() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "layout.aru".into(),
        "const WIDTH usize = @sizeOf(usize)\nfunc main(): usize { return comptime WIDTH }".into(),
    );
    db.set_target_config(DataLayout::ptr_width(4));
    assert_eq!(frozen(&db, file), [4]);
}
