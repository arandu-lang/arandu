#![allow(clippy::expect_used, clippy::panic)]

use arandu_middle::{ctfe::ConstValue, DiagCode, SymbolId};
use arandu_query::{
    ctfe::{global_const_value, MAX_QUERY_DEPENDENCY_DEPTH},
    passes::seed_header_signatures,
    DatabaseImpl, SourceFile,
};
use salsa::Setter;

fn constant(db: &DatabaseImpl, file: SourceFile, name: &str) -> SymbolId {
    assert!(
        arandu_query::passes::parse(db, file).is_ok(),
        "{:?}",
        arandu_query::passes::parse(db, file).as_ref().err()
    );
    let headers = seed_header_signatures(db, file);
    headers
        .symbols
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| {
            panic!(
                "constant {name}: {:?}; symbols: {:?}",
                headers.diagnostics,
                headers
                    .symbols
                    .iter()
                    .map(|entry| (&entry.name, entry.kind))
                    .collect::<Vec<_>>()
            )
        })
        .id
}

#[test]
fn long_acyclic_global_dependencies_fail_with_a_bounded_diagnostic_and_recover() {
    let mut source = String::new();
    for index in 0..MAX_QUERY_DEPENDENCY_DEPTH + 4 {
        source.push_str(&format!("const C{index} = comptime (C{} + 0)\n", index + 1));
    }
    source.push_str(&format!(
        "const C{} = comptime 42\n",
        MAX_QUERY_DEPENDENCY_DEPTH + 4
    ));
    let mut db = DatabaseImpl::new();
    let file = db.new_file("long_dependencies.aru".into(), source);
    let symbol = constant(&db, file, "C0");
    let failed = global_const_value(&db, file, symbol);
    let diagnostics = failed.result.as_ref().expect_err("query depth ceiling");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::T045ComptimeLimitExceeded),
        "{diagnostics:?}"
    );
    assert!(diagnostics
        .iter()
        .all(|diagnostic| !diagnostic.code.as_str().starts_with("ICE")));
    assert_eq!(
        diagnostics,
        global_const_value(&db, file, symbol)
            .result
            .as_ref()
            .expect_err("deterministic cached failure")
    );
    file.set_text(&mut db).to("const C0 = comptime 42".into());
    let recovered = global_const_value(&db, file, constant(&db, file, "C0"));
    assert!(
        matches!(&recovered.result.as_ref().expect("recovery").value, ConstValue::Integer(value) if value.value() == 42)
    );
}

#[test]
fn contextual_keys_do_not_turn_global_cycles_into_depth_failures() {
    for source in [
        "const A = comptime A",
        "const A = comptime B\nconst B = comptime A",
        "const A = comptime helper()\nfunc helper(): int { return A }",
        "const A = comptime helper<1>()\nfunc helper<comptime N: uint>(): int { return A }",
        "const A = comptime helper()\nfunc helper(): int { comptime if A == 42 { return 42 } else { return 0 } }",
    ] {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("cycle.aru".into(), source.into());
        let failed = global_const_value(&db, file, constant(&db, file, "A"));
        let diagnostics = failed.result.as_ref().expect_err("cycle");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagCode::T044ComptimeEvaluationFailed),
            "{source}: {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != DiagCode::T045ComptimeLimitExceeded),
            "{diagnostics:?}"
        );
    }
}

#[test]
fn discarded_static_branches_do_not_enter_the_dependency_path() {
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("discarded.aru".into(), "const A = comptime helper()\nconst B = comptime A\nfunc helper(): int { comptime if false { return B }; return 42 }".into());
    let frozen = global_const_value(&db, file, constant(&db, file, "A"));
    assert!(
        matches!(&frozen.result.as_ref().expect("discarded dependency").value, ConstValue::Integer(value) if value.value() == 42)
    );
    assert_eq!(
        log.count_executions_matching("global_const_value_in_context"),
        1,
        "{}",
        log.format_chain(false)
    );
}

#[test]
fn dependency_depth_survives_generic_helpers_static_conditions_and_nominal_headers() {
    for route in ["helper", "generic", "condition", "header"] {
        let mut source = String::new();
        for index in 0..MAX_QUERY_DEPENDENCY_DEPTH + 2 {
            let next = index + 1;
            match route {
                "helper" => source.push_str(&format!(
                    "const C{index} = comptime helper{index}()\nfunc helper{index}(): int {{ return C{next} }}\n"
                )),
                "generic" => source.push_str(&format!(
                    "const C{index} = comptime helper{index}<1>()\nfunc helper{index}<comptime N: uint>(): int {{ return C{next} }}\n"
                )),
                "condition" => source.push_str(&format!(
                    "const C{index} = comptime helper{index}()\nfunc helper{index}(): int {{ comptime if C{next} == 42 {{ return 42 }} else {{ return 0 }} }}\n"
                )),
                "header" => source.push_str(&format!(
                    "const C{index} = comptime @sizeOf(S{index})\nstruct S{index} {{ bytes: [comptime (C{next})]u8 }}\n"
                )),
                _ => unreachable!(),
            }
        }
        source.push_str(&format!(
            "const C{} = comptime 42\n",
            MAX_QUERY_DEPENDENCY_DEPTH + 2
        ));
        let mut db = DatabaseImpl::new();
        let file = db.new_file(format!("{route}.aru"), source);
        let failed = global_const_value(&db, file, constant(&db, file, "C0"));
        let diagnostics = failed.result.as_ref().expect_err(route);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagCode::T045ComptimeLimitExceeded),
            "{route}: {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.code.as_str().starts_with("ICE")),
            "{route}: {diagnostics:?}"
        );
    }
}
