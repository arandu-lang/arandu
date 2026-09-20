use crate::diagnostics::Diagnostic;
use crate::hir::{HirPlace, HirPlaceSuffix};
use crate::passes::type_checker::types::ArType;
use crate::{NodeKey, TypeCheckResult};
use arandu_middle::types::{TypeId, TypeInterner};
use arandu_parser::ast_pool::AstPool;
use arandu_parser::{Place, PlaceSuffix};

pub(crate) fn lower_place(
    type_check: &mut TypeCheckResult,
    pool: &AstPool,
    hir_pool: &mut crate::hir::HirPool,
    place: &Place,
) -> Result<HirPlace, Diagnostic> {
    let root_key = NodeKey::from(place.span);
    let root_symbol = type_check
        .resolved
        .value_refs
        .get(&root_key)
        .copied()
        .or_else(|| type_check.resolved.definitions.get(&root_key).copied())
        .ok_or_else(|| {
            Diagnostic::error(
                crate::diagnostics::DiagCode::L001LoweringUnresolvedSymbol,
                "cannot lower place: root symbol not resolved",
                place.span,
            )
        })?;

    let error_id = TypeInterner::preinterned_error_id();
    let mut current_ty: TypeId = type_check
        .type_info
        .decl_type_id(root_symbol)
        .unwrap_or(error_id);

    let mut suffixes = Vec::new();
    for suffix in &place.suffixes {
        if current_ty == error_id {
            match suffix {
                PlaceSuffix::Field { span, name } => {
                    suffixes.push(HirPlaceSuffix::Field {
                        span: *span,
                        name: name.clone(),
                        field_symbol: None,
                        ty: error_id,
                    });
                }
                PlaceSuffix::Index { span, expr } => {
                    let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
                    suffixes.push(HirPlaceSuffix::Index {
                        span: *span,
                        expr: eid,
                        ty: error_id,
                    });
                }
            }
            continue;
        }

        let interner = &type_check.type_info.type_interner;
        match suffix {
            PlaceSuffix::Field { span, name } => {
                let base = interner.resolve(current_ty);
                // Auto-deref: Nullable / & / &mut / ptr (shared|mut self places).
                let mut actual_base_ty = match base {
                    ArType::Nullable(inner) => interner.resolve(inner),
                    other => other,
                };
                for _ in 0..4 {
                    actual_base_ty = match actual_base_ty {
                        ArType::Ref(inner) | ArType::RefMut(inner) | ArType::Ptr(inner) => {
                            interner.resolve(inner)
                        }
                        other => other,
                    };
                    if !matches!(
                        actual_base_ty,
                        ArType::Ref(_) | ArType::RefMut(_) | ArType::Ptr(_)
                    ) {
                        break;
                    }
                }
                let struct_id_opt = match &actual_base_ty {
                    ArType::Named(id, _) => Some(*id),
                    ArType::Ptr(inner) => match interner.resolve(*inner) {
                        ArType::Named(id, _) => Some(id),
                        _ => None,
                    },
                    _ => None,
                };
                let (field_ty, field_symbol) = if let Some(struct_id) = struct_id_opt
                    && let Some(fields) = type_check.type_info.struct_fields.get(&struct_id)
                    && let Some(f) = fields.get(name.as_str())
                {
                    let symbol = f.symbol;
                    (f.ty, symbol)
                } else {
                    (error_id, None)
                };
                current_ty = field_ty;
                suffixes.push(HirPlaceSuffix::Field {
                    span: *span,
                    name: name.clone(),
                    field_symbol,
                    ty: field_ty,
                });
            }
            PlaceSuffix::Index { span, expr } => {
                let base = interner.resolve(current_ty);
                let actual_base_ty = match base {
                    ArType::Nullable(inner) => interner.resolve(inner),
                    other => other,
                };
                let elem_ty = match &actual_base_ty {
                    ArType::Array(_, inner)
                    | ArType::ConstArray(_, inner)
                    | ArType::Slice(inner) => *inner,
                    _ => error_id,
                };
                current_ty = elem_ty;
                let eid = super::expr::lower_expr(type_check, pool, hir_pool, *expr)?;
                suffixes.push(HirPlaceSuffix::Index {
                    span: *span,
                    expr: eid,
                    ty: elem_ty,
                });
            }
        }
    }

    Ok(HirPlace {
        root_symbol,
        suffixes: suffixes.into(),
        ty: current_ty,
        span: place.span,
    })
}
