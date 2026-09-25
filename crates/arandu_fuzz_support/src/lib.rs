//! Shared fuzz target implementations.
//!
//! Both libFuzzer and the mandatory regression runner call these functions, so
//! a target cannot silently bit-rot outside the scheduled fuzz workflow.

use arandu_query::{AnalysisHost, DatabaseImpl};

mod incremental;
mod lsp_session;
mod module_graph;
pub mod process_job;
pub mod shrinker;
mod smith;

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
    EmiCorpus,
    SynthesizedAll,
    LspSession,
}

impl Target {
    pub const ALL: [Self; 16] = [
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
        Self::EmiCorpus,
        Self::SynthesizedAll,
        Self::LspSession,
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
            Self::EmiCorpus => "emi-corpus",
            Self::SynthesizedAll => "synthesized-all",
            Self::LspSession => "lsp-session",
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
            Self::EmiCorpus => "fuzz_emi_corpus",
            Self::SynthesizedAll => "fuzz_synthesized_all",
            Self::LspSession => "fuzz_lsp_session",
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
        Target::EmiCorpus => smith::run_emi_corpus(data),
        Target::SynthesizedAll => smith::run_all_backends(data),
        Target::LspSession => lsp_session::run(data),
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
