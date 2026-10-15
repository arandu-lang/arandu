//! Concrete staging before body resolution and typing. Instance keys own all
//! selected branches, frozen arguments and public root values.

use crate::{db::HashEq, passes::PreparedHir, runtime::Instance, ArandCompilerDb, StableHash};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct StagedInstance {
    pub artifacts: Arc<PreparedHir>,
    pub occurrence_symbols: Vec<arandu_middle::SymbolId>,
}
impl StableHash for StagedInstance {
    fn stable_hash(&self) -> blake3::Hash {
        let mut hash = blake3::Hasher::new();
        hash.update(self.artifacts.stable_hash().as_bytes());
        for symbol in &self.occurrence_symbols {
            hash.update(&symbol.file_id.to_le_bytes());
            hash.update(&symbol.local_id.0.to_le_bytes());
        }
        hash.finalize()
    }
}

pub(crate) fn instance_staged_hir<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
) -> HashEq<PreparedHir> {
    HashEq::from_arc(Arc::clone(
        &instance_staged_result(db, instance, false, super::DependencyContext::default()).artifacts,
    ))
}

pub(crate) fn instance_staged_symbols<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
) -> Vec<arandu_middle::SymbolId> {
    instance_staged_result(db, instance, false, super::DependencyContext::default())
        .occurrence_symbols
        .clone()
}

pub(crate) fn instance_ctfe_symbols<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
    context: &super::DependencyContext,
) -> Vec<arandu_middle::SymbolId> {
    instance_staged_result(db, instance, true, context.clone())
        .occurrence_symbols
        .clone()
}

pub(crate) fn instance_ctfe_hir<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
    context: &super::DependencyContext,
) -> HashEq<PreparedHir> {
    HashEq::from_arc(Arc::clone(
        &instance_staged_result(db, instance, true, context.clone()).artifacts,
    ))
}

#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db, instance),
    fields(query = "instance_staged_hir")
)]
fn instance_staged_result<'db>(
    db: &'db dyn ArandCompilerDb,
    instance: Instance<'db>,
    for_ctfe: bool,
    context: super::DependencyContext,
) -> HashEq<StagedInstance> {
    let file = *instance.file(db);
    let key = instance.key(db);
    let parsed = crate::passes::parse(db, file);
    let declared = if for_ctfe {
        crate::passes::seed_header_signatures(db, file)
    } else {
        crate::passes::header_signatures(db, file)
    };
    let mut checked = (**declared).clone();
    let mut diagnostics = Vec::new();
    if let Err(errors) = super::contracts::prepare_owner(db, file, key, &mut checked, &context) {
        diagnostics.extend(errors);
    }
    let mut hir = None;
    let mut occurrence_symbols = Vec::new();
    if let Ok(program) = &**parsed {
        let concrete = (!key.arguments.is_empty()).then_some(key);
        let branches = super::branches::select_branches_in_context(
            db,
            file,
            key.definition,
            concrete,
            &[],
            &context,
        );
        let loops = super::loops::select_loops_in_context(
            db,
            file,
            key.definition,
            concrete,
            &branches,
            &[],
            &context,
        );
        let mut expansion = 0_u64;
        for (node, (lower, upper)) in &loops.domains {
            let mut occurrences = u64::try_from(
                upper
                    .value()
                    .checked_sub(lower.value())
                    .unwrap_or(i128::MAX)
                    .max(0),
            )
            .unwrap_or(u64::MAX);
            for (outer, (start, end)) in &loops.domains {
                if outer != node && outer.start <= node.start && node.end <= outer.end {
                    let size = u64::try_from(
                        end.value()
                            .checked_sub(start.value())
                            .unwrap_or(i128::MAX)
                            .max(0),
                    )
                    .unwrap_or(u64::MAX);
                    occurrences = occurrences.saturating_mul(size);
                }
            }
            expansion = expansion.saturating_add(occurrences);
        }
        if expansion > 4096 {
            checked.diagnostics.push(arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                "static loop expansion exceeds 4096 occurrences",
                program.span,
            ));
            return HashEq::new(StagedInstance {
                artifacts: Arc::new(PreparedHir {
                    hir: None,
                    diagnostics: checked.diagnostics.clone(),
                    type_check: checked,
                    source_fingerprint: key.stable_hash(),
                }),
                occurrence_symbols,
            });
        }
        let arguments = super::arguments::select_arguments_in_context(
            db,
            file,
            key.definition,
            concrete,
            &branches,
            &[],
            &context,
        );
        let mut headers = (**crate::passes::resolved_headers(db, file)).clone();
        headers.declarations.symbols = Arc::clone(&checked.symbols);
        headers.declarations.resolved = Arc::clone(&checked.resolved);
        let resolved = Arc::make_mut(&mut headers.declarations.resolved);
        resolved
            .comptime_branches
            .extend(branches.decisions.iter().map(|(&key, &value)| (key, value)));
        resolved
            .comptime_loops
            .extend(loops.domains.iter().map(|(&key, &value)| (key, value)));
        resolved.typed_comptime_arguments.extend(
            arguments
                .typed_values
                .iter()
                .map(|(&key, value)| (key, value.clone())),
        );
        resolved
            .comptime_arguments
            .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
        resolved
            .deferred_loop_bodies
            .extend(loops.domains.keys().copied().filter(|node| {
                program.pool.stmts.iter().any(|statement| match statement {
                    arandu_parser::Stmt::For { span, body, .. }
                        if arandu_middle::NodeKey::from(*span) == *node =>
                    {
                        loops.empty_domain(node)
                            || super::roots::loop_requires_occurrences(&program.pool, body)
                    }
                    _ => false,
                }) && !program.pool.exprs.iter().zip(&program.pool.expr_spans).any(
                    |(expression, span)| {
                        matches!(expression, arandu_parser::ExprKind::Comptime { .. })
                            && span.start <= node.start
                            && node.end <= span.end
                    },
                )
            }));
        let resolved = arandu_resolve::resolve_selected_body_with_poll(
            program,
            headers,
            arandu_resolve::BodySelection::Function(key.definition),
            || db.unwind_if_revision_cancelled(),
        );
        checked.symbols = resolved.symbols;
        checked.resolved = resolved.resolved;
        checked.diagnostics = resolved.diagnostics;
        if for_ctfe {
            let mut span = None;
            program.for_each_decl_recursive(|_, declaration| {
                if arandu_semantics::primary_def_key(declaration)
                    .and_then(|node| checked.resolved.definitions.get(&node))
                    == Some(&key.definition)
                {
                    span = Some(arandu_semantics::item_source_span(declaration));
                }
            });
            if let Some(span) = span {
                if let Err(error) = super::globals::install_referenced_globals_in_context(
                    db,
                    program,
                    span,
                    &mut checked,
                    &context,
                ) {
                    super::public::append_failure(
                        &mut checked.diagnostics,
                        super::RootEvalError::Build(error),
                        span,
                    );
                }
            }
        }
        let substitution = if key.arguments.is_empty() {
            Ok(arandu_middle::types::GenericSubst::new())
        } else {
            super::roots::instance_substitution(&checked, key)
        };
        match substitution {
            Ok(substitution) => {
                let prerequisite_diagnostics = std::mem::take(&mut checked.diagnostics);
                checked = super::contracts::check_body(
                    db,
                    &checked,
                    program,
                    key.definition,
                    &substitution,
                    &context,
                );
                checked.diagnostics.extend(prerequisite_diagnostics);
                checked
                    .diagnostics
                    .extend(branches.diagnostics.iter().cloned());
                checked
                    .diagnostics
                    .extend(loops.diagnostics.iter().cloned());
                checked
                    .diagnostics
                    .extend(arguments.diagnostics.iter().cloned());
                super::loops::install_array_values(
                    &mut checked,
                    program,
                    &loops,
                    *db.target_config().data_layout(db),
                );
                if !for_ctfe {
                    checked = (*super::public::stage_public_roots(
                        db,
                        file,
                        key.definition,
                        &HashEq::new(checked),
                        concrete,
                    ))
                    .clone();
                }
                let lowering = if for_ctfe {
                    arandu_semantics::lower_ctfe_function_to_hir
                } else {
                    arandu_semantics::lower_function_to_hir
                };
                match lowering(&mut checked, program, key.definition) {
                    Ok(mut lowered) => {
                        if let Some(hir) = &mut lowered {
                            let source_symbols: rustc_hash::FxHashSet<_> =
                                checked.symbols.iter().map(|symbol| symbol.id).collect();
                            let mut count = 0;
                            install_occurrences(
                                db,
                                file,
                                key,
                                program,
                                &mut checked,
                                hir,
                                &substitution,
                                &[],
                                &mut count,
                                &mut diagnostics,
                                &context,
                            );
                            occurrence_symbols.extend(
                                checked
                                    .symbols
                                    .iter()
                                    .filter(|symbol| !source_symbols.contains(&symbol.id))
                                    .map(|symbol| symbol.id),
                            );
                        }
                        hir = lowered;
                    }
                    Err(errors) => diagnostics.extend(errors),
                }
            }
            Err(error) => super::public::append_failure(
                &mut diagnostics,
                super::RootEvalError::Build(error),
                program.span,
            ),
        }
    }
    diagnostics.extend(checked.diagnostics.iter().cloned());
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == arandu_middle::Severity::Error)
    {
        hir = None;
    }
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"InstanceStagedHir/v1");
    fingerprint.update(key.stable_hash().as_bytes());
    fingerprint.update(
        crate::passes::item_source_input(db, file, key.definition)
            .stable_hash()
            .as_bytes(),
    );
    HashEq::new(StagedInstance {
        artifacts: Arc::new(PreparedHir {
            hir,
            type_check: checked,
            diagnostics,
            source_fingerprint: fingerprint.finalize(),
        }),
        occurrence_symbols,
    })
}

#[allow(clippy::too_many_arguments)]
fn install_occurrences(
    db: &dyn ArandCompilerDb,
    file: crate::SourceFile,
    key: &arandu_middle::types::FunctionInstance,
    program: &arandu_parser::Program,
    checked: &mut arandu_typeck::TypeCheckResult,
    hir: &mut arandu_middle::hir::HirProgram,
    substitution: &arandu_middle::types::GenericSubst,
    occurrence: &[(u32, super::FrozenConstant, u64)],
    count: &mut u64,
    diagnostics: &mut Vec<arandu_middle::Diagnostic>,
    context: &super::DependencyContext,
) {
    use arandu_middle::hir::HirStmtKind;
    use arandu_parser::{ForClause, Stmt};
    if occurrence.len() >= super::MAX_QUERY_DEPENDENCY_DEPTH {
        diagnostics.push(
            arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                "static iteration staging exceeds its nesting limit",
                hir.span,
            )
            .with_primary_label("at most 16 nested static iteration contexts"),
        );
        return;
    }
    let mut owner_span = None;
    program.for_each_decl_recursive(|_, decl| {
        if arandu_semantics::primary_def_key(decl)
            .and_then(|node| checked.resolved.definitions.get(&node))
            == Some(&key.definition)
        {
            if let arandu_parser::TopLevelDecl::Func(function) = decl {
                owner_span = Some(function.body.span);
            }
        }
    });
    let Some(owner_span) = owner_span else {
        return;
    };
    let source_loops = super::roots::static_fors(&program.pool, owner_span);
    let pending: Vec<_> = hir
        .pool
        .stmts
        .iter()
        .enumerate()
        .filter_map(|(id, stmt)| {
            if let HirStmtKind::For {
                comptime_bounds: Some(bounds),
                comptime_bodies: None,
                ..
            } = &stmt.kind
            {
                if !checked
                    .resolved
                    .deferred_loop_bodies
                    .contains(&stmt.span.into())
                {
                    return None;
                }
                source_loops
                    .iter()
                    .position(|source| source.span() == stmt.span)
                    .map(|ordinal| (id, ordinal, *bounds))
            } else {
                None
            }
        })
        .collect();
    for (hir_statement, ordinal, (lower, upper)) in pending {
        let Stmt::For {
            span,
            body,
            clause: ForClause::In {
                bindings, iterable, ..
            },
            ..
        } = source_loops[ordinal]
        else {
            continue;
        };
        let enclosing = source_loops
            .iter()
            .filter(|source| match source {
                Stmt::For { body: outer, .. } => {
                    outer.span.start <= span.start && span.end <= outer.span.end
                }
                _ => false,
            })
            .count();
        if enclosing != occurrence.len() {
            continue;
        }
        let Some(binding) = bindings.first() else {
            continue;
        };
        let Ok(ordinal) = u32::try_from(ordinal) else {
            continue;
        };
        let total = upper
            .value()
            .checked_sub(lower.value())
            .unwrap_or(i128::MAX)
            .max(0);
        let Ok(total) = u64::try_from(total) else {
            continue;
        };
        if count.saturating_add(total) > 4096 {
            diagnostics.push(arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                "static loop expansion exceeds 4096 occurrences",
                *span,
            ));
            return;
        }
        *count += total;
        let mut bodies = Vec::new();
        for offset in 0..total {
            db.unwind_if_revision_cancelled();
            let value = if let Some((arandu_middle::ctfe::ConstValue::Aggregate(array), _)) =
                checked
                    .type_info
                    .ctfe_values
                    .get(&program.pool.expr_span(*iterable))
            {
                let arandu_middle::types::TypeShape::Array(_, element) = array.shape() else {
                    continue;
                };
                let Some(value) = usize::try_from(offset)
                    .ok()
                    .and_then(|index| array.values().get(index))
                else {
                    continue;
                };
                super::FrozenConstant {
                    value: value.clone(),
                    shape: (**element).clone(),
                }
            } else {
                let Ok(integer) = arandu_middle::ctfe::ConstInt::new(
                    lower.ty(),
                    lower.value() + i128::from(offset),
                ) else {
                    continue;
                };
                super::FrozenConstant {
                    value: arandu_middle::ctfe::ConstValue::Integer(integer),
                    shape: arandu_middle::types::TypeShape::Primitive(integer.ty().primitive()),
                }
            };
            let Ok(value_ty) = value.shape.intern(&checked.type_info.type_interner) else {
                continue;
            };
            let mut selected = occurrence.to_vec();
            selected.push((ordinal, value, total));
            let branches = super::branches::select_branches_in_context(
                db,
                file,
                key.definition,
                (!key.arguments.is_empty()).then_some(key),
                &selected,
                context,
            );
            let mut all_branches = super::StaticBranches {
                decisions: checked.resolved.comptime_branches.clone(),
                diagnostics: branches.diagnostics.clone(),
            };
            all_branches.decisions.extend(
                branches
                    .decisions
                    .iter()
                    .map(|(&node, &value)| (node, value)),
            );
            let loops = super::loops::select_loops_in_context(
                db,
                file,
                key.definition,
                (!key.arguments.is_empty()).then_some(key),
                &all_branches,
                &selected,
                context,
            );
            let arguments = super::arguments::select_arguments_in_context(
                db,
                file,
                key.definition,
                (!key.arguments.is_empty()).then_some(key),
                &all_branches,
                &selected,
                context,
            );
            let mut headers = (**crate::passes::resolved_headers(db, file)).clone();
            headers.declarations.symbols = Arc::clone(&checked.symbols);
            headers.declarations.resolved = Arc::clone(&checked.resolved);
            let names = Arc::make_mut(&mut headers.declarations.resolved);
            names.comptime_branches.extend(
                branches
                    .decisions
                    .iter()
                    .map(|(&node, &value)| (node, value)),
            );
            names.typed_comptime_arguments.extend(
                arguments
                    .typed_values
                    .iter()
                    .map(|(&key, value)| (key, value.clone())),
            );
            names
                .comptime_arguments
                .extend(arguments.values.iter().map(|(&node, &value)| (node, value)));
            names
                .comptime_loops
                .extend(loops.domains.iter().map(|(&node, &value)| (node, value)));
            names
                .deferred_loop_bodies
                .extend(loops.domains.keys().copied().filter(|node| {
                    program.pool.stmts.iter().any(|statement| match statement {
                        Stmt::For { span, body, .. }
                            if arandu_middle::NodeKey::from(*span) == *node =>
                        {
                            loops.empty_domain(node)
                                || super::roots::loop_requires_occurrences(&program.pool, body)
                        }
                        _ => false,
                    })
                }));
            let resolved = arandu_resolve::resolve_selected_body_with_poll(
                program,
                headers,
                arandu_resolve::BodySelection::LexicalLoopBody {
                    owner: key.definition,
                    statement: *span,
                },
                || db.unwind_if_revision_cancelled(),
            );
            let mut initial = checked.clone();
            for (node, symbol) in &resolved.resolved.definitions {
                if let Some(old) = checked
                    .resolved
                    .definitions
                    .get(node)
                    .and_then(|old| checked.type_info.decl_type_id(*old))
                {
                    initial.type_info_mut().decl_types.insert(*symbol, old);
                }
            }
            initial.symbols = resolved.symbols;
            initial.resolved = resolved.resolved;
            initial.diagnostics = resolved.diagnostics;
            let Some(&binding_symbol) = initial.resolved.definitions.get(&binding.span.into())
            else {
                continue;
            };
            let mut typed = arandu_typeck::type_checker::check::check_residual_loop_body(
                &initial,
                program,
                key.definition,
                body,
                (binding_symbol, value_ty),
                crate::passes::database_target_info(db),
                substitution,
            );
            for _ in 0..super::MAX_QUERY_DEPENDENCY_DEPTH {
                match super::contracts::freeze_requests(db, &mut typed, context) {
                    Ok(0) => break,
                    Ok(_) => {
                        typed = arandu_typeck::type_checker::check::check_residual_loop_body(
                            &typed,
                            program,
                            key.definition,
                            body,
                            (binding_symbol, value_ty),
                            crate::passes::database_target_info(db),
                            substitution,
                        );
                    }
                    Err(errors) => {
                        typed.diagnostics.extend(errors);
                        break;
                    }
                }
            }
            if !typed.type_info.header_requests.is_empty() {
                typed.diagnostics.push(arandu_middle::Diagnostic::error(
                    arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                    "concrete header discovery exceeds its continuation limit",
                    body.span,
                ));
            }
            typed
                .diagnostics
                .extend(all_branches.diagnostics.iter().cloned());
            typed
                .diagnostics
                .extend(arguments.diagnostics.iter().cloned());
            typed.diagnostics.extend(loops.diagnostics.iter().cloned());
            typed = (*super::public::stage_public_roots_in_context(
                db,
                file,
                key.definition,
                &HashEq::new(typed),
                (!key.arguments.is_empty()).then_some(key),
                &selected,
                context,
            ))
            .clone();
            super::loops::install_array_values(
                &mut typed,
                program,
                &loops,
                *db.target_config().data_layout(db),
            );
            match arandu_semantics::lower_block_to_hir(&mut typed, &program.pool, hir, body) {
                Ok(block) => {
                    *checked = typed;
                    install_occurrences(
                        db,
                        file,
                        key,
                        program,
                        checked,
                        hir,
                        substitution,
                        &selected,
                        count,
                        diagnostics,
                        context,
                    );
                    bodies.push(block);
                }
                Err(errors) => diagnostics.extend(errors),
            }
        }
        if let Some(statement) = hir.pool.stmts.iter_mut().nth(hir_statement) {
            if let HirStmtKind::For {
                comptime_bodies, ..
            } = &mut statement.kind
            {
                *comptime_bodies = Some(bodies);
            }
        }
    }
}
