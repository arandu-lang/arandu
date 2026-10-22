//! Lower one synthesized thread-capture `run` body.
//!
//! The call site lowers the lambda to a struct literal. This pass lowers only
//! the closure body, rewriting each captured path to a fresh `self.field`.

use crate::TypeCheckResult;
use crate::diagnostics::{DiagCode, Diagnostic, Severity};
use crate::hir::{
    HirBlock, HirDecl, HirExpr, HirExprKind, HirFieldInit, HirFunc, HirParam, HirProgram, HirStmt,
    HirStmtKind, HirStruct, HirStructField, ReceiverKind,
};
use arandu_middle::SymbolId;
use arandu_middle::hir::{HirBlockId, HirExprId, IndexRange};
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
            rewrite_expr(&mut pool, value, &recipe, recipe.self_param, struct_ty);
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
            rewrite_block(&mut pool, lowered, &recipe, recipe.self_param, struct_ty);
            lowered
        }
    };

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

fn rewrite_block(
    pool: &mut crate::hir::HirPool,
    block: HirBlockId,
    recipe: &ThreadCaptureRecipe,
    self_param: SymbolId,
    self_ty: TypeId,
) {
    let statements = pool.stmt_list(pool.block(block).statements).to_vec();
    for statement in statements {
        rewrite_stmt(pool, statement, recipe, self_param, self_ty);
    }
}

fn rewrite_stmt(
    pool: &mut crate::hir::HirPool,
    id: arandu_middle::hir::HirStmtId,
    recipe: &ThreadCaptureRecipe,
    self_param: SymbolId,
    self_ty: TypeId,
) {
    let kind = pool.stmt(id).kind.clone();
    match kind {
        HirStmtKind::VarDecl { value, .. }
        | HirStmtKind::Set { value, .. }
        | HirStmtKind::Free(value)
        | HirStmtKind::Expr(value) => rewrite_expr(pool, value, recipe, self_param, self_ty),
        HirStmtKind::Return { values } => {
            for value in expr_ids(pool, values) {
                rewrite_expr(pool, value, recipe, self_param, self_ty);
            }
        }
        HirStmtKind::If {
            condition,
            then_block,
            else_block,
        } => {
            rewrite_condition(pool, &condition, recipe, self_param, self_ty);
            rewrite_block(pool, then_block, recipe, self_param, self_ty);
            if let Some(else_block) = else_block {
                rewrite_block(pool, else_block, recipe, self_param, self_ty);
            }
        }
        HirStmtKind::While { condition, body } => {
            rewrite_condition(pool, &condition, recipe, self_param, self_ty);
            rewrite_block(pool, body, recipe, self_param, self_ty);
        }
        HirStmtKind::For { clause, body, .. } => {
            match &clause {
                arandu_middle::hir::HirForClause::In { iterable, .. } => {
                    rewrite_expr(pool, *iterable, recipe, self_param, self_ty);
                }
                arandu_middle::hir::HirForClause::CStyle {
                    init,
                    condition,
                    step,
                    ..
                } => {
                    if let Some(init) = init {
                        rewrite_simple(pool, init, recipe, self_param, self_ty);
                    }
                    if let Some(condition) = condition {
                        rewrite_expr(pool, *condition, recipe, self_param, self_ty);
                    }
                    if let Some(step) = step {
                        rewrite_simple(pool, step, recipe, self_param, self_ty);
                    }
                }
            }
            rewrite_block(pool, body, recipe, self_param, self_ty);
        }
        HirStmtKind::Match { value, arms } => {
            rewrite_expr(pool, value, recipe, self_param, self_ty);
            let arms = pool.match_arms_list(arms).to_vec();
            for arm in arms {
                if let Some(guard) = arm.guard {
                    rewrite_expr(pool, guard, recipe, self_param, self_ty);
                }
                match arm.body {
                    arandu_middle::hir::HirMatchArmBody::Expr(expr) => {
                        rewrite_expr(pool, expr, recipe, self_param, self_ty);
                    }
                    arandu_middle::hir::HirMatchArmBody::Block(block) => {
                        rewrite_block(pool, block, recipe, self_param, self_ty);
                    }
                }
            }
        }
        HirStmtKind::Defer(block)
        | HirStmtKind::ErrDefer(block)
        | HirStmtKind::Unsafe(block)
        | HirStmtKind::Scope(block) => rewrite_block(pool, block, recipe, self_param, self_ty),
        HirStmtKind::Break | HirStmtKind::Continue | HirStmtKind::Error => {}
    }
}

fn rewrite_simple(
    pool: &mut crate::hir::HirPool,
    stmt: &arandu_middle::hir::HirSimpleStmt,
    recipe: &ThreadCaptureRecipe,
    self_param: SymbolId,
    self_ty: TypeId,
) {
    match stmt {
        arandu_middle::hir::HirSimpleStmt::VarDecl { value, .. }
        | arandu_middle::hir::HirSimpleStmt::Set { value, .. }
        | arandu_middle::hir::HirSimpleStmt::Expr(value) => {
            rewrite_expr(pool, *value, recipe, self_param, self_ty);
        }
    }
}

fn rewrite_condition(
    pool: &mut crate::hir::HirPool,
    condition: &arandu_middle::hir::HirCondition,
    recipe: &ThreadCaptureRecipe,
    self_param: SymbolId,
    self_ty: TypeId,
) {
    match condition {
        arandu_middle::hir::HirCondition::Expr(expr)
        | arandu_middle::hir::HirCondition::Is { expr, .. } => {
            rewrite_expr(pool, *expr, recipe, self_param, self_ty);
        }
        arandu_middle::hir::HirCondition::And(parts) => {
            for part in parts.iter() {
                rewrite_condition(pool, part, recipe, self_param, self_ty);
            }
        }
    }
}

fn rewrite_expr(
    pool: &mut crate::hir::HirPool,
    id: HirExprId,
    recipe: &ThreadCaptureRecipe,
    self_param: SymbolId,
    self_ty: TypeId,
) {
    let kind = pool.expr(id).kind.clone();
    if let HirExprKind::Path { symbol } = &kind
        && let Some(field) = recipe.fields.iter().find(|field| field.source == *symbol)
    {
        let span = pool.expr(id).span;
        let base = pool.alloc_expr(HirExpr {
            kind: HirExprKind::Path { symbol: self_param },
            ty: self_ty,
            span,
        });
        let expr = pool.expr_mut(id);
        expr.kind = HirExprKind::Field {
            base,
            field: field.name.clone(),
        };
        expr.ty = field.ty;
        return;
    }
    match kind {
        HirExprKind::Generic { callee, .. }
        | HirExprKind::Field { base: callee, .. }
        | HirExprKind::SafeField { base: callee, .. }
        | HirExprKind::Try { expr: callee }
        | HirExprKind::ArrayRepeat { value: callee }
        | HirExprKind::Alloc { expr: callee }
        | HirExprKind::Unary { expr: callee, .. }
        | HirExprKind::Cast { expr: callee, .. }
        | HirExprKind::ToStr { value: callee } => {
            rewrite_expr(pool, callee, recipe, self_param, self_ty);
        }
        HirExprKind::Index { base, index }
        | HirExprKind::SafeIndex { base, index }
        | HirExprKind::NullCoalesce {
            left: base,
            right: index,
        }
        | HirExprKind::Binary {
            left: base,
            right: index,
            ..
        } => {
            rewrite_expr(pool, base, recipe, self_param, self_ty);
            rewrite_expr(pool, index, recipe, self_param, self_ty);
        }
        HirExprKind::Call {
            callee,
            args,
            trailing_block,
        } => {
            rewrite_expr(pool, callee, recipe, self_param, self_ty);
            for arg in expr_ids(pool, args) {
                rewrite_expr(pool, arg, recipe, self_param, self_ty);
            }
            if let Some(block) = trailing_block {
                rewrite_block(pool, block, recipe, self_param, self_ty);
            }
        }
        HirExprKind::ResultCtor { value, .. } => {
            rewrite_expr(pool, value, recipe, self_param, self_ty);
        }
        HirExprKind::StructLiteral { fields, .. } => {
            let values = pool
                .field_inits_list(fields)
                .iter()
                .map(|field| field.value)
                .collect::<Vec<_>>();
            for value in values {
                rewrite_expr(pool, value, recipe, self_param, self_ty);
            }
        }
        HirExprKind::Array { items } | HirExprKind::Tuple { items } => {
            for item in expr_ids(pool, items) {
                rewrite_expr(pool, item, recipe, self_param, self_ty);
            }
        }
        HirExprKind::Lambda { body, .. } => match body {
            arandu_middle::hir::HirLambdaBody::Expr(expr) => {
                rewrite_expr(pool, expr, recipe, self_param, self_ty);
            }
            arandu_middle::hir::HirLambdaBody::Block(block) => {
                rewrite_block(pool, block, recipe, self_param, self_ty);
            }
        },
        HirExprKind::AsyncBlock { block }
        | HirExprKind::ValueBlock { block }
        | HirExprKind::UnsafeBlock { block } => {
            rewrite_block(pool, block, recipe, self_param, self_ty);
        }
        HirExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            rewrite_condition(pool, &condition, recipe, self_param, self_ty);
            rewrite_block(pool, then_block, recipe, self_param, self_ty);
            rewrite_block(pool, else_block, recipe, self_param, self_ty);
        }
        HirExprKind::Match { value, arms } => {
            rewrite_expr(pool, value, recipe, self_param, self_ty);
            let arms = pool.match_arms_list(arms).to_vec();
            for arm in arms {
                if let Some(guard) = arm.guard {
                    rewrite_expr(pool, guard, recipe, self_param, self_ty);
                }
                match arm.body {
                    arandu_middle::hir::HirMatchArmBody::Expr(expr) => {
                        rewrite_expr(pool, expr, recipe, self_param, self_ty);
                    }
                    arandu_middle::hir::HirMatchArmBody::Block(block) => {
                        rewrite_block(pool, block, recipe, self_param, self_ty);
                    }
                }
            }
        }
        HirExprKind::Catch { expr, handler } => {
            rewrite_expr(pool, expr, recipe, self_param, self_ty);
            match handler {
                arandu_middle::hir::HirCatchHandler::Expr(expr) => {
                    rewrite_expr(pool, expr, recipe, self_param, self_ty);
                }
                arandu_middle::hir::HirCatchHandler::Block { block, .. } => {
                    rewrite_block(pool, block, recipe, self_param, self_ty);
                }
            }
        }
        HirExprKind::StringInterp { parts } => {
            for part in parts {
                if let arandu_middle::hir::HirStringPart::Expr(expr) = part {
                    rewrite_expr(pool, expr, recipe, self_param, self_ty);
                }
            }
        }
        HirExprKind::Layout { .. }
        | HirExprKind::FloatBits(_)
        | HirExprKind::FrozenBytes(_)
        | HirExprKind::Path { .. }
        | HirExprKind::TypePath { .. }
        | HirExprKind::Int(_)
        | HirExprKind::Float(_)
        | HirExprKind::Bool(_)
        | HirExprKind::Char(_)
        | HirExprKind::Str(_)
        | HirExprKind::Nil
        | HirExprKind::Error => {}
    }
}

fn expr_ids(pool: &crate::hir::HirPool, range: IndexRange) -> Vec<HirExprId> {
    pool.expr_list(range).to_vec()
}
