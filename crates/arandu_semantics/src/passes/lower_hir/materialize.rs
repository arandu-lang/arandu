//! The scalar CTFE/residual boundary. Values carry no arena IDs; the caller
//! supplies the already checked type in the destination HIR context. This is
//! not constant folding, a cast, or permission to execute the original root.

use arandu_middle::{
    Span,
    ctfe::{ConstValue, ConstValueError, IntegerType},
    hir::{HirExpr, HirExprKind},
    layout::{DataLayout, DataLayoutError},
    types::{ArType, Primitive, TypeId, TypeInterner},
};

/// Internal boundary failures. The public staging caller must supply its
/// source context/diagnostic; none of these is a new user-facing error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializationError {
    InvalidLayout(DataLayoutError),
    InvalidType(TypeId),
    TypeMismatch,
    InvalidIntegerType(ConstValueError),
    TargetWidthMismatch {
        evaluated: IntegerType,
        destination: IntegerType,
    },
}

/// Materialize an admitted scalar as an ordinary, typed HIR literal. The
/// destination type must match exactly: coercions must have happened during
/// initial typing/evaluation, not be invented at this boundary. In particular,
/// a ptr4 `usize` value cannot be reused under ptr8, even when its number fits.
///
/// No HIR/AST pool is mutated or cloned, and no interpreter handles/addresses
/// escape. The decimal text is the existing HIR integer representation; it
/// preserves all mathematical bits, including `u64::MAX` and signed minima.
pub fn materialize_ctfe_scalar(
    value: ConstValue,
    expected: TypeId,
    types: &TypeInterner,
    layout: DataLayout,
    span: Span,
) -> Result<HirExpr, MaterializationError> {
    layout
        .validate()
        .map_err(MaterializationError::InvalidLayout)?;
    let expected_type = types
        .try_resolve(expected)
        .ok_or(MaterializationError::InvalidType(expected))?;
    let kind = match value {
        // Nil with the explicit Void type is the canonical HIR unit value,
        // not a nullable/reference value and not an Option constructor.
        ConstValue::Void if expected_type == ArType::Void => HirExprKind::Nil,
        ConstValue::Bool(boolean) if expected_type == ArType::Primitive(Primitive::Bool) => {
            HirExprKind::Bool(boolean)
        }
        ConstValue::Integer(integer) => {
            let ArType::Primitive(primitive) = expected_type else {
                return Err(MaterializationError::TypeMismatch);
            };
            if primitive != integer.ty().primitive() {
                return Err(MaterializationError::TypeMismatch);
            }
            let destination = IntegerType::new(primitive, layout)
                .map_err(MaterializationError::InvalidIntegerType)?;
            if destination != integer.ty() {
                return Err(MaterializationError::TargetWidthMismatch {
                    evaluated: integer.ty(),
                    destination,
                });
            }
            HirExprKind::Int(integer.value().to_string().into())
        }
        ConstValue::Float(value) if expected_type == ArType::Primitive(value.ty().primitive()) => {
            let destination = arandu_middle::ctfe::FloatType::new(value.ty().primitive(), layout)
                .map_err(|_| MaterializationError::TypeMismatch)?;
            if destination != value.ty() {
                return Err(MaterializationError::TypeMismatch);
            }
            HirExprKind::FloatBits(value)
        }
        ConstValue::String(value) if expected_type == ArType::Primitive(Primitive::Str) => {
            HirExprKind::Str(value.as_str().into())
        }
        ConstValue::Bytes(value) if matches!(expected_type, ArType::Slice(inner) if matches!(types.try_resolve(inner), Some(ArType::Primitive(Primitive::U8 | Primitive::Byte)))) => {
            HirExprKind::FrozenBytes(value)
        }
        ConstValue::Void
        | ConstValue::Bool(_)
        | ConstValue::Aggregate(_)
        | ConstValue::Float(_)
        | ConstValue::String(_)
        | ConstValue::Bytes(_) => {
            return Err(MaterializationError::TypeMismatch);
        }
    };
    Ok(HirExpr {
        kind,
        ty: expected,
        span,
    })
}

/// Freeze a semantic product into ordinary destination HIR. All child type IDs
/// are reconstructed from that destination, never copied from the VM unit.
pub fn materialize_ctfe_value(
    value: &ConstValue,
    expected: TypeId,
    info: &arandu_typeck::type_checker::TypeInfo,
    pool: &mut arandu_middle::hir::HirPool,
    layout: DataLayout,
    span: Span,
) -> Result<HirExpr, MaterializationError> {
    // The target/type/source context is fixed throughout the traversal. Keep
    // mutable destination storage and the shared structural allowance separate.
    struct Destination<'a> {
        info: &'a arandu_typeck::type_checker::TypeInfo,
        layout: DataLayout,
        span: Span,
    }

    fn materialize(
        value: &ConstValue,
        expected: TypeId,
        destination: &Destination<'_>,
        pool: &mut arandu_middle::hir::HirPool,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<HirExpr, MaterializationError> {
        let info = destination.info;
        let layout = destination.layout;
        let span = destination.span;
        if depth >= arandu_middle::types::TypeShape::MAX_DEPTH {
            return Err(MaterializationError::TypeMismatch);
        }
        *remaining = remaining
            .checked_sub(1)
            .ok_or(MaterializationError::TypeMismatch)?;
        let mut shape = arandu_middle::types::TypeShape::from_id(expected, &info.type_interner)
            .map_err(|_| MaterializationError::TypeMismatch)?;
        shape
            .default_numeric_literals()
            .map_err(|_| MaterializationError::TypeMismatch)?;
        let expected = shape
            .intern(&info.type_interner)
            .map_err(|_| MaterializationError::TypeMismatch)?;
        let ConstValue::Aggregate(aggregate) = value else {
            return materialize_ctfe_scalar(
                value.clone(),
                expected,
                &info.type_interner,
                layout,
                span,
            );
        };
        if arandu_middle::types::TypeShape::from_id(expected, &info.type_interner)
            .ok()
            .as_ref()
            != Some(aggregate.shape())
            || !info.is_copy(expected)
        {
            return Err(MaterializationError::TypeMismatch);
        }
        if let Some(variant) = aggregate.variant() {
            use arandu_middle::hir::ResultCtorVariant;
            use arandu_middle::layout::StructLayoutProvider;
            if info.destructor_for_type(expected).is_some() {
                return Err(MaterializationError::TypeMismatch);
            }
            let expected_type = info
                .type_interner
                .try_resolve(expected)
                .ok_or(MaterializationError::InvalidType(expected))?;
            let (symbol, payload_ty, builtin) = match expected_type {
                ArType::Option(inner) => match variant.tag {
                    0 => (None, None, Some(ResultCtorVariant::None)),
                    1 => (None, Some(inner), Some(ResultCtorVariant::Some)),
                    _ => return Err(MaterializationError::TypeMismatch),
                },
                ArType::Result(ok, error) => match variant.tag {
                    0 => (None, Some(ok), Some(ResultCtorVariant::Ok)),
                    1 => (None, Some(error), Some(ResultCtorVariant::Err)),
                    _ => return Err(MaterializationError::TypeMismatch),
                },
                ArType::Named(owner, _) => {
                    let symbol = info
                        .get_enum_variant_symbol(owner, variant.tag)
                        .ok_or(MaterializationError::TypeMismatch)?;
                    let declared = info
                        .get_enum_variants_for_type(&expected_type, &info.type_interner)
                        .and_then(|variants| variants.get(variant.tag).cloned())
                        .ok_or(MaterializationError::TypeMismatch)?;
                    let payload = declared
                        .payload_ty
                        .map(|_| {
                            arandu_middle::layout::instantiated_enum_variant_payload_type(
                                &expected_type,
                                variant.tag,
                                &info.type_interner,
                                info,
                            )
                            .ok_or(MaterializationError::TypeMismatch)
                        })
                        .transpose()?;
                    (Some(symbol), payload, None)
                }
                _ => return Err(MaterializationError::TypeMismatch),
            };
            if symbol != variant.symbol {
                return Err(MaterializationError::TypeMismatch);
            }
            let payload = match (payload_ty, aggregate.values()) {
                (None, []) => None,
                (Some(ty), [value]) => Some(materialize(
                    value,
                    ty,
                    destination,
                    pool,
                    depth + 1,
                    remaining,
                )?),
                _ => return Err(MaterializationError::TypeMismatch),
            };
            let kind = if let Some(variant) = builtin {
                let child = payload.unwrap_or(HirExpr {
                    kind: HirExprKind::Bool(false),
                    ty: info
                        .type_interner
                        .intern(ArType::Primitive(Primitive::Bool)),
                    span,
                });
                HirExprKind::ResultCtor {
                    variant,
                    value: pool.alloc_expr(child),
                }
            } else {
                let symbol = symbol.ok_or(MaterializationError::TypeMismatch)?;
                let callee = pool.alloc_expr(HirExpr {
                    kind: HirExprKind::Path { symbol },
                    ty: info.decl_type_id(symbol).unwrap_or(expected),
                    span,
                });
                let args = match payload {
                    Some(HirExpr {
                        kind: HirExprKind::Tuple { items },
                        ..
                    }) => items,
                    Some(child) => {
                        let id = pool.alloc_expr(child);
                        pool.alloc_expr_list(&[id])
                    }
                    None => pool.alloc_expr_list(&[]),
                };
                HirExprKind::Call {
                    callee,
                    args,
                    trailing_block: None,
                }
            };
            return Ok(HirExpr {
                kind,
                ty: expected,
                span,
            });
        }
        let mut children = Vec::new();
        let mut names = Vec::new();
        let expected_type = info
            .type_interner
            .try_resolve(expected)
            .ok_or(MaterializationError::InvalidType(expected))?;
        match expected_type {
            ArType::Array(count, element) => {
                if usize::try_from(count).ok() != Some(aggregate.values().len()) {
                    return Err(MaterializationError::TypeMismatch);
                }
                children.resize(aggregate.values().len(), element);
            }
            ArType::Tuple(arguments) => {
                children = info
                    .type_interner
                    .try_type_args(arguments)
                    .ok_or(MaterializationError::TypeMismatch)?
            }
            ArType::Named(symbol, arguments) => {
                use arandu_middle::layout::StructLayoutProvider;
                if info.destructor_for_type(expected).is_some() {
                    return Err(MaterializationError::TypeMismatch);
                }
                let args = info
                    .type_interner
                    .try_type_args(arguments)
                    .ok_or(MaterializationError::TypeMismatch)?;
                let table = info
                    .fields_for(symbol, &args)
                    .ok_or(MaterializationError::TypeMismatch)?;
                let parameters = info
                    .generic_params
                    .get(&symbol)
                    .map_or(&[][..], |parameters| parameters.as_slice());
                if parameters.len() != args.len() {
                    return Err(MaterializationError::TypeMismatch);
                }
                let substitution =
                    arandu_middle::types::build_subst_ids(parameters, &args, &info.type_interner);
                for field in table.iter() {
                    children.push(arandu_middle::types::substitute_type_id(
                        field.ty,
                        &substitution,
                        &info.type_interner,
                    ));
                    names.push(field.name.clone());
                }
            }
            _ => return Err(MaterializationError::TypeMismatch),
        }
        if children.len() != aggregate.values().len() {
            return Err(MaterializationError::TypeMismatch);
        }
        let mut expressions = Vec::new();
        for (child, value) in children.into_iter().zip(aggregate.values()) {
            let child = materialize(value, child, destination, pool, depth + 1, remaining)?;
            expressions.push(pool.alloc_expr(child));
        }
        let kind = match expected_type {
            ArType::Array(..) => HirExprKind::Array {
                items: pool.alloc_expr_list(&expressions),
            },
            ArType::Tuple(..) => HirExprKind::Tuple {
                items: pool.alloc_expr_list(&expressions),
            },
            ArType::Named(symbol, _) => {
                let fields = names
                    .into_iter()
                    .zip(expressions)
                    .map(|(name, value)| arandu_middle::hir::HirFieldInit { name, value, span })
                    .collect::<Vec<_>>();
                HirExprKind::StructLiteral {
                    struct_symbol: symbol,
                    fields: pool.alloc_field_init_list(&fields),
                }
            }
            _ => return Err(MaterializationError::TypeMismatch),
        };
        Ok(HirExpr {
            kind,
            ty: expected,
            span,
        })
    }
    let mut remaining = arandu_middle::types::TypeShape::MAX_NODES;
    let destination = Destination { info, layout, span };
    materialize(value, expected, &destination, pool, 0, &mut remaining)
}
