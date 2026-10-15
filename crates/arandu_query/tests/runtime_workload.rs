//! Informational producer benchmark; not a shared-CI latency assertion.
#![allow(clippy::expect_used)]

mod common;

use arandu_query::{db::HashEq, passes, runtime, ArandCompilerDb, DatabaseImpl, SourceFile};
use salsa::Setter;
use std::sync::Arc;
use std::time::Instant;

// Reproduce the legacy tracked boundary, not just its pure lowering on every
// call. Both producers must get an honest memo hit in the warm measurement.
#[salsa::tracked]
fn legacy_program(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> HashEq<passes::LowerAmirArtifacts> {
    let checked = passes::type_check(db, file);
    let prepared = passes::prepare_hir(db, file);
    assert!(
        prepared.diagnostics.is_empty(),
        "{:?}",
        prepared.diagnostics
    );
    let mut context = prepared.type_check.clone();
    context.type_info_mut().return_borrow_summaries.extend(
        checked
            .type_info
            .return_borrow_summaries
            .iter()
            .map(|(&symbol, summary)| (symbol, summary.clone())),
    );
    let (amir, diagnostics) = arandu_semantics::lower_to_amir_with_interfaces(
        &mut context,
        prepared.hir.as_ref().expect("valid legacy HIR"),
        db.target_config().data_layout(db).pointer_width(),
    )
    .expect("valid legacy MIR");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    HashEq::new(passes::LowerAmirArtifacts {
        amir,
        type_check: context,
    })
}

#[test]
#[ignore = "run legacy and units in separate processes; see architecture docs"]
fn runtime_producer_workload() {
    let legacy = std::env::var("ARANDU_WORKLOAD_PRODUCER").is_ok_and(|value| value == "legacy");
    let mut source =
        String::from("import std.alloc.vec as vec\nfunc identity<T>(x: T): T { return x }\n");
    for index in 0..32 {
        source.push_str(&format!("func worker{index}(): int {{ let mut values = vec.Vec<int>.new() values.push({index}) return identity<int>({index}) }}\n"));
    }
    source.push_str("func main(): int { return worker0() }\n");
    let (mut db, log) = DatabaseImpl::with_rebuild_log();
    for &(path, text) in common::STDLIB_MODULES {
        db.new_file(path.into(), text.into());
    }
    let file = db.new_file("main.aru".into(), source.clone());
    let produce = |stage: &str| {
        log.clear();
        let start = Instant::now();
        let count = if legacy {
            legacy_program(&db, file).amir.funcs.len()
        } else {
            let composed = runtime::runtime_program(&db, file);
            assert!(
                composed.diagnostics.is_empty(),
                "{:?}",
                composed.diagnostics
            );
            composed.artifacts.amir.funcs.len()
        };
        assert!(count >= 34);
        eprintln!(
            "producer={} stage={stage} elapsed_us={} functions={count} raw_units={} final_units={}",
            if legacy { "legacy" } else { "units" },
            start.elapsed().as_micros(),
            log.count_executions_matching("runtime_raw_unit"),
            log.count_executions_matching("runtime_unit")
        );
    };
    produce("cold");
    produce("warm");
    if !legacy && std::env::var_os("ARANDU_WORKLOAD_COMPOSE_ONLY").is_some() {
        let composed = runtime::runtime_program(&db, file);
        let units = composed
            .instances
            .iter()
            .filter(|(symbol, _)| {
                composed
                    .artifacts
                    .amir
                    .funcs
                    .iter()
                    .any(|function| function.symbol == *symbol)
            })
            .map(|(_, key)| {
                let source = db
                    .source_file_by_id(key.definition.file_id)
                    .expect("registered definition");
                (
                    key,
                    runtime::runtime_unit(&db, runtime::Instance::new(&db, source, key.clone())),
                )
            })
            .collect::<Vec<_>>();
        let inputs = units
            .iter()
            .map(|(key, unit)| arandu_mir::ContextualFunctionUnit {
                key,
                unit: unit.result.as_ref().expect("valid body"),
                context: &unit.context,
                generated_symbols: &unit.generated_symbols,
                instances: &unit.instances,
                borrow_summary: unit.borrow_summary.as_ref().expect("validated summary"),
            })
            .collect::<Vec<_>>();
        let mut aggregate = runtime::declaration_hir(&db, file)
            .artifacts
            .type_check
            .clone();
        let start = Instant::now();
        arandu_mir::compose_function_units(&mut aggregate, &inputs, || {})
            .expect("canonical composition");
        eprintln!(
            "producer=units stage=composition_only elapsed_us={}",
            start.elapsed().as_micros()
        );
    }
    file.set_text(&mut db).to(Arc::from(
        source.replace("return identity<int>(0)", "return identity<int>(1)"),
    ));
    log.clear();
    let start = Instant::now();
    if legacy {
        assert!(legacy_program(&db, file).amir.funcs.len() >= 34);
    } else {
        let composed = runtime::runtime_program(&db, file);
        assert!(
            composed.diagnostics.is_empty(),
            "{:?}",
            composed.diagnostics
        );
        assert_eq!(log.count_executions_matching("runtime_raw_unit"), 1);
        assert_eq!(log.count_executions_matching("runtime_unit"), 1);
        assert_eq!(log.count_executions_matching("function_hir"), 1);
    }
    eprintln!(
        "producer={} stage=body_edit elapsed_us={} raw_units={} final_units={}",
        if legacy { "legacy" } else { "units" },
        start.elapsed().as_micros(),
        log.count_executions_matching("runtime_raw_unit"),
        log.count_executions_matching("runtime_unit")
    );
    eprintln!("{}", log.format_chain(false));
}
