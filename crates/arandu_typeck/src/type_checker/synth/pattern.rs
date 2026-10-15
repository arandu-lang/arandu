use arandu_parser::{Pattern, ast_pool::PatternId};

use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::{self, ArType, TypeId};
use super::expr::synth_expr;

fn instantiate_enum_payload(
    checker: &mut TypeChecker<'_>,
    enum_id: crate::SymbolId,
    variant_id: crate::SymbolId,
    enum_args: &[TypeId],
    payload: &[TypeId],
) -> Vec<TypeId> {
    let span = checker.symbols.try_get(variant_id).map_or(
        arandu_middle::Span::new(variant_id.file_id, 0, 0),
        |symbol| symbol.span,
    );
    checker.type_info.demand_header(enum_id, enum_args, span);
    let frozen = checker
        .type_info
        .variant_payload_for(variant_id, enum_args)
        .cloned();
    let payload = match &frozen {
        Some(super::super::EnumPayloadShape::Tuple(items)) => items.as_slice(),
        _ => payload,
    };
    let params = checker
        .type_info
        .generic_params
        .get(&enum_id)
        .map(|params| params.as_ref().clone())
        .or_else(|| enum_params_from_variant(checker, variant_id));
    let Some(params) = params else {
        return payload.to_vec();
    };
    if enum_args.is_empty() || params.is_empty() {
        return payload.to_vec();
    }
    let concrete: Vec<_> = enum_args.iter().map(|&id| checker.resolve(id)).collect();
    let subst = types::build_subst(&params, &concrete[..params.len().min(concrete.len())]);
    payload
        .iter()
        .map(|&id| {
            let ty = checker.resolve(id);
            checker.intern(types::substitute_type(
                &ty,
                &subst,
                &checker.type_info.type_interner,
            ))
        })
        .collect()
}

fn enum_params_from_variant(
    checker: &TypeChecker<'_>,
    variant_id: crate::SymbolId,
) -> Option<Vec<crate::SymbolId>> {
    let ret_ty = match checker.type_info.decl_type(variant_id)? {
        ArType::Func(_, ret) => checker.resolve(ret),
        ty => ty,
    };
    let ArType::Named(_, args) = ret_ty else {
        return None;
    };
    let args = checker.type_info.type_interner.type_args(args);
    let params: Vec<_> = args
        .into_iter()
        .filter_map(|arg| match checker.resolve(arg) {
            ArType::Named(id, args) if args.is_empty() => Some(id),
            _ => None,
        })
        .collect();
    (!params.is_empty()).then_some(params)
}

/// Match ergonomics: a payload reached through a shared or mutable reference
/// binds non-Copy values by reference, so checking a pattern cannot duplicate
/// ownership or move out of the borrowed scrutinee.
fn borrowed_payload_type(
    checker: &mut TypeChecker<'_>,
    source_ty: TypeId,
    payload_ty: TypeId,
) -> TypeId {
    if checker.type_info.is_copy(payload_ty) {
        return payload_ty;
    }
    match checker.resolve(source_ty) {
        ArType::Ref(_) => checker.intern(ArType::Ref(payload_ty)),
        ArType::RefMut(_) => checker.intern(ArType::RefMut(payload_ty)),
        _ => payload_ty,
    }
}

pub fn check_pattern(checker: &mut TypeChecker<'_>, pattern: PatternId, value_ty: TypeId) {
    let pat = checker.pool.pattern(pattern);
    let pattern_source_ty = value_ty;
    let borrowed_scrutinee = matches!(
        checker.resolve(value_ty),
        ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)
    );
    let value_ty = if matches!(pat, Pattern::Bind { .. }) {
        value_ty
    } else {
        let mut id = value_ty;
        for _ in 0..4 {
            match checker.resolve(id) {
                ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => id = inner,
                _ => break,
            }
        }
        id
    };
    match pat {
        Pattern::Wildcard { .. } => {}
        Pattern::Bind { span, .. } => {
            let key = crate::NodeKey::from(*span);
            if let Some(symbol_id) = checker.resolved.definitions.get(&key) {
                checker.ctx.bind(*symbol_id, value_ty);
                checker.record_decl_type(*symbol_id, value_ty);
            }
        }
        Pattern::Literal { expr, .. } => {
            let expr_ty_id = synth_expr(checker, *expr);
            if !checker.unify_ids(value_ty, expr_ty_id) {
                checker.add_constraint(
                    value_ty,
                    expr_ty_id,
                    ConstraintOrigin::Assignment {
                        lhs_span: pat.span(),
                        rhs_span: checker.pool.expr_span(*expr),
                    },
                );
            }
        }
        Pattern::Enum {
            span,
            type_name,
            variant,
            payload,
        } => {
            // Peel only to inspect the enum constructor. A plain binding must
            // retain `ref T`; globally peeling here silently turned
            // `Option<ref T>` payload bindings into owned `T` values.
            let value_ty = {
                let mut id = value_ty;
                for _ in 0..4 {
                    match checker.resolve(id) {
                        ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                            id = inner;
                        }
                        _ => break,
                    }
                }
                id
            };
            let type_key = crate::NodeKey::from(type_name.span);
            if let Some(enum_symbol_id) = checker.resolved.type_refs.get(&type_key).copied() {
                let val_ty = checker.resolve(value_ty);
                let qualifier_is_result = checker.symbols.is_result_type(enum_symbol_id);
                let qualifier_is_option = checker.symbols.is_option_type(enum_symbol_id);
                let qualifier_is_poll = checker.symbols.is_poll_type(enum_symbol_id);

                if qualifier_is_result && let ArType::Result(ok_id, err_id) = val_ty {
                    match variant.as_str() {
                        "Ok" => {
                            if payload.len != 1 {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "enum variant 'Ok' expects 1 payload item, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                            if let Some(&pat_id) = checker.pool.pattern_list(*payload).first() {
                                let payload_ty =
                                    borrowed_payload_type(checker, pattern_source_ty, ok_id);
                                check_pattern(checker, pat_id, payload_ty);
                            }
                        }
                        "Err" => {
                            if payload.len != 1 {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "enum variant 'Err' expects 1 payload item, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                            if let Some(&pat_id) = checker.pool.pattern_list(*payload).first() {
                                let payload_ty =
                                    borrowed_payload_type(checker, pattern_source_ty, err_id);
                                check_pattern(checker, pat_id, payload_ty);
                            }
                        }
                        _ => {
                            checker.diagnostics.push(crate::Diagnostic::error(
                                crate::DiagCode::T018UndefinedField,
                                format!("variant '{variant}' is not defined on Result"),
                                *span,
                            ));
                        }
                    }
                } else if qualifier_is_option && let ArType::Option(inner_id) = val_ty {
                    match variant.as_str() {
                        "Some" => {
                            if payload.len != 1 {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "enum variant 'Some' expects 1 payload item, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                            if let Some(&pat_id) = checker.pool.pattern_list(*payload).first() {
                                let payload_ty =
                                    borrowed_payload_type(checker, pattern_source_ty, inner_id);
                                check_pattern(checker, pat_id, payload_ty);
                            }
                        }
                        "None" => {
                            if !payload.is_empty() {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "enum variant 'None' expects 0 payload items, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                        }
                        _ => {
                            checker.diagnostics.push(crate::Diagnostic::error(
                                crate::DiagCode::T018UndefinedField,
                                format!("variant '{variant}' is not defined on Option"),
                                *span,
                            ));
                        }
                    }
                } else if qualifier_is_poll && let ArType::Poll(inner_id) = val_ty {
                    match variant.as_str() {
                        "Ready" => {
                            if payload.len != 1 {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "enum variant 'Ready' expects 1 payload item, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                            if let Some(&pat_id) = checker.pool.pattern_list(*payload).first() {
                                let payload_ty =
                                    borrowed_payload_type(checker, pattern_source_ty, inner_id);
                                check_pattern(checker, pat_id, payload_ty);
                            }
                        }
                        "Pending" => {
                            if !payload.is_empty() {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "enum variant 'Pending' expects 0 payload items, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                        }
                        _ => {
                            checker.diagnostics.push(crate::Diagnostic::error(
                                crate::DiagCode::T018UndefinedField,
                                format!("variant '{variant}' is not defined on Poll"),
                                *span,
                            ));
                        }
                    }
                } else {
                    let enum_args = match val_ty {
                        ArType::Named(_, args) => checker.type_info.type_interner.type_args(args),
                        _ => Vec::new(),
                    };
                    let expected_enum_ty =
                        ArType::named(enum_symbol_id, &enum_args, &checker.type_info.type_interner);
                    if !super::super::types::unify(
                        &val_ty,
                        &expected_enum_ty,
                        &checker.type_info.type_interner,
                    ) {
                        checker.add_constraint(
                            expected_enum_ty,
                            value_ty,
                            ConstraintOrigin::Assignment {
                                lhs_span: *span,
                                rhs_span: type_name.span,
                            },
                        );
                    }

                    let mut variant_symbol_opt = None;
                    for (&var_id, &(parent_id, _)) in &checker.type_info.enum_variants {
                        if parent_id == enum_symbol_id {
                            let Some(var_sym) = checker.symbols.try_get(var_id) else {
                                continue;
                            };
                            let var_name = &var_sym.name;
                            if var_name == variant || var_name.ends_with(&format!(".{}", variant)) {
                                variant_symbol_opt = Some(var_id);
                                break;
                            }
                        }
                    }
                    if let Some(variant_symbol_id) = variant_symbol_opt {
                        let shape_opt = checker
                            .type_info
                            .enum_variants
                            .get(&variant_symbol_id)
                            .cloned();
                        if let Some((_, shape)) = shape_opt {
                            match shape {
                                super::super::EnumPayloadShape::Unit => {
                                    if !payload.is_empty() {
                                        checker.diagnostics.push(crate::Diagnostic::error(
                                            crate::DiagCode::T012WrongArgCount,
                                            format!(
                                                "enum variant '{}' expects 0 payload items, found {}",
                                                variant, payload.len
                                            ),
                                            *span,
                                        ));
                                    }
                                }
                                super::super::EnumPayloadShape::Tuple(tids) => {
                                    let tids = instantiate_enum_payload(
                                        checker,
                                        enum_symbol_id,
                                        variant_symbol_id,
                                        &enum_args,
                                        &tids,
                                    );
                                    if tids.len() != payload.len as usize {
                                        checker.diagnostics.push(crate::Diagnostic::error(
                                            crate::DiagCode::T012WrongArgCount,
                                            format!(
                                                "enum variant '{}' expects {} payload items, found {}",
                                                variant,
                                                tids.len(),
                                                payload.len
                                            ),
                                            *span,
                                        ));
                                    }
                                    for (i, &pat_id) in
                                        checker.pool.pattern_list(*payload).iter().enumerate()
                                    {
                                        let expected_pat_ty_id =
                                            tids.get(i).copied().unwrap_or_else(|| {
                                                checker.type_info.type_interner.error_type_id()
                                            });
                                        let payload_ty = borrowed_payload_type(
                                            checker,
                                            pattern_source_ty,
                                            expected_pat_ty_id,
                                        );
                                        check_pattern(checker, pat_id, payload_ty);
                                    }
                                }
                            }
                        }
                    } else {
                        checker.diagnostics.push(crate::Diagnostic::error(
                            crate::DiagCode::T018UndefinedField,
                            format!(
                                "variant '{}' is not defined on enum '{}'",
                                variant,
                                type_name.path.join(".")
                            ),
                            *span,
                        ));
                    }
                }
            }
        }
        Pattern::TypeTuple {
            span,
            name,
            payload,
        } => {
            enum EnumInfo {
                Named(crate::SymbolId, Vec<TypeId>),
                Result(TypeId, TypeId),
                Option(TypeId),
                Poll(TypeId),
            }
            let enum_info = match checker.resolve(value_ty) {
                ArType::Named(enum_symbol_id, args) => Some(EnumInfo::Named(
                    enum_symbol_id,
                    checker.type_info.type_interner.type_args(args),
                )),
                ArType::Result(ok_id, err_id) => Some(EnumInfo::Result(ok_id, err_id)),
                ArType::Option(inner_id) => Some(EnumInfo::Option(inner_id)),
                ArType::Poll(inner_id) => Some(EnumInfo::Poll(inner_id)),
                _ => None,
            };
            if let Some(info) = enum_info {
                match info {
                    EnumInfo::Named(enum_symbol_id, enum_args) => {
                        let Some(enum_sym) = checker.symbols.try_get(enum_symbol_id) else {
                            return;
                        };
                        let enum_name = enum_sym.name.clone();
                        let mut variant_symbol_opt = None;
                        for (&var_id, &(parent_id, _)) in &checker.type_info.enum_variants {
                            if parent_id == enum_symbol_id {
                                let Some(var_sym) = checker.symbols.try_get(var_id) else {
                                    continue;
                                };
                                let var_name = &var_sym.name;
                                if var_name == name || var_name.ends_with(&format!(".{}", name)) {
                                    variant_symbol_opt = Some(var_id);
                                    break;
                                }
                            }
                        }
                        if let Some(variant_symbol_id) = variant_symbol_opt {
                            let shape_opt = checker
                                .type_info
                                .enum_variants
                                .get(&variant_symbol_id)
                                .cloned();
                            if let Some((_, shape)) = shape_opt {
                                match shape {
                                    super::super::EnumPayloadShape::Unit => {
                                        if !payload.is_empty() {
                                            checker.diagnostics.push(crate::Diagnostic::error(
                                                crate::DiagCode::T012WrongArgCount,
                                                format!(
                                                    "enum variant '{}' expects 0 payload items, found {}",
                                                    name, payload.len
                                                ),
                                                *span,
                                            ));
                                        }
                                    }
                                    super::super::EnumPayloadShape::Tuple(tids) => {
                                        let tids = instantiate_enum_payload(
                                            checker,
                                            enum_symbol_id,
                                            variant_symbol_id,
                                            &enum_args,
                                            &tids,
                                        );
                                        if tids.len() != payload.len as usize {
                                            checker.diagnostics.push(crate::Diagnostic::error(
                                                crate::DiagCode::T012WrongArgCount,
                                                format!(
                                                    "enum variant '{}' expects {} payload items, found {}",
                                                    name,
                                                    tids.len(),
                                                    payload.len
                                                ),
                                                *span,
                                            ));
                                        }
                                        for (i, &pat_id) in
                                            checker.pool.pattern_list(*payload).iter().enumerate()
                                        {
                                            let expected_pat_ty_id =
                                                tids.get(i).copied().unwrap_or_else(|| {
                                                    checker.type_info.type_interner.error_type_id()
                                                });
                                            let payload_ty = borrowed_payload_type(
                                                checker,
                                                pattern_source_ty,
                                                expected_pat_ty_id,
                                            );
                                            check_pattern(checker, pat_id, payload_ty);
                                        }
                                    }
                                }
                            }
                        } else {
                            checker.diagnostics.push(crate::Diagnostic::error(
                                crate::DiagCode::T018UndefinedField,
                                format!("variant '{name}' is not defined on enum '{enum_name}'"),
                                *span,
                            ));
                        }
                    }
                    EnumInfo::Result(ok_id, err_id) => match name.as_str() {
                        "Ok" => {
                            if payload.len != 1 {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "variant 'Ok' expects 1 payload item, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                            if let Some(&pat_id) = checker.pool.pattern_list(*payload).first() {
                                let payload_ty =
                                    borrowed_payload_type(checker, pattern_source_ty, ok_id);
                                check_pattern(checker, pat_id, payload_ty);
                            }
                        }
                        "Err" => {
                            if payload.len != 1 {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "variant 'Err' expects 1 payload item, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                            if let Some(&pat_id) = checker.pool.pattern_list(*payload).first() {
                                let payload_ty =
                                    borrowed_payload_type(checker, pattern_source_ty, err_id);
                                check_pattern(checker, pat_id, payload_ty);
                            }
                        }
                        _ => {
                            checker.diagnostics.push(crate::Diagnostic::error(
                                crate::DiagCode::T018UndefinedField,
                                format!("variant '{name}' is not defined on Result"),
                                *span,
                            ));
                        }
                    },
                    EnumInfo::Option(inner_id) => match name.as_str() {
                        "Some" => {
                            if payload.len != 1 {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "variant 'Some' expects 1 payload item, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                            if let Some(&pat_id) = checker.pool.pattern_list(*payload).first() {
                                let payload_ty =
                                    borrowed_payload_type(checker, pattern_source_ty, inner_id);
                                check_pattern(checker, pat_id, payload_ty);
                            }
                        }
                        "None" => {
                            if !payload.is_empty() {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "variant 'None' expects 0 payload items, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                        }
                        _ => {
                            checker.diagnostics.push(crate::Diagnostic::error(
                                crate::DiagCode::T018UndefinedField,
                                format!("variant '{name}' is not defined on Option"),
                                *span,
                            ));
                        }
                    },
                    EnumInfo::Poll(inner_id) => match name.as_str() {
                        "Ready" => {
                            if payload.len != 1 {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "variant 'Ready' expects 1 payload item, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                            if let Some(&pat_id) = checker.pool.pattern_list(*payload).first() {
                                let payload_ty =
                                    borrowed_payload_type(checker, pattern_source_ty, inner_id);
                                check_pattern(checker, pat_id, payload_ty);
                            }
                        }
                        "Pending" => {
                            if !payload.is_empty() {
                                checker.diagnostics.push(crate::Diagnostic::error(
                                    crate::DiagCode::T012WrongArgCount,
                                    format!(
                                        "variant 'Pending' expects 0 payload items, found {}",
                                        payload.len
                                    ),
                                    *span,
                                ));
                            }
                        }
                        _ => {
                            checker.diagnostics.push(crate::Diagnostic::error(
                                crate::DiagCode::T018UndefinedField,
                                format!("variant '{name}' is not defined on Poll"),
                                *span,
                            ));
                        }
                    },
                }
            } else {
                let val_ty = checker.resolve(value_ty);
                let interner = &checker.type_info.type_interner;
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T002IncompatibleAssignment,
                    format!(
                        "cannot match type tuple pattern against non-enum type '{}'",
                        val_ty.display(&checker.symbols, interner)
                    ),
                    *span,
                ));
            }
        }
        Pattern::Tuple { items, span: _ } => {
            let val_ty = checker.type_info.resolve_type_id(value_ty);
            if let ArType::Tuple(tys) = val_ty {
                let tys_cloned = checker.type_info.type_interner.type_args(tys);
                for (i, &item_id) in checker.pool.pattern_list(*items).iter().enumerate() {
                    let item_ty = tys_cloned
                        .get(i)
                        .copied()
                        .unwrap_or_else(|| checker.intern(ArType::Error));
                    // Destructuring tuple compares purely TypeIds!
                    check_pattern(checker, item_id, item_ty);
                }
            } else {
                let interner = &checker.type_info.type_interner;
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T002IncompatibleAssignment,
                    format!(
                        "cannot match tuple pattern against non-tuple type '{}'",
                        val_ty.display(&checker.symbols, interner)
                    ),
                    pat.span(),
                ));
            }
        }
        Pattern::Struct {
            type_name,
            fields,
            span: _,
        } => {
            let type_key = crate::NodeKey::from(type_name.span);
            let val_ty = checker.resolve(value_ty);
            let mut resolved_sym = checker.resolved.type_refs.get(&type_key).copied();
            if let ArType::Named(val_sym, _) = val_ty
                && type_name.path.len() == 1
                && let Some(assoc_sym) = checker
                    .symbols
                    .lookup_associated_member(val_sym, &type_name.path[0])
                && checker.type_info.struct_fields.contains_key(&assoc_sym)
            {
                resolved_sym = Some(assoc_sym);
                std::sync::Arc::make_mut(&mut checker.resolved)
                    .type_refs
                    .insert(type_key, assoc_sym);
            }
            if let Some(struct_symbol_id) = resolved_sym {
                let enum_parent = checker
                    .type_info
                    .enum_variants
                    .get(&struct_symbol_id)
                    .map(|&(enum_id, _)| enum_id);
                let expected_type_sym = enum_parent.unwrap_or(struct_symbol_id);
                let expected_struct_ty =
                    ArType::named(expected_type_sym, &[], &checker.type_info.type_interner);
                // Struct patterns match by *symbol*: the pattern syntax cannot
                // carry generic arguments (`BoxG { v }`), so a generic struct
                // pattern must accept any instantiation of the same struct. An
                // arity-exact `unify` against `Named(struct, [])` made every
                // generic struct pattern fail with a spurious T002.
                let same_struct = match val_ty {
                    ArType::Named(vid, _) => vid == expected_type_sym,
                    _ => false,
                };
                if !same_struct {
                    checker.add_constraint(
                        expected_struct_ty,
                        value_ty,
                        ConstraintOrigin::Assignment {
                            lhs_span: pat.span(),
                            rhs_span: type_name.span,
                        },
                    );
                }
                // Field types come from the *value's* instantiation
                // (`BoxG<int> { v }` binds `v` as `int`, not as the type
                // parameter symbol).
                let val_args: Vec<ArType> = match val_ty {
                    ArType::Named(_, args) => checker
                        .type_info
                        .type_interner
                        .type_args(args)
                        .iter()
                        .map(|&arg_id| checker.resolve(arg_id))
                        .collect(),
                    _ => Vec::new(),
                };
                for &field_id in checker.pool.field_pattern_list(*fields) {
                    let field = checker.pool.field_pattern(field_id);
                    checker.check_field_visibility(
                        struct_symbol_id,
                        field.name.as_str(),
                        field.span,
                    );
                    let field_ty_id_opt = types::struct_field_instantiated(
                        checker,
                        struct_symbol_id,
                        &val_args,
                        field.name.as_str(),
                    )
                    .map(|t| checker.intern(t))
                    .or_else(|| {
                        checker
                            .type_info
                            .struct_fields
                            .get(&struct_symbol_id)
                            .and_then(|df| df.get(field.name.as_str()))
                            .map(|f| f.ty)
                    });
                    if let Some(field_ty_id) = field_ty_id_opt {
                        let effective_field_ty_id = if enum_parent.is_some() {
                            borrowed_payload_type(checker, pattern_source_ty, field_ty_id)
                        } else {
                            if borrowed_scrutinee
                                && !checker.type_info.is_copy(field_ty_id)
                                && field.pattern.is_none_or(|nested| {
                                    !matches!(
                                        checker.pool.pattern(nested),
                                        Pattern::Wildcard { .. }
                                    )
                                })
                            {
                                checker.diagnostics.push(
                                    crate::Diagnostic::error(
                                        crate::DiagCode::O002MoveWhileBorrowed,
                                        "cannot bind a non-Copy field by value from a borrowed struct",
                                        field.span,
                                    )
                                    .with_note(
                                        "match the owner by value or use a borrowing field operation",
                                    ),
                                );
                            }
                            field_ty_id
                        };
                        if let Some(pat_id) = field.pattern {
                            check_pattern(checker, pat_id, effective_field_ty_id);
                        } else {
                            let key = crate::NodeKey::from(field.span);
                            if let Some(symbol_id) = checker.resolved.definitions.get(&key).copied()
                            {
                                checker.ctx.bind(symbol_id, effective_field_ty_id);
                                checker.record_decl_type(symbol_id, effective_field_ty_id);
                            }
                        }
                    } else {
                        checker.diagnostics.push(crate::Diagnostic::error(
                            crate::DiagCode::T018UndefinedField,
                            format!(
                                "field '{}' is not defined on struct '{}'",
                                field.name,
                                type_name.path.join(".")
                            ),
                            field.span,
                        ));
                    }
                }
            }
        }
        Pattern::Range { start, end, .. } => {
            let start_ty_id = synth_expr(checker, *start);
            let end_ty_id = synth_expr(checker, *end);
            if !checker.unify_ids(value_ty, start_ty_id) {
                checker.add_constraint(
                    value_ty,
                    start_ty_id,
                    ConstraintOrigin::Assignment {
                        lhs_span: pat.span(),
                        rhs_span: checker.pool.expr_span(*start),
                    },
                );
            }
            if !checker.unify_ids(value_ty, end_ty_id) {
                checker.add_constraint(
                    value_ty,
                    end_ty_id,
                    ConstraintOrigin::Assignment {
                        lhs_span: pat.span(),
                        rhs_span: checker.pool.expr_span(*end),
                    },
                );
            }
        }
        Pattern::Or { alts, .. } => {
            // SYN.4: each alternative is checked against the same scrutinee type.
            // Bindings across alts are not required to match (v0.1); prefer literals.
            for &alt in checker.pool.pattern_list(*alts) {
                check_pattern(checker, alt, value_ty);
            }
        }
    }
}
