#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolId;
    use crate::hir::{HirBlock, HirConst, HirDecl, HirExpr, HirExprKind, HirStmt, HirStmtKind};
    use crate::types::{Primitive, TypeInterner};
    use arandu_base::span::Span;
    const S: Span = Span::new(0, 0, 0);
    fn int_ty() -> crate::types::TypeId {
        TypeInterner::preinterned_primitive(Primitive::Int)
    }
    fn bool_ty() -> crate::types::TypeId {
        TypeInterner::preinterned_primitive(Primitive::Bool)
    }
    fn float_ty() -> crate::types::TypeId {
        TypeInterner::preinterned_primitive(Primitive::Float)
    }

    #[test]
    fn alloc_and_get_expr() {
        let mut pool = HirPool::new();
        let expr = HirExpr {
            kind: HirExprKind::Int("42".into()),
            ty: int_ty(),
            span: S,
        };
        let id = pool.alloc_expr(expr);
        assert_eq!(id, HirExprId::from_usize(0));
        assert_eq!(pool.expr(id).ty, int_ty());
    }

    #[test]
    fn alloc_and_get_stmt() {
        let mut pool = HirPool::new();
        let stmt = HirStmt {
            kind: HirStmtKind::Break,
            span: S,
        };
        let id = pool.alloc_stmt(stmt);
        assert_eq!(id, HirStmtId::from_usize(0));
        assert!(matches!(pool.stmt(id).kind, HirStmtKind::Break));
    }

    #[test]
    fn alloc_and_get_block() {
        let mut pool = HirPool::new();
        let block = HirBlock {
            statements: IndexRange::empty(),
            span: S,
        };
        let id = pool.alloc_block(block);
        assert_eq!(id, HirBlockId::from_usize(0));
        assert!(pool.block(id).statements.is_empty());
    }

    #[test]
    fn alloc_and_get_decl() {
        let mut pool = HirPool::new();
        let decl = HirDecl::Const(HirConst {
            symbol: SymbolId::new(0, 0),
            ty: int_ty(),
            value: HirExprId::from_usize(0),
            span: S,
        });
        let id = pool.alloc_decl(decl);
        assert_eq!(id, HirDeclId::from_usize(0));
    }

    #[test]
    fn alloc_expr_list() {
        let mut pool = HirPool::new();
        let e1 = pool.alloc_expr(HirExpr {
            kind: HirExprKind::Bool(true),
            ty: bool_ty(),
            span: S,
        });
        let e2 = pool.alloc_expr(HirExpr {
            kind: HirExprKind::Bool(false),
            ty: bool_ty(),
            span: S,
        });
        let range = pool.alloc_expr_list(&[e1, e2]);
        assert_eq!(range.len, 2);
        assert_eq!(pool.expr_list(range), &[e1, e2]);
    }

    #[test]
    fn alloc_param_list() {
        let mut pool = HirPool::new();
        let params = vec![crate::hir::HirParam {
            symbol: SymbolId::new(0, 0),
            ty: int_ty(),
            span: S,
            is_receiver: false,
            receiver_kind: None,
        }];
        let range = pool.alloc_param_list(&params);
        assert_eq!(pool.params_list(range).len(), 1);
    }

    #[test]
    fn alloc_struct_field_list() {
        let mut pool = HirPool::new();
        let fields = vec![crate::hir::HirStructField {
            symbol: SymbolId::new(0, 1),
            ty: float_ty(),
            span: S,
        }];
        let range = pool.alloc_struct_field_list(&fields);
        assert_eq!(pool.struct_fields_list(range).len(), 1);
    }

    #[test]
    fn alloc_binding_list_and_place_list() {
        let mut pool = HirPool::new();
        let b_range = pool.alloc_binding_list(&[crate::hir::HirBindingItem {
            symbol: SymbolId::new(0, 2),
            ty: int_ty(),
            span: S,
        }]);
        assert_eq!(pool.bindings_list(b_range).len(), 1);
        let p_range = pool.alloc_place_list(&[crate::hir::HirPlace {
            root_symbol: SymbolId::new(0, 3),
            suffixes: smallvec::SmallVec::new(),
            ty: int_ty(),
            span: S,
        }]);
        assert_eq!(pool.places_list(p_range).len(), 1);
    }

    #[test]
    fn empty_pool() {
        let pool = HirPool::new();
        assert!(pool.exprs.is_empty());
        assert!(pool.stmts.is_empty());
        assert!(pool.blocks.is_empty());
    }
}

use crate::index_vec::IndexVec;

crate::newtype_index!(HirExprId);
crate::newtype_index!(HirStmtId);
crate::newtype_index!(HirBlockId);
crate::newtype_index!(HirDeclId);
crate::newtype_index!(HirParamId);
crate::newtype_index!(HirStructFieldId);
crate::newtype_index!(HirEnumVariantId);
crate::newtype_index!(HirFuncSignatureId);
crate::newtype_index!(HirPatternId);
crate::newtype_index!(HirFieldPatternId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndexRange {
    pub start: u32,
    pub len: u32,
}

impl IndexRange {
    #[must_use]
    pub const fn empty() -> Self {
        Self { start: 0, len: 0 }
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub fn range(self) -> std::ops::Range<usize> {
        (self.start as usize)..(self.start as usize + self.len as usize)
    }
}

/// A compact, index-backed storage for HIR nodes.
#[derive(Debug, Default)]
pub struct HirPool {
    pub exprs: IndexVec<HirExprId, super::HirExpr>,
    pub stmts: IndexVec<HirStmtId, super::HirStmt>,
    pub blocks: IndexVec<HirBlockId, super::HirBlock>,
    pub decls: IndexVec<HirDeclId, super::HirDecl>,
    pub patterns: IndexVec<HirPatternId, super::HirPattern>,
    pub field_patterns: IndexVec<HirFieldPatternId, super::HirFieldPattern>,

    // Contiguous storage for IndexRanges
    pub params: Vec<super::HirParam>,
    pub struct_fields: Vec<super::HirStructField>,
    pub enum_variants: Vec<super::HirEnumVariant>,
    pub func_signatures: Vec<super::HirFuncSignature>,
    pub bindings: Vec<super::HirBindingItem>,
    pub places: Vec<super::HirPlace>,
    pub for_bindings: Vec<super::HirForBinding>,
    pub match_arms: Vec<super::HirMatchArm>,
    pub field_inits: Vec<super::HirFieldInit>,
    pub lambda_params: Vec<super::HirLambdaParam>,

    pub expr_ids: Vec<HirExprId>,
    pub stmt_ids: Vec<HirStmtId>,
    pub pattern_ids: Vec<HirPatternId>,
    pub field_pattern_ids: Vec<HirFieldPatternId>,
}

impl HirPool {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alloc_expr(&mut self, expr: super::HirExpr) -> HirExprId {
        self.exprs.push(expr)
    }

    /// Fallible lookup (prefer this when the id may be untrusted).
    #[must_use]
    pub fn expr(&self, id: HirExprId) -> &super::HirExpr {
        match self.exprs.get(id) {
            Some(e) => e,
            None => crate::ice::invalid_dense_id("HirExprId", id.as_usize()),
        }
    }

    pub fn expr_mut(&mut self, id: HirExprId) -> &mut super::HirExpr {
        match self.exprs.get_mut(id) {
            Some(e) => e,
            None => crate::ice::invalid_dense_id("HirExprId", id.as_usize()),
        }
    }

    pub fn alloc_stmt(&mut self, stmt: super::HirStmt) -> HirStmtId {
        self.stmts.push(stmt)
    }

    #[must_use]
    pub fn try_stmt(&self, id: HirStmtId) -> Option<&super::HirStmt> {
        self.stmts.get(id)
    }

    #[must_use]
    pub fn stmt(&self, id: HirStmtId) -> &super::HirStmt {
        match self.stmts.get(id) {
            Some(s) => s,
            None => crate::ice::invalid_dense_id("HirStmtId", id.as_usize()),
        }
    }

    pub fn alloc_block(&mut self, block: super::HirBlock) -> HirBlockId {
        self.blocks.push(block)
    }

    #[must_use]
    pub fn block(&self, id: HirBlockId) -> &super::HirBlock {
        match self.blocks.get(id) {
            Some(b) => b,
            None => crate::ice::invalid_dense_id("HirBlockId", id.as_usize()),
        }
    }

    pub fn alloc_decl(&mut self, decl: super::HirDecl) -> HirDeclId {
        self.decls.push(decl)
    }

    #[must_use]
    pub fn decl(&self, id: HirDeclId) -> &super::HirDecl {
        match self.decls.get(id) {
            Some(d) => d,
            None => crate::ice::invalid_dense_id("HirDeclId", id.as_usize()),
        }
    }

    pub fn alloc_pattern(&mut self, pattern: super::HirPattern) -> HirPatternId {
        self.patterns.push(pattern)
    }

    #[must_use]
    pub fn pattern(&self, id: HirPatternId) -> &super::HirPattern {
        match self.patterns.get(id) {
            Some(p) => p,
            None => crate::ice::invalid_dense_id("HirPatternId", id.as_usize()),
        }
    }

    pub fn alloc_field_pattern(&mut self, field: super::HirFieldPattern) -> HirFieldPatternId {
        self.field_patterns.push(field)
    }

    #[must_use]
    pub fn field_pattern(&self, id: HirFieldPatternId) -> &super::HirFieldPattern {
        match self.field_patterns.get(id) {
            Some(f) => f,
            None => crate::ice::invalid_dense_id("HirFieldPatternId", id.as_usize()),
        }
    }

    // List allocators for IndexRange
    pub fn alloc_expr_list(&mut self, ids: &[HirExprId]) -> IndexRange {
        let start = self.expr_ids.len() as u32;
        self.expr_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_stmt_list(&mut self, ids: &[HirStmtId]) -> IndexRange {
        let start = self.stmt_ids.len() as u32;
        self.stmt_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_pattern_list(&mut self, ids: &[HirPatternId]) -> IndexRange {
        let start = self.pattern_ids.len() as u32;
        self.pattern_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_field_pattern_list(&mut self, ids: &[HirFieldPatternId]) -> IndexRange {
        let start = self.field_pattern_ids.len() as u32;
        self.field_pattern_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_param_list(&mut self, items: &[super::HirParam]) -> IndexRange {
        let start = self.params.len() as u32;
        self.params.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    pub fn alloc_struct_field_list(&mut self, items: &[super::HirStructField]) -> IndexRange {
        let start = self.struct_fields.len() as u32;
        self.struct_fields.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    pub fn alloc_enum_variant_list(&mut self, items: &[super::HirEnumVariant]) -> IndexRange {
        let start = self.enum_variants.len() as u32;
        self.enum_variants.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    pub fn alloc_func_signature_list(&mut self, items: &[super::HirFuncSignature]) -> IndexRange {
        let start = self.func_signatures.len() as u32;
        self.func_signatures.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    pub fn alloc_binding_list(&mut self, items: &[super::HirBindingItem]) -> IndexRange {
        let start = self.bindings.len() as u32;
        self.bindings.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    pub fn alloc_place_list(&mut self, items: &[super::HirPlace]) -> IndexRange {
        let start = self.places.len() as u32;
        self.places.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    pub fn alloc_for_binding_list(&mut self, items: &[super::HirForBinding]) -> IndexRange {
        let start = self.for_bindings.len() as u32;
        self.for_bindings.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    pub fn alloc_match_arm_list(&mut self, items: &[super::HirMatchArm]) -> IndexRange {
        let start = self.match_arms.len() as u32;
        self.match_arms.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    pub fn alloc_field_init_list(&mut self, items: &[super::HirFieldInit]) -> IndexRange {
        let start = self.field_inits.len() as u32;
        self.field_inits.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    pub fn alloc_lambda_param_list(&mut self, items: &[super::HirLambdaParam]) -> IndexRange {
        let start = self.lambda_params.len() as u32;
        self.lambda_params.extend_from_slice(items);
        IndexRange {
            start,
            len: items.len() as u32,
        }
    }

    // List readers
    #[must_use]
    pub fn expr_list(&self, range: IndexRange) -> &[HirExprId] {
        &self.expr_ids[range.range()]
    }

    #[must_use]
    pub fn stmt_list(&self, range: IndexRange) -> &[HirStmtId] {
        &self.stmt_ids[range.range()]
    }

    #[must_use]
    pub fn pattern_list(&self, range: IndexRange) -> &[HirPatternId] {
        &self.pattern_ids[range.range()]
    }

    #[must_use]
    pub fn field_pattern_list(&self, range: IndexRange) -> &[HirFieldPatternId] {
        &self.field_pattern_ids[range.range()]
    }

    #[must_use]
    pub fn params_list(&self, range: IndexRange) -> &[super::HirParam] {
        &self.params[range.range()]
    }

    #[must_use]
    pub fn struct_fields_list(&self, range: IndexRange) -> &[super::HirStructField] {
        &self.struct_fields[range.range()]
    }

    #[must_use]
    pub fn enum_variants_list(&self, range: IndexRange) -> &[super::HirEnumVariant] {
        &self.enum_variants[range.range()]
    }

    #[must_use]
    pub fn func_signatures_list(&self, range: IndexRange) -> &[super::HirFuncSignature] {
        &self.func_signatures[range.range()]
    }

    #[must_use]
    pub fn bindings_list(&self, range: IndexRange) -> &[super::HirBindingItem] {
        &self.bindings[range.range()]
    }

    #[must_use]
    pub fn places_list(&self, range: IndexRange) -> &[super::HirPlace] {
        &self.places[range.range()]
    }

    #[must_use]
    pub fn for_bindings_list(&self, range: IndexRange) -> &[super::HirForBinding] {
        &self.for_bindings[range.range()]
    }

    #[must_use]
    pub fn match_arms_list(&self, range: IndexRange) -> &[super::HirMatchArm] {
        &self.match_arms[range.range()]
    }

    #[must_use]
    pub fn field_inits_list(&self, range: IndexRange) -> &[super::HirFieldInit] {
        &self.field_inits[range.range()]
    }

    #[must_use]
    pub fn lambda_params_list(&self, range: IndexRange) -> &[super::HirLambdaParam] {
        &self.lambda_params[range.range()]
    }
}
