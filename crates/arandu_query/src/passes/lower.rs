use crate::db::HashEq;
use crate::{ArandCompilerDb, SourceFile};
use arandu_middle::Diagnostic;
use arandu_semantics::{amir::AmirProgram, TypeCheckResult};
use salsa::Accumulator;

/// AMIR plus the **post-monomorphize** [`TypeCheckResult`] used to build it.
///
/// Codegen must use `type_check` from this bundle — monomorphization allocates
/// new symbols that do not exist on the pre-mono `type_check` query alone.
/// Returning both from one Salsa query avoids the old CLI double path
/// (local HIR+mono + `lower_amir` HIR+mono) while keeping symbol tables aligned.
#[derive(Debug, Clone)]
pub struct LowerAmirArtifacts {
    pub amir: AmirProgram,
    pub type_check: TypeCheckResult,
    /// Doc comments from the same resolution this program was lowered from.
    /// Consumers that describe the source (the generated C header) read them
    /// here instead of resolving again outside the Salsa graph.
    pub docs: std::sync::Arc<arandu_middle::DocCommentMap>,
}

/// Shared canonical HIR/monomorphization stage. Its conservative fingerprint
/// records the typed source inputs, not Debug output or incidental addresses.
/// Narrow public projections (`borrow_interfaces`) provide semantic cutoff.
#[derive(Debug)]
pub struct PreparedHir {
    pub hir: Option<arandu_middle::hir::HirProgram>,
    pub type_check: TypeCheckResult,
    pub diagnostics: Vec<Diagnostic>,
    pub(crate) source_fingerprint: blake3::Hash,
}

/// Canonical borrow contracts isolated from the function bodies that proved
/// them. Consumers use this narrow query so Salsa can cut propagation when an
/// implementation edit leaves the public dependency relation unchanged.
#[derive(Debug, Clone)]
pub struct BorrowInterfaces {
    pub entries: Vec<(
        arandu_middle::SymbolId,
        arandu_middle::types::ReturnBorrowSummary,
    )>,
    /// Concrete contracts use structural keys, never synthetic symbol numbers
    /// allocated in another producer's context.
    pub instances: Vec<(
        arandu_middle::types::FunctionInstance,
        arandu_middle::types::ReturnBorrowSummary,
    )>,
}

/// Collect transitive imports of `root`, lower each to HIR, and link into `hir`.
///
/// ## Why `file_typing` (not `type_check`)
///
/// Salsa accumulators bubble: calling `type_check` on an import from inside
/// `lower_amir(entry)` would re-accumulate the import's body diagnostics into
/// `lower_amir::accumulated(entry)`, so `check` of a clean entry would fail on
/// unrelated stdlib residuals (e.g. `std.alloc`). Body typeck for the link path
/// must not accumulate or request callee flow contracts: [`file_typing`] uses
/// the same canonical body checker, without the compatibility composition.
///
/// Skips cycles, missing modules (prelude-only), parse failures, and modules
/// that cannot lower (`lower_to_hir` error). Import body errors stay on that
/// module's own `type_check` query when the user checks that file directly.
fn link_imported_hir_modules(
    db: &dyn ArandCompilerDb,
    root: SourceFile,
    type_check_result: &mut TypeCheckResult,
    hir: &mut arandu_semantics::hir::HirProgram,
    fingerprint: &mut blake3::Hasher,
) {
    let mut visited = std::collections::HashSet::new();
    visited.insert(*root.file_id(db));

    // Explicit heap stack. The root is visited first and is not linked here;
    // each import is fingerprinted and linked after its own imports (post-order).
    struct ImportFrame {
        file: SourceFile,
        children: Vec<SourceFile>,
        paths: Vec<String>,
        index: usize,
        entered: bool,
    }

    let mut stack = vec![ImportFrame {
        file: root,
        children: Vec::new(),
        paths: Vec::new(),
        index: 0,
        entered: false,
    }];

    while let Some(top) = stack.len().checked_sub(1) {
        if !stack[top].entered {
            stack[top].entered = true;
            let file = stack[top].file;
            let program_res = super::resolution::parse(db, file);
            if let Ok(program) = &**program_res {
                let mut children = Vec::new();
                let mut paths = Vec::new();
                for import in &program.imports {
                    db.unwind_if_revision_cancelled();
                    let Some(path) = arandu_resolve::canonicalize_import_path(import) else {
                        continue;
                    };
                    let Some(imported_file) = db.as_source_db().resolve_module_path(&path) else {
                        // Prelude-only or missing file — nothing to lower.
                        continue;
                    };
                    let imported_id = *imported_file.file_id(db);
                    if !visited.insert(imported_id) {
                        continue;
                    }
                    children.push(imported_file);
                    paths.push(path);
                }
                stack[top].children = children;
                stack[top].paths = paths;
            }
        }

        if stack[top].index < stack[top].children.len() {
            let child = stack[top].children[stack[top].index];
            stack.push(ImportFrame {
                file: child,
                children: Vec::new(),
                paths: Vec::new(),
                index: 0,
                entered: false,
            });
            continue;
        }

        let finished = stack.pop().expect("import frame");
        if stack.is_empty() {
            break;
        }
        let path = {
            let parent = stack.last_mut().expect("import parent");
            let path = parent.paths[parent.index].clone();
            parent.index += 1;
            path
        };

        let imported_file = finished.file;
        let imported_parse = super::resolution::parse(db, imported_file);
        fingerprint.update(crate::StableHash::stable_hash(&**imported_parse).as_bytes());
        let Ok(imported_program) = &**imported_parse else {
            continue;
        };

        // Full body typeck without accumulating diags into the entry pipeline.
        let imported_tc_arc = super::typing::file_typing(db, imported_file);
        fingerprint.update(crate::StableHash::stable_hash(&**imported_tc_arc).as_bytes());
        // Skip modules with hard type errors — signatures already merged via
        // module_signatures; codegen for those bodies is not required for
        // entry check when the entry only references public signatures.
        if imported_tc_arc
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, arandu_middle::Severity::Error))
        {
            tracing::debug!(
                target: "arandu_query",
                %path,
                "skip HIR link for import (body typeck has errors)"
            );
            continue;
        }
        let mut imported_tc = (**imported_tc_arc).clone();

        match arandu_semantics::lower_to_hir(&mut imported_tc, imported_program) {
            Ok(temp_hir) => {
                tracing::debug!(
                    target: "arandu_query",
                    %path,
                    decls = temp_hir.decls.len(),
                    "linking imported HIR module"
                );
                arandu_semantics::link_hir_module(type_check_result, hir, &imported_tc, &temp_hir);
            }
            Err(_) => {
                tracing::debug!(
                    target: "arandu_query",
                    %path,
                    "skip HIR link for import (lower_to_hir failed)"
                );
            }
        }
    }
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "prepare_hir", file = ?file.file_id(db),
))]
pub fn prepare_hir(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<PreparedHir> {
    let program_res = super::resolution::parse(db, file);
    let type_check_result_arc = super::typing::file_typing(db, file);
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"PreparedHir/sources/v1");
    fingerprint.update(crate::StableHash::stable_hash(&**program_res).as_bytes());
    fingerprint.update(crate::StableHash::stable_hash(&**type_check_result_arc).as_bytes());

    // Clone for mutation: lower_to_hir / monomorphize update symbols + type_info.
    // Arc fields are O(1); mono may Arc::make_mut type_info once.
    let mut type_check_result = (**type_check_result_arc).clone();

    let mut hir = {
        arandu_base::time_pass!("lower-hir");
        match &**program_res {
            Ok(program) => match arandu_semantics::lower_to_hir(&mut type_check_result, program) {
                Ok(h) => h,
                Err(diags) => {
                    return HashEq::new(PreparedHir {
                        hir: None,
                        type_check: type_check_result,
                        diagnostics: diags,
                        source_fingerprint: fingerprint.finalize(),
                    });
                }
            },
            Err(_) => {
                return HashEq::new(PreparedHir {
                    hir: None,
                    type_check: type_check_result,
                    diagnostics: Vec::new(),
                    source_fingerprint: fingerprint.finalize(),
                });
            }
        }
    };

    // Multi-file HIR: lower imported modules and append their function bodies so
    // monomorphize + codegen see real definitions (not just merged signatures).
    {
        arandu_base::time_pass!("link-hir-imports");
        link_imported_hir_modules(db, file, &mut type_check_result, &mut hir, &mut fingerprint);
    }

    {
        arandu_base::time_pass!("monomorphize");
        db.unwind_if_revision_cancelled();
        if let Err(diags) = arandu_semantics::passes::monomorphize::monomorphize_program(
            &mut type_check_result,
            &mut hir,
        ) {
            return HashEq::new(PreparedHir {
                hir: None,
                type_check: type_check_result,
                diagnostics: diags,
                source_fingerprint: fingerprint.finalize(),
            });
        }
    }
    db.unwind_if_revision_cancelled();

    HashEq::new(PreparedHir {
        hir: Some(hir),
        type_check: type_check_result,
        diagnostics: Vec::new(),
        source_fingerprint: fingerprint.finalize(),
    })
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "lower_amir", file = ?file.file_id(db),
))]
pub fn lower_amir(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<LowerAmirArtifacts> {
    // Executable assembly consumes independently checked instances. Share the
    // composed result instead of cloning the program into a compatibility memo.
    let composed = crate::runtime::runtime_program(db, file);
    for diagnostic in &composed.diagnostics {
        arandu_middle::db::DiagnosticsAccumulator(diagnostic.clone()).accumulate(db);
    }
    // The artifacts are part of the composed program, whose identity already
    // determines them.
    let identity = crate::StableHash::stable_hash(composed);
    #[cfg(any(test, debug_assertions))]
    crate::fingerprint::verify(
        db,
        "lower_amir",
        identity,
        crate::StableHash::stable_hash(&*composed.artifacts),
    );
    HashEq::with_identity(std::sync::Arc::clone(&composed.artifacts), identity)
}

#[salsa::tracked]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(
    query = "borrow_interfaces",
    file = ?file.file_id(db),
))]
pub fn borrow_interfaces(db: &dyn ArandCompilerDb, file: SourceFile) -> HashEq<BorrowInterfaces> {
    use crate::StableHash;
    let declarations = super::signatures::declaration_signatures(db, file);
    let parsed = super::resolution::parse(db, file);
    let mut summaries = declarations.type_info.return_borrow_summaries.clone();
    let mut concrete = rustc_hash::FxHashMap::default();
    if let Ok(program) = &**parsed {
        for symbol in arandu_semantics::free_func_symbols(program, &declarations.resolved) {
            db.unwind_if_revision_cancelled();
            // Uninstantiated templates retain declaration contracts only.
            // Their executable proof belongs to each concrete instance.
            if declarations.type_info.generic_params.contains_key(&symbol) {
                continue;
            }
            let carries_borrow =
                declarations
                    .type_info
                    .decl_type(symbol)
                    .is_some_and(|ty| match ty {
                        arandu_middle::types::ArType::Func(_, result) => declarations
                            .type_info
                            .borrow_paths(result)
                            .map_or(true, |paths| !paths.is_empty()),
                        _ => false,
                    });
            if !carries_borrow {
                continue;
            }
            let instance = crate::runtime::Instance::new(
                db,
                file,
                arandu_middle::types::FunctionInstance {
                    definition: symbol,
                    arguments: Vec::new(),
                },
            );
            let contracts = crate::runtime::instance_contracts(db, instance);
            // Failed source recovery never certifies the callee. Runtime/IDE
            // units own and report its error; keep declaration recovery here.
            if !contracts.diagnostics.is_empty() {
                continue;
            }
            for (key, summary) in &contracts.entries {
                if key.arguments.is_empty() {
                    summaries.insert(key.definition, summary.clone());
                } else {
                    concrete.insert(key.clone(), summary.clone());
                }
            }
        }
    }
    let mut entries = summaries
        .into_iter()
        .filter(|(_, summary)| !summary.dependencies.is_empty())
        .collect::<Vec<_>>();
    entries.sort_by_key(|(symbol, _)| (symbol.file_id, symbol.local_id.0));
    let mut instances = concrete
        .into_iter()
        .filter(|(_, summary)| !summary.dependencies.is_empty())
        .collect::<Vec<_>>();
    instances.sort_by_key(|(key, _)| {
        (
            key.definition.file_id,
            key.definition.local_id.0,
            *key.stable_hash().as_bytes(),
        )
    });
    HashEq::new(BorrowInterfaces { entries, instances })
}

#[salsa::tracked]
pub fn module_dependency_graph(
    db: &dyn ArandCompilerDb,
    root: SourceFile,
) -> HashEq<petgraph::Graph<u32, ()>> {
    use petgraph::Graph;
    let mut graph = Graph::new();
    let mut visited = std::collections::HashMap::new();

    fn walk(
        db: &dyn ArandCompilerDb,
        file: SourceFile,
        graph: &mut Graph<u32, ()>,
        visited: &mut std::collections::HashMap<u32, petgraph::graph::NodeIndex>,
    ) -> petgraph::graph::NodeIndex {
        let file_id = *file.file_id(db.as_source_db());
        if let Some(&node) = visited.get(&file_id) {
            return node;
        }

        let node = graph.add_node(file_id);
        visited.insert(file_id, node);

        let program_res = crate::passes::parse(db, file);
        if let Ok(program) = &**program_res {
            for import in &program.imports {
                if let Some(path) = arandu_resolve::canonicalize_import_path(import) {
                    if let Some(imported_file) = db.as_source_db().resolve_module_path(&path) {
                        let imported_node = walk(db, imported_file, graph, visited);
                        graph.add_edge(node, imported_node, ());
                    }
                }
            }
        }

        node
    }

    walk(db, root, &mut graph, &mut visited);
    HashEq::new(graph)
}
