use arandu_lexer::Span;
use arandu_parser::TypeName;
use arandu_parser::ast_pool::IndexRange;
use smol_str::SmolStr;

use super::super::super::TypeChecker;
use super::super::super::constraints::ConstraintOrigin;
use super::super::super::types::{ArType, TypeId};
use super::{borrowed_payload_type, check_pattern, instantiate_enum_payload};

pub(super) fn check_enum(
    checker: &mut TypeChecker<'_>,
    span: &Span,
    type_name: &TypeName,
    variant: &SmolStr,
    payload: &IndexRange,
    value_ty: TypeId,
    pattern_source_ty: TypeId,
) {
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
                        let payload_ty = borrowed_payload_type(checker, pattern_source_ty, ok_id);
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
                        let payload_ty = borrowed_payload_type(checker, pattern_source_ty, err_id);
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
            if !super::super::super::types::unify(
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
                        super::super::super::EnumPayloadShape::Unit => {
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
                        super::super::super::EnumPayloadShape::Tuple(tids) => {
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
