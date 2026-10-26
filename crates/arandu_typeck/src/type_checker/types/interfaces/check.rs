use super::super::build_subst;
use super::super::{ArType, TypeId};
use super::satisfy::{missing_methods_note, type_satisfies_interface};
use crate::passes::type_checker::TypeChecker;
use crate::{SymbolId, SymbolKind};
use arandu_lexer::Span;
use smallvec::SmallVec;

/// After monomorphic instantiation, verify each type argument satisfies its constraints.
pub(crate) fn check_instantiation_constraints(
    checker: &mut TypeChecker,
    param_symbols: &[SymbolId],
    arg_types: &[ArType],
    span: Span,
) {
    for (&param_sym, arg_ty) in param_symbols.iter().zip(arg_types) {
        if matches!(arg_ty, ArType::Error) {
            continue;
        }
        let Some(parameter) = checker.symbols.try_get(param_sym) else {
            continue;
        };
        let valid_kind = match parameter.kind {
            SymbolKind::ConstParam => matches!(
                arg_ty,
                ArType::Const(_) | ArType::FrozenConst(_) | ArType::ConstParam(_)
            ),
            SymbolKind::TypeParam => !matches!(
                arg_ty,
                ArType::Const(_) | ArType::FrozenConst(_) | ArType::ConstParam(_)
            ),
            _ => true,
        };
        if !valid_kind {
            let expected = if parameter.kind == SymbolKind::ConstParam {
                "a compile-time scalar value"
            } else {
                "a type"
            };
            checker.diagnostics.push(crate::Diagnostic::error(
                crate::DiagCode::T011GenericConstraintNotSatisfied,
                format!("generic parameter '{}' expects {expected}", parameter.name),
                span,
            ));
        }
        if parameter.kind == SymbolKind::ConstParam
            && let ArType::FrozenConst(value) = arg_ty
            && let Some(expected) = checker.type_info.decl_type_id(param_sym)
        {
            let valid = match (value.as_ref(), checker.resolve(expected)) {
                (
                    arandu_middle::ctfe::ConstValue::Bool(_),
                    ArType::Primitive(super::super::Primitive::Bool),
                ) => true,
                (arandu_middle::ctfe::ConstValue::Integer(value), ArType::Primitive(primitive))
                    if primitive.is_integer() =>
                {
                    arandu_middle::ctfe::IntegerType::new(
                        primitive,
                        checker.type_info.target_layout,
                    )
                    .ok()
                    .is_some_and(|ty| value.cast(ty).is_ok())
                }
                (arandu_middle::ctfe::ConstValue::Aggregate(value), _) => {
                    arandu_middle::types::TypeShape::from_id(
                        expected,
                        &checker.type_info.type_interner,
                    )
                    .ok()
                    .as_ref()
                        == Some(value.shape())
                }
                _ => false,
            };
            if !valid {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T011GenericConstraintNotSatisfied,
                    format!(
                        "constant argument does not match the type of parameter '{}'",
                        parameter.name
                    ),
                    span,
                ));
            }
        }
        if parameter.kind == SymbolKind::ConstParam
            && let ArType::Const(value) = arg_ty
            && let Some(ArType::Primitive(primitive)) = checker
                .type_info
                .decl_types
                .get(&param_sym)
                .map(|&id| checker.resolve(id))
        {
            // Use the same checked mathematical integer domain as CTFE. The
            // instance key's u64 storage is not the parameter's declared range.
            // Only integer range is queried here, not aggregate size or ABI.
            let width = match checker.target_info.pointer_width {
                32 => Some(4),
                64 => Some(8),
                _ => None,
            };
            let fits = width
                .and_then(|width| {
                    arandu_middle::ctfe::IntegerType::new(
                        primitive,
                        arandu_middle::DataLayout::ptr_width(width),
                    )
                    .ok()
                })
                .is_some_and(|ty| i128::from(*value) <= ty.max());
            if !fits {
                checker.diagnostics.push(crate::Diagnostic::error(
                    crate::DiagCode::T011GenericConstraintNotSatisfied,
                    format!("constant argument {value} is outside the range of '{}' for parameter '{}'", primitive.as_str(), parameter.name),
                    span,
                ).with_primary_label("constant does not fit the declared parameter type")
                 .with_label(parameter.span, "constant parameter declared here"));
            }
        }
        if parameter.kind == SymbolKind::ConstParam
            && let ArType::ConstParam(source) = arg_ty
            && let Some(source_domain) = constant_parameter_domain(checker, *source)
            && let Some(destination_domain) = constant_parameter_domain(checker, param_sym)
            && (source_domain.min() < destination_domain.min()
                || source_domain.max() > destination_domain.max())
            && let Some(source_parameter) = checker.symbols.try_get(*source)
        {
            // The source's entire admitted domain, not a particular caller's
            // lucky value, must fit the callee, including negative values.
            checker.diagnostics.push(
                crate::Diagnostic::error(
                    crate::DiagCode::T011GenericConstraintNotSatisfied,
                    format!(
                        "constant parameter '{}' has values outside the range of parameter '{}'",
                        source_parameter.name, parameter.name
                    ),
                    span,
                )
                .with_primary_label("forwarded constant range is too wide")
                .with_label(
                    source_parameter.span,
                    "source constant parameter declared here",
                )
                .with_label(
                    parameter.span,
                    "destination constant parameter declared here",
                )
                .with_hint(
                    "declare the source parameter with an integer range that fits the destination",
                ),
            );
        }
    }

    // Bounds may reference another parameter of this declaration (J: Job<R>).
    // Instantiate that obligation in the caller's type environment before
    // comparing it with the caller's declared bounds. Build only when needed.
    let mut substitution = None;
    for (param_sym, arg_ty) in param_symbols.iter().zip(arg_types) {
        if matches!(arg_ty, ArType::Error) {
            continue;
        }
        let constraints = checker.type_info.param_constraints.get(param_sym).cloned();
        let Some(constraints) = constraints else {
            continue;
        };
        for bound in constraints.iter() {
            let bound_args: SmallVec<[TypeId; 2]> = if bound.type_args.is_empty() {
                SmallVec::new()
            } else {
                let subst =
                    substitution.get_or_insert_with(|| build_subst(param_symbols, arg_types));
                bound
                    .type_args
                    .iter()
                    .map(|&id| {
                        arandu_middle::types::subst::substitute_type_id(
                            id,
                            subst,
                            &checker.type_info.type_interner,
                        )
                    })
                    .collect()
            };
            if !type_satisfies_interface(checker, arg_ty, bound.iface_sym, &bound_args, span) {
                let mut iface_name = checker
                    .symbols
                    .try_get(bound.iface_sym)
                    .map(|s| s.name.to_string())
                    .unwrap_or_else(|| "Interface".to_string());
                if !bound_args.is_empty() {
                    iface_name.push('<');
                    for (index, &argument) in bound_args.iter().enumerate() {
                        if index != 0 {
                            iface_name.push_str(", ");
                        }
                        iface_name.push_str(
                            &checker
                                .resolve(argument)
                                .display(&checker.symbols, &checker.type_info.type_interner),
                        );
                    }
                    iface_name.push('>');
                }
                let ty_display = arg_ty.display(&checker.symbols, &checker.type_info.type_interner);
                let detail = missing_methods_note(checker, arg_ty, bound.iface_sym, &bound_args);
                // Put the method-level root cause in the primary message (notes are easy to miss).
                let diag = crate::Diagnostic::error(
                    crate::DiagCode::T025InterfaceNotSatisfied,
                    format!(
                        "type '{ty_display}' does not satisfy interface '{iface_name}': {detail}"
                    ),
                    span,
                )
                .with_note(detail);
                checker.diagnostics.push(diag);
            }
        }
    }
}

fn constant_parameter_domain(
    checker: &TypeChecker<'_>,
    symbol: SymbolId,
) -> Option<arandu_middle::ctfe::IntegerType> {
    let ArType::Primitive(primitive) = checker.resolve(*checker.type_info.decl_types.get(&symbol)?)
    else {
        return None;
    };
    let width = match checker.target_info.pointer_width {
        32 => 4,
        64 => 8,
        _ => return None,
    };
    arandu_middle::ctfe::IntegerType::new(primitive, arandu_middle::DataLayout::ptr_width(width))
        .ok()
}
