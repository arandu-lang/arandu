//! Equivalence Modulo Inputs (EMI) mutator, corpus, and profiling probes.

use super::artifact::Failure;
use super::oracle::{
    check_source, check_source_with_block_coverage, run_source_with_expected, seed_from_data,
    BackendObservation, ExpectedObservation,
};
use super::types::{Rng, MAX_DEPTH};

pub const EMI_SEED_SALT: u64 = 0x454d_4900_0000_0001;
pub const MAX_EMI_PROFILE_PROBES: usize = 4;
pub const EMI_PROBE_EXIT_CODE: i32 = -1_073_741_824;

pub const EMI_CORPUS: &[(&str, &str, Option<ExpectedObservation>)] = &[
    (
        "enum-if-join",
        include_str!("../../../../tests/regressions/enum-if-join.aru"),
        None,
    ),
    (
        "enum-optimizer-join",
        include_str!("../../../../tests/regressions/enum-optimizer-join.aru"),
        None,
    ),
    (
        "enum-wasm-match",
        include_str!("../../../../tests/regressions/enum-wasm-match.aru"),
        None,
    ),
    (
        "generic-module-name-collision",
        include_str!("../../../../tests/regressions/generic-module-name-collision.aru"),
        None,
    ),
    (
        "control-flow-loop",
        include_str!("../../../../tests/emi-corpus/control-flow-loop.aru"),
        None,
    ),
    (
        "vec-operations",
        include_str!("../../../../tests/emi-corpus/vec-operations.aru"),
        None,
    ),
    (
        "option-stdlib",
        include_str!("../../../../tests/emi-corpus/option-stdlib.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "example-vec-free-functions",
        include_str!("../../../../examples/minimal/m13_vec.aru"),
        Some(ExpectedObservation {
            result: 78,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "example-vec-methods",
        include_str!("../../../../examples/minimal/m18_vec_methods.aru"),
        Some(ExpectedObservation {
            result: 78,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "example-core-str",
        include_str!("../../../../examples/minimal/m20_str.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "example-gen-arena",
        include_str!("../../../../examples/minimal/m16_gen_arena.aru"),
        Some(ExpectedObservation {
            result: 83,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "example-struct-enum",
        include_str!("../../../../examples/minimal/m02_structs_enums.aru"),
        Some(ExpectedObservation {
            result: 3,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "example-result-option",
        include_str!("../../../../examples/minimal/m03_result_option.aru"),
        Some(ExpectedObservation {
            result: 7,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "example-interpolation",
        include_str!("../../../../examples/minimal/m09_interp_tostr.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"n=3\n",
            stderr: b"",
        }),
    ),
    (
        "example-match-result",
        include_str!("../../../../examples/minimal/m23_match_result.aru"),
        Some(ExpectedObservation {
            result: 13,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "example-vec-capacity",
        include_str!("../../../../examples/minimal/m15_vec_capacity.aru"),
        Some(ExpectedObservation {
            result: 21,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "result-stdlib",
        include_str!("../../../../tests/emi-corpus/result-stdlib.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "hash-map-collisions",
        include_str!("../../../../tests/emi-corpus/hash-map-collisions.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "bitset-stdlib",
        include_str!("../../../../tests/emi-corpus/bitset-stdlib.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "smallvec-spill",
        include_str!("../../../../tests/emi-corpus/smallvec-spill.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "arena-lifecycle",
        include_str!("../../../../tests/emi-corpus/arena-lifecycle.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "string-lifecycle",
        include_str!("../../../../tests/emi-corpus/string-lifecycle.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"",
            stderr: b"",
        }),
    ),
    (
        "num-boundaries",
        include_str!("../../../../tests/emi-corpus/num-boundaries.aru"),
        Some(ExpectedObservation {
            result: 0,
            stdout: b"",
            stderr: b"",
        }),
    ),
];

pub(super) fn run_emi_corpus(data: &[u8]) {
    let seed = seed_from_data(data);
    let corpus_index = (seed % EMI_CORPUS.len() as u64) as usize;
    let (name, source, expected) = EMI_CORPUS[corpus_index];
    run_source_with_expected(source, seed, true, true, name, expected);
}

pub(super) fn verify_emi_regression_source(source: &str, seed: u64) -> Result<(), String> {
    check_emi_pair_with_expected(source, seed, true, true, None)
        .map(|_| ())
        .map_err(|failure| format!("{}: {}", failure.kind, failure.message))
}

#[cfg(test)]
pub(crate) fn check_emi_pair(
    source: &str,
    seed: u64,
    compare_c: bool,
    compare_wasm: bool,
) -> Result<(), Failure> {
    check_emi_pair_with_expected(source, seed, compare_c, compare_wasm, None).map(|_| ())
}

pub(crate) fn check_emi_pair_with_expected(
    source: &str,
    seed: u64,
    compare_c: bool,
    compare_wasm: bool,
    expected: Option<ExpectedObservation>,
) -> Result<BackendObservation, Failure> {
    let (reference, reference_coverage) =
        check_source_with_block_coverage(source, compare_c, compare_wasm)?;
    if let Some(expected) = expected {
        if reference.result != expected.result {
            return Err(Failure::new(
                "synthesized-result-mismatch",
                format!(
                    "program returned {}, independent oracle expected {}",
                    reference.result, expected.result
                ),
                true,
            ));
        }
        if reference.stdout != expected.stdout {
            return Err(Failure::new(
                "synthesized-output-mismatch",
                format!(
                    "generated program emitted {:?}, independent synthesis oracle expected {:?}",
                    String::from_utf8_lossy(&reference.stdout),
                    String::from_utf8_lossy(expected.stdout)
                ),
                true,
            ));
        }
        if reference.stderr != expected.stderr {
            return Err(Failure::new(
                "synthesized-stderr-mismatch",
                format!(
                    "generated program emitted stderr {:?}, independent synthesis oracle expected {:?}",
                    String::from_utf8_lossy(&reference.stderr),
                    String::from_utf8_lossy(expected.stderr)
                ),
                true,
            ));
        }
    }
    let (mutated, insertion) =
        inject_emi_mutation_with_profile(source, seed, &reference, &reference_coverage)
            .ok_or_else(|| {
                Failure::new(
                    "emi-injection-point-missing",
                    "generated source has no usable EMI insertion point",
                    false,
                )
            })?;
    let mutated_result = check_source(&mutated, compare_c, compare_wasm)?;
    if reference != mutated_result {
        return Err(Failure::new(
            "emi-observation-mismatch",
            format!(
                "adding a pure statement to a {insertion} branch changed observable behavior: original={reference:?}, mutated={mutated_result:?}"
            ),
            true,
        ));
    }
    Ok(reference)
}

/// Probe conditional branches and loop bodies in `main` with a distinct early
/// return. A block whose probe leaves the reference observation unchanged was
/// not entered by this input, so a pure statement can be injected there for EMI
/// comparison. The probe budget is fixed; the static `if false` insertion
/// remains available when no dynamic candidate is established.
pub(crate) fn inject_emi_mutation(
    source: &str,
    seed: u64,
    reference: &BackendObservation,
) -> Option<(String, &'static str)> {
    let (_, coverage) = check_source_with_block_coverage(source, false, false).ok()?;
    inject_emi_mutation_with_profile(source, seed, reference, &coverage)
}

fn inject_emi_mutation_with_profile(
    source: &str,
    seed: u64,
    reference: &BackendObservation,
    reference_coverage: &arandu_backend_cranelift::BlockCoverage,
) -> Option<(String, &'static str)> {
    let (program, tree) = arandu_parser::syntax::parse_dual(source);
    if let Ok(program) = program {
        let profile = profile_main_regions(source, &program, seed, reference, reference_coverage);
        if let Some(region) = profile
            .iter()
            .find(|entry| entry.entered == Some(false))
            .map(|entry| entry.region)
        {
            let insertion = match region {
                ProfileRegion::Block(insertion) => insertion,
                ProfileRegion::Loop(probe) => probe.body_insertion,
            };
            let mut rng = Rng::new(seed ^ EMI_SEED_SALT);
            let expression = gen_pure_int(&mut rng, MAX_DEPTH);
            let binding = fresh_emi_binding(source);
            let mutation = insert_in_block(
                source,
                insertion,
                &format!("let {binding}: int = {expression}"),
            )?;
            return Some((mutation, "dynamically unexecuted"));
        }
    }
    inject_dead_pure_statement_in_tree(source, seed, &tree)
        .map(|mutation| (mutation, "constant-false"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileRegion {
    Block(usize),
    Loop(LoopProbe),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileEntry {
    pub region: ProfileRegion,
    /// `None` means the observations and block trace did not prove either state.
    pub entered: Option<bool>,
}

/// Collects a bounded, source-level profile for candidate regions in `main`.
/// A failed probe is unknown and omitted: only a successful unchanged
/// observation proves that the candidate was not entered for this input.
pub(crate) fn profile_main_regions(
    source: &str,
    program: &arandu_parser::Program,
    seed: u64,
    reference: &BackendObservation,
    reference_coverage: &arandu_backend_cranelift::BlockCoverage,
) -> Vec<ProfileEntry> {
    let positions = main_control_flow_insertions(source, program);
    let loops = main_loop_probes(source, program);
    let loop_budget = if loops.is_empty() {
        0
    } else {
        MAX_EMI_PROFILE_PROBES / 2
    };
    let branch_budget = MAX_EMI_PROFILE_PROBES - loop_budget;
    let probe_exit = if reference.result == EMI_PROBE_EXIT_CODE {
        EMI_PROBE_EXIT_CODE + 1
    } else {
        EMI_PROBE_EXIT_CODE
    };
    let mut profile = Vec::with_capacity(MAX_EMI_PROFILE_PROBES);

    if let Some(first) = seed_index(seed, positions.len()) {
        let mut candidates = (0..positions.len().min(branch_budget))
            .map(|offset| positions[(first + offset) % positions.len()])
            .collect::<Vec<_>>();
        // Prefer likely-unentered regions, but keep the injection probe as the
        // authority for classifying a candidate as dynamically dead.
        candidates.sort_by_key(|&insertion| {
            match source_region_hit_status(reference_coverage, insertion) {
                Some(false) => 0,
                None => 1,
                Some(true) => 2,
            }
        });
        for insertion in candidates {
            let Some(probe) = insert_in_block(source, insertion, &format!("return {probe_exit}"))
            else {
                continue;
            };
            let Ok((observation, coverage)) =
                check_source_with_block_coverage(&probe, false, false)
            else {
                continue;
            };
            profile.push(ProfileEntry {
                region: ProfileRegion::Block(insertion),
                entered: if observation != *reference {
                    Some(true)
                } else if trace_preserves_reference(reference_coverage, &coverage) {
                    Some(false)
                } else {
                    None
                },
            });
        }
    }

    if let Some(first) = seed_index(seed, loops.len()) {
        let mut candidates = (0..loops.len().min(loop_budget))
            .map(|offset| loops[(first + offset) % loops.len()])
            .collect::<Vec<_>>();
        candidates.sort_by_key(|probe| {
            match source_region_hit_status(reference_coverage, probe.body_insertion) {
                Some(false) => 0,
                None => 1,
                Some(true) => 2,
            }
        });
        for probe in candidates {
            let Some(probe_source) = instrument_loop_probe(source, probe, probe_exit) else {
                continue;
            };
            let Ok((observation, coverage)) =
                check_source_with_block_coverage(&probe_source, false, false)
            else {
                continue;
            };
            profile.push(ProfileEntry {
                region: ProfileRegion::Loop(probe),
                entered: if observation != *reference {
                    Some(true)
                } else if trace_preserves_reference(reference_coverage, &coverage) {
                    Some(false)
                } else {
                    None
                },
            });
        }
    }
    profile
}

pub(crate) fn source_region_hit_status(
    coverage: &arandu_backend_cranelift::BlockCoverage,
    insertion: usize,
) -> Option<bool> {
    let offset = u32::try_from(insertion).ok()?;
    let narrowest = coverage
        .source_blocks
        .iter()
        .filter(|mapping| mapping.span.start <= offset && offset <= mapping.span.end)
        .map(|mapping| mapping.span.end.saturating_sub(mapping.span.start))
        .min()?;
    let blocks = coverage
        .source_blocks
        .iter()
        .filter(|mapping| {
            mapping.span.start <= offset
                && offset <= mapping.span.end
                && mapping.span.end.saturating_sub(mapping.span.start) == narrowest
        })
        .map(|mapping| (mapping.function_index, mapping.block_index))
        .collect::<Vec<_>>();
    let was_hit = blocks.iter().any(|block| {
        coverage
            .hits
            .iter()
            .any(|hit| (hit.function_index, hit.block_index) == *block)
    });
    if coverage.truncated && !was_hit {
        None
    } else {
        Some(was_hit)
    }
}

/// Instrumentation may add blocks to a probe. Preserve the reference execution
/// as an ordered subsequence so extra probe-only blocks do not invalidate it.
fn trace_preserves_reference(
    reference: &arandu_backend_cranelift::BlockCoverage,
    probe: &arandu_backend_cranelift::BlockCoverage,
) -> bool {
    if reference.truncated || probe.truncated {
        return false;
    }
    let mut probe_hits = probe.hits.iter();
    reference.hits.iter().all(|expected| {
        probe_hits.by_ref().any(|actual| {
            actual.function_index == expected.function_index
                && actual.block_index == expected.block_index
        })
    })
}

pub(crate) fn seed_index(seed: u64, len: usize) -> Option<usize> {
    let len = u64::try_from(len).ok()?;
    if len == 0 {
        return None;
    }
    usize::try_from(seed % len).ok()
}

fn main_control_flow_insertions(source: &str, program: &arandu_parser::Program) -> Vec<usize> {
    let Some(main) = program.decls.iter().find_map(|&id| {
        match program.pool.decl(id) {
            arandu_parser::TopLevelDecl::Func(func)
                if matches!(&func.name, arandu_parser::FuncName::Free { name, .. } if name == "main") =>
            {
                Some(func)
            }
            _ => None,
        }
    }) else {
        return Vec::new();
    };

    let main_span = main.body.span;
    let excluded_spans = program
        .pool
        .exprs
        .iter()
        .enumerate()
        .filter(|(_, expr)| {
            matches!(
                expr,
                arandu_parser::ExprKind::Lambda { .. } | arandu_parser::ExprKind::AsyncBlock { .. }
            )
        })
        .map(|(index, _)| program.pool.expr_spans[index])
        .chain(
            program
                .pool
                .stmts
                .iter()
                .filter(|stmt| {
                    matches!(
                        stmt,
                        arandu_parser::Stmt::Defer { .. } | arandu_parser::Stmt::ErrDefer { .. }
                    )
                })
                .map(arandu_parser::Stmt::span),
        )
        .chain(
            program
                .pool
                .stmts
                .iter()
                .filter(|stmt| {
                    matches!(
                        stmt,
                        arandu_parser::Stmt::For { .. } | arandu_parser::Stmt::While { .. }
                    )
                })
                .map(arandu_parser::Stmt::span),
        )
        .collect::<Vec<_>>();

    let mut positions = Vec::new();
    let mut add_block = |block: &arandu_parser::Block| {
        let span = block.span;
        if span.start < main_span.start
            || span.end > main_span.end
            || excluded_spans
                .iter()
                .any(|excluded| span.start >= excluded.start && span.end <= excluded.end)
        {
            return;
        }
        let Ok(start) = usize::try_from(span.start) else {
            return;
        };
        if source.as_bytes().get(start) == Some(&b'{') {
            positions.push(start + 1);
        }
    };

    for stmt in &program.pool.stmts {
        if let arandu_parser::Stmt::If {
            then_block,
            else_block,
            ..
        } = stmt
        {
            for block in std::iter::once(then_block).chain(else_block.iter()) {
                add_block(block);
            }
        }
    }
    for (index, expr) in program.pool.exprs.iter().enumerate() {
        if let arandu_parser::ExprKind::If {
            then_block,
            else_block,
            ..
        } = expr
        {
            let span = program.pool.expr_spans[index];
            if span.start < main_span.start
                || span.end > main_span.end
                || excluded_spans
                    .iter()
                    .any(|excluded| span.start >= excluded.start && span.end <= excluded.end)
            {
                continue;
            }
            for block_id in [*then_block, *else_block] {
                add_block(program.pool.block(block_id));
            }
        } else if let arandu_parser::ExprKind::Match { arms, .. } = expr {
            let span = program.pool.expr_spans[index];
            if span.start < main_span.start
                || span.end > main_span.end
                || excluded_spans
                    .iter()
                    .any(|excluded| span.start >= excluded.start && span.end <= excluded.end)
            {
                continue;
            }
            for &arm_id in program.pool.match_arm_list(*arms) {
                if let arandu_parser::MatchArmBody::Block { block, .. } =
                    &program.pool.match_arm(arm_id).body
                {
                    add_block(block);
                }
            }
        } else if let arandu_parser::ExprKind::Catch { handler, .. } = expr {
            let span = program.pool.expr_spans[index];
            if span.start < main_span.start
                || span.end > main_span.end
                || excluded_spans
                    .iter()
                    .any(|excluded| span.start >= excluded.start && span.end <= excluded.end)
            {
                continue;
            }
            if let arandu_parser::CatchHandler::Block { block, .. } =
                &program.pool.catch_handler(*handler)
            {
                add_block(block);
            }
        }
    }
    positions.sort_unstable();
    positions.dedup();
    positions
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoopProbe {
    main_start: usize,
    body_insertion: usize,
    after_loop: usize,
}

fn main_loop_probes(source: &str, program: &arandu_parser::Program) -> Vec<LoopProbe> {
    let Some(main) = program.decls.iter().find_map(|&id| {
        match program.pool.decl(id) {
            arandu_parser::TopLevelDecl::Func(func)
                if matches!(&func.name, arandu_parser::FuncName::Free { name, .. } if name == "main") =>
            {
                Some(func)
            }
            _ => None,
        }
    }) else {
        return Vec::new();
    };

    let main_span = main.body.span;
    let excluded_spans = program
        .pool
        .exprs
        .iter()
        .enumerate()
        .filter(|(_, expr)| {
            matches!(
                expr,
                arandu_parser::ExprKind::Lambda { .. } | arandu_parser::ExprKind::AsyncBlock { .. }
            )
        })
        .map(|(index, _)| program.pool.expr_spans[index])
        .chain(
            program
                .pool
                .stmts
                .iter()
                .filter(|stmt| {
                    matches!(
                        stmt,
                        arandu_parser::Stmt::Defer { .. } | arandu_parser::Stmt::ErrDefer { .. }
                    )
                })
                .map(arandu_parser::Stmt::span),
        )
        .collect::<Vec<_>>();

    let Ok(main_start) = usize::try_from(main_span.start) else {
        return Vec::new();
    };
    let Some(main_start) = main_start.checked_add(1) else {
        return Vec::new();
    };
    if source.as_bytes().get(main_start.saturating_sub(1)) != Some(&b'{') {
        return Vec::new();
    }

    let mut probes = Vec::new();
    for stmt in &program.pool.stmts {
        let (span, body) = match stmt {
            arandu_parser::Stmt::For { span, body, .. }
            | arandu_parser::Stmt::While { span, body, .. } => (span, body),
            _ => continue,
        };
        if span.start < main_span.start
            || span.end > main_span.end
            || excluded_spans
                .iter()
                .any(|excluded| span.start >= excluded.start && span.end <= excluded.end)
        {
            continue;
        }
        // The probe's observation runs after the loop. A function-level
        // return (including `?`, which lowers to an early return) inside the
        // body can bypass that observation and make an entered body look dead.
        // Nested lambdas/defer bodies are excluded because their return exits a
        // different control-flow context.
        let has_early_exit = program.pool.stmts.iter().any(|stmt| {
            let stmt_span = stmt.span();
            matches!(stmt, arandu_parser::Stmt::Return { .. })
                && stmt_span.start >= span.start
                && stmt_span.end <= span.end
                && !excluded_spans.iter().any(|excluded| {
                    stmt_span.start >= excluded.start && stmt_span.end <= excluded.end
                })
        }) || program.pool.exprs.iter().enumerate().any(|(index, expr)| {
            let expr_span = program.pool.expr_spans[index];
            matches!(expr, arandu_parser::ExprKind::Try { .. })
                && expr_span.start >= span.start
                && expr_span.end <= span.end
                && !excluded_spans.iter().any(|excluded| {
                    expr_span.start >= excluded.start && expr_span.end <= excluded.end
                })
        });
        if has_early_exit {
            continue;
        }
        let (Ok(body_start), Ok(after_loop)) =
            (usize::try_from(body.span.start), usize::try_from(span.end))
        else {
            continue;
        };
        let Some(body_insertion) = body_start.checked_add(1) else {
            continue;
        };
        if source.as_bytes().get(body_start) != Some(&b'{')
            || after_loop > source.len()
            || !source.is_char_boundary(after_loop)
        {
            continue;
        }
        probes.push(LoopProbe {
            main_start,
            body_insertion,
            after_loop,
        });
    }
    probes.sort_unstable_by_key(|probe| (probe.body_insertion, probe.after_loop));
    probes.dedup_by_key(|probe| (probe.body_insertion, probe.after_loop));
    probes
}

fn instrument_loop_probe(source: &str, probe: LoopProbe, exit_code: i32) -> Option<String> {
    let flag = fresh_emi_binding(source);
    let mut edits = [
        (
            probe.main_start,
            format!("\n    let mut {flag}: bool = false"),
        ),
        (
            probe.body_insertion,
            format!("\n        set {flag} = true\n"),
        ),
        (
            probe.after_loop,
            format!("\n    if {flag} {{ return {exit_code} }}\n"),
        ),
    ];
    edits.sort_unstable_by_key(|(position, _)| std::cmp::Reverse(*position));

    let mut instrumented = source.to_owned();
    for (position, insertion) in edits {
        if !instrumented.is_char_boundary(position) || position > instrumented.len() {
            return None;
        }
        instrumented.insert_str(position, &insertion);
    }
    Some(instrumented)
}

fn insert_in_block(source: &str, insertion: usize, statement: &str) -> Option<String> {
    let prefix = source.get(..insertion)?;
    let suffix = source.get(insertion..)?;
    Some(format!("{prefix}\n        {statement}\n{suffix}"))
}

fn fresh_emi_binding(source: &str) -> String {
    let mut suffix = 0_u32;
    loop {
        let candidate = format!("__arandu_smith_emi_dead_{suffix}");
        if !source.contains(&candidate) {
            return candidate;
        }
        suffix = suffix.wrapping_add(1);
    }
}

/// Inject pure, bounded code into a statically unreachable branch in `main`.
pub(crate) fn inject_dead_pure_statement(source: &str, seed: u64) -> Option<String> {
    let tree = arandu_parser::parse_syntax(source);
    inject_dead_pure_statement_in_tree(source, seed, &tree)
}

fn inject_dead_pure_statement_in_tree(
    source: &str,
    seed: u64,
    tree: &arandu_parser::syntax::SyntaxTree,
) -> Option<String> {
    let insertion = tree.items().into_iter().find_map(|item| {
        if item.kind() != arandu_parser::syntax::SyntaxKind::FUNC_ITEM {
            return None;
        }
        let mut tokens = item
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| !token.kind().is_trivia());
        if !tokens.by_ref().any(|token| token.text() == "func")
            || !tokens.next().is_some_and(|token| token.text() == "main")
        {
            return None;
        }
        let body = item
            .children()
            .find(|child| child.kind() == arandu_parser::syntax::SyntaxKind::BLOCK)?;
        let end = usize::try_from(u32::from(body.text_range().end())).ok()?;
        let brace = end.checked_sub(1)?;
        source
            .as_bytes()
            .get(brace)
            .is_some_and(|byte| *byte == b'}')
            .then_some(brace)
    })?;
    let mut rng = Rng::new(seed ^ EMI_SEED_SALT);
    let expression = gen_pure_int(&mut rng, MAX_DEPTH);
    Some(format!(
        "{}\n    if false {{\n        let emi_dead: int = {}\n    }}\n{}",
        &source[..insertion],
        expression,
        &source[insertion..]
    ))
}

fn gen_pure_int(rng: &mut Rng, depth: u8) -> String {
    if depth == 0 || rng.below(3) == 0 {
        return rng.below(4).to_string();
    }
    let left = gen_pure_int(rng, depth - 1);
    match rng.below(5) {
        0 => format!("({left} + {})", rng.below(3)),
        1 => format!("({left} - {})", rng.below(3)),
        2 => format!("({left} * {})", 1 + rng.below(2)),
        3 => format!("({left} / {})", 1 + rng.below(3)),
        _ => format!("({left} % {})", 1 + rng.below(3)),
    }
}
