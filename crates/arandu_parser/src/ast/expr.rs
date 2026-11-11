use super::{Block, TypeExprId};
use arandu_lexer::Span;
use smol_str::SmolStr;

pub use super::ast_pool::ExprId as Expr;
pub use super::ast_pool::ExprId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
    Await,
    /// Shared address-of: `&expr` → `&T` (F2.0 safe borrow).
    Ref,
    /// Exclusive address-of: `&mut expr` → `&mut T` (F2.0 safe borrow).
    RefMut,
    /// Dereference: `*expr` — safe load for `&T`/`&mut T`; raw `ptr[T]` needs unsafe.
    Deref,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Equal,
    NotEqual,
    Lt,
    Gt,
    LtEqual,
    GtEqual,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitOr,
    BitXor,
    BitAnd,
    ShiftLeft,
    ShiftRight,
    NullCoalesce,
    RangeExclusive,
    RangeInclusive,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldInit {
    pub span: Span,
    pub name: SmolStr,
    pub value: ExprId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LambdaParam {
    pub span: Span,
    pub name: SmolStr,
    pub ty: Option<TypeExprId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LambdaBody {
    Expr { span: Span, expr: ExprId },
    Block { span: Span, block: Block },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CatchHandler {
    Expr {
        span: Span,
        expr: ExprId,
    },
    Block {
        span: Span,
        error: SmolStr,
        block: Block,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    Text { span: Span, text: SmolStr },
    Expr { span: Span, expr: ExprId },
}

/// Whether `text` is a tuple element index as written after a dot (`t.0`):
/// decimal digits with no leading zero.
#[must_use]
pub fn is_tuple_index(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'))
}
