//! High-level Intermediate Representation (HIR) declarations.

use crate::SymbolId;
use crate::hir::pool::{HirBlockId, HirExprId, IndexRange};
use crate::types::TypeId;
use arandu_lexer::Span;

#[derive(Debug, Clone)]
pub enum HirDecl {
    Const(HirConst),
    TypeAlias(HirTypeAlias),
    Func(HirFunc),
    Struct(HirStruct),
    Enum(HirEnum),
    Interface(HirInterface),
    Extern(HirExtern),
}

impl HirDecl {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            HirDecl::Const(decl) => decl.span,
            HirDecl::TypeAlias(decl) => decl.span,
            HirDecl::Func(decl) => decl.span,
            HirDecl::Struct(decl) => decl.span,
            HirDecl::Enum(decl) => decl.span,
            HirDecl::Interface(decl) => decl.span,
            HirDecl::Extern(decl) => decl.span,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HirConst {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub value: HirExprId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirTypeAlias {
    pub symbol: SymbolId,
    pub target: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirFunc {
    pub symbol: SymbolId,
    pub params: IndexRange,
    pub return_type: TypeId,
    pub body: Option<HirBlockId>,
    pub span: Span,
    /// A3: `async func` — return type is `Coroutine[T]`; body returns bare `T` and
    /// AMIR wraps with `CoroutineReady` (type sugar, not a separate colour world).
    pub is_async: bool,
    /// G2 / F2.3.3: `@NoFallback` — promote generational-escape O004 notes to errors.
    /// Not a silent strict mode: only affects scopes that opt in.
    pub no_fallback: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReceiverKind {
    Shared,
    Mut,
    Own,
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub span: Span,
    pub is_receiver: bool,
    pub receiver_kind: Option<ReceiverKind>,
}

#[derive(Debug, Clone)]
pub struct HirStruct {
    pub symbol: SymbolId,
    pub fields: IndexRange,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirStructField {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirEnum {
    pub symbol: SymbolId,
    pub variants: IndexRange,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirEnumVariant {
    pub symbol: SymbolId,
    pub payload: Option<TypeId>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirInterface {
    pub symbol: SymbolId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirExtern {
    pub abi: arandu_parser::AbiKind,
    pub members: IndexRange,
    pub span: Span,
}

impl HirExtern {
    #[must_use]
    pub fn is_intrinsic(&self) -> bool {
        self.abi == arandu_parser::AbiKind::AranduIntrinsic
    }
}

#[derive(Debug, Clone)]
pub struct HirFuncSignature {
    pub symbol: SymbolId,
    pub params: IndexRange,
    pub return_type: TypeId,
    pub span: Span,
}
