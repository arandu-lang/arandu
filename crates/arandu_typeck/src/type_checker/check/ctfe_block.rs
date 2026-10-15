//! Initial typing of an isolated evaluation block. Shared by public comptime
//! obligations and internal root selectors; evaluation belongs to the AMIR VM.

use arandu_parser::{Block, Stmt, ast_pool::ExprId};

use super::super::types::{ArType, TypeId};
use super::super::{TypeChecker, constraints::ConstraintOrigin, context::ReturnValue};

#[derive(Debug, Clone, Copy)]
pub struct CtfeBlockType {
    pub return_type: TypeId,
    /// Initial effects of this root, not effects of its runtime owner. VM
    /// admission still checks the actual retained operations/call closure.
    pub observed_effects: arandu_middle::EffectFlags,
    /// A semicolon discards the expression's value even with an expected type.
    pub value_tail: Option<ExprId>,
}

/// Check only this block against its own return target, borrowing the canonical
/// AST pool. The caller supplies resolved declarations/bindings and later lowers
/// the typed block to AMIR; no final runtime query may be requested from here.
///
/// All explicit returns and an unterminated final expression participate in one
/// type, including syntactically present but unreachable exits/tails. Functions
/// nested during checking get their own return targets as usual.
#[must_use]
pub fn check_ctfe_block(
    checker: &mut TypeChecker<'_>,
    block: &Block,
    expected: Option<TypeId>,
) -> CtfeBlockType {
    let outer_effects = checker.current_observed_effects;
    checker.current_observed_effects = arandu_middle::EffectFlags::NONE;
    checker.ctx.push_ctfe_return(expected, block.span);
    let statements = checker.pool.stmt_list(block.statements);
    let value_tail = statements
        .last()
        .and_then(|&id| match checker.pool.stmt(id) {
            Stmt::Expr {
                expr,
                has_semi: false,
                ..
            } => Some(*expr),
            _ => None,
        });
    let mut tail = None;
    for (index, &id) in statements.iter().enumerate() {
        if index + 1 == statements.len()
            && let Some(expression) = value_tail
        {
            let ty = super::super::synth::synth_expr_expected(checker, expression, expected);
            tail = Some(ReturnValue {
                ty,
                span: checker.pool.expr_span(expression),
                expression: Some(expression),
            });
        } else {
            super::check_stmt(checker, checker.pool, checker.pool.stmt(id));
        }
    }
    let observed_effects = checker.current_observed_effects;
    checker.current_observed_effects = outer_effects;
    let Some((mut returns, propagations)) = checker.ctx.pop_ctfe_return() else {
        checker.diagnostics.push(crate::Diagnostic::ice(
            crate::DiagCode::ICET001,
            "isolated block lost its return target during type checking",
            block.span,
        ));
        return CtfeBlockType {
            return_type: checker.intern(ArType::Error),
            observed_effects,
            value_tail,
        };
    };
    if let Some(tail) = tail {
        returns.push(tail);
    }
    // Prefer a concrete type over an unsuffixed numeric literal. The canonical
    // literal solver subsequently verifies that every literal fits this type.
    let inferred = returns
        .iter()
        .find(|value| {
            let ty = checker.resolve(value.ty);
            !ty.is_literal() && !ty.is_error()
        })
        .or_else(|| returns.first())
        .map(|value| checker.resolve(value.ty).default_literal())
        .unwrap_or(ArType::Void);
    let result = expected.unwrap_or_else(|| checker.intern(inferred));
    for propagation in propagations {
        if !checker.can_propagate_try(&checker.resolve(propagation.ty), &checker.resolve(result)) {
            checker.add_constraint(
                ArType::Error,
                result,
                ConstraintOrigin::TryReturnInvalid {
                    span: propagation.span,
                    return_span: block.span,
                },
            );
        }
    }
    if returns.is_empty() && !matches!(checker.resolve(result), ArType::Void | ArType::Error) {
        checker.add_constraint(
            result,
            ArType::Void,
            ConstraintOrigin::CtfeResult {
                value_span: block.span,
                block_span: block.span,
            },
        );
    }
    for value in returns {
        if let Some(expression) = value.expression
            && let Some(variable) = checker.literal_table.var_for_expr(expression)
            && checker.resolve(result).is_numeric()
        {
            checker.constrain_literal_var(
                variable,
                result,
                ConstraintOrigin::CtfeResult {
                    value_span: value.span,
                    block_span: block.span,
                },
            );
        }
        if !checker.unify_ids(result, value.ty) {
            checker.add_constraint(
                result,
                value.ty,
                ConstraintOrigin::CtfeResult {
                    value_span: value.span,
                    block_span: block.span,
                },
            );
        }
    }
    CtfeBlockType {
        return_type: result,
        observed_effects,
        value_tail,
    }
}
