//! Bounded CST-guided reducer for preserving a caller-supplied failure oracle.

use arandu_parser::syntax::SyntaxKind;

const MAX_ATTEMPTS: usize = 512;

/// Result of reducing a byte-encoded edit or protocol sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteSequenceShrinkResult {
    /// Smallest sequence found within the attempt budget.
    pub sequence: Vec<u8>,
    /// Number of oracle calls, including the original sequence.
    pub attempts: usize,
    /// Number of accepted deletions or byte simplifications.
    pub reductions: usize,
    /// Whether the reduced sequence still satisfies the failure oracle.
    pub reproduced: bool,
}

/// Minimize a byte-encoded sequence with hierarchical chunk deletion, then
/// simplify individual bytes toward common operation boundaries.
///
/// The input counts as the first oracle call. The attempt budget also caps
/// candidate cloning and evaluation, making this usable from bounded fuzz
/// failure handling.
#[must_use]
pub fn shrink_byte_sequence_with_budget(
    input: &[u8],
    max_attempts: usize,
    mut still_fails: impl FnMut(&[u8]) -> bool,
) -> ByteSequenceShrinkResult {
    let max_attempts = max_attempts.max(1);
    let mut result = ByteSequenceShrinkResult {
        sequence: input.to_vec(),
        attempts: 1,
        reductions: 0,
        reproduced: still_fails(input),
    };
    if !result.reproduced {
        return result;
    }

    let mut granularity = 2.min(result.sequence.len());
    while !result.sequence.is_empty() && result.attempts < max_attempts {
        let chunk_size = result.sequence.len().div_ceil(granularity.max(1));
        let mut accepted = false;
        let mut start = 0;
        while start < result.sequence.len() && result.attempts < max_attempts {
            let end = start.saturating_add(chunk_size).min(result.sequence.len());
            let mut candidate = result.sequence.clone();
            candidate.drain(start..end);
            result.attempts += 1;
            if still_fails(&candidate) {
                result.sequence = candidate;
                result.reductions += 1;
                granularity = 2.min(result.sequence.len());
                accepted = true;
                break;
            }
            start = end;
        }
        if accepted {
            continue;
        }
        if granularity >= result.sequence.len() {
            break;
        }
        granularity = (granularity * 2).min(result.sequence.len());
    }

    const SIMPLER_BYTES: [u8; 11] = [0, 1, 2, 3, 4, 5, 8, 15, 16, 32, 64];
    let mut index = 0;
    while index < result.sequence.len() && result.attempts < max_attempts {
        let original = result.sequence[index];
        let mut simplified = false;
        for replacement in SIMPLER_BYTES {
            if replacement == original || result.attempts >= max_attempts {
                continue;
            }
            let mut candidate = result.sequence.clone();
            candidate[index] = replacement;
            result.attempts += 1;
            if still_fails(&candidate) {
                result.sequence = candidate;
                result.reductions += 1;
                simplified = true;
                break;
            }
        }
        if !simplified {
            index += 1;
        }
    }
    result
}

/// Result of a deterministic source reduction pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShrinkResult {
    /// Smallest source found within the attempt budget.
    pub source: String,
    /// Number of candidate sources passed to the oracle, including the input.
    pub attempts: usize,
    /// Number of candidate reductions accepted by the oracle.
    pub reductions: usize,
    /// Whether the reduced source still satisfies the supplied failure oracle.
    pub reproduced: bool,
}

/// Reduce a source while retaining a failure predicate supplied by the caller.
///
/// Reduction repeatedly applies delta-debugging to top-level CST items and
/// non-overlapping statements, then replaces expression subtrees with smaller
/// descendant expressions or literals until a pass finds no further reduction.
/// The budget bounds total
/// oracle calls; candidates and traversal order are deterministic. This helper
/// performs no file I/O and never assumes that a candidate remains valid: the
/// caller's predicate decides whether it still reproduces the failure.
#[must_use]
pub fn shrink_source(source: &str, mut still_fails: impl FnMut(&str) -> bool) -> ShrinkResult {
    shrink_source_with_budget(source, MAX_ATTEMPTS, &mut still_fails)
}

/// Reduce a source under a caller-selected oracle budget.
///
/// The input counts as the first oracle call. A budget of zero is treated as
/// one so the result always reports whether the original source reproduces.
#[must_use]
pub fn shrink_source_with_budget(
    source: &str,
    max_attempts: usize,
    mut still_fails: impl FnMut(&str) -> bool,
) -> ShrinkResult {
    let max_attempts = max_attempts.max(1);
    let mut result = ShrinkResult {
        source: source.to_owned(),
        attempts: 1,
        reductions: 0,
        reproduced: still_fails(source),
    };
    if !result.reproduced {
        return result;
    }

    loop {
        let pass_reductions = result.reductions;
        for stage in [Stage::Items, Stage::Statements] {
            loop {
                if result.attempts >= max_attempts {
                    return result;
                }
                let groups = deletion_groups(&result.source, stage);
                if groups.is_empty() {
                    break;
                }

                let mut accepted = false;
                'groups: for ranges in groups {
                    if ranges.is_empty() {
                        continue;
                    }
                    let mut granularity = 2.min(ranges.len());
                    loop {
                        let mut attempts = result.attempts;
                        let mut accepted_source = None;
                        visit_chunk_deletions(&ranges, granularity, |candidate| {
                            if attempts >= max_attempts {
                                return true;
                            }
                            let Some(candidate_source) = apply_candidate(&result.source, candidate)
                            else {
                                return false;
                            };
                            if candidate_source == result.source {
                                return false;
                            }
                            attempts += 1;
                            if still_fails(&candidate_source) {
                                accepted_source = Some(candidate_source);
                                true
                            } else {
                                false
                            }
                        });
                        result.attempts = attempts;
                        if let Some(source) = accepted_source {
                            result.source = source;
                            result.reductions += 1;
                            accepted = true;
                            break 'groups;
                        }
                        if result.attempts >= max_attempts {
                            return result;
                        }
                        if granularity == ranges.len() {
                            break;
                        }
                        granularity = (granularity * 2).min(ranges.len());
                    }
                }
                if !accepted {
                    break;
                }
            }
        }

        if result.attempts >= max_attempts {
            return result;
        }

        let mut attempts = result.attempts;
        let mut accepted_source = None;
        visit_expression_candidates(&result.source, |candidate| {
            if attempts >= max_attempts {
                return true;
            }
            let Some(candidate_source) = apply_candidate(&result.source, candidate) else {
                return false;
            };
            // Replacements must strictly shorten the program. Besides keeping
            // reduction monotonic, this prevents toggling between equally
            // sized literals when an oracle accepts both values.
            if candidate_source.len() >= result.source.len() {
                return false;
            }
            attempts += 1;
            if still_fails(&candidate_source) {
                accepted_source = Some(candidate_source);
                true
            } else {
                false
            }
        });
        result.attempts = attempts;
        let accepted = accepted_source.is_some();
        if let Some(source) = accepted_source {
            result.source = source;
            result.reductions += 1;
        }
        if !accepted && result.reductions == pass_reductions {
            break;
        }
    }
    result
}

#[derive(Clone, Copy)]
enum Stage {
    Items,
    Statements,
}

#[derive(Clone)]
enum Candidate {
    DeleteRanges(Vec<(usize, usize)>),
    Replace {
        start: usize,
        end: usize,
        replacement: Replacement,
    },
}

#[derive(Clone, Eq, PartialEq)]
enum Replacement {
    SourceRange { start: usize, end: usize },
    Literal(&'static str),
}

fn deletion_groups(source: &str, stage: Stage) -> Vec<Vec<(usize, usize)>> {
    let tree = arandu_parser::parse_syntax(source);
    let loop_safety = loop_safety_ranges(&tree);
    match stage {
        Stage::Items => {
            let ranges = tree
                .item_ranges()
                .into_iter()
                .filter_map(|(start, end)| {
                    Some((usize::try_from(start).ok()?, usize::try_from(end).ok()?))
                })
                .collect();
            vec![ranges]
        }
        Stage::Statements => tree
            .root()
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::BLOCK)
            .map(|block| {
                block
                    .children()
                    .filter(|node| node.kind() == SyntaxKind::STMT)
                    .filter_map(|node| {
                        let range = node.text_range();
                        let range = (
                            usize::try_from(u32::from(range.start())).ok()?,
                            usize::try_from(u32::from(range.end())).ok()?,
                        );
                        loop_candidate_is_safe(range, &loop_safety).then_some(range)
                    })
                    .collect()
            })
            .collect(),
    }
}

/// Visit deletion candidates in delta-debugging order without retaining all
/// chunks and complements at once. At the finest granularity, eagerly
/// materializing every complement would allocate O(n²) ranges.
fn visit_chunk_deletions(
    ranges: &[(usize, usize)],
    granularity: usize,
    mut visit: impl FnMut(Candidate) -> bool,
) -> bool {
    if ranges.is_empty() {
        return false;
    }
    let granularity = granularity.clamp(1, ranges.len());
    for chunk in 0..granularity {
        let Some(start) = chunk_boundary(ranges.len(), granularity, chunk) else {
            return false;
        };
        let Some(end) = chunk_boundary(ranges.len(), granularity, chunk + 1) else {
            return false;
        };
        if start >= end {
            continue;
        }

        if visit(Candidate::DeleteRanges(ranges[start..end].to_vec())) {
            return true;
        }

        // Also try keeping this chunk and deleting everything outside it.
        // This is the complement step in delta debugging; it can remove
        // separated irrelevant regions when no individual contiguous chunk
        // preserves the failure. At granularity two, complements duplicate
        // the two chunk-deletion candidates already emitted above.
        if granularity > 2 {
            let mut complement = Vec::with_capacity(ranges.len() - (end - start));
            complement.extend_from_slice(&ranges[..start]);
            complement.extend_from_slice(&ranges[end..]);
            if !complement.is_empty() && visit(Candidate::DeleteRanges(complement)) {
                return true;
            }
        }
    }
    false
}

fn chunk_boundary(len: usize, granularity: usize, chunk: usize) -> Option<usize> {
    let len = u128::try_from(len).ok()?;
    let chunk = u128::try_from(chunk).ok()?;
    let granularity = u128::try_from(granularity).ok()?;
    let position = len.checked_mul(chunk)? / granularity;
    usize::try_from(position).ok()
}

/// Track termination-critical ranges without excluding every loop-body edit.
/// Only a canonical unconditional countdown in a `while` allows unrelated
/// statements to be reduced. Loops whose termination is not proven from the
/// CST, and all `for` loops, stay fully protected because the JIT oracle runs
/// in-process and cannot safely kill an infinite candidate.
#[derive(Default)]
struct LoopSafetyRanges {
    statements: Vec<(usize, usize)>,
    protected: Vec<(usize, usize)>,
}

fn loop_safety_ranges(tree: &arandu_parser::syntax::SyntaxTree) -> LoopSafetyRanges {
    let mut safety = LoopSafetyRanges::default();
    for token in tree
        .root()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if token.kind() != SyntaxKind::KEYWORD || !matches!(token.text(), "while" | "for") {
            continue;
        }
        let mut parent = token.parent();
        let loop_stmt = loop {
            let Some(node) = parent else { break None };
            if node.kind() == SyntaxKind::STMT {
                break Some(node);
            }
            parent = node.parent();
        };
        let Some(loop_stmt) = loop_stmt else {
            continue;
        };
        let range = node_range(&loop_stmt);
        safety.statements.push(range);

        if token.text() != "while" {
            safety.protected.push(range);
            continue;
        }

        let body_start = loop_stmt
            .children()
            .find(|node| node.kind() == SyntaxKind::BLOCK)
            .map(|node| node_range(&node).0);
        let Some(body_start) = body_start else {
            safety.protected.push(range);
            continue;
        };
        let Some(progress_range) = unconditional_countdown_progress(&loop_stmt, body_start) else {
            safety.protected.push(range);
            continue;
        };
        safety.protected.push((range.0, body_start));
        safety.protected.push(progress_range);
    }
    safety.statements.sort_unstable();
    safety.statements.dedup();
    safety.protected.sort_unstable();
    safety.protected.dedup();
    safety
}

fn node_range(node: &arandu_parser::syntax::SyntaxNode) -> (usize, usize) {
    let range = node.text_range();
    (
        usize::try_from(u32::from(range.start())).unwrap_or(usize::MAX),
        usize::try_from(u32::from(range.end())).unwrap_or(usize::MAX),
    )
}

fn significant_tokens(
    node: &arandu_parser::syntax::SyntaxNode,
) -> Vec<arandu_parser::syntax::SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia() && token.text() != ";")
        .collect()
}

fn unconditional_countdown_progress(
    loop_stmt: &arandu_parser::syntax::SyntaxNode,
    body_start: usize,
) -> Option<(usize, usize)> {
    let header_tokens: Vec<_> = significant_tokens(loop_stmt)
        .into_iter()
        .take_while(|token| {
            usize::try_from(u32::from(token.text_range().start()))
                .is_ok_and(|start| start < body_start)
        })
        .collect();
    let [while_keyword, counter, greater_than, zero] = header_tokens.as_slice() else {
        return None;
    };
    if while_keyword.text() != "while"
        || counter.kind() != SyntaxKind::IDENT
        || greater_than.text() != ">"
        || zero.text() != "0"
    {
        return None;
    }
    let counter_name = counter.text().to_owned();
    let body = loop_stmt
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)?;

    // Nested loops and continue can bypass the decrement or add another
    // unbounded execution path. Leave such loops unchanged by the reducer.
    let body_tokens = significant_tokens(&body);
    if body_tokens
        .iter()
        .any(|token| token.text() == "continue" || token.text() == "while" || token.text() == "for")
    {
        return None;
    }

    let progress_nodes: Vec<_> = body
        .children()
        .filter(|node| node.kind() == SyntaxKind::STMT)
        .filter(|node| {
            let tokens = significant_tokens(node);
            matches!(
                tokens.as_slice(),
                [target, assign, source, subtract, amount]
                    if target.kind() == SyntaxKind::IDENT
                        && target.text() == counter_name
                        && assign.text() == "="
                        && source.kind() == SyntaxKind::IDENT
                        && source.text() == counter_name
                        && subtract.text() == "-"
                        && amount.text() == "1"
            )
        })
        .collect();
    let [progress_node] = progress_nodes.as_slice() else {
        return None;
    };
    let progress_range = node_range(progress_node);

    // Reject any other use of the counter in the body. This prevents hidden
    // writes in nested branches from invalidating the termination proof.
    for token in body_tokens {
        if token.kind() == SyntaxKind::IDENT && token.text() == counter_name {
            let position = node_range(&token.parent()?);
            if !(progress_range.0 <= position.0 && position.1 <= progress_range.1) {
                return None;
            }
        }
    }

    Some(progress_range)
}

fn loop_candidate_is_safe(range: (usize, usize), safety: &LoopSafetyRanges) -> bool {
    // Deleting the whole loop is safe: it cannot make the JIT run longer.
    if safety.statements.contains(&range) {
        return true;
    }
    let inside_loop = safety
        .statements
        .iter()
        .any(|&(start, end)| start <= range.0 && range.1 <= end);
    !inside_loop
        || !safety
            .protected
            .iter()
            .any(|&(start, end)| start < range.1 && range.0 < end)
}

/// Visit candidates one at a time so a small oracle budget does not require
/// retaining every expression replacement for the entire syntax tree.
fn visit_expression_candidates(source: &str, mut visit: impl FnMut(Candidate) -> bool) -> bool {
    let tree = arandu_parser::parse_syntax(source);
    let loop_safety = loop_safety_ranges(&tree);
    let mut expressions: Vec<_> = tree
        .root()
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::EXPR)
        .collect();
    expressions.sort_by_key(|node| {
        let range = node.text_range();
        (
            std::cmp::Reverse(u32::from(range.end()) - u32::from(range.start())),
            u32::from(range.start()),
        )
    });
    for node in expressions {
        let range = node.text_range();
        let Some(start) = usize::try_from(u32::from(range.start())).ok() else {
            continue;
        };
        let Some(end) = usize::try_from(u32::from(range.end())).ok() else {
            continue;
        };
        if !loop_candidate_is_safe((start, end), &loop_safety) {
            continue;
        }
        if source.get(start..end).is_none() {
            continue;
        }
        let mut replacements = node
            .children()
            .filter(|child| child.kind() == SyntaxKind::EXPR)
            .filter_map(|child| {
                let range = child.text_range();
                let child_start = usize::try_from(u32::from(range.start())).ok()?;
                let child_end = usize::try_from(u32::from(range.end())).ok()?;
                source.get(child_start..child_end)?;
                let replacement_len = child_end.checked_sub(child_start)?;
                (replacement_len < end.saturating_sub(start)).then_some(Replacement::SourceRange {
                    start: child_start,
                    end: child_end,
                })
            })
            .collect::<Vec<_>>();
        replacements.sort_by_key(|replacement| match replacement {
            Replacement::SourceRange { start, end } => (end - start, *start),
            Replacement::Literal(literal) => (literal.len(), usize::MAX),
        });
        replacements.dedup();
        replacements.extend(["0", "1", "false", "true"].map(Replacement::Literal));
        for replacement in replacements {
            if visit(Candidate::Replace {
                start,
                end,
                replacement,
            }) {
                return true;
            }
        }
    }
    false
}

fn apply_candidate(source: &str, candidate: Candidate) -> Option<String> {
    match candidate {
        Candidate::DeleteRanges(ranges) => {
            let mut ranges = ranges;
            // CST-derived ranges and their complements preserve source order.
            ranges.reverse();
            let mut candidate = source.to_owned();
            for (start, end) in ranges {
                candidate.replace_range(start..end, "");
            }
            Some(candidate)
        }
        Candidate::Replace {
            start,
            end,
            replacement,
        } => {
            let prefix = source.get(..start)?;
            let suffix = source.get(end..)?;
            let replacement = match replacement {
                Replacement::SourceRange { start, end } => source.get(start..end)?,
                Replacement::Literal(literal) => literal,
            };
            Some(format!("{prefix}{replacement}{suffix}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_sequence_reducer_deletes_noise_and_simplifies_operations() {
        let input = [77, 1, 88, 2, 99];
        let result = shrink_byte_sequence_with_budget(&input, 64, |candidate| {
            candidate.contains(&1) && candidate.contains(&2)
        });

        assert!(result.reproduced);
        assert_eq!(result.sequence, [1, 2]);
        assert!(result.reductions > 0);
        assert!(result.attempts <= 64);
    }

    #[test]
    fn byte_sequence_reducer_preserves_original_when_failure_is_not_reproducible() {
        let input = [3, 5, 8];
        let result = shrink_byte_sequence_with_budget(&input, 0, |_| false);

        assert_eq!(result.sequence, input);
        assert_eq!(result.attempts, 1);
        assert_eq!(result.reductions, 0);
        assert!(!result.reproduced);
    }

    #[test]
    fn removes_irrelevant_items_and_statements_but_keeps_failure() {
        let source = concat!(
            "struct Unused { value: int }\n",
            "func helper(): int {\n",
            "    let dead: int = 8\n",
            "    return 0\n",
            "}\n",
            "func main(): int {\n",
            "    let dead: int = 3\n",
            "    return 7\n",
            "}\n",
        );
        let result = shrink_source(source, |candidate| {
            candidate.contains("return 7")
                && arandu_parser::parse_syntax(candidate)
                    .lex_diagnostics()
                    .is_empty()
        });

        assert!(result.reproduced);
        assert!(result.reductions > 0);
        assert!(result.source.contains("return 7"));
        assert!(!result.source.contains("Unused"));
        assert!(!result.source.contains("helper"));
        assert!(!result.source.contains("dead"));
    }

    #[test]
    fn refuses_to_claim_a_failure_that_does_not_reproduce() {
        let source = "func main(): int { return 0 }\n";
        let result = shrink_source(source, |_| false);
        assert_eq!(result.source, source);
        assert!(!result.reproduced);
        assert_eq!(result.attempts, 1);
        assert_eq!(result.reductions, 0);
    }

    #[test]
    fn honors_the_callers_oracle_budget() {
        let source = concat!(
            "func helper(): int { return 1 }\n",
            "func main(): int { return 7 }\n",
        );
        let result =
            shrink_source_with_budget(source, 3, |candidate| candidate.contains("return 7"));

        assert!(result.reproduced);
        assert!(result.attempts <= 3);
        assert!(result.source.contains("return 7"));
    }

    #[test]
    fn delta_debugging_removes_groups_of_irrelevant_items_with_few_oracle_calls() {
        let mut source = String::new();
        for index in 0..8 {
            source.push_str(&format!("func unused{index}(): int {{ return {index} }}\n"));
        }
        source.push_str("func main(): int { return 7 }\n");

        let result = shrink_source_with_budget(&source, 12, |candidate| {
            candidate.contains("func main()") && candidate.contains("return 7")
        });

        assert!(result.reproduced);
        assert!(result.source.contains("func main()"));
        assert!(!result.source.contains("unused"));
        assert!(result.attempts <= 12);
        assert!(result.reductions >= 1);
    }

    #[test]
    fn delta_debugging_removes_separated_regions_by_testing_chunk_complements() {
        let source = concat!(
            "func x(): int { return 1 }\n",
            "func a(): int { return 2 }\n",
            "func b(): int { return 3 }\n",
            "func y(): int { return 4 }\n",
        );
        let result = shrink_source_with_budget(source, 64, |candidate| {
            candidate == source
                || (!candidate.contains("func x()")
                    && !candidate.contains("func y()")
                    && candidate.contains("func a()"))
        });

        assert!(result.reproduced);
        assert!(!result.source.contains("func x()"));
        assert!(!result.source.contains("func y()"));
        assert!(result.source.contains("func a()"));
        assert!(result.attempts < 64);
    }

    #[test]
    fn delta_deletion_candidates_stop_after_the_oracle_accepts_one() {
        let ranges: Vec<_> = (0..64).map(|index| (index * 2, index * 2 + 1)).collect();
        let mut visited = 0;

        let stopped = visit_chunk_deletions(&ranges, ranges.len(), |_| {
            visited += 1;
            true
        });

        assert!(stopped);
        assert_eq!(visited, 1);
    }

    #[test]
    fn delta_deletion_handles_empty_ranges_and_extreme_partition_arithmetic() {
        assert!(!visit_chunk_deletions(&[], 1, |_| panic!(
            "empty input has no candidates"
        )));
        assert_eq!(
            chunk_boundary(usize::MAX, usize::MAX, usize::MAX - 1),
            Some(usize::MAX - 1)
        );
        assert_eq!(
            chunk_boundary(usize::MAX, usize::MAX, usize::MAX),
            Some(usize::MAX)
        );
    }

    #[test]
    fn shrinking_preserves_loop_bodies_that_bound_jit_execution() {
        let source = concat!(
            "func main(): int {\n",
            "    let mut remaining: int = 3\n",
            "    while remaining > 0 {\n",
            "        remaining = remaining - 1\n",
            "    }\n",
            "    return 7\n",
            "}\n",
        );
        let result = shrink_source_with_budget(source, 32, |candidate| {
            candidate.contains("remaining = remaining - 1")
        });

        assert!(result.reproduced);
        assert!(result.source.contains("remaining = remaining - 1"));
    }

    #[test]
    fn reduces_unrelated_while_body_statements_but_keeps_progress() {
        let source = concat!(
            "func main(): int {\n",
            "    let mut remaining: int = 3\n",
            "    while remaining > 0 {\n",
            "        let unrelated: int = 11\n",
            "        remaining = remaining - 1\n",
            "    }\n",
            "    return 7\n",
            "}\n",
        );
        let result = shrink_source_with_budget(source, 64, |candidate| {
            candidate.contains("remaining = remaining - 1") && candidate.contains("return 7")
        });

        assert!(result.reproduced);
        assert!(result.source.contains("remaining = remaining - 1"));
        assert!(!result.source.contains("unrelated"));
    }

    #[test]
    fn only_unconditional_countdown_loops_allow_partial_body_shrinking() {
        for source in [
            concat!(
                "func main(): int {\n",
                "    let mut remaining: int = 3\n",
                "    while remaining > 0 {\n",
                "        if false { remaining = remaining - 1 }\n",
                "        let unchanged: int = remaining\n",
                "    }\n",
                "    return 7\n",
                "}\n",
            ),
            concat!(
                "func main(): int {\n",
                "    let mut remaining: int = 3\n",
                "    while remaining > 0 {\n",
                "        remaining = remaining\n",
                "        let unchanged: int = remaining\n",
                "    }\n",
                "    return 7\n",
                "}\n",
            ),
        ] {
            let tree = arandu_parser::parse_syntax(source);
            let loop_statement = tree
                .root()
                .descendants()
                .find(|node| {
                    node.kind() == SyntaxKind::STMT
                        && node
                            .descendants_with_tokens()
                            .filter_map(|element| element.into_token())
                            .any(|token| token.text() == "while")
                })
                .expect("fixture must contain a while statement");
            let loop_range = node_range(&loop_statement);
            assert!(
                loop_safety_ranges(&tree).protected.contains(&loop_range),
                "loop without an unconditional decrement must stay intact: {source}"
            );
        }
    }

    #[test]
    fn reduces_nested_statements_while_preserving_the_enclosing_block() {
        let source = concat!(
            "func main(): int {\n",
            "    if false {\n",
            "        let dead_inner: int = 3\n",
            "        let dead_sibling: int = 4\n",
            "    }\n",
            "    return 7\n",
            "}\n",
        );

        let result = shrink_source(source, |candidate| {
            candidate.contains("if false") && candidate.contains("return 7")
        });

        assert!(result.reproduced);
        assert!(result.source.contains("if false"));
        assert!(result.source.contains("return 7"));
        assert!(!result.source.contains("dead_inner"));
        assert!(!result.source.contains("dead_sibling"));
    }

    #[test]
    fn revisits_parent_items_after_statement_reduction_unlocks_them() {
        let source = concat!(
            "func helper(): int {\n",
            "    let helper_dead: int = 9\n",
            "    return 1\n",
            "}\n",
            "func main(): int {\n",
            "    let permission: int = 1\n",
            "    return 7\n",
            "}\n",
        );
        let result = shrink_source(source, |candidate| {
            candidate.contains("return 7")
                && (!candidate.contains("permission") || candidate.contains("func helper"))
        });

        assert!(result.reproduced);
        assert!(!result.source.contains("permission"));
        assert!(!result.source.contains("func helper"));
        assert!(result.source.contains("return 7"));
    }

    #[test]
    fn expression_replacements_must_make_strict_size_progress() {
        let source = "func main(): int { return 0 }\n";
        let result = shrink_source(source, |candidate| candidate.contains("return 0"));

        assert!(result.reproduced);
        assert_eq!(result.source, source);
        assert_eq!(result.reductions, 0);
    }

    #[test]
    fn expression_candidates_stop_when_the_oracle_accepts_one() {
        let source = "func main(): int { return (1 + 2) * (3 + 4) }\n";
        let mut visited = 0;

        let stopped = visit_expression_candidates(source, |_| {
            visited += 1;
            true
        });

        assert!(stopped);
        assert_eq!(visited, 1);
    }

    #[test]
    fn replaces_a_parent_expression_with_a_failure_preserving_subexpression() {
        let source = "func main(): int { return (10 + 7) * 99 }\n";
        let result = shrink_source(source, |candidate| {
            candidate.contains("func main()") && candidate.contains("10 + 7")
        });

        assert!(result.reproduced);
        assert!(result.source.contains("10 + 7"));
        assert!(!result.source.contains("99"));
        assert!(result.source.len() < source.len());
    }
}
