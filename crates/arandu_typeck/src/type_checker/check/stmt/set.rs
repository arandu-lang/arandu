use super::super::super::TypeChecker;
use super::super::super::constraints::ConstraintOrigin;
use super::super::super::types::{ArType, TypeId};
use super::super::place::synth_place;

pub(super) fn check_set_stmt(
    checker: &mut TypeChecker<'_>,
    places: &[arandu_parser::Place],
    op: arandu_parser::SetOp,
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
    // `s += t` appends text to a `String` in place (`s.pushStr(t)`); a `str`
    // is a view and has nothing to append to.
    if op == arandu_parser::SetOp::AddAssign
        && let Some(target) = expected
    {
        let target_ty = checker.resolve(target);
        let text = crate::type_checker::synth::expr_text::is_string(checker, &target_ty);
        let view = matches!(
            target_ty,
            ArType::Primitive(arandu_middle::types::Primitive::Str)
        );
        if text || view {
            let str_ty = checker.intern(ArType::Primitive(arandu_middle::types::Primitive::Str));
            checker.text_view_sites.insert(value);
            let value_ty =
                super::super::super::synth::synth_expr_expected(checker, value, Some(str_ty));
            let value_ty = checker.resolve(value_ty);
            let span = places
                .first()
                .map_or(checker.pool.expr_span(value), |place| place.span);
            if view {
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T002IncompatibleAssignment,
                        "cannot append to a `str`: it is a view of text owned elsewhere",
                        span,
                    )
                    .with_primary_label("this binding is a `str`")
                    .with_hint("build the text in a `String`: `let mut s = String.from(\"...\")`, then `s += ...`"),
                );
            } else if !value_ty.is_error()
                && !crate::type_checker::synth::expr_text::is_text(checker, &value_ty)
            {
                let value_id = checker.intern(value_ty);
                checker.add_constraint(
                    str_ty,
                    value_id,
                    ConstraintOrigin::Assignment {
                        lhs_span: span,
                        rhs_span: checker.pool.expr_span(value),
                    },
                );
            }
            return;
        }
    }
    let is_ptr_offset_assign = matches!(
        op,
        arandu_parser::SetOp::AddAssign | arandu_parser::SetOp::SubAssign
    ) && expected
        .is_some_and(|id| matches!(checker.resolve(id), ArType::Ptr(_)));
    let synth_expected = if is_ptr_offset_assign {
        Some(checker.intern(ArType::Primitive(arandu_middle::types::Primitive::USize)))
    } else {
        expected
    };
    let val_ty_id = super::super::super::synth::synth_expr_expected(checker, value, synth_expected);

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

        if is_ptr_offset_assign && checker.resolve(final_val_ty_id).is_integer() {
            if !checker.ctx.is_in_unsafe() {
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::O012AllocRequiresUnsafe,
                        "pointer arithmetic requires an `unsafe` block",
                        place.span,
                    )
                    .with_label(place.span, "raw pointer offset assignment is unsafe"),
                );
            }
            if let Some(var_id) = checker.literal_table.var_for_expr(value) {
                let usize_id =
                    checker.intern(ArType::Primitive(arandu_middle::types::Primitive::USize));
                checker.constrain_literal_var(
                    var_id,
                    usize_id,
                    ConstraintOrigin::SetTarget {
                        place_span: place.span,
                        value_span: checker.pool.expr_span(value),
                    },
                );
            }
            return;
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

pub(super) fn apply_assignment_constraints(
    checker: &mut TypeChecker<'_>,
    expected: &ArType,
    actual: &ArType,
    lhs_span: arandu_base::Span,
    rhs_span: arandu_base::Span,
) {
    if !actual.is_literal()
        && actual.default_literal() != expected.default_literal()
        && !(actual.is_u8_or_byte() && expected.is_u8_or_byte())
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
    } else if !super::super::super::types::is_assignable(
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
            && !(actual.is_u8_or_byte() && expected.is_u8_or_byte())
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
    let root_key = crate::NodeKey::from(place.span);
    let symbol_id_opt = checker.resolved.value_refs.get(&root_key).copied();

    // A `static mut` is writable storage at any projection; only the
    // `unsafe` requirement applies.
    if !place.suffixes.is_empty()
        && let Some(symbol_id) = symbol_id_opt
        && checker.symbols.get(symbol_id).kind == crate::SymbolKind::StaticMut
    {
        if !checker.ctx.is_in_unsafe() {
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::O017StaticMutRequiresUnsafe,
                    "access to `static mut` requires an `unsafe` block",
                    place.span,
                )
                .with_label(place.span, "modifying `static mut` is unsafe"),
            );
        }
        return;
    }

    if place.suffixes.is_empty() {
        if let Some(symbol_id) = symbol_id_opt {
            let symbol = checker.symbols.get(symbol_id);
            if symbol.kind == crate::SymbolKind::StaticMut {
                if !checker.ctx.is_in_unsafe() {
                    let diag = crate::Diagnostic::error(
                        crate::DiagCode::O017StaticMutRequiresUnsafe,
                        "access to `static mut` requires an `unsafe` block",
                        place.span,
                    )
                    .with_label(place.span, "modifying `static mut` is unsafe");
                    checker.diagnostics.push(diag);
                }
                return;
            }
            if symbol.kind == crate::SymbolKind::Const {
                let name = &symbol.name;
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T026CannotAssignImmutable,
                    format!("cannot assign to constant '{name}'"),
                    place.span,
                )
                .with_label(place.span, "cannot assign to constant");
                checker.diagnostics.push(diag);
                return;
            }
            if (symbol.kind == crate::SymbolKind::Local || symbol.kind == crate::SymbolKind::Param)
                && !checker.resolved.mutable_symbols.contains(&symbol_id)
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
        return;
    }

    let mut current_ty_id = if let Some(symbol_id) = symbol_id_opt {
        if let Some(ty_id) = checker.ctx.lookup(symbol_id) {
            ty_id
        } else if let Some(ty_id) = checker.decl_type_id(symbol_id) {
            ty_id
        } else {
            return;
        }
    } else {
        return;
    };

    let is_root_mutable =
        symbol_id_opt.is_some_and(|sym_id| checker.resolved.mutable_symbols.contains(&sym_id));
    let mut is_mutable = is_root_mutable;

    let last = place.suffixes.len().saturating_sub(1);
    for (index, suffix) in place.suffixes.iter().enumerate() {
        if checker.resolve(current_ty_id).is_error() {
            return;
        }

        // An explicit `*` grants exactly what the reference grants: writing
        // through `mut ref` needs no `mut` binding, and `ref` never allows it.
        if let arandu_parser::PlaceSuffix::Deref { span } = suffix {
            match checker.resolve(current_ty_id) {
                ArType::RefMut(inner) | ArType::Ptr(inner) => {
                    is_mutable = true;
                    current_ty_id = inner;
                }
                ArType::Ref(inner) => {
                    is_mutable = false;
                    current_ty_id = inner;
                }
                _ => return,
            }
            if index == last && !is_mutable {
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::T026CannotAssignImmutable,
                        "cannot assign through a shared reference",
                        *span,
                    )
                    .with_label(*span, "this place is behind a `ref`")
                    .with_hint("take it as `mut ref` to write through it"),
                );
            }
            continue;
        }

        let (peeled_id, was_nullable) =
            super::super::place::peel_auto_deref_place(checker, current_ty_id);
        if was_nullable {
            return;
        }

        let mut check_id = current_ty_id;
        while check_id != peeled_id {
            match checker.resolve(check_id) {
                ArType::RefMut(inner) | ArType::Ptr(inner) => {
                    is_mutable = true;
                    check_id = inner;
                }
                ArType::Ref(inner) => {
                    is_mutable = false;
                    check_id = inner;
                }
                ArType::Nullable(inner) => {
                    check_id = inner;
                }
                _ => break,
            }
        }

        let actual_base_ty = checker.resolve(peeled_id);

        match suffix {
            arandu_parser::PlaceSuffix::Index { span, .. } => {
                match &actual_base_ty {
                    ArType::Slice(_) => {
                        let diag = crate::Diagnostic::error(
                            crate::DiagCode::T026CannotAssignImmutable,
                            "cannot assign to immutable slice element",
                            *span,
                        )
                        .with_label(
                            *span,
                            "cannot assign to element of immutable slice view `[]T`",
                        )
                        .with_hint("consider using a mutable slice `mut []T`".to_string());
                        checker.diagnostics.push(diag);
                        return;
                    }
                    ArType::MutSlice(inner) | ArType::Ptr(inner) => {
                        is_mutable = true;
                        current_ty_id = *inner;
                    }
                    ArType::Array(_, inner) | ArType::ConstArray(_, inner) => {
                        if !is_mutable {
                            let (sym_name, sym_span) = symbol_id_opt
                                .map(|s| {
                                    (
                                        checker.symbols.get(s).name.as_str(),
                                        checker.symbols.get(s).span,
                                    )
                                })
                                .unwrap_or(("", place.span));
                            let diag = crate::Diagnostic::error(
                                crate::DiagCode::T026CannotAssignImmutable,
                                format!("cannot mutate immutable array '{sym_name}'"),
                                *span,
                            )
                            .with_label(*span, "cannot assign to element of immutable array")
                            .with_label(sym_span, "variable declared here as immutable")
                            .with_hint(format!("consider declaring the variable as mutable: `mut {sym_name} = ...;`"));
                            checker.diagnostics.push(diag);
                            return;
                        }
                        current_ty_id = *inner;
                    }
                    ArType::Named(_, args)
                        if arandu_middle::types::is_vec_type(&actual_base_ty, &checker.symbols) =>
                    {
                        if !is_mutable {
                            let (sym_name, sym_span) = symbol_id_opt
                                .map(|s| {
                                    (
                                        checker.symbols.get(s).name.as_str(),
                                        checker.symbols.get(s).span,
                                    )
                                })
                                .unwrap_or(("", place.span));
                            let diag = crate::Diagnostic::error(
                                crate::DiagCode::T026CannotAssignImmutable,
                                format!("cannot mutate immutable variable '{sym_name}'"),
                                *span,
                            )
                            .with_label(*span, "cannot assign to element of immutable vector")
                            .with_label(sym_span, "variable declared here as immutable")
                            .with_hint(format!("consider declaring the variable as mutable: `mut {sym_name} = ...;`"));
                            checker.diagnostics.push(diag);
                            return;
                        }
                        current_ty_id = checker.type_info.type_interner.type_args(*args)[0];
                    }
                    _ => return,
                }
            }
            arandu_parser::PlaceSuffix::Field { name, span, .. } => {
                let interner = &checker.type_info.type_interner;
                let struct_info_opt = match actual_base_ty {
                    ArType::Named(id, args) => Some((id, interner.type_args(args))),
                    ArType::Ptr(inner) => {
                        is_mutable = true;
                        match interner.resolve(inner) {
                            ArType::Named(id, args) => Some((id, interner.type_args(args))),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some((struct_id, args)) = struct_info_opt {
                    let resolved_args: Vec<ArType> =
                        args.iter().map(|&a| interner.resolve(a)).collect();
                    if let Some(field_ty) = super::super::super::types::struct_field_instantiated(
                        checker,
                        struct_id,
                        &resolved_args,
                        name.as_str(),
                    ) {
                        current_ty_id = checker.intern(field_ty);
                    } else if let Some(field) = checker
                        .type_info
                        .struct_fields
                        .get(&struct_id)
                        .and_then(|fields| fields.get(name.as_str()))
                    {
                        current_ty_id = field.ty;
                    } else {
                        return;
                    }
                } else {
                    return;
                }
                if !is_mutable {
                    let (sym_name, sym_span) = symbol_id_opt
                        .map(|s| {
                            (
                                checker.symbols.get(s).name.as_str(),
                                checker.symbols.get(s).span,
                            )
                        })
                        .unwrap_or(("", place.span));
                    let diag = crate::Diagnostic::error(
                        crate::DiagCode::T026CannotAssignImmutable,
                        format!("cannot assign to field `{name}` of immutable '{sym_name}'"),
                        *span,
                    )
                    .with_label(
                        *span,
                        "cannot assign to a field reached without mutable access",
                    )
                    .with_label(sym_span, "declared here without mutable access")
                    .with_hint(format!(
                        "declare it mutable (`mut {sym_name}`) or take it as `mut ref`/`mut self`"
                    ));
                    checker.diagnostics.push(diag);
                    return;
                }
            }
            arandu_parser::PlaceSuffix::Deref { .. } => {}
        }
    }
}
