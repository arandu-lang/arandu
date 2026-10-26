use arandu_parser::ast_pool::ExprKind;

use super::super::super::TypeChecker;
use super::super::super::constraints::ConstraintOrigin;
use super::super::super::types::{ArType, TypeId};

pub(super) fn check_var_decl_stmt(
    checker: &mut TypeChecker<'_>,
    bindings: &[arandu_parser::BindingItem],
    value: arandu_parser::ast_pool::ExprId,
) {
    // T2.2: when a single binding has a type annotation, pass it as expected type
    // so `.Ok` / `.None` sugar can resolve.
    let expected = if bindings.len() == 1
        && let Some(binding) = bindings.first()
        && let Some(ty_expr) = &binding.ty
    {
        let expected_ty = checker.lower_type_expr(*ty_expr, checker.type_scope());
        if !expected_ty.is_error() {
            Some(checker.intern(expected_ty))
        } else {
            None
        }
    } else {
        None
    };
    let val_ty_id = super::super::super::synth::synth_expr_expected(checker, value, expected);

    if bindings.len() > 1 {
        check_multi_var_decl(checker, bindings, value, val_ty_id);
    } else if let Some(binding) = bindings.first() {
        check_single_var_decl(checker, binding, value, val_ty_id);
    }
}

fn check_multi_var_decl(
    checker: &mut TypeChecker<'_>,
    bindings: &[arandu_parser::BindingItem],
    value: arandu_parser::ast_pool::ExprId,
    val_ty_id: TypeId,
) {
    // Root cause fix (RC-ERR-NIL): Go-style `let ok, err = f()` where `f` returns
    // `Result<T, E>` binds `err` as `E?` (nullable error), so `err != nil` typechecks.
    let val_tys: Vec<TypeId> = if let Some((ok_id, err_id)) = checker.result_ok_err_ids(val_ty_id) {
        let err_nullable = checker.intern(ArType::Nullable(err_id));
        vec![ok_id, err_nullable]
    } else {
        match checker.resolve(val_ty_id) {
            ArType::Tuple(tys) => checker.type_info.type_interner.type_args(tys),
            ArType::Error => vec![checker.intern(ArType::Error); bindings.len()],
            _ => vec![val_ty_id; bindings.len()],
        }
    };

    for (i, binding) in bindings.iter().enumerate() {
        let binding_key = crate::NodeKey::from(binding.span);
        if let Some(symbol_id) = checker.resolved.definitions.get(&binding_key).copied() {
            let elem_ty_id = val_tys
                .get(i)
                .copied()
                .unwrap_or_else(|| checker.intern(ArType::Error));
            let mut bind_ty_id = elem_ty_id;

            if let Some(ty_expr) = &binding.ty {
                let expected = checker.lower_type_expr(*ty_expr, checker.type_scope());
                let elem_ty = checker.resolve(elem_ty_id);

                super::set::apply_assignment_constraints(
                    checker,
                    &expected,
                    &elem_ty,
                    binding.span,
                    checker.pool.expr_span(value),
                );
                bind_ty_id = checker.intern(expected);
            }

            checker.ctx.bind(symbol_id, bind_ty_id);
            checker.record_decl_type(symbol_id, bind_ty_id);
        }
    }
}

fn check_single_var_decl(
    checker: &mut TypeChecker<'_>,
    binding: &arandu_parser::BindingItem,
    value: arandu_parser::ast_pool::ExprId,
    val_ty_id: TypeId,
) {
    let binding_key = crate::NodeKey::from(binding.span);
    if let Some(symbol_id) = checker.resolved.definitions.get(&binding_key).copied() {
        let mut bind_ty_id = val_ty_id;

        if matches!(checker.pool.expr(value), ExprKind::Nil)
            && let Some(ty_expr) = &binding.ty
        {
            let expected = checker.lower_type_expr(*ty_expr, checker.type_scope());
            if !expected.is_error() {
                let expected_id = checker.intern(expected);
                checker.type_info.record_expr_type(value, expected_id);
                bind_ty_id = expected_id;
            }
        }

        let annotated_as_result = binding.ty.as_ref().is_some_and(|ty_expr| {
            let expected = checker.lower_type_expr(*ty_expr, checker.type_scope());
            checker.result_ok_err(&expected).is_some()
        });

        let val_ty = checker.resolve(val_ty_id);
        // Result must be handled via `?`, match, or multi-binding `let ok, err = …`.
        // A single binding of Result is a warning (not a hard error).
        if checker.result_ok_err(&val_ty).is_some() && !annotated_as_result {
            checker.diagnostics.push(crate::Diagnostic::warning(
                crate::DiagCode::W006UnhandledResult,
                "Result value must be handled with `?` or `value, err = f()`",
                checker.pool.expr_span(value),
            ));
        }

        if let Some(ty_expr) = &binding.ty {
            let expected = checker.lower_type_expr(*ty_expr, checker.type_scope());
            let bind_ty = checker.resolve(bind_ty_id);

            if let Some(var_id) = checker.literal_table.var_for_expr(value) {
                let exp_id = checker.intern(expected.clone());
                checker.constrain_literal_var(
                    var_id,
                    exp_id,
                    ConstraintOrigin::Assignment {
                        lhs_span: binding.span,
                        rhs_span: checker.pool.expr_span(value),
                    },
                );
            }

            super::set::apply_assignment_constraints(
                checker,
                &expected,
                &bind_ty,
                binding.span,
                checker.pool.expr_span(value),
            );
            bind_ty_id = checker.intern(expected);
        } else if let Some(var_id) = checker.literal_table.var_for_expr(value) {
            checker.literal_table.bind_symbol(symbol_id, var_id);
        }

        checker.ctx.bind(symbol_id, bind_ty_id);
        checker.record_decl_type(symbol_id, bind_ty_id);
    }
}
