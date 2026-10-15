//! High-level Intermediate Representation (HIR) and symbol interfaces.

pub mod decl;
pub mod expr;
pub mod pattern;
pub mod pool;
pub mod pretty;
pub mod stmt;
pub(crate) mod validation;

pub use crate::ops::{BinaryOp, SetOp, UnaryOp};
pub use decl::{
    HirConst, HirDecl, HirEnum, HirEnumVariant, HirExtern, HirFunc, HirFuncSignature, HirInterface,
    HirParam, HirStruct, HirStructField, HirTypeAlias, ReceiverKind,
};
pub use expr::{
    HirCatchHandler, HirExpr, HirExprKind, HirFieldInit, HirLambdaBody, HirLambdaParam,
    HirMatchArm, HirMatchArmBody, HirStringPart, LayoutQuery, ResultCtorVariant,
};
pub use pattern::{HirFieldPattern, HirPattern};
pub use pool::{
    HirBlockId, HirDeclId, HirEnumVariantId, HirExprId, HirFieldPatternId, HirFuncSignatureId,
    HirParamId, HirPatternId, HirPool, HirStmtId, HirStructFieldId, IndexRange,
};
pub use pretty::HirPrettyCtx;
pub use stmt::{
    HirBindingItem, HirBlock, HirCondition, HirForBinding, HirForClause, HirPlace, HirPlaceSuffix,
    HirSimpleStmt, HirStmt, HirStmtKind,
};

use crate::{SymbolId, SymbolTable};
use arandu_lexer::Span;

#[must_use]
pub fn symbol_name(symbols: &SymbolTable, id: SymbolId) -> &str {
    &symbols.get(id).name
}

#[derive(Debug)]
pub struct HirProgram {
    pub span: Span,
    pub module: Option<String>,
    pub decls: Vec<HirDeclId>,
    pub pool: crate::hir::HirPool,
}

impl HirProgram {
    #[must_use]
    pub fn pretty_print(&self, ctx: &HirPrettyCtx<'_>) -> String {
        pretty::print_program(self, ctx)
    }
}
