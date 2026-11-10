//! Lower one synthesized closure method body (`run` or `call`).
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
use arandu_middle::hir::HirCaptureAlias;
use arandu_middle::hir::{HirBlockId, HirFieldPattern, HirMatchArm, HirMatchArmBody, HirPattern};
use arandu_middle::ops::UnaryOp;
use arandu_middle::types::TypeId;
use arandu_middle::types::TypeInterner;
use arandu_parser::Program;
use arandu_parser::ast_pool::{ExprId, ExprKind};
use arandu_typeck::type_checker::info::{CaptureMode, ClosureRecipe};

pub fn lower_closure_hir(
    type_check: &mut TypeCheckResult,
    program: &Program,
    method_symbol: SymbolId,
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
        .closure_for_method(method_symbol)
        .cloned()
    else {
        return Err(vec![Diagnostic::ice(
            DiagCode::ICEL001,
            "closure recipe is missing for the synthesized method",
            arandu_lexer::Span::new(method_symbol.file_id, 0, 0),
        )]);
    };
    let mut pool = crate::hir::HirPool::new();
    // A callback body reaches its captures through `mut self`; a job
    // destructures its owned `self` once instead.
    let self_ty = type_check
        .type_info
        .decl_type_id(recipe.self_param)
        .unwrap_or_else(TypeInterner::preinterned_error_id);
    if recipe.is_callback() {
        pool.capture_aliases = recipe
            .fields
            .iter()
            .map(|field| {
                (
                    field.source,
                    HirCaptureAlias {
                        env: recipe.self_param,
                        env_ty: self_ty,
                        field: field.name.clone(),
                        field_symbol: field.symbol,
                        field_ty: field.field_ty,
                        through_ref: matches!(
                            field.mode,
                            CaptureMode::Borrow | CaptureMode::BorrowMut
                        ),
                    },
                )
            })
            .collect();
    }
    let struct_ty = type_check
        .type_info
        .decl_type_id(recipe.struct_symbol)
        .unwrap_or_else(|| {
            let interner = &type_check.type_info.type_interner;
            let named = arandu_middle::types::ArType::named(recipe.struct_symbol, &[], interner);
            type_check.type_info_mut().type_interner.intern(named)
        });
    let body_block = if let Some(function) = recipe.adapts {
        adapter_body(&mut pool, type_check, &recipe, function)
    } else {
        let Some(lambda) = find_lambda(program, recipe.lambda_span) else {
            return Err(vec![Diagnostic::ice(
                DiagCode::ICEL001,
                "closure lambda is missing from the source file",
                recipe.lambda_span,
            )]);
        };
        let body = match program.pool.expr(lambda) {
            ExprKind::Lambda { body, .. } => body.clone(),
            _ => {
                return Err(vec![Diagnostic::ice(
                    DiagCode::ICEL001,
                    "closure span is not a lambda",
                    recipe.lambda_span,
                )]);
            }
        };
        match &body {
            arandu_parser::LambdaBody::Expr { expr, .. } => {
                let value = super::expr::lower_expr(type_check, &program.pool, &mut pool, *expr)
                    .map_err(|diagnostic| vec![diagnostic])?;
                let span = program.pool.expr_span(*expr);
                let returns_void = matches!(
                    type_check
                        .type_info
                        .type_interner
                        .resolve(recipe.return_type),
                    arandu_middle::types::ArType::Void
                );
                let kind = if returns_void {
                    HirStmtKind::Expr(value)
                } else {
                    HirStmtKind::Return {
                        values: pool.alloc_expr_list(&[value]),
                    }
                };
                let ret = pool.alloc_stmt(HirStmt { kind, span });
                let statements = pool.alloc_stmt_list(&[ret]);
                pool.alloc_block(HirBlock { statements, span })
            }
            arandu_parser::LambdaBody::Block { block, .. } => {
                let lowered = super::stmt::lower_block(type_check, &program.pool, &mut pool, block)
                    .map_err(|diagnostic| vec![diagnostic])?;
                return_value_tail(&mut pool, lowered, &program.pool, block);
                lowered
            }
        }
    };
    let body_block = if recipe.is_callback() {
        pool.capture_aliases.clear();
        body_block
    } else {
        destructure_captures(&mut pool, &recipe, struct_ty, body_block, type_check)
    };

    let mut params = Vec::with_capacity(recipe.params.len() + 1);
    params.push(HirParam {
        symbol: recipe.self_param,
        ty: self_ty,
        span: recipe.lambda_span,
        is_receiver: true,
        receiver_kind: Some(if recipe.is_callback() {
            ReceiverKind::Mut
        } else {
            ReceiverKind::Own
        }),
    });
    params.extend(recipe.params.iter().map(|param| HirParam {
        symbol: param.symbol,
        ty: param.ty,
        span: recipe.lambda_span,
        is_receiver: false,
        receiver_kind: None,
    }));
    let params = pool.alloc_param_list(&params);
    let func = pool.alloc_decl(HirDecl::Func(HirFunc {
        symbol: recipe.method_symbol,
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
                ty: field.field_ty,
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

/// The environment at the call site: each capture by value, or a `ref`/
/// `mut ref` to the caller's local for a borrow. Inside another closure body
/// the caller's local is itself a capture, reached through that alias.
pub(crate) fn capture_struct_literal(
    hir_pool: &mut crate::hir::HirPool,
    recipe: &ClosureRecipe,
) -> HirExprKind {
    let inits = recipe
        .fields
        .iter()
        .map(|field| {
            let kind = super::expr::captured_value(hir_pool, field.source, field.span);
            let mut value = hir_pool.alloc_expr(HirExpr {
                kind,
                ty: field.ty,
                span: field.span,
            });
            let borrow = match field.mode {
                CaptureMode::Borrow => Some(UnaryOp::Ref),
                CaptureMode::BorrowMut => Some(UnaryOp::RefMut),
                CaptureMode::Copy | CaptureMode::Move => None,
            };
            if let Some(op) = borrow {
                value = hir_pool.alloc_expr(HirExpr {
                    kind: HirExprKind::Unary { op, expr: value },
                    ty: field.field_ty,
                    span: field.span,
                });
            }
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
    recipe: &ClosureRecipe,
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

/// `call` of a named-function adapter: forward every parameter to the
/// function and return its result.
fn adapter_body(
    pool: &mut crate::hir::HirPool,
    type_check: &TypeCheckResult,
    recipe: &ClosureRecipe,
    function: SymbolId,
) -> HirBlockId {
    let span = recipe.lambda_span;
    let function_ty = type_check
        .type_info
        .decl_type_id(function)
        .unwrap_or_else(TypeInterner::preinterned_error_id);
    let callee = pool.alloc_expr(HirExpr {
        kind: HirExprKind::Path { symbol: function },
        ty: function_ty,
        span,
    });
    let args = recipe
        .params
        .iter()
        .map(|param| {
            pool.alloc_expr(HirExpr {
                kind: HirExprKind::Path {
                    symbol: param.symbol,
                },
                ty: param.ty,
                span,
            })
        })
        .collect::<Vec<_>>();
    let args = pool.alloc_expr_list(&args);
    let call = pool.alloc_expr(HirExpr {
        kind: HirExprKind::Call {
            callee,
            args,
            trailing_block: None,
        },
        ty: recipe.return_type,
        span,
    });
    let returns_void = matches!(
        type_check
            .type_info
            .type_interner
            .resolve(recipe.return_type),
        arandu_middle::types::ArType::Void
    );
    let kind = if returns_void {
        HirStmtKind::Expr(call)
    } else {
        HirStmtKind::Return {
            values: pool.alloc_expr_list(&[call]),
        }
    };
    let statement = pool.alloc_stmt(HirStmt { kind, span });
    let statements = pool.alloc_stmt_list(&[statement]);
    pool.alloc_block(HirBlock { statements, span })
}
