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
    let was_nullable = false;
    let mut was_option = false;
    // Bound iterations: Option/Nullable? + a few ref layers is plenty; avoid infinite
    // loops on malformed interned graphs.
    for _ in 0..8 {
        match checker.resolve(id) {
            ArType::Option(inner) => {
                was_option = true;
                id = inner;
            }
            ArType::Ref(inner) | ArType::RefMut(inner) => {
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
    if !checker
        .type_info
        .generic_params
        .get(&symbol_id)
        .is_some_and(|parameters| !parameters.is_empty())
        && let Some(ty_id) = checker.type_info.demand_header(symbol_id, &[], _span)
    {
        return Some(ty_id);
    }
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
    resolve_field_type(
        checker,
        base_ty_id,
        checker.pool.expr_span(base),
        field,
        field_span,
        safe,
    )
}

pub(crate) fn resolve_field_type(
    checker: &mut TypeChecker<'_>,
    base_ty_id: TypeId,
    base_span: arandu_lexer::Span,
    field: &str,
    field_span: arandu_lexer::Span,
    safe: bool,
) -> TypeId {
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
            base_span,
            format!(
                "this has type '{}'",
                base_ty.display(&checker.symbols, &checker.type_info.type_interner)
            ),
        )
        .with_hint("use safe access `?.` or unwrap the value first".to_string());
        checker.diagnostics.push(diag);
        return checker.intern(ArType::Error);
    }

    // `t.0`: an element of a tuple, by position.
    if let ArType::Tuple(items) = actual_base_ty {
        let items = checker.type_info.type_interner.type_args(items);
        let element = arandu_parser::is_tuple_index(field)
            .then(|| field.parse::<usize>().ok())
            .flatten()
            .and_then(|index| items.get(index).copied());
        return element.unwrap_or_else(|| {
            let base_ty = checker.resolve(base_ty_id);
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::T018UndefinedField,
                    format!(
                        "no element '{field}' on the tuple '{}'",
                        base_ty.display(&checker.symbols, &checker.type_info.type_interner)
                    ),
                    field_span,
                )
                .with_label(
                    base_span,
                    format!("this tuple has {} elements", items.len()),
                )
                .with_hint(format!(
                    "tuple elements are read by position, from `.0` to `.{}`",
                    items.len().saturating_sub(1)
                )),
            );
            checker.intern(ArType::Error)
        });
    }

    let struct_info_opt = match actual_base_ty {
        ArType::Named(id, args) => Some((id, args)),
        ArType::Ptr(inner) => match checker.resolve(inner) {
            ArType::Named(id, args) => {
                if !checker.ctx.is_in_unsafe() {
                    checker.diagnostics.push(
                        crate::Diagnostic::error(
                            crate::DiagCode::O019RawPtrRequiresUnsafe,
                            "reading a field through a raw pointer requires an `unsafe` block",
                            field_span,
                        )
                        .with_label(field_span, "raw `ptr[T]` field access is unsafe"),
                    );
                }
                Some((id, args))
            }
            _ => None,
        },
        _ => None,
    };

    let field_ty = if let Some((struct_id, args)) = struct_info_opt {
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
            checker.check_field_visibility(struct_id, field, field_span);
            if checker.type_info.struct_unions.contains(&struct_id) && !checker.ctx.is_in_unsafe() {
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::O012AllocRequiresUnsafe,
                        "reading a field of a `union` requires an `unsafe` block",
                        field_span,
                    )
                    .with_label(field_span, "union field access is unsafe"),
                );
            }
            field_ty
        } else {
            if let Some(method_sym) = checker.symbols.lookup_associated_member(struct_id, field)
                && let Some(ArType::Func(params, ret)) = checker.decl_type(method_sym)
            {
                let interner = &checker.type_info.type_interner;
                ArType::func(&interner.type_args(params), ret, interner)
            } else if let Some(constraints) = checker.type_info.param_constraints.get(&struct_id) {
                let mut found_method_ty = None;
                for bound in constraints.iter() {
                    if let Some(iface_info) = checker.type_info.interfaces.get(&bound.iface_sym)
                        && let Some(m) = iface_info.methods.iter().find(|m| m.name == field)
                    {
                        let raw_sig = checker.resolve(m.sig_id);
                        let base_ty = checker.resolve(actual_base_ty_id);
                        let inst_sig =
                            crate::type_checker::types::interfaces::instantiate_interface_method(
                                checker,
                                bound.iface_sym,
                                &bound.type_args,
                                &base_ty,
                                &raw_sig,
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
                            base_span,
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
                        base_span,
                        field_span,
                        field_name: field.to_string(),
                    },
                );
                return checker.intern(ArType::Error);
            }
        }
    } else if field == "len"
        && matches!(
            actual_base_ty,
            ArType::Primitive(Primitive::Str)
                | ArType::Slice(_)
                | ArType::MutSlice(_)
                | ArType::Array(_, _)
                | ArType::ConstArray(_, _)
        )
    {
        ArType::Primitive(Primitive::USize)
    } else {
        checker.add_constraint(
            actual_base_ty_id,
            ArType::Error,
            ConstraintOrigin::UndefinedField {
                base_span,
                field_span,
                field_name: field.to_string(),
            },
        );
        return checker.intern(ArType::Error);
    };

    let field_already_nullable = matches!(field_ty, ArType::Option(_));
    let field_id = checker.intern(field_ty);
    if (safe || was_nullable || was_option) && !field_already_nullable {
        checker.intern(ArType::Option(field_id))
    } else {
        // `a?.b` where `b: T?` (`Option<T>`) collapses to one `T?`.
        field_id
    }
}

/// Returns `true` for types handled natively by built-in indexing codegen.
pub(crate) fn is_builtin_index_base(ty: &ArType, symbols: &arandu_middle::SymbolTable) -> bool {
    matches!(
        ty,
        ArType::Array(_, _)
            | ArType::ConstArray(_, _)
            | ArType::Slice(_)
            | ArType::MutSlice(_)
            | ArType::Ptr(_)
            | ArType::Error
    ) || arandu_middle::types::is_vec_type(ty, symbols)
}

fn validate_trait_index_arg(
    checker: &mut TypeChecker<'_>,
    expected_idx_ty: TypeId,
    index_expr: Option<ExprId>,
    actual_idx_ty: TypeId,
    base_span: arandu_lexer::Span,
    index_span: arandu_lexer::Span,
) {
    let actual = checker.resolve(actual_idx_ty);
    let expected = checker.resolve(expected_idx_ty);
    if actual.is_error() || expected.is_error() {
        return;
    }
    if !checker.unify_ids(expected_idx_ty, actual_idx_ty)
        && !checker.is_assignable(actual_idx_ty, expected_idx_ty)
    {
        checker.add_constraint(
            expected_idx_ty,
            actual_idx_ty,
            ConstraintOrigin::InvalidIndex {
                base_span,
                index_span,
                is_base_error: false,
            },
        );
    } else if matches!(actual, ArType::IntLiteral)
        && expected.is_integer()
        && let Some(eid) = index_expr
    {
        checker.record_expr_type(eid, expected_idx_ty);
    }
}

/// Resolve `Index<Idx, Out>` or `IndexMut<Idx, Out>` for a non-built-in receiver
/// (concrete nominal type or generic `TypeParam` bounded by `Index`/`IndexMut`).
pub(crate) fn resolve_trait_index(
    checker: &mut TypeChecker<'_>,
    actual_base_ty: &ArType,
    index_expr: Option<ExprId>,
    index_ty_id: TypeId,
    mutable: bool,
    base_span: arandu_lexer::Span,
    index_span: arandu_lexer::Span,
) -> Option<(TypeId, TypeId)> {
    if let ArType::Named(param_sym, args) = actual_base_ty
        && args.is_empty()
        && checker
            .symbols
            .try_get(*param_sym)
            .is_some_and(|s| s.kind == arandu_middle::SymbolKind::TypeParam)
    {
        let target_lang = if mutable {
            arandu_middle::symbol_table::LangItem::IndexMut
        } else {
            arandu_middle::symbol_table::LangItem::Index
        };
        let matched = checker
            .type_info
            .param_constraints
            .get(param_sym)
            .and_then(|bounds| {
                bounds.iter().find_map(|b| {
                    let is_target = checker
                        .symbols
                        .try_get(b.iface_sym)
                        .is_some_and(|iface| iface.lang_item == Some(target_lang));
                    (is_target && b.type_args.len() == 2).then(|| (b.type_args[0], b.type_args[1]))
                })
            });
        if let Some((idx_ty, out_ty)) = matched {
            validate_trait_index_arg(
                checker,
                idx_ty,
                index_expr,
                index_ty_id,
                base_span,
                index_span,
            );
            return Some((idx_ty, out_ty));
        }
        return None;
    }

    let ArType::Named(type_sym, type_args) = actual_base_ty else {
        return None;
    };
    let method_name = if mutable { "indexMut" } else { "index" };
    let method_sym = checker
        .symbols
        .lookup_associated_member(*type_sym, method_name)?;
    let method_ty = checker.decl_type(method_sym)?;
    let arg_vec = checker.type_info.type_interner.type_args(*type_args);
    let subst = if !arg_vec.is_empty() {
        let concrete_args: Vec<ArType> = arg_vec.iter().map(|&a| checker.resolve(a)).collect();
        if let Some(gp) = checker.type_info.generic_params.get(&method_sym) {
            let n = gp.len().min(concrete_args.len());
            Some(crate::type_checker::types::build_subst(
                &gp[..n],
                &concrete_args[..n],
            ))
        } else if let Some(gp) = checker.type_info.generic_params.get(type_sym) {
            let n = gp.len().min(concrete_args.len());
            Some(crate::type_checker::types::build_subst(
                &gp[..n],
                &concrete_args[..n],
            ))
        } else {
            None
        }
    } else {
        None
    };
    let inst_method_ty = match subst {
        Some(ref map) => crate::type_checker::types::substitute_type(
            &method_ty,
            map,
            &checker.type_info.type_interner,
        ),
        None => method_ty,
    };
    let ArType::Func(params_id, ret_id) = inst_method_ty else {
        return None;
    };
    let params = checker.type_info.type_interner.type_args(params_id);
    if params.len() != 2 {
        return None;
    }
    let recv_ok = matches!(
        (mutable, checker.resolve(params[0])),
        (false, ArType::Ref(_)) | (true, ArType::RefMut(_))
    );
    let out_ty = match (mutable, checker.resolve(ret_id)) {
        (false, ArType::Ref(inner)) => inner,
        (true, ArType::RefMut(inner)) => inner,
        _ => return None,
    };
    if !recv_ok {
        return None;
    }
    let idx_ty = params[1];
    validate_trait_index_arg(
        checker,
        idx_ty,
        index_expr,
        index_ty_id,
        base_span,
        index_span,
    );
    Some((idx_ty, out_ty))
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

    let index_ty = checker.resolve(index_ty_id);
    let is_range_index = super::expr_range::range_parts(checker, &index_ty).is_some();

    let mut is_trait_index = false;
    let elem_ty_id = match &actual_base_ty {
        ArType::Array(_, inner)
        | ArType::ConstArray(_, inner)
        | ArType::Slice(inner)
        | ArType::MutSlice(inner) => *inner,
        ArType::Ptr(inner) => {
            if !checker.ctx.is_in_unsafe() {
                let idx_span = checker.pool.expr_span(index);
                checker.diagnostics.push(
                    crate::Diagnostic::error(
                        crate::DiagCode::O019RawPtrRequiresUnsafe,
                        "indexing a raw pointer requires an `unsafe` block",
                        idx_span,
                    )
                    .with_label(idx_span, "raw `ptr[T]` indexing is unsafe"),
                );
            }
            *inner
        }
        ArType::Named(_, args)
            if arandu_middle::types::is_vec_type(&actual_base_ty, &checker.symbols) =>
        {
            checker.type_info.type_interner.type_args(*args)[0]
        }
        _ => {
            if !is_range_index
                && let Some((_idx_ty, out_ty)) = resolve_trait_index(
                    checker,
                    &actual_base_ty,
                    Some(index),
                    index_ty_id,
                    false,
                    checker.pool.expr_span(base),
                    checker.pool.expr_span(index),
                )
            {
                is_trait_index = true;
                out_ty
            } else {
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
        }
    };

    if !is_trait_index && !index_ty.is_error() && !index_ty.is_integer() && !is_range_index {
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
        let is_mut_slice = matches!(actual_base_ty, ArType::MutSlice(_));
        let slice_ty_id = if is_mut_slice {
            checker.intern(ArType::MutSlice(elem_ty_id))
        } else {
            checker.intern(ArType::Slice(elem_ty_id))
        };
        if safe || was_nullable || was_option {
            checker.intern(ArType::Option(slice_ty_id))
        } else {
            slice_ty_id
        }
    } else {
        let elem_already_optional = matches!(checker.resolve(elem_ty_id), ArType::Option(_));
        if (safe || was_nullable || was_option) && !elem_already_optional {
            checker.intern(ArType::Option(elem_ty_id))
        } else {
            elem_ty_id
        }
    }
}
