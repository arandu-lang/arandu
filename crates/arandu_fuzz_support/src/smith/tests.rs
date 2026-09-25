use super::artifact::*;
use super::emi::*;
use super::oracle::*;
use super::process::*;
use super::synth::*;
use super::types::*;

use arandu_query::AnalysisHost;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

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
        if seed > 0 {
            host.set_text(file, source.as_str());
        }
        drop(lower_validated_file(&host, file).unwrap_or_else(|failure| {
            panic!(
                "synthesized seed {seed} did not reach valid AMIR: {}",
                failure.message
            )
        }));
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
        run_all_backends(&seed.to_le_bytes());
    }
}

#[test]
fn synthesized_seeded_collection_cases_match_every_backend_and_level() {
    let generated = [synthesize_with_oracle(0), synthesize_with_oracle(64)];
    let first_map_insert = generated[0]
        .source
        .lines()
        .find(|line| line.contains("hash_map.insert<GeneratedKey, int>"))
        .expect("seeded HashMap source has an insertion");
    let second_map_insert = generated[1]
        .source
        .lines()
        .find(|line| line.contains("hash_map.insert<GeneratedKey, int>"))
        .expect("seeded HashMap source has an insertion");
    assert_ne!(first_map_insert, second_map_insert);
    assert!(generated[0]
        .source
        .contains("generated_bit_seed = generated_bits.insert(128 as uint)"));
    assert!(generated[1]
        .source
        .contains("generated_bit_seed = generated_bits.insert(129 as uint)"));

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
    let mut exercised_variants = [false; 3];
    let mut exercised_loop_forms = [false; 2];
    for seed in 0..12_u64 {
        let source = synthesize(seed);
        let base = source
            .lines()
            .find_map(|line| line.strip_prefix("    let base: int = "))
            .and_then(|value| value.parse::<usize>().ok())
            .expect("synthesized base literal");
        exercised_variants[base % exercised_variants.len()] = true;
        let loop_form = if source.contains("for loop_index =") {
            1
        } else {
            0
        };
        exercised_loop_forms[loop_form] = true;
        if !seed.is_multiple_of(64) {
            run_all_backends(&seed.to_le_bytes());
        }
    }
    assert!(
        exercised_variants.into_iter().all(|exercised| exercised),
        "all three enum variants must be selected by the differential corpus"
    );
    assert!(
        exercised_loop_forms.into_iter().all(|exercised| exercised),
        "differential seeds must cover both bounded loop forms"
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
fn generic_owned_value_can_be_returned_and_used_by_every_backend() {
    let source = r#"
import std.alloc.vec as vec

func transfer<T>(own value: T): T {
    return value
}

func main(): int {
    let mut values = transfer<vec.Vec<int>>(vec.new<int>())
    if !vec.tryPush<int>(values, 42) {
        vec.destroy<int>(values)
        return -1
    }
    let length = vec.len<int>(values) as int
    values.destroy()
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
    assert!(source.contains("vec.len<int>(owned_pack.items) != 1 as uint"));
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
    assert!(source.contains("vec.tryPush<uint>(unsigned_values, uint_result)"));
    assert!(source.contains("vec.Vec<core_result.Result<int, bool>>"));
    assert!(source.contains("relay<vec.Vec<core_result.Result<int, bool>>>("));
    assert!(source.contains("Result.Err(sample.enabled)"));
    assert!(source.contains("relay<vec.Vec<int>>(make_vec<int>())"));
    assert!(source.contains("transfer<vec.Vec<int>>(relay<vec.Vec<int>>(make_vec<int>()))"));
    assert!(source.contains("return vec.new<T>()"));
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
    assert!(source.contains("vec.tryReserve<int>(dynamic, 1 as uint)"));
    assert!(source.contains("slice.len<int>(vec.asSlice<int>(dynamic))"));
    assert!(source.contains("vec.get<int>(dynamic, dynamic_len as uint)"));
    assert!(source.contains("while dynamic_count < 9"));
    assert!(source.contains("vec.tryPush<int>(dynamic, sample.left + dynamic_count)"));
    assert!(source.contains("transfer<vec.Vec<bool>>(make_vec<bool>())"));
    assert!(source.contains("vec.tryPush<bool>(flags, sample.enabled)"));
    assert!(source.contains("slice.len<bool>(vec.asSlice<bool>(flags))"));
    assert!(source.contains("transfer<vec.Vec<Choice>>(make_vec<Choice>())"));
    assert!(source.contains("vec.tryPush<Choice>(choices, Choice.Flag(sample.enabled))"));
    assert!(source.contains("vec.pop<Choice>(choices)"));
    assert!(source.contains("Some(Choice.Flag(value)) => value"));
    assert!(source.contains("Choice.Letter(sample.character)"));
    assert!(source.contains("Some(Choice.Letter(value)) => value"));
    assert!(source.contains("recovered_character != sample.character"));
    assert!(source.contains("choices.destroy()"));
    assert!(source.contains("transfer<vec.Vec<char>>(make_vec<char>())"));
    assert!(source.contains("vec.tryPush<char>(characters, sample.character)"));
    assert!(source.contains("slice.len<char>(vec.asSlice<char>(characters))"));
    assert!(source.contains("vec.get<char>(characters, 0 as uint)"));
    assert!(source.contains("recovered_vector_character != sample.character"));
    assert!(source.contains("characters.destroy()"));
    assert!(source.contains("transfer<vec.Vec<float>>(make_vec<float>())"));
    assert!(source.contains("vec.tryPush<float>(floats, float_result)"));
    assert!(source.contains("vec.get<float>(floats, 0 as uint)"));
    assert!(source.contains("recovered_vector_float != float_result"));
    assert!(source.contains("floats.destroy()"));
    assert!(source.contains("vec.destroy<int>(dynamic)"));
    assert!(source.contains("dynamic.destroy()"));
    assert!(source.contains("flags.destroy()"));
    assert!(source.contains("vec.get<int>(dynamic, dynamic_index)"));
    assert!(source.contains("if dynamic_len != 9"));
    assert!(source.contains("if dynamic_tail != sample.left + 8"));
    assert!(source.contains("vec.put<int>(dynamic, 8 as uint, sample.right)"));
    assert!(source.contains("vec.pop<int>(dynamic)"));
    assert!(source.contains("vec.clear<int>(dynamic)"));
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
        signatures.len() == 5,
        "type-directed triples should cover all rotations: {signatures:?}"
    );
    assert!(
        pair_signatures.len() == 5,
        "type-directed pairs should cover all rotations: {pair_signatures:?}"
    );
    for ty in ["int", "uint", "bool", "float", "char"] {
        assert!(
            pair_signatures
                .iter()
                .any(|signature| signature.contains(ty)),
            "no generated pair included {ty}: {pair_signatures:?}"
        );
    }
    for ty in ["int", "uint", "bool", "float", "char"] {
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
    run(&32u64.to_le_bytes());
}
