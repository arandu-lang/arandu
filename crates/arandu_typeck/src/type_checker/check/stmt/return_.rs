use super::super::super::TypeChecker;
use super::super::super::constraints::ConstraintOrigin;
use super::super::super::types::{ArType, TypeId};

pub(super) fn check_return_stmt(
    checker: &mut TypeChecker<'_>,
    span: arandu_base::Span,
    values: &[arandu_parser::ast_pool::ExprId],
) {
    if checker.ctx.is_ctfe_return() {
        // Do not default an inferred CTFE target to the enclosing function's
        // return type (or to void). All exits are unified with the root tail.
        let expected = checker.ctx.current_return();
        let ty = match values {
            [] => checker.intern(ArType::Void),
            [value] => super::super::super::synth::synth_expr_expected(checker, *value, expected),
            values => {
                let types = values
                    .iter()
                    .map(|&value| super::super::super::synth::synth_expr(checker, value))
                    .collect::<Vec<_>>();
                checker.intern(ArType::tuple(&types, &checker.type_info.type_interner))
            }
        };
        checker
            .ctx
            .record_ctfe_return(super::super::super::context::ReturnValue {
                ty,
                span,
                expression: if values.len() == 1 {
                    values.first().copied()
                } else {
                    None
                },
            });
        return;
    }
    let current_ret_id = checker
        .ctx
        .current_return()
        .unwrap_or_else(|| checker.intern(ArType::Void));
    let current_ret = checker.resolve(current_ret_id);

    let val_ty_id = if values.is_empty() {
        checker.intern(ArType::Void)
    } else if values.len() == 1 {
        // T2.2: return type is the expected context for `.Ok` / `.None` sugar.
        super::super::super::synth::synth_expr_expected(checker, values[0], Some(current_ret_id))
    } else {
        let tys_vec: Vec<TypeId> = values
            .iter()
            .map(|v| super::super::super::synth::synth_expr(checker, *v))
            .collect();
        checker.intern(ArType::tuple(&tys_vec, &checker.type_info.type_interner))
    };
    let val_ty = checker.resolve(val_ty_id);

    if values.len() == 1
        && let Some(var_id) = checker.literal_table.var_for_expr(values[0])
        && !current_ret.is_literal()
        && !current_ret.is_error()
    {
        checker.constrain_literal_var(
            var_id,
            current_ret_id,
            ConstraintOrigin::ReturnType {
                return_span: span,
                declared_span: checker.ctx.current_return_decl_span().unwrap_or(span),
            },
        );
    }

    if !checker.is_assignable_return_type(&current_ret, &val_ty) {
        checker.add_subtype_constraint(
            current_ret,
            val_ty,
            ConstraintOrigin::ReturnType {
                return_span: span,
                declared_span: checker.ctx.current_return_decl_span().unwrap_or(span),
            },
        );
    } else if !val_ty.is_literal()
        && val_ty.default_literal() != current_ret.default_literal()
        && !(val_ty.is_u8_or_byte() && current_ret.is_u8_or_byte())
        && current_ret.is_numeric()
        && val_ty.is_numeric()
    {
        checker.add_constraint(
            current_ret,
            val_ty,
            ConstraintOrigin::ImplicitWidening {
                source_span: values.first().map_or(span, |v| checker.pool.expr_span(*v)),
                target_span: span,
            },
        );
    }
}
