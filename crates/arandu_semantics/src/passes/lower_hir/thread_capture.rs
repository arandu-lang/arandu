//! Lower one synthesized thread-capture `run` body.
//!
//! The call site lowers the lambda to a struct literal. This pass lowers only
//! the closure body and wraps it in one irrefutable struct pattern,
//! `match self { Capture { field: capture, .. } => body }`, that rebinds every
//! capture under its original symbol.
//!
//! Destructuring once, instead of rewriting each use to `self.field`, keeps
//! the captures independent owned values: consuming one capture would
//! otherwise move all of `self` and reject any later use of another capture.
//! It also leaves the body exactly as the type checker saw it, including
//! assignment targets, interpolation and nested closures.

use crate::TypeCheckResult;
use crate::diagnostics::{DiagCode, Diagnostic, Severity};
use crate::hir::{
    HirBlock, HirDecl, HirExpr, HirExprKind, HirFieldInit, HirFunc, HirParam, HirProgram, HirStmt,
    HirStmtKind, HirStruct, HirStructField, ReceiverKind,
};
use arandu_middle::SymbolId;
use arandu_middle::hir::{HirBlockId, HirFieldPattern, HirMatchArm, HirMatchArmBody, HirPattern};
use arandu_middle::types::TypeId;
use arandu_parser::Program;
use arandu_parser::ast_pool::{ExprId, ExprKind};
use arandu_typeck::type_checker::info::ThreadCaptureRecipe;

pub fn lower_thread_capture_hir(
    type_check: &mut TypeCheckResult,
    program: &Program,
    run_symbol: SymbolId,
) -> Result<HirProgram, Vec<Diagnostic>> {
    if type_check
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Err(type_check.diagnostics.clone());
    }
    let Some(recipe) = type_check
        .type_info
        .thread_capture_for_run(run_symbol)
        .cloned()
    else {
        return Err(vec![Diagnostic::ice(
            DiagCode::ICEL001,
            "thread capture recipe is missing for the synthesized run method",
            arandu_lexer::Span::new(run_symbol.file_id, 0, 0),
        )]);
    };
    let Some(lambda) = find_lambda(program, recipe.lambda_span) else {
        return Err(vec![Diagnostic::ice(
            DiagCode::ICEL001,
            "thread capture lambda is missing from the source file",
            recipe.lambda_span,
        )]);
    };
    let body = match program.pool.expr(lambda) {
        ExprKind::Lambda { body, .. } => body.clone(),
        _ => {
            return Err(vec![Diagnostic::ice(
                DiagCode::ICEL001,
                "thread capture span is not a lambda",
                recipe.lambda_span,
            )]);
        }
    };

    let mut pool = crate::hir::HirPool::new();
    let struct_ty = type_check
        .type_info
        .decl_type_id(recipe.struct_symbol)
        .unwrap_or_else(|| {
            let interner = &type_check.type_info.type_interner;
            let named = arandu_middle::types::ArType::named(recipe.struct_symbol, &[], interner);
            type_check.type_info_mut().type_interner.intern(named)
        });
    let body_block = match &body {
        arandu_parser::LambdaBody::Expr { expr, .. } => {
            let value = super::expr::lower_expr(type_check, &program.pool, &mut pool, *expr)
                .map_err(|diagnostic| vec![diagnostic])?;
            let span = program.pool.expr_span(*expr);
            let values = pool.alloc_expr_list(&[value]);
            let ret = pool.alloc_stmt(HirStmt {
                kind: HirStmtKind::Return { values },
                span,
            });
            let statements = pool.alloc_stmt_list(&[ret]);
            pool.alloc_block(HirBlock { statements, span })
        }
        arandu_parser::LambdaBody::Block { block, .. } => {
            let lowered = super::stmt::lower_block(type_check, &program.pool, &mut pool, block)
                .map_err(|diagnostic| vec![diagnostic])?;
            return_value_tail(&mut pool, lowered, &program.pool, block);
            lowered
        }
    };
    let body_block = destructure_captures(&mut pool, &recipe, struct_ty, body_block, type_check);

    let params = pool.alloc_param_list(&[
        HirParam {
            symbol: recipe.self_param,
            ty: struct_ty,
            span: recipe.lambda_span,
            is_receiver: true,
            receiver_kind: Some(ReceiverKind::Own),
        },
        HirParam {
            symbol: recipe.arg_param,
            ty: type_check
                .type_info
                .type_interner
                .intern(arandu_middle::types::ArType::Void),
            span: recipe.lambda_span,
            is_receiver: false,
            receiver_kind: None,
        },
    ]);
    let func = pool.alloc_decl(HirDecl::Func(HirFunc {
        symbol: recipe.run_symbol,
        params,
        return_type: recipe.return_type,
        body: Some(body_block),
        span: recipe.lambda_span,
        is_async: false,
        no_fallback: false,
    }));
    let struct_fields = pool.alloc_struct_field_list(
        &recipe
            .fields
            .iter()
            .map(|field| HirStructField {
                symbol: field.symbol,
                ty: field.ty,
                span: field.span,
            })
            .collect::<Vec<_>>(),
    );
    let struct_decl = pool.alloc_decl(HirDecl::Struct(HirStruct {
        symbol: recipe.struct_symbol,
        fields: struct_fields,
        span: recipe.lambda_span,
    }));
    Ok(HirProgram {
        span: recipe.lambda_span,
        module: program.module.as_ref().map(|module| module.path.join(".")),
        decls: vec![struct_decl, func],
        pool,
    })
}

fn find_lambda(program: &Program, span: arandu_lexer::Span) -> Option<ExprId> {
    program
        .pool
        .expr_spans
        .iter()
        .enumerate()
        .find_map(|(index, expr_span)| {
            (*expr_span == span
                && matches!(
                    program.pool.expr(ExprId::new(index)),
                    ExprKind::Lambda { .. }
                ))
            .then_some(ExprId::new(index))
        })
}

/// The call site moves each capture once. The closure body is not lowered here.
pub(crate) fn capture_struct_literal(
    hir_pool: &mut crate::hir::HirPool,
    recipe: &ThreadCaptureRecipe,
) -> HirExprKind {
    let inits = recipe
        .fields
        .iter()
        .map(|field| {
            let value = hir_pool.alloc_expr(HirExpr {
                kind: HirExprKind::Path {
                    symbol: field.source,
                },
                ty: field.ty,
                span: field.span,
            });
            HirFieldInit {
                span: field.span,
                name: field.name.clone(),
                value,
            }
        })
        .collect::<Vec<_>>();
    HirExprKind::StructLiteral {
        struct_symbol: recipe.struct_symbol,
        fields: hir_pool.alloc_field_init_list(&inits),
    }
}

/// A final expression without `;` is the closure result. Inside the
/// destructuring arm it would only be the arm's value, so it becomes an
/// explicit `return`.
fn return_value_tail(
    pool: &mut crate::hir::HirPool,
    lowered: HirBlockId,
    ast: &arandu_parser::ast_pool::AstPool,
    block: &arandu_parser::Block,
) {
    let Some(tail) = ast
        .stmt_list(block.statements)
        .last()
        .and_then(|&id| match ast.stmt(id) {
            arandu_parser::Stmt::Expr {
                expr,
                has_semi: false,
                ..
            } => Some(ast.expr_span(*expr)),
            _ => None,
        })
    else {
        return;
    };
    let Some(&last) = pool.stmt_list(pool.block(lowered).statements).last() else {
        return;
    };
    let HirStmtKind::Expr(value) = pool.stmt(last).kind else {
        return;
    };
    if pool.expr(value).span != tail {
        return;
    }
    let values = pool.alloc_expr_list(&[value]);
    if let Some(statement) = pool.stmts.get_mut(last) {
        statement.kind = HirStmtKind::Return { values };
    }
}

/// `match self { Capture { f: capture, .. } => body }`, binding each field to
/// the symbol the closure body already uses.
fn destructure_captures(
    pool: &mut crate::hir::HirPool,
    recipe: &ThreadCaptureRecipe,
    struct_ty: TypeId,
    body: HirBlockId,
    type_check: &TypeCheckResult,
) -> HirBlockId {
    let span = recipe.lambda_span;
    let field_patterns = recipe
        .fields
        .iter()
        .map(|field| {
            let name = type_check
                .symbols
                .try_get(field.source)
                .map_or_else(|| field.name.clone(), |symbol| symbol.name.clone());
            let binding = pool.alloc_pattern(HirPattern::Bind {
                span: field.span,
                name,
                symbol: field.source,
            });
            pool.alloc_field_pattern(HirFieldPattern {
                span: field.span,
                name: field.name.clone(),
                pattern: Some(binding),
            })
        })
        .collect::<Vec<_>>();
    let fields = pool.alloc_field_pattern_list(&field_patterns);
    let pattern = pool.alloc_pattern(HirPattern::Struct {
        span,
        struct_symbol: recipe.struct_symbol,
        fields,
    });
    let value = pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path {
            symbol: recipe.self_param,
        },
        ty: struct_ty,
        span,
    });
    let arms = pool.alloc_match_arm_list(&[HirMatchArm {
        span,
        pattern,
        guard: None,
        body: HirMatchArmBody::Block(body),
    }]);
    let destructure = pool.alloc_stmt(HirStmt {
        kind: HirStmtKind::Match { value, arms },
        span,
    });
    let statements = pool.alloc_stmt_list(&[destructure]);
    pool.alloc_block(HirBlock { statements, span })
}
