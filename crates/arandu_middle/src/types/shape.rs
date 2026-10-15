//! Structural type identities for function instances crossing interner domains.
//! Dense `TypeId` and argument-pool ranges belong to their originating interner;
//! neither is a valid instance key on its own.

use super::{ArType, Primitive, TypeId, TypeInterner};
use crate::SymbolId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeShape {
    Primitive(Primitive),
    Named(SymbolId, Vec<Self>),
    Func(Vec<Self>, Box<Self>),
    Nullable(Box<Self>),
    Slice(Box<Self>),
    Array(u64, Box<Self>),
    ConstArray(SymbolId, Box<Self>),
    Const(u64),
    FrozenConst(std::sync::Arc<crate::ctfe::ConstValue>),
    ConstParam(SymbolId),
    Ptr(Box<Self>),
    Ref(Box<Self>),
    RefMut(Box<Self>),
    GenRef,
    Tuple(Vec<Self>),
    Result(Box<Self>, Box<Self>),
    Option(Box<Self>),
    Coroutine(Box<Self>),
    Poll(Box<Self>),
    Range(Box<Self>),
    Err,
    Void,
    IntLiteral,
    FloatLiteral,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hir::pool::IndexRange, symbol_table::LocalSymbolId};

    #[test]
    fn every_shape_roundtrips_between_differently_ordered_interners() {
        let a = TypeInterner::new();
        let b = TypeInterner::new();
        let sym = SymbolId {
            file_id: 7,
            local_id: LocalSymbolId(3),
        };
        let int = || Box::new(TypeShape::Primitive(Primitive::Int));
        let shapes = vec![
            TypeShape::Primitive(Primitive::Int),
            TypeShape::Named(sym, vec![TypeShape::Const(u64::MAX), *int()]),
            TypeShape::Func(vec![*int()], Box::new(TypeShape::Void)),
            TypeShape::Nullable(int()),
            TypeShape::Slice(int()),
            TypeShape::Array(u64::MAX, int()),
            TypeShape::ConstArray(sym, int()),
            TypeShape::Const(u64::MAX),
            TypeShape::ConstParam(sym),
            TypeShape::Ptr(int()),
            TypeShape::Ref(int()),
            TypeShape::RefMut(int()),
            TypeShape::GenRef,
            TypeShape::Tuple(vec![TypeShape::Option(int()), *int()]),
            TypeShape::Result(int(), Box::new(TypeShape::Err)),
            TypeShape::Option(int()),
            TypeShape::Coroutine(int()),
            TypeShape::Poll(int()),
            TypeShape::Range(int()),
            TypeShape::Err,
            TypeShape::Void,
            TypeShape::IntLiteral,
            TypeShape::FloatLiteral,
            TypeShape::Error,
        ];
        for shape in shapes.iter().rev() {
            shape.intern(&b).expect("valid shape");
        }
        for shape in shapes {
            let from = shape.intern(&a).expect("source type");
            let canonical = TypeShape::from_id(from, &a).expect("source shape");
            let to = canonical.intern(&b).expect("translated type");
            assert_eq!(TypeShape::from_id(to, &b), Ok(shape));
        }
    }

    #[test]
    fn invalid_ids_and_ranges_are_errors_not_panics() {
        let interner = TypeInterner::new();
        assert_eq!(
            TypeShape::from_id(TypeId::from_usize(100_000), &interner),
            Err(TypeShapeError::InvalidType)
        );
        let invalid = interner.intern(ArType::Tuple(IndexRange {
            start: u32::MAX,
            len: 1,
        }));
        assert_eq!(
            TypeShape::from_id(invalid, &interner),
            Err(TypeShapeError::InvalidArguments)
        );
        assert!(
            interner
                .try_type_args(IndexRange {
                    start: u32::MAX,
                    len: u32::MAX
                })
                .is_none()
        );
    }

    #[test]
    fn depth_and_shared_dag_expansion_are_bounded() {
        let interner = TypeInterner::new();
        let mut deep = TypeShape::Void;
        for _ in 0..TypeShape::MAX_DEPTH {
            deep = TypeShape::Option(Box::new(deep));
        }
        assert_eq!(deep.intern(&interner), Err(TypeShapeError::DepthLimit));
        let mut dag = interner.intern(ArType::Void);
        for _ in 0..13 {
            dag = interner.intern(ArType::tuple(&[dag, dag], &interner));
        }
        assert_eq!(
            TypeShape::from_id(dag, &interner),
            Err(TypeShapeError::NodeLimit)
        );
        let wide = TypeShape::Tuple(vec![TypeShape::Void; TypeShape::MAX_NODES]);
        assert_eq!(wide.intern(&interner), Err(TypeShapeError::NodeLimit));
    }

    #[test]
    fn shape_visitors_keep_source_identities_and_default_only_numeric_leaves() {
        let symbol = SymbolId {
            file_id: 7,
            local_id: LocalSymbolId(2),
        };
        let mut shape = TypeShape::Named(
            symbol,
            vec![
                TypeShape::Option(Box::new(TypeShape::IntLiteral)),
                TypeShape::Func(
                    vec![TypeShape::FloatLiteral],
                    Box::new(TypeShape::ConstParam(symbol)),
                ),
            ],
        );
        let mut visited = Vec::new();
        shape
            .for_each_symbol(|id| visited.push(id))
            .expect("bounded shape");
        assert_eq!(visited, [symbol, symbol]);
        shape.default_numeric_literals().expect("bounded shape");
        assert_eq!(
            shape,
            TypeShape::Named(
                symbol,
                vec![
                    TypeShape::Option(Box::new(TypeShape::Primitive(Primitive::Int))),
                    TypeShape::Func(
                        vec![TypeShape::Primitive(Primitive::Float)],
                        Box::new(TypeShape::ConstParam(symbol))
                    )
                ]
            )
        );
        let mut wide = TypeShape::Tuple(vec![TypeShape::Void; TypeShape::MAX_NODES]);
        assert_eq!(wide.for_each_symbol(|_| {}), Err(TypeShapeError::NodeLimit));
        assert_eq!(
            wide.default_numeric_literals(),
            Err(TypeShapeError::NodeLimit)
        );
    }

    #[test]
    fn instance_identity_keeps_the_full_definition_and_nested_arguments() {
        let definition = SymbolId {
            file_id: 1,
            local_id: LocalSymbolId(2),
        };
        let first = FunctionInstance {
            definition,
            arguments: vec![TypeShape::Option(Box::new(TypeShape::Const(7)))],
        };
        let mut different_module = first.clone();
        different_module.definition.file_id = 2;
        let mut different_argument = first.clone();
        different_argument.arguments = vec![TypeShape::Option(Box::new(TypeShape::Const(8)))];
        assert_ne!(first, different_module);
        assert_ne!(first, different_argument);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeShapeError {
    InvalidType,
    InvalidArguments,
    DepthLimit,
    NodeLimit,
}

/// An instance retains the full defining symbol identity and structural
/// arguments. It never substitutes a hash or source offset for `SymbolId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionInstance {
    pub definition: SymbolId,
    pub arguments: Vec<TypeShape>,
}

impl TypeShape {
    pub const MAX_DEPTH: usize = 128;
    pub const MAX_NODES: usize = 4096;

    /// Whether a bounded shape has no unresolved constant dimensions or errors.
    #[must_use]
    pub fn is_concrete(&self) -> bool {
        let mut pending = vec![self];
        let mut remaining = Self::MAX_NODES;
        while let Some(shape) = pending.pop() {
            if remaining == 0 {
                return false;
            }
            remaining -= 1;
            match shape {
                Self::Error
                | Self::IntLiteral
                | Self::FloatLiteral
                | Self::ConstParam(_)
                | Self::ConstArray(_, _) => return false,
                Self::Named(_, items) | Self::Tuple(items) => pending.extend(items),
                Self::Func(items, result) => {
                    pending.extend(items);
                    pending.push(result);
                }
                Self::Nullable(inner)
                | Self::Slice(inner)
                | Self::Array(_, inner)
                | Self::Ptr(inner)
                | Self::Ref(inner)
                | Self::RefMut(inner)
                | Self::Option(inner)
                | Self::Coroutine(inner)
                | Self::Poll(inner)
                | Self::Range(inner) => pending.push(inner),
                Self::Result(ok, err) => {
                    pending.push(ok);
                    pending.push(err);
                }
                Self::Primitive(_)
                | Self::Const(_)
                | Self::FrozenConst(_)
                | Self::GenRef
                | Self::Err
                | Self::Void => {}
            }
        }
        true
    }

    /// Finalize numeric literal leaves before publishing an executable
    /// instance key. Pseudo-types and their defaults have the same ABI and
    /// must not create two instances with the same mangled backend name.
    pub fn default_numeric_literals(&mut self) -> Result<(), TypeShapeError> {
        let mut remaining = Self::MAX_NODES;
        self.default_literals(0, &mut remaining)
    }

    fn default_literals(
        &mut self,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<(), TypeShapeError> {
        if depth >= Self::MAX_DEPTH {
            return Err(TypeShapeError::DepthLimit);
        }
        *remaining = remaining.checked_sub(1).ok_or(TypeShapeError::NodeLimit)?;
        match self {
            Self::IntLiteral => *self = Self::Primitive(Primitive::Int),
            Self::FloatLiteral => *self = Self::Primitive(Primitive::Float),
            Self::Named(_, args) | Self::Tuple(args) => {
                for arg in args {
                    arg.default_literals(depth + 1, remaining)?;
                }
            }
            Self::Func(args, result) => {
                for arg in args {
                    arg.default_literals(depth + 1, remaining)?;
                }
                result.default_literals(depth + 1, remaining)?;
            }
            Self::Nullable(inner)
            | Self::Slice(inner)
            | Self::Array(_, inner)
            | Self::ConstArray(_, inner)
            | Self::Ptr(inner)
            | Self::Ref(inner)
            | Self::RefMut(inner)
            | Self::Option(inner)
            | Self::Coroutine(inner)
            | Self::Poll(inner)
            | Self::Range(inner) => inner.default_literals(depth + 1, remaining)?,
            Self::Result(ok, error) => {
                ok.default_literals(depth + 1, remaining)?;
                error.default_literals(depth + 1, remaining)?;
            }
            Self::Primitive(_)
            | Self::FrozenConst(_)
            | Self::Const(_)
            | Self::ConstParam(_)
            | Self::GenRef
            | Self::Err
            | Self::Void
            | Self::Error => {}
        }
        Ok(())
    }

    /// Visit source identities inside a structural argument without interning
    /// it or allocating an expanded child list. Uses the same shape bounds.
    pub fn for_each_symbol(&self, mut visitor: impl FnMut(SymbolId)) -> Result<(), TypeShapeError> {
        let mut remaining = Self::MAX_NODES;
        self.visit_symbols(0, &mut remaining, &mut visitor)
    }

    fn visit_symbols(
        &self,
        depth: usize,
        remaining: &mut usize,
        visitor: &mut impl FnMut(SymbolId),
    ) -> Result<(), TypeShapeError> {
        if depth >= Self::MAX_DEPTH {
            return Err(TypeShapeError::DepthLimit);
        }
        *remaining = remaining.checked_sub(1).ok_or(TypeShapeError::NodeLimit)?;
        match self {
            Self::Named(symbol, args) => {
                visitor(*symbol);
                for arg in args {
                    arg.visit_symbols(depth + 1, remaining, visitor)?;
                }
            }
            Self::FrozenConst(value) => {
                value
                    .type_shape()
                    .visit_symbols(depth + 1, remaining, visitor)?
            }
            Self::ConstParam(symbol) => visitor(*symbol),
            Self::ConstArray(symbol, inner) => {
                visitor(*symbol);
                inner.visit_symbols(depth + 1, remaining, visitor)?;
            }
            Self::Func(args, result) => {
                for arg in args {
                    arg.visit_symbols(depth + 1, remaining, visitor)?;
                }
                result.visit_symbols(depth + 1, remaining, visitor)?;
            }
            Self::Tuple(args) => {
                for arg in args {
                    arg.visit_symbols(depth + 1, remaining, visitor)?;
                }
            }
            Self::Nullable(inner)
            | Self::Slice(inner)
            | Self::Array(_, inner)
            | Self::Ptr(inner)
            | Self::Ref(inner)
            | Self::RefMut(inner)
            | Self::Option(inner)
            | Self::Coroutine(inner)
            | Self::Poll(inner)
            | Self::Range(inner) => inner.visit_symbols(depth + 1, remaining, visitor)?,
            Self::Result(ok, error) => {
                ok.visit_symbols(depth + 1, remaining, visitor)?;
                error.visit_symbols(depth + 1, remaining, visitor)?;
            }
            Self::Primitive(_)
            | Self::Const(_)
            | Self::GenRef
            | Self::Err
            | Self::Void
            | Self::IntLiteral
            | Self::FloatLiteral
            | Self::Error => {}
        }
        Ok(())
    }

    pub fn from_id(id: TypeId, interner: &TypeInterner) -> Result<Self, TypeShapeError> {
        let mut remaining = Self::MAX_NODES;
        Self::read(id, interner, 0, &mut remaining)
    }

    fn read(
        id: TypeId,
        interner: &TypeInterner,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<Self, TypeShapeError> {
        if depth >= Self::MAX_DEPTH {
            return Err(TypeShapeError::DepthLimit);
        }
        *remaining = remaining.checked_sub(1).ok_or(TypeShapeError::NodeLimit)?;
        let one = |id, remaining: &mut usize| {
            Self::read(id, interner, depth + 1, remaining).map(Box::new)
        };
        let many = |range: crate::hir::pool::IndexRange, remaining: &mut usize| {
            if usize::try_from(range.len).map_or(true, |len| len > *remaining) {
                return Err(TypeShapeError::NodeLimit);
            }
            interner
                .try_type_args(range)
                .ok_or(TypeShapeError::InvalidArguments)?
                .into_iter()
                .map(|id| Self::read(id, interner, depth + 1, remaining))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(
            match interner
                .try_resolve(id)
                .ok_or(TypeShapeError::InvalidType)?
            {
                ArType::Primitive(p) => Self::Primitive(p),
                ArType::Named(symbol, args) => Self::Named(symbol, many(args, remaining)?),
                ArType::Func(args, ret) => Self::Func(many(args, remaining)?, one(ret, remaining)?),
                ArType::Nullable(id) => Self::Nullable(one(id, remaining)?),
                ArType::Slice(id) => Self::Slice(one(id, remaining)?),
                ArType::Array(n, id) => Self::Array(n, one(id, remaining)?),
                ArType::ConstArray(n, id) => Self::ConstArray(n, one(id, remaining)?),
                ArType::Const(n) => Self::Const(n),
                ArType::FrozenConst(value) => Self::FrozenConst(value),
                ArType::ConstParam(n) => Self::ConstParam(n),
                ArType::Ptr(id) => Self::Ptr(one(id, remaining)?),
                ArType::Ref(id) => Self::Ref(one(id, remaining)?),
                ArType::RefMut(id) => Self::RefMut(one(id, remaining)?),
                ArType::GenRef => Self::GenRef,
                ArType::Tuple(args) => Self::Tuple(many(args, remaining)?),
                ArType::Result(ok, error) => {
                    Self::Result(one(ok, remaining)?, one(error, remaining)?)
                }
                ArType::Option(id) => Self::Option(one(id, remaining)?),
                ArType::Coroutine(id) => Self::Coroutine(one(id, remaining)?),
                ArType::Poll(id) => Self::Poll(one(id, remaining)?),
                ArType::Range(id) => Self::Range(one(id, remaining)?),
                ArType::Err => Self::Err,
                ArType::Void => Self::Void,
                ArType::IntLiteral => Self::IntLiteral,
                ArType::FloatLiteral => Self::FloatLiteral,
                ArType::Error => Self::Error,
            },
        )
    }

    /// Re-intern a validated shape into the consumer's own type domain.
    pub fn intern(&self, interner: &TypeInterner) -> Result<TypeId, TypeShapeError> {
        let mut remaining = Self::MAX_NODES;
        self.write(interner, 0, &mut remaining)
    }

    fn write(
        &self,
        interner: &TypeInterner,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<TypeId, TypeShapeError> {
        if depth >= Self::MAX_DEPTH {
            return Err(TypeShapeError::DepthLimit);
        }
        *remaining = remaining.checked_sub(1).ok_or(TypeShapeError::NodeLimit)?;
        let one = |shape: &Self, remaining: &mut usize| shape.write(interner, depth + 1, remaining);
        let many = |shapes: &[Self], remaining: &mut usize| {
            shapes
                .iter()
                .map(|shape| one(shape, remaining))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(interner.intern(match self {
            Self::Primitive(p) => ArType::Primitive(*p),
            Self::Named(symbol, args) => ArType::named(*symbol, &many(args, remaining)?, interner),
            Self::Func(args, ret) => {
                ArType::func(&many(args, remaining)?, one(ret, remaining)?, interner)
            }
            Self::Nullable(inner) => ArType::Nullable(one(inner, remaining)?),
            Self::Slice(inner) => ArType::Slice(one(inner, remaining)?),
            Self::Array(n, inner) => ArType::Array(*n, one(inner, remaining)?),
            Self::ConstArray(n, inner) => ArType::ConstArray(*n, one(inner, remaining)?),
            Self::Const(n) => ArType::Const(*n),
            Self::FrozenConst(value) => ArType::FrozenConst(std::sync::Arc::clone(value)),
            Self::ConstParam(n) => ArType::ConstParam(*n),
            Self::Ptr(inner) => ArType::Ptr(one(inner, remaining)?),
            Self::Ref(inner) => ArType::Ref(one(inner, remaining)?),
            Self::RefMut(inner) => ArType::RefMut(one(inner, remaining)?),
            Self::GenRef => ArType::GenRef,
            Self::Tuple(args) => ArType::tuple(&many(args, remaining)?, interner),
            Self::Result(ok, error) => ArType::Result(one(ok, remaining)?, one(error, remaining)?),
            Self::Option(inner) => ArType::Option(one(inner, remaining)?),
            Self::Coroutine(inner) => ArType::Coroutine(one(inner, remaining)?),
            Self::Poll(inner) => ArType::Poll(one(inner, remaining)?),
            Self::Range(inner) => ArType::Range(one(inner, remaining)?),
            Self::Err => ArType::Err,
            Self::Void => ArType::Void,
            Self::IntLiteral => ArType::IntLiteral,
            Self::FloatLiteral => ArType::FloatLiteral,
            Self::Error => ArType::Error,
        }))
    }
}
