use arandu_middle::{types::ArType, Span};
use arandu_parser::{ast_pool::ExprId, Block, DeferBody, Program, Stmt};

use super::super::BuildFailure;
use super::{BlockBranch, RootSelector};

pub(super) enum Selected<'a> {
    Block(&'a Block),
    Expression(ExprId),
}

pub(crate) fn source_selector(selector: &RootSelector) -> &RootSelector {
    match selector {
        RootSelector::InInstance { selector, .. } | RootSelector::InIteration { selector, .. } => {
            source_selector(selector)
        }
        selector => selector,
    }
}

pub(crate) fn in_occurrence(
    mut selector: RootSelector,
    occurrence: &[(u32, super::super::FrozenConstant, u64)],
) -> RootSelector {
    for (ordinal, value, domain_size) in occurrence.iter().rev() {
        selector = RootSelector::InIteration {
            ordinal: *ordinal,
            value: value.clone(),
            domain_size: *domain_size,
            selector: Box::new(selector),
        };
    }
    selector
}

pub(crate) fn loop_requires_occurrences(
    pool: &arandu_parser::AstPool,
    body: &arandu_parser::Block,
) -> bool {
    !static_ifs(pool, body.span).is_empty()
        || !static_fors(pool, body.span).is_empty()
        || !const_arguments(pool, body.span).is_empty()
        || !public_roots(pool, body.span).is_empty()
}

pub(crate) fn instance_substitution(
    checked: &arandu_typeck::TypeCheckResult,
    instance: &arandu_middle::types::FunctionInstance,
) -> Result<arandu_middle::types::GenericSubst, BuildFailure> {
    let parameters = checked
        .type_info
        .generic_params
        .get(&instance.definition)
        .ok_or(BuildFailure::GenericFunction)?;
    if parameters.len() != instance.arguments.len() {
        return Err(BuildFailure::GenericFunction);
    }
    let mut substitution = arandu_middle::types::GenericSubst::new();
    for (&parameter, argument) in parameters.iter().zip(&instance.arguments) {
        let ty = argument
            .intern(&checked.type_info.type_interner)
            .map_err(|_| BuildFailure::InvalidRoot)?;
        let ty = checked.type_info.type_interner.resolve(ty);
        let symbol = checked
            .symbols
            .try_get(parameter)
            .ok_or(BuildFailure::GenericFunction)?;
        if matches!(symbol.kind, arandu_middle::SymbolKind::ConstParam)
            != matches!(ty, ArType::Const(_) | ArType::FrozenConst(_))
        {
            return Err(BuildFailure::GenericFunction);
        }
        substitution.push((parameter, ty));
    }
    Ok(substitution)
}

impl Selected<'_> {
    pub(super) fn span(&self, program: &Program) -> Span {
        match self {
            Self::Block(block) => block.span,
            Self::Expression(expression) => program.pool.expr_span(*expression),
        }
    }
}

fn statement<'a>(program: &'a Program, block: &Block, index: u32) -> Option<&'a Stmt> {
    let index = usize::try_from(index).ok()?;
    let id = *program.pool.stmt_list(block.statements).get(index)?;
    Some(program.pool.stmt(id))
}

pub(super) fn select<'a>(
    program: &'a Program,
    body: &'a Block,
    selector: &RootSelector,
) -> Result<Selected<'a>, BuildFailure> {
    if let RootSelector::StaticForDomain(ordinal) = selector {
        let statements = static_fors(&program.pool, body.span);
        let statement = statements
            .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
            .ok_or(BuildFailure::InvalidRoot)?;
        let Stmt::For {
            clause: arandu_parser::ForClause::In { iterable, .. },
            ..
        } = statement
        else {
            return Err(BuildFailure::InvalidRoot);
        };
        return Ok(Selected::Expression(*iterable));
    }
    if let RootSelector::StaticForBound { ordinal, upper } = selector {
        let statements = static_fors(&program.pool, body.span);
        let statement = statements
            .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
            .ok_or(BuildFailure::InvalidRoot)?;
        let Stmt::For {
            clause: arandu_parser::ForClause::In { iterable, .. },
            ..
        } = statement
        else {
            return Err(BuildFailure::InvalidRoot);
        };
        let arandu_parser::ExprKind::Binary {
            op: arandu_parser::BinaryOp::RangeExclusive,
            left,
            right,
        } = program.pool.expr(*iterable)
        else {
            return Err(BuildFailure::InvalidRoot);
        };
        return Ok(Selected::Expression(if *upper { *right } else { *left }));
    }
    if let RootSelector::ConstArgument(ordinal) = selector {
        let roots = const_arguments(&program.pool, body.span);
        let (_, expression) = roots
            .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
            .ok_or(BuildFailure::InvalidRoot)?;
        return Ok(Selected::Expression(*expression));
    }
    if let RootSelector::StaticIfCondition(ordinal) = selector {
        let statements = static_ifs(&program.pool, body.span);
        let stmt = statements
            .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
            .ok_or(BuildFailure::InvalidRoot)?;
        return match stmt {
            Stmt::If {
                condition: arandu_parser::Condition::Expr { expr, .. },
                ..
            } => Ok(Selected::Expression(*expr)),
            _ => Err(BuildFailure::InvalidRoot),
        };
    }
    if let RootSelector::PublicComptime(ordinal) = selector {
        let roots = public_roots(&program.pool, body.span);
        let expression = *roots
            .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
            .ok_or(BuildFailure::InvalidRoot)?;
        return match program.pool.expr(expression) {
            arandu_parser::ExprKind::Comptime { body } => match body {
                arandu_parser::ast_pool::ComptimeBody::Expression(expr) => {
                    Ok(Selected::Expression(*expr))
                }
                arandu_parser::ast_pool::ComptimeBody::Block(block) => {
                    Ok(Selected::Block(program.pool.block(*block)))
                }
            },
            _ => Err(BuildFailure::InvalidRoot),
        };
    }
    let path = match selector {
        RootSelector::Block(path)
        | RootSelector::Initializer { block: path, .. }
        | RootSelector::IfCondition { block: path, .. } => path,
        RootSelector::InInstance { .. }
        | RootSelector::InIteration { .. }
        | RootSelector::PublicComptime(_)
        | RootSelector::StaticIfCondition(_)
        | RootSelector::StaticForBound { .. }
        | RootSelector::StaticForDomain(_)
        | RootSelector::ConstArgument(_)
        | RootSelector::HeaderArgument(_)
        | RootSelector::GlobalInitializer => return Err(BuildFailure::InvalidRoot),
    };
    // A malformed/stale selector cannot recurse indefinitely or index an arena
    // unchecked. This bound is internal, not a public CTFE execution budget.
    if path.len() > 64 {
        return Err(BuildFailure::InvalidRoot);
    }
    let mut block = body;
    for step in path {
        let stmt = statement(program, block, step.statement).ok_or(BuildFailure::InvalidRoot)?;
        block = match stmt {
            Stmt::If {
                then_block,
                else_block,
                ..
            } => match step.branch {
                BlockBranch::IfThen => then_block,
                BlockBranch::IfElse => else_block.as_ref().ok_or(BuildFailure::InvalidRoot)?,
                _ => return Err(BuildFailure::InvalidRoot),
            },
            Stmt::For { body, .. } | Stmt::While { body, .. }
                if step.branch == BlockBranch::LoopBody =>
            {
                body
            }
            Stmt::Unsafe { block, .. } if step.branch == BlockBranch::Unsafe => block,
            Stmt::Defer {
                body: DeferBody::Block { block, .. },
                ..
            } if step.branch == BlockBranch::Defer => block,
            Stmt::ErrDefer {
                body: DeferBody::Block { block, .. },
                ..
            } if step.branch == BlockBranch::ErrDefer => block,
            Stmt::VarDecl { .. }
            | Stmt::Set { .. }
            | Stmt::Return { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Free { .. }
            | Stmt::Expr { .. }
            | Stmt::For { .. }
            | Stmt::While { .. }
            | Stmt::Match { .. }
            | Stmt::Defer { .. }
            | Stmt::ErrDefer { .. }
            | Stmt::Unsafe { .. }
            | Stmt::Error(_) => return Err(BuildFailure::InvalidRoot),
        };
    }
    match selector {
        RootSelector::InInstance { .. }
        | RootSelector::InIteration { .. }
        | RootSelector::PublicComptime(_)
        | RootSelector::StaticIfCondition(_)
        | RootSelector::StaticForBound { .. }
        | RootSelector::StaticForDomain(_)
        | RootSelector::ConstArgument(_)
        | RootSelector::HeaderArgument(_)
        | RootSelector::GlobalInitializer => Err(BuildFailure::InvalidRoot),
        RootSelector::Block(_) => Ok(Selected::Block(block)),
        RootSelector::Initializer {
            statement: index, ..
        } => match statement(program, block, *index) {
            Some(Stmt::VarDecl { value, .. }) => Ok(Selected::Expression(*value)),
            _ => Err(BuildFailure::InvalidRoot),
        },
        RootSelector::IfCondition {
            statement: index, ..
        } => match statement(program, block, *index) {
            Some(Stmt::If {
                condition:
                    arandu_parser::Condition::Expr {
                        expr: expression, ..
                    },
                ..
            }) => Ok(Selected::Expression(*expression)),
            _ => Err(BuildFailure::InvalidRoot),
        },
    }
}

pub(crate) fn const_arguments(
    pool: &arandu_parser::ast_pool::AstPool,
    owner: Span,
) -> Vec<(Span, ExprId)> {
    let mut roots: Vec<_> = pool
        .type_exprs
        .iter()
        .filter_map(|ty| match ty {
            arandu_parser::TypeExpr::ConstExpression { span, expression }
                if contains(owner, *span) =>
            {
                Some((*span, *expression))
            }
            _ => None,
        })
        .collect();
    roots.sort_unstable_by_key(|(span, _)| (span.start, std::cmp::Reverse(span.end)));
    roots
}

/// Select a lexical occurrence without resolving its runtime container.
/// Its initializer/condition is never resolved as part of argument staging.
pub(super) fn argument_statement(program: &Program, body: &Block, argument: Span) -> Option<Span> {
    program
        .pool
        .stmts
        .iter()
        .filter(|stmt| contains(body.span, stmt.span()) && contains(stmt.span(), argument))
        .min_by_key(|stmt| stmt.span().end - stmt.span().start)?;
    // Scope navigation admits headers and declares only bindings preceding the
    // selected occurrence. Runtime initializers remain unevaluated captures.
    Some(argument)
}

/// Canonical AST discovery, not parsing or an offset-based query identity.
pub(crate) fn static_ifs(pool: &arandu_parser::ast_pool::AstPool, owner: Span) -> Vec<&Stmt> {
    let mut statements: Vec<_> = pool
        .stmts
        .iter()
        .filter(|stmt| {
            matches!(
                stmt,
                Stmt::If {
                    is_comptime: true,
                    ..
                }
            ) && contains(owner, stmt.span())
        })
        .collect();
    statements.sort_unstable_by_key(|stmt| (stmt.span().start, std::cmp::Reverse(stmt.span().end)));
    statements
}

pub(crate) fn static_fors(pool: &arandu_parser::ast_pool::AstPool, owner: Span) -> Vec<&Stmt> {
    let mut statements: Vec<_> = pool
        .stmts
        .iter()
        .filter(|statement| {
            matches!(
                statement,
                Stmt::For {
                    is_comptime: true,
                    ..
                }
            ) && contains(owner, statement.span())
        })
        .collect();
    statements.sort_unstable_by_key(|statement| {
        (
            statement.span().start,
            std::cmp::Reverse(statement.span().end),
        )
    });
    statements
}

/// Canonical AST discovery, not parsing or an offset-based query identity.
/// Sort lexically because allocation order is postorder and can change when a
/// sibling expression changes shape. Arena IDs are used only in this revision.
pub(crate) fn public_roots(pool: &arandu_parser::ast_pool::AstPool, owner: Span) -> Vec<ExprId> {
    let mut roots = pool
        .exprs
        .iter()
        .zip(&pool.expr_spans)
        .enumerate()
        .filter_map(|(index, (kind, &span))| {
            (matches!(kind, arandu_parser::ExprKind::Comptime { .. }) && contains(owner, span))
                .then(|| {
                    u32::try_from(index)
                        .ok()?
                        .checked_add(1)
                        .and_then(std::num::NonZeroU32::new)
                        .map(ExprId)
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    roots.sort_unstable_by_key(|&expr| {
        let span = pool.expr_span(expr);
        (span.start, span.end)
    });
    roots
}

pub(super) fn contains(outer: Span, inner: Span) -> bool {
    outer.file_id == inner.file_id && outer.start <= inner.start && inner.end <= outer.end
}
