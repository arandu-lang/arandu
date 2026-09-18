use arandu_parser::{Pattern, ast_pool::PatternId};

use super::super::TypeChecker;
use super::super::constraints::ConstraintOrigin;
use super::super::types::{self, ArType, TypeId};
use super::expr::synth_expr;

pub fn check_pattern(checker: &mut TypeChecker<'_>, pattern: PatternId, value_ty: TypeId) {
    let pat = checker.pool.pattern(pattern);
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

                if let ArType::Result(ok_id, err_id) = val_ty {
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
                                check_pattern(checker, pat_id, ok_id);
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
                                check_pattern(checker, pat_id, err_id);
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
                } else if let ArType::Option(inner_id) = val_ty {
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
                                check_pattern(checker, pat_id, inner_id);
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
                } else {
                    let expected_enum_ty =
                        ArType::named(enum_symbol_id, &[], &checker.type_info.type_interner);
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
                            let var_name = &checker.symbols.get(var_id).name;
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
                                        check_pattern(checker, pat_id, expected_pat_ty_id);
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
                Named(crate::SymbolId),
                Result(TypeId, TypeId),
                Option(TypeId),
            }
            let enum_info = match checker.resolve(value_ty) {
                ArType::Named(enum_symbol_id, _) => Some(EnumInfo::Named(enum_symbol_id)),
                ArType::Result(ok_id, err_id) => Some(EnumInfo::Result(ok_id, err_id)),
                ArType::Option(inner_id) => Some(EnumInfo::Option(inner_id)),
                _ => None,
            };
            if let Some(info) = enum_info {
                match info {
                    EnumInfo::Named(enum_symbol_id) => {
                        let enum_name = checker.symbols.get(enum_symbol_id).name.clone();
                        let mut variant_symbol_opt = None;
                        for (&var_id, &(parent_id, _)) in &checker.type_info.enum_variants {
                            if parent_id == enum_symbol_id {
                                let var_name = &checker.symbols.get(var_id).name;
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
                                            check_pattern(checker, pat_id, expected_pat_ty_id);
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
                                check_pattern(checker, pat_id, ok_id);
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
                                check_pattern(checker, pat_id, err_id);
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
                                check_pattern(checker, pat_id, inner_id);
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
            if let Some(struct_symbol_id) = checker.resolved.type_refs.get(&type_key).copied() {
                let expected_struct_ty =
                    ArType::named(struct_symbol_id, &[], &checker.type_info.type_interner);
                let val_ty = checker.resolve(value_ty);
                // Struct patterns match by *symbol*: the pattern syntax cannot
                // carry generic arguments (`BoxG { v }`), so a generic struct
                // pattern must accept any instantiation of the same struct. An
                // arity-exact `unify` against `Named(struct, [])` made every
                // generic struct pattern fail with a spurious T002.
                let same_struct = match val_ty {
                    ArType::Named(vid, _) => vid == struct_symbol_id,
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
                        if let Some(pat_id) = field.pattern {
                            check_pattern(checker, pat_id, field_ty_id);
                        } else {
                            let key = crate::NodeKey::from(field.span);
                            if let Some(symbol_id) = checker.resolved.definitions.get(&key).copied()
                            {
                                checker.ctx.bind(symbol_id, field_ty_id);
                                checker.record_decl_type(symbol_id, field_ty_id);
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
