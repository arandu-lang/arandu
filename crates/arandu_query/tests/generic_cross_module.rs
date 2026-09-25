#![allow(clippy::expect_used)]

use arandu_middle::amir::{AmirOperand, AmirStmt};
use arandu_middle::{Severity, SymbolKind};
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::lower_amir;

#[test]
fn identical_cross_module_instantiations_share_one_definition() {
    let mut db = DatabaseImpl::new();
    db.new_file(
        "generic.aru".into(),
        r#"module generic
public func identity<T>(value: T): T { return value }
"#
        .into(),
    );
    db.new_file(
        "left.aru".into(),
        r#"module left
import generic
public func leftValue(): int { return generic.identity<int>(20) }
"#
        .into(),
    );
    db.new_file(
        "right.aru".into(),
        r#"module right
import generic
public func rightValue(): int { return generic.identity<int>(22) }
"#
        .into(),
    );
    let root = db.new_file(
        "main.aru".into(),
        r#"module app
import left
import right
func main(): int { return left.leftValue() + right.rightValue() }
"#
        .into(),
    );

    let artifacts = lower_amir(&db, root);
    let accumulated =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, root);
    let diagnostics: Vec<_> = accumulated.iter().map(|diagnostic| &diagnostic.0).collect();
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "cross-module generic lowering failed: {diagnostics:?}"
    );

    let specialized_symbols: Vec<_> = artifacts
        .type_check
        .symbols
        .iter()
        .filter(|symbol| {
            symbol.kind == SymbolKind::Func && symbol.name.starts_with("_A$generic.identity$I_int_")
        })
        .map(|symbol| symbol.id)
        .collect();
    assert_eq!(
        specialized_symbols.len(),
        1,
        "one concrete type must produce one linked specialization"
    );
    let specialized = specialized_symbols[0];
    assert_eq!(
        artifacts
            .amir
            .funcs
            .iter()
            .filter(|function| function.symbol == specialized)
            .count(),
        1,
        "the specialization must have exactly one AMIR definition"
    );

    let call_count = artifacts
        .amir
        .funcs
        .iter()
        .flat_map(|function| function.stmts.payloads.raw.iter())
        .filter(|statement| {
            matches!(
                statement,
                AmirStmt::Call {
                    callee: AmirOperand::FunctionRef(symbol),
                    ..
                } if *symbol == specialized
            )
        })
        .count();
    assert_eq!(
        call_count, 2,
        "both modules must call the shared definition"
    );
}
