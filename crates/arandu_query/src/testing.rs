//! Incremental discovery of compiler-validated Arandu tests.

use std::sync::Arc;

use crate::passes::{item_source_input, module_signatures, parse};
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::{NodeKey, SymbolId};
use arandu_parser::TopLevelDecl;
use arandu_semantics::testing::{BenchmarkCase, TestCase};

#[cfg(any(test, debug_assertions))]
pub static ITEM_TEST_CASE_EXEC_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(any(test, debug_assertions))]
pub static ITEM_BENCHMARK_CASE_EXEC_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn declaration_matches(
    decl: &TopLevelDecl,
    resolved: &arandu_resolve::ResolvedNames,
    item_sym: SymbolId,
) -> bool {
    arandu_semantics::primary_def_key(decl)
        .is_some_and(|key| resolved.definitions.get(&key) == Some(&item_sym))
        || matches!(
            decl,
            TopLevelDecl::Extern(ext)
                if ext.members.iter().any(|member| {
                    resolved.definitions.get(&NodeKey::from(member.span)) == Some(&item_sym)
                })
        )
}

/// Discover one valid `@Test` case. The dependency is content-addressed by
/// the owning item, so edits to siblings do not execute this query again.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_test_case",
    file = ?file.file_id(db),
    item = ?item_sym,
))]
pub fn item_test_case(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> Option<TestCase> {
    #[cfg(any(test, debug_assertions))]
    ITEM_TEST_CASE_EXEC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let item = item_source_input(db, file, item_sym);
    let signatures = module_signatures(db, file);
    for decl_id in &item.program.decls {
        let decl = item.program.pool.decl(*decl_id);
        if !declaration_matches(decl, signatures.resolved.as_ref(), item_sym) {
            continue;
        }
        let annotations =
            arandu_semantics::attributes::validate_decl_attributes(decl, &item.program.pool);
        return arandu_semantics::testing::validate_test_case(
            decl,
            &annotations,
            item_sym,
            signatures.type_info.as_ref(),
        )
        .case;
    }
    None
}

/// Ordered semantic test surface for one source module.
#[salsa::tracked]
pub fn file_test_manifest(db: &dyn ArandCompilerDb, file: SourceFile) -> Arc<Vec<TestCase>> {
    let parsed = parse(db, file);
    let Ok(program) = parsed.as_ref() else {
        return Arc::new(Vec::new());
    };
    let signatures = module_signatures(db, file);
    let symbols = arandu_semantics::body_item_symbols(program, signatures.resolved.as_ref());
    let mut cases = symbols
        .into_iter()
        .filter_map(|symbol| item_test_case(db, file, symbol).clone())
        .collect::<Vec<_>>();
    cases.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.symbol.local_id.0.cmp(&right.symbol.local_id.0))
    });
    Arc::new(cases)
}

/// Discover one valid `@Benchmark` case with item-granular dependencies.
#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "item_benchmark_case",
    file = ?file.file_id(db),
    item = ?item_sym,
))]
pub fn item_benchmark_case(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    item_sym: SymbolId,
) -> Option<BenchmarkCase> {
    #[cfg(any(test, debug_assertions))]
    ITEM_BENCHMARK_CASE_EXEC_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let item = item_source_input(db, file, item_sym);
    let signatures = module_signatures(db, file);
    for decl_id in &item.program.decls {
        let decl = item.program.pool.decl(*decl_id);
        if !declaration_matches(decl, signatures.resolved.as_ref(), item_sym) {
            continue;
        }
        let annotations =
            arandu_semantics::attributes::validate_decl_attributes(decl, &item.program.pool);
        return arandu_semantics::testing::validate_benchmark_case(
            decl,
            &annotations,
            item_sym,
            signatures.type_info.as_ref(),
            signatures.symbols.as_ref(),
        )
        .case;
    }
    None
}

/// Ordered semantic benchmark surface for one source module.
#[salsa::tracked]
pub fn file_benchmark_manifest(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
) -> Arc<Vec<BenchmarkCase>> {
    let parsed = parse(db, file);
    let Ok(program) = parsed.as_ref() else {
        return Arc::new(Vec::new());
    };
    let signatures = module_signatures(db, file);
    let symbols = arandu_semantics::body_item_symbols(program, signatures.resolved.as_ref());
    let mut cases = symbols
        .into_iter()
        .filter_map(|symbol| item_benchmark_case(db, file, symbol).clone())
        .collect::<Vec<_>>();
    cases.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.symbol.local_id.0.cmp(&right.symbol.local_id.0))
    });
    Arc::new(cases)
}
