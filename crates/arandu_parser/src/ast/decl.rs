use super::{Block, DeclId, Expr, IndexRange, TypeExprId, TypeName};
pub use arandu_base::lang::{AbiKind, Visibility};
use arandu_lexer::Span;
use smallvec::SmallVec;
use smol_str::SmolStr;

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub span: Span,
    pub module: Option<ModuleDecl>,
    pub imports: Vec<ImportDecl>,
    pub interface_impls: Vec<InterfaceImplDecl>,
    pub decls: Vec<DeclId>,
    pub docs: Vec<DocCommentAttachment>,
    pub pool: super::AstPool,
}

impl Program {
    pub fn for_each_decl_recursive<'a>(&'a self, mut f: impl FnMut(DeclId, &'a TopLevelDecl)) {
        self.walk_decls_recursive(&self.decls, &mut f);
    }

    fn walk_decls_recursive<'a>(
        &'a self,
        decl_ids: &[DeclId],
        f: &mut impl FnMut(DeclId, &'a TopLevelDecl),
    ) {
        for &id in decl_ids {
            let decl = self.pool.decl(id);
            f(id, decl);
            if let TopLevelDecl::Submodule(submod) = decl {
                self.walk_decls_recursive(&submod.decls, f);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceImplDecl {
    pub span: Span,
    pub for_type: TypeName,
    pub interface: TypeName,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocCommentAttachment {
    pub span: Span,
    pub text: SmolStr,
    pub target_span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModuleDecl {
    pub span: Span,
    pub path: SmallVec<[SmolStr; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportDecl {
    /// `import path.to.module as alias`
    ModuleAlias {
        span: Span,
        path: SmallVec<[SmolStr; 3]>,
        alias: SmolStr,
    },
    /// `from path.to.module import { Item, Other }`
    Named {
        span: Span,
        items: Vec<ImportItem>,
        path: SmallVec<[SmolStr; 3]>,
    },
    /// `public use path.to.module.{ Item, Other }`
    ReExport {
        span: Span,
        visibility: Visibility,
        items: Vec<ImportItem>,
        path: SmallVec<[SmolStr; 3]>,
    },
    /// `from "external" import { Item }`
    ExternalNamed {
        span: Span,
        items: Vec<ImportItem>,
        source: SmolStr,
    },
    /// `import "external" as alias`
    ExternalAlias {
        span: Span,
        source: SmolStr,
        alias: SmolStr,
    },
}

impl ImportDecl {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            ImportDecl::ModuleAlias { span, .. }
            | ImportDecl::Named { span, .. }
            | ImportDecl::ReExport { span, .. }
            | ImportDecl::ExternalNamed { span, .. }
            | ImportDecl::ExternalAlias { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportItem {
    pub span: Span,
    pub name: SmolStr,
    pub alias: Option<SmolStr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TopLevelDecl {
    Const(ConstDecl),
    TypeAlias(TypeAliasDecl),
    Func(FuncDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Interface(InterfaceDecl),
    Extern(ExternDecl),
    Submodule(SubmoduleDecl),
    Error(Span),
}

impl TopLevelDecl {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            TopLevelDecl::Const(decl) => decl.span,
            TopLevelDecl::TypeAlias(decl) => decl.span,
            TopLevelDecl::Func(decl) => decl.span,
            TopLevelDecl::Struct(decl) => decl.span,
            TopLevelDecl::Enum(decl) => decl.span,
            TopLevelDecl::Interface(decl) => decl.span,
            TopLevelDecl::Extern(decl) => decl.span,
            TopLevelDecl::Submodule(decl) => decl.span,
            TopLevelDecl::Error(span) => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubmoduleDecl {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub visibility: Visibility,
    pub name: SmolStr,
    pub decls: Vec<DeclId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub span: Span,
    /// Exact span of the annotation identifier, excluding the leading `@`
    /// and any argument list. Used by structured migration replacements.
    pub name_span: Span,
    pub name: SmolStr,
    pub args: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub span: Span,
    pub name: SmolStr,
    /// `Some(type)` for a scalar const parameter (`const N: uint`).
    pub const_ty: Option<TypeExprId>,
    pub constraints: SmallVec<[TypeExprId; 2]>,
    /// T2.1: optional default type arg, e.g. `A = GlobalAllocator` in `Vec<T, A = GlobalAllocator>`.
    pub default: Option<TypeExprId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhereItem {
    pub span: Span,
    pub name: SmolStr,
    pub constraints: SmallVec<[TypeExprId; 2]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDecl {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub visibility: Visibility,
    pub name: SmolStr,
    pub ty: Option<TypeExprId>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeAliasDecl {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub visibility: Visibility,
    pub name: SmolStr,
    pub generic_params: SmallVec<[GenericParam; 2]>,
    pub ty: TypeExprId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncDecl {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub visibility: Visibility,
    pub is_async: bool,
    pub name: FuncName,
    pub generic_params: SmallVec<[GenericParam; 2]>,
    pub params: Vec<Param>,
    pub result: Option<super::ResultType>,
    pub where_clause: SmallVec<[WhereItem; 2]>,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FuncName {
    Free {
        span: Span,
        name: SmolStr,
    },
    Method {
        span: Span,
        receiver: TypeName,
        name: SmolStr,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncSignature {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub name: SmolStr,
    pub generic_params: SmallVec<[GenericParam; 2]>,
    pub params: Vec<Param>,
    pub result: Option<super::ResultType>,
    pub where_clause: SmallVec<[WhereItem; 2]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDecl {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub visibility: Visibility,
    pub name: SmolStr,
    pub generic_params: SmallVec<[GenericParam; 2]>,
    pub where_clause: SmallVec<[WhereItem; 2]>,
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub visibility: Visibility,
    pub name: SmolStr,
    pub ty: TypeExprId,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub visibility: Visibility,
    pub name: SmolStr,
    pub generic_params: SmallVec<[GenericParam; 2]>,
    pub where_clause: SmallVec<[WhereItem; 2]>,
    pub variants: Vec<EnumVariant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub name: SmolStr,
    pub payload: Option<EnumPayload>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnumPayload {
    Tuple { span: Span, types: IndexRange },
    Struct { span: Span, fields: Vec<FieldDecl> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceDecl {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub visibility: Visibility,
    pub sealed: bool,
    pub name: SmolStr,
    pub generic_params: SmallVec<[GenericParam; 2]>,
    pub where_clause: SmallVec<[WhereItem; 2]>,
    pub members: Vec<FuncSignature>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternDecl {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub abi: SmolStr,
    pub members: Vec<FuncSignature>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub span: Span,
    pub attrs: SmallVec<[Attribute; 2]>,
    pub ownership: Option<Ownership>,
    pub name: SmolStr,
    pub ty: TypeExprId,
    pub is_variadic: bool,
    /// `true` when this parameter is the method receiver (`self`).
    pub is_receiver: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Own,
    Mut,
    Shared,
}
