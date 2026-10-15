//! Target-resolved, pool-independent value descriptors for one AMIR unit.

use super::*;
use arandu_middle::ctfe::FloatType;
use arandu_middle::layout::StructLayoutProvider;
use arandu_middle::types::{ArType, TypeShape, build_subst_ids, substitute_type_id};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ValueType {
    Void,
    Bool,
    Integer(IntegerType),
    Float(FloatType),
    String,
    Bytes(Primitive),
    Aggregate(Arc<AggregateType>),
    Enum(Arc<EnumType>),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct EnumType {
    pub shape: TypeShape,
    pub variants: Vec<(Option<SymbolId>, Option<ValueType>)>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AggregateType {
    pub shape: TypeShape,
    pub kind: AggregateKind,
    pub fields: Vec<ValueType>,
    pub names: Vec<smol_str::SmolStr>,
    pub symbols: Vec<Option<SymbolId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AggregateKind {
    Array,
    Tuple,
    Struct(SymbolId),
}

impl ValueType {
    pub fn shape(&self) -> TypeShape {
        match self {
            Self::Void => TypeShape::Void,
            Self::Bool => TypeShape::Primitive(Primitive::Bool),
            Self::Integer(ty) => TypeShape::Primitive(ty.primitive()),
            Self::Float(ty) => TypeShape::Primitive(ty.primitive()),
            Self::String => TypeShape::Primitive(Primitive::Str),
            Self::Bytes(ty) => TypeShape::Slice(Box::new(TypeShape::Primitive(*ty))),
            Self::Aggregate(ty) => ty.shape.clone(),
            Self::Enum(ty) => ty.shape.clone(),
        }
    }

    pub fn resolve(
        id: TypeId,
        types: &TypeInterner,
        layout: DataLayout,
        provider: Option<&dyn StructLayoutProvider>,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<Self, EvalErrorKind> {
        if depth >= TypeShape::MAX_DEPTH {
            return Err(EvalErrorKind::ValueLimit);
        }
        *remaining = remaining.checked_sub(1).ok_or(EvalErrorKind::ValueLimit)?;
        // Numeric placeholders are an inference artifact, not frozen value
        // types. Resolve their ordinary language defaults recursively at the
        // unit boundary, using the same structural helper as instance keys.
        let mut shape = TypeShape::from_id(id, types).map_err(|_| EvalErrorKind::ValueLimit)?;
        shape
            .default_numeric_literals()
            .map_err(|_| EvalErrorKind::ValueLimit)?;
        let id = shape.intern(types).map_err(|_| EvalErrorKind::ValueLimit)?;
        let recurse = |id, remaining: &mut usize| {
            Self::resolve(id, types, layout, provider, depth + 1, remaining)
        };
        let ty = types.try_resolve(id).ok_or(EvalErrorKind::InvalidIr)?;
        if matches!(
            ty,
            ArType::Array(..)
                | ArType::Tuple(_)
                | ArType::Named(..)
                | ArType::Option(_)
                | ArType::Result(..)
        ) {
            let shape = TypeShape::from_id(id, types).map_err(|_| EvalErrorKind::ValueLimit)?;
            arandu_middle::ctfe::validate_ctfe_type_shape(&shape)
                .map_err(|_| EvalErrorKind::UnsupportedType(id))?;
            if provider.is_some_and(|provider| {
                provider.is_copy_type(id) != Some(true)
                    || provider.destructor_for_type(id).is_some()
            }) {
                return Err(EvalErrorKind::UnsupportedType(id));
            }
        }
        match ty {
            ArType::Void => Ok(Self::Void),
            ArType::Primitive(Primitive::Bool) => Ok(Self::Bool),
            ArType::Primitive(Primitive::Str) => Ok(Self::String),
            ArType::Primitive(primitive) if primitive.is_float() => {
                FloatType::new(primitive, layout)
                    .map(Self::Float)
                    .map_err(|_| EvalErrorKind::UnsupportedType(id))
            }
            ArType::Primitive(primitive) => IntegerType::new(primitive, layout)
                .map(Self::Integer)
                .map_err(|error| match error {
                    ConstValueError::UnsupportedIntegerType(_) => {
                        EvalErrorKind::UnsupportedType(id)
                    }
                    error => EvalErrorKind::Value(error),
                }),
            ArType::Slice(element) => match types.try_resolve(element) {
                Some(ArType::Primitive(primitive @ (Primitive::Byte | Primitive::U8))) => {
                    Ok(Self::Bytes(primitive))
                }
                _ => Err(EvalErrorKind::UnsupportedType(id)),
            },
            ArType::Option(inner) => Ok(Self::Enum(Arc::new(EnumType {
                shape,
                variants: vec![(None, None), (None, Some(recurse(inner, remaining)?))],
            }))),
            ArType::Result(ok, error) => Ok(Self::Enum(Arc::new(EnumType {
                shape,
                variants: vec![
                    (None, Some(recurse(ok, remaining)?)),
                    (None, Some(recurse(error, remaining)?)),
                ],
            }))),
            ArType::Array(count, element) => {
                let count = usize::try_from(count).map_err(|_| EvalErrorKind::ValueLimit)?;
                if count > *remaining {
                    return Err(EvalErrorKind::ValueLimit);
                }
                let mut fields = Vec::new();
                fields
                    .try_reserve_exact(count)
                    .map_err(|_| EvalErrorKind::AllocationFailed)?;
                if count == 0 {
                    recurse(element, remaining)?;
                }
                for _ in 0..count {
                    fields.push(recurse(element, remaining)?);
                }
                Ok(Self::Aggregate(Arc::new(AggregateType {
                    shape: TypeShape::from_id(id, types).map_err(|_| EvalErrorKind::ValueLimit)?,
                    kind: AggregateKind::Array,
                    fields,
                    names: Vec::new(),
                    symbols: Vec::new(),
                })))
            }
            ArType::Tuple(args) => {
                let mut fields = Vec::new();
                for child in types.try_type_args(args).ok_or(EvalErrorKind::InvalidIr)? {
                    fields.push(recurse(child, remaining)?);
                }
                Ok(Self::Aggregate(Arc::new(AggregateType {
                    shape: TypeShape::from_id(id, types).map_err(|_| EvalErrorKind::ValueLimit)?,
                    kind: AggregateKind::Tuple,
                    fields,
                    names: Vec::new(),
                    symbols: Vec::new(),
                })))
            }
            ArType::Named(symbol, args) => {
                let Some(provider) = provider else {
                    return Err(EvalErrorKind::UnsupportedType(id));
                };
                // Immutable Arc sharing is not a substitute for ownership. Only
                // canonical POD Copy structs without observable cleanup enter.
                if provider.is_copy_type(id) != Some(true)
                    || provider.destructor_for_type(id).is_some()
                {
                    return Err(EvalErrorKind::UnsupportedType(id));
                }
                if let Some(variants) =
                    provider.get_enum_variants_for_type(&types.resolve(id), types)
                {
                    if variants.len() > *remaining {
                        return Err(EvalErrorKind::ValueLimit);
                    }
                    let arguments = types.try_type_args(args).ok_or(EvalErrorKind::InvalidIr)?;
                    let parameters = provider.get_generic_params(symbol).unwrap_or(&[]);
                    if parameters.len() != arguments.len() {
                        return Err(EvalErrorKind::UnsupportedType(id));
                    }
                    let substitution = build_subst_ids(parameters, &arguments, types);
                    let mut fields = Vec::new();
                    for (tag, variant) in variants.into_iter().enumerate() {
                        *remaining = remaining.checked_sub(1).ok_or(EvalErrorKind::ValueLimit)?;
                        let payload = variant
                            .payload_ty
                            .map(|payload| {
                                recurse(
                                    substitute_type_id(payload, &substitution, types),
                                    remaining,
                                )
                            })
                            .transpose()?;
                        // Layout-only providers may omit source identity, but
                        // frozen nominal values must retain the variant's full
                        // symbol for materialization and specialization keys.
                        let variant = provider
                            .get_enum_variant_symbol(symbol, tag)
                            .ok_or(EvalErrorKind::UnsupportedType(id))?;
                        fields.push((Some(variant), payload));
                    }
                    return Ok(Self::Enum(Arc::new(EnumType {
                        shape,
                        variants: fields,
                    })));
                }
                let table = provider
                    .get_struct_fields_for_type(&types.resolve(id), types)
                    .ok_or(EvalErrorKind::UnsupportedType(id))?;
                let arguments = types.try_type_args(args).ok_or(EvalErrorKind::InvalidIr)?;
                let parameters = provider.get_generic_params(symbol).unwrap_or(&[]);
                if parameters.len() != arguments.len() {
                    return Err(EvalErrorKind::UnsupportedType(id));
                }
                let substitution = build_subst_ids(parameters, &arguments, types);
                let mut fields = Vec::new();
                let mut names = Vec::new();
                let mut symbols = Vec::new();
                for field in table.iter() {
                    fields.push(recurse(
                        substitute_type_id(field.ty, &substitution, types),
                        remaining,
                    )?);
                    names.push(field.name.clone());
                    symbols.push(field.symbol);
                }
                Ok(Self::Aggregate(Arc::new(AggregateType {
                    shape: TypeShape::from_id(id, types).map_err(|_| EvalErrorKind::ValueLimit)?,
                    kind: AggregateKind::Struct(symbol),
                    fields,
                    names,
                    symbols,
                })))
            }
            _ => Err(EvalErrorKind::UnsupportedType(id)),
        }
    }

    pub fn accepts(&self, value: &ConstValue) -> bool {
        match (self, value) {
            (Self::Void, ConstValue::Void) | (Self::Bool, ConstValue::Bool(_)) => true,
            (Self::Integer(ty), ConstValue::Integer(value)) => *ty == value.ty(),
            (Self::Float(ty), ConstValue::Float(value)) => *ty == value.ty(),
            (Self::String, ConstValue::String(_)) | (Self::Bytes(_), ConstValue::Bytes(_)) => true,
            (Self::Aggregate(ty), ConstValue::Aggregate(value)) => {
                value.variant().is_none()
                    && ty.shape == *value.shape()
                    && ty.fields.len() == value.values().len()
                    && ty
                        .fields
                        .iter()
                        .zip(value.values())
                        .all(|(ty, value)| ty.accepts(value))
            }
            (Self::Enum(ty), ConstValue::Aggregate(value)) => {
                let Some(variant) = value.variant() else {
                    return false;
                };
                ty.shape == *value.shape()
                    && ty
                        .variants
                        .get(variant.tag)
                        .is_some_and(|(symbol, payload)| {
                            *symbol == variant.symbol
                                && match (payload, value.values()) {
                                    (None, []) => true,
                                    (Some(ty), [value]) => ty.accepts(value),
                                    _ => false,
                                }
                        })
            }
            _ => false,
        }
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        match self {
            Self::Void => ConstValue::Void.canonical_bytes(),
            Self::Bool => ConstValue::Bool(false).canonical_bytes(),
            Self::Integer(integer) => integer.canonical_bytes().to_vec(),
            Self::Float(float) => float.canonical_bytes().to_vec(),
            Self::String => vec![2, 4],
            Self::Bytes(primitive) => vec![2, 5, u8::from(*primitive == Primitive::U8)],
            Self::Enum(ty) => {
                let mut bytes = vec![2, 6];
                if let Ok(shape) = arandu_middle::ctfe::canonical_type_bytes(&ty.shape) {
                    bytes.extend(shape);
                }
                bytes.extend_from_slice(&(ty.variants.len() as u64).to_le_bytes());
                for (symbol, payload) in &ty.variants {
                    bytes.push(u8::from(symbol.is_some()));
                    if let Some(symbol) = symbol {
                        bytes.extend_from_slice(&symbol.file_id.to_le_bytes());
                        bytes.extend_from_slice(&symbol.local_id.0.to_le_bytes());
                    }
                    bytes.push(u8::from(payload.is_some()));
                    if let Some(payload) = payload {
                        let child = payload.canonical_bytes();
                        bytes.extend_from_slice(&(child.len() as u64).to_le_bytes());
                        bytes.extend(child);
                    }
                }
                bytes
            }
            Self::Aggregate(ty) => {
                let mut bytes = vec![2, 3];
                if let Ok(shape) = arandu_middle::ctfe::canonical_type_bytes(&ty.shape) {
                    bytes.extend(shape);
                }
                bytes.extend_from_slice(&(ty.fields.len() as u64).to_le_bytes());
                for field in &ty.fields {
                    let child = field.canonical_bytes();
                    bytes.extend_from_slice(&(child.len() as u64).to_le_bytes());
                    bytes.extend(child);
                }
                // Field projection uses source symbols/names. Reordering two
                // fields with equal physical types must invalidate admission,
                // rather than cutting off at an identical list of child types.
                for (name, symbol) in ty.names.iter().zip(&ty.symbols) {
                    bytes.extend_from_slice(&(name.len() as u64).to_le_bytes());
                    bytes.extend_from_slice(name.as_bytes());
                    bytes.push(u8::from(symbol.is_some()));
                    if let Some(symbol) = symbol {
                        bytes.extend_from_slice(&symbol.file_id.to_le_bytes());
                        bytes.extend_from_slice(&symbol.local_id.0.to_le_bytes());
                    }
                }
                bytes
            }
        }
    }
}
