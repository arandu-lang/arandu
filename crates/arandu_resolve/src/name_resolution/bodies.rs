use super::HeaderResolution;
use super::Resolver;
use crate::ResolutionResult;
use arandu_parser::{FuncName, Program, TopLevelDecl};

#[must_use]
pub fn resolve_bodies_with_poll(
    program: &Program,
    headers: HeaderResolution,
    poll: impl FnMut(),
) -> ResolutionResult {
    resolve_selected_body_with_poll(program, headers, BodySelection::All, poll)
}

/// A revision-local selection borrowed from the same AST as the header state.
/// Expression staging sees declaration/parameter scope, not surrounding runtime
/// locals. The orchestration layer must select parents before nested conditions.
#[derive(Debug, Clone, Copy)]
pub enum BodySelection {
    All,
    Function(crate::SymbolId),
    Expression {
        owner: crate::SymbolId,
        expression: arandu_parser::ast_pool::ExprId,
    },
    /// Establish the lexical scope of an enclosing ordinary statement without
    /// resolving its runtime expressions. Shared by conditions and arguments.
    LexicalExpression {
        owner: crate::SymbolId,
        expression: arandu_parser::ast_pool::ExprId,
        statement: arandu_middle::Span,
    },
    LexicalBlock {
        owner: crate::SymbolId,
        block: arandu_parser::ast_pool::BlockId,
        statement: arandu_middle::Span,
    },
    LexicalLoopBody {
        owner: crate::SymbolId,
        statement: arandu_middle::Span,
    },
}

/// Continue the canonical resolver for one body/root without visiting siblings.
/// Unused-import warnings are meaningful only for complete resolution.
#[must_use]
pub fn resolve_selected_body_with_poll(
    program: &Program,
    headers: HeaderResolution,
    selection: BodySelection,
    mut poll: impl FnMut(),
) -> ResolutionResult {
    let HeaderResolution {
        declarations,
        body_scopes,
        import_aliases,
        current_module,
        imported_symbols,
        used_symbols,
    } = headers;
    let failed_import_aliases = declarations
        .symbols
        .unresolved_module_aliases
        .iter()
        .cloned()
        .collect();
    let mut resolver = Resolver {
        reusable_definitions: declarations.resolved.definitions.keys().copied().collect(),
        symbols: std::sync::Arc::unwrap_or_clone(declarations.symbols),
        resolved: std::sync::Arc::unwrap_or_clone(declarations.resolved),
        docs: declarations.docs,
        diagnostics: declarations.diagnostics,
        pool: &program.pool,
        import_aliases,
        failed_import_aliases,
        current_module,
        imported_symbols,
        used_symbols,
    };
    program.for_each_decl_recursive(|_, declaration| {
        poll();
        if let TopLevelDecl::Func(function) = declaration {
            let key = match &function.name {
                FuncName::Free { span, .. } | FuncName::Method { span, .. } => (*span).into(),
            };
            let symbol = resolver.resolved.definitions.get(&key).copied();
            let selected = match selection {
                BodySelection::All => true,
                BodySelection::Function(owner) | BodySelection::Expression { owner, .. } | BodySelection::LexicalExpression { owner, .. } | BodySelection::LexicalBlock { owner, .. } | BodySelection::LexicalLoopBody { owner, .. } => {
                    symbol == Some(owner)
                }
            };
            if !selected {
                return;
            }
            if symbol.is_some_and(|symbol| resolver.resolved.deferred_comptime_functions.contains(&symbol))
                && matches!(selection, BodySelection::All)
            {
                return;
            }
            if let Some(&scope) = body_scopes.get(&key) {
                match selection {
                    BodySelection::All | BodySelection::Function(_) => {
                        resolver.resolve_block_in_scope(scope, &program.pool, &function.body);
                    }
                    BodySelection::Expression { expression, .. } => {
                        resolver.resolve_expr(scope, expression);
                    }
                    BodySelection::LexicalExpression { expression, statement, .. } => {
                        if let Some(scope) = resolver.staging_context(scope, &program.pool, &function.body, statement, 0) {
                            resolver.resolve_expr(scope, expression);
                        } else {
                            resolver.diagnostics.push(crate::Diagnostic::error(
                                crate::DiagCode::T042UnsupportedComptime,
                                "compile-time staging could not establish this expression's lexical scope",
                                statement,
                            ));
                        }
                    }
                    BodySelection::LexicalBlock { block, statement, .. } => {
                        if let Some(scope) = resolver.staging_context(scope, &program.pool, &function.body, statement, 0) {
                            resolver.resolve_block_in_scope(scope, &program.pool, program.pool.block(block));
                        }
                    }
                    BodySelection::LexicalLoopBody { statement, .. } => {
                        if let Some(arandu_parser::Stmt::For { body, .. }) = program.pool.stmts.iter().find(|stmt| stmt.span() == statement)
                            && let Some(scope) = resolver.staging_context(scope, &program.pool, &function.body, body.span, 0) {
                            resolver.resolve_block_in_scope(scope, &program.pool, body);
                        }
                    }
                }
            } else {
                resolver.diagnostics.push(crate::Diagnostic::ice(
                    crate::DiagCode::ICET001,
                    "function body has no declaration resolution scope",
                    function.span,
                ));
            }
        }
    });
    if matches!(selection, BodySelection::All) {
        resolver.check_unused_imports();
    }
    ResolutionResult {
        is_cycle_fallback: declarations.is_cycle_fallback,
        symbols: std::sync::Arc::new(resolver.symbols),
        resolved: std::sync::Arc::new(resolver.resolved),
        docs: resolver.docs,
        diagnostics: resolver.diagnostics,
    }
}
