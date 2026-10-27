#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A valid module chain must exercise compilation, caching and body cutoff.
//! Timings are informational; query executions are the deterministic oracle.

use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{parse, type_check};
use salsa::Setter;
use std::sync::Arc;
use std::time::Instant;

fn module_source(index: usize, value: usize) -> String {
    let mut text = String::new();
    if index > 0 {
        text.push_str(&format!("import mod_{}\n", index - 1));
    }
    for function in 0..10 {
        text.push_str(&format!(
            "public func func_{function}(): int {{ return {value} }}\n"
        ));
        if index > 0 {
            text.push_str(&format!(
                "public func call_prev_{function}(): int {{ return mod_{}.func_{function}() }}\n",
                index - 1,
            ));
        }
    }
    text
}

#[test]
fn test_salsa_phases_performance() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let files: Vec<_> = (0..50)
        .map(|index| db.new_file(format!("mod_{index}.aru"), module_source(index, index)))
        .collect();

    let cold = Instant::now();
    // Demand the entire chain from its importing end, then validate every
    // source and body: type_check alone need not expose a parsing failure.
    for &file in files.iter().rev() {
        assert!(parse(&db, file).is_ok(), "performance corpus must parse");
        let checked = type_check(&db, file);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    }
    let cold_duration = cold.elapsed();
    assert!(log.count_executions_matching("item_local_typing") >= 50 * 10);

    log.clear();
    let hot = Instant::now();
    for &file in files.iter().rev() {
        let checked = type_check(&db, file);
        assert!(checked.diagnostics.is_empty());
    }
    let hot_duration = hot.elapsed();
    assert_eq!(
        log.counts().executed,
        0,
        "unchanged inputs must use cached queries"
    );

    log.clear();
    // Edit the existing Salsa input. Re-registering a new FileId would test
    // replacement of the registry, not invalidation of existing dependencies.
    files[0]
        .set_text(&mut db)
        .to(Arc::from(module_source(0, 9999)));
    let checked = type_check(&db, files[0]);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    assert_eq!(
        log.count_executions_matching("item_local_typing"),
        10,
        "the ten edited bodies must actually be checked"
    );
    log.clear();
    let edit = Instant::now();
    for &file in files[1..].iter().rev() {
        let checked = type_check(&db, file);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    }
    let edit_duration = edit.elapsed();
    assert_eq!(
        log.count_executions_matching("item_local_typing"),
        0,
        "a dependency body edit must not recheck importers' bodies"
    );
    println!(
        "50 valid modules: cold={cold_duration:?}, cached={hot_duration:?}, dependency body edit={edit_duration:?}"
    );
}

/// Measure the cost behind the public summary without changing query boundaries.
#[test]
#[ignore = "informational borrowed-return summary workload"]
fn borrow_interface_workload_measurement() {
    use arandu_query::borrow_interfaces;
    use std::fmt::Write;

    fn source(edited: bool) -> String {
        let mut text = String::from("module loans\n");
        for index in 0..64 {
            let extra = match (index, edited) {
                (0, true) => "let marker = 1\n",
                (0, false) => "let marker = 0\n",
                _ => "",
            };
            writeln!(
                text,
                "public func loan_{index}(value: ref int): ref int {{ {extra}return value }}"
            )
            .unwrap();
        }
        text
    }

    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("loans.aru".into(), source(false));
    assert!(parse(&db, file).is_ok());
    let start = Instant::now();
    let initial = borrow_interfaces(&db, file);
    assert_eq!(initial.entries.len(), 64);
    let initial = arandu_query::db::HashEq::share(initial);
    let cold = start.elapsed();
    log.clear();
    let start = Instant::now();
    assert_eq!(borrow_interfaces(&db, file).entries, initial.entries);
    let cached = start.elapsed();
    assert_eq!(log.counts().executed, 0);

    file.set_text(&mut db).to(Arc::from(source(true)));
    log.clear();
    let start = Instant::now();
    let updated = borrow_interfaces(&db, file);
    let edited = start.elapsed();
    assert_eq!(updated.entries, initial.entries);
    let checked = type_check(&db, file);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    println!("64 borrowed-return functions: cold={cold:?} cached={cached:?} body_edit={edited:?}");
    println!("{}", log.format_chain(true));
}
