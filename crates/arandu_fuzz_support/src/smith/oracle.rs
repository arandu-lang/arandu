//! Multi-backend differential testing oracles and execution triangulators.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::LazyLock;

use arandu_query::AnalysisHost;

use super::artifact::{
    artifact_root, catch_backend_panic, oracle_target_name, panic_payload_message,
    shrink_and_confirm, write_failure_artifact, Failure, FailureArtifact, TemporaryArtifacts,
    AUTOMATIC_SHRINK_BUDGET, NEXT_TEMP_ID,
};
use super::emi::{check_emi_pair_with_expected, inject_dead_pure_statement, inject_emi_mutation};
use super::process::{
    capture_jit_eprint, capture_jit_println, captured_stderr, parse_backend_result,
    read_result_channel, reset_jit_stdout_capture, run_with_timeout, synthesized_args_arg,
    synthesized_args_len, take_jit_stderr_capture, take_jit_stdout_capture,
    BACKEND_PROCESS_TIMEOUT,
};
use super::synth::synthesize_with_oracle;
use super::types::SYNTHESIZED_PROGRAM_ARGS;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackendObservation {
    pub result: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpectedObservation {
    pub result: i32,
    pub stdout: &'static [u8],
    pub stderr: &'static [u8],
}

impl ExpectedObservation {
    pub fn synthesized(result: i32) -> Self {
        if result % 2 == 0 {
            Self {
                result,
                stdout: b"result-even\n",
                stderr: b"result-even",
            }
        } else {
            Self {
                result,
                stdout: b"result-odd\n",
                stderr: b"result-odd",
            }
        }
    }

    #[allow(dead_code)]
    pub fn known(result: i32, stdout: &'static [u8], stderr: &'static [u8]) -> Self {
        Self {
            result,
            stdout,
            stderr,
        }
    }
}

pub(super) fn run(data: &[u8]) {
    run_with_oracles(data, false, false);
}

pub(super) fn run_c(data: &[u8]) {
    run_with_oracles(data, true, false);
}

pub(super) fn run_wasm(data: &[u8]) {
    run_with_oracles(data, false, true);
}

pub(super) fn run_all_backends(data: &[u8]) {
    run_with_oracles(data, true, true);
}

pub(crate) fn run_with_oracles(data: &[u8], compare_c: bool, compare_wasm: bool) {
    let seed = seed_from_data(data);
    let generated = synthesize_with_oracle(seed);
    run_source_with_expected(
        &generated.source,
        seed,
        compare_c,
        compare_wasm,
        "synthesized",
        Some(ExpectedObservation::synthesized(generated.expected_result)),
    );
}

pub(crate) fn seed_from_data(data: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x100_0000_01b3;

    let Some(prefix) = data.get(..8) else {
        return data.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
        });
    };
    let mut hash = u64::from_le_bytes(prefix.try_into().unwrap_or_default());
    for byte in &data[8..] {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
    }
    hash
}

pub(crate) fn run_source_with_expected(
    text: &str,
    seed: u64,
    compare_c: bool,
    compare_wasm: bool,
    corpus_name: &str,
    expected: Option<ExpectedObservation>,
) {
    match check_emi_pair_with_expected(text, seed, compare_c, compare_wasm, expected) {
        Ok(_) => {}
        Err(failure) if failure.shrinkable => {
            let (minimized, reproduced) =
                shrink_and_confirm(text, AUTOMATIC_SHRINK_BUDGET, |candidate| {
                    check_emi_pair_with_expected(candidate, seed, compare_c, compare_wasm, expected)
                        .is_err_and(|candidate_failure| failure.same_identity(&candidate_failure))
                });
            let source = if reproduced {
                minimized.source.as_str()
            } else {
                text
            };
            let emi_candidate = check_source(source, false, false)
                .ok()
                .and_then(|reference| inject_emi_mutation(source, seed, &reference))
                .map(|(candidate, _)| candidate)
                .or_else(|| inject_dead_pure_statement(source, seed))
                .unwrap_or_else(|| source.to_owned());
            let target = oracle_target_name(compare_c, compare_wasm, corpus_name);
            let artifact = write_failure_artifact(
                &artifact_root(),
                FailureArtifact {
                    target,
                    corpus_name,
                    seed,
                    failure: &failure,
                    source,
                    emi_candidate: &emi_candidate,
                    shrink_attempts: minimized.attempts,
                    shrink_reductions: minimized.reductions,
                    shrink_confirmed: reproduced,
                },
            );
            panic!(
                "{}; target={target}; corpus={corpus_name}; seed={seed}; shrink_attempts={}; shrink_reductions={}; shrink_confirmed={reproduced}; artifact={}\n--- reproducer ---\n{source}\n--- EMI-mutated candidate ---\n{emi_candidate}",
                failure.message,
                minimized.attempts,
                minimized.reductions,
                artifact.as_ref().map_or_else(
                    |error| format!("write-error:{error}"),
                    |path| path.display().to_string()
                )
            );
        }
        Err(failure) => panic!("{}; corpus={corpus_name}; seed={seed}", failure.message),
    }
}

pub(crate) fn check_source(
    source: &str,
    compare_c: bool,
    compare_wasm: bool,
) -> Result<BackendObservation, Failure> {
    check_source_internal(source, compare_c, compare_wasm, false)
        .map(|(observation, _)| observation)
}

pub(crate) fn check_source_with_block_coverage(
    source: &str,
    compare_c: bool,
    compare_wasm: bool,
) -> Result<(BackendObservation, arandu_backend_cranelift::BlockCoverage), Failure> {
    check_source_internal(source, compare_c, compare_wasm, true)
}

fn check_source_internal(
    source: &str,
    compare_c: bool,
    compare_wasm: bool,
    collect_block_coverage: bool,
) -> Result<(BackendObservation, arandu_backend_cranelift::BlockCoverage), Failure> {
    let lowered = lower_validated_source(source)?;

    let mut expected: Option<BackendObservation> = None;
    let mut reference_coverage = arandu_backend_cranelift::BlockCoverage::default();
    let mut optimized_programs = Vec::with_capacity(3);
    let mut cranelift_outputs = Vec::with_capacity(3);

    for level in [
        arandu_mir::OptLevel::O0,
        arandu_mir::OptLevel::O1,
        arandu_mir::OptLevel::O2,
    ] {
        let mut program = lowered.amir.clone();
        let optimization = catch_unwind(AssertUnwindSafe(|| {
            arandu_mir::optimize_amir_checked_with_level(
                &mut program,
                &lowered.type_check.symbols,
                &lowered.type_check.type_info.type_interner,
                level,
            )
        }))
        .map_err(|payload| {
            Failure::new(
                "optimizer-panicked",
                format!(
                    "optimizer panicked at {level:?}: {}",
                    panic_payload_message(payload)
                ),
                true,
            )
            .with_scope(format!("{level:?}"))
        })?;
        optimization.map_err(|diagnostic| {
            Failure::new(
                "optimizer-rejected-valid-amir",
                format!("optimizer {level:?} rejected generated AMIR: {diagnostic:?}"),
                true,
            )
            .with_scope(format!("{level:?}"))
        })?;
        let (output, coverage) = catch_backend_panic("Cranelift", level, || {
            if collect_block_coverage && level == arandu_mir::OptLevel::O0 {
                execute_cranelift_with_block_coverage(
                    &program,
                    &lowered.type_check.symbols,
                    &lowered.type_check.type_info,
                )
            } else {
                execute_cranelift(
                    &program,
                    &lowered.type_check.symbols,
                    &lowered.type_check.type_info,
                )
                .map(|observation| {
                    (
                        observation,
                        arandu_backend_cranelift::BlockCoverage::default(),
                    )
                })
            }
        })?
        .map_err(|error| {
            Failure::new(
                "cranelift-execution-failed",
                format!("Cranelift failed at {level:?}: {error}"),
                true,
            )
            .with_scope(format!("{level:?}"))
        })?;
        if let Some(expected) = &expected {
            if &output != expected {
                return Err(Failure::new(
                    "optimization-result-mismatch",
                    format!("optimization changed observable behavior at {level:?}: O0={expected:?}, {level:?}={output:?}"),
                    true,
                ).with_scope(format!("{level:?}")));
            }
        } else {
            reference_coverage = coverage;
            expected = Some(output.clone());
        }
        optimized_programs.push((level, program));
        cranelift_outputs.push(output);
    }

    if compare_c || compare_wasm {
        for ((level, program), output) in optimized_programs.iter().zip(cranelift_outputs.iter()) {
            if compare_c {
                let c_output = catch_backend_panic("C", *level, || {
                    execute_c(
                        program,
                        &lowered.type_check.symbols,
                        &lowered.type_check.type_info,
                    )
                })?
                .map_err(|error| {
                    let shrinkable = is_backend_failure_reproducible(&error);
                    let kind = backend_failure_kind("C", &error);
                    Failure::new(kind, error, shrinkable).with_scope(format!("{level:?}"))
                })?;
                if &c_output != output {
                    return Err(Failure::new(
                        "cranelift-c-mismatch",
                        format!(
                            "Cranelift/C execution mismatch at {level:?}: Cranelift={output:?}, C={c_output:?}"
                        ),
                        true,
                    ).with_scope(format!("{level:?}")));
                }
            }
            if compare_wasm {
                let wasm_output = catch_backend_panic("Wasm", *level, || {
                    execute_wasm(
                        program,
                        &lowered.type_check.symbols,
                        &lowered.type_check.type_info,
                    )
                })?
                .map_err(|error| {
                    let shrinkable = is_backend_failure_reproducible(&error);
                    let kind = backend_failure_kind("Wasm", &error);
                    Failure::new(kind, error, shrinkable).with_scope(format!("{level:?}"))
                })?;
                if &wasm_output != output {
                    return Err(Failure::new(
                        "cranelift-wasm-mismatch",
                        format!("Cranelift/Wasm execution mismatch at {level:?}: Cranelift={output:?}, Wasm={wasm_output:?}"),
                        true,
                    ).with_scope(format!("{level:?}")));
                }
            }
        }
    }
    expected
        .map(|observation| (observation, reference_coverage))
        .ok_or_else(|| {
            Failure::new(
                "optimizer-produced-no-result",
                "optimization oracle ran without producing a reference result",
                false,
            )
        })
}

fn lower_validated_source(
    source: &str,
) -> Result<arandu_query::db::HashEq<arandu_query::LowerAmirArtifacts>, Failure> {
    match catch_unwind(AssertUnwindSafe(|| lower_validated_source_inner(source))) {
        Ok(result) => result,
        Err(payload) => Err(Failure::new(
            "frontend-panicked",
            format!(
                "compiler panicked while lowering synthesized source: {}",
                panic_payload_message(payload)
            ),
            true,
        )
        .with_scope("frontend")),
    }
}

fn lower_validated_source_inner(
    source: &str,
) -> Result<arandu_query::db::HashEq<arandu_query::LowerAmirArtifacts>, Failure> {
    let mut host = AnalysisHost::new();
    if [
        "import std.alloc.vec",
        "import std.alloc.arena",
        "import std.alloc.bitset",
        "import std.alloc.gen_arena",
        "import std.alloc.hash_map",
        "import std.alloc.smallvec",
        "import std.alloc.string",
        "import std.core.str",
        "import std.core.hash",
        "import std.core.num",
        "import std.core.option",
        "import std.core.result",
        "import std.env",
    ]
    .iter()
    .any(|import| source.contains(import))
    {
        register_fuzz_stdlib(&mut host).map_err(|error| {
            Failure::new(
                "stdlib-setup-failed",
                format!("failed to register fuzz stdlib dependencies: {error}"),
                false,
            )
        })?;
    }
    let file = host.new_file("smith/generated.aru".into(), source.to_owned());
    lower_validated_file(&host, file)
}

pub(crate) fn lower_validated_file(
    host: &AnalysisHost,
    file: arandu_query::SourceFile,
) -> Result<arandu_query::db::HashEq<arandu_query::LowerAmirArtifacts>, Failure> {
    let parsed = arandu_query::passes::parse(host.db(), file);
    if parsed.is_err() {
        let diagnostics = arandu_query::passes::parse_diagnostics(host.db(), file);
        return Err(Failure::new(
            "invalid-generated-source",
            format!(
                "type-directed generator emitted parse diagnostics: {:?}",
                **diagnostics
            ),
            false,
        ));
    }
    let lowered = arandu_query::lower_amir(host.db(), file);
    if !lowered.type_check.diagnostics.is_empty() {
        return Err(Failure::new(
            "invalid-amir-lowering",
            format!(
                "generated program failed AMIR lowering: {:?}",
                lowered.type_check.diagnostics
            ),
            false,
        ));
    }
    let lowering_diagnostics = arandu_query::passes::lower_amir::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(host.db(), file)
    .into_iter()
    .map(|diagnostic| diagnostic.0.clone())
    .filter(|diagnostic| {
        diagnostic.severity == arandu_middle::Severity::Error
            || matches!(
                diagnostic.code,
                arandu_middle::DiagCode::O002MoveWhileBorrowed
                    | arandu_middle::DiagCode::O003MutableBorrowConflict
                    | arandu_middle::DiagCode::O004GenerationalFallback
                    | arandu_middle::DiagCode::O006DestroyWhileBorrowed
                    | arandu_middle::DiagCode::O010EscapeOfBorrowedValue
            )
    })
    .collect::<Vec<_>>();
    if !lowering_diagnostics.is_empty() {
        return Err(Failure::new(
            "invalid-generated-source",
            format!("generated program accumulated lowering diagnostics: {lowering_diagnostics:?}"),
            false,
        ));
    }
    let amir_diagnostics = arandu_middle::amir_validate::validate_amir_program(
        &lowered.amir,
        &lowered.type_check.symbols,
        &lowered.type_check.type_info.type_interner,
    );
    if !amir_diagnostics.is_empty() {
        return Err(Failure::new(
            "invalid-generated-amir",
            format!("generated program produced invalid AMIR: {amir_diagnostics:?}"),
            false,
        ));
    }
    Ok(arandu_query::db::HashEq::share(lowered))
}

type StdlibFiles = (PathBuf, Vec<(String, &'static str)>);

static STDLIB_CACHE: LazyLock<Result<StdlibFiles, String>> = LazyLock::new(|| {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib")
        .canonicalize()
        .map_err(|error| format!("canonicalize stdlib root: {error}"))?;
    let sources: [(&str, &str); 17] = [
        (
            "alloc/bitset.aru",
            include_str!("../../../../stdlib/alloc/bitset.aru"),
        ),
        (
            "alloc/arena.aru",
            include_str!("../../../../stdlib/alloc/arena.aru"),
        ),
        (
            "alloc/gen_arena.aru",
            include_str!("../../../../stdlib/alloc/gen_arena.aru"),
        ),
        (
            "alloc/hash_map.aru",
            include_str!("../../../../stdlib/alloc/hash_map.aru"),
        ),
        (
            "alloc/smallvec.aru",
            include_str!("../../../../stdlib/alloc/smallvec.aru"),
        ),
        (
            "alloc/string.aru",
            include_str!("../../../../stdlib/alloc/string.aru"),
        ),
        (
            "alloc/vec.aru",
            include_str!("../../../../stdlib/alloc/vec.aru"),
        ),
        (
            "core/char.aru",
            include_str!("../../../../stdlib/core/char.aru"),
        ),
        (
            "core/mem.aru",
            include_str!("../../../../stdlib/core/mem.aru"),
        ),
        (
            "core/intrinsics.aru",
            include_str!("../../../../stdlib/core/intrinsics.aru"),
        ),
        (
            "core/hash.aru",
            include_str!("../../../../stdlib/core/hash.aru"),
        ),
        (
            "core/option.aru",
            include_str!("../../../../stdlib/core/option.aru"),
        ),
        (
            "core/num.aru",
            include_str!("../../../../stdlib/core/num.aru"),
        ),
        (
            "core/result.aru",
            include_str!("../../../../stdlib/core/result.aru"),
        ),
        (
            "core/slice.aru",
            include_str!("../../../../stdlib/core/slice.aru"),
        ),
        (
            "core/str.aru",
            include_str!("../../../../stdlib/core/str.aru"),
        ),
        (
            "std/env.aru",
            include_str!("../../../../stdlib/std/env.aru"),
        ),
    ];
    let file_entries = sources
        .into_iter()
        .map(|(relative, content)| (root.join(relative).to_string_lossy().into_owned(), content))
        .collect();
    Ok((root, file_entries))
});

pub(crate) fn register_fuzz_stdlib(host: &mut AnalysisHost) -> Result<(), String> {
    let (root, files) = match &*STDLIB_CACHE {
        Ok(cached) => cached,
        Err(error) => return Err(error.clone()),
    };
    host.db_mut().set_stdlib_root(root.clone());
    for (path, text) in files {
        host.new_file(path.clone(), (*text).to_owned());
    }
    Ok(())
}

pub(crate) fn is_backend_failure_reproducible(error: &str) -> bool {
    error.starts_with("emission diagnostic:")
        || error.starts_with("C compiler ") && error.contains(" exited with ")
        || error.starts_with("generated C program exited with ")
        || error.starts_with("generated C program stdout exceeded the capture limit")
        || error.starts_with("generated C program stderr exceeded the capture limit")
        || error.starts_with("generated C result channel exceeded the 64-byte limit")
        || error.starts_with("generated C result channel emitted invalid UTF-8")
        || error.starts_with("Wasm runner ") && error.contains(" exited with ")
        || error.starts_with("Wasm runner stdout exceeded the capture limit")
        || error.starts_with("Wasm program stderr exceeded the capture limit")
        || error.starts_with("Wasm result channel exceeded the 64-byte limit")
        || error.starts_with("Wasm result channel emitted invalid UTF-8")
        || error.starts_with("Cranelift stdout exceeded the capture limit")
        || error.starts_with("Cranelift stderr exceeded the capture limit")
        || error.contains("timed out after")
        || error.starts_with("Wasm result channel did not terminate its result")
        || error.starts_with("Wasm result channel did not emit exactly one result line")
        || error.starts_with("generated C result channel did not terminate its result")
        || error.starts_with("generated C result channel did not emit exactly one result line")
        || error.starts_with("parse generated C result channel result ")
        || error.starts_with("parse Wasm result channel result ")
}

pub(crate) fn backend_failure_kind(backend: &str, error: &str) -> &'static str {
    if error.starts_with("emission diagnostic:") {
        return match backend {
            "C" => "c-emission-failed",
            "Wasm" => "wasm-emission-failed",
            _ => "backend-emission-failed",
        };
    }

    match backend {
        "C" if error.starts_with("C compiler ") => {
            if error.contains("failed to start process") {
                "c-compiler-unavailable"
            } else if error.contains("timed out after") {
                "c-compiler-timeout"
            } else {
                "c-compilation-failed"
            }
        }
        "C" if error.starts_with("run ") && error.contains("timed out after") => {
            "c-execution-timeout"
        }
        "C" if error.starts_with("run ") => "c-execution-failed",
        "C" if error.starts_with("generated C program exited with ") => "c-program-exited",
        "C" if error.contains("result channel") || error.starts_with("parse generated C ") => {
            "c-result-channel-failed"
        }
        "C" if error.starts_with("generated C program stdout") => "c-stdout-capture-failed",
        "C" if error.starts_with("generated C program stderr") => "c-stderr-capture-failed",
        "C" if error.starts_with("write ") => "c-artifact-write-failed",
        "C" => "c-backend-failed",
        "Wasm"
            if error.starts_with("Wasm runner ") && error.contains("failed to start process") =>
        {
            "wasm-runner-unavailable"
        }
        "Wasm" if error.starts_with("Wasm runner ") && error.contains("timed out after") => {
            "wasm-runner-timeout"
        }
        "Wasm" if error.starts_with("Wasm runner stdout exceeded") => "wasm-runner-stdout-limit",
        "Wasm" if error.starts_with("Wasm runner ") => "wasm-runner-exited",
        "Wasm" if error.starts_with("Wasm program stderr") => "wasm-stderr-capture-failed",
        "Wasm" if error.contains("result channel") || error.starts_with("parse Wasm ") => {
            "wasm-result-channel-failed"
        }
        "Wasm" if error.starts_with("write ") => "wasm-artifact-write-failed",
        "Wasm" => "wasm-backend-failed",
        _ => "backend-failed",
    }
}

pub(crate) fn execute_cranelift(
    program: &arandu_semantics::amir::AmirProgram,
    symbols: &arandu_semantics::SymbolTable,
    type_info: &arandu_semantics::TypeInfo,
) -> Result<BackendObservation, String> {
    execute_cranelift_internal(program, symbols, type_info, false)
        .map(|(observation, _)| observation)
}

fn execute_cranelift_with_block_coverage(
    program: &arandu_semantics::amir::AmirProgram,
    symbols: &arandu_semantics::SymbolTable,
    type_info: &arandu_semantics::TypeInfo,
) -> Result<(BackendObservation, arandu_backend_cranelift::BlockCoverage), String> {
    execute_cranelift_internal(program, symbols, type_info, true)
}

fn execute_cranelift_internal(
    program: &arandu_semantics::amir::AmirProgram,
    symbols: &arandu_semantics::SymbolTable,
    type_info: &arandu_semantics::TypeInfo,
    collect_block_coverage: bool,
) -> Result<(BackendObservation, arandu_backend_cranelift::BlockCoverage), String> {
    reset_jit_stdout_capture();
    let backend = if collect_block_coverage {
        arandu_backend_cranelift::CraneliftBackend::
            try_new_with_block_coverage_and_io_and_process_args(
                capture_jit_println,
                capture_jit_eprint,
                synthesized_args_len,
                synthesized_args_arg,
            )
    } else {
        arandu_backend_cranelift::CraneliftBackend::try_new_with_io_and_process_args(
            capture_jit_println,
            capture_jit_eprint,
            synthesized_args_len,
            synthesized_args_arg,
        )
    }
    .map_err(|diagnostic| format!("JIT initialization diagnostic: {diagnostic:?}"))?;
    let module = if collect_block_coverage {
        backend.compile_with_block_coverage(program, symbols, type_info)
    } else {
        backend.compile(program, symbols, type_info)
    }
    .map_err(|diagnostic| format!("code generation diagnostic: {diagnostic:?}"))?;
    // SAFETY: the synthesizer always emits `func main(): int` with no
    // parameters; the entry signature below matches the language ABI used by
    // the CLI's JIT invocation path.
    let main = unsafe { module.get_fn::<unsafe fn() -> i32>("main") }
        .ok_or_else(|| "compiled module does not export main as () -> int".to_owned())?;
    // SAFETY: the JIT module remains alive for the duration of the call, and
    // the function signature was checked by the generated source contract.
    let result = unsafe { main() };
    let stdout = take_jit_stdout_capture();
    let stderr = take_jit_stderr_capture();
    if stdout.invalid || stderr.invalid {
        return Err("Cranelift I/O callback received an invalid string ABI value".to_owned());
    }
    if stdout.truncated {
        return Err("Cranelift stdout exceeded the capture limit".to_owned());
    }
    if stderr.truncated {
        return Err("Cranelift stderr exceeded the capture limit".to_owned());
    }
    let block_coverage = if collect_block_coverage {
        module
            .take_block_coverage()
            .ok_or_else(|| "coverage-enabled JIT module has no recorder".to_owned())?
    } else {
        arandu_backend_cranelift::BlockCoverage::default()
    };
    Ok((
        BackendObservation {
            result,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        },
        block_coverage,
    ))
}

fn execute_c(
    program: &arandu_semantics::amir::AmirProgram,
    symbols: &arandu_semantics::SymbolTable,
    type_info: &arandu_semantics::TypeInfo,
) -> Result<BackendObservation, String> {
    let has_env_runtime = program.extern_funcs.keys().any(|symbol| {
        matches!(
            symbols.get(*symbol).name.as_ref(),
            "ar_env_args_len" | "ar_env_arg"
        )
    });
    let mut source = arandu_backend_c::emit_c(
        program,
        symbols,
        type_info,
        &type_info.type_interner,
        arandu_middle::layout::DataLayout::host(),
    )
    .map_err(|diagnostic| format!("emission diagnostic: {diagnostic:?}"))?;
    source = format!("#define main arandu_main\n{source}\n#undef main\n");
    let argv0_setup = if has_env_runtime {
        source.push_str(
            "\nstatic void arandu_smith_set_argv0(void) {\n#if defined(_WIN32)\n    ar_init_env_args_if_needed();\n#endif\n    if (ar_env_c_argv && ar_env_c_argc > 0) ar_env_c_argv[0] = \"arandu-smith\";\n}\n",
        );
        "arandu_smith_set_argv0(); "
    } else {
        ""
    };
    source.push_str(&format!(
        "\n#include <stdio.h>\n#include <stdlib.h>\nint main(void) {{ {argv0_setup}int32_t result = arandu_main(); const char *path = getenv(\"ARANDU_SMITH_RESULT_PATH\"); if (!path) return 125; FILE *out = fopen(path, \"wb\"); if (!out) return 126; int ok = fprintf(out, \"%d\\n\", result) >= 0; if (fclose(out) != 0) ok = 0; return ok ? 0 : 127; }}\n"
    ));

    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let stem = format!("arandu_smith_{}_{}", std::process::id(), id);
    let directory = std::env::temp_dir();
    let source_path = directory.join(format!("{stem}.c"));
    let executable_path = directory.join(if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.clone()
    });
    let result_path = directory.join(format!("{stem}.result"));
    let _cleanup = TemporaryArtifacts(vec![
        source_path.clone(),
        executable_path.clone(),
        result_path.clone(),
    ]);
    std::fs::write(&source_path, source)
        .map_err(|error| format!("write {}: {error}", source_path.display()))?;

    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let mut compile_command = Command::new(&compiler);
    compile_command
        .arg("-fwrapv")
        .arg("-O0")
        .arg(&source_path)
        .arg("-o")
        .arg(&executable_path)
        .arg("-lm");
    let compile = run_with_timeout(&mut compile_command, BACKEND_PROCESS_TIMEOUT)
        .map_err(|error| format!("C compiler {:?}: {error}", compiler))?;
    if !compile.status.success() {
        return Err(format!(
            "C compiler {:?} exited with {}: {}",
            compiler,
            compile.status,
            captured_stderr(&compile)
        ));
    }

    let mut execution_command = Command::new(&executable_path);
    execution_command.args(SYNTHESIZED_PROGRAM_ARGS);
    execution_command.env("ARANDU_SMITH_RESULT_PATH", &result_path);
    let execution = run_with_timeout(&mut execution_command, BACKEND_PROCESS_TIMEOUT)
        .map_err(|error| format!("run {}: {error}", executable_path.display()))?;
    if !execution.status.success() {
        return Err(format!(
            "generated C program exited with {}: {}",
            execution.status,
            captured_stderr(&execution)
        ));
    }
    if execution.stdout_truncated {
        return Err("generated C program stdout exceeded the capture limit".to_owned());
    }
    if execution.stderr_truncated {
        return Err("generated C program stderr exceeded the capture limit".to_owned());
    }
    let result_bytes = read_result_channel(&result_path, "generated C")?;
    let result = parse_backend_result(&result_bytes, false, "generated C result channel")?;
    Ok(BackendObservation {
        result,
        stdout: execution.stdout,
        stderr: execution.stderr,
    })
}

fn execute_wasm(
    program: &arandu_semantics::amir::AmirProgram,
    symbols: &arandu_semantics::SymbolTable,
    type_info: &arandu_semantics::TypeInfo,
) -> Result<BackendObservation, String> {
    let bytes = arandu_backend_wasm::emit_wasm(
        program,
        symbols,
        &type_info.type_interner,
        type_info,
        arandu_middle::layout::DataLayout::ptr_width(4),
    )
    .map_err(|diagnostic| format!("emission diagnostic: {diagnostic:?}"))?;
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let wasm_path =
        std::env::temp_dir().join(format!("arandu_smith_{}_{}.wasm", std::process::id(), id));
    let _cleanup = TemporaryArtifacts(vec![wasm_path.clone()]);
    std::fs::write(&wasm_path, bytes)
        .map_err(|error| format!("write {}: {error}", wasm_path.display()))?;

    const RUNNER: &str = r#"(async()=>{
const fs=require('fs');
const bytes=fs.readFileSync(process.argv[1]);
const programArgs=['arandu-smith',...process.argv.slice(2)];
let instance;
const env={
  ar_vec_malloc: size=>instance.exports.cabi_realloc(0,0,8,size),
  ar_vec_realloc: (ptr,oldSize,newSize)=>instance.exports.cabi_realloc(ptr,oldSize,8,newSize),
  ar_vec_buf_free: (ptr,size)=>{instance.exports.cabi_realloc(ptr,size,8,0)},
  ar_string_push_str: (raw,valuePtr,valueLen)=>{
    const memory=instance.exports.memory;
    const owner=Number(raw), source=Number(valuePtr), count=Number(valueLen);
    if(!memory || !Number.isSafeInteger(owner) || owner<=0 || owner+12>memory.buffer.byteLength ||
       !Number.isSafeInteger(source) || !Number.isSafeInteger(count) || count<0 || count>0x7fffffff ||
       (count>0 && (source<=0 || source+count>memory.buffer.byteLength))) return 0;
    let view=new DataView(memory.buffer);
    let data=view.getUint32(owner,true), length=view.getUint32(owner+4,true), capacity=view.getUint32(owner+8,true);
    if(length>capacity || (capacity>0 && data===0) || data>memory.buffer.byteLength ||
       capacity>memory.buffer.byteLength-data || count>0x7fffffff-length) return 0;
    const required=length+count;
    const bytes=count===0 ? new Uint8Array(0) : new Uint8Array(memory.buffer,source,count).slice();
    if(required>capacity){
      let next=capacity<8 ? 8 : capacity;
      while(next<required) next=next>0x3fffffff ? 0x7fffffff : next*2;
      const replacement=instance.exports.cabi_realloc(data,capacity,8,next);
      if(!replacement || replacement>memory.buffer.byteLength ||
         next>memory.buffer.byteLength-replacement) return 0;
      data=replacement;
      view=new DataView(memory.buffer);
      view.setUint32(owner,data,true);
      view.setUint32(owner+8,next,true);
    }
    if(count>0) new Uint8Array(memory.buffer,data+length,count).set(bytes);
    view=new DataView(memory.buffer);
    view.setUint32(owner+4,required,true);
    return 1;
  },
  ar_env_args_len: ()=>programArgs.length,
  ar_env_arg: index=>{
    const value=programArgs[Number(index)]||'';
    const encoded=Buffer.from(value,'utf8');
    if(encoded.length===0)return [0,0];
    const ptr=instance.exports.cabi_realloc(0,0,1,encoded.length);
    new Uint8Array(instance.exports.memory.buffer,ptr,encoded.length).set(encoded);
    return [ptr,encoded.length];
  },
  ar_env_var_is_set: (ptr,len)=>{
    const name=new TextDecoder().decode(new Uint8Array(instance.exports.memory.buffer,Number(ptr),Number(len)));
    return Object.hasOwn(process.env,name)?1:0;
  }
};
const io={
  println: (ptr,len)=>{
    const start=Number(ptr), size=Number(len);
    const memory=instance.exports.memory;
    if(!memory || !Number.isSafeInteger(start) || !Number.isSafeInteger(size) || start<0 || size<0 || start+size>memory.buffer.byteLength) throw new Error('invalid io.println memory range');
    process.stdout.write(new Uint8Array(memory.buffer,start,size));
    process.stdout.write('\n');
  },
  eprint: (ptr,len)=>{
    const start=Number(ptr), size=Number(len);
    const memory=instance.exports.memory;
    if(!memory || !Number.isSafeInteger(start) || !Number.isSafeInteger(size) || start<0 || size<0 || start+size>memory.buffer.byteLength) throw new Error('invalid io.eprint memory range');
    process.stderr.write(new Uint8Array(memory.buffer,start,size));
  }
};
({instance}=await WebAssembly.instantiate(bytes,{env,io}));
const main=instance.exports.main;
if(typeof main!=='function')throw new Error('Wasm module does not export main');
const result=main();
if(typeof result!=='number')throw new Error('Wasm main did not return i32');
const resultPath=process.env.ARANDU_SMITH_RESULT_PATH;
if(!resultPath)throw new Error('missing result channel path');
fs.writeFileSync(resultPath,String(result)+'\n');
})().catch(e=>{console.error(e&&e.stack||e);process.exitCode=1})"#;
    let node = std::env::var_os("ARANDU_NODE").unwrap_or_else(|| "node".into());
    let mut execution_command = Command::new(&node);
    execution_command.arg("-e").arg(RUNNER).arg(&wasm_path);
    execution_command.args(SYNTHESIZED_PROGRAM_ARGS);
    let result_path = wasm_path.with_extension("result");
    let _result_cleanup = TemporaryArtifacts(vec![result_path.clone()]);
    execution_command.env("ARANDU_SMITH_RESULT_PATH", &result_path);
    let execution = run_with_timeout(&mut execution_command, BACKEND_PROCESS_TIMEOUT)
        .map_err(|error| format!("Wasm runner {:?}: {error}", node))?;
    if !execution.status.success() {
        return Err(format!(
            "Wasm runner {:?} exited with {}: {}",
            node,
            execution.status,
            captured_stderr(&execution)
        ));
    }
    if execution.stdout_truncated {
        return Err("Wasm runner stdout exceeded the capture limit".to_owned());
    }
    if execution.stderr_truncated {
        return Err("Wasm program stderr exceeded the capture limit".to_owned());
    }
    let result_bytes = read_result_channel(&result_path, "Wasm")?;
    let result = parse_backend_result(&result_bytes, false, "Wasm result channel")?;
    Ok(BackendObservation {
        result,
        stdout: execution.stdout,
        stderr: execution.stderr,
    })
}
