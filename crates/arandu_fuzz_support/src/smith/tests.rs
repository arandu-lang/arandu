use super::artifact::*;
use super::emi::*;
use super::oracle::*;
use super::process::*;
use super::synth::*;
use super::synth::{int_hash_bucket, seeded_hash_collision_key};
use super::types::*;

use arandu_query::AnalysisHost;
#[cfg(unix)]
use std::process::Command;
use std::sync::atomic::Ordering;
#[cfg(unix)]
use std::time::{Duration, Instant};

fn assert_synthesized_seed_matches_all_backends(seed: u64) {
    let generated = synthesize_with_oracle(seed);
    let observation = check_source(&generated.source, true, true)
        .unwrap_or_else(|failure| panic!("generated seed {seed} failed: {failure:?}"));
    assert_eq!(observation.result, generated.expected_result, "seed {seed}");
    let expected = ExpectedObservation::synthesized(generated.expected_result);
    assert_eq!(observation.stdout, expected.stdout, "seed {seed}");
    assert_eq!(observation.stderr, expected.stderr, "seed {seed}");
}

#[test]
fn synthesized_staging_arrays_and_boundary_casts_match_all_backends() {
    // Cover every conversion family and both endpoints of the generated
    // integer domain, including negative and positive repeated values.
    for seed in [0, 1, 2, 3, 4, 5, 6, 7, 255, 2047, u64::MAX] {
        let generated = synthesize_staged_arrays_and_casts(seed);
        assert_eq!(
            generated.source,
            synthesize_staged_arrays_and_casts(seed).source
        );
        let source = format!(
            "{}\nfunc main(): int {{ return smith_staged_case() }}",
            generated.source
        );
        let observed = check_source(&source, true, true)
            .unwrap_or_else(|failure| panic!("staged seed {seed}: {failure:?}\n{source}"));
        assert_eq!(observed.result, generated.expected_result, "seed {seed}");
        assert!(observed.stdout.is_empty() && observed.stderr.is_empty());
    }
}

#[test]
fn preferred_mem_layout_calls_match_across_backends_and_staging_contexts() {
    let source = "import std.core.mem as mem\n\
        const WIDTH usize = comptime mem.sizeOf<int>()\n\
        func width<T>(): usize { return mem.sizeOf<T>() }\n\
        func count<comptime N: uint>(): uint { return N }\n\
        func main(): int {\n\
            let values: [comptime (mem.sizeOf<int>())]u8 = [1 as u8; comptime (mem.sizeOf<int>())]\n\
            comptime if mem.alignOf<int>() == 4 {\n\
                return (WIDTH + width<int>() + count<comptime (mem.sizeOf<int>())>() as usize + values[3] as usize) as int\n\
            } else { return absent() }\n\
        }";
    let observed = check_source(source, true, true)
        .unwrap_or_else(|failure| panic!("preferred layout calls: {failure:?}"));
    assert_eq!(observed.result, 13);
    assert!(observed.stdout.is_empty() && observed.stderr.is_empty());
}

#[test]
fn scalar_ctfe_matches_defined_runtime_in_every_backend_and_optimization_level() {
    use arandu_middle::ctfe::ConstValue;
    use arandu_mir::ctfe::Budget;
    use arandu_query::ctfe::{ctfe_eval, CtfeRequest};
    use arandu_query::{passes::declaration_signatures, DatabaseImpl};
    // Checked CTFE arithmetic and runtime wrapping are compared only where
    // both are defined. Large unsigned values are compared before returning
    // int so the result channel, not the process exit byte, is the oracle.
    let cases = [
        (
            "func add(a: int, b: int): int { return a + b }\nfunc main(): int { return add(20, 22) }",
            42,
        ),
        (
            "func main(): int { let mut i = 0\n let mut total = 0\n while i < 10 { total = total + i\n i = i + 1 }\n return total }",
            45,
        ),
        (
            "func sum(n: int): int { if n == 0 { return 0 }\n return n + sum(n - 1) }\nfunc main(): int { return sum(10) }",
            55,
        ),
        (
            "func main(): int { let a: int = -27\n let b: int = 4\n return (a / b) * 10 + a % b }",
            -63,
        ),
        (
            "func main(): int { let a: int = -32\n let b: uint = 32\n return (a >> 3) + ((b >> 2) as int) + (3 << 2) }",
            16,
        ),
        (
            "func main(): int { let high: u64 = 18446744073709551615\n if high > 9223372036854775807 { return 42 }\n return 1 }",
            42,
        ),
    ];
    for (source, expected) in cases {
        let mut db = DatabaseImpl::new();
        let file = db.new_file("ctfe-parity.aru".into(), source.into());
        let main = declaration_signatures(&db, file)
            .symbols
            .iter()
            .find(|symbol| symbol.name == "main")
            .expect("main")
            .id;
        let request = CtfeRequest::new(
            &db,
            file,
            main,
            vec![],
            Budget {
                fuel: 100_000,
                frames: 64,
                values: 10_000,
            },
        );
        let value = ctfe_eval(&db, request)
            .as_ref()
            .unwrap_or_else(|error| panic!("CTFE failed: {error:?}\n{source}"));
        let ConstValue::Integer(value) = value else {
            panic!("integer result expected")
        };
        assert_eq!(value.value(), i128::from(expected), "CTFE: {source}");
        let runtime = check_source(source, true, true)
            .unwrap_or_else(|error| panic!("runtime failed: {error:?}\n{source}"));
        assert_eq!(runtime.result, expected, "runtime: {source}");
        assert!(runtime.stdout.is_empty() && runtime.stderr.is_empty());
    }
}

#[test]
fn static_if_scopes_match_every_backend_and_optimization_level() {
    let cases = [
        (
            "func choose(): bool { comptime if true { return true } else { return missing } }\nfunc main(): int { comptime if choose() { return 42 } else { return absent() } }",
            42,
        ),
        (
            "func id<T>(x: T): T { return x }\nfunc main(): int { comptime if false { return absent() } else comptime if true && !false { let answer = comptime 42; return id(answer) } else { return missing } }",
            42,
        ),
        (
            "func main(): int { let mut i = 0; let mut total = 0; while i < 4 { comptime if true { defer { total = total + 1 }; i = i + 1; if i == 1 { continue }; if i == 3 { break } } else { missing() } }; return total }",
            3,
        ),
        (
            "func main(): int { let mut i = 0; let mut total = 0; while i < 4 { if true { defer { total = total + 1 }; i = i + 1; if i == 1 { continue }; if i == 3 { break } } }; return total }",
            3,
        ),
    ];
    for (source, expected) in cases {
        let observation = check_source(source, true, true)
            .unwrap_or_else(|failure| panic!("static selection failed: {failure:?}\n{source}"));
        assert_eq!(observation.result, expected, "{source}");
        assert!(observation.stdout.is_empty() && observation.stderr.is_empty());
    }
}

#[test]
fn finite_static_loops_execute_identically_in_all_backends_and_optimization_levels() {
    // The independent results include normal/empty/reversed domains, fresh
    // iteration locals and scope exits. `check_source` exercises O0/O1/O2/Os;
    // these are execution oracles, not merely successful lowering checks.
    let cases = [
        (
            "func main(): int { let mut sum = 0; comptime for i in 0..4 { let value = i * 2; sum += value }; return sum }",
            12,
        ),
        (
            "func main(): int { let mut sum = 42; comptime for i in 4..4 { sum += i }; comptime for i in 5..2 { sum += i }; return sum }",
            42,
        ),
        (
            "func main(): int { let mut sum = 0; comptime for i in 0..4 { if i == 1 { continue }; if i == 3 { break }; sum += i }; return sum }",
            2,
        ),
        (
            "func main(): int { let mut sum = 0; comptime for i in 0..4 { defer { sum += 10 }; if i == 1 { continue }; if i == 3 { break }; sum += i }; return sum }",
            42,
        ),
        (
            "func main(): int { comptime for i in 0..4 { return i + 42 }; return 99 }",
            42,
        ),
        (
            "func main(): int { let mut sum = 0; comptime for i in 0..3 { comptime for j in 0..2 { sum += i + j } }; return sum }",
            9,
        ),
        (
            "func end(): uint { return 4 }\nfunc main(): int { let mut sum = 0; comptime for i in 0..end() { sum += i as int }; return sum }",
            6,
        ),
        (
            "func main(): int { let mut sum = 0; comptime for i in -2..2 { sum += i }; return sum }",
            -2,
        ),
        (
            "func main(): int { return comptime { let mut sum = 0; comptime for i in 0..4 { sum += i }; sum } }",
            6,
        ),
        (
            "func main(): int { let mut sum: usize = 0; comptime for i in 0..(@sizeOf([3]u16)) { sum += i }; return sum as int }",
            15,
        ),
        (
            "func main(): int { let mut sum = 0; comptime for i in 0..4 { comptime if i == 2 { sum += 42 } else { sum += i } }; return sum }",
            46,
        ),
        (
            "func count<comptime N: uint>(): uint { return N }\nfunc main(): int { let mut sum = 0; comptime for i in 0..4 { sum += count<comptime (i + 1)>() as int }; return sum }",
            10,
        ),
        (
            "func main(): int { let mut sum = 0; comptime for i in 0..4 { let value = comptime (i + 1); sum += value }; return sum }",
            10,
        ),
        (
            "func main(): int { let mut sum = 0; comptime for i in 0..4 { comptime for j in 0..i { sum += j } }; return sum }",
            4,
        ),
    ];
    for (source, expected) in cases {
        let observation = check_source(source, true, true).unwrap_or_else(|failure| {
            panic!("static loop execution failed: {failure:?}\n{source}")
        });
        assert_eq!(observation.result, expected, "{source}");
        assert!(observation.stdout.is_empty() && observation.stderr.is_empty());
    }
}

#[test]
fn synthesized_nested_comptime_if_matches_oracle_across_backends() {
    use super::synthesize_nested_comptime_if;
    for seed in [0, 7, 42, 99, 1337] {
        let synthesized = synthesize_nested_comptime_if(seed, 3);
        let observation = check_source(&synthesized.source, true, true).unwrap_or_else(|failure| {
            panic!("seed {seed} failed: {failure:?}\n{}", synthesized.source)
        });
        assert_eq!(
            observation.result, synthesized.expected_result,
            "seed {seed}:\n{}",
            synthesized.source
        );
        assert!(observation.stdout.is_empty() && observation.stderr.is_empty());
    }
}

#[test]
fn synthesized_nested_comptime_if_salsa_cache_invalidation_is_clean() {
    use super::synthesize_nested_comptime_if;
    use arandu_query::{passes, DatabaseImpl};

    for seed in [0, 42, 128] {
        super::oracle::check_static_cache(seed).unwrap_or_else(|error| panic!("{error:?}"));
        let synthesized = synthesize_nested_comptime_if(seed, 3);
        let (mut db, log) = DatabaseImpl::with_rebuild_log();
        let file = db.new_file("static.aru".into(), synthesized.source.clone());

        // First pass: type checking must pass with zero errors because discarded
        // branches are never resolved or typed.
        let checked = passes::type_check(&db, file);
        assert!(
            checked
                .diagnostics
                .iter()
                .all(|d| d.severity != arandu_middle::Severity::Error),
            "seed {seed} had unexpected errors: {:?}",
            checked.diagnostics
        );

        let branches_initial = passes::resolve(&db, file)
            .resolved
            .comptime_branches
            .clone();
        assert!(
            !branches_initial.is_empty(),
            "seed {seed} should have static branch decisions"
        );

        // Append outside the owner: leading text would move every real span
        // and must revalidate diagnostic locations, not preserve stale keys.
        log.clear();
        db.update_file_text(
            file,
            format!("{}\n// trailing comment\n", synthesized.source),
        );
        let branches_after_comment = passes::resolve(&db, file)
            .resolved
            .comptime_branches
            .clone();
        assert_eq!(branches_initial, branches_after_comment);
        assert_eq!(
            log.count_executions_matching("item_static_branches"),
            0,
            "seed {seed}: item_static_branches re-executed spuriously on comment edit"
        );

        // 2. Editing discarded code in unselected branches: must NOT invalidate
        // the selected branch typing or report errors.
        log.clear();
        let modified_discarded = synthesized
            .source
            .replace("missing", "unknown")
            .replace("nonexistent", "unavailable")
            .replace("absent", "unseen");
        db.update_file_text(file, modified_discarded);
        let checked_after_discarded = passes::type_check(&db, file);
        assert!(
            checked_after_discarded
                .diagnostics
                .iter()
                .all(|d| d.severity != arandu_middle::Severity::Error),
            "seed {seed} had errors after modifying discarded branch: {:?}",
            checked_after_discarded.diagnostics
        );
        let branches_after_discarded = passes::resolve(&db, file)
            .resolved
            .comptime_branches
            .clone();
        assert_eq!(branches_initial, branches_after_discarded);
        assert_eq!(
            log.count_executions_matching("ctfe_eval_root"),
            0,
            "seed {seed}: unchanged conditions must cut off before the VM"
        );

        // A source displacement must publish current spans and converge to a
        // fresh DB. This is legitimate revalidation, not spurious invalidation.
        let displaced = format!("// 😀 shifted source\n{}", synthesized.source);
        db.update_file_text(file, displaced.clone());
        let incremental = passes::type_check(&db, file);
        let mut clean = DatabaseImpl::new();
        let clean_file = clean.new_file("static.aru".into(), displaced);
        let clean_result = passes::type_check(&clean, clean_file);
        assert_eq!(incremental.diagnostics, clean_result.diagnostics);
        assert_eq!(
            incremental.resolved.comptime_branches,
            clean_result.resolved.comptime_branches
        );
    }
}

#[test]
fn comptime_value_parameters_match_const_parameters_across_all_backends() {
    for marker in ["const", "comptime"] {
        for source in [
            format!(
                "func count<{marker} N: uint>(): uint {{ return N }}\nfunc main(): int {{ return (count<20>() + count<22>()) as int }}"
            ),
            format!(
                "func total<{marker} N: uint>(values: [N]int): int {{ return (N as int) + values[0] + values[1] }}\nfunc main(): int {{ return total<3>([19, 20, 0]) }}"
            ),
        ] {
            let observation = check_source(&source, true, true)
                .unwrap_or_else(|failure| panic!("{marker}: {failure:?}\n{source}"));
            assert_eq!(observation.result, 42);
            assert!(observation.stdout.is_empty() && observation.stderr.is_empty());
        }
    }
}

#[test]
fn computed_const_arguments_match_every_backend_and_optimization_level() {
    for source in [
        "func count<comptime N: uint>(): uint { return N }\nfunc wrapper<T>(x: T): uint { return count<comptime (20 + 22)>() }\nfunc main(): int { if wrapper<int>(0) != 42 || wrapper<bool>(false) != 42 { return 1 }; return 42 }",
        "func leaf<comptime N: u16>(): int { return N as int }\nfunc wrapper<comptime M: u8>(): int { return leaf<M>() }\nfunc main(): int { return wrapper<42>() }",
        "func add(a: int, b: int): int { return a + b }\nfunc count<comptime N: uint>(): uint { return N }\nfunc main(): int { return count<comptime (add(20, 22))>() as int }",
        "func total<comptime N: uint>(values: [N]int): int { return (N as int) + values[0] + values[1] }\nfunc main(): int { return total<comptime (1 + 2)>([19, 20, 0]) }",
        "func count<comptime N: uint>(): uint { return N }\nfunc main(): int { comptime if true { return count<comptime (42)>() as int } else { return count<comptime (missing / 0)>() as int } }",
        "func count<comptime N: uint>(): uint { return N }\nfunc main(): int { return count<comptime (if 1 < 2 { 42 } else { 0 })>() as int }",
    ] {
        let observation = check_source(source, true, true)
            .unwrap_or_else(|failure| panic!("computed constant failed: {failure:?}\n{source}"));
        assert_eq!(observation.result, 42, "{source}");
        assert!(observation.stdout.is_empty() && observation.stderr.is_empty());
    }
}

#[test]
fn confirmed_failure_artifact_contains_source_seed_and_replay_metadata() {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "arandu-smith-artifact-test-{}-{id}",
        std::process::id()
    ));
    let failure = Failure::new("cranelift-c-mismatch", "Cranelift=2, C=3", true).with_scope("O2");
    let artifact = write_failure_artifact(
        &root,
        FailureArtifact {
            target: "synthesized-all",
            corpus_name: "synthesized",
            seed: 42,
            failure: &failure,
            source: "func main(): int { return 2 }\n",
            emi_candidate: "func main(): int { if false { let x = 1 }; return 2 }\n",
            shrink_attempts: 7,
            shrink_reductions: 3,
            shrink_confirmed: true,
        },
    )
    .unwrap();

    assert_eq!(
        std::fs::read(artifact.join("candidate.aru")).unwrap(),
        b"func main(): int { return 2 }\n"
    );
    assert_eq!(
        std::fs::read(artifact.join("reproducer.bin")).unwrap(),
        42_u64.to_le_bytes()
    );
    let seed = std::fs::read_to_string(artifact.join("reproducer.seed")).unwrap();
    assert_eq!(seed, "encoding=hex\n2a00000000000000\n");
    let metadata = std::fs::read_to_string(artifact.join("metadata.txt")).unwrap();
    assert!(metadata.contains("failure_kind=cranelift-c-mismatch"));
    assert!(metadata.contains("failure_scope=O2"));
    assert!(metadata.contains("shrink_confirmed=true"));
    assert!(metadata.contains("run-fuzz-seed synthesized-all"));
    assert_eq!(
        std::fs::read_to_string(artifact.join("failure.txt")).unwrap(),
        "Cranelift=2, C=3"
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn oracle_target_name_matches_the_selected_backend_matrix() {
    assert_eq!(
        oracle_target_name(false, false, "synthesized"),
        "synthesized"
    );
    assert_eq!(
        oracle_target_name(true, false, "synthesized"),
        "synthesized-c"
    );
    assert_eq!(
        oracle_target_name(false, true, "synthesized"),
        "synthesized-wasm"
    );
    assert_eq!(
        oracle_target_name(true, true, "synthesized"),
        "synthesized-all"
    );
    assert_eq!(oracle_target_name(true, true, "enum-if-join"), "emi-corpus");
}

#[test]
fn shrink_identity_preserves_backend_optimization_level() {
    let original = Failure::new("cranelift-c-mismatch", "mismatch at O2", true).with_scope("O2");
    let same =
        Failure::new("cranelift-c-mismatch", "different values at O2", true).with_scope("O2");
    let other_level = Failure::new("cranelift-c-mismatch", "mismatch at O0", true).with_scope("O0");
    let other_kind =
        Failure::new("cranelift-wasm-mismatch", "mismatch at O2", true).with_scope("O2");

    assert!(original.same_identity(&same));
    assert!(!original.same_identity(&other_level));
    assert!(!original.same_identity(&other_kind));
}

#[test]
fn shrink_identity_preserves_backend_failure_phase() {
    let compile = Failure::new(
        backend_failure_kind("C", "C compiler \"cc\" exited with 1: invalid C"),
        "C compiler rejected generated code",
        true,
    )
    .with_scope("O2");
    let runtime = Failure::new(
        backend_failure_kind("C", "generated C program exited with exit status: 1"),
        "generated C program exited",
        true,
    )
    .with_scope("O2");
    let same_phase = Failure::new(
        backend_failure_kind("C", "C compiler \"cc\" exited with 2: another C error"),
        "different compiler diagnostic",
        true,
    )
    .with_scope("O2");

    assert!(!compile.same_identity(&runtime));
    assert!(compile.same_identity(&same_phase));
}

#[test]
fn seeded_hash_map_keys_really_collide_after_fixed_width_hashing() {
    for value in [-32_768, -1, 0, 240, 248, 256, i32::MAX as i64 - 16] {
        let existing = (0..9).map(|index| value + index * 8).collect::<Vec<_>>();
        let collision = seeded_hash_collision_key(value, &existing);

        assert!(!existing.contains(&collision));
        assert_eq!(int_hash_bucket(value), int_hash_bucket(collision));
    }
}

#[test]
fn backend_panics_become_scoped_shrinkable_failures() {
    let failure = catch_backend_panic("Cranelift", arandu_mir::OptLevel::O2, || {
        std::panic::panic_any("simulated backend panic")
    })
    .unwrap_err();

    assert_eq!(failure.kind, "cranelift-panicked");
    assert_eq!(failure.scope.as_deref(), Some("O2"));
    assert!(failure.shrinkable);
    assert!(failure.message.contains("simulated backend panic"));

    let same_phase = catch_backend_panic("Cranelift", arandu_mir::OptLevel::O2, || {
        std::panic::panic_any("same panic class")
    })
    .unwrap_err();
    let different_level = catch_backend_panic("Cranelift", arandu_mir::OptLevel::O0, || {
        std::panic::panic_any("simulated backend panic")
    })
    .unwrap_err();
    assert!(failure.same_identity(&same_phase));
    assert!(!failure.same_identity(&different_level));
}

#[cfg(unix)]
#[test]
fn subprocess_runner_captures_output_and_enforces_timeout() {
    let mut output_command = Command::new("sh");
    output_command
        .arg("-c")
        .arg("printf stdout; printf stderr >&2");
    let output = run_with_timeout(&mut output_command, Duration::from_secs(2)).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"stdout");
    assert_eq!(output.stderr, b"stderr");

    let mut hanging_command = Command::new("sh");
    hanging_command.arg("-c").arg("exec sleep 2");
    let error = run_with_timeout(&mut hanging_command, Duration::from_millis(20)).unwrap_err();
    assert!(error.contains("timed out after"), "{error}");
    assert!(is_backend_failure_reproducible(&error));
}

#[cfg(unix)]
#[test]
fn subprocess_runner_closes_inherited_pipes_after_parent_exit() {
    let mut command = Command::new("sh");
    command.arg("-c").arg("sleep 3 & exit 0");
    let started = Instant::now();
    let output = run_with_timeout(&mut command, Duration::from_secs(2)).unwrap();

    assert!(output.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "descendant kept subprocess pipes open for {:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn subprocess_runner_kills_descendants_when_the_timeout_expires() {
    let mut command = Command::new("sh");
    command.arg("-c").arg("sleep 30 & wait");
    let started = Instant::now();
    let error = run_with_timeout(&mut command, Duration::from_millis(50)).unwrap_err();

    assert!(error.contains("timed out after"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "descendant survived timeout for {:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn subprocess_runner_drains_pipes_while_bounding_captured_output() {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("head -c 2097152 /dev/zero; head -c 2097152 /dev/zero >&2");
    let output = run_with_timeout(&mut command, Duration::from_secs(3)).unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout.len(), MAX_CAPTURED_PROCESS_OUTPUT);
    assert_eq!(output.stderr.len(), MAX_CAPTURED_PROCESS_OUTPUT);
    assert!(output.stdout_truncated);
    assert!(output.stderr_truncated);
}

#[test]
fn backend_result_parser_requires_one_complete_integer_line() {
    assert_eq!(
        parse_backend_result(b"-17\n", false, "runner").unwrap(),
        -17
    );
    assert!(parse_backend_result(b"17", false, "runner").is_err());
    assert!(parse_backend_result(b"log\n17\n", false, "runner").is_err());
    assert!(parse_backend_result(b"17\nextra\n", false, "runner").is_err());
    assert!(parse_backend_result(b"17\n", true, "runner").is_err());
}

#[test]
fn result_channel_reader_bounds_sidecar_allocations() {
    let path = std::env::temp_dir().join(format!(
        "arandu-result-channel-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, vec![b'0'; MAX_RESULT_CHANNEL_BYTES + 1]).unwrap();
    let error = read_result_channel(&path, "test").unwrap_err();
    assert!(error.contains("exceeded the 64-byte limit"));
    std::fs::write(&path, b"17\n").unwrap();
    assert_eq!(read_result_channel(&path, "test").unwrap(), b"17\n");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn missing_backend_executable_is_not_misreported_as_a_reproducible_bug() {
    let missing_cc = "C compiler \"missing-cc\": failed to start process: not found";
    let missing_node = "Wasm runner \"missing-node\": failed to start process: not found";
    assert!(!is_backend_failure_reproducible(missing_cc));
    assert!(!is_backend_failure_reproducible(missing_node));
    assert_eq!(
        backend_failure_kind("C", missing_cc),
        "c-compiler-unavailable"
    );
    assert_eq!(
        backend_failure_kind("Wasm", missing_node),
        "wasm-runner-unavailable"
    );
    assert!(is_backend_failure_reproducible(
        "C compiler \"cc\" exited with 1: invalid generated C"
    ));
    assert!(is_backend_failure_reproducible(
        "generated C result channel emitted invalid UTF-8"
    ));
    assert!(is_backend_failure_reproducible(
        "Wasm result channel emitted invalid UTF-8"
    ));
}

#[test]
fn synthesis_is_deterministic_and_reaches_amir_for_many_seeds() {
    let mut host = AnalysisHost::new();
    register_fuzz_stdlib(&mut host)
        .unwrap_or_else(|error| panic!("failed to register Vec dependencies: {error}"));
    let first_source = synthesize(0);
    let file = host.new_file("smith/generated.aru".into(), first_source);

    // Keep a few separated incremental updates to exercise the same-file
    // analysis path without lowering every generated seed. The loop below
    // still checks determinism and required source features for all seeds.
    let amir_seeds = [1, 12, 24];
    drop(lower_validated_file(&host, file).unwrap_or_else(|failure| {
        panic!(
            "synthesized seed 0 did not reach valid AMIR: {}",
            failure.message
        )
    }));

    for seed in 1..25 {
        let source = synthesize(seed);
        assert_eq!(source, synthesize(seed));
        assert!(
            source.contains("identity<float>("),
            "seed {seed} omitted float"
        );
        assert!(
            source.contains("identity<uint>("),
            "seed {seed} omitted uint"
        );
        assert!(
            source.contains("identity<char>("),
            "seed {seed} omitted char"
        );
        assert!(
            source.contains("Choice.Letter(sample.character)"),
            "seed {seed} omitted the char enum payload roundtrip"
        );
        assert_eq!(
            source.contains("GeneratedKey.hash"),
            seed.is_multiple_of(64),
            "seed {seed} did not follow the deterministic HashMap generation schedule"
        );
        if amir_seeds.contains(&seed) {
            host.set_text(file, source.as_str());
            drop(lower_validated_file(&host, file).unwrap_or_else(|failure| {
                panic!(
                    "synthesized seed {seed} did not reach valid AMIR: {}",
                    failure.message
                )
            }));
        }
    }
}

#[test]
fn generated_conditions_cover_scalar_comparisons_across_all_backends() {
    let mut covered = [None; 8];
    for seed in 0..4096_u64 {
        let mut rng = Rng::new(seed);
        let condition = gen_synthesized_prefix(&mut rng).condition;
        let has_comparison = [" == ", " != ", " < ", " <= ", " > ", " >= "]
            .into_iter()
            .any(|operator| condition.source.contains(operator));
        let is_unsigned =
            condition.source.contains("uint_base") || condition.source.contains(" as uint");
        let is_float = condition.source.contains("float_base") || condition.source.contains(".0");
        let is_char = condition.source.contains('\'');
        let is_char_ordering = is_char
            && [" < ", " <= ", " > ", " >= "]
                .into_iter()
                .any(|operator| condition.source.contains(operator));
        let candidates = [
            condition.source.contains("!("),
            condition.source.contains(" && "),
            condition.source.contains(" || "),
            has_comparison && !is_unsigned && !is_float && !is_char,
            has_comparison && is_unsigned,
            has_comparison && is_float,
            has_comparison && is_char,
            is_char_ordering,
        ];
        for (index, present) in candidates.into_iter().enumerate() {
            if covered[index].is_none() && present {
                covered[index] = Some(seed);
            }
        }
        if covered.iter().all(Option::is_some) {
            break;
        }
    }

    let seeds: std::collections::BTreeSet<_> = covered
        .into_iter()
        .map(|seed| seed.expect("synthesizer must cover every condition form"))
        .collect();
    for seed in seeds {
        let mut rng = Rng::new(seed);
        let condition = gen_synthesized_prefix(&mut rng).condition;
        let source = synthesize(seed);
        assert!(
            source.contains(&format!("enabled: identity<bool>({})", condition.source)),
            "coverage seed {seed} was not exercised by the synthesized program"
        );
        assert_synthesized_seed_matches_all_backends(seed);
    }
}

#[test]
fn synthesized_seeded_collection_cases_match_every_backend_and_level() {
    let generated = [synthesize_with_oracle(0), synthesize_with_oracle(64)];
    let first_map_insert = generated[0]
        .source
        .lines()
        .find(|line| line.contains("generated_map.put("))
        .expect("seeded HashMap source has an insertion");
    let second_map_insert = generated[1]
        .source
        .lines()
        .find(|line| line.contains("generated_map.put("))
        .expect("seeded HashMap source has an insertion");
    assert_ne!(first_map_insert, second_map_insert);
    assert!(generated[0]
        .source
        .contains("generated_bit_seed = generated_bits.insert(128 as usize)"));
    assert!(generated[1]
        .source
        .contains("generated_bit_seed = generated_bits.insert(129 as usize)"));
    assert!(generated[0]
        .source
        .contains("bs_union.unionWith(ref bs_cap)"));
    assert!(generated[0]
        .source
        .contains("generated_map_pre = hash_map.HashMap<GeneratedKey, int>.withCapacity"));
    assert!(generated[0]
        .source
        .contains("cap_vec = vec.Vec<int>.withCapacity"));

    for generated in generated {
        assert!(generated.source.contains("GeneratedKey.hash"));
        let observation = check_source(&generated.source, true, true)
            .unwrap_or_else(|failure| panic!("generated HashMap case failed: {failure:?}"));

        assert_eq!(observation.result, generated.expected_result);
        let expected = ExpectedObservation::synthesized(generated.expected_result);
        assert_eq!(observation.stdout, expected.stdout);
        assert_eq!(observation.stderr, expected.stderr);
    }
}

#[test]
fn synthesized_source_changes_with_seed() {
    let sources: std::collections::BTreeSet<_> = (0..32).map(synthesize).collect();
    assert!(sources.len() > 1);
}

#[test]
fn rng_maps_the_absorbing_zero_seed_to_a_live_state() {
    let zero_state_seed = 0x9e37_79b9_7f4a_7c15;
    let mut rng = Rng::new(zero_state_seed);
    let first = rng.next();
    let second = rng.next();
    assert_ne!(first, 0);
    assert_ne!(second, 0);
    assert_ne!(first, second);
}

#[test]
fn synthesized_sources_cover_empty_argument_nonempty_argument_and_literal_strings() {
    let sources: std::collections::BTreeSet<_> = (0..128)
        .map(synthesize)
        .map(|source| {
            if source.contains("generated_text: str = env.arg(1)") {
                0
            } else if source.contains("generated_text: str = env.arg(2)") {
                1
            } else if source.contains("generated_text: str = \"argument-two\"") {
                2
            } else {
                panic!("synthesized source omitted generated string expression")
            }
        })
        .collect();
    assert_eq!(sources, [0, 1, 2].into_iter().collect());
}

#[test]
fn synthesized_chars_cover_ascii_and_unicode_scalars() {
    let generated: std::collections::BTreeSet<_> = (0..128)
        .map(synthesize)
        .map(|source| {
            let line = source
                .lines()
                .find(|line| line.contains("let generated_character: char ="))
                .expect("synthesized source omitted char expression");
            line.to_owned()
        })
        .collect();

    assert!(generated.len() > 1);
    assert!(generated.iter().any(|line| line.contains("'🦀'")));
    assert!(generated.iter().any(|line| line.contains("'a'")));
    assert!(generated.iter().any(|line| line.contains("'é'")));
    assert!(generated.iter().any(|line| line.contains("'中'")));
    assert!(generated.iter().any(|line| line.contains("'\\n'")));
    assert!(generated.iter().any(|line| line.contains("'\\''")));
}

#[test]
fn synthesized_seed_uses_trailing_bytes_and_preserves_eight_byte_seeds() {
    let prefix = 0x0123_4567_89ab_cdef_u64;
    let prefix_bytes = prefix.to_le_bytes();
    assert_eq!(seed_from_data(&prefix_bytes), prefix);

    let mut with_zero_tail = prefix_bytes.to_vec();
    with_zero_tail.push(0);
    let mut with_one_tail = prefix_bytes.to_vec();
    with_one_tail.push(1);
    assert_ne!(
        seed_from_data(&with_zero_tail),
        seed_from_data(&with_one_tail)
    );
}

#[test]
fn empty_synthesized_argument_uses_the_supported_null_zero_str_pair() {
    let argument = synthesized_arg(1);
    assert!(argument.ptr.is_null());
    assert_eq!(argument.len, 0);
}

#[test]
fn synthesized_arguments_match_argv_zero_and_out_of_range_contracts() {
    let executable = synthesized_arg(0);
    assert!(!executable.ptr.is_null());
    assert_eq!(executable.len, "arandu-smith".len() as isize);

    for index in [-1, 3, 4, isize::MAX] {
        let argument = synthesized_arg(index);
        assert!(argument.ptr.is_null(), "index {index}");
        assert_eq!(argument.len, 0, "index {index}");
    }
}

#[test]
fn differential_string_interpolation_skips_empty_null_parts() {
    let source = concat!(
        "import std.env as env\n",
        "func main(): int {\n",
        "    let joined = \"${env.arg(1)}${env.arg(2)}\"\n",
        "    if joined != \"argument-two\" { return -1 }\n",
        "    return 0\n",
        "}\n",
    );

    let observation = check_source(source, true, true)
        .unwrap_or_else(|failure| panic!("empty-string interpolation failed: {failure:?}"));
    assert_eq!(observation.result, 0);
}

#[test]
fn emi_dead_code_mutation_is_deterministic_and_preserves_all_backend_results() {
    let source = synthesize(0);
    let mutated = inject_dead_pure_statement(&source, 0).unwrap();
    assert_eq!(mutated, inject_dead_pure_statement(&source, 0).unwrap());
    assert!(mutated.contains("if false {\n        let emi_dead: int ="));
    check_emi_pair(&source, 0, true, true).unwrap();
}

#[test]
fn emi_seed_index_handles_large_seeds_without_host_width_truncation() {
    assert_eq!(seed_index(u64::MAX, 7), Some(1));
    assert_eq!(seed_index(42, 7), Some(0));
    assert_eq!(seed_index(42, 0), None);
}

#[test]
fn emi_profiles_an_unexecuted_branch_before_mutating_it() {
    let source = concat!(
        "func main(): int {\n",
        "    let base: int = 7\n",
        "    if base > 0 {\n",
        "        let live: int = 1\n",
        "    } else {\n",
        "        let dead: int = 2\n",
        "    }\n",
        "    return base\n",
        "}\n",
    );
    let reference = check_source(source, true, true).unwrap();
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "dynamically unexecuted");
    assert!(mutated.contains("else {\n        let __arandu_smith_emi_dead_0: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_profile_records_taken_and_untaken_sampled_regions() {
    let source = concat!(
        "func main(): int {\n",
        "    let base: int = 7\n",
        "    if base > 0 {\n",
        "        let live: int = 1\n",
        "    } else {\n",
        "        let dead: int = 2\n",
        "    }\n",
        "    return base\n",
        "}\n",
    );
    let (reference, coverage) = check_source_with_block_coverage(source, true, true).unwrap();
    let (program, _) = arandu_parser::syntax::parse_dual(source);
    let profile = profile_main_regions(source, &program.unwrap(), 0, &reference, &coverage);

    assert!(
        !coverage.hits.is_empty(),
        "the JIT must report executed AMIR blocks"
    );
    assert!(
        coverage.hits.iter().any(|hit| hit.source_span.is_some()),
        "executed blocks should resolve to their source spans"
    );
    assert!(
        !coverage.source_blocks.is_empty(),
        "the JIT coverage must retain AMIR-to-source block mappings"
    );
    assert_eq!(
        source_region_hit_status(&coverage, source.find("let live").unwrap()),
        Some(true)
    );
    assert_eq!(
        source_region_hit_status(&coverage, source.find("let dead").unwrap()),
        Some(false)
    );
    assert!(profile.iter().any(|entry| entry.entered == Some(true)));
    assert!(profile.iter().any(|entry| entry.entered == Some(false)));
    assert!(profile.iter().all(|entry| entry.entered.is_some()));
    assert!(profile.len() <= MAX_EMI_PROFILE_PROBES);
}

#[test]
fn emi_profiles_nested_if_statement_branches() {
    let source = concat!(
        "func main(): int {\n",
        "    let base: int = 7\n",
        "    let __arandu_smith_emi_dead_0: int = 4\n",
        "    if base > 0 {\n",
        "        if base < 0 {\n",
        "            let dead: int = 2\n",
        "        } else {\n",
        "            let live: int = 3\n",
        "        }\n",
        "    }\n",
        "    return base\n",
        "}\n",
    );
    let reference = check_source(source, true, true).unwrap();
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "dynamically unexecuted");
    assert!(mutated.contains("if base < 0 {\n        let __arandu_smith_emi_dead_1: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_profiles_expression_if_branches() {
    let source = concat!(
        "func main(): int {\n",
        "    let base: int = 7\n",
        "    let selected = if base < 0 { 1 } else { 2 }\n",
        "    return selected + base\n",
        "}\n",
    );
    let reference = check_source(source, true, true).unwrap();
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "dynamically unexecuted");
    assert!(mutated.contains("if base < 0 {\n        let __arandu_smith_emi_dead_0: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_profiles_unentered_while_bodies() {
    let source = concat!(
        "func main(): int {\n",
        "    while false {\n",
        "        let dead: int = 2\n",
        "    }\n",
        "    return 7\n",
        "}\n",
    );
    let reference = check_source(source, true, true).unwrap();
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "dynamically unexecuted");
    assert!(mutated.contains("while false {\n        let __arandu_smith_emi_dead_0: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_profiles_unentered_c_style_for_bodies() {
    let source = concat!(
        "func main(): int {\n",
        "    for ; false; {\n",
        "        if true {\n",
        "            let nested_dead: int = 3\n",
        "        }\n",
        "        let dead: int = 2\n",
        "    }\n",
        "    return 7\n",
        "}\n",
    );
    let reference = check_source(source, true, true).unwrap();
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "dynamically unexecuted");
    assert!(mutated.contains("for ; false; {\n        let __arandu_smith_emi_dead_0: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_does_not_mark_entered_for_bodies_as_dead() {
    let source = concat!(
        "func main(): int {\n",
        "    let mut index = 0\n",
        "    for set index = 0; index < 2; set index = index + 1 {\n",
        "        let live: int = index\n",
        "    }\n",
        "    return index\n",
        "}\n",
    );
    let reference = check_source(source, true, true).unwrap();
    assert_eq!(reference.result, 2);
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "constant-false");
    assert!(mutated.contains("if false {\n        let emi_dead: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_does_not_treat_loop_with_early_return_as_unentered() {
    let source = concat!(
        "func main(): int {\n",
        "    while true {\n",
        "        return 7\n",
        "    }\n",
        "    return 0\n",
        "}\n",
    );
    let reference = check_source(source, true, true).unwrap();
    assert_eq!(reference.result, 7);
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "constant-false");
    assert!(mutated.contains("if false {\n        let emi_dead: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_profiles_match_statement_arms() {
    let source = concat!(
        "func main(): int {\n",
        "    let base: int = 7\n",
        "    match base {\n",
        "        7 => { let live: int = 1 }\n",
        "        _ => { let dead: int = 2 }\n",
        "    }\n",
        "    return base\n",
        "}\n",
    );
    let reference = check_source(source, true, true).unwrap();
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "dynamically unexecuted");
    assert!(mutated.contains("_ => {\n        let __arandu_smith_emi_dead_0: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_profiles_unentered_catch_blocks() {
    let source = concat!(
        "func ok(): Result<int, int> {\n",
        "    return Result.Ok(7)\n",
        "}\n",
        "func main(): int {\n",
        "    let value = ok() catch |error| {\n",
        "        let dead: int = error\n",
        "        0\n",
        "    }\n",
        "    return value\n",
        "}\n",
    );
    let (_, coverage) = check_source_with_block_coverage(source, false, false).unwrap();
    assert_eq!(
        source_region_hit_status(&coverage, source.find("let dead").unwrap()),
        Some(false)
    );
    let reference = check_source(source, true, true).unwrap();
    assert_eq!(reference.result, 7);
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "dynamically unexecuted");
    assert!(mutated.contains("catch |error| {\n        let __arandu_smith_emi_dead_0: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_profiles_match_expression_arms() {
    let source = concat!(
        "func main(): int {\n",
        "    let base: int = 7\n",
        "    let selected = match base { 0 => { 1 } _ => { 2 } }\n",
        "    return selected + base\n",
        "}\n",
    );
    let reference = check_source(source, true, true).unwrap();
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "dynamically unexecuted");
    assert!(mutated.contains("0 => {\n        let __arandu_smith_emi_dead_0: int ="));
    assert_eq!(check_source(&mutated, true, true).unwrap(), reference);
}

#[test]
fn emi_profiles_an_unexecuted_std_option_guard() {
    let source = include_str!("../../../../tests/emi-corpus/option-stdlib.aru");
    let reference = check_source(source, false, false).unwrap();
    let (mutated, insertion) = inject_emi_mutation(source, 0, &reference).unwrap();
    assert_eq!(insertion, "dynamically unexecuted");
    assert!(
        mutated.contains("if !present.isSome() {\n        let __arandu_smith_emi_dead_0: int ="),
        "unexpected EMI mutation placement:\n{mutated}"
    );
    assert_eq!(
        check_source(&mutated, true, true).unwrap(),
        check_source(source, true, true).unwrap()
    );
}

#[test]
fn synthesized_triangular_oracle_compares_all_backends_for_multiple_seeds() {
    let mut compared_variants = [false; 3];
    let mut compared_loop_forms = [false; 2];
    for seed in 0..12_u64 {
        let source = synthesize(seed);
        let base = source
            .lines()
            .find_map(|line| line.strip_prefix("    let base: int = "))
            .and_then(|value| value.parse::<usize>().ok())
            .expect("synthesized base literal");
        let variant = base % compared_variants.len();
        let loop_form = if source.contains("for loop_index =") {
            1
        } else {
            0
        };
        if !seed.is_multiple_of(64)
            && (!compared_variants[variant] || !compared_loop_forms[loop_form])
        {
            assert_synthesized_seed_matches_all_backends(seed);
            compared_variants[variant] = true;
            compared_loop_forms[loop_form] = true;
        }
    }
    assert!(
        compared_variants.into_iter().all(|compared| compared),
        "all three enum variants must be compared across backends"
    );
    assert!(
        compared_loop_forms.into_iter().all(|compared| compared),
        "both bounded loop forms must be compared across backends"
    );
}

#[test]
fn synthesized_generic_tuples_match_all_backends() {
    let mut pair_signatures = std::collections::BTreeSet::new();
    let mut triple_signatures = std::collections::BTreeSet::new();
    for seed in 0..32 {
        let generated = synthesize_with_oracle(seed);
        let pair_call = generated
            .source
            .lines()
            .find(|line| line.contains("let pair_value") && line.contains("make_pair<"))
            .expect("synthesized source includes the generic pair call");
        let pair_signature = pair_call
            .split_once("make_pair<")
            .and_then(|(_, tail)| tail.split_once(">("))
            .map(|(signature, _)| signature)
            .expect("pair call includes concrete type arguments");
        let new_pair_signature = pair_signatures.insert(pair_signature.to_owned());

        let triple_call = generated
            .source
            .lines()
            .find(|line| line.contains("let triple_value") && line.contains("make_triple<"))
            .expect("synthesized source includes the generic triple call");
        let triple_signature = triple_call
            .split_once("make_triple<")
            .and_then(|(_, tail)| tail.split_once(">("))
            .map(|(signature, _)| signature)
            .expect("triple call includes concrete type arguments");
        let new_triple_signature = triple_signatures.insert(triple_signature.to_owned());

        if !new_pair_signature && !new_triple_signature {
            continue;
        }

        let observation = check_source(&generated.source, true, true).unwrap_or_else(|failure| {
            panic!("generic tuple differential failed for seed {seed}: {failure:?}")
        });

        assert_eq!(observation.result, generated.expected_result, "seed {seed}");
        let expected = ExpectedObservation::synthesized(generated.expected_result);
        assert_eq!(observation.stdout, expected.stdout, "seed {seed}");
        assert_eq!(observation.stderr, expected.stderr, "seed {seed}");

        if pair_signatures.len() == 5 && triple_signatures.len() == 5 {
            break;
        }
    }
    assert_eq!(
        pair_signatures.len(),
        5,
        "all scalar pair rotations were tested"
    );
    assert_eq!(
        triple_signatures.len(),
        5,
        "all scalar triple rotations were tested"
    );
}

#[test]
fn nominal_enum_payload_equality_matches_all_backends_and_optimization_levels() {
    let source = r#"
enum Choice { Empty, Value(int), Pair(int, str), Nested(Option<int>) }
enum GenericChoice<T> { Empty, Value(T) }
func main(): int {
    let left = Option.Some(Choice.Value(7))
    let right = Option.Some(Choice.Value(7))
    if left != right { return 1 }
    if left == Option.Some(Choice.Value(8)) { return 2 }
    if left == Option.Some(Choice.Empty) { return 3 }
    let empty = Option.Some(Choice.Empty)
    if empty != Option.Some(Choice.Empty) { return 4 }
    let pair = Option.Some(Choice.Pair(7, "text"))
    if pair != Option.Some(Choice.Pair(7, "text")) { return 5 }
    if pair == Option.Some(Choice.Pair(8, "text")) { return 6 }
    if pair == Option.Some(Choice.Pair(7, "other")) { return 7 }
    let nested = Option.Some(Choice.Nested(Option.Some(9)))
    if nested != Option.Some(Choice.Nested(Option.Some(9))) { return 8 }
    if nested == Option.Some(Choice.Nested(Option.None)) { return 9 }
    let ok: Result<Choice, bool> = Result.Ok(Choice.Pair(3, "ok"))
    let sameOk: Result<Choice, bool> = Result.Ok(Choice.Pair(3, "ok"))
    let error: Result<Choice, bool> = Result.Err(false)
    if ok != sameOk { return 10 }
    if ok == error { return 11 }
    let generic: Option<GenericChoice<int>> = Option.Some(GenericChoice.Value(42))
    let sameGeneric: Option<GenericChoice<int>> = Option.Some(GenericChoice.Value(42))
    let differentGeneric: Option<GenericChoice<int>> = Option.Some(GenericChoice.Value(43))
    if generic != sameGeneric { return 12 }
    if generic == differentGeneric { return 13 }
    return 0
}
"#;
    let observation = check_source(source, true, true)
        .unwrap_or_else(|failure| panic!("nominal equality failed: {failure:?}"));
    assert_eq!(observation.result, 0);
}

#[test]
fn io_print_output_matches_all_backends_at_every_optimization_level() {
    let observation = check_source(
        "import io\nfunc main(): int { io.print(\"hello\"); io.print(\"\"); io.print(\"\\0world\"); return 17 }\n",
        true,
        true,
    )
    .unwrap_or_else(|failure| panic!("partial-output differential failed: {failure:?}"));

    assert_eq!(observation.result, 17);
    assert_eq!(observation.stdout, b"hello\0world");
    assert!(observation.stderr.is_empty());
}

#[test]
fn io_println_output_matches_all_backends_at_every_optimization_level() {
    let observation = check_source(
        "import io\nfunc main(): int { io.println(\"smith-output\"); return 17 }\n",
        true,
        true,
    )
    .unwrap_or_else(|failure| panic!("observable-output differential failed: {failure:?}"));

    assert_eq!(observation.result, 17);
    assert_eq!(observation.stdout, b"smith-output\n");
}

#[test]
fn io_eprint_output_matches_all_backends_at_every_optimization_level() {
    let observation = check_source(
        "import io\nfunc main(): int { io.eprint(\"smith-stderr\"); return 17 }\n",
        true,
        true,
    )
    .unwrap_or_else(|failure| panic!("observable-stderr differential failed: {failure:?}"));

    assert_eq!(observation.result, 17);
    assert_eq!(observation.stderr, b"smith-stderr");
}

#[test]
fn scalar_comparisons_match_all_backends_at_every_optimization_level() {
    let source = r#"
func main(): int {
    let signed: int = -3
    let unsigned: uint = 7 as uint
    let floating: float = 2.5
    if signed == -3 && signed != -2 && signed < -2 && signed <= -3 && signed > -4 && signed >= -3 && unsigned == 7 as uint && unsigned != 8 as uint && unsigned < 8 as uint && unsigned <= 7 as uint && unsigned > 6 as uint && unsigned >= 7 as uint && floating == 2.5 && floating != 3.0 && floating < 3.0 && floating <= 2.5 && floating > 2.0 && floating >= 2.5 {
        return 0
    }
    return 1
}

"#;
    let observation = check_source(source, true, true)
        .unwrap_or_else(|failure| panic!("float comparison differential failed: {failure:?}"));

    assert_eq!(observation.result, 0);
}

#[test]
fn fixed_width_int_hash_matches_all_backends_and_optimization_levels() {
    let source = r#"
import std.core.hash as hash

func main(): int {
    let mut hasher = hash.FnvHasher.new()
    hasher.writeInt(240)
    if hasher.finish() as int != -490752091 { return 1 }

    hasher = hash.FnvHasher.new()
    hasher.writeInt(-1)
    if hasher.finish() as int != 1042954577 { return 2 }
    return 0
}
"#;
    let observation = check_source(source, true, true)
        .unwrap_or_else(|failure| panic!("fixed-width int hash diverged: {failure:?}"));

    assert_eq!(observation.result, 0);
}

#[test]
fn escaped_character_literals_have_scalar_semantics_across_all_backends() {
    let source = r#"
func main(): int {
    let newline: char = '\n'
    let quote: char = '\''
    let slash: char = '\\'
    if newline < 'Z' && newline == '\u{A}' && quote == '\u{27}' && slash == '\u{5C}' {
        return 0
    }
    return 1
}
"#;
    let observation = check_source(source, true, true)
        .unwrap_or_else(|failure| panic!("escaped character differential failed: {failure:?}"));
    assert_eq!(observation.result, 0);
}

#[test]
fn synthesized_output_matches_independent_oracle_and_survives_emi() {
    let generated = synthesize_with_oracle(0);
    assert!(generated.source.contains("identity<uint>("));
    assert!(generated.source.contains("identity<float>("));
    assert!(generated.source.contains("if float_result != "));
    check_emi_pair_with_expected(
        &generated.source,
        0,
        true,
        true,
        Some(ExpectedObservation::synthesized(generated.expected_result)),
    )
    .unwrap_or_else(|failure| panic!("synthesized observation failed: {failure:?}"));
}

#[test]
fn independent_result_oracle_rejects_backend_results_with_wrong_semantics() {
    let failure = check_emi_pair_with_expected(
        "func main(): int { return 3 }\n",
        0,
        true,
        true,
        Some(ExpectedObservation::synthesized(2)),
    )
    .unwrap_err();

    assert_eq!(failure.kind, "synthesized-result-mismatch");
    assert!(failure.message.contains("expected 2"));
}

#[test]
fn independent_output_oracle_rejects_wrong_stderr() {
    let failure = check_emi_pair_with_expected(
            "import io\nfunc main(): int { io.println(\"result-even\"); io.eprint(\"wrong\"); return 2 }\n",
            0,
            false,
            false,
            Some(ExpectedObservation::synthesized(2)),
        )
        .unwrap_err();

    assert_eq!(failure.kind, "synthesized-stderr-mismatch");
    assert!(failure.message.contains("expected \"result-even\""));
}

#[test]
fn c_output_normalization_removes_crlf_translation_only() {
    assert_eq!(
        normalize_captured_output(b"first\r\r\nsecond\nthird\r".to_vec()),
        b"first\r\nsecond\nthird\r"
    );
}

#[test]
fn generic_owned_value_can_be_returned_and_used_by_every_backend() {
    let source = r#"
import std.alloc.vec as vec

func transfer<T>(own value: T): T {
    return value
}

func main(): int {
    let mut values = transfer<vec.Vec<int>>(vec.Vec<int>.new())
    if !values.tryPush(42) {
        return -1
    }
    let length = values.len() as int
    return length
}
"#;

    check_emi_pair(source, 0, true, true).unwrap();
}

#[test]
fn emi_corpus_mutation_preserves_results_across_every_backend() {
    for (index, (name, source, expected)) in EMI_CORPUS.iter().enumerate() {
        let seed = index as u64;
        let mutated = inject_dead_pure_statement(source, seed)
            .unwrap_or_else(|| panic!("cannot inject dead code into EMI corpus {name}"));
        assert_ne!(&mutated, source, "EMI mutation did not change {name}");
        let observation = check_emi_pair_with_expected(source, seed, true, true, *expected)
            .unwrap_or_else(|failure| panic!("EMI corpus {name} failed: {failure:?}\n{source}"));
        if let Some(expected) = expected {
            assert_eq!(
                observation.result, expected.result,
                "EMI example {name} returned an unexpected value"
            );
            assert_eq!(
                observation.stdout.as_slice(),
                expected.stdout,
                "EMI example {name} wrote unexpected stdout"
            );
            assert_eq!(
                observation.stderr.as_slice(),
                expected.stderr,
                "EMI example {name} wrote unexpected stderr"
            );
        }
    }
}

#[test]
fn hash_map_collision_corpus_matches_all_backends_and_preserves_its_oracle() {
    let (name, source, expected) = EMI_CORPUS
        .iter()
        .find(|(name, _, _)| *name == "hash-map-collisions")
        .expect("HashMap collision fixture is registered in the EMI corpus");
    let observation = check_emi_pair_with_expected(source, 17, true, true, *expected)
        .unwrap_or_else(|failure| panic!("EMI corpus {name} failed: {failure:?}"));

    assert_eq!(observation.result, 0);
    assert_eq!(observation.stdout, b"");
    assert_eq!(observation.stderr, b"");
}

#[test]
fn stdlib_bitset_emi_matches_all_backends_and_preserves_its_oracle() {
    let (name, source, expected) = EMI_CORPUS
        .iter()
        .find(|(name, _, _)| *name == "bitset-stdlib")
        .expect("BitSet fixture is registered in the EMI corpus");
    let observation = check_emi_pair_with_expected(source, 18, true, true, *expected)
        .unwrap_or_else(|failure| panic!("EMI corpus {name} failed: {failure:?}"));

    assert_eq!(observation.result, 0);
    assert_eq!(observation.stdout, b"");
    assert_eq!(observation.stderr, b"");
}

#[test]
fn stdlib_smallvec_spill_emi_matches_all_backends_and_preserves_its_oracle() {
    let (name, source, expected) = EMI_CORPUS
        .iter()
        .find(|(name, _, _)| *name == "smallvec-spill")
        .expect("SmallVec spill fixture is registered in the EMI corpus");
    let observation = check_emi_pair_with_expected(source, 19, true, true, *expected)
        .unwrap_or_else(|failure| panic!("EMI corpus {name} failed: {failure:?}"));

    assert_eq!(observation.result, 0);
    assert_eq!(observation.stdout, b"");
    assert_eq!(observation.stderr, b"");
}

#[test]
fn stdlib_arena_lifecycle_emi_matches_all_backends_and_preserves_its_oracle() {
    let (name, source, expected) = EMI_CORPUS
        .iter()
        .find(|(name, _, _)| *name == "arena-lifecycle")
        .expect("Arena lifecycle fixture is registered in the EMI corpus");
    let observation = check_emi_pair_with_expected(source, 20, true, true, *expected)
        .unwrap_or_else(|failure| panic!("EMI corpus {name} failed: {failure:?}"));

    assert_eq!(observation.result, 0);
    assert_eq!(observation.stdout, b"");
    assert_eq!(observation.stderr, b"");
}

#[test]
fn stdlib_string_lifecycle_emi_matches_all_backends_and_preserves_its_oracle() {
    let (name, source, expected) = EMI_CORPUS
        .iter()
        .find(|(name, _, _)| *name == "string-lifecycle")
        .expect("String lifecycle fixture is registered in the EMI corpus");
    let observation = check_emi_pair_with_expected(source, 21, true, true, *expected)
        .unwrap_or_else(|failure| panic!("EMI corpus {name} failed: {failure:?}"));

    assert_eq!(observation.result, 0);
    assert_eq!(observation.stdout, b"");
    assert_eq!(observation.stderr, b"");
}

#[test]
fn stdlib_num_boundaries_emi_matches_all_backends_and_preserves_its_oracle() {
    let (name, source, expected) = EMI_CORPUS
        .iter()
        .find(|(name, _, _)| *name == "num-boundaries")
        .expect("numeric boundary fixture is registered in the EMI corpus");
    let observation = check_emi_pair_with_expected(source, 22, true, true, *expected)
        .unwrap_or_else(|failure| panic!("EMI corpus {name} failed: {failure:?}"));

    assert_eq!(observation.result, 0);
    assert_eq!(observation.stdout, b"");
    assert_eq!(observation.stderr, b"");
}

#[test]
fn emi_seed_files_select_their_named_corpus_entries() {
    let fixtures = [
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-option.seed"),
            "option-stdlib",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-vec-free.seed"),
            "example-vec-free-functions",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-vec-methods.seed"),
            "example-vec-methods",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-core-str.seed"),
            "example-core-str",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-gen-arena.seed"),
            "example-gen-arena",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-struct-enum.seed"),
            "example-struct-enum",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-result-option.seed"),
            "example-result-option",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-interpolation.seed"),
            "example-interpolation",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-match-result.seed"),
            "example-match-result",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-vec-capacity.seed"),
            "example-vec-capacity",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-example-result-stdlib.seed"),
            "result-stdlib",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-hash-map-collisions.seed"),
            "hash-map-collisions",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-bitset-stdlib.seed"),
            "bitset-stdlib",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-smallvec-spill.seed"),
            "smallvec-spill",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-arena-lifecycle.seed"),
            "arena-lifecycle",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-string-lifecycle.seed"),
            "string-lifecycle",
        ),
        (
            include_str!("../../../../tests/fuzz-regressions/emi-num-boundaries.seed"),
            "num-boundaries",
        ),
    ];

    for (fixture, expected_name) in fixtures {
        let encoded = fixture
            .lines()
            .find(|line| !line.starts_with("encoding="))
            .expect("EMI seed fixture contains its hex payload");
        let bytes = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
                    .expect("EMI seed payload is hexadecimal")
            })
            .collect::<Vec<_>>();
        let seed = seed_from_data(&bytes);
        let index = (seed % EMI_CORPUS.len() as u64) as usize;
        assert_eq!(
            EMI_CORPUS[index].0, expected_name,
            "seed fixture: {fixture}"
        );
    }
}

#[test]
fn synthesis_exercises_array_aggregate_enum_and_control_flow() {
    let source = synthesize(0);
    let main_source = source
        .split_once("func main(): int {")
        .map(|(_, body)| body)
        .expect("synthesized source has a main function");
    assert!(source.contains("let values = ["));
    assert!(source.contains("let index: int = base % 3"));
    assert!(source.contains("values[index]"));
    for line in main_source.lines().filter(|line| line.contains("return -")) {
        let code = line
            .split_once("return -")
            .and_then(|(_, value)| value.split_whitespace().next())
            .and_then(|value| value.parse::<i64>().ok())
            .map(|value| -value)
            .expect("generated guard exit has an integer code");
        assert!(
            (SYNTH_FAILURE_CODE_BASE - SYNTH_FAILURE_CODE_COUNT + 1..=SYNTH_FAILURE_CODE_BASE)
                .contains(&code),
            "guard failure code {code} is outside the reserved range"
        );
        assert!(
            code < -MAX_SYNTH_RESULT_MAGNITUDE,
            "guard failure code {code} can collide with a valid oracle result"
        );
    }
    assert!(source.contains("values[(index + 1) % 3]"));
    assert!(source.contains("struct Sample"));
    assert!(source.contains("struct OwnedPack"));
    assert!(source.contains("func make_owned_pack(value: int, marker: int): OwnedPack"));
    assert!(source.contains("return OwnedPack { items: items, marker: marker }"));
    assert!(source.contains("transfer<OwnedPack>(make_owned_pack(sample.left, sample.right))"));
    assert!(source.contains("owned_pack.items.len() != 1 as usize"));
    assert!(source.contains("func identity<T>(value: T): T"));
    assert!(source.contains("func make_pair<T, U>(left: T, enabled: U): (T, U)"));
    assert!(source.contains("let pair_value, pair_enabled = make_pair<"));
    assert!(
        source.contains("func make_triple<T, U, V>(left: T, enabled: U, character: V): (T, U, V)")
    );
    assert!(source.contains("let triple_value, triple_enabled, triple_character = make_triple<"));
    assert!(source.contains("triple_character != "));
    assert!(source.contains("func transfer<T>(own value: T): T"));
    assert!(source.contains("func relay<T>(own value: T): T"));
    assert!(source.contains("func make_vec<T>(): vec.Vec<T>"));
    assert!(source.contains("vec.Vec<uint>"));
    assert!(source.contains("relay<vec.Vec<uint>>(make_vec<uint>())"));
    assert!(source.contains("unsigned_values.tryPush(uint_result)"));
    assert!(source.contains("vec.Vec<core_result.Result<int, bool>>"));
    assert!(source.contains("relay<vec.Vec<core_result.Result<int, bool>>>("));
    assert!(source.contains("Result.Err(sample.enabled)"));
    assert!(source.contains("relay<vec.Vec<int>>(make_vec<int>())"));
    assert!(source.contains("transfer<vec.Vec<int>>(relay<vec.Vec<int>>(make_vec<int>()))"));
    assert!(source.contains("return vec.Vec<T>.new()"));
    assert!(source.contains("identity<int>("));
    assert!(source.contains("identity<bool>("));
    assert!(source.contains("env.arg(0) != \"arandu-smith\""));
    assert!(source.contains("env.arg(3) != \"\" || env.arg(-1) != \"\""));
    assert!(source.contains("func read_ref(value: ref int): int"));
    assert!(source.contains("func read_exclusive(value: mut ref int): int"));
    assert!(source.contains("read_ref(ref borrow_target)"));
    assert!(source.contains("read_exclusive(mut ref borrow_target)"));
    assert!(source.contains("import std.alloc.vec as vec"));
    assert!(source.contains("import std.core.slice as slice"));
    assert!(source.contains("import std.core.result as core_result"));
    assert!(source.contains("core_result.Result<int, bool>"));
    assert!(source.contains("boxed.isOk()"));
    assert!(source.contains("boxed.isErr()"));
    assert!(source.contains("boxed.unwrapOr(result + 1)"));
    assert!(source.contains("core_result.Result<char, bool>"));
    assert!(source.contains("boxed_character_ok: core_result.Result<char, bool>"));
    assert!(source.contains("boxed_character_err: core_result.Result<char, bool>"));
    assert!(source.contains("boxed_character_ok.unwrapOr('q')"));
    assert!(source.contains("boxed_character_err.unwrapOr('q')"));
    assert!(source.contains("Result<int, char> = Result.Ok(result)"));
    assert!(source.contains("Result<int, char> = Result.Err(sample.character)"));
    assert!(source.contains("import io"));
    assert!(source.contains("import std.env as env"));
    assert!(source.contains("env.argsLen() != 3"));
    assert!(source.contains("env.arg(1) != \"\""));
    assert!(source.contains("env.arg(2) != \"argument-two\""));
    assert!(source.contains("if result % 2 == 0"));
    assert!(source.contains("io.println(\"result-even\")"));
    assert!(source.contains("io.println(\"result-odd\")"));
    assert!(source.contains("io.eprint(\"result-even\")"));
    assert!(source.contains("io.eprint(\"result-odd\")"));
    assert!(source.contains("dynamic.tryReserve(1 as usize)"));
    assert!(source.contains("slice.len<int>(dynamic.asSlice())"));
    assert!(source.contains("dynamic.getCopy(dynamic_len as usize)"));
    assert!(source.contains("while dynamic_count < 9"));
    assert!(source.contains("dynamic.tryPush(sample.left + dynamic_count)"));
    assert!(source.contains("transfer<vec.Vec<bool>>(make_vec<bool>())"));
    assert!(source.contains("flags.tryPush(sample.enabled)"));
    assert!(source.contains("slice.len<bool>(flags.asSlice())"));
    assert!(source.contains("transfer<vec.Vec<Choice>>(make_vec<Choice>())"));
    assert!(source.contains("choices.tryPush(Choice.Flag(sample.enabled))"));
    assert!(source.contains("choices.pop()"));
    assert!(source.contains("Some(Choice.Flag(value)) => value"));
    assert!(source.contains("Choice.Letter(sample.character)"));
    assert!(source.contains("Some(Choice.Letter(value)) => value"));
    assert!(source.contains("recovered_character != sample.character"));
    assert!(source.contains("transfer<vec.Vec<char>>(make_vec<char>())"));
    assert!(source.contains("characters.tryPush(sample.character)"));
    assert!(source.contains("slice.len<char>(characters.asSlice())"));
    assert!(source.contains("characters.getCopy(0 as usize)"));
    assert!(source.contains("recovered_vector_character != sample.character"));
    assert!(source.contains("transfer<vec.Vec<float>>(make_vec<float>())"));
    assert!(source.contains("floats.tryPush(float_result)"));
    assert!(source.contains("floats.getCopy(0 as usize)"));
    assert!(source.contains("recovered_vector_float != float_result"));
    assert!(source.contains("dynamic.getCopy(dynamic_index)"));
    assert!(source.contains("if dynamic_len != 9"));
    assert!(source.contains("if dynamic_tail != sample.left + 8"));
    assert!(source.contains("dynamic.set(8 as usize, sample.right)"));
    assert!(source.contains("dynamic.pop()"));
    assert!(source.contains("dynamic.clear()"));
    assert!(source.contains("Some(_) => true"));
    assert!(source.contains("func adjust(value: int, enabled: bool): int"));
    assert!(source.contains(
            "let adjusted = adjust(shared_value, sample.enabled) + adjust(exclusive_value, !sample.enabled)"
        ));
    assert!(source.contains("enabled: bool"));
    assert!(source.contains("Sample { left:"));
    assert!(source.contains("enabled: "));
    assert!(source.contains("enum Choice"));
    assert!(source.contains("while remaining > 0"));
    assert!(source.contains("remaining = remaining - 1"));
    assert!(source.contains(&format!("base % {}", MAX_LOOP_TRIPS + 1)));
    assert!(source.contains("if "));
    assert!(source.contains("Choice.Left(adjusted)"));
    assert!(source.contains("Choice.Right(sample.right)"));
    assert!(source.contains("Choice.Flag(sample.enabled)"));
    assert!(source.contains("Choice.Left(value) => value"));
    assert!(source.contains("Choice.Right(value) => value"));
    assert!(source.contains("Choice.Flag(enabled) => if enabled"));
    assert!(source.contains("Choice.Letter(character) => if character == sample.character"));
}

#[test]
fn synthesized_triple_varies_type_combinations_across_seeds() {
    let mut pair_signatures = std::collections::BTreeSet::new();
    let mut signatures = std::collections::BTreeSet::new();
    for seed in 0..32 {
        let source = synthesize(seed);
        let pair = source
            .lines()
            .find(|line| line.contains("let pair_value") && line.contains("make_pair<"))
            .expect("synthesized source includes the heterogeneous pair call");
        let (_, pair_signature) = pair
            .split_once("make_pair<")
            .expect("pair call includes type arguments");
        let pair_signature = pair_signature
            .split_once(">(")
            .expect("pair call closes its type arguments")
            .0;
        pair_signatures.insert(pair_signature.to_owned());
        for ty in pair_signature.split(", ") {
            assert!(
                pair.contains(&format!("identity<{ty}>(")),
                "pair argument of type {ty} should pass through identity<T>: {pair}"
            );
        }

        let call = source
            .lines()
            .find(|line| line.contains("let triple_value") && line.contains("make_triple<"))
            .expect("synthesized source includes the heterogeneous triple call");
        let (_, signature) = call
            .split_once("make_triple<")
            .expect("triple call includes type arguments");
        let signature = signature
            .split_once(">(")
            .expect("triple call closes its type arguments")
            .0;
        signatures.insert(signature.to_owned());
        for ty in signature.split(", ") {
            assert!(
                call.contains(&format!("identity<{ty}>(")),
                "triple argument of type {ty} should pass through identity<T>: {call}"
            );
        }
    }

    assert!(
        signatures.len() == 7,
        "type-directed triples should cover all rotations: {signatures:?}"
    );
    assert!(
        pair_signatures.len() == 7,
        "type-directed pairs should cover all rotations: {pair_signatures:?}"
    );
    for ty in ["int", "uint", "usize", "isize", "bool", "float", "char"] {
        assert!(
            pair_signatures
                .iter()
                .any(|signature| signature.contains(ty)),
            "no generated pair included {ty}: {pair_signatures:?}"
        );
    }
    for ty in ["int", "uint", "usize", "isize", "bool", "float", "char"] {
        assert!(
            signatures.iter().any(|signature| signature.contains(ty)),
            "no generated triple included {ty}: {signatures:?}"
        );
    }
}

#[test]
fn enum_if_join_matches_across_optimization_levels() {
    let source = include_str!("../../../../tests/regressions/enum-if-join.aru");
    let mut host = AnalysisHost::new();
    let file = host.new_file("known-failure.aru".into(), source.to_owned());
    let lowered = arandu_query::lower_amir(host.db(), file);
    assert!(lowered.type_check.diagnostics.is_empty());
    let result = execute_cranelift(
        &lowered.amir,
        &lowered.type_check.symbols,
        &lowered.type_check.type_info,
    );
    assert_eq!(result.unwrap().result, 11);

    for level in [
        arandu_mir::OptLevel::O0,
        arandu_mir::OptLevel::O1,
        arandu_mir::OptLevel::O2,
    ] {
        let mut optimized = lowered.amir.clone();
        arandu_mir::optimize_amir_checked_with_level(
            &mut optimized,
            &lowered.type_check.symbols,
            &lowered.type_check.type_info.type_interner,
            level,
        )
        .unwrap();
        let result = execute_cranelift(
            &optimized,
            &lowered.type_check.symbols,
            &lowered.type_check.type_info,
        );
        assert_eq!(result.unwrap().result, 11, "optimizer level {level:?}");
    }
}

#[test]
fn historical_enum_join_failures_match_all_backends_and_optimization_levels() {
    for (name, source) in [
        (
            "minimal Cranelift join",
            include_str!("../../../../tests/regressions/enum-if-join.aru"),
        ),
        (
            "optimizer join",
            include_str!("../../../../tests/regressions/enum-optimizer-join.aru"),
        ),
        (
            "Wasm match join",
            include_str!("../../../../tests/regressions/enum-wasm-match.aru"),
        ),
        (
            "wasm fat pointer struct projection",
            include_str!("../../../../tests/regressions/wasm-fat-pointer-struct-projection.aru"),
        ),
        (
            "wasm memory local and niche",
            include_str!("../../../../tests/regressions/wasm-memory-local-and-niche.aru"),
        ),
    ] {
        check_source(source, true, true)
            .unwrap_or_else(|failure| panic!("{name} regression failed: {failure:?}"));
    }
}

#[test]
fn same_named_generic_functions_keep_their_module_identity_in_every_backend() {
    let source = include_str!("../../../../tests/regressions/generic-module-name-collision.aru");
    let result = check_source(source, true, true)
        .unwrap_or_else(|failure| panic!("generic module identity regression failed: {failure:?}"));
    assert_eq!(
        result.result, 2,
        "Vec length and slice length should both be one"
    );
}

#[test]
fn c_backend_matches_cranelift_for_a_reproducible_seed() {
    for seed in [0u64, 1] {
        run_c(&seed.to_le_bytes());
    }
}

#[test]
fn wasm_backend_matches_cranelift_for_a_reproducible_seed() {
    for seed in [0u64, 1] {
        run_wasm(&seed.to_le_bytes());
    }
}

#[test]
fn minimized_failure_is_confirmed_again_before_reporting() {
    let source = concat!(
        "func helper(): int { return 1 }\n",
        "func main(): int { return 7 }\n",
    );
    let mut oracle_calls = 0;
    let (result, confirmed) = shrink_and_confirm(source, 8, |candidate| {
        oracle_calls += 1;
        candidate.contains("return 7")
    });

    assert!(result.reproduced);
    assert!(confirmed);
    assert!(result.reductions > 0);
    assert_eq!(oracle_calls, result.attempts + 1);
    assert!(result.source.contains("return 7"));
}

#[test]
fn synthesized_try_operator_and_match_case_matches_oracle() {
    let program = synthesize_with_oracle(32);
    assert!(program.source.contains("func generated_try_pipeline"));
    assert!(program.source.contains("generated_try_step"));
    assert!(program.source.contains("generated_match_range"));
    let observation =
        check_source(&program.source, true, true).expect("execution across all backends");
    assert_eq!(observation.result, program.expected_result);
}

#[test]
fn test_iter_slice_chain_across_all_backends() {
    let source = concat!(
        "import std.alloc.vec as vec\n",
        "import std.core.slice as slice\n",
        "import std.core.iter as iter\n",
        "import std.core.option as core_option\n\n",
        "struct VecIter {\n",
        "    items: vec.Vec<int>\n",
        "    index: usize\n",
        "}\n\n",
        "func make_vec_iter(v: vec.Vec<int>): VecIter {\n",
        "    return VecIter { items: v, index: 0 as usize }\n",
        "}\n\n",
        "func VecIter.next(self: mut ref VecIter): Option<int> {\n",
        "    let item = self.items.getCopy(self.index)\n",
        "    if self.index < self.items.len() {\n",
        "        self.index = self.index + (1 as usize)\n",
        "    }\n",
        "    return item\n",
        "}\n\n",
        "func main(): int {\n",
        "    let mut v = vec.Vec<int>.new()\n",
        "    v.push(10)\n",
        "    v.push(20)\n",
        "    v.push(30)\n",
        "    v.push(40)\n",
        "    v.push(50)\n",
        "    let it = make_vec_iter(v)\n",
        "    let taken = iter.take(it, 4 as uint)\n",
        "    let skipped = iter.skip(taken, 1 as uint)\n",
        "    let stepped = iter.stepBy(skipped, 2 as uint)\n",
        "    let total = iter.count(stepped)\n",
        "    return total as int\n",
        "}\n",
    );
    let observation = check_source(source, true, true).expect("execution across all backends");
    // Elements: [10, 20, 30, 40, 50]
    // take 4: [10, 20, 30, 40]
    // skip 1: [20, 30, 40]
    // stepBy 2: [20, 40] -> count = 2
    assert_eq!(observation.result, 2);
}

#[test]
fn test_slice_split_and_cuts_across_all_backends() {
    let source = concat!(
        "import std.alloc.vec as vec\n",
        "import std.core.slice as slice\n",
        "import std.core.iter as iter\n\n",
        "struct VecIter {\n",
        "    items: vec.Vec<int>\n",
        "    index: usize\n",
        "}\n\n",
        "func make_vec_iter(v: vec.Vec<int>): VecIter {\n",
        "    return VecIter { items: v, index: 0 as usize }\n",
        "}\n\n",
        "func VecIter.next(self: mut ref VecIter): Option<int> {\n",
        "    let item = self.items.getCopy(self.index)\n",
        "    if self.index < self.items.len() {\n",
        "        self.index = self.index + (1 as usize)\n",
        "    }\n",
        "    return item\n",
        "}\n\n",
        "func main(): int {\n",
        "    let mut v = vec.Vec<int>.new()\n",
        "    v.push(100)\n",
        "    v.push(200)\n",
        "    v.push(300)\n",
        "    v.push(400)\n",
        "    let s = v.asSlice()\n",
        "    let empty_start = match slice.subslice<int>(s, 0 as usize, 0 as usize) {\n",
        "        Some(sub) => slice.len<int>(sub) == (0 as usize)\n",
        "        None => false\n",
        "    }\n",
        "    if !empty_start { return -1 }\n",
        "    let empty_end = match slice.subslice<int>(s, 4 as usize, 0 as usize) {\n",
        "        Some(sub) => slice.len<int>(sub) == (0 as usize)\n",
        "        None => false\n",
        "    }\n",
        "    if !empty_end { return -2 }\n",
        "    let head = match slice.subslice<int>(s, 0 as usize, 2 as usize) {\n",
        "        Some(sub) => slice.len<int>(sub) == (2 as usize) && sub[0] == 100 && sub[1] == 200\n",
        "        None => false\n",
        "    }\n",
        "    if !head { return -3 }\n",
        "    let tail = match slice.subslice<int>(s, 2 as usize, 2 as usize) {\n",
        "        Some(sub) => slice.len<int>(sub) == (2 as usize) && sub[0] == 300 && sub[1] == 400\n",
        "        None => false\n",
        "    }\n",
        "    if !tail { return -4 }\n",
        "    let oob1 = match slice.subslice<int>(s, 0 as usize, 5 as usize) {\n",
        "        Some(_) => false\n",
        "        None => true\n",
        "    }\n",
        "    if !oob1 { return -5 }\n",
        "    let oob2 = match slice.subslice<int>(s, 5 as usize, 0 as usize) {\n",
        "        Some(_) => false\n",
        "        None => true\n",
        "    }\n",
        "    let split_check = match slice.splitAt<int>(s, 2 as usize) {\n",
        "        Some(sp) => slice.len<int>(sp.left) == (2 as usize) && slice.len<int>(sp.right) == (2 as usize) && sp.left[0] == 100 && sp.right[1] == 400\n",
        "        None => false\n",
        "    }\n",
        "    if !split_check { return -7 }\n",
        "    let split_oob = match slice.splitAt<int>(s, 5 as usize) {\n",
        "        Some(_) => false\n",
        "        None => true\n",
        "    }\n",
        "    if !split_oob { return -8 }\n",
        "    let it = make_vec_iter(v)\n",
        "    let enumerated = iter.enumerate(it)\n",
        "    let taken_enum = iter.take(enumerated, 3 as uint)\n",
        "    let mut enum_it = taken_enum\n",
        "    let mut loop_sum: int = 0\n",
        "    while true {\n",
        "        match enum_it.next() {\n",
        "            Some(item) => {\n",
        "                if item.index == (1 as uint) {\n",
        "                    loop_sum = loop_sum + item.value\n",
        "                }\n",
        "            }\n",
        "            None => { break }\n",
        "        }\n",
        "    }\n",
        "    if loop_sum != 200 { return -9 }\n",
        "    return 42\n",
        "}\n",
    );
    let observation = check_source(source, true, true).expect("execution across all backends");
    assert_eq!(observation.result, 42);
}

#[test]
fn test_seeded_slice_iter_differential_across_all_backends() {
    let source = synthesize(8);
    assert!(source.contains("gen_empty_s"));
    assert!(source.contains("gen_full_s"));
    assert!(source.contains("gen_iter_chain_count"));
    let observation = check_source(&source, true, true).expect("execution across all backends");
    assert!(
        observation.result >= 0,
        "unexpected error return code: {}",
        observation.result
    );
}

#[test]
fn test_option_result_cmp_across_all_backends() {
    let source = concat!(
        "import std.core.option as core_option\n",
        "import std.core.result as core_result\n",
        "import std.core.cmp as cmp\n\n",
        "struct PriorityItem {\n",
        "    priority: int\n",
        "    payload: int\n",
        "}\n\n",
        "func PriorityItem.cmp(self: ref PriorityItem, other: ref PriorityItem): cmp.Ordering {\n",
        "    if self.priority < other.priority {\n",
        "        return cmp.Ordering.Less\n",
        "    }\n",
        "    if self.priority > other.priority {\n",
        "        return cmp.Ordering.Greater\n",
        "    }\n",
        "    return cmp.Ordering.Equal\n",
        "}\n\n",
        "func main(): int {\n",
        "    // 1. Option checks\n",
        "    let some_val: Option<int> = Option.Some(42)\n",
        "    let none_val: Option<int> = Option.None\n",
        "    if !some_val.isSome() || some_val.isNone() { return -1 }\n",
        "    if none_val.isSome() || !none_val.isNone() { return -2 }\n",
        "    if some_val.unwrapOr(0) != 42 { return -3 }\n",
        "    if none_val.unwrapOr(100) != 100 { return -4 }\n",
        "    let nested_opt: Option<Option<int>> = Option.Some(Option.Some(77))\n",
        "    let nested_ok = match nested_opt {\n",
        "        Some(inner) => match inner {\n",
        "            Some(v) => v == 77\n",
        "            None => false\n",
        "        }\n",
        "        None => false\n",
        "    }\n",
        "    if !nested_ok { return -5 }\n",
        "    let inner_none: Option<int> = Option.None\n",
        "    let nested_none: Option<Option<int>> = Option.Some(inner_none)\n",
        "    let nested_none_ok = match nested_none {\n",
        "        Some(inner) => match inner {\n",
        "            Some(_) => false\n",
        "            None => true\n",
        "        }\n",
        "        None => false\n",
        "    }\n",
        "    if !nested_none_ok { return -6 }\n",
        "\n",
        "    // 2. Result checks\n",
        "    let ok_res: Result<int, int> = Result.Ok(55)\n",
        "    let err_res: Result<int, int> = Result.Err(99)\n",
        "    if !ok_res.isOk() || ok_res.isErr() { return -7 }\n",
        "    if err_res.isOk() || !err_res.isErr() { return -8 }\n",
        "    if ok_res.unwrapOr(0) != 55 { return -9 }\n",
        "    if err_res.unwrapOr(123) != 123 { return -10 }\n",
        "    let ok_to_opt = ok_res.ok()\n",
        "    if !ok_to_opt.isSome() || ok_to_opt.unwrapOr(0) != 55 { return -11 }\n",
        "    let err_to_opt = err_res.ok()\n",
        "    if err_to_opt.isSome() || !err_to_opt.isNone() { return -12 }\n",
        "    let ok_to_err = ok_res.err()\n",
        "    if ok_to_err.isSome() || !ok_to_err.isNone() { return -13 }\n",
        "    let err_to_err = err_res.err()\n",
        "    if !err_to_err.isSome() || err_to_err.unwrapOr(0) != 99 { return -14 }\n",
        "\n",
        "    // 3. Ordering enum & helpers\n",
        "    if !cmp.Ordering.Less.isLess() || cmp.Ordering.Less.isEqual() || cmp.Ordering.Less.isGreater() { return -15 }\n",
        "    if cmp.Ordering.Equal.isLess() || !cmp.Ordering.Equal.isEqual() || cmp.Ordering.Equal.isGreater() { return -16 }\n",
        "    if cmp.Ordering.Greater.isLess() || cmp.Ordering.Greater.isEqual() || !cmp.Ordering.Greater.isGreater() { return -17 }\n",
        "    if !cmp.Ordering.Equal.then(cmp.Ordering.Less).isLess() { return -18 }\n",
        "    if !cmp.Ordering.Equal.then(cmp.Ordering.Greater).isGreater() { return -19 }\n",
        "    if !cmp.Ordering.Less.then(cmp.Ordering.Greater).isLess() { return -20 }\n",
        "    if !cmp.Ordering.Greater.then(cmp.Ordering.Less).isGreater() { return -21 }\n",
        "\n",
        "    // 4. Scalar comparison helpers\n",
        "    if cmp.min(10, 20) != 10 { return -22 }\n",
        "    if cmp.min(20, 10) != 10 { return -23 }\n",
        "    if cmp.max(10, 20) != 20 { return -24 }\n",
        "    if cmp.max(20, 10) != 20 { return -25 }\n",
        "    if cmp.clamp(5, 10, 20) != 10 { return -26 }\n",
        "    if cmp.clamp(15, 10, 20) != 15 { return -27 }\n",
        "    if cmp.clamp(25, 10, 20) != 20 { return -28 }\n",
        "\n",
        "    // 5. Generic Ord interface dispatch\n",
        "    let min_item = cmp.minBy(PriorityItem { priority: 10, payload: 100 }, PriorityItem { priority: 20, payload: 200 })\n",
        "    if min_item.priority != 10 || min_item.payload != 100 { return -29 }\n",
        "    let max_item = cmp.maxBy(PriorityItem { priority: 10, payload: 100 }, PriorityItem { priority: 20, payload: 200 })\n",
        "    if max_item.priority != 20 || max_item.payload != 200 { return -30 }\n",
        "    let clamp_low = cmp.clampBy(PriorityItem { priority: 5, payload: 50 }, PriorityItem { priority: 10, payload: 100 }, PriorityItem { priority: 20, payload: 200 })\n",
        "    if clamp_low.priority != 10 { return -31 }\n",
        "    let clamp_mid = cmp.clampBy(PriorityItem { priority: 15, payload: 150 }, PriorityItem { priority: 10, payload: 100 }, PriorityItem { priority: 20, payload: 200 })\n",
        "    if clamp_mid.priority != 15 { return -32 }\n",
        "    let clamp_high = cmp.clampBy(PriorityItem { priority: 25, payload: 250 }, PriorityItem { priority: 10, payload: 100 }, PriorityItem { priority: 20, payload: 200 })\n",
        "    if clamp_high.priority != 20 { return -33 }\n",
        "\n",
        "    return 42\n",
        "}\n",
    );
    let observation = check_source(source, true, true).expect("execution across all backends");
    assert_eq!(observation.result, 42);
}

#[test]
fn test_seeded_option_result_cmp_differential_across_all_backends() {
    let source = synthesize(4);
    assert!(source.contains("gen_some_val"));
    assert!(source.contains("gen_ok_res"));
    assert!(source.contains("gen_min_item"));
    let observation = check_source(&source, true, true).expect("execution across all backends");
    assert!(
        observation.result >= 0,
        "unexpected error return code: {}",
        observation.result
    );
}

#[test]
fn test_str_mem_across_all_backends() {
    let source = concat!(
        "import std.alloc.vec as vec\n",
        "import std.core.slice as slice\n",
        "import std.core.str as core_str\n",
        "import std.core.mem as mem\n",
        "\n",
        "func main(): int {\n",
        "    // 1. String operations\n",
        "    if !core_str.isEmpty(\"\") || core_str.isEmpty(\"hello\") { return -1 }\n",
        "    if core_str.len(\"\") != 0 as usize || core_str.len(\"hello\") != 5 as usize || core_str.len(\"olá\") != 4 as usize { return -2 }\n",
        "    if !core_str.startsWith(\"hello world\", \"hello\") || core_str.startsWith(\"hello\", \"world\") || !core_str.startsWith(\"hello\", \"\") { return -3 }\n",
        "    if !core_str.endsWith(\"main.aru\", \".aru\") || core_str.endsWith(\"main.c\", \".aru\") || !core_str.endsWith(\"main.aru\", \"\") { return -4 }\n",
        "    if !core_str.contains(\"arandu compiler\", \"compiler\") || core_str.contains(\"arandu\", \"rust\") || !core_str.contains(\"arandu\", \"\") { return -5 }\n",
        "    let find_comp = core_str.find(\"arandu compiler\", \"compiler\")\n",
        "    let find_comp_ok = match find_comp {\n",
        "        Some(idx) => idx == 7 as usize\n",
        "        None => false\n",
        "    }\n",
        "    if !find_comp_ok { return -6 }\n",
        "    let find_none = core_str.find(\"arandu\", \"xyz\")\n",
        "    let find_none_ok = match find_none {\n",
        "        Some(_) => false\n",
        "        None => true\n",
        "    }\n",
        "    if !find_none_ok { return -7 }\n",
        "    let find_empty = core_str.find(\"arandu\", \"\")\n",
        "    let find_empty_ok = match find_empty {\n",
        "        Some(idx) => idx == 0 as usize\n",
        "        None => false\n",
        "    }\n",
        "    if !find_empty_ok { return -8 }\n",
        "\n",
        "    // 2. Memory intrinsics: sizeOf, alignOf\n",
        "    if mem.sizeOf<int>() != 4 as usize || mem.alignOf<int>() != 4 as usize { return -9 }\n",
        "    if mem.sizeOf<u8>() != 1 as usize || mem.alignOf<u8>() != 1 as usize { return -10 }\n",
        "    if mem.sizeOf<u16>() != 2 as usize || mem.alignOf<u16>() != 2 as usize { return -11 }\n",
        "    if mem.sizeOf<u64>() != 8 as usize || mem.alignOf<u64>() != 8 as usize { return -12 }\n",
        "    if mem.sizeOf<usize>() < 4 as usize || mem.alignOf<usize>() < 4 as usize { return -13 }\n",
        "\n",
        "    // 3. Memory value helpers: swap\n",
        "    let mut swap_x: int = 111\n",
        "    let mut swap_y: int = 222\n",
        "    mem.swap<int>(mut ref swap_x, mut ref swap_y)\n",
        "    if swap_x != 222 || swap_y != 111 { return -14 }\n",
        "    let mut rep_val: int = 50\n",
        "    let old_rep = mem.replace<int>(mut ref rep_val, 99)\n",
        "    let mut ref_target: int = 0\n",
        "    mem.refWrite<int>(mut ref ref_target, 333)\n",
        "    if ref_target != 333 { return -16 }\n",
        "\n",
        "    // 4. Raw pointer operations\n",
        "    let mut v = vec.Vec<int>.new()\n",
        "    if !v.tryPush(10) || !v.tryPush(20) {\n",
        "        return -17\n",
        "    }\n",
        "    let v_slice = v.asSlice()\n",
        "    let raw_ptr = slice.asPtr<int>(v_slice)\n",
        "    let val0 = unsafe { mem.ptrRead<int>(raw_ptr) }\n",
        "    let ptr1 = unsafe { mem.ptrOffset<int>(raw_ptr, 1 as isize) }\n",
        "    let val1 = unsafe { mem.ptrRead<int>(ptr1) }\n",
        "    unsafe { mem.ptrWrite<int>(ptr1, 999) }\n",
        "    let val1_up = unsafe { mem.ptrRead<int>(ptr1) }\n",
        "    let elem1 = match v.getCopy(1 as usize) {\n",
        "        Some(val) => val\n",
        "        None => 0\n",
        "    }\n",
        "    if val0 != 10 || val1 != 20 || val1_up != 999 || elem1 != 999 { return -18 }\n",
        "\n",
        "    return 42\n",
        "}\n",
    );
    let observation = check_source(source, true, true).expect("execution across all backends");
    assert_eq!(observation.result, 42);
}

#[test]
fn test_seeded_str_mem_differential_across_all_backends() {
    for seed in [12, 28] {
        let source = synthesize(seed);
        assert!(source.contains("gen_find_prefix"));
        assert!(source.contains("gen_swap_a"));
        assert!(source.contains("gen_mem_vec"));
        let observation = check_source(&source, true, true).expect("execution across all backends");
        assert!(
            observation.result >= 0,
            "seed {seed} failed with return code: {}",
            observation.result
        );
    }
}

#[test]
fn test_collections_across_all_backends() {
    let source = concat!(
        "import std.alloc.vec as vec\n",
        "import std.alloc.hash_map as hash_map\n",
        "import std.alloc.bitset as bitset\n",
        "import std.core.hash as hash\n",
        "\n",
        "struct MapKey {\n",
        "    id: int\n",
        "}\n",
        "func MapKey.eq(self: ref MapKey, other: ref MapKey): bool {\n",
        "    return self.id == other.id\n",
        "}\n",
        "func MapKey.hash<H: hash.Hasher>(self: ref MapKey, state: mut ref H): void {\n",
        "    state.writeInt(self.id)\n",
        "}\n",
        "\n",
        "func main(): int {\n",
        "    // 1. Vec withCapacity, isEmpty, push, pop, clear\n",
        "    let mut v = vec.Vec<int>.withCapacity(16 as usize)\n",
        "    if !v.isEmpty() || v.len() != 0 as usize || v.capacity() < 16 as usize { return -1 }\n",
        "    v.push(100)\n",
        "    v.push(200)\n",
        "    if v.isEmpty() || v.len() != 2 as usize { return -2 }\n",
        "    let popped = match v.pop() {\n",
        "        Some(val) => val == 200\n",
        "        None => false\n",
        "    }\n",
        "    if !popped || v.len() != 1 as usize { return -3 }\n",
        "    v.clear()\n",
        "    if !v.isEmpty() || v.len() != 0 as usize || v.capacity() < 16 as usize { return -4 }\n",
        "\n",
        "    // 2. HashMap withCapacity, put, contains, remove, clear\n",
        "    let mut map = hash_map.HashMap<MapKey, int>.withCapacity(16 as usize)\n",
        "    if !map.isEmpty() || map.capacity() != 16 as usize { return -5 }\n",
        "    let k1 = MapKey { id: 1 }\n",
        "    let k2 = MapKey { id: 2 }\n",
        "    map.put(k1, 10)\n",
        "    map.put(k2, 20)\n",
        "    if map.isEmpty() || map.len() != 2 as usize { return -6 }\n",
        "    if !map.contains(ref k1) || !map.contains(ref k2) { return -7 }\n",
        "    let k3 = MapKey { id: 3 }\n",
        "    if map.contains(ref k3) { return -8 }\n",
        "    let rem1 = match map.remove(ref k1) {\n",
        "        Some(val) => val == 10\n",
        "        None => false\n",
        "    }\n",
        "    if !rem1 || map.contains(ref k1) || map.len() != 1 as usize { return -9 }\n",
        "    map.clear()\n",
        "    if !map.isEmpty() || map.len() != 0 as usize { return -10 }\n",
        "\n",
        "    // 3. BitSet BitSet.withCapacity, set ops (union, intersect, diff)\n",
        "    let mut bs1 = bitset.BitSet.withCapacity(128 as usize)\n",
        "    if bs1.countOnes() != 0 as uint || bs1.contains(0 as usize) || bs1.contains(64 as usize) { return -11 }\n",
        "    bs1.insert(1 as usize)\n",
        "    bs1.insert(70 as usize)\n",
        "    let mut bs2 = bitset.BitSet.new()\n",
        "    bs2.insert(70 as usize)\n",
        "    bs2.insert(100 as usize)\n",
        "    let mut bs_u = bitset.BitSet.new()\n",
        "    bs_u.unionWith(ref bs1)\n",
        "    bs_u.unionWith(ref bs2)\n",
        "    if bs_u.countOnes() != 3 as uint || !bs_u.contains(1 as usize) || !bs_u.contains(70 as usize) || !bs_u.contains(100 as usize) { return -12 }\n",
        "    let mut bs_i = bitset.BitSet.new()\n",
        "    bs_i.unionWith(ref bs1)\n",
        "    bs_i.intersectWith(ref bs2)\n",
        "    if bs_i.countOnes() != 1 as uint || !bs_i.contains(70 as usize) || bs_i.contains(1 as usize) || bs_i.contains(100 as usize) { return -13 }\n",
        "    let mut bs_d = bitset.BitSet.new()\n",
        "    bs_d.unionWith(ref bs1)\n",
        "    bs_d.differenceWith(ref bs2)\n",
        "    if bs_d.countOnes() != 1 as uint || !bs_d.contains(1 as usize) || bs_d.contains(70 as usize) { return -14 }\n",
        "\n",
        "    return 42\n",
        "}\n",
    );
    let observation = check_source(source, true, true).expect("execution across all backends");
    assert_eq!(observation.result, 42);
}

#[test]
fn test_shrinker_early_cutoff_and_reproduction() {
    let source = concat!(
        "func helper_a(): int { return 1 }\n",
        "func helper_b(): int { return 2 }\n",
        "func helper_c(): int { return 3 }\n",
        "func main(): int { return 99 }\n",
    );
    // Early cutoff with budget = 1 attempt
    let mut oracle_attempts = 0;
    let (result, confirmed) = shrink_and_confirm(source, 1, |candidate| {
        oracle_attempts += 1;
        candidate.contains("return 99")
    });
    assert!(result.reproduced);
    assert!(confirmed);
    assert!(result.attempts <= 2);
    assert_eq!(oracle_attempts, result.attempts + 1);
    assert!(result.source.contains("return 99"));

    // Early cutoff when candidate does not reproduce
    let (failed_shrink, unconfirmed) = shrink_and_confirm(source, 8, |_| false);
    assert!(!failed_shrink.reproduced);
    assert!(!unconfirmed);
    assert_eq!(failed_shrink.source, source);
}

#[test]
fn test_smith_synthesis_determinism_and_snapshot() {
    for seed in [0, 4, 8, 12, 64] {
        let p1 = synthesize_with_oracle(seed);
        let p2 = synthesize_with_oracle(seed);
        assert_eq!(p1.expected_result, p2.expected_result);
        assert_eq!(
            p1.source, p2.source,
            "synthesis must be deterministic for seed {seed}"
        );
    }
}

#[test]
#[ignore]
fn __smith_jit_worker() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--") {
        let worker_args = &args[pos + 1..];
        if worker_args.len() >= 5 {
            let source_path = std::path::Path::new(&worker_args[0]);
            let opt_level = &worker_args[1];
            let result_path = std::path::Path::new(&worker_args[2]);
            let stdout_path = std::path::Path::new(&worker_args[3]);
            let stderr_path = std::path::Path::new(&worker_args[4]);
            let coverage_path = worker_args.get(5).map(std::path::Path::new);
            let code = run_jit_worker(
                source_path,
                opt_level,
                result_path,
                stdout_path,
                stderr_path,
                coverage_path,
            );
            std::process::exit(code);
        }
    }
}

#[test]
fn synthesized_wasm_seed_eight_matches_all_optimization_levels() {
    let generated = synthesize_with_oracle(8);
    let observation =
        check_source(&generated.source, false, true).expect("seed-eight JIT/Wasm differential");
    let expected = ExpectedObservation::synthesized(generated.expected_result);
    assert_eq!(observation.result, expected.result);
    assert_eq!(observation.stdout, expected.stdout);
    assert_eq!(observation.stderr, expected.stderr);
}

#[test]
fn scalar_optimizations_preserve_mutation_recursion_and_narrowing_in_every_backend() {
    for (source, expected) in [
        ("func numeric(a: int): int { let x = a + 1; return x / 2 } func main(): int { return numeric(2147483647) }", -1073741824),
        ("struct Pair { x: int; y: int } func keep(n: int): Pair { let mut pair = Pair { x: n, y: n + 1 }; pair.x = pair.x + 1; return pair } func main(): int { let pair = keep(20); return pair.x + pair.y }", 42),
        ("func rotate(n: int, a: int, b: int): int { if n == 0 { return a }; return rotate(n - 1, b, a) } func main(): int { return rotate(199, 17, 42) }", 42),
        ("struct Pair { x: int; y: int } func fields(flag: bool, n: int): int { let mut pair = Pair { x: n, y: n + 1 }; if flag { pair.x = n + 2 }; return pair.x + pair.y } func main(): int { return fields(true, 10) + fields(false, 10) }", 44),
        ("struct Pair { x: int; y: int } func main(): int { let mut pair = Pair { x: 17, y: 25 }; let p = &mut pair; p.x = 18; return p.x + p.y }", 43),
        ("func compute(a: u8, b: u8): u8 { let x: u8 = a + b; return x * (2 as u8) } func main(): int { return compute(250 as u8, 10 as u8) as int }", 8),
        ("func fib(n: int): int { if n < 2 { return n }; return fib(n - 1) + fib(n - 2) } func main(): int { return fib(10) }", 55),
    ] {
        let observation = check_source(source, true, true).unwrap_or_else(|failure| panic!("{failure:?}\n{source}"));
        assert_eq!(observation.result, expected, "{source}");
        assert!(observation.stdout.is_empty() && observation.stderr.is_empty());
    }
    let cleanup = check_source("import io\nfunc choose(flag: bool): int { defer { io.print(\"x\") }; if flag { return 17 }; return 25 } func main(): int { return choose(true) + choose(false) }", true, true).unwrap();
    assert_eq!(cleanup.result, 42);
    assert_eq!(cleanup.stdout, b"xx");
    assert!(cleanup.stderr.is_empty());
}

#[test]
fn array_sha256_padding_vectors_match_every_backend_and_optimization_level() {
    let source = include_str!("../../../../tests/perf/sha256-array.aru");
    let observation = check_source(source, true, true).expect("SHA-256 array/loop differential");
    assert_eq!(observation.result, 0);
    assert!(observation.stdout.is_empty() && observation.stderr.is_empty());
}

/// Manual profile of the same expanded program that exposed native Wasm
/// compilation pressure. Timings are evidence, never correctness thresholds.
#[test]
#[ignore = "manual seed-eight codegen profile"]
fn profile_seed_eight_codegen() {
    let generated = synthesize_with_oracle(8);
    let mut host = AnalysisHost::new();
    register_fuzz_stdlib(&mut host).unwrap();
    let file = host.new_file("profile-seed-eight.aru".into(), generated.source);
    let lowered = arandu_query::lower_amir(host.db(), file);
    assert!(lowered.type_check.diagnostics.is_empty());
    let mut program = lowered.amir.clone();
    arandu_mir::optimize_amir_checked_with_level(
        &mut program,
        &lowered.type_check.symbols,
        &lowered.type_check.type_info.type_interner,
        arandu_mir::OptLevel::O1,
    )
    .unwrap();
    let temps: usize = program.funcs.iter().map(|func| func.temps.len()).sum();
    for sample in 0..3 {
        let start = std::time::Instant::now();
        let jit = execute_cranelift(
            &program,
            &lowered.type_check.symbols,
            &lowered.type_check.type_info,
        )
        .unwrap();
        let jit_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = std::time::Instant::now();
        let source = arandu_backend_c::emit_c(
            &program,
            &lowered.type_check.symbols,
            lowered.type_check.type_info.as_ref(),
            &lowered.type_check.type_info.type_interner,
            arandu_middle::layout::DataLayout::host(),
        )
        .unwrap();
        let emit_ms = start.elapsed().as_secs_f64() * 1000.0;
        let declarations = source
            .lines()
            .filter(|line| {
                let line = line.trim();
                line.ends_with(';')
                    && !line.contains('=')
                    && line.split_whitespace().last().is_some_and(|name| {
                        name.strip_prefix('t')
                            .and_then(|name| name.strip_suffix(';'))
                            .is_some_and(|index| {
                                !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                            })
                    })
            })
            .count();
        let start = std::time::Instant::now();
        let c = execute_c(
            &program,
            &lowered.type_check.symbols,
            &lowered.type_check.type_info,
        )
        .unwrap();
        let c_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(jit.result, generated.expected_result);
        assert_eq!(c.result, jit.result);
        assert_eq!(c.stdout, jit.stdout);
        assert_eq!(c.stderr, jit.stderr);
        eprintln!("sample={sample} temps={temps} declarations={declarations} c_bytes={} jit_ms={jit_ms:.2} emit_ms={emit_ms:.2} c_ms={c_ms:.2}", source.len());
    }
}
