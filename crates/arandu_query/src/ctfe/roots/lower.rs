use std::sync::Arc;

use arandu_middle::Span;
use arandu_parser::TopLevelDecl;

use super::super::{BuildFailure, CtfeLowering};
use super::{CtfeRoot, RootExpectedType, RootSelector};
use crate::db::HashEq;
use crate::ArandCompilerDb;

#[salsa::tracked]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db, root),
    fields(query = "ctfe_root_amir")
)]
pub fn ctfe_root_amir<'db>(
    db: &'db dyn ArandCompilerDb,
    root: CtfeRoot<'db>,
) -> HashEq<CtfeLowering> {
    let file = *root.file(db);
    let owner = *root.owner(db);
    let layout = *db.target_config().data_layout(db);
    let selector = super::select::source_selector(root.selector(db));
    let mut wrapper = root.selector(db);
    let mut instance = None;
    let mut iterations = Vec::new();
    for _ in 0..64 {
        match wrapper {
            RootSelector::InInstance {
                instance: key,
                selector,
            } => {
                instance = Some(key);
                wrapper = selector;
            }
            RootSelector::InIteration {
                ordinal,
                value,
                domain_size,
                selector,
            } => {
                iterations.push((*ordinal, value.clone(), *domain_size));
                wrapper = selector;
            }
            _ => break,
        }
    }
    let build = || {
        if iterations.len() > super::super::MAX_QUERY_DEPENDENCY_DEPTH {
            return Err(BuildFailure::Diagnostics(vec![
                arandu_middle::Diagnostic::error(
                    arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                    "static iteration staging exceeds its nesting limit",
                    Span::new(*file.file_id(db), 0, 0),
                ),
            ]));
        }
        if owner.file_id != *file.file_id(db) {
            return Err(BuildFailure::MissingFunction);
        }
        if instance.is_some_and(|instance| instance.definition != owner)
            || matches!(selector, RootSelector::InInstance { .. })
        {
            return Err(BuildFailure::InvalidRoot);
        }
        layout.validate().map_err(|error| {
            BuildFailure::Evaluation(arandu_mir::ctfe::EvalErrorKind::InvalidLayout(error))
        })?;
        if let Some(RootExpectedType::Primitive(primitive)) = root.expected(db).as_ref() {
            if primitive.is_integer() {
                arandu_middle::ctfe::IntegerType::new(*primitive, layout).map_err(|error| {
                    BuildFailure::Evaluation(arandu_mir::ctfe::EvalErrorKind::Value(error))
                })?;
            } else if !primitive.is_float()
                && !matches!(
                    primitive,
                    arandu_middle::types::Primitive::Bool | arandu_middle::types::Primitive::Str
                )
            {
                return Err(BuildFailure::Evaluation(
                    arandu_mir::ctfe::EvalErrorKind::Value(
                        arandu_middle::ctfe::ConstValueError::UnsupportedIntegerType(*primitive),
                    ),
                ));
            }
        }
        let pre_body = instance.is_some()
            || !iterations.is_empty()
            || matches!(
                selector,
                RootSelector::IfCondition { .. }
                    | RootSelector::StaticIfCondition(_)
                    | RootSelector::StaticForBound { .. }
                    | RootSelector::StaticForDomain(_)
                    | RootSelector::ConstArgument(_)
                    | RootSelector::GlobalInitializer
                    | RootSelector::HeaderArgument(_)
            );
        let source = crate::passes::item_source_input(db, file, owner);
        let parsed = pre_body.then(|| crate::passes::parse(db, file));
        // Header continuation and expression IDs must come from one canonical
        // AST revision. The content-addressed item memo may retain an older
        // arena after a sibling edit; do not pair it with current header state.
        let program = if let Some(parsed) = parsed {
            match &**parsed {
                Ok(program) => program.as_ref(),
                Err(_) => return Err(BuildFailure::InvalidRoot),
            }
        } else {
            source.program.as_ref()
        };
        let condition = matches!(
            selector,
            RootSelector::IfCondition { .. } | RootSelector::StaticIfCondition(_)
        );
        if condition
            && root.expected(db).as_ref().is_some_and(|expected| {
                *expected != RootExpectedType::Primitive(arandu_middle::types::Primitive::Bool)
            })
        {
            return Err(BuildFailure::InvalidRoot);
        }
        let declared = if pre_body {
            crate::passes::seed_header_signatures(db, file)
        } else {
            crate::passes::declaration_signatures(db, file)
        };
        if declared.type_info.generic_params.contains_key(&owner)
            && instance.is_none()
            && !matches!(
                selector,
                RootSelector::ConstArgument(_)
                    | RootSelector::StaticIfCondition(_)
                    | RootSelector::HeaderArgument(_)
            )
        {
            return Err(BuildFailure::GenericFunction);
        }
        let mut body = None;
        let mut initializer = None;
        program.for_each_decl_recursive(|_, declaration| {
            if arandu_semantics::primary_def_key(declaration)
                .and_then(|key| declared.resolved.definitions.get(&key))
                == Some(&owner)
            {
                match declaration {
                    TopLevelDecl::Func(function) => body = Some(&function.body),
                    TopLevelDecl::Const(constant) => initializer = Some(constant.value),
                    _ => {}
                }
            }
        });
        let selected = if let RootSelector::HeaderArgument(ordinal) = selector {
            let roots = super::super::headers::header_arguments(program, &declared.resolved, owner);
            let (_, expression) = roots
                .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
                .ok_or(BuildFailure::InvalidRoot)?;
            super::select::Selected::Expression(*expression)
        } else if matches!(selector, RootSelector::GlobalInitializer) {
            super::select::Selected::Expression(initializer.ok_or(BuildFailure::InvalidRoot)?)
        } else {
            super::select::select(
                program,
                body.ok_or(BuildFailure::MissingFunction)?,
                selector,
            )?
        };
        let span = selected.span(program);
        let mut initial = if matches!(selector, RootSelector::GlobalInitializer) {
            super::super::headers::owner_signatures_in_context(db, file, owner, root.dependency(db))
        } else {
            (**declared).clone()
        };
        if pre_body
            && !matches!(
                selector,
                RootSelector::GlobalInitializer | RootSelector::HeaderArgument(_)
            )
        {
            let mut headers = (**crate::passes::resolved_headers(db, file)).clone();
            headers.declarations.symbols = Arc::clone(&declared.symbols);
            headers.declarations.resolved = Arc::clone(&declared.resolved);
            let selection = if let RootSelector::PublicComptime(ordinal) = selector {
                let expression = *super::select::public_roots(
                    &program.pool,
                    body.ok_or(BuildFailure::MissingFunction)?.span,
                )
                .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
                .ok_or(BuildFailure::InvalidRoot)?;
                let statement = super::select::argument_statement(
                    program,
                    body.ok_or(BuildFailure::MissingFunction)?,
                    program.pool.expr_span(expression),
                )
                .ok_or(BuildFailure::InvalidRoot)?;
                match program.pool.expr(expression) {
                    arandu_parser::ExprKind::Comptime {
                        body: arandu_parser::ast_pool::ComptimeBody::Block(block),
                    } => arandu_resolve::BodySelection::LexicalBlock {
                        owner,
                        block: *block,
                        statement,
                    },
                    arandu_parser::ExprKind::Comptime {
                        body: arandu_parser::ast_pool::ComptimeBody::Expression(expression),
                    } => arandu_resolve::BodySelection::LexicalExpression {
                        owner,
                        expression: *expression,
                        statement,
                    },
                    _ => return Err(BuildFailure::InvalidRoot),
                }
            } else if matches!(selected, super::select::Selected::Block(_)) {
                arandu_resolve::BodySelection::Function(owner)
            } else {
                let super::select::Selected::Expression(expression) = selected else {
                    return Err(BuildFailure::InvalidRoot);
                };
                if let RootSelector::StaticForBound { ordinal, .. }
                | RootSelector::StaticForDomain(ordinal) = selector
                {
                    let statements = super::select::static_fors(
                        &program.pool,
                        body.ok_or(BuildFailure::MissingFunction)?.span,
                    );
                    let statement = statements
                        .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
                        .ok_or(BuildFailure::InvalidRoot)?;
                    arandu_resolve::BodySelection::LexicalExpression {
                        owner,
                        expression,
                        statement: statement.span(),
                    }
                } else if let RootSelector::StaticIfCondition(ordinal) = selector {
                    let statements = super::select::static_ifs(
                        &program.pool,
                        body.ok_or(BuildFailure::MissingFunction)?.span,
                    );
                    let stmt = statements
                        .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
                        .ok_or(BuildFailure::InvalidRoot)?;
                    arandu_resolve::BodySelection::LexicalExpression {
                        owner,
                        expression,
                        statement: stmt.span(),
                    }
                } else if let RootSelector::ConstArgument(ordinal) = selector {
                    let body = body.ok_or(BuildFailure::MissingFunction)?;
                    let arguments = super::select::const_arguments(&program.pool, body.span);
                    let (argument, _) = arguments
                        .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
                        .ok_or(BuildFailure::InvalidRoot)?;
                    let statement = super::select::argument_statement(program, body, *argument)
                        .ok_or_else(|| {
                            BuildFailure::Diagnostics(vec![arandu_middle::Diagnostic::error(
                                arandu_middle::DiagCode::T042UnsupportedComptime,
                                "computed generic argument has no enclosing lexical statement",
                                *argument,
                            )
                            .with_primary_label("unsupported argument staging context")])
                        })?;
                    arandu_resolve::BodySelection::LexicalExpression {
                        owner,
                        expression,
                        statement,
                    }
                } else {
                    arandu_resolve::BodySelection::Expression { owner, expression }
                }
            };
            let resolved = arandu_resolve::resolve_selected_body_with_poll(
                program,
                headers,
                selection,
                || db.unwind_if_revision_cancelled(),
            );
            initial.symbols = resolved.symbols;
            initial.resolved = resolved.resolved;
            initial.diagnostics = resolved.diagnostics;
            if matches!(
                selector,
                RootSelector::StaticIfCondition(_)
                    | RootSelector::StaticForBound { .. }
                    | RootSelector::StaticForDomain(_)
                    | RootSelector::ConstArgument(_)
            ) && initial
                .diagnostics
                .iter()
                .any(|d| d.code == arandu_middle::DiagCode::T042UnsupportedComptime)
            {
                return Err(BuildFailure::Diagnostics(initial.diagnostics));
            }
        }
        super::lower_staged::lower_staged_root(
            db,
            root,
            file,
            owner,
            layout,
            selector,
            instance,
            &iterations,
            program,
            body,
            selected,
            span,
            initial,
            condition,
        )
    };
    HashEq::new(CtfeLowering { result: build() })
}
