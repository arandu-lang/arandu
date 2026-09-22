use std::sync::Arc;

use arandu_middle::types::TypeId;
use arandu_parser::ast_pool::{ExprId, ExprKind};

use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::{ArType, Primitive};
use super::expr::synth_expr;

/// Peel auto-deref layers for field/index bases: `Option`, `Nullable`, then `&`/`&mut`/`ptr`.
///
/// Returns `(inner_ty_id, was_nullable, was_option)`. Used so `shared self` (`&T`) and
/// `mut self` (`&mut T`) can use `self.field` without an explicit `*`.
fn peel_auto_deref_base(checker: &TypeChecker<'_>, base_ty_id: TypeId) -> (TypeId, bool, bool) {
    let mut id = base_ty_id;
    let mut was_nullable = false;
    let mut was_option = false;
    // Bound iterations: Option/Nullable? + a few ref layers is plenty; avoid infinite
    // loops on malformed interned graphs.
    for _ in 0..8 {
        match checker.resolve(id) {
            ArType::Option(inner) => {
                was_option = true;
                id = inner;
            }
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
    (id, was_nullable, was_option)
}

pub(crate) fn resolve_namespace_field(
    checker: &mut TypeChecker<'_>,
    base: ExprId,
    expr: ExprId,
    field: &str,
    _span: arandu_lexer::Span,
) -> Option<TypeId> {
    let ExprKind::Path { path } = checker.pool.expr(base) else {
        return None;
    };
    if path.len() != 1 {
        return None;
    }
    let symbol_id = checker.symbols.lookup_module_member(&path[0], field)?;
    Arc::make_mut(&mut checker.resolved).expr_ref(expr, symbol_id);
    if let Some(ty_id) = checker.ctx.lookup(symbol_id) {
        return Some(ty_id);
    }
    checker.decl_type_id(symbol_id)
}

pub(crate) fn resolve_namespace_member_type(
    checker: &TypeChecker<'_>,
    expr: ExprId,
) -> Option<TypeId> {
    if let Some(symbol_id) = checker.resolved.expr_symbol(expr) {
        if let Some(ty_id) = checker.ctx.lookup(symbol_id) {
            return Some(ty_id);
        }
        if let Some(ty_id) = checker.decl_type_id(symbol_id) {
            return Some(ty_id);
        }
    }
    None
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker))]
pub(crate) fn resolve_field(
    checker: &mut TypeChecker<'_>,
    base: ExprId,
    field: &str,
    field_span: arandu_lexer::Span,
    safe: bool,
) -> TypeId {
    let base_ty_id = synth_expr(checker, base);
    if checker.resolve(base_ty_id).is_error() {
        return checker.intern(ArType::Error);
    }

    let (actual_base_ty_id, was_nullable, was_option) = peel_auto_deref_base(checker, base_ty_id);
    let actual_base_ty = checker.resolve(actual_base_ty_id);

    if (was_nullable || was_option) && !safe {
        let base_ty = checker.resolve(base_ty_id);
        let diag = crate::Diagnostic::error(
            crate::DiagCode::T006NotNullable,
            format!(
                "cannot access field '{}' on nullable or optional type '{}'",
                field,
                base_ty.display(&checker.symbols, &checker.type_info.type_interner)
            ),
            field_span,
        )
        .with_label(
            checker.pool.expr_span(base),
            format!(
                "this has type '{}'",
                base_ty.display(&checker.symbols, &checker.type_info.type_interner)
            ),
        )
        .with_hint("use safe access `?.` or unwrap the value first".to_string());
        checker.diagnostics.push(diag);
        return checker.intern(ArType::Error);
    }

    let struct_info_opt = match actual_base_ty {
        ArType::Named(id, args) => Some((id, args)),
        ArType::Ptr(inner) => match checker.resolve(inner) {
            ArType::Named(id, args) => Some((id, args)),
            _ => None,
        },
        _ => None,
    };

    let field_ty =
        if let Some((struct_id, args)) = struct_info_opt {
            let arg_ids: Vec<TypeId> = checker.type_info.type_interner.type_args(args);
            let resolved_args: Vec<ArType> = arg_ids.iter().map(|&a| checker.resolve(a)).collect();
            let field_from_struct = if let Some(field_ty) =
                super::super::types::struct_field_instantiated(
                    checker,
                    struct_id,
                    &resolved_args,
                    field,
                ) {
                Some(field_ty)
            } else {
                checker
                    .type_info
                    .struct_fields
                    .get(&struct_id)
                    .and_then(|fields| fields.get(field))
                    .map(|f| checker.resolve(f.ty))
            };

            if let Some(field_ty) = field_from_struct {
                field_ty
            } else {
                if let Some(method_sym) = checker.symbols.lookup_associated_member(struct_id, field)
                    && let Some(ArType::Func(params, ret)) = checker.decl_type(method_sym)
                {
                    let interner = &checker.type_info.type_interner;
                    ArType::func(&interner.type_args(params), ret, interner)
                } else if let Some(constraints) =
                    checker.type_info.param_constraints.get(&struct_id)
                {
                    let mut found_method_ty = None;
                    for bound in constraints.iter() {
                        if let Some(iface_info) = checker.type_info.interfaces.get(&bound.iface_sym)
                            && let Some(m) = iface_info.methods.iter().find(|m| m.name == field)
                        {
                            let raw_sig = checker.resolve(m.sig_id);
                            let base_ty = checker.resolve(actual_base_ty_id);
                            let inst_sig =
                            crate::type_checker::types::interfaces::instantiate_interface_method(
                                checker, bound.iface_sym, &bound.type_args, &base_ty, &raw_sig,
                            );
                            found_method_ty = Some(inst_sig);
                            break;
                        }
                    }
                    if let Some(method_ty) = found_method_ty {
                        if let ArType::Func(params, ret) = method_ty {
                            let interner = &checker.type_info.type_interner;
                            let params_vec = interner.type_args(params);
                            let payload = if params_vec.first().is_some_and(|&p| {
                                checker.unify_ids(p, actual_base_ty_id)
                                    || match checker.resolve(p) {
                                        ArType::Ref(inner)
                                        | ArType::RefMut(inner)
                                        | ArType::Ptr(inner) => {
                                            checker.unify_ids(inner, actual_base_ty_id)
                                        }
                                        _ => false,
                                    }
                            }) {
                                &params_vec[1..]
                            } else {
                                &params_vec[..]
                            };
                            let mut new_params = vec![actual_base_ty_id];
                            new_params.extend_from_slice(payload);
                            ArType::func(&new_params, ret, interner)
                        } else {
                            method_ty
                        }
                    } else {
                        checker.add_constraint(
                            actual_base_ty_id,
                            ArType::Error,
                            ConstraintOrigin::UndefinedField {
                                base_span: checker.pool.expr_span(base),
                                field_span,
                                field_name: field.to_string(),
                            },
                        );
                        return checker.intern(ArType::Error);
                    }
                } else {
                    checker.add_constraint(
                        actual_base_ty_id,
                        ArType::Error,
                        ConstraintOrigin::UndefinedField {
                            base_span: checker.pool.expr_span(base),
                            field_span,
                            field_name: field.to_string(),
                        },
                    );
                    return checker.intern(ArType::Error);
                }
            }
        } else {
            checker.add_constraint(
                actual_base_ty_id,
                ArType::Error,
                ConstraintOrigin::UndefinedField {
                    base_span: checker.pool.expr_span(base),
                    field_span,
                    field_name: field.to_string(),
                },
            );
            return checker.intern(ArType::Error);
        };

    let field_id = checker.intern(field_ty);
    if was_option {
        checker.intern(ArType::Option(field_id))
    } else if safe || was_nullable {
        checker.intern(ArType::Nullable(field_id))
    } else {
        field_id
    }
}

#[tracing::instrument(level = "trace", target = "arandu_typeck", skip(checker))]
pub(crate) fn resolve_index(
    checker: &mut TypeChecker<'_>,
    base: ExprId,
    index: ExprId,
    safe: bool,
) -> TypeId {
    let base_ty_id = synth_expr(checker, base);
    let index_ty_id = synth_expr(checker, index);

    if checker.resolve(base_ty_id).is_error() {
        return checker.intern(ArType::Error);
    }

    let (actual_base_ty_id, was_nullable, was_option) = peel_auto_deref_base(checker, base_ty_id);
    let actual_base_ty = checker.resolve(actual_base_ty_id);

    if (was_nullable || was_option) && !safe {
        let base_ty = checker.resolve(base_ty_id);
        let diag = crate::Diagnostic::error(
            crate::DiagCode::T006NotNullable,
            format!(
                "cannot index nullable or optional type '{}'",
                base_ty.display(&checker.symbols, &checker.type_info.type_interner)
            ),
            checker.pool.expr_span(index),
        )
        .with_label(
            checker.pool.expr_span(base),
            format!(
                "this has type '{}'",
                base_ty.display(&checker.symbols, &checker.type_info.type_interner)
            ),
        )
        .with_hint("use safe index `?[...]` or make the value non-nullable".to_string());
        checker.diagnostics.push(diag);
        return checker.intern(ArType::Error);
    }

    let elem_ty_id = match &actual_base_ty {
        ArType::Array(_, inner) | ArType::ConstArray(_, inner) | ArType::Slice(inner) => *inner,
        ArType::Named(_, args)
            if arandu_middle::types::is_vec_type(&actual_base_ty, &checker.symbols) =>
        {
            checker.type_info.type_interner.type_args(*args)[0]
        }
        _ => {
            checker.add_constraint(
                actual_base_ty_id,
                ArType::Error,
                ConstraintOrigin::InvalidIndex {
                    base_span: checker.pool.expr_span(base),
                    index_span: checker.pool.expr_span(index),
                    is_base_error: true,
                },
            );
            checker.intern(ArType::Error)
        }
    };

    let index_ty = checker.resolve(index_ty_id);
    let is_range_index = matches!(index_ty, ArType::Range(_));

    if !index_ty.is_error() && !index_ty.is_integer() && !is_range_index {
        checker.add_constraint(
            ArType::Primitive(Primitive::Int),
            index_ty_id,
            ConstraintOrigin::InvalidIndex {
                base_span: checker.pool.expr_span(base),
                index_span: checker.pool.expr_span(index),
                is_base_error: false,
            },
        );
    }

    if is_range_index {
        let slice_ty_id = checker.intern(ArType::Slice(elem_ty_id));
        if was_option {
            checker.intern(ArType::Option(slice_ty_id))
        } else if safe || was_nullable {
            checker.intern(ArType::Nullable(slice_ty_id))
        } else {
            slice_ty_id
        }
    } else if was_option {
        checker.intern(ArType::Option(elem_ty_id))
    } else if safe || was_nullable {
        checker.intern(ArType::Nullable(elem_ty_id))
    } else {
        elem_ty_id
    }
}
