//! Freeze computed generic arguments before the owner's ordinary body typing.
//! The pure checker consumes values; it never calls the VM or runtime lowering.

use arandu_middle::{ctfe::ConstValue, DiagCode, Diagnostic, NodeKey, Span, SymbolId};
use arandu_parser::{Stmt, TopLevelDecl};
use rustc_hash::FxHashMap;

use super::{CtfeRoot, CtfeRootRequest, RootSelector};
use crate::{db::HashEq, ArandCompilerDb, SourceFile, StableHash};

#[derive(Debug, Clone, Default)]
pub struct ConstArguments {
    pub values: FxHashMap<NodeKey, Option<u64>>,
    pub typed_values: FxHashMap<NodeKey, ConstValue>,
    pub diagnostics: Vec<Diagnostic>,
}

impl StableHash for ConstArguments {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hash = blake3::Hasher::new();
        hash.update(b"ConstArguments/v1");
        let mut values: Vec<_> = self.values.iter().collect();
        values.sort_by_key(|(key, _)| (key.start, key.end));
        for (key, value) in values {
            hash.update(&key.start.to_le_bytes());
            hash.update(&key.end.to_le_bytes());
            hash.update(&[u8::from(value.is_some())]);
            if let Some(value) = value {
                hash.update(&value.to_le_bytes());
            }
        }
        let mut typed: Vec<_> = self.typed_values.iter().collect();
        typed.sort_by_key(|(key, _)| (key.start, key.end));
        for (key, value) in typed {
            hash.update(&key.start.to_le_bytes());
            hash.update(&key.end.to_le_bytes());
            hash.update(&value.canonical_bytes());
        }
        hash.update(self.diagnostics.stable_hash().as_bytes());
        hash.finalize()
    }
}

fn cycle(
    db: &dyn ArandCompilerDb,
    _id: salsa::Id,
    file: SourceFile,
    _owner: SymbolId,
) -> HashEq<ConstArguments> {
    HashEq::new(ConstArguments {
        values: FxHashMap::default(),
        typed_values: FxHashMap::default(),
        diagnostics: vec![Diagnostic::error(
            DiagCode::T044ComptimeEvaluationFailed,
            "compile-time generic argument depends on itself",
            Span::new(*file.file_id(db), 0, 0),
        )
        .with_primary_label("cyclic compile-time dependency")],
    })
}

fn contains(outer: Span, inner: Span) -> bool {
    outer.file_id == inner.file_id && outer.start <= inner.start && inner.end <= outer.end
}

#[salsa::tracked(cycle_result = cycle)]
#[tracing::instrument(level = "trace", target = "arandu_query", skip(db), fields(query = "item_const_arguments", file = ?file.file_id(db), owner = ?owner))]
pub fn item_const_arguments(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
) -> HashEq<ConstArguments> {
    let branches = super::item_static_branches(db, file, owner);
    select_arguments(db, file, owner, None, branches)
}

pub(crate) fn select_arguments(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    branches: &super::StaticBranches,
) -> HashEq<ConstArguments> {
    select_arguments_in_occurrence(db, file, owner, instance, branches, &[])
}

pub(crate) fn select_arguments_in_occurrence(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    branches: &super::StaticBranches,
    occurrence: &[(u32, super::FrozenConstant, u64)],
) -> HashEq<ConstArguments> {
    select_arguments_in_context(
        db,
        file,
        owner,
        instance,
        branches,
        occurrence,
        &super::DependencyContext::default(),
    )
}

pub(crate) fn select_arguments_in_context(
    db: &dyn ArandCompilerDb,
    file: SourceFile,
    owner: SymbolId,
    instance: Option<&arandu_middle::types::FunctionInstance>,
    branches: &super::StaticBranches,
    occurrence: &[(u32, super::FrozenConstant, u64)],
    context: &super::DependencyContext,
) -> HashEq<ConstArguments> {
    let source = crate::passes::item_source_input(db, file, owner);
    if !source.may_have_comptime {
        return HashEq::new(ConstArguments::default());
    }
    let mut body = None;
    source.program.for_each_decl_recursive(|_, declaration| {
        if let TopLevelDecl::Func(function) = declaration {
            if function.span.start == source.item_start {
                body = Some(function.body.span);
            }
        }
    });
    let mut result = ConstArguments::default();
    let Some(body_span) = body else {
        return HashEq::new(result);
    };
    // No body memo or header cloning for ordinary functions. A current AST is
    // needed only when staging roots exist (arena IDs are revision-local).
    if super::roots::const_arguments(&source.program.pool, body_span).is_empty() {
        return HashEq::new(result);
    }
    let parsed = crate::passes::parse(db, file);
    let Ok(program) = &**parsed else {
        return HashEq::new(result);
    };
    let roots = super::roots::const_arguments(&program.pool, body_span);
    if roots.len() > 4096 {
        // One resource diagnostic, not thousands of misleading "unstaged"
        // messages from pure consumers of this failed staging obligation.
        result
            .values
            .extend(roots.iter().map(|(span, _)| ((*span).into(), None)));
        result.diagnostics.push(
            Diagnostic::error(
                DiagCode::T045ComptimeLimitExceeded,
                "this function contains too many computed generic arguments",
                body_span,
            )
            .with_primary_label("at most 4096 arguments per function"),
        );
        return HashEq::new(result);
    }
    let excluded: Vec<_> = super::roots::static_ifs(&program.pool, body_span)
        .into_iter()
        .filter_map(|stmt| {
            let Stmt::If {
                then_block,
                else_block,
                ..
            } = stmt
            else {
                return None;
            };
            match branches.decisions.get(&stmt.span().into()) {
                Some(true) => else_block.as_ref().map(|b| b.span),
                Some(false) => Some(then_block.span),
                None => Some(stmt.span()),
            }
        })
        .collect();
    for (ordinal, &(span, _)) in roots.iter().enumerate() {
        db.unwind_if_revision_cancelled();
        let enclosing: Vec<_> = super::roots::static_fors(&program.pool, body_span)
            .iter()
            .enumerate()
            .filter_map(|(ordinal, stmt)| {
                let Stmt::For { body, .. } = stmt else {
                    return None;
                };
                contains(body.span, span).then_some(ordinal)
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
        if excluded.iter().any(|&outer| contains(outer, span)) {
            continue;
        }
        result.values.insert(span.into(), None);
        let Ok(ordinal) = u32::try_from(ordinal) else {
            continue;
        };
        let selector = instance.map_or(RootSelector::ConstArgument(ordinal), |instance| {
            RootSelector::InInstance {
                instance: instance.clone(),
                selector: Box::new(RootSelector::ConstArgument(ordinal)),
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
        let budget = super::public::staging_budget(
            super::config::public_budget(db),
            roots.len(),
            super::public::iteration_count(occurrence),
        );
        match super::ctfe_eval_root(db, CtfeRootRequest::new(db, root, budget)) {
            Ok(value @ ConstValue::Integer(integer)) => {
                if let Ok(number) = integer.to_const_generic() {
                    result.values.insert(span.into(), Some(number));
                } else {
                    result.typed_values.insert(span.into(), value.clone());
                }
            }
            Ok(value @ (ConstValue::Bool(_) | ConstValue::Aggregate(_))) => {
                result.typed_values.insert(span.into(), value.clone());
            }
            Ok(_) => result.diagnostics.push(Diagnostic::error(
                DiagCode::T003IncompatibleCallArg,
                "constant generic arguments require an integer, bool, or closed Copy aggregate",
                span,
            )),
            Err(error) => {
                super::public::append_failure(&mut result.diagnostics, error.clone(), span)
            }
        }
    }
    HashEq::new(result)
}
