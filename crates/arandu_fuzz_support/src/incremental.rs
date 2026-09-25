//! Differential edit sequences: a warm database must agree with a fresh one.
//! The oracle compares diagnostic contents, not implementation hashes.

use arandu_query::{file_ide_diagnostics, AnalysisHost, SourceFile};

const PATHS: [&str; 2] = ["dependency.aru", "consumer.aru"];
const LIBRARY: &str = "module dependency\npublic func value(): int { return 1 }\n";
const CONSUMER: &str =
    "module consumer\nimport dependency\nfunc main(): int { return dependency.value() }\n";
const MAX_EDITS: usize = 24;

const PRIVATE_DEPENDENCY: &str = "module dependency\npublic func value(): int { return helper() }\nfunc helper(): int { return 1 }\n";
const CUTOFF_CONSUMER: &str =
    "module consumer\nimport dependency\nfunc main(): int { return dependency.value() }\n";
const SHRINK_ATTEMPT_BUDGET: usize = 48;

fn run_with_sequence_reduction(data: &[u8], verify: fn(&[u8]) -> Result<(), String>) {
    let initial = run_verifier(verify, data);
    let Err(original_failure) = initial else {
        return;
    };
    let original_class = failure_class(&original_failure).to_owned();

    let reduced = crate::shrinker::shrink_byte_sequence_with_budget(
        data,
        SHRINK_ATTEMPT_BUDGET,
        |candidate| {
            run_verifier(verify, candidate)
                .is_err_and(|failure| failure_class(&failure) == original_class)
        },
    );
    let shrink_confirmed = reduced.reproduced
        && run_verifier(verify, &reduced.sequence)
            .is_err_and(|failure| failure_class(&failure) == original_class);
    let (sequence_label, sequence) = if shrink_confirmed {
        ("minimized_sequence_hex", reduced.sequence.as_slice())
    } else {
        ("original_sequence_hex", data)
    };
    let sequence_hex = sequence
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    panic!(
        "incremental fuzz failure: {original_failure}; {sequence_label}={sequence_hex}; shrink_reproduced={}; shrink_confirmed={shrink_confirmed}; shrink_attempts={}; shrink_reductions={}",
        reduced.reproduced,
        reduced.attempts,
        reduced.reductions
    );
}

fn failure_class(message: &str) -> &str {
    if message.starts_with("incremental verifier panicked:") {
        return message;
    }
    message.split_once(": ").map_or(message, |(class, _)| class)
}

fn run_verifier(verify: fn(&[u8]) -> Result<(), String>, data: &[u8]) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| verify(data))).unwrap_or_else(
        |payload| {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("internal panic while verifying edit sequence");
            Err(format!("incremental verifier panicked: {message}"))
        },
    )
}

/// Assert the public-export early-cutoff contract against Salsa's event log.
pub(super) fn run_private_cutoff(data: &[u8]) {
    run_with_sequence_reduction(&data[..data.len().min(1)], verify_private_cutoff);
}

fn verify_private_cutoff(data: &[u8]) -> Result<(), String> {
    use arandu_query::passes::{exported_symbols, type_check};

    let replacement = 2 + u32::from(data.first().copied().unwrap_or(0)) % 127;
    let (mut host, rebuild_log) = AnalysisHost::with_rebuild_log();
    let dependency = host.new_file("dependency.aru".into(), PRIVATE_DEPENDENCY.into());
    let consumer = host.new_file("consumer.aru".into(), CUTOFF_CONSUMER.into());

    let exports_before = (*exported_symbols(host.db(), dependency)).clone();
    let warm = type_check(host.db(), consumer);
    if !warm.diagnostics.is_empty() {
        return Err(format!(
            "incremental cutoff baseline must typecheck: {:?}",
            warm.diagnostics
        ));
    }

    rebuild_log.clear();
    let edited = format!(
        "module dependency\npublic func value(): int {{ return helper() }}\nfunc helper(): int {{ return {replacement} }}\n"
    );
    host.set_text(dependency, edited);

    let exports_after = (*exported_symbols(host.db(), dependency)).clone();
    if exports_before != exports_after {
        return Err("private body edit changed the exported symbol surface".to_owned());
    }
    let current = type_check(host.db(), consumer);
    if !current.diagnostics.is_empty() {
        return Err(format!(
            "consumer diagnostics changed after private body edit: {:?}",
            current.diagnostics
        ));
    }
    let importer_typeck_runs = rebuild_log.count_executions_matching("type_check");
    if importer_typeck_runs != 0 {
        return Err(format!(
            "private body edit propagated into consumer type_check: {:?}",
            rebuild_log.snapshot()
        ));
    }

    run_public_surface_invalidation(data)
}

fn run_public_surface_invalidation(data: &[u8]) -> Result<(), String> {
    use arandu_query::passes::type_check;

    // `exported_symbols` tracks names/kinds for resolution; imported function
    // signatures flow through `module_signatures` and must invalidate typeck.
    let (mut host, rebuild_log) = AnalysisHost::with_rebuild_log();
    let dependency = host.new_file("dependency.aru".into(), LIBRARY.into());
    let consumer = host.new_file("consumer.aru".into(), CONSUMER.into());
    let baseline = type_check(host.db(), consumer);
    if !baseline.diagnostics.is_empty() {
        return Err(format!(
            "public-surface invalidation baseline must typecheck: {:?}",
            baseline.diagnostics
        ));
    }

    let replacement = match data.first().copied().unwrap_or_default() % 3 {
        0 => "module dependency\npublic func value(): str { return \"text\" }\n",
        1 => "module dependency\npublic func renamed(): int { return 1 }\n",
        _ => "module dependency\npublic func value(input: int): int { return input }\n",
    };
    host.set_text(dependency, replacement);

    rebuild_log.clear();
    let current = type_check(host.db(), consumer);
    if current.diagnostics.is_empty() {
        return Err("consumer type_check did not observe the changed public surface".to_owned());
    }
    let importer_typeck_runs = rebuild_log.count_executions_matching("type_check");
    if importer_typeck_runs == 0 {
        return Err(format!(
            "public surface edit failed to invalidate consumer type_check: {:?}",
            rebuild_log.snapshot()
        ));
    }
    Ok(())
}

fn register(host: &mut AnalysisHost, sources: &[String; 2]) -> [SourceFile; 2] {
    std::array::from_fn(|index| host.new_file(PATHS[index].into(), sources[index].clone()))
}

fn assert_completion_equivalence(
    warm: &AnalysisHost,
    warm_file: SourceFile,
    fresh: &AnalysisHost,
    fresh_file: SourceFile,
    text: &str,
    file_name: &str,
    step: usize,
) -> Result<(), String> {
    let Ok(end) = u32::try_from(text.len()) else {
        return Ok(());
    };
    let mut offsets = vec![end];
    for marker in ["return ", "dependency."] {
        offsets.extend(
            text.match_indices(marker)
                .filter_map(|(start, _)| u32::try_from(start.checked_add(marker.len())?).ok()),
        );
    }
    offsets.sort_unstable();
    offsets.dedup();

    let warm_snapshot = warm.snapshot();
    let fresh_snapshot = fresh.snapshot();
    for offset in offsets {
        let actual = arandu_ide::completions(&warm_snapshot, warm_file, text, offset);
        let expected = arandu_ide::completions(&fresh_snapshot, fresh_file, text, offset);
        if actual != expected {
            return Err(format!(
                "incremental/cold completion mismatch: step={step}, file={file_name}, offset={offset}, actual={actual:?}, expected={expected:?}\n--- source ---\n{text}"
            ));
        }
    }
    Ok(())
}

fn apply_local_edit(source: &str, operation: u8, step: usize) -> String {
    const FRAGMENTS: [&str; 8] = ["", " ", "\n", "(", ")", "0", "x", "/* 🦀 */"];

    let mut boundaries: Vec<_> = source.char_indices().map(|(offset, _)| offset).collect();
    boundaries.push(source.len());
    let last_boundary = boundaries.len().saturating_sub(1);
    let start_index = (usize::from(operation)
        .wrapping_mul(37)
        .wrapping_add(step.wrapping_mul(13)))
        % boundaries.len();
    let end_index = start_index
        .saturating_add(1 + (usize::from(operation) + step) % 5)
        .min(last_boundary);
    let start = boundaries[start_index];
    let end = boundaries[end_index];
    let fragment = FRAGMENTS[(usize::from(operation >> 4) + step) % FRAGMENTS.len()];

    match operation % 4 {
        0 => format!("{}{fragment}{}", &source[..start], &source[start..]),
        1 => format!("{}{}", &source[..start], &source[end..]),
        2 => format!("{}{fragment}{}", &source[..start], &source[end..]),
        _ => {
            let comment = format!("// edição {step} 🦀\n");
            format!("{}{comment}{}", &source[..start], &source[start..])
        }
    }
}

pub(super) fn run(data: &[u8]) {
    run_with_sequence_reduction(
        &data[..data.len().min(MAX_EDITS)],
        verify_incremental_sequence,
    );
}

fn verify_incremental_sequence(data: &[u8]) -> Result<(), String> {
    let mut sources = [LIBRARY.to_owned(), CONSUMER.to_owned()];
    let mut warm = AnalysisHost::new();
    let files = register(&mut warm, &sources);
    for (index, file) in files.into_iter().enumerate() {
        let diagnostics = file_ide_diagnostics(warm.db(), file);
        if !diagnostics.is_empty() {
            return Err(format!(
                "generator baseline must be valid: file={}, diagnostics={:?}",
                PATHS[index], **diagnostics
            ));
        }
    }
    for (step, &operation) in data.iter().take(MAX_EDITS).enumerate() {
        let (index, replacement) = match operation % 16 {
            0 => (
                0,
                format!("module dependency\npublic func value(): int {{ return {operation} }}\n"),
            ),
            1 => (
                0,
                "module dependency\npublic func value(): str { return \"text\" }\n".into(),
            ),
            2 => (
                0,
                "module dependency\npublic func renamed(): int { return 1 }\n".into(),
            ),
            3 => (
                0,
                "module dependency\npublic func value(): int { let 1 = 2; return 0 }\n".into(),
            ),
            4 => (0, LIBRARY.into()),
            5 => (
                1,
                "module consumer\nimport dependency\nfunc main(): int { return unknown }\n".into(),
            ),
            6 => (1, format!("// ação 🦀\n{CONSUMER}")),
            7 => (
                1,
                "module consumer\nimport dependency\nfunc main(): int { let 1 = 2; let 3 = 4; }\n"
                    .into(),
            ),
            8 => (1, CONSUMER.into()),
            9 => (
                1,
                format!("{CONSUMER}\nfunc sibling(): int {{ return {operation} }}\n"),
            ),
            10 => (
                0,
                "module dependency\npublic func value(): int { return\n".into(),
            ),
            11 => (
                1,
                "module consumer\nimport dependency\nfunc main(): int { return dependency.value(\n"
                    .into(),
            ),
            local_edit => {
                let index = usize::from(local_edit % 2);
                (index, apply_local_edit(&sources[index], operation, step))
            }
        };
        sources[index] = replacement;
        warm.set_text(files[index], sources[index].as_str());

        let mut fresh = AnalysisHost::new();
        let fresh_files = register(&mut fresh, &sources);
        // Alternate demand order without changing registration identities.
        for index in if step % 2 == 0 { [1, 0] } else { [0, 1] } {
            let actual = file_ide_diagnostics(warm.db(), files[index]);
            let expected = file_ide_diagnostics(fresh.db(), fresh_files[index]);
            if **actual != **expected {
                return Err(format!(
                    "incremental/cold mismatch: step={step}, operations={:?}, file={}\n--- dependency ---\n{}\n--- consumer ---\n{}",
                    &data[..=step], PATHS[index], sources[0], sources[1]
                ));
            }
            let actual_type_check = arandu_query::passes::type_check(warm.db(), files[index]);
            let expected_type_check =
                arandu_query::passes::type_check(fresh.db(), fresh_files[index]);
            let actual_type_check_hash =
                arandu_query::StableHash::stable_hash(actual_type_check.value.as_ref());
            let expected_type_check_hash =
                arandu_query::StableHash::stable_hash(expected_type_check.value.as_ref());
            if actual_type_check_hash != expected_type_check_hash {
                return Err(format!(
                    "incremental/cold type_check mismatch: step={step}, operations={:?}, file={}\n--- dependency ---\n{}\n--- consumer ---\n{}",
                    &data[..=step], PATHS[index], sources[0], sources[1]
                ));
            }
            if let Some(diagnostic) = actual
                .iter()
                .find(|diagnostic| diagnostic.code.starts_with("ICE"))
            {
                return Err(format!(
                    "generated source must recover without ICE: step={step}, operations={:?}, diagnostic={diagnostic:?}",
                    &data[..=step]
                ));
            }
            assert_completion_equivalence(
                &warm,
                files[index],
                &fresh,
                fresh_files[index],
                &sources[index],
                PATHS[index],
                step,
            )?;
        }
    }

    // Diagnostic and completion parity can miss stale semantic or AMIR state
    // that happens not to change either surface. Compare the final lowered
    // artifacts against a cold database once per input, rather than paying this
    // transitive lowering cost at every intermediate edit.
    let mut cold = AnalysisHost::new();
    let cold_files = register(&mut cold, &sources);
    for index in 0..files.len() {
        let warm_amir = arandu_query::lower_amir(warm.db(), files[index]);
        let cold_amir = arandu_query::lower_amir(cold.db(), cold_files[index]);
        // HashEq is optimized for Salsa cutoff and is not a collision-proof
        // structural equality oracle. Compare the ordered function graph and
        // every other top-level AMIR payload directly as well.
        let warm_functions = format!("{:?}", warm_amir.amir.funcs);
        let cold_functions = format!("{:?}", cold_amir.amir.funcs);
        let warm_type_check = arandu_query::StableHash::stable_hash(&warm_amir.type_check);
        let cold_type_check = arandu_query::StableHash::stable_hash(&cold_amir.type_check);
        if !(warm_functions == cold_functions
            && warm_amir.amir.literal_pool.entries == cold_amir.amir.literal_pool.entries
            && warm_amir.amir.extern_funcs == cold_amir.amir.extern_funcs
            && warm_amir.amir.debug_bindings == cold_amir.amir.debug_bindings
            && warm_type_check == cold_type_check)
        {
            return Err(format!(
                "incremental/cold AMIR mismatch after edit sequence: operations={:?}, file={}\n--- dependency ---\n{}\n--- consumer ---\n{}\nwarm diagnostics: {:?}\ncold diagnostics: {:?}",
                data.iter().take(MAX_EDITS).copied().collect::<Vec<_>>(),
                PATHS[index],
                sources[0],
                sources[1],
                warm_amir.type_check.diagnostics,
                cold_amir.type_check.diagnostics,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn failure_requires_ordered_operations(data: &[u8]) -> Result<(), String> {
        if data.windows(2).any(|pair| pair == [1, 2]) {
            Err("incremental mismatch: required adjacent operation pair".to_owned())
        } else if data.contains(&1) {
            Err("unrelated failure: operation one without its successor".to_owned())
        } else {
            Ok(())
        }
    }

    #[test]
    fn incremental_failure_reports_a_confirmed_minimized_edit_sequence() {
        let panic = catch_unwind(AssertUnwindSafe(|| {
            super::run_with_sequence_reduction(
                &[77, 1, 2, 88, 99],
                failure_requires_ordered_operations,
            );
        }))
        .expect_err("the synthetic failing sequence must remain a failure");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .expect("reduction panic carries its failure report");

        assert!(message.contains("incremental mismatch: required adjacent operation pair"));
        assert!(message.contains("minimized_sequence_hex=0102"), "{message}");
        assert!(message.contains("shrink_reproduced=true"));
        assert!(message.contains("shrink_confirmed=true"));
    }

    #[test]
    fn dependency_and_syntax_edits_match_cold_analysis() {
        super::run(&[1, 4, 2, 4, 3, 4, 5, 8, 6, 7, 8, 9, 0, 4, 8]);
    }

    #[test]
    fn private_and_public_edits_obey_importer_cutoff_contracts() {
        for seed in [&[42][..], &[43][..], &[44][..]] {
            super::run_private_cutoff(seed);
        }
    }

    #[test]
    fn all_pairs_of_edit_operations_match_cold_analysis() {
        for first in 0..16 {
            for second in 0..16 {
                super::run(&[first, second, 4, 8]);
            }
        }
    }

    #[test]
    fn local_utf8_edits_match_cold_analysis() {
        super::run(&[12, 13, 14, 15, 4, 8]);
    }

    #[test]
    fn incomplete_dependency_and_consumer_edits_match_cold_analysis() {
        super::run(&[10, 11, 4, 8, 11, 10]);
    }
}
