use arandu_parser::Stmt;
use arandu_parser::ast_pool::AstPool;
use arandu_parser::ast_pool::ExprKind;

use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::{ArType, Primitive, TypeId};
use super::block::check_block;
use super::condition::check_condition;
use super::place::synth_place;

/// Roteador principal de checagem de tipo de instruções.
#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker, pool, stmt))]
pub fn check_stmt(checker: &mut TypeChecker<'_>, pool: &AstPool, stmt: &Stmt) {
    match stmt {
        Stmt::VarDecl {
            span: _,
            bindings,
            value,
        } => {
            check_var_decl_stmt(checker, bindings, *value);
        }
        Stmt::Set {
            span: _,
            places,
            op: _,
            value,
        } => {
            check_set_stmt(checker, places, *value);
        }
        Stmt::Return { span, values } => {
            check_return_stmt(checker, *span, values);
        }
        Stmt::Expr { expr, .. } => {
            super::super::synth::synth_expr(checker, *expr);
        }
        Stmt::If {
            span: _,
            condition,
            then_block,
            else_block,
        } => {
            check_if_stmt(checker, pool, condition, then_block, else_block.as_ref());
        }
        Stmt::While {
            span: _,
            condition,
            body,
        } => {
            check_while_stmt(checker, pool, condition, body);
        }
        Stmt::For {
            span: _,
            clause,
            body,
        } => {
            check_for_stmt(checker, pool, clause, body);
        }
        Stmt::Match { span: _, expr } => {
            super::super::synth::synth_expr(checker, *expr);
        }
        Stmt::Free { span, expr } => {
            check_free_stmt(checker, *span, *expr);
        }
        Stmt::Defer { span: _, body } | Stmt::ErrDefer { span: _, body } => {
            check_defer_stmt(checker, pool, body);
        }
        Stmt::Break { span } | Stmt::Continue { span } => {
            check_loop_control_stmt(checker, stmt, *span);
        }
        Stmt::Unsafe { span: _, block } => {
            checker.ctx.enter_unsafe();
            check_block(checker, pool, block);
            checker.ctx.exit_unsafe();
        }
        Stmt::Error(_) => {}
    }
}
fn check_var_decl_stmt(
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
    let val_ty_id = super::super::synth::synth_expr_expected(checker, value, expected);

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

                apply_assignment_constraints(
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

            apply_assignment_constraints(
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

fn check_set_stmt(
    checker: &mut TypeChecker<'_>,
    places: &[arandu_parser::Place],
    value: arandu_parser::ast_pool::ExprId,
) {
    for place in places {
        validate_mutability(checker, place);
    }
    let expected = if places.len() == 1 {
        places.first().map(|p| synth_place(checker, p))
    } else {
        None
    };
    let val_ty_id = super::super::synth::synth_expr_expected(checker, value, expected);

    if places.len() > 1 {
        let val_tys: Vec<TypeId> =
            if let Some((ok_id, err_id)) = checker.result_ok_err_ids(val_ty_id) {
                vec![ok_id, err_id]
            } else {
                match checker.resolve(val_ty_id) {
                    ArType::Tuple(tys) => checker.type_info.type_interner.type_args(tys),
                    ArType::Error => vec![checker.intern(ArType::Error); places.len()],
                    _ => vec![val_ty_id; places.len()],
                }
            };

        for (i, place) in places.iter().enumerate() {
            let expected_ty_id = synth_place(checker, place);
            let elem_ty_id = val_tys
                .get(i)
                .copied()
                .unwrap_or_else(|| checker.intern(ArType::Error));
            apply_set_constraints(
                checker,
                expected_ty_id,
                elem_ty_id,
                place.span,
                checker.pool.expr_span(value),
            );
        }
    } else if let Some(place) = places.first() {
        // The single-place synthesis already ran above (used as `expected` for
        // the value); reuse it instead of synthesizing the place twice, which
        // duplicated T006/T017/T018 diagnostics and re-synthesized the index
        // sub-expression.
        let expected_ty_id = expected.unwrap_or_else(|| checker.intern(ArType::Error));
        let mut final_val_ty_id = val_ty_id;

        if matches!(checker.pool.expr(value), arandu_parser::ExprKind::Nil)
            && !checker.resolve(expected_ty_id).is_error()
        {
            checker.type_info.record_expr_type(value, expected_ty_id);
            final_val_ty_id = expected_ty_id;
        }

        let is_result = checker
            .result_ok_err(&checker.resolve(final_val_ty_id))
            .is_some();
        if is_result
            && checker
                .result_ok_err(&checker.resolve(expected_ty_id))
                .is_none()
        {
            checker.diagnostics.push(crate::Diagnostic::warning(
                crate::DiagCode::W006UnhandledResult,
                "Result value must be handled with `?` or `value, err = f()`",
                checker.pool.expr_span(value),
            ));
        }

        if let Some(var_id) = checker.literal_table.var_for_expr(value) {
            let exp_ty = checker.resolve(expected_ty_id);
            if !exp_ty.is_literal() && !exp_ty.is_error() {
                checker.constrain_literal_var(
                    var_id,
                    expected_ty_id,
                    ConstraintOrigin::SetTarget {
                        place_span: place.span,
                        value_span: checker.pool.expr_span(value),
                    },
                );
            }
        }

        apply_set_constraints(
            checker,
            expected_ty_id,
            final_val_ty_id,
            place.span,
            checker.pool.expr_span(value),
        );
    }
}

fn check_return_stmt(
    checker: &mut TypeChecker<'_>,
    span: arandu_base::Span,
    values: &[arandu_parser::ast_pool::ExprId],
) {
    let current_ret_id = checker
        .ctx
        .current_return()
        .unwrap_or_else(|| checker.intern(ArType::Void));
    let current_ret = checker.resolve(current_ret_id);

    let val_ty_id = if values.is_empty() {
        checker.intern(ArType::Void)
    } else if values.len() == 1 {
        // T2.2: return type is the expected context for `.Ok` / `.None` sugar.
        super::super::synth::synth_expr_expected(checker, values[0], Some(current_ret_id))
    } else {
        let tys_vec: Vec<TypeId> = values
            .iter()
            .map(|v| super::super::synth::synth_expr(checker, *v))
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

fn check_if_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    condition: &arandu_parser::Condition,
    then_block: &arandu_parser::Block,
    else_block: Option<&arandu_parser::Block>,
) {
    check_condition(checker, condition);
    check_block(checker, pool, then_block);
    if let Some(eb) = else_block {
        check_block(checker, pool, eb);
    }
}

fn check_while_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    condition: &arandu_parser::Condition,
    body: &arandu_parser::Block,
) {
    check_condition(checker, condition);
    checker.ctx.enter_loop();
    check_block(checker, pool, body);
    checker.ctx.exit_loop();
}

fn check_for_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    clause: &arandu_parser::ForClause,
    body: &arandu_parser::Block,
) {
    match clause {
        arandu_parser::ForClause::In {
            span: _,
            bindings,
            iterable,
        } => {
            let iterable_ty_id = super::super::synth::synth_expr(checker, *iterable);
            let iterable_ty = checker.resolve(iterable_ty_id);
            let elem_ty = match &iterable_ty {
                ArType::Array(_, inner) | ArType::Slice(inner) | ArType::Range(inner) => {
                    checker.type_info.type_interner.resolve(*inner)
                }
                ArType::Error => ArType::Error,
                _ => {
                    checker.diagnostics.push(crate::Diagnostic::error(
                        crate::DiagCode::T005OperatorNotApplicable,
                        format!(
                            "expected array, slice, or range, found '{}'",
                            iterable_ty.display(&checker.symbols, &checker.type_info.type_interner)
                        ),
                        checker.pool.expr_span(*iterable),
                    ));
                    ArType::Error
                }
            };
            if let Some(binding) = bindings.first() {
                let binding_key = crate::NodeKey::from(binding.span);
                if let Some(symbol_id) = checker.resolved.definitions.get(&binding_key).copied() {
                    let elem_ty_id = checker.intern(elem_ty);
                    checker.ctx.bind(symbol_id, elem_ty_id);
                    checker.record_decl_type(symbol_id, elem_ty_id);
                }
            }
        }
        arandu_parser::ForClause::CStyle {
            init,
            condition,
            step,
            ..
        } => {
            if let Some(init_stmt) = init {
                check_simple_stmt(checker, pool, init_stmt);
            }
            if let Some(cond_expr) = condition {
                let cond_ty_id = super::super::synth::synth_expr(checker, *cond_expr);
                let cond_ty = checker.resolve(cond_ty_id);
                if !cond_ty.is_error()
                    && !super::super::types::unify(
                        &cond_ty,
                        &ArType::Primitive(Primitive::Bool),
                        &checker.type_info.type_interner,
                    )
                {
                    checker.add_constraint(
                        ArType::Primitive(Primitive::Bool),
                        cond_ty,
                        ConstraintOrigin::Condition {
                            span: checker.pool.expr_span(*cond_expr),
                        },
                    );
                }
            }
            if let Some(step_stmt) = step {
                check_simple_stmt(checker, pool, step_stmt);
            }
        }
    }
    checker.ctx.enter_loop();
    check_block(checker, pool, body);
    checker.ctx.exit_loop();
}

fn check_simple_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    stmt: &arandu_parser::SimpleStmt,
) {
    match stmt {
        arandu_parser::SimpleStmt::VarDecl {
            span,
            bindings,
            value,
        } => {
            check_stmt(
                checker,
                pool,
                &Stmt::VarDecl {
                    span: *span,
                    bindings: bindings.clone(),
                    value: *value,
                },
            );
        }
        arandu_parser::SimpleStmt::Set {
            span,
            places,
            op,
            value,
        } => {
            check_stmt(
                checker,
                pool,
                &Stmt::Set {
                    span: *span,
                    places: places.clone(),
                    op: op.clone(),
                    value: *value,
                },
            );
        }
        arandu_parser::SimpleStmt::Expr { expr, span: _ } => {
            super::super::synth::synth_expr(checker, *expr);
        }
    }
}

fn check_free_stmt(
    checker: &mut TypeChecker<'_>,
    span: arandu_base::Span,
    expr: arandu_parser::ast_pool::ExprId,
) {
    checker.current_observed_effects = checker
        .current_observed_effects
        .union(arandu_middle::EffectFlags::HEAP);
    if !checker.ctx.is_in_unsafe() {
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::O014FreeRequiresUnsafe,
                "`free` requires an `unsafe` block",
                span,
            )
            .with_label(
                span,
                "`free` is unsafe and must be inside an `unsafe` block",
            ),
        );
    }
    let ty_id = super::super::synth::synth_expr(checker, expr);
    let ty = checker.resolve(ty_id);
    if !ty.is_error() && !matches!(ty, ArType::Ptr(_)) {
        let interner = &checker.type_info.type_interner;
        checker.diagnostics.push(
            crate::Diagnostic::error(
                crate::DiagCode::O011FreeRequiresPtr,
                format!(
                    "`free` requires a pointer type (`ptr[T]`), found '{}'",
                    ty.display(&checker.symbols, interner)
                ),
                span,
            )
            .with_label(
                checker.pool.expr_span(expr),
                format!(
                    "expression has type '{}'",
                    ty.display(&checker.symbols, interner)
                ),
            ),
        );
    }
}

fn check_defer_stmt(
    checker: &mut TypeChecker<'_>,
    pool: &AstPool,
    body: &arandu_parser::DeferBody,
) {
    match body {
        arandu_parser::DeferBody::Block { block, .. } => {
            check_block(checker, pool, block);
        }
        arandu_parser::DeferBody::Expr { expr, .. } => {
            super::super::synth::synth_expr(checker, *expr);
        }
    }
}

fn check_loop_control_stmt(checker: &mut TypeChecker<'_>, stmt: &Stmt, span: arandu_base::Span) {
    if !checker.ctx.is_in_loop() {
        let msg = match stmt {
            Stmt::Break { .. } => "`break` is only allowed inside a loop",
            Stmt::Continue { .. } => "`continue` is only allowed inside a loop",
            // Defensive invariant: only Break/Continue reach this path. A new
            // loop-control statement added upstream must surface as a reportable
            // ICE instead of panicking the compiler out of the host process.
            _ => {
                checker.diagnostics.push(crate::Diagnostic::ice(
                    crate::DiagCode::ICET001,
                    "loop-control checker reached an unexpected statement kind",
                    span,
                ));
                return;
            }
        };
        checker.diagnostics.push(crate::Diagnostic::error(
            crate::DiagCode::N011BreakContinueOutsideLoop,
            msg,
            span,
        ));
    }
}

fn apply_assignment_constraints(
    checker: &mut TypeChecker<'_>,
    expected: &ArType,
    actual: &ArType,
    lhs_span: arandu_base::Span,
    rhs_span: arandu_base::Span,
) {
    if !actual.is_literal()
        && actual.default_literal() != expected.default_literal()
        && expected.is_numeric()
        && actual.is_numeric()
    {
        checker.add_constraint(
            expected.clone(),
            actual.clone(),
            ConstraintOrigin::ImplicitWidening {
                source_span: rhs_span,
                target_span: lhs_span,
            },
        );
    } else if !super::super::types::is_assignable(
        actual,
        expected,
        &checker.type_info.type_interner,
    ) {
        checker.add_subtype_constraint(
            expected.clone(),
            actual.clone(),
            ConstraintOrigin::Assignment { lhs_span, rhs_span },
        );
    }
}

fn apply_set_constraints(
    checker: &mut TypeChecker<'_>,
    expected_id: TypeId,
    actual_id: TypeId,
    place_span: arandu_base::Span,
    value_span: arandu_base::Span,
) {
    if !checker.is_assignable(actual_id, expected_id) {
        checker.add_subtype_constraint(
            expected_id,
            actual_id,
            ConstraintOrigin::SetTarget {
                place_span,
                value_span,
            },
        );
    } else {
        let actual = checker.resolve(actual_id);
        let expected = checker.resolve(expected_id);
        if !actual.is_literal()
            && actual.default_literal() != expected.default_literal()
            && expected.is_numeric()
            && actual.is_numeric()
        {
            checker.add_constraint(
                expected_id,
                actual_id,
                ConstraintOrigin::ImplicitWidening {
                    source_span: value_span,
                    target_span: place_span,
                },
            );
        }
    }
}

fn validate_mutability(checker: &mut TypeChecker<'_>, place: &arandu_parser::Place) {
    if place.suffixes.is_empty() {
        let root_key = crate::NodeKey::from(place.span);
        if let Some(symbol_id) = checker.resolved.value_refs.get(&root_key) {
            let symbol = checker.symbols.get(*symbol_id);
            if (symbol.kind == crate::SymbolKind::Local || symbol.kind == crate::SymbolKind::Param)
                && !checker.resolved.mutable_symbols.contains(symbol_id)
            {
                let name = &symbol.name;
                let replacement = crate::Hint {
                    message: format!(
                        "consider declaring the variable as mutable: `mut {} = ...;`",
                        name
                    ),
                    replacement: Some(crate::CodeReplacement {
                        span: symbol.span,
                        new_text: format!("mut {name}"),
                    }),
                };
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T026CannotAssignImmutable,
                    format!("cannot assign twice to immutable variable '{name}'"),
                    place.span,
                )
                .with_label(place.span, "cannot assign to immutable variable")
                .with_label(symbol.span, "variable declared here as immutable")
                .with_hint_replacement(replacement);
                checker.diagnostics.push(diag);
            }
        }
    }
}
