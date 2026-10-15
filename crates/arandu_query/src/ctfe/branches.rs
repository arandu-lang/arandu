//! Static selection before full body resolution. Only the VM decides values;
//! syntax is always retained, and descendants of discarded branches are skipped.

use arandu_middle::{ctfe::ConstValue, DiagCode, Diagnostic, NodeKey, Span, SymbolId};
use arandu_parser::{Stmt, TopLevelDecl};
use rustc_hash::FxHashMap;

use super::{CtfeRoot, CtfeRootRequest, RootSelector};
use crate::{db::HashEq, ArandCompilerDb, SourceFile, StableHash};

#[derive(Debug, Clone, Default)]
pub struct StaticBranches {
    pub decisions: FxHashMap<NodeKey, bool>,
    pub diagnostics: Vec<Diagnostic>,
}

impl StableHash for StaticBranches {
    fn stable_hash(&self) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        h.update(b"StaticBranches/v1");
        let mut decisions: Vec<_> = self.decisions.iter().collect();
        decisions.sort_by_key(|(key, _)| (key.start, key.end));
        for (key, selected) in decisions {
            h.update(&key.start.to_le_bytes());
            h.update(&key.end.to_le_bytes());
            h.update(&[u8::from(*selected)]);
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
) -> HashEq<StaticBranches> {
    HashEq::new(StaticBranches {
        decisions: FxHashMap::default(),
        diagnostics: vec![Diagnostic::error(
            DiagCode::T044ComptimeEvaluationFailed,
            "compile-time branch selection depends on itself",
            Span::new(*file.file_id(db), 0, 0),
        )
        .with_primary_label("cyclic compile-time dependency")],
    })
}

#[salsa::tracked(cycle_result = cycle)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(query = "item_static_branches", file = ?file.file_id(db), owner = ?owner))]
pub fn item_static_branches(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
) -> HashEq<StaticBranches> {
    select_branches(db, file, owner, None)
}

pub(crate) fn select_branches(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    instance: Option<&arandu_middle::types::FunctionInstance>,
) -> HashEq<StaticBranches> {
    select_branches_in_occurrence(db, file, owner, instance, &[])
}

pub(crate) fn select_branches_in_occurrence(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    occurrence: &[(u32, super::FrozenConstant, u64)],
) -> HashEq<StaticBranches> {
    select_branches_in_context(
        db,
        file,
        owner,
        instance,
        occurrence,
        &super::DependencyContext::default(),
    )
}

pub(crate) fn select_branches_in_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    occurrence: &[(u32, super::FrozenConstant, u64)],
    context: &super::DependencyContext,
) -> HashEq<StaticBranches> {
    let parsed = crate::passes::parse(db, file);
    let headers = crate::passes::resolved_headers(db, file);
    let mut result = StaticBranches::default();
    let Ok(program) = &**parsed else {
        return HashEq::new(result);
    };
    let mut body = None;
    program.for_each_decl_recursive(|_, declaration| {
        if let TopLevelDecl::Func(function) = declaration {
            if arandu_semantics::primary_def_key(declaration)
                .and_then(|key| headers.declarations.resolved.definitions.get(&key))
                == Some(&owner)
            {
                body = Some(&function.body);
            }
        }
    });
    let Some(body) = body else {
        return HashEq::new(result);
    };
    let statements = super::roots::static_ifs(&program.pool, body.span);
    if statements.is_empty() {
        return HashEq::new(result);
    }
    if statements.len() > 4096 {
        result.diagnostics.push(
            Diagnostic::error(
                DiagCode::T045ComptimeLimitExceeded,
                "this function contains too many compile-time branches",
                body.span,
            )
            .with_primary_label("at most 4096 branches per function"),
        );
        return HashEq::new(result);
    }
    let mut excluded: Vec<Span> = Vec::new();
    for (ordinal, statement) in statements.iter().enumerate() {
        db.unwind_if_revision_cancelled();
        let span = statement.span();
        let loops = super::roots::static_fors(&program.pool, body.span);
        let enclosing: Vec<_> = loops
            .iter()
            .enumerate()
            .filter_map(|(ordinal, stmt)| {
                let Stmt::For { body, .. } = stmt else {
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
        if excluded
            .iter()
            .any(|outer| outer.start <= span.start && span.end <= outer.end)
        {
            continue;
        }
        let Ok(ordinal) = u32::try_from(ordinal) else {
            continue;
        };
        let root = CtfeRoot::new_in_context(
            db,
            file,
            owner,
            super::roots::in_occurrence(
                instance.map_or(RootSelector::StaticIfCondition(ordinal), |instance| {
                    RootSelector::InInstance {
                        instance: instance.clone(),
                        selector: Box::new(RootSelector::StaticIfCondition(ordinal)),
                    }
                }),
                occurrence,
            ),
            None,
            context.clone(),
        );
        let budget = super::public::staging_budget(
            super::config::public_budget(db),
            statements.len(),
            super::public::iteration_count(occurrence),
        );
        let value = super::ctfe_eval_root(db, CtfeRootRequest::new(db, root, budget));
        let Stmt::If {
            then_block,
            else_block,
            ..
        } = statement
        else {
            continue;
        };
        match value {
            Ok(ConstValue::Bool(selected)) => {
                result.decisions.insert(span.into(), *selected);
                if *selected {
                    if let Some(block) = else_block {
                        excluded.push(block.span);
                    }
                } else {
                    excluded.push(then_block.span);
                }
            }
            Ok(_) => {
                result.diagnostics.push(Diagnostic::error(
                    DiagCode::T044ComptimeEvaluationFailed,
                    "compile-time condition did not produce bool",
                    span,
                ));
                excluded.push(span);
            }
            Err(error) => {
                // Reuse the public CTFE diagnostic mapper, including labels,
                // resource limits, captures and call-site traces.
                super::public::append_failure(&mut result.diagnostics, error.clone(), span);
                excluded.push(span);
            }
        }
    }
    HashEq::new(result)
}
