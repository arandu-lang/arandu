//! Immutable, pool-independent aggregate values. Sharing does not prove Copy;
//! the VM admits aggregate storage only after the canonical language proof.

use super::{Arc, ConstValue, ConstValueError, TypeShape};
use crate::SymbolId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstAggregate {
    shape: TypeShape,
    values: Arc<[ConstValue]>,
    variant: Option<ConstVariant>,
}

/// A semantic variant identity, independent of storage layout and arena IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConstVariant {
    pub tag: usize,
    pub symbol: Option<SymbolId>,
}

impl ConstAggregate {
    /// Freeze an enum. Nominal payload types and Copy are verified at admission;
    /// structural Option/Result payloads are also checked at this boundary.
    pub fn enumeration(
        shape: TypeShape,
        variant: ConstVariant,
        payload: Option<ConstValue>,
    ) -> Result<Self, ConstValueError> {
        validate_ctfe_type_shape(&shape)?;
        if variant.tag >= TypeShape::MAX_NODES {
            return Err(ConstValueError::StructuralLimit);
        }
        let valid = match &shape {
            TypeShape::Named(..) => variant.symbol.is_some(),
            TypeShape::Option(inner) => {
                variant.symbol.is_none()
                    && match (variant.tag, &payload) {
                        (1, Some(value)) => shape_accepts(inner, value),
                        (0, None) => true,
                        _ => false,
                    }
            }
            TypeShape::Result(ok, error) => {
                variant.symbol.is_none()
                    && match (variant.tag, &payload) {
                        (0, Some(value)) => shape_accepts(ok, value),
                        (1, Some(value)) => shape_accepts(error, value),
                        _ => false,
                    }
            }
            _ => false,
        };
        if !valid {
            return Err(ConstValueError::InvalidAggregateShape);
        }
        Self::freeze(shape, payload.into_iter().collect(), Some(variant))
    }

    #[must_use]
    pub fn variant(&self) -> Option<ConstVariant> {
        self.variant
    }

    pub fn new(shape: TypeShape, values: Vec<ConstValue>) -> Result<Self, ConstValueError> {
        validate_ctfe_type_shape(&shape)?;
        match &shape {
            TypeShape::Array(count, element)
                if usize::try_from(*count).ok() == Some(values.len()) =>
            {
                if !values.iter().all(|value| shape_accepts(element, value)) {
                    return Err(ConstValueError::InvalidAggregateShape);
                }
            }
            TypeShape::Tuple(elements) if elements.len() == values.len() => {
                if !elements
                    .iter()
                    .zip(&values)
                    .all(|(shape, value)| shape_accepts(shape, value))
                {
                    return Err(ConstValueError::InvalidAggregateShape);
                }
            }
            TypeShape::Named(_, _) => {}
            _ => return Err(ConstValueError::InvalidAggregateShape),
        }
        Self::freeze(shape, values, None)
    }

    fn freeze(
        shape: TypeShape,
        values: Vec<ConstValue>,
        variant: Option<ConstVariant>,
    ) -> Result<Self, ConstValueError> {
        shape
            .for_each_symbol(|_| {})
            .map_err(|_| ConstValueError::StructuralLimit)?;
        fn inspect(
            value: &ConstValue,
            depth: usize,
            remaining: &mut usize,
        ) -> Result<(), ConstValueError> {
            if depth >= TypeShape::MAX_DEPTH {
                return Err(ConstValueError::StructuralLimit);
            }
            *remaining = remaining
                .checked_sub(1)
                .ok_or(ConstValueError::StructuralLimit)?;
            if let ConstValue::Aggregate(aggregate) = value {
                for child in aggregate.values() {
                    inspect(child, depth + 1, remaining)?;
                }
            }
            Ok(())
        }
        let mut remaining = TypeShape::MAX_NODES - 1;
        for value in &values {
            inspect(value, 1, &mut remaining)?;
        }
        Ok(Self {
            shape,
            values: values.into(),
            variant,
        })
    }

    #[must_use]
    pub fn shape(&self) -> &TypeShape {
        &self.shape
    }

    #[must_use]
    pub fn values(&self) -> &[ConstValue] {
        &self.values
    }

    /// Versioned semantic encoding, never physical padding or interpreter IDs.
    /// Types are encoded structurally; nominal identity retains the full source
    /// symbol. Construction/admission bound nesting before this frozen bridge.
    pub(super) fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![2, if self.variant.is_some() { 6 } else { 3 }];
        if let Some(variant) = self.variant {
            bytes.extend_from_slice(&u64::try_from(variant.tag).unwrap_or(u64::MAX).to_le_bytes());
            bytes.push(u8::from(variant.symbol.is_some()));
            if let Some(symbol) = variant.symbol {
                bytes.extend_from_slice(&symbol.file_id.to_le_bytes());
                bytes.extend_from_slice(&symbol.local_id.0.to_le_bytes());
            }
        }
        encode_shape(&self.shape, &mut bytes);
        bytes.extend_from_slice(&(self.values.len() as u64).to_le_bytes());
        for value in self.values.iter() {
            let child = value.canonical_bytes();
            bytes.extend_from_slice(&(child.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&child);
        }
        bytes
    }
}

#[cfg(test)]
mod enum_tests {
    use super::*;

    #[test]
    fn nominal_enums_require_full_variant_identity_in_the_frozen_bridge() {
        let shape = TypeShape::Named(SymbolId::new(1, 0), Vec::new());
        assert_eq!(
            ConstAggregate::enumeration(
                shape.clone(),
                ConstVariant {
                    tag: 0,
                    symbol: None
                },
                None
            ),
            Err(ConstValueError::InvalidAggregateShape)
        );
        let a = ConstAggregate::enumeration(
            shape.clone(),
            ConstVariant {
                tag: 0,
                symbol: Some(SymbolId::new(1, 1)),
            },
            None,
        );
        let b = ConstAggregate::enumeration(
            shape,
            ConstVariant {
                tag: 0,
                symbol: Some(SymbolId::new(2, 1)),
            },
            None,
        );
        assert!(a.is_ok() && b.is_ok());
        assert_ne!(a, b);
        assert_ne!(
            a.map(|value| value.canonical_bytes()),
            b.map(|value| value.canonical_bytes())
        );
    }
}

// A structural identity alone is not proof of POD/Copy for a nominal type.
// That proof belongs to VM admission/materialization. This boundary still
// rejects open types and references even when hidden in nominal arguments.
/// Reject open/resource/reference types before they enter a frozen value.
/// Nominal POD/Copy and destructor checks still require the typeck provider.
pub fn validate_ctfe_type_shape(shape: &TypeShape) -> Result<(), ConstValueError> {
    shape
        .for_each_symbol(|_| {})
        .map_err(|_| ConstValueError::StructuralLimit)?;
    fn visit(shape: &TypeShape, argument: bool) -> bool {
        match shape {
            TypeShape::Primitive(primitive) => {
                primitive.is_numeric()
                    || matches!(primitive, super::Primitive::Bool | super::Primitive::Str)
            }
            TypeShape::Void => true,
            TypeShape::Array(_, inner) => visit(inner, false),
            TypeShape::Tuple(children) => children.iter().all(|child| visit(child, false)),
            TypeShape::Named(_, args) => args.iter().all(|child| visit(child, true)),
            TypeShape::Const(_) | TypeShape::FrozenConst(_) => argument,
            TypeShape::Option(inner) => visit(inner, false),
            TypeShape::Result(ok, error) => visit(ok, false) && visit(error, false),
            TypeShape::Slice(inner) => matches!(
                inner.as_ref(),
                TypeShape::Primitive(super::Primitive::Byte | super::Primitive::U8)
            ),
            TypeShape::Func(..)
            | TypeShape::Nullable(_)
            | TypeShape::ConstArray(..)
            | TypeShape::ConstParam(_)
            | TypeShape::Ptr(_)
            | TypeShape::Ref(_)
            | TypeShape::RefMut(_)
            | TypeShape::GenRef
            | TypeShape::Coroutine(_)
            | TypeShape::Poll(_)
            | TypeShape::Range(_)
            | TypeShape::Err
            | TypeShape::IntLiteral
            | TypeShape::FloatLiteral
            | TypeShape::Error => false,
        }
    }
    if visit(shape, false) {
        Ok(())
    } else {
        Err(ConstValueError::InvalidAggregateShape)
    }
}

fn shape_accepts(shape: &TypeShape, value: &ConstValue) -> bool {
    match (shape, value) {
        (TypeShape::Void, ConstValue::Void) => true,
        (TypeShape::Primitive(super::Primitive::Bool), ConstValue::Bool(_)) => true,
        (TypeShape::Primitive(primitive), ConstValue::Integer(value)) => {
            *primitive == value.ty().primitive()
        }
        (TypeShape::Primitive(primitive), ConstValue::Float(value)) => {
            *primitive == value.ty().primitive()
        }
        (TypeShape::Primitive(super::Primitive::Str), ConstValue::String(_)) => true,
        (TypeShape::Slice(_), ConstValue::Bytes(_)) => true,
        (
            TypeShape::Array(..)
            | TypeShape::Tuple(_)
            | TypeShape::Named(..)
            | TypeShape::Option(_)
            | TypeShape::Result(..),
            ConstValue::Aggregate(value),
        ) => shape == value.shape(),
        _ => false,
    }
}

/// Bounded structural identity encoding shared with VM admission fingerprints.
pub fn canonical_type_bytes(shape: &TypeShape) -> Result<Vec<u8>, ConstValueError> {
    shape
        .for_each_symbol(|_| {})
        .map_err(|_| ConstValueError::StructuralLimit)?;
    let mut bytes = vec![2];
    encode_shape(shape, &mut bytes);
    Ok(bytes)
}

fn encode_shape(shape: &TypeShape, bytes: &mut Vec<u8>) {
    match shape {
        TypeShape::Array(count, inner) => {
            bytes.push(0);
            bytes.extend_from_slice(&count.to_le_bytes());
            encode_shape(inner, bytes);
        }
        TypeShape::Tuple(children) => {
            bytes.push(1);
            bytes.extend_from_slice(&(children.len() as u64).to_le_bytes());
            for child in children {
                encode_shape(child, bytes);
            }
        }
        TypeShape::Named(symbol, args) => {
            bytes.push(2);
            bytes.extend_from_slice(&symbol.file_id.to_le_bytes());
            bytes.extend_from_slice(&symbol.local_id.0.to_le_bytes());
            bytes.extend_from_slice(&(args.len() as u64).to_le_bytes());
            for arg in args {
                encode_shape(arg, bytes);
            }
        }
        TypeShape::Primitive(primitive) => {
            bytes.push(3);
            // Explicit spelling is canonical, unlike enum layout/Debug output.
            let name = primitive.as_str().as_bytes();
            bytes.extend_from_slice(&(name.len() as u64).to_le_bytes());
            bytes.extend_from_slice(name);
        }
        TypeShape::Void => bytes.push(4),
        TypeShape::FrozenConst(value) => {
            bytes.push(24);
            let value = value.canonical_bytes();
            bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&value);
        }
        TypeShape::Const(value) => {
            bytes.push(5);
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        TypeShape::Func(args, result) => {
            bytes.push(6);
            bytes.extend_from_slice(&(args.len() as u64).to_le_bytes());
            for arg in args {
                encode_shape(arg, bytes);
            }
            encode_shape(result, bytes);
        }
        TypeShape::Nullable(inner) => {
            bytes.push(7);
            encode_shape(inner, bytes);
        }
        TypeShape::Slice(inner) => {
            bytes.push(8);
            encode_shape(inner, bytes);
        }
        TypeShape::Ptr(inner) => {
            bytes.push(9);
            encode_shape(inner, bytes);
        }
        TypeShape::Ref(inner) => {
            bytes.push(10);
            encode_shape(inner, bytes);
        }
        TypeShape::RefMut(inner) => {
            bytes.push(11);
            encode_shape(inner, bytes);
        }
        TypeShape::Option(inner) => {
            bytes.push(12);
            encode_shape(inner, bytes);
        }
        TypeShape::Coroutine(inner) => {
            bytes.push(13);
            encode_shape(inner, bytes);
        }
        TypeShape::Poll(inner) => {
            bytes.push(14);
            encode_shape(inner, bytes);
        }
        TypeShape::Range(inner) => {
            bytes.push(15);
            encode_shape(inner, bytes);
        }
        TypeShape::Result(ok, error) => {
            bytes.push(16);
            encode_shape(ok, bytes);
            encode_shape(error, bytes);
        }
        TypeShape::ConstArray(symbol, inner) => {
            bytes.push(17);
            bytes.extend_from_slice(&symbol.file_id.to_le_bytes());
            bytes.extend_from_slice(&symbol.local_id.0.to_le_bytes());
            encode_shape(inner, bytes);
        }
        TypeShape::ConstParam(symbol) => {
            bytes.push(18);
            bytes.extend_from_slice(&symbol.file_id.to_le_bytes());
            bytes.extend_from_slice(&symbol.local_id.0.to_le_bytes());
        }
        TypeShape::GenRef => bytes.push(19),
        TypeShape::Err => bytes.push(20),
        TypeShape::IntLiteral => bytes.push(21),
        TypeShape::FloatLiteral => bytes.push(22),
        TypeShape::Error => bytes.push(23),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use crate::{
        SymbolId,
        ctfe::{ConstInt, IntegerType},
        layout::DataLayout,
        types::Primitive,
    };

    fn integer() -> ConstValue {
        ConstValue::Integer(
            ConstInt::new(
                IntegerType::new(Primitive::Int, DataLayout::ptr_width(8)).expect("int"),
                42,
            )
            .expect("42"),
        )
    }

    #[test]
    fn frozen_products_validate_element_types_and_reject_hidden_open_shapes() {
        let int = TypeShape::Primitive(Primitive::Int);
        assert!(
            ConstAggregate::new(TypeShape::Array(1, Box::new(int.clone())), vec![integer()])
                .is_ok()
        );
        assert_eq!(
            ConstAggregate::new(
                TypeShape::Array(1, Box::new(int.clone())),
                vec![ConstValue::Bool(true)]
            ),
            Err(ConstValueError::InvalidAggregateShape)
        );
        assert_eq!(
            ConstAggregate::new(
                TypeShape::Tuple(vec![int.clone()]),
                vec![ConstValue::Bool(true)]
            ),
            Err(ConstValueError::InvalidAggregateShape)
        );
        assert_eq!(
            ConstAggregate::new(
                TypeShape::Array(0, Box::new(TypeShape::Ref(Box::new(int.clone())))),
                vec![]
            ),
            Err(ConstValueError::InvalidAggregateShape)
        );
        assert_eq!(
            ConstAggregate::new(
                TypeShape::Named(
                    SymbolId::new(1, 1),
                    vec![TypeShape::ConstParam(SymbolId::new(1, 2))]
                ),
                vec![]
            ),
            Err(ConstValueError::InvalidAggregateShape)
        );
        assert_eq!(
            ConstAggregate::new(
                TypeShape::Named(SymbolId::new(1, 1), vec![TypeShape::Ptr(Box::new(int))]),
                vec![]
            ),
            Err(ConstValueError::InvalidAggregateShape)
        );
    }

    #[test]
    fn nominal_identity_and_const_substitutions_are_encoded_without_pool_ids() {
        let a = ConstAggregate::new(
            TypeShape::Named(SymbolId::new(1, 3), vec![TypeShape::Const(1)]),
            vec![integer()],
        )
        .expect("product");
        let b = ConstAggregate::new(
            TypeShape::Named(SymbolId::new(2, 3), vec![TypeShape::Const(1)]),
            vec![integer()],
        )
        .expect("product");
        let c = ConstAggregate::new(
            TypeShape::Named(SymbolId::new(1, 3), vec![TypeShape::Const(2)]),
            vec![integer()],
        )
        .expect("product");
        assert_ne!(a.canonical_bytes(), b.canonical_bytes());
        assert_ne!(a.canonical_bytes(), c.canonical_bytes());
        assert_eq!(a.canonical_bytes(), a.clone().canonical_bytes());
    }

    #[test]
    fn total_value_depth_and_nodes_are_bounded_including_shared_children() {
        let leaf = ConstAggregate::new(TypeShape::Tuple(vec![]), vec![]).expect("empty");
        let shape = TypeShape::Named(SymbolId::new(1, 1), vec![]);
        let mut value = ConstValue::Aggregate(leaf);
        for _ in 0..TypeShape::MAX_DEPTH - 1 {
            value = ConstValue::Aggregate(
                ConstAggregate::new(shape.clone(), vec![value]).expect("bounded"),
            );
        }
        assert_eq!(
            ConstAggregate::new(shape.clone(), vec![value]),
            Err(ConstValueError::StructuralLimit)
        );
        assert_eq!(
            ConstAggregate::new(shape, vec![integer(); TypeShape::MAX_NODES]),
            Err(ConstValueError::StructuralLimit)
        );
    }
}
