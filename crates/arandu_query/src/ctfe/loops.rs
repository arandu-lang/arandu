//! Finite static domains evaluated by the canonical AMIR VM before lowering.
//! A runtime body is never executed to discover a domain.

use super::{CtfeRoot, CtfeRootRequest, RootSelector};
use crate::{db::HashEq, ArandCompilerDb, SourceFile, StableHash};
use arandu_middle::{
    ctfe::{ConstInt, ConstValue},
    DiagCode, Diagnostic, NodeKey, Span, SymbolId,
};
use arandu_parser::{ForClause, Stmt, TopLevelDecl};
use rustc_hash::FxHashMap;

pub const MAX_STATIC_ITERATIONS: u64 = 4096;

#[derive(Debug, Clone, Default)]
pub struct StaticLoops {
    pub arrays: FxHashMap<NodeKey, arandu_middle::ctfe::ConstAggregate>,
    pub domains: FxHashMap<NodeKey, (ConstInt, ConstInt)>,
    pub diagnostics: Vec<Diagnostic>,
}

impl StaticLoops {
    pub(crate) fn empty_domain(&self, node: &NodeKey) -> bool {
        self.domains
            .get(node)
            .is_some_and(|(lower, upper)| upper.value() <= lower.value())
    }
}

impl StableHash for StaticLoops {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        h.update(b"StaticLoops/v2");
        let mut domains: Vec<_> = self.domains.iter().collect();
        domains.sort_by_key(|(key, _)| (key.start, key.end));
        for (key, (lower, upper)) in domains {
            h.update(&key.start.to_le_bytes());
            h.update(&key.end.to_le_bytes());
            h.update(&ConstValue::Integer(*lower).canonical_bytes());
            h.update(&ConstValue::Integer(*upper).canonical_bytes());
        }
        let mut arrays: Vec<_> = self.arrays.iter().collect();
        arrays.sort_by_key(|(key, _)| (key.start, key.end));
        for (key, value) in arrays {
            h.update(&key.start.to_le_bytes());
            h.update(&key.end.to_le_bytes());
            h.update(&ConstValue::Aggregate(value.clone()).canonical_bytes());
        }
        h.update(self.diagnostics.stable_hash().as_bytes());
        h.finalize()
    }
}

fn cycle(
    db: &dyn ArandCompilerDb,
    _id: salsa::Id,
    file: SourceFile,
    _owner: SymbolId,
) -> HashEq<StaticLoops> {
    HashEq::new(StaticLoops {
        arrays: FxHashMap::default(),
        domains: FxHashMap::default(),
        diagnostics: vec![Diagnostic::error(
            DiagCode::T044ComptimeEvaluationFailed,
            "static loop domain depends on itself",
            Span::new(*file.file_id(db), 0, 0),
        )],
    })
}

#[salsa::tracked(cycle_result = cycle)]
#[tracing::instrument(level="trace", target="arandu_query", skip(db, file), fields(query="item_static_loops", file=?file.file_id(db)))]
pub fn item_static_loops(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
) -> HashEq<StaticLoops> {
    let branches = super::item_static_branches(db, file, owner);
    select_loops(db, file, owner, None, branches)
}

pub(crate) fn select_loops(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    branches: &super::StaticBranches,
) -> HashEq<StaticLoops> {
    select_loops_in_occurrence(db, file, owner, instance, branches, &[])
}

pub(crate) fn select_loops_in_occurrence(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    branches: &super::StaticBranches,
    occurrence: &[(u32, super::FrozenConstant, u64)],
) -> HashEq<StaticLoops> {
    select_loops_in_context(
        db,
        file,
        owner,
        instance,
        branches,
        occurrence,
        &super::DependencyContext::default(),
    )
}

pub(crate) fn select_loops_in_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    branches: &super::StaticBranches,
    occurrence: &[(u32, super::FrozenConstant, u64)],
    context: &super::DependencyContext,
) -> HashEq<StaticLoops> {
    let parsed = crate::passes::parse(db, file);
    let headers = crate::passes::resolved_headers(db, file);
    let mut result = StaticLoops::default();
    let Ok(program) = &**parsed else {
        return HashEq::new(result);
    };
    let mut body = None;
    program.for_each_decl_recursive(|_, declaration| {
        let TopLevelDecl::Func(function) = declaration else {
            return;
        };
        if arandu_semantics::primary_def_key(declaration)
            .and_then(|key| headers.declarations.resolved.definitions.get(&key))
            == Some(&owner)
        {
            body = Some(&function.body);
        }
    });
    let Some(body) = body else {
        return HashEq::new(result);
    };
    let loops = super::roots::static_fors(&program.pool, body.span);
    if loops.is_empty() {
        return HashEq::new(result);
    }
    if loops.len() > 4096 {
        result.diagnostics.push(Diagnostic::error(
            DiagCode::T045ComptimeLimitExceeded,
            "too many static loops in this function",
            body.span,
        ));
        return HashEq::new(result);
    }
    let budget = super::public::staging_budget(
        super::config::public_budget(db),
        loops.len().saturating_mul(2),
        super::public::iteration_count(occurrence),
    );
    let discarded: Vec<_> = super::roots::static_ifs(&program.pool, body.span)
        .into_iter()
        .filter_map(|stmt| {
            let Stmt::If {
                span,
                then_block,
                else_block,
                ..
            } = stmt
            else {
                return None;
            };
            match branches.decisions.get(&(*span).into()) {
                Some(true) => else_block.as_ref().map(|b| b.span),
                Some(false) => Some(then_block.span),
                None => Some(*span),
            }
        })
        .collect();
    for (ordinal, stmt) in loops.iter().enumerate() {
        db.unwind_if_revision_cancelled();
        let span = stmt.span();
        let enclosing: Vec<_> = loops
            .iter()
            .enumerate()
            .filter_map(|(ordinal, statement)| {
                let Stmt::For { body, .. } = statement else {
                    return None;
                };
                (body.span.start <= span.start && span.end <= body.span.end).then_some(ordinal)
            })
            .collect();
        if enclosing.len() != occurrence.len()
            || enclosing
                .iter()
                .zip(occurrence)
                .any(|(ordinal, (selected, _, _))| {
                    usize::try_from(*selected).ok() != Some(*ordinal)
                })
        {
            continue;
        }
        if discarded
            .iter()
            .any(|s| s.start <= span.start && span.end <= s.end)
        {
            continue;
        }
        let Stmt::For {
            clause: ForClause::In {
                bindings, iterable, ..
            },
            ..
        } = stmt
        else {
            result.diagnostics.push(unsupported(span));
            continue;
        };
        if bindings.len() != 1 || bindings[0].mutable {
            result.diagnostics.push(unsupported(span));
            continue;
        }
        let Ok(ordinal) = u32::try_from(ordinal) else {
            continue;
        };
        if !matches!(
            program.pool.expr(*iterable),
            arandu_parser::ExprKind::Binary {
                op: arandu_parser::BinaryOp::RangeExclusive,
                ..
            }
        ) {
            let selector = instance.map_or(RootSelector::StaticForDomain(ordinal), |instance| {
                RootSelector::InInstance {
                    instance: instance.clone(),
                    selector: Box::new(RootSelector::StaticForDomain(ordinal)),
                }
            });
            let root = CtfeRoot::new_in_context(
                db,
                file,
                owner,
                super::roots::in_occurrence(selector, occurrence),
                None,
                context.clone(),
            );
            match super::ctfe_eval_root(db, CtfeRootRequest::new(db, root, budget)) {
                Ok(ConstValue::Aggregate(array))
                    if matches!(array.shape(), arandu_middle::types::TypeShape::Array(..)) =>
                {
                    let Ok(count) = u64::try_from(array.values().len()) else {
                        result.diagnostics.push(unsupported(span));
                        continue;
                    };
                    if count > MAX_STATIC_ITERATIONS {
                        result.diagnostics.push(
                            Diagnostic::error(
                                DiagCode::T045ComptimeLimitExceeded,
                                "static loop exceeds the iteration limit",
                                span,
                            )
                            .with_primary_label("at most 4096 iterations per expansion"),
                        );
                        continue;
                    }
                    let int = arandu_middle::ctfe::IntegerType::new(
                        arandu_middle::types::Primitive::Int,
                        *db.target_config().data_layout(db),
                    );
                    let bounds = int.and_then(|int| {
                        Ok((
                            ConstInt::new(int, 0)?,
                            ConstInt::new(int, i128::from(count))?,
                        ))
                    });
                    match bounds {
                        Ok(bounds) => {
                            result.domains.insert(span.into(), bounds);
                            result.arrays.insert(span.into(), array.clone());
                        }
                        Err(_) => result.diagnostics.push(unsupported(span)),
                    }
                }
                Ok(_) => result.diagnostics.push(unsupported(span)),
                Err(error) => {
                    super::public::append_failure(&mut result.diagnostics, error.clone(), span)
                }
            }
            continue;
        }
        let mut bounds = Vec::with_capacity(2);
        for upper in [false, true] {
            let selector = instance.map_or(
                RootSelector::StaticForBound { ordinal, upper },
                |instance| RootSelector::InInstance {
                    instance: instance.clone(),
                    selector: Box::new(RootSelector::StaticForBound { ordinal, upper }),
                },
            );
            let root = CtfeRoot::new_in_context(
                db,
                file,
                owner,
                super::roots::in_occurrence(selector, occurrence),
                None,
                context.clone(),
            );
            match super::ctfe_eval_root(db, CtfeRootRequest::new(db, root, budget)) {
                Ok(ConstValue::Integer(value)) => bounds.push(*value),
                Ok(_) => result.diagnostics.push(unsupported(span)),
                Err(error) => {
                    super::public::append_failure(&mut result.diagnostics, error.clone(), span)
                }
            }
        }
        if let [lower, upper] = bounds.as_slice() {
            let (mut lower, mut upper) = (*lower, *upper);
            // Endpoint roots are isolated, so an unsuffixed literal defaults
            // before the range's other endpoint supplies its integer context.
            // Restore only checked literal coercion, never an implicit cast of
            // an already typed helper result or other expression.
            if lower.ty() != upper.ty() {
                let arandu_parser::ExprKind::Binary { left, right, .. } =
                    program.pool.expr(*iterable)
                else {
                    continue;
                };
                if is_integer_literal(&program.pool, *left) {
                    if let Ok(value) = lower.cast(upper.ty()) {
                        lower = value;
                    }
                } else if is_integer_literal(&program.pool, *right) {
                    if let Ok(value) = upper.cast(lower.ty()) {
                        upper = value;
                    }
                }
            }
            if lower.ty() != upper.ty() {
                result.diagnostics.push(Diagnostic::error(
                    DiagCode::T046ComptimeArithmetic,
                    "static range endpoints must have the same integer type",
                    span,
                ));
                continue;
            }
            let count = upper
                .value()
                .checked_sub(lower.value())
                .unwrap_or(i128::MAX)
                .max(0);
            if count > i128::from(MAX_STATIC_ITERATIONS) {
                result.diagnostics.push(
                    Diagnostic::error(
                        DiagCode::T045ComptimeLimitExceeded,
                        "static loop exceeds the iteration limit",
                        span,
                    )
                    .with_primary_label("at most 4096 iterations per expansion"),
                );
                continue;
            }
            result.domains.insert(span.into(), (lower, upper));
        }
    }
    // Keep source order independent of map iteration and avoid publishing a
    // partially accepted domain when one of its obligations failed.
    result
        .diagnostics
        .sort_by_key(|d| (d.span.start, d.span.end));
    HashEq::new(result)
}

fn is_integer_literal(
    pool: &arandu_parser::AstPool,
    mut expression: arandu_parser::ExprId,
) -> bool {
    let mut negated = false;
    for _ in 0..128 {
        match pool.expr(expression) {
            arandu_parser::ExprKind::Int { .. } => return true,
            arandu_parser::ExprKind::Group { expr } => expression = *expr,
            arandu_parser::ExprKind::Unary {
                op: arandu_parser::UnaryOp::Neg,
                expr,
            } if !negated => {
                negated = true;
                expression = *expr;
            }
            _ => return false,
        }
    }
    false
}

fn unsupported(span: Span) -> Diagnostic {
    Diagnostic::error(
        DiagCode::T042UnsupportedComptime,
        "comptime for requires one immutable binding over a finite half-open integer range or frozen Copy array",
        span,
    )
    .with_primary_label("use comptime for item in start..end or a fixed Copy array")
}

/// Freeze the iterator itself so residual code never calls its pure producer.
/// The AST expression and its semantic type stay in the destination domain.
pub(crate) fn install_array_values(
    checked: &mut arandu_typeck::TypeCheckResult,
    program: &arandu_parser::Program,
    loops: &StaticLoops,
    layout: arandu_middle::DataLayout,
) {
    for statement in &program.pool.stmts {
        let Stmt::For {
            span,
            clause: ForClause::In { iterable, .. },
            ..
        } = statement
        else {
            continue;
        };
        let Some(value) = loops.arrays.get(&(*span).into()) else {
            continue;
        };
        match value.shape().intern(&checked.type_info.type_interner) {
            Ok(ty) => {
                let info = checked.type_info_mut();
                info.record_expr_type(*iterable, ty);
                info.ctfe_values.insert(
                    program.pool.expr_span(*iterable),
                    (ConstValue::Aggregate(value.clone()), layout),
                );
            }
            Err(_) => checked.diagnostics.push(Diagnostic::error(
                DiagCode::T042UnsupportedComptime,
                "static array domain has an unsupported element type",
                *span,
            )),
        }
    }
}
