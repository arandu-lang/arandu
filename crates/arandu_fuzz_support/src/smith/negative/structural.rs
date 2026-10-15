//! AST-directed edits of admitted source, with a bounded failure-preserving reducer.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use arandu_middle::{types::ArType, DiagCode, Severity, Span};
use arandu_parser::{
    ast_pool::{ComptimeBody, ExprKind},
    BinaryOp, Pattern, Stmt, TopLevelDecl, TypeExpr,
};
use arandu_query::{passes, AnalysisHost};

use super::super::artifact::{artifact_root, panic_payload_message, Failure, NEXT_TEMP_ID};
use super::super::oracle::seed_from_data;
use super::{verify_rejection, Case, Mutation};

const MAX_SOURCE: usize = 16 * 1024;
const MAX_EDITS: usize = 32;
const REDUCTION_BUDGET: usize = 12;

#[derive(Debug)]
pub enum NegativeSourceError {
    InvalidBaseline(String),
    NoEligibleMutation(String),
    Counterexample(String),
}

impl std::fmt::Display for NegativeSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBaseline(s) | Self::NoEligibleMutation(s) | Self::Counterexample(s) => {
                f.write_str(s)
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Edit {
    name: &'static str,
    mutation: Mutation,
    expected: DiagCode,
    span: Span,
    replacement: String,
}

impl Edit {
    fn apply(&self, source: &str) -> Option<String> {
        let start = usize::try_from(self.span.start).ok()?;
        let end = usize::try_from(self.span.end).ok()?;
        source.get(start..end)?;
        let mut output = source.to_owned();
        output.replace_range(start..end, &self.replacement);
        Some(output)
    }
}

fn candidates(source: &str) -> Result<Vec<Edit>, NegativeSourceError> {
    catch_unwind(AssertUnwindSafe(|| candidates_inner(source))).unwrap_or_else(|payload| {
        Err(NegativeSourceError::Counterexample(format!(
            "baseline frontend panicked: {}",
            panic_payload_message(payload)
        )))
    })
}

fn candidates_inner(source: &str) -> Result<Vec<Edit>, NegativeSourceError> {
    if source.len() > MAX_SOURCE {
        return Err(NegativeSourceError::InvalidBaseline(format!(
            "source exceeds {MAX_SOURCE} bytes"
        )));
    }
    let mut host = AnalysisHost::new();
    let file = host.new_file("negative/generated.aru".into(), source.to_owned());
    let parsed = passes::parse(host.db(), file);
    let Ok(program) = &**parsed else {
        return Err(NegativeSourceError::InvalidBaseline(
            "baseline has syntax errors".into(),
        ));
    };
    let typed = passes::type_check(host.db(), file);
    if typed.diagnostics.iter().any(|d| d.is_ice()) {
        return Err(NegativeSourceError::Counterexample(format!(
            "baseline produced ICE: {:?}",
            typed.diagnostics
        )));
    }
    if typed
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error || d.is_ice())
    {
        return Err(NegativeSourceError::InvalidBaseline(format!(
            "baseline rejected: {:?}",
            typed.diagnostics
        )));
    }
    let pool = &program.pool;
    // Source AST identities remain direct in this runtime-only subset. Do not
    // infer that a source expression survived staging or generic substitution.
    let runtime_only = program.imports.is_empty()
        && pool.decls.iter().all(|d| match d {
            TopLevelDecl::Func(f) => f.generic_params.is_empty(),
            TopLevelDecl::Enum(e) => e.generic_params.is_empty(),
            _ => false,
        })
        && !pool.exprs.iter().any(|e| {
            matches!(
                e,
                ExprKind::Comptime { .. } | ExprKind::Generic { .. } | ExprKind::Layout { .. }
            )
        })
        && !pool
            .type_exprs
            .iter()
            .any(|t| matches!(t, TypeExpr::ConstExpression { .. }))
        && !pool.stmts.iter().any(|s| {
            matches!(
                s,
                Stmt::If {
                    is_comptime: true,
                    ..
                } | Stmt::For {
                    is_comptime: true,
                    ..
                }
            )
        });
    let mut edits = Vec::new();
    if runtime_only {
        for statement in &pool.stmts {
            let Stmt::VarDecl {
                bindings, value, ..
            } = statement
            else {
                continue;
            };
            if bindings.len() == 1
                && bindings[0].ty.is_some()
                && typed
                    .type_info
                    .expr_type(*value)
                    .is_some_and(|t| t.is_numeric())
            {
                edits.push(Edit {
                    name: "assignment-type",
                    mutation: Mutation::AssignmentType,
                    expected: DiagCode::T002IncompatibleAssignment,
                    span: pool.expr_span(*value),
                    replacement: "false".into(),
                });
            }
        }
        for expression in &pool.exprs {
            match expression {
                ExprKind::Binary {
                    op: BinaryOp::Div | BinaryOp::Mod,
                    left,
                    right,
                } if typed
                    .type_info
                    .expr_type(*left)
                    .is_some_and(|t| t.is_integer()) =>
                {
                    edits.push(Edit {
                        name: "zero-divisor",
                        mutation: Mutation::ZeroDivisor,
                        expected: DiagCode::T040DivisionByZero,
                        span: pool.expr_span(*right),
                        replacement: "(0)".into(),
                    });
                }
                ExprKind::Match { value, arms } if arms.len == 1 => {
                    let is_enum = typed.type_info.expr_type(*value).is_some_and(|t| match t {
                        ArType::Named(id, _) => typed
                            .type_info
                            .enum_variants
                            .values()
                            .any(|(parent, _)| *parent == id),
                        _ => false,
                    });
                    let arm = pool.match_arm(pool.match_arm_list(*arms)[0]);
                    if is_enum && arm.guard.is_none() {
                        if let Pattern::Wildcard { span } | Pattern::Bind { span, .. } =
                            pool.pattern(arm.pattern)
                        {
                            edits.push(Edit {
                                name: "guarded-catchall",
                                mutation: Mutation::GuardedWildcard,
                                expected: DiagCode::T024NonExhaustiveMatch,
                                span: Span::new(span.file_id, span.end, span.end),
                                replacement: " if false".into(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // Capture edits require an immutable runtime local in the function's outer
    // block, preceding an expression root. Nested block roots are not inferred.
    for declaration in &pool.decls {
        let TopLevelDecl::Func(function) = declaration else {
            continue;
        };
        if !function.generic_params.is_empty() {
            continue;
        }
        for &statement in pool.stmt_list(function.body.statements) {
            let Stmt::VarDecl {
                span,
                bindings,
                value,
            } = pool.stmt(statement)
            else {
                continue;
            };
            if bindings.len() != 1
                || bindings[0].mutable
                || !matches!(pool.expr(*value), ExprKind::Int { .. })
            {
                continue;
            }
            let spelling = &bindings[0].name;
            let definitions = pool
                .stmt_list(function.body.statements)
                .iter()
                .filter_map(|id| match pool.stmt(*id) {
                    Stmt::VarDecl { bindings, .. } => Some(bindings),
                    _ => None,
                })
                .flatten()
                .filter(|binding| binding.name == *spelling)
                .count();
            if definitions != 1 {
                continue;
            }
            for &later in pool.stmt_list(function.body.statements) {
                let expression = match pool.stmt(later) {
                    Stmt::Return { values, .. } if values.len() == 1 => values[0],
                    Stmt::VarDecl { value, .. } => *value,
                    _ => continue,
                };
                let ExprKind::Comptime {
                    body: ComptimeBody::Expression(inner),
                } = pool.expr(expression)
                else {
                    continue;
                };
                if span.end < pool.expr_span(expression).start {
                    edits.push(Edit {
                        name: "runtime-capture",
                        mutation: Mutation::RuntimeCapture,
                        expected: DiagCode::T043ComptimeRuntimeCapture,
                        span: pool.expr_span(*inner),
                        replacement: bindings[0].name.to_string(),
                    });
                }
            }
        }
    }
    // Syntax edits are independent of staged reachability: the parser must
    // still inspect both sides of a compile-time branch.
    let syntax = arandu_query::syntax_tree(host.db(), file);
    for node in syntax
        .root()
        .descendants()
        .filter(|node| node.kind() == arandu_parser::SyntaxKind::BLOCK)
    {
        let closing = node
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .filter(|t| t.kind() == arandu_parser::SyntaxKind::PUNCT && t.text() == "}")
            .last();
        if let Some(token) = closing {
            // Removing an earlier function's brace can instead produce P005
            // at the following declaration. P001 is guaranteed only at EOF.
            if !source
                .get(usize::from(token.text_range().end())..)
                .is_some_and(|tail| tail.trim().is_empty())
            {
                continue;
            }
            edits.push(Edit {
                name: "missing-brace",
                mutation: Mutation::MissingBrace,
                expected: DiagCode::P001UnexpectedToken,
                span: Span::new(
                    program.span.file_id,
                    u32::from(token.text_range().start()),
                    u32::from(token.text_range().end()),
                ),
                replacement: "".into(),
            });
        }
    }
    for token in syntax
        .root()
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        if token.kind() == arandu_parser::SyntaxKind::STRING
            && token.text().starts_with('"')
            && token.text().ends_with('"')
        {
            let end = u32::from(token.text_range().end());
            let Some(start) = end.checked_sub(1) else {
                continue;
            };
            if source
                .get(usize::try_from(end).unwrap_or(usize::MAX)..)
                .is_some_and(|tail| !tail.contains('"'))
            {
                edits.push(Edit {
                    name: "unterminated-string",
                    mutation: Mutation::UnterminatedUnicodeString,
                    expected: DiagCode::LX001UnterminatedString,
                    span: Span::new(program.span.file_id, start, end),
                    replacement: "".into(),
                });
            }
        }
    }
    edits.sort_by_key(|e| (e.span.start, e.span.end, e.name));
    edits.dedup_by(|a, b| a.span == b.span && a.name == b.name);
    edits.truncate(MAX_EDITS);
    Ok(edits)
}

fn check_edit(source: &str, edit: &Edit) -> Option<Failure> {
    let invalid = edit.apply(source)?;
    let case = Case {
        mutation: edit.mutation,
        valid: source.to_owned(),
        invalid,
        expected: edit.expected,
    };
    catch_unwind(AssertUnwindSafe(|| verify_rejection(&case)))
        .unwrap_or_else(|payload| {
            Err(Failure::new(
                "negative-mutation-panicked",
                panic_payload_message(payload),
                true,
            ))
        })
        .err()
        .map(|failure| failure.with_scope(format!("{}:{}", edit.name, edit.expected.as_str())))
}

/// Check all eligible edits (bounded to MAX_EDITS), optionally for one family.
pub fn verify_source(source: &str, name: Option<&str>) -> Result<usize, NegativeSourceError> {
    let edits = candidates(source)?;
    let mut checked = 0;
    for edit in edits.iter().filter(|e| name.is_none_or(|n| n == e.name)) {
        checked += 1;
        if let Some(failure) = check_edit(source, edit) {
            return Err(NegativeSourceError::Counterexample(format!("{failure:?}")));
        }
    }
    if checked == 0 {
        return Err(NegativeSourceError::NoEligibleMutation(format!(
            "no eligible mutation for {name:?}"
        )));
    }
    Ok(checked)
}

fn generated(seed: u64) -> String {
    let bytes = seed.to_le_bytes();
    let mut source = "enum GeneratedFlag { A, B }\nfunc select(flag: GeneratedFlag): int { return match flag { _ => 1 } }\n".to_owned();
    let count = bytes[0] % 4 + 1;
    for index in 0..count {
        let value = bytes[usize::from(index) + 1] % 31 + 1;
        let denominator = value % 7 + 1;
        let body = match index % 3 {
            0 => "if input > 0 { let nested: int = values[0]; return nested } else { return values[1] }",
            1 => "let mut total: int = 0; let mut i: int = 0; while i < 2 { let nested: int = values[i]; total += nested; i += 1 }; return total",
            _ => "if input > 0 { if input > 2 { let nested: int = values[0]; return nested }; return values[1] }; return quotient",
        };
        source.push_str(&format!("func helper_{index}(input: int): int {{ let local: int = input + {value}; let quotient: int = local / {denominator}; let values = [quotient, local]; {body} }}\n"));
    }
    source.push_str("func main(): int { let text: str = \"ação_界_🦀\"; let mut answer: int = select(GeneratedFlag.A);\n");
    for index in 0..count {
        source.push_str(&format!("answer += helper_{index}({index});\n"));
    }
    source.push_str("return answer }\n");
    source
}

fn reduce(source: &str, edit: &Edit, failure: &Failure) -> (String, Option<Edit>, usize) {
    reduce_with(source, edit, failure, check_edit)
}

fn reduce_with(
    source: &str,
    edit: &Edit,
    failure: &Failure,
    mut check: impl FnMut(&str, &Edit) -> Option<Failure>,
) -> (String, Option<Edit>, usize) {
    let mut checks = 0;
    let mut retained = None;
    let reduced =
        crate::shrinker::shrink_source_with_budget(source, REDUCTION_BUDGET, |candidate| {
            let Ok(edits) = candidates(candidate) else {
                return false;
            };
            for proposed in edits.into_iter().filter(|e| e.name == edit.name) {
                if checks >= REDUCTION_BUDGET {
                    break;
                }
                checks += 1;
                if check(candidate, &proposed).is_some_and(|f| f.same_identity(failure)) {
                    retained = Some((candidate.to_owned(), proposed));
                    return true;
                }
            }
            false
        });
    let confirmation_checks = usize::from(retained.is_some());
    let confirmed = retained.and_then(|(valid, selected)| {
        if valid == reduced.source
            && check(&valid, &selected).is_some_and(|f| f.same_identity(failure))
        {
            Some(selected)
        } else {
            None
        }
    });
    (reduced.source, confirmed, checks + confirmation_checks)
}

fn save(root: &Path, source: &str, edit: &Edit, failure: &Failure) -> Result<PathBuf, String> {
    let (minimized, confirmed, attempts) = reduce(source, edit, failure);
    let (valid, selected) = confirmed
        .as_ref()
        .map_or((source, edit), |e| (minimized.as_str(), e));
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let id = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = root.join(format!("negative-{}-{id}", std::process::id()));
    std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
    let invalid = selected.apply(valid).ok_or("invalid mutation edit")?;
    for (name, bytes) in [
        ("original-valid.aru", source),
        ("valid.aru", valid),
        ("mutant.aru", invalid.as_str()),
    ] {
        std::fs::write(directory.join(name), bytes).map_err(|e| e.to_string())?;
    }
    let original_mutant = edit.apply(source).ok_or("invalid original mutation edit")?;
    std::fs::write(directory.join("original-mutant.aru"), original_mutant)
        .map_err(|e| e.to_string())?;
    let metadata = serde_json::json!({"version":1,"mutation":edit.name,"expected_code":edit.expected.as_str(),"failure_kind":failure.kind,"failure_scope":failure.scope,"failure_message":failure.message,"original_edit":{"start":edit.span.start,"end":edit.span.end,"replacement":edit.replacement},"reduced_edit":{"start":selected.span.start,"end":selected.span.end,"replacement":selected.replacement},"shrink_attempts":attempts,"shrink_confirmed":confirmed.is_some(),"replay_args":["cargo","run","--locked","-p","xtask","--","check-negative-source",directory.join("valid.aru"),edit.name]});
    std::fs::write(
        directory.join("negative.json"),
        serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(directory)
}

pub(super) fn run(data: &[u8]) {
    let seed = seed_from_data(data);
    let supplied = std::str::from_utf8(data)
        .ok()
        .and_then(|s| match candidates(s) {
            Ok(edits) if !edits.is_empty() => Some((s.to_owned(), edits)),
            Err(NegativeSourceError::Counterexample(error)) => {
                panic!("negative baseline oracle: {error}\n{s}")
            }
            _ => None,
        });
    let (source, edits) = supplied.unwrap_or_else(|| {
        let source = generated(seed);
        let edits = candidates(&source)
            .unwrap_or_else(|e| panic!("invalid negative generator: {e}\n{source}"));
        (source, edits)
    });
    let requested = source
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("// negative-family: "));
    let eligible: Vec<_> = edits
        .iter()
        .filter(|e| requested.is_none_or(|name| name == e.name))
        .collect();
    let eligible = if eligible.is_empty() {
        edits.iter().collect()
    } else {
        eligible
    };
    let index = usize::from(seed.to_le_bytes()[0]) % eligible.len();
    let edit = eligible[index];
    if let Some(failure) = check_edit(&source, edit) {
        let artifact = save(&artifact_root(), &source, edit, &failure);
        panic!("negative mutation oracle: {failure:?}; artifact={artifact:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_seeds_contain_valid_source_and_select_their_named_mutation_family() {
        let seeds = [
            include_str!("../../../../../tests/fuzz-regressions/negative-guard-wildcard.seed"),
            include_str!("../../../../../tests/fuzz-regressions/negative-guard-binding.seed"),
            include_str!("../../../../../tests/fuzz-regressions/negative-block-eof.seed"),
            include_str!("../../../../../tests/fuzz-regressions/negative-unicode-string.seed"),
            include_str!("../../../../../tests/fuzz-regressions/negative-comptime-capture.seed"),
            include_str!("../../../../../tests/fuzz-regressions/negative-assignment-type.seed"),
            include_str!("../../../../../tests/fuzz-regressions/negative-zero-divisor.seed"),
        ];
        for seed in seeds {
            let source = seed.strip_prefix("encoding=utf8\n").expect("seed encoding");
            let family = source
                .lines()
                .next()
                .and_then(|s| s.strip_prefix("// negative-family: "))
                .expect("explicit family");
            assert!(verify_source(source, Some(family)).expect("source oracle") > 0);
            run(source.as_bytes());
        }
    }

    #[test]
    fn composed_programs_have_checked_mutations_in_helpers_branches_loops_and_arrays() {
        let mut names = std::collections::BTreeSet::new();
        for seed in [0, 1, 2, 3, 255, 256, u64::MAX] {
            let source = generated(seed);
            assert_eq!(source, generated(seed));
            let edits = candidates(&source).expect("valid generated source");
            assert!(edits.len() <= MAX_EDITS);
            for edit in &edits {
                names.insert(edit.name);
                assert!(edit.apply(&source).is_some());
                assert!(check_edit(&source, edit).is_none(), "{seed}: {edit:?}");
            }
            assert!(verify_source(&source, None).expect("rejection oracle") >= 5);
        }
        assert!(names.contains("assignment-type"));
        assert!(names.contains("zero-divisor"));
        assert!(names.contains("guarded-catchall"));
        assert!(names.contains("missing-brace"));
        assert!(names.contains("unterminated-string"));
        assert!(generated(3).contains("while"));
        assert!(generated(3).contains("if input > 2"));
    }

    #[test]
    fn capture_mutation_uses_a_preceding_runtime_binding_in_the_same_outer_block() {
        let source = "func main(): int { let outside: int = 7; return comptime (1 + 2) }";
        assert_eq!(verify_source(source, Some("runtime-capture")).unwrap(), 1);
        let unsupported =
            "func main(): int { comptime for outside in 0..2 { let x = comptime 7 }; return 0 }";
        assert!(matches!(
            verify_source(unsupported, Some("runtime-capture")),
            Err(NegativeSourceError::NoEligibleMutation(_))
        ));
        let shadowed =
            "func main(): int { let outside = 1; let outside = comptime 2; return comptime 3 }";
        let result = verify_source(shadowed, Some("runtime-capture"));
        assert!(
            matches!(result, Err(NegativeSourceError::InvalidBaseline(_))),
            "{result:?}"
        );
    }

    #[test]
    fn semantic_edits_do_not_guess_types_in_discarded_branches_or_float_division() {
        for source in [
            "func main(): int { comptime if false { let x: int = 2 / 1; return x } else { return 0 } }",
            "func id<T>(x: T): T { let y: int = 2 / 1; return x }\nfunc main(): int { return id<int>(3) }",
            "func main(): float { return 2.0 / 1.0 }",
            "func main(): int { let values: [comptime (2)]int = [1, 2]; return values[0] / 1 }",
        ] {
            assert!(matches!(verify_source(source, Some("zero-divisor")), Err(NegativeSourceError::NoEligibleMutation(_))));
        }
        assert!(matches!(
            verify_source(
                "func main(): int { return match 1 { _ => 42 } }",
                Some("guarded-catchall")
            ),
            Err(NegativeSourceError::NoEligibleMutation(_))
        ));
    }

    #[test]
    fn invalid_baselines_and_missing_families_are_not_reported_as_compiler_bugs() {
        assert!(matches!(
            verify_source("func main() {", None),
            Err(NegativeSourceError::InvalidBaseline(_))
        ));
        assert!(matches!(
            verify_source(&generated(0), Some("invented")),
            Err(NegativeSourceError::NoEligibleMutation(_))
        ));
        assert!(matches!(
            verify_source(&" ".repeat(MAX_SOURCE + 1), None),
            Err(NegativeSourceError::InvalidBaseline(_))
        ));
    }

    #[test]
    fn source_reducer_preserves_a_valid_baseline_and_the_same_failure_class_with_a_budget() {
        let source = "func unused(): int { return 17 }\nfunc main(): int { let target: int = 9; return target }";
        let edit = candidates(source)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "assignment-type")
            .unwrap();
        let failure = Failure::new("injected-counterexample", "test", true)
            .with_scope("assignment-type:T002");
        let (reduced, selected, attempts) = reduce_with(source, &edit, &failure, |_, e| {
            (e.name == "assignment-type").then(|| failure.clone())
        });
        assert!(selected.is_some());
        assert!(reduced.len() < source.len());
        assert!(candidates(&reduced).is_ok());
        assert!(attempts <= REDUCTION_BUDGET + 1);
        let (_, rejected, _) = reduce_with(source, &edit, &failure, |_, _| {
            Some(Failure::new("unrelated-failure", "test", true))
        });
        assert!(rejected.is_none());
    }

    #[test]
    fn artifact_preserves_readable_sources_expected_code_and_explicit_replay_arguments() {
        let source = "func main(): int { let target: int = 9; return target }";
        let edit = candidates(source)
            .unwrap()
            .into_iter()
            .find(|e| e.name == "assignment-type")
            .unwrap();
        let failure = Failure::new("injected-writer-test", "test", false);
        let root = std::env::temp_dir().join(format!(
            "negative-artifact-test-{}-{}",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let directory = save(&root, source, &edit, &failure).unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join("valid.aru")).unwrap(),
            source
        );
        assert!(std::fs::read_to_string(directory.join("mutant.aru"))
            .unwrap()
            .contains("false"));
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("negative.json")).unwrap())
                .unwrap();
        assert_eq!(metadata["expected_code"], "T002");
        assert_eq!(metadata["replay_args"][6], "check-negative-source");
        assert_eq!(metadata["shrink_confirmed"], false);
        std::fs::remove_dir_all(root).unwrap();
    }
}
