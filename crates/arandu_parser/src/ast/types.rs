use super::{IndexRange, TypeExprId};
use arandu_lexer::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum ResultType {
    Single { span: Span, ty: TypeExprId },
    Multi { span: Span, types: IndexRange },
}

use smallvec::SmallVec;
use smol_str::SmolStr;

#[derive(Debug, Clone, PartialEq)]
pub struct TypeName {
    pub span: Span,
    pub path: SmallVec<[SmolStr; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// Scalar compile-time argument in a generic argument list.
    Const {
        span: Span,
        value: SmolStr,
    },
    Primitive {
        span: Span,
        name: SmolStr,
    },
    Named {
        span: Span,
        name: TypeName,
        args: IndexRange,
    },
    Nullable {
        span: Span,
        inner: TypeExprId,
    },
    Pointer {
        span: Span,
        inner: TypeExprId,
    },
    /// Shared reference type: canonical `ref T` (legacy `&T`).
    Ref {
        span: Span,
        inner: TypeExprId,
    },
    /// Exclusive reference type: canonical `mut ref T` (legacy `&mut T`).
    RefMut {
        span: Span,
        inner: TypeExprId,
    },
    Slice {
        span: Span,
        inner: TypeExprId,
    },
    Array {
        span: Span,
        size: SmolStr,
        size_span: Span,
        elem: TypeExprId,
    },
    Func {
        span: Span,
        params: IndexRange,
        result: Option<ResultType>,
    },
    Group {
        span: Span,
        inner: TypeExprId,
    },
}

impl TypeExpr {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Const { span, .. }
            | TypeExpr::Primitive { span, .. }
            | TypeExpr::Named { span, .. }
            | TypeExpr::Nullable { span, .. }
            | TypeExpr::Pointer { span, .. }
            | TypeExpr::Ref { span, .. }
            | TypeExpr::RefMut { span, .. }
            | TypeExpr::Slice { span, .. }
            | TypeExpr::Array { span, .. }
            | TypeExpr::Func { span, .. }
            | TypeExpr::Group { span, .. } => *span,
        }
    }
}
