use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::{ArType, Primitive, TypeId};

/// Peel `Nullable` / `&` / `&mut` / `ptr` so place field/index works on `shared`/`mut self`.
fn peel_auto_deref_place(checker: &TypeChecker<'_>, base_ty_id: TypeId) -> (TypeId, bool) {
    let mut id = base_ty_id;
    let mut was_nullable = false;
    for _ in 0..8 {
        match checker.resolve(id) {
            ArType::Nullable(inner) => {
                was_nullable = true;
                id = inner;
            }
            ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                id = inner;
            }
            _ => break,
        }
    }
    (id, was_nullable)
}

pub(crate) fn synth_place(checker: &mut TypeChecker<'_>, place: &arandu_parser::Place) -> TypeId {
    let root_key = crate::NodeKey::from(place.span);
    let mut current_ty_id = if let Some(symbol_id) = checker.resolved.value_refs.get(&root_key) {
        if let Some(ty_id) = checker.ctx.lookup(*symbol_id) {
            ty_id
        } else if let Some(ty_id) = checker.decl_type_id(*symbol_id) {
            ty_id
        } else {
            checker.intern(ArType::Error)
        }
    } else {
        checker.intern(ArType::Error)
    };

    // 2. Traverse suffixes
    for suffix in &place.suffixes {
        if checker.resolve(current_ty_id).is_error() {
            break;
        }
        match suffix {
            arandu_parser::PlaceSuffix::Field { span, name } => {
                let interner = &checker.type_info.type_interner;
                let (actual_base_ty_id, was_nullable) =
                    peel_auto_deref_place(checker, current_ty_id);
                let actual_base_ty = checker.resolve(actual_base_ty_id);
                if was_nullable {
                    let current_ty = checker.resolve(current_ty_id);
                    let diag = crate::Diagnostic::error(
                        crate::DiagCode::T006NotNullable,
                        format!(
                            "cannot access field '{}' on nullable type '{}'",
                            name,
                            current_ty.display(&checker.symbols, interner)
                        ),
                        *span,
                    )
                    .with_label(
                        place.span,
                        format!(
                            "this has type '{}'",
                            current_ty.display(&checker.symbols, interner)
                        ),
                    )
                    .with_hint("use safe access `?.` or make the value non-nullable".to_string());
                    checker.diagnostics.push(diag);
                    current_ty_id = checker.intern(ArType::Error);
                    break;
                }
                let struct_info_opt = match actual_base_ty {
                    ArType::Named(id, args) => {
                        let args_vec = interner.type_args(args);
                        Some((id, args_vec))
                    }
                    ArType::Ptr(inner) => match interner.resolve(inner) {
                        ArType::Named(id, args) => {
                            let args_vec = interner.type_args(args);
                            Some((id, args_vec))
                        }
                        _ => None,
                    },
                    _ => None,
                };
                let field_from_struct = if let Some((struct_id, args)) = struct_info_opt {
                    let resolved_args: Vec<ArType> =
                        args.iter().map(|&a| interner.resolve(a)).collect();
                    if let Some(field_ty) = super::super::types::struct_field_instantiated(
                        checker,
                        struct_id,
                        &resolved_args,
                        name.as_str(),
                    ) {
                        Some(field_ty)
                    } else {
                        checker
                            .type_info
                            .struct_fields
                            .get(&struct_id)
                            .and_then(|fields| fields.get(name.as_str()))
                            .map(|f| checker.resolve(f.ty))
                    }
                } else {
                    None
                };

                if let Some(field_ty) = field_from_struct {
                    current_ty_id = checker.intern(field_ty);
                } else {
                    let err_id = checker.intern(ArType::Error);
                    checker.add_constraint(
                        actual_base_ty_id,
                        err_id,
                        ConstraintOrigin::UndefinedField {
                            base_span: place.span,
                            field_span: *span,
                            field_name: name.to_string(),
                        },
                    );
                    current_ty_id = err_id;
                }
            }
            arandu_parser::PlaceSuffix::Index { span, expr } => {
                let index_ty_id = super::super::synth::synth_expr(checker, *expr);
                let interner = &checker.type_info.type_interner;
                let (actual_base_ty_id, was_nullable) =
                    peel_auto_deref_place(checker, current_ty_id);
                let actual_base_ty = checker.resolve(actual_base_ty_id);
                if was_nullable {
                    let current_ty = checker.resolve(current_ty_id);
                    let diag = crate::Diagnostic::error(
                        crate::DiagCode::T006NotNullable,
                        format!(
                            "cannot index nullable type '{}'",
                            current_ty.display(&checker.symbols, interner)
                        ),
                        *span,
                    )
                    .with_label(
                        place.span,
                        format!(
                            "this has type '{}'",
                            current_ty.display(&checker.symbols, interner)
                        ),
                    )
                    .with_hint(
                        "use safe index `?[...]` or make the value non-nullable".to_string(),
                    );
                    checker.diagnostics.push(diag);
                    current_ty_id = checker.intern(ArType::Error);
                    break;
                }
                match &actual_base_ty {
                    ArType::Array(_, inner)
                    | ArType::ConstArray(_, inner)
                    | ArType::Slice(inner) => {
                        current_ty_id = *inner;
                    }
                    ArType::Named(_, args)
                        if arandu_middle::types::is_vec_type(&actual_base_ty, &checker.symbols) =>
                    {
                        current_ty_id = interner.type_args(*args)[0];
                    }
                    _ => {
                        let err_id = checker.intern(ArType::Error);
                        checker.add_constraint(
                            actual_base_ty_id,
                            err_id,
                            ConstraintOrigin::InvalidIndex {
                                base_span: place.span,
                                index_span: checker.pool.expr_span(*expr),
                                is_base_error: true,
                            },
                        );
                        current_ty_id = err_id;
                    }
                }
                let index_ty = checker.resolve(index_ty_id);
                if !index_ty.is_error() && !index_ty.is_integer() {
                    checker.add_constraint(
                        ArType::Primitive(Primitive::Int),
                        index_ty_id,
                        ConstraintOrigin::InvalidIndex {
                            base_span: place.span,
                            index_span: checker.pool.expr_span(*expr),
                            is_base_error: false,
                        },
                    );
                }
            }
        }
    }

    current_ty_id
}
