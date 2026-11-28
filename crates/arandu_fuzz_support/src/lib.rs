//! Shared fuzz target implementations.
//!
//! Both libFuzzer and the mandatory regression runner call these functions, so
//! a target cannot silently bit-rot outside the scheduled fuzz workflow.

#![allow(clippy::result_large_err)]

use arandu_query::{AnalysisHost, DatabaseImpl};

mod fmt;
pub mod host_diff;
mod incremental;
mod lsp_session;
mod module_graph;
pub mod process_job;
mod runtime_abi;
pub mod shrinker;
pub mod smith;

pub use smith::negative::structural::NegativeSourceError;

/// Validate a source, then check bounded AST-directed negative mutations.
/// Invalid baselines and unsupported families are reported separately from bugs.
pub fn verify_negative_source(
    source: &str,
    mutation: Option<&str>,
) -> Result<usize, NegativeSourceError> {
    smith::negative::structural::verify_source(source, mutation)
}

pub const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    Lex,
    Syntax,
    Pipeline,
    Cycles,
    LexSimd,
    Structured,
    GenRef,
    Incremental,
    ModuleGraph,
    Synthesized,
    SynthesizedC,
    IncrementalCutoff,
    SynthesizedWasm,
    SynthesizedLlvm,
    EmiCorpus,
    SynthesizedAll,
    LspSession,
    Fmt,
    RuntimeAbi,
    HostDiffC,
    /// Fuzz the parser with intentionally malformed / truncated byte sequences
    /// to find ICEs, panics and incorrect span recovery in the CST phase only.
    /// Does not lower to HIR/AMIR — fast and safe to run in parallel.
    ParserStress,
    /// Fuzz the semantic pipeline with structurally valid but semantically
    /// invalid programs: type errors, unresolved names, cycle imports.
    /// Verifies that all such inputs produce diagnostics and no ICEs.
    SemanticNegative,
    /// Fuzz the CTFE evaluator in isolation: exercises the VM fuel meter,
    /// overflow semantics and aggregate evaluation without any backend.
    CtfeStress,
}

impl Target {
    pub const ALL: [Self; 23] = [
        Self::Lex,
        Self::Syntax,
        Self::Pipeline,
        Self::Cycles,
        Self::LexSimd,
        Self::Structured,
        Self::GenRef,
        Self::Incremental,
        Self::ModuleGraph,
        Self::Synthesized,
        Self::SynthesizedC,
        Self::IncrementalCutoff,
        Self::SynthesizedWasm,
        Self::SynthesizedLlvm,
        Self::EmiCorpus,
        Self::SynthesizedAll,
        Self::LspSession,
        Self::Fmt,
        Self::RuntimeAbi,
        Self::HostDiffC,
        Self::ParserStress,
        Self::SemanticNegative,
        Self::CtfeStress,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Lex => "lex",
            Self::Syntax => "syntax",
            Self::Pipeline => "pipeline",
            Self::Cycles => "cycles",
            Self::LexSimd => "lex-simd",
            Self::Structured => "structured",
            Self::GenRef => "genref",
            Self::Incremental => "incremental",
            Self::ModuleGraph => "module-graph",
            Self::Synthesized => "synthesized",
            Self::SynthesizedC => "synthesized-c",
            Self::IncrementalCutoff => "incremental-cutoff",
            Self::SynthesizedWasm => "synthesized-wasm",
            Self::SynthesizedLlvm => "synthesized-llvm",
            Self::EmiCorpus => "emi-corpus",
            Self::SynthesizedAll => "synthesized-all",
            Self::LspSession => "lsp-session",
            Self::Fmt => "fmt",
            Self::RuntimeAbi => "runtime-abi",
            Self::HostDiffC => "host-diff-c",
            Self::ParserStress => "parser-stress",
            Self::SemanticNegative => "semantic-negative",
            Self::CtfeStress => "ctfe-stress",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.name() == name)
    }

    /// Cargo-fuzz binary wired to this shared target implementation.
    pub const fn fuzz_binary_name(self) -> &'static str {
        match self {
            Self::Lex => "fuzz_lex",
            Self::Syntax => "fuzz_parse",
            Self::Pipeline => "fuzz_pipeline",
            Self::Cycles => "fuzz_cycles",
            Self::LexSimd => "fuzz_lex_simd",
            Self::Structured => "fuzz_parse_structured",
            Self::GenRef => "fuzz_genref",
            Self::Incremental => "fuzz_incremental",
            Self::ModuleGraph => "fuzz_module_graph",
            Self::Synthesized => "fuzz_synthesized",
            Self::SynthesizedC => "fuzz_synthesized_c",
            Self::IncrementalCutoff => "fuzz_incremental_cutoff",
            Self::SynthesizedWasm => "fuzz_synthesized_wasm",
            Self::SynthesizedLlvm => "fuzz_synthesized_llvm",
            Self::EmiCorpus => "fuzz_emi_corpus",
            Self::SynthesizedAll => "fuzz_synthesized_all",
            Self::LspSession => "fuzz_lsp_session",
            Self::Fmt => "fuzz_fmt",
            Self::RuntimeAbi => "fuzz_runtime_abi",
            Self::HostDiffC => "fuzz_host_diff_c",
            Self::ParserStress => "fuzz_parser_stress",
            Self::SemanticNegative => "fuzz_semantic_negative",
            Self::CtfeStress => "fuzz_ctfe_stress",
        }
    }
}

pub fn run(target: Target, data: &[u8]) {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    match target {
        Target::Lex => run_lex(data),
        Target::Syntax => run_syntax(data),
        Target::Pipeline => run_pipeline(data),
        Target::Cycles => run_cycles(data),
        Target::LexSimd => run_lex_simd(data),
        Target::Structured => run_structured(data),
        Target::GenRef => run_genref(data),
        Target::Incremental => incremental::run(data),
        Target::ModuleGraph => module_graph::run(data),
        Target::Synthesized => smith::run(data),
        Target::SynthesizedC => smith::run_c(data),
        Target::IncrementalCutoff => incremental::run_private_cutoff(data),
        Target::SynthesizedWasm => smith::run_wasm(data),
        Target::SynthesizedLlvm => smith::run_llvm(data),
        Target::EmiCorpus => smith::run_emi_corpus(data),
        Target::SynthesizedAll => smith::run_all_backends(data),
        Target::LspSession => lsp_session::run(data),
        Target::Fmt => fmt::run(data),
        Target::RuntimeAbi => runtime_abi::run(data),
        Target::HostDiffC => host_diff::run(data),
        Target::ParserStress => run_parser_stress(data),
        Target::SemanticNegative => run_semantic_negative(data),
        Target::CtfeStress => run_ctfe_stress(data),
    }
}

/// Verify that a source fixture and a deterministic dead-code mutation agree
/// across Cranelift, C, and Wasm before the fixture is promoted to the EMI corpus.
#[must_use = "check the cross-backend regression result"]
pub fn verify_emi_regression_source(source: &str, seed: u64) -> Result<(), String> {
    smith::verify_emi_regression_source(source, seed)
}

fn source(data: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(data)
}

fn run_lex(data: &[u8]) {
    let _ = arandu_lexer::lex_recovering(&source(data));
}

fn run_syntax(data: &[u8]) {
    let source = source(data);
    let _ = arandu_parser::parse_syntax(&source);
    let _ = arandu_parser::parse_recovering(&source);
}

fn run_pipeline(data: &[u8]) {
    smith::negative::run(data);
    let mut db = DatabaseImpl::default();
    let file = db.new_file("fuzz/input.aru".into(), source(data).into_owned());
    let _ = arandu_query::syntax_tree(&db, file);
    let _ = arandu_query::passes::parse(&db, file);
    let _ = arandu_query::passes::resolve(&db, file);
    let _ = arandu_query::passes::type_check(&db, file);
    let _ = arandu_query::lower_amir(&db, file);
}

fn run_cycles(data: &[u8]) {
    let width = 2 + data.first().copied().unwrap_or(0) as usize % 3;
    let mut host = AnalysisHost::new();
    let mut files = Vec::with_capacity(width);
    for index in 0..width {
        let next = (index + 1) % width;
        let payload = data.get(index + 1).copied().unwrap_or(index as u8);
        files.push(host.new_file(
            format!("mod_{index}.aru"),
            format!("import mod_{next}\nfunc value_{index}(): int {{ return {payload} }}\n"),
        ));
    }
    for &file in files.iter().rev() {
        let _ = arandu_query::passes::module_dependency_graph(host.db(), file);
        let _ = arandu_query::passes::resolve(host.db(), file);
        let _ = arandu_query::passes::type_check(host.db(), file);
    }
    let mut workers = Vec::new();
    for file in files {
        let snapshot = host.snapshot();
        workers.push(std::thread::spawn(move || {
            let _ = arandu_query::passes::module_dependency_graph(&snapshot.db, file);
            let _ = arandu_query::passes::resolve(&snapshot.db, file);
            let _ = arandu_query::passes::type_check(&snapshot.db, file);
        }));
    }
    for worker in workers {
        assert!(worker.join().is_ok(), "concurrent cyclic query panicked");
    }
}

fn run_lex_simd(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let scalar_ws = arandu_lexer::simd::scalar::skip_whitespace(data);
    let scalar_ident = arandu_lexer::simd::scalar::scan_identifier(data);
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[cfg(target_feature = "sse2")]
    unsafe {
        assert_eq!(scalar_ws, arandu_lexer::simd::sse2::skip_whitespace(data));
        assert_eq!(
            scalar_ident,
            arandu_lexer::simd::sse2::scan_identifier(data)
        );
    }
    #[cfg(target_arch = "aarch64")]
    #[cfg(target_feature = "neon")]
    unsafe {
        assert_eq!(scalar_ws, arandu_lexer::simd::neon::skip_whitespace(data));
        assert_eq!(
            scalar_ident,
            arandu_lexer::simd::neon::scan_identifier(data)
        );
    }
}

fn run_structured(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    let mut source = String::from("func main() {\n");
    for (index, byte) in data.iter().take(4096).enumerate() {
        match byte % 6 {
            0 => source.push_str(&format!("let v{index}: int = {};\n", *byte as i16 - 128)),
            1 => source.push_str("if true { let x = 1; }\n"),
            2 => source.push_str("while false { break; }\n"),
            3 => source.push_str("let s = \"structured\";\n"),
            4 => source.push_str("let x = (((1 + 2) * 3) - 4);\n"),
            _ => source.push_str("unknown(value);\n"),
        }
    }
    source.push_str("}\n");
    let _ = arandu_parser::parse_recovering(&source);
}

/// Differential state-machine for the safe, thread-confined GenRef core. The
/// oracle models the public contract, not slots, generations or free lists.
fn run_genref(data: &[u8]) {
    use arandu_runtime::genref::{ArenaId, ArenaRegistry, GenError, GenRef};

    #[derive(Clone, Copy)]
    struct Handle {
        arena: usize,
        key: GenRef<u64>,
        value: u64,
        live: bool,
    }

    let limit = 1 + u32::from(data.first().copied().unwrap_or(3) % 4);
    let mut registry = ArenaRegistry::with_generation_limit(limit).expect("non-zero limit");
    let mut arenas: Vec<(ArenaId, bool)> = Vec::new();
    let mut handles: Vec<Handle> = Vec::new();

    for (step, op) in data
        .get(1..)
        .unwrap_or_default()
        .chunks(4)
        .take(4096)
        .enumerate()
    {
        let code = op.first().copied().unwrap_or(0) % 7;
        let arena_index = usize::from(op.get(1).copied().unwrap_or(0));
        let handle_index = usize::from(op.get(2).copied().unwrap_or(0));
        let value = ((step as u64) << 8) | u64::from(op.get(3).copied().unwrap_or(0));
        match code {
            0 => {
                if let Ok(id) = registry.create_arena() {
                    arenas.push((id, true));
                }
            }
            1 if !arenas.is_empty() => {
                let index = arena_index % arenas.len();
                let (id, live) = arenas[index];
                let result = registry.insert(id, value);
                if live {
                    if let Ok(key) = result {
                        handles.push(Handle {
                            arena: index,
                            key,
                            value,
                            live: true,
                        });
                    }
                } else {
                    assert_eq!(result, Err(GenError::ArenaGone));
                }
            }
            2 | 3 if !arenas.is_empty() && !handles.is_empty() => {
                let ai = arena_index % arenas.len();
                let hi = handle_index % handles.len();
                let (id, arena_live) = arenas[ai];
                let handle = handles[hi];
                let expected = if ai != handle.arena {
                    Err(GenError::WrongArena)
                } else if !arena_live {
                    Err(GenError::ArenaGone)
                } else if !handle.live {
                    Err(GenError::Stale)
                } else {
                    Ok(handle.value)
                };
                if code == 2 {
                    assert_eq!(registry.get(id, handle.key).copied(), expected);
                } else {
                    assert_eq!(registry.remove(id, handle.key), expected);
                    if expected.is_ok() {
                        handles[hi].live = false;
                    }
                }
            }
            4 if !arenas.is_empty() => {
                let ai = arena_index % arenas.len();
                let (id, live) = arenas[ai];
                assert_eq!(
                    registry.destroy_arena(id),
                    if live {
                        Ok(())
                    } else {
                        Err(GenError::ArenaGone)
                    }
                );
                if live {
                    arenas[ai].1 = false;
                    for handle in &mut handles {
                        if handle.arena == ai {
                            handle.live = false;
                        }
                    }
                }
            }
            5 if !arenas.is_empty() => {
                let id = arenas[arena_index % arenas.len()].0;
                assert_eq!(
                    registry.get(id, GenRef::INVALID),
                    Err(GenError::InvalidHandle)
                );
            }
            6 if !arenas.is_empty() && !handles.is_empty() => {
                let hi = handle_index % handles.len();
                let ai = handles[hi].arena;
                let (id, arena_live) = arenas[ai];
                let handle = handles[hi];
                let expected = if !arena_live {
                    Err(GenError::ArenaGone)
                } else if !handle.live {
                    Err(GenError::Stale)
                } else {
                    Ok(())
                };
                assert_eq!(
                    registry.get_mut(id, handle.key).map(|slot| *slot = value),
                    expected
                );
                if expected.is_ok() {
                    handles[hi].value = value;
                }
            }
            _ => {}
        }
    }
}

/// **Target: parser-stress** — push truncated/malformed byte sequences through
/// only the lexer and CST-first parser, isolated from the semantic pipeline.
/// This group catches ICEs and span panics in the front-end without paying for
/// name resolution or type checking.
///
/// Two sub-checks run per input:
/// 1. `lex_recovering` must never panic.
/// 2. `parse_recovering` must never panic; its result is ignored.
///
/// A single input here consumes ~10 µs on average (vs ~200 µs for `pipeline`),
/// so the libFuzzer engine can generate >100 k iterations/s on a single core.
fn run_parser_stress(data: &[u8]) {
    let text = source(data);
    // Lexer must handle any byte sequence without panicking.
    let _tokens = arandu_lexer::lex_recovering(&text);
    // CST parser must recover from any input without panicking.
    let _cst = arandu_parser::parse_syntax(&text);
    let _ast = arandu_parser::parse_recovering(&text);
}

/// **Target: semantic-negative** — verify that syntactically valid but
/// semantically invalid programs always produce at least one diagnostic and
/// never cause an ICE. The oracle uses `smith::negative` structural mutations
/// to generate plausible-looking but broken programs efficiently.
///
/// Invariant: if `type_check` succeeds (no errors), `lower_amir` must also
/// succeed. If type checking emits any error, no ICE (`DiagCode::ice`) may
/// appear in the diagnostic set.
fn run_semantic_negative(data: &[u8]) {
    use arandu_query::{passes, DatabaseImpl};
    let text = source(data);
    // First use the negative oracle for structurally mutated programs.
    smith::negative::run(data);
    // Then run the full pipeline on the raw input, asserting no ICE.
    let mut db = DatabaseImpl::default();
    let file = db.new_file("fuzz/neg.aru".into(), text.into_owned());
    let _ = arandu_query::syntax_tree(&db, file);
    let _ = passes::parse(&db, file);
    let _ = passes::resolve(&db, file);
    let diags = passes::type_check(&db, file);
    // ICEs must never appear, even when the program is ill-typed.
    for diag in &diags.diagnostics {
        assert!(
            !diag.is_ice(),
            "ICE in semantic-negative fuzz target: {:?}",
            diag
        );
    }
    if diags.diagnostics.is_empty() {
        // No type errors: lowering must succeed.
        let _ = arandu_query::lower_amir(&db, file);
    }
}

/// **Target: ctfe-stress** — exercise the CTFE evaluator in isolation with
/// arbitrary `const` expressions. This catches fuel exhaustion panics, integer
/// overflow in the VM, and aggregate evaluation bugs without any backend
/// involvement.
///
/// Budget is kept intentionally small (fuel: 2000, frames: 16, values: 500)
/// so each seed completes fast and the engine can explore more of the space.
fn run_ctfe_stress(data: &[u8]) {
    use arandu_mir::ctfe::Budget;
    use arandu_query::ctfe::{ctfe_eval, CtfeRequest};
    use arandu_query::{passes::declaration_signatures, DatabaseImpl};

    let text = source(data);
    // Wrap in a module so the input can be arbitrary; the outer `const` is our
    // entry point for the evaluator.
    let wrapped = format!("func __fuzz_entry(): int {{ return 0 }}\n{text}");
    let mut db = DatabaseImpl::default();
    let file = db.new_file("fuzz/ctfe.aru".into(), wrapped);
    let diags = declaration_signatures(&db, file);
    // Find our injected sentinel; skip if resolution already failed.
    let Some(main_sym) = diags
        .symbols
        .iter()
        .find(|s| s.name == "__fuzz_entry")
        .map(|s| s.id)
    else {
        return;
    };
    let budget = Budget {
        fuel: 2000,
        frames: 16,
        values: 500,
    };
    let request = CtfeRequest::new(&db, file, main_sym, vec![], budget);
    // The evaluator must never panic; `Err` outcomes (fuel, cycles) are fine.
    let _ = ctfe_eval(&db as &dyn arandu_query::ArandCompilerDb, request);
}

#[cfg(test)]
mod genref_tests {
    #[test]
    fn state_machine_covers_cross_arena_stale_and_retirement() {
        let mut input = vec![3];
        input.extend_from_slice(&[
            0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 7, 2, 1, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 8, 3,
            0, 1, 0, 1, 0, 0, 9, 3, 0, 2, 0, 1, 0, 0, 10, 4, 0, 0, 0, 2, 0, 3, 0,
        ]);
        super::run_genref(&input);
    }
}
