//! Isolated staged roots selected from canonical AST. Public roots use lexical
//! ordinals; internal selectors use statement paths. Initial typing never requests the runtime owner's
//! body, final AMIR or borrow contracts. These selectors are source-local paths,
//! not persistent IDs or an alternative source spelling of public `comptime`.

use std::sync::Arc;

use arandu_middle::{hir::HirDecl, types::ArType, Severity, Span, SymbolId};
use arandu_mir::ctfe::{Budget, CtfeFunction, EvalError};
use arandu_parser::{ast_pool::ExprId, Block, DeferBody, Program, Stmt, TopLevelDecl};

use super::{BuildFailure, CtfeLowering, QueryProvider};
use crate::{db::HashEq, ArandCompilerDb, SourceFile};

/// A source-local statement child. No offsets or arena IDs are part of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockBranch {
    IfThen,
    IfElse,
    LoopBody,
    Unsafe,
    Defer,
    ErrDefer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockStep {
    pub statement: u32,
    pub branch: BlockBranch,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RootSelector {
    InIteration {
        ordinal: u32,
        value: super::FrozenConstant,
        domain_size: u64,
        selector: Box<RootSelector>,
    },
    InInstance {
        instance: arandu_middle::types::FunctionInstance,
        selector: Box<RootSelector>,
    },
    /// Source-local ordinal of an explicit public root in the owner's body.
    PublicComptime(u32),
    /// The initializer of a module/type constant, evaluated independently.
    GlobalInitializer,
    HeaderArgument(u32),
    /// Lexical ordinal of a public static-if statement in the owner.
    StaticIfCondition(u32),
    /// A frozen fixed-array domain, before runtime body typing.
    StaticForDomain(u32),
    /// A finite half-open static range endpoint, before runtime body typing.
    StaticForBound {
        ordinal: u32,
        upper: bool,
    },
    /// Lexical ordinal of a computed generic argument in the owner body.
    ConstArgument(u32),
    /// Empty path selects the owner's body with an independent return target.
    Block(Vec<BlockStep>),
    /// Select one initializer without typing the surrounding runtime statements.
    Initializer {
        block: Vec<BlockStep>,
        statement: u32,
    },
    /// Internal pre-body condition staging; validates neither branch. This is
    /// not public `comptime if` syntax or authorization to select nested parents.
    IfCondition {
        block: Vec<BlockStep>,
        statement: u32,
    },
}

/// Pool-independent expected type for the initial scalar staging slice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RootExpectedType {
    Unit,
    Primitive(arandu_middle::types::Primitive),
    Structural(arandu_middle::types::TypeShape),
}

#[salsa::interned(constructor = new_in_context)]
pub struct CtfeRoot<'db> {
    pub file: SourceFile,
    pub owner: SymbolId,
    #[returns(ref)]
    pub selector: RootSelector,
    pub expected: Option<RootExpectedType>,
    #[returns(ref)]
    pub dependency: super::DependencyContext,
}

impl<'db> CtfeRoot<'db> {
    pub fn new(
        db: &'db dyn ArandCompilerDb,
        file: SourceFile,
        owner: SymbolId,
        selector: RootSelector,
        expected: Option<RootExpectedType>,
    ) -> Self {
        Self::new_in_context(
            db,
            file,
            owner,
            selector,
            expected,
            super::DependencyContext::default(),
        )
    }
}

#[salsa::interned]
pub struct CtfeRootRequest<'db> {
    pub root: CtfeRoot<'db>,
    pub budget: Budget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootEvalError {
    Build(BuildFailure),
    Evaluation(EvalError),
}

enum Selected<'a> {
    Block(&'a Block),
    Expression(ExprId),
}

enum LoweredRoot {
    Block(arandu_middle::hir::HirBlockId),
    Expression(arandu_middle::hir::HirExprId),
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
    occurrence: &[(u32, super::FrozenConstant, u64)],
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
    fn span(&self, program: &Program) -> Span {
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

fn select<'a>(
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
fn argument_statement(program: &Program, body: &Block, argument: Span) -> Option<Span> {
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

fn contains(outer: Span, inner: Span) -> bool {
    outer.file_id == inner.file_id && outer.start <= inner.start && inner.end <= outer.end
}

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
    let selector = source_selector(root.selector(db));
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
        if iterations.len() > super::MAX_QUERY_DEPENDENCY_DEPTH {
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
            let roots = super::headers::header_arguments(program, &declared.resolved, owner);
            let (_, expression) = roots
                .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
                .ok_or(BuildFailure::InvalidRoot)?;
            Selected::Expression(*expression)
        } else if matches!(selector, RootSelector::GlobalInitializer) {
            Selected::Expression(initializer.ok_or(BuildFailure::InvalidRoot)?)
        } else {
            select(
                program,
                body.ok_or(BuildFailure::MissingFunction)?,
                selector,
            )?
        };
        let span = selected.span(program);
        let mut initial = if matches!(selector, RootSelector::GlobalInitializer) {
            super::headers::owner_signatures_in_context(db, file, owner, root.dependency(db))
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
                let expression = *public_roots(
                    &program.pool,
                    body.ok_or(BuildFailure::MissingFunction)?.span,
                )
                .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
                .ok_or(BuildFailure::InvalidRoot)?;
                let statement = argument_statement(
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
            } else if matches!(selected, Selected::Block(_)) {
                arandu_resolve::BodySelection::Function(owner)
            } else {
                let Selected::Expression(expression) = selected else {
                    return Err(BuildFailure::InvalidRoot);
                };
                if let RootSelector::StaticForBound { ordinal, .. }
                | RootSelector::StaticForDomain(ordinal) = selector
                {
                    let statements = static_fors(
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
                    let statements = static_ifs(
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
                    let arguments = const_arguments(&program.pool, body.span);
                    let (argument, _) = arguments
                        .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
                        .ok_or(BuildFailure::InvalidRoot)?;
                    let statement =
                        argument_statement(program, body, *argument).ok_or_else(|| {
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
        if matches!(selector, RootSelector::PublicComptime(_)) {
            let branches = if let Some(instance) = instance {
                super::branches::select_branches(db, file, owner, Some(instance))
            } else {
                super::item_static_branches(db, file, owner).clone()
            };
            let arguments =
                super::arguments::select_arguments(db, file, owner, instance, &branches);
            let names = Arc::make_mut(&mut initial.resolved);
            names
                .comptime_arguments
                .extend(arguments.values.iter().map(|(&key, &value)| (key, value)));
            names.typed_comptime_arguments.extend(
                arguments
                    .typed_values
                    .iter()
                    .map(|(&key, value)| (key, value.clone())),
            );
            initial
                .diagnostics
                .extend(arguments.diagnostics.iter().cloned());
        }
        if matches!(
            selector,
            RootSelector::ConstArgument(_)
                | RootSelector::StaticIfCondition(_)
                | RootSelector::StaticForBound { .. }
                | RootSelector::StaticForDomain(_)
        ) {
            // Stage children of the selected header only. Requesting all body
            // arguments here would re-enter parent-first branch/domain queries.
            let body_span = body.ok_or(BuildFailure::MissingFunction)?.span;
            let arguments = const_arguments(&program.pool, body_span);
            let nesting = arguments
                .iter()
                .filter(|(outer, _)| contains(*outer, span))
                .count();
            if nesting > 64 {
                return Err(BuildFailure::Diagnostics(vec![
                    arandu_middle::Diagnostic::error(
                        arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                        "computed generic argument nesting exceeds 64 levels",
                        span,
                    ),
                ]));
            }
            for (ordinal, &(child, _)) in arguments.iter().enumerate() {
                if child == span || !contains(span, child) {
                    continue;
                }
                let ordinal = u32::try_from(ordinal).map_err(|_| BuildFailure::InvalidRoot)?;
                let selector = instance.map_or(RootSelector::ConstArgument(ordinal), |instance| {
                    RootSelector::InInstance {
                        instance: instance.clone(),
                        selector: Box::new(RootSelector::ConstArgument(ordinal)),
                    }
                });
                let child_root = CtfeRoot::new_in_context(
                    db,
                    file,
                    owner,
                    in_occurrence(selector, &iterations),
                    None,
                    root.dependency(db).clone(),
                );
                let budget = super::public::staging_budget(
                    super::config::public_budget(db),
                    arguments.len(),
                    super::public::iteration_count(&iterations),
                );
                let value = ctfe_eval_root(db, CtfeRootRequest::new(db, child_root, budget))
                    .as_ref()
                    .map_err(|error| match error {
                        RootEvalError::Build(error) => error.clone(),
                        RootEvalError::Evaluation(error) => BuildFailure::Evaluation(error.kind),
                    })?;
                let names = Arc::make_mut(&mut initial.resolved);
                names.comptime_arguments.insert(
                    child.into(),
                    match value {
                        arandu_middle::ctfe::ConstValue::Integer(integer) => {
                            integer.to_const_generic().ok()
                        }
                        _ => None,
                    },
                );
                if !matches!(value, arandu_middle::ctfe::ConstValue::Integer(integer) if integer.to_const_generic().is_ok())
                {
                    names
                        .typed_comptime_arguments
                        .insert(child.into(), value.clone());
                }
            }
        }
        if instance.is_none()
            && (matches!(selector, RootSelector::ConstArgument(_))
                || (matches!(selector, RootSelector::StaticIfCondition(_))
                    && initial.type_info.generic_params.contains_key(&owner)))
        {
            // An instance-independent argument in a template is safe to freeze
            // once. A dependent argument needs a future per-instance staging
            // boundary; never evaluate a template parameter as an uninitialized
            // VM local or erase it into an arbitrary constant key.
            let dependency = initial
                .resolved
                .value_refs
                .iter()
                .chain(initial.resolved.type_refs.iter())
                .map(|(key, id)| (*key, *id))
                .chain(
                    initial
                        .resolved
                        .expr_symbols
                        .iter()
                        .zip(&program.pool.expr_spans)
                        .filter_map(|(symbol, span)| {
                            symbol.map(|symbol| (arandu_middle::NodeKey::from(*span), symbol))
                        }),
                )
                .filter(|(key, _)| span.start <= key.start && key.end <= span.end)
                .filter_map(|(key, id)| {
                    let symbol = initial.symbols.try_get(id)?;
                    matches!(
                        symbol.kind,
                        arandu_middle::SymbolKind::ConstParam
                            | arandu_middle::SymbolKind::TypeParam
                    )
                    .then_some((key, symbol))
                })
                .min_by_key(|(key, _)| (key.start, key.end));
            if let Some((key, symbol)) = dependency {
                return Err(BuildFailure::Diagnostics(vec![
                    arandu_middle::Diagnostic::error(arandu_middle::DiagCode::T042UnsupportedComptime,
                        "this compile-time expression depends on template parameters and requires concrete instance staging",
                        Span::new(span.file_id, key.start, key.end))
                        .with_primary_label("this value depends on the generic instance")
                        .with_label(symbol.span, "template parameter declared here")
                        .with_note("instance-independent computed arguments are supported; dependent evaluation requires per-instance staging"),
                ]));
            }
        }
        let mut iteration_symbols = Vec::new();
        for (ordinal, value, _) in &iterations {
            let loops = static_fors(
                &program.pool,
                body.ok_or(BuildFailure::MissingFunction)?.span,
            );
            let statement = loops
                .get(usize::try_from(*ordinal).map_err(|_| BuildFailure::InvalidRoot)?)
                .ok_or(BuildFailure::InvalidRoot)?;
            let Stmt::For {
                clause: arandu_parser::ForClause::In { bindings, .. },
                body: loop_body,
                ..
            } = statement
            else {
                return Err(BuildFailure::InvalidRoot);
            };
            if !contains(loop_body.span, span) {
                return Err(BuildFailure::InvalidRoot);
            }
            let binding = bindings.first().ok_or(BuildFailure::InvalidRoot)?;
            let symbol = *initial
                .resolved
                .definitions
                .get(&binding.span.into())
                .ok_or(BuildFailure::InvalidRoot)?;
            let ty = value
                .shape
                .intern(&initial.type_info.type_interner)
                .map_err(|_| BuildFailure::InvalidRoot)?;
            initial.type_info_mut().decl_types.insert(symbol, ty);
            iteration_symbols.push((symbol, value));
        }
        let exempt: Vec<_> = iteration_symbols
            .iter()
            .map(|(symbol, _)| *symbol)
            .collect();
        if let Some(capture) = arandu_typeck::type_checker::check::find_ctfe_runtime_capture_except(
            &initial,
            &program.pool,
            span,
            &exempt,
        ) {
            return Err(BuildFailure::RuntimeCapture(Box::new(
                super::CapturedRuntimeValue {
                    symbol: capture.symbol,
                    name: Arc::from(initial.symbols.get(capture.symbol).name.as_str()),
                    use_span: capture.use_span,
                    declaration_span: capture.declaration_span,
                },
            )));
        }
        super::headers::install_referenced_headers_in_context(
            db,
            &program.pool,
            span,
            &mut initial,
            root.dependency(db),
        )?;
        super::globals::install_referenced_globals_in_context(
            db,
            program,
            span,
            &mut initial,
            root.dependency(db),
        )?;
        let diagnostics = initial
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                contains(span, diagnostic.span)
                    || program
                        .imports
                        .iter()
                        .any(|import| contains(import.span(), diagnostic.span))
            })
            .cloned()
            .collect();
        initial.diagnostics = diagnostics;
        let expected = if condition {
            Some(ArType::Primitive(arandu_middle::types::Primitive::Bool))
        } else {
            root.expected(db).as_ref().map(|ty| match ty {
                RootExpectedType::Unit => ArType::Void,
                RootExpectedType::Primitive(primitive) => ArType::Primitive(*primitive),
                RootExpectedType::Structural(shape) => shape
                    .intern(&initial.type_info.type_interner)
                    .ok()
                    .map(|id| initial.type_info.type_interner.resolve(id))
                    .unwrap_or(ArType::Error),
            })
        };
        let substitution = instance
            .map(|instance| instance_substitution(&initial, instance))
            .transpose()?
            .unwrap_or_default();
        use arandu_typeck::type_checker::check::{
            check_ctfe_root_with_substitution, CtfeInitialRoot,
        };
        let input = match selected {
            Selected::Block(block) => CtfeInitialRoot::Block(block),
            Selected::Expression(expression) => CtfeInitialRoot::Expression(expression),
        };
        let (mut checked, mut typed) = check_ctfe_root_with_substitution(
            &initial,
            &program.pool,
            input,
            expected.clone(),
            crate::passes::database_target_info(db),
            &substitution,
        );
        for _ in 0..super::MAX_QUERY_DEPENDENCY_DEPTH {
            match super::contracts::freeze_requests(db, &mut checked, root.dependency(db))
                .map_err(BuildFailure::Diagnostics)?
            {
                0 => break,
                _ => {
                    (checked, typed) = check_ctfe_root_with_substitution(
                        &checked,
                        &program.pool,
                        input,
                        expected.clone(),
                        crate::passes::database_target_info(db),
                        &substitution,
                    );
                }
            }
        }
        if !checked.type_info.header_requests.is_empty() {
            checked.diagnostics.push(arandu_middle::Diagnostic::error(
                arandu_middle::DiagCode::T045ComptimeLimitExceeded,
                "concrete header discovery exceeds its continuation limit",
                span,
            ));
        }
        if checked
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return Err(BuildFailure::Diagnostics(checked.diagnostics));
        }
        db.unwind_if_revision_cancelled();
        let mut hir = super::globals::declaration_context(db, file, program, &mut checked)?;
        super::globals::link_referenced_globals(db, program, span, &mut checked, &mut hir)?;
        let mut lowered = match selected {
            Selected::Block(block) => {
                let block = arandu_semantics::lower_block_to_hir(
                    &mut checked,
                    &program.pool,
                    &mut hir,
                    block,
                )
                .map_err(BuildFailure::Diagnostics)?;
                LoweredRoot::Block(block)
            }
            Selected::Expression(expression) => {
                let expression = arandu_semantics::lower_expression_to_hir(
                    &mut checked,
                    &program.pool,
                    &mut hir,
                    expression,
                )
                .map_err(BuildFailure::Diagnostics)?;
                LoweredRoot::Expression(expression)
            }
        };
        if let Some(instance) = instance {
            let substitution = instance_substitution(&checked, instance)?;
            lowered = match lowered {
                LoweredRoot::Expression(expression) => LoweredRoot::Expression(
                    arandu_semantics::passes::monomorphize::specialize_root_expression(
                        &mut checked,
                        &mut hir,
                        expression,
                        &substitution,
                    )
                    .map_err(|diagnostic| BuildFailure::Diagnostics(vec![diagnostic]))?,
                ),
                LoweredRoot::Block(block) => LoweredRoot::Block(
                    arandu_semantics::passes::monomorphize::specialize_root_block(
                        &mut checked,
                        &mut hir,
                        block,
                        &substitution,
                    )
                    .map_err(|diagnostic| BuildFailure::Diagnostics(vec![diagnostic]))?,
                ),
            };
        }
        let replacements: Vec<_> = hir
            .pool
            .exprs
            .iter()
            .enumerate()
            .filter_map(|(index, expression)| {
                let arandu_middle::hir::HirExprKind::Path { symbol } = expression.kind else {
                    return None;
                };
                let (_, frozen) = iteration_symbols
                    .iter()
                    .find(|(binding, _)| *binding == symbol)?;
                Some((
                    arandu_middle::hir::HirExprId::from_usize(index),
                    expression.ty,
                    expression.span,
                    frozen.value.clone(),
                ))
            })
            .collect();
        for (id, ty, span, value) in replacements {
            let replacement = arandu_semantics::passes::lower_hir::materialize_ctfe_value(
                &value,
                ty,
                &checked.type_info,
                &mut hir.pool,
                layout,
                span,
            )
            .map_err(|_| BuildFailure::InvalidRoot)?;
            *hir.pool.expr_mut(id) = replacement;
        }
        super::link_extern_headers(db, &mut checked, &mut hir)?;
        let calls = arandu_semantics::passes::monomorphize::specialize_root_callees(
            &mut checked,
            &mut hir,
            match lowered {
                LoweredRoot::Expression(expression) => {
                    arandu_semantics::passes::monomorphize::InstantiationRoot::Expression(
                        expression,
                    )
                }
                LoweredRoot::Block(block) => {
                    arandu_semantics::passes::monomorphize::InstantiationRoot::Block(block)
                }
            },
        )
        .map_err(BuildFailure::Diagnostics)?;
        let global_header = arandu_middle::hir::HirFunc {
            symbol: owner,
            params: hir.pool.alloc_param_list(&[]),
            return_type: typed.return_type,
            body: None,
            span,
            is_async: false,
            no_fallback: false,
        };
        let header = hir
            .decls
            .iter()
            .find_map(|&id| match hir.pool.decl(id) {
                HirDecl::Func(function) if function.symbol == owner => Some(function),
                _ => None,
            })
            .or_else(|| {
                matches!(
                    selector,
                    RootSelector::GlobalInitializer | RootSelector::HeaderArgument(_)
                )
                .then_some(&global_header)
            })
            .ok_or(BuildFailure::MissingFunction)?;
        db.unwind_if_revision_cancelled();
        let unit = match lowered {
            LoweredRoot::Expression(expression) => {
                arandu_mir::lower_expression_unit(&checked, &hir, header, expression, layout)
            }
            LoweredRoot::Block(block) => arandu_mir::lower_block_unit(
                &checked,
                &hir,
                header,
                block,
                typed.return_type,
                typed.value_tail.is_some(),
                layout,
            ),
        }
        .map_err(BuildFailure::Diagnostics)?;
        CtfeFunction::new_with_provider(
            Arc::unwrap_or_clone(unit.function),
            unit.literals,
            &checked.type_info.type_interner,
            layout,
            checked.type_info.as_ref(),
        )
        .and_then(|unit| {
            unit.bind_instance(
                instance
                    .cloned()
                    .unwrap_or(arandu_middle::types::FunctionInstance {
                        definition: owner,
                        arguments: Vec::new(),
                    }),
                calls,
            )
        })
        .map(Arc::new)
        .map_err(BuildFailure::Evaluation)
    };
    HashEq::new(CtfeLowering { result: build() })
}

fn root_cycle<'db>(
    db: &'db dyn ArandCompilerDb,
    _id: salsa::Id,
    request: CtfeRootRequest<'db>,
) -> Result<arandu_middle::ctfe::ConstValue, RootEvalError> {
    let root = request.root(db);
    let file = root.file(db);
    Err(RootEvalError::Build(BuildFailure::Diagnostics(vec![
        arandu_middle::Diagnostic::error(
            arandu_middle::DiagCode::T044ComptimeEvaluationFailed,
            "compile-time evaluation depends on its own static selection",
            Span::new(*file.file_id(db), 0, 0),
        )
        .with_primary_label("cyclic compile-time dependency"),
    ])))
}

#[salsa::tracked(cycle_result = root_cycle)]
#[tracing::instrument(
    level = "trace",
    target = "arandu_query",
    skip(db, request),
    fields(query = "ctfe_eval_root")
)]
pub fn ctfe_eval_root<'db>(
    db: &'db dyn ArandCompilerDb,
    request: CtfeRootRequest<'db>,
) -> Result<arandu_middle::ctfe::ConstValue, RootEvalError> {
    let lowered = ctfe_root_amir(db, *request.root(db));
    let unit = lowered
        .result
        .as_ref()
        .map(Arc::clone)
        .map_err(|error| RootEvalError::Build(error.clone()))?;
    let provider = QueryProvider {
        db,
        context: request.root(db).dependency(db).clone(),
        failures: Default::default(),
        headers_only: matches!(
            source_selector(request.root(db).selector(db)),
            RootSelector::IfCondition { .. }
                | RootSelector::StaticIfCondition(_)
                | RootSelector::StaticForBound { .. }
                | RootSelector::StaticForDomain(_)
                | RootSelector::ConstArgument(_)
                | RootSelector::GlobalInitializer
                | RootSelector::HeaderArgument(_)
        ),
    };
    let evaluated =
        arandu_mir::ctfe::evaluate_unit(&provider, unit, &[], *request.budget(db), || {
            db.unwind_if_revision_cancelled();
            false
        });
    if evaluated.is_err() && !provider.failures.borrow().is_empty() {
        return Err(RootEvalError::Build(BuildFailure::Diagnostics(
            provider.failures.into_inner(),
        )));
    }
    evaluated.map_err(RootEvalError::Evaluation)
}
