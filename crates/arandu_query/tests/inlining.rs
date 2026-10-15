#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_middle::amir::{AmirConstant, AmirOperand, AmirRvalue, AmirStmt};
use arandu_mir::optimize_amir_checked_with_level;
use arandu_mir::pass_manager::OptLevel;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{lower_amir, type_check};

const SOURCE: &str = r#"
func isHorizontalWhitespace(c: int): bool {
    return c == 32 || c == 9 || c == 13
}

func test_call(c: int): bool {
    return isHorizontalWhitespace(c)
}

func test_const(): bool {
    return isHorizontalWhitespace(32)
}

struct Stats {
    code: int
    comment: int
    blank: int
}

func finishLine(
    stats: mut ref Stats,
    hasNonWhitespace: bool,
    hasCode: bool,
    hasComment: bool,
): void {
    if !hasNonWhitespace {
        stats.blank = stats.blank + 1
    } else if hasCode {
        stats.code = stats.code + 1
    } else if hasComment {
        stats.comment = stats.comment + 1
    } else {
        stats.code = stats.code + 1
    }
}

func test_finish(stats: mut ref Stats): void {
    finishLine(stats, true, false, true)
}
"#;

#[test]
fn o2_self_tail_recursion_uses_a_separate_loop_header() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("tail.aru".into(), "func rotate(n: int, a: int, b: int): int { if n == 0 { return a }; return rotate(n - 1, b, a) }".into());
    let artifacts = lower_amir(&db, file);
    assert!(
        artifacts.type_check.diagnostics.is_empty(),
        "{:?}",
        artifacts.type_check.diagnostics
    );
    let mut program = artifacts.amir.clone();
    let symbols = &artifacts.type_check.symbols;
    let interner = &artifacts.type_check.type_info.type_interner;
    optimize_amir_checked_with_level(&mut program, symbols, interner, OptLevel::O2).unwrap();
    let function = program
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "rotate")
        .unwrap();
    assert!(!function.stmts.payloads.iter().any(|stmt| matches!(stmt, AmirStmt::Call { callee: AmirOperand::FunctionRef(symbol), .. } if *symbol == function.symbol)));
    assert!(function
        .blocks
        .iter()
        .any(|block| function.block_params(block.params).len() == 3));
    assert!(function
        .stmts
        .iter_ids()
        .all(|id| function.stmts.kinds[id] == function.stmt(id).kind()));
}

#[test]
fn o2_scalarizes_mutable_copy_fields_across_a_join() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("fields.aru".into(), "struct Pair { x: int; y: int } func fields(flag: bool, n: int): int { let mut pair = Pair { x: n, y: n + 1 }; if flag { pair.x = n + 2 }; return pair.x + pair.y }".into());
    let artifacts = lower_amir(&db, file);
    assert!(
        artifacts.type_check.diagnostics.is_empty(),
        "{:?}",
        artifacts.type_check.diagnostics
    );
    let mut program = artifacts.amir.clone();
    let symbols = &artifacts.type_check.symbols;
    let function = program
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "fields")
        .unwrap();
    let local = function
        .locals
        .iter()
        .find(|local| {
            local
                .symbol
                .is_some_and(|symbol| symbols.get(symbol).name == "pair")
        })
        .unwrap()
        .id;
    assert!(function.stmts.payloads.iter().any(|stmt| matches!(stmt, AmirStmt::Assign { rhs: AmirRvalue::Load(place), .. } if place.local == local)));
    let original_temps = function.temps.len();
    let field_reads = function
        .stmts
        .payloads
        .iter()
        .filter(|stmt| {
            matches!(
                stmt,
                AmirStmt::Assign {
                    rhs: AmirRvalue::FieldAccess { .. },
                    ..
                }
            )
        })
        .count();
    optimize_amir_checked_with_level(
        &mut program,
        symbols,
        &artifacts.type_check.type_info.type_interner,
        OptLevel::O2,
    )
    .unwrap();
    let function = program
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "fields")
        .unwrap();
    assert!(!function.stmts.payloads.iter().any(|stmt| matches!(stmt, AmirStmt::Assign { rhs: AmirRvalue::Load(place), .. } if place.local == local)));
    assert!(
        function.temps.len() <= original_temps + field_reads,
        "snapshot capture must grow with actual field reads"
    );
}

#[test]
fn leaf_function_is_inlined_at_o1() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("inlining_test.aru".into(), SOURCE.into());

    let tc = type_check(&db, file);
    assert!(
        tc.diagnostics.is_empty(),
        "type check failed: {:?}",
        tc.diagnostics
    );

    let artifacts = lower_amir(&db, file);
    let symbols = artifacts.type_check.symbols.clone();
    let type_info = artifacts.type_check.type_info.clone();

    // At O0, test_call has a Call statement
    let amir_o0 = artifacts.amir.clone();
    let test_call_o0 = amir_o0
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "test_call")
        .expect("test_call not found");
    let has_call_o0 = test_call_o0
        .stmts
        .payloads
        .iter()
        .any(|s| matches!(s, AmirStmt::Call { .. }));
    assert!(has_call_o0, "At O0, Call statement must be preserved");

    // At O1, leaf inlining runs and eliminates the Call statement
    let mut amir_o1 = artifacts.amir.clone();
    optimize_amir_checked_with_level(
        &mut amir_o1,
        &symbols,
        &type_info.type_interner,
        OptLevel::O1,
    )
    .expect("AMIR optimization at O1 must succeed");

    let test_call_o1 = amir_o1
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "test_call")
        .expect("test_call not found");
    let has_call_o1 = test_call_o1
        .stmts
        .payloads
        .iter()
        .any(|s| matches!(s, AmirStmt::Call { .. }));
    assert!(
        !has_call_o1,
        "At O1, leaf function isHorizontalWhitespace must be inlined into test_call"
    );

    let test_finish_o1 = amir_o1
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "test_finish")
        .expect("test_finish not found");
    let finish_line_o0 = artifacts
        .amir
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "finishLine")
        .expect("finishLine not found");
    assert!(
        !test_finish_o1
            .stmts
            .payloads
            .iter()
            .any(|stmt| matches!(stmt, AmirStmt::Call { .. })),
        "At O1, a bounded branch-only leaf must be inlined into test_finish (callee blocks: {})",
        finish_line_o0.blocks.len()
    );

    // In test_const, inlining + SCCP should fold the result to constant true
    let test_const_o1 = amir_o1
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "test_const")
        .expect("test_const not found");
    let has_call_const = test_const_o1
        .stmts
        .payloads
        .iter()
        .any(|s| matches!(s, AmirStmt::Call { .. }));
    assert!(
        !has_call_const,
        "test_const must not have any Call statements"
    );

    let folds_to_true = test_const_o1.stmts.payloads.iter().any(|s| {
        matches!(
            s,
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                ..
            }
        )
    });
    assert!(
        folds_to_true,
        "test_const() calling isHorizontalWhitespace(32) must fold to true"
    );
}
