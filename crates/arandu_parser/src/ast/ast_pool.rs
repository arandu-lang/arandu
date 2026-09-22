use super::{
    Block, CatchHandler, FieldInit, FieldPattern, LambdaParam, MatchArm, Pattern, Stmt, StringPart,
    TopLevelDecl, TypeExpr,
};
use arandu_lexer::Span;
use smallvec::SmallVec;
use smol_str::SmolStr;
use std::num::NonZeroU32;

// ─── ID Types ──────────────────────────────────────────────────────────────────
// All IDs wrap NonZeroU32 for niche optimization: Option<XId> == 4 bytes.

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub NonZeroU32);

        impl $name {
            /// Creates a new ID from a 0-based index.
            ///
            /// # Panics
            /// Panics if `index == usize::MAX` (would overflow the NonZero encoding).
            #[must_use]
            pub fn new(index: usize) -> Self {
                let raw = u32::try_from(index)
                    .ok()
                    .and_then(|i| i.checked_add(1))
                    .and_then(NonZeroU32::new);
                match raw {
                    Some(n) => Self(n),
                    None => panic!("{}: dense id overflow at index {index}", stringify!($name)),
                }
            }

            /// Returns the 0-based index of this ID.
            #[must_use]
            pub fn as_usize(self) -> usize {
                (self.0.get() - 1) as usize
            }
        }
    };
}

define_id!(
    /// A type-safe identifier for AST expressions.
    ExprId
);

define_id!(
    /// A type-safe identifier for AST statements.
    StmtId
);

define_id!(
    /// A type-safe identifier for AST blocks.
    BlockId
);

define_id!(
    /// A type-safe identifier for type expressions.
    TypeExprId
);

define_id!(
    /// A type-safe identifier for pool lambda parameters.
    LambdaParamId
);

define_id!(
    /// A type-safe identifier for pool field initializers (struct literals).
    FieldInitId
);

define_id!(
    /// A type-safe identifier for pool string parts (interpolated strings).
    StringPartId
);

define_id!(
    /// A type-safe identifier for pool match arms.
    MatchArmId
);

define_id!(
    /// A type-safe identifier for pool patterns.
    PatternId
);

define_id!(
    /// A type-safe identifier for pool field patterns.
    FieldPatternId
);

define_id!(
    /// A type-safe identifier for catch handlers.
    CatchHandlerId
);

define_id!(
    /// A type-safe identifier for declarations.
    DeclId
);

// ─── IndexRange ────────────────────────────────────────────────────────────────

/// Contiguous range index pointing to a central backing buffer.
/// Avoids the overhead and cache fragmentation of nested `Vec` allocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndexRange {
    pub start: u32,
    pub len: u32,
}

impl IndexRange {
    /// Creates an empty index range.
    #[must_use]
    pub const fn empty() -> Self {
        Self { start: 0, len: 0 }
    }

    /// Returns `true` if the range contains no elements.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Converts this range to a standard rust range.
    #[must_use]
    pub fn range(self) -> std::ops::Range<usize> {
        (self.start as usize)..(self.start as usize + self.len as usize)
    }
}

// ─── ExprKind ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Path {
        path: SmallVec<[SmolStr; 3]>,
    },
    TypePath {
        type_name: super::TypeName,
        member: SmolStr,
    },
    /// T2.2: scoped enum/result sugar `.Ok(x)` / `.None` (resolved via expected type).
    VariantSugar {
        name: SmolStr,
        /// Empty range for unit variants (`.None`, `.Pending`).
        args: IndexRange,
    },
    Generic {
        callee: ExprId,
        args: IndexRange,
    }, // type_expr_ids range
    Field {
        base: ExprId,
        field: SmolStr,
    },
    SafeField {
        base: ExprId,
        field: SmolStr,
    },
    Index {
        base: ExprId,
        index: ExprId,
    },
    SafeIndex {
        base: ExprId,
        index: ExprId,
    },
    Try {
        expr: ExprId,
    },
    Call {
        callee: ExprId,
        args: IndexRange,
        trailing_block: Option<BlockId>,
    }, // expr_ids range
    StructLiteral {
        ty: TypeExprId,
        fields: IndexRange,
    }, // field_init_ids range
    Array {
        items: IndexRange,
    }, // expr_ids range
    Lambda {
        params: IndexRange,
        body: super::LambdaBody,
    }, // lambda_param_ids range. body is non-recursive since its inner exprs use ExprId.
    Alloc {
        expr: ExprId,
    },
    AsyncBlock {
        block: BlockId,
    },
    UnsafeBlock {
        block: BlockId,
    },
    If {
        condition: super::Condition,
        then_block: BlockId,
        else_block: BlockId,
    },
    Match {
        value: ExprId,
        arms: IndexRange,
    }, // match_arm_ids range
    Catch {
        expr: ExprId,
        handler: CatchHandlerId,
    },
    NullCoalesce {
        left: ExprId,
        right: ExprId,
    },
    Cast {
        expr: ExprId,
        ty: TypeExprId,
    },
    Error,
    Group {
        expr: ExprId,
    },
    Unary {
        op: crate::ast::UnaryOp,
        expr: ExprId,
    },
    Binary {
        op: crate::ast::BinaryOp,
        left: ExprId,
        right: ExprId,
    },
    Int {
        value: SmolStr,
    },
    Float {
        value: SmolStr,
    },
    Bool {
        value: bool,
    },
    Char {
        value: SmolStr,
    },
    InterpolatedString {
        parts: IndexRange,
    }, // string_part_ids range
    Nil,
}

// ─── AstPool ───────────────────────────────────────────────────────────────────

/// A centralized AST Pool containing contiguous vectors of node components and metadata.
/// Designed for high cache density, fast compilation, and easy serialization.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AstPool {
    // ── Expression storage (SoA layout) ────────────────────────────────────
    pub exprs: Vec<ExprKind>,
    pub expr_spans: Vec<Span>,

    pub stmts: Vec<Stmt>,
    pub stmt_spans: Vec<Span>,

    // ── Support type vector storage (stores the legacy types using ExprId) ─
    pub type_exprs: Vec<TypeExpr>,
    pub type_expr_spans: Vec<Span>,

    pub blocks: Vec<Block>,

    pub patterns: Vec<Pattern>,
    pub field_patterns: Vec<FieldPattern>,

    pub decls: Vec<TopLevelDecl>,
    pub decl_spans: Vec<Span>,

    pub field_inits: Vec<FieldInit>,

    pub lambda_params: Vec<LambdaParam>,

    pub string_parts: Vec<StringPart>,

    pub match_arms: Vec<MatchArm>,

    pub catch_handlers: Vec<CatchHandler>,

    // ── Contiguous backing lists for IndexRange references ─────────────────
    pub expr_ids: Vec<ExprId>,
    pub stmt_ids: Vec<StmtId>,
    pub type_expr_ids: Vec<TypeExprId>,
    pub field_init_ids: Vec<FieldInitId>,
    pub lambda_param_ids: Vec<LambdaParamId>,
    pub string_part_ids: Vec<StringPartId>,
    pub match_arm_ids: Vec<MatchArmId>,
    pub pattern_ids: Vec<PatternId>,
    pub field_pattern_ids: Vec<FieldPatternId>,
}

impl AstPool {
    /// Creates an empty `AstPool`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ── Allocators ─────────────────────────────────────────────────────────

    pub fn alloc_expr(&mut self, kind: ExprKind, span: Span) -> ExprId {
        let id = ExprId::new(self.exprs.len());
        self.exprs.push(kind);
        self.expr_spans.push(span);
        id
    }

    pub fn alloc_stmt(&mut self, stmt: Stmt) -> StmtId {
        let id = StmtId::new(self.stmts.len());
        let span = stmt.span();
        self.stmts.push(stmt);
        self.stmt_spans.push(span);
        id
    }

    pub fn alloc_type_expr(&mut self, ty: TypeExpr) -> TypeExprId {
        let id = TypeExprId::new(self.type_exprs.len());
        let span = ty.span();
        self.type_exprs.push(ty);
        self.type_expr_spans.push(span);
        id
    }

    pub fn alloc_block(&mut self, block: Block) -> BlockId {
        let id = BlockId::new(self.blocks.len());
        self.blocks.push(block);
        id
    }

    pub fn alloc_pattern(&mut self, pattern: Pattern) -> PatternId {
        let id = PatternId::new(self.patterns.len());
        self.patterns.push(pattern);
        id
    }

    pub fn alloc_field_pattern(&mut self, field: FieldPattern) -> FieldPatternId {
        let id = FieldPatternId::new(self.field_patterns.len());
        self.field_patterns.push(field);
        id
    }

    pub fn alloc_field_init(&mut self, init: FieldInit) -> FieldInitId {
        let id = FieldInitId::new(self.field_inits.len());
        self.field_inits.push(init);
        id
    }

    pub fn alloc_lambda_param(&mut self, param: LambdaParam) -> LambdaParamId {
        let id = LambdaParamId::new(self.lambda_params.len());
        self.lambda_params.push(param);
        id
    }

    pub fn alloc_string_part(&mut self, part: StringPart) -> StringPartId {
        let id = StringPartId::new(self.string_parts.len());
        self.string_parts.push(part);
        id
    }

    pub fn alloc_match_arm(&mut self, arm: MatchArm) -> MatchArmId {
        let id = MatchArmId::new(self.match_arms.len());
        self.match_arms.push(arm);
        id
    }

    pub fn alloc_catch_handler(&mut self, handler: CatchHandler) -> CatchHandlerId {
        let id = CatchHandlerId::new(self.catch_handlers.len());
        self.catch_handlers.push(handler);
        id
    }

    pub fn alloc_decl(&mut self, decl: TopLevelDecl) -> DeclId {
        let id = DeclId::new(self.decls.len());
        let span = decl.span();
        self.decls.push(decl);
        self.decl_spans.push(span);
        id
    }

    // ── IndexRange list allocators ─────────────────────────────────────────

    pub fn alloc_expr_list(&mut self, ids: &[ExprId]) -> IndexRange {
        let start = self.expr_ids.len() as u32;
        self.expr_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    #[must_use]
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[id.as_usize()]
    }

    #[must_use]
    pub fn stmt_span(&self, id: StmtId) -> Span {
        self.stmt_spans[id.as_usize()]
    }

    pub fn alloc_stmt_list(&mut self, ids: &[StmtId]) -> IndexRange {
        let start = self.stmt_ids.len() as u32;
        self.stmt_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_type_expr_list(&mut self, ids: &[TypeExprId]) -> IndexRange {
        let start = self.type_expr_ids.len() as u32;
        self.type_expr_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_field_init_list(&mut self, ids: &[FieldInitId]) -> IndexRange {
        let start = self.field_init_ids.len() as u32;
        self.field_init_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_lambda_param_list(&mut self, ids: &[LambdaParamId]) -> IndexRange {
        let start = self.lambda_param_ids.len() as u32;
        self.lambda_param_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_string_part_list(&mut self, ids: &[StringPartId]) -> IndexRange {
        let start = self.string_part_ids.len() as u32;
        self.string_part_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_match_arm_list(&mut self, ids: &[MatchArmId]) -> IndexRange {
        let start = self.match_arm_ids.len() as u32;
        self.match_arm_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_pattern_list(&mut self, ids: &[PatternId]) -> IndexRange {
        let start = self.pattern_ids.len() as u32;
        self.pattern_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    pub fn alloc_field_pattern_list(&mut self, ids: &[FieldPatternId]) -> IndexRange {
        let start = self.field_pattern_ids.len() as u32;
        self.field_pattern_ids.extend_from_slice(ids);
        IndexRange {
            start,
            len: ids.len() as u32,
        }
    }

    // ── Lookup helpers ─────────────────────────────────────────────────────

    #[must_use]
    pub fn expr(&self, id: ExprId) -> &ExprKind {
        &self.exprs[id.as_usize()]
    }

    #[must_use]
    pub fn expr_span(&self, id: ExprId) -> Span {
        self.expr_spans[id.as_usize()]
    }

    #[must_use]
    pub fn type_expr(&self, id: TypeExprId) -> &TypeExpr {
        &self.type_exprs[id.as_usize()]
    }

    #[must_use]
    pub fn type_expr_span(&self, id: TypeExprId) -> Span {
        self.type_expr_spans[id.as_usize()]
    }

    #[must_use]
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.as_usize()]
    }

    #[must_use]
    pub fn pattern(&self, id: PatternId) -> &Pattern {
        &self.patterns[id.as_usize()]
    }

    #[must_use]
    pub fn field_pattern(&self, id: FieldPatternId) -> &FieldPattern {
        &self.field_patterns[id.as_usize()]
    }

    #[must_use]
    pub fn field_init(&self, id: FieldInitId) -> &FieldInit {
        &self.field_inits[id.as_usize()]
    }

    #[must_use]
    pub fn lambda_param(&self, id: LambdaParamId) -> &LambdaParam {
        &self.lambda_params[id.as_usize()]
    }

    #[must_use]
    pub fn string_part(&self, id: StringPartId) -> &StringPart {
        &self.string_parts[id.as_usize()]
    }

    #[must_use]
    pub fn match_arm(&self, id: MatchArmId) -> &MatchArm {
        &self.match_arms[id.as_usize()]
    }

    #[must_use]
    pub fn catch_handler(&self, id: CatchHandlerId) -> &CatchHandler {
        &self.catch_handlers[id.as_usize()]
    }

    #[must_use]
    pub fn decl(&self, id: DeclId) -> &TopLevelDecl {
        &self.decls[id.as_usize()]
    }

    #[must_use]
    pub fn decl_span(&self, id: DeclId) -> Span {
        self.decl_spans[id.as_usize()]
    }

    #[must_use]
    pub fn expr_list(&self, range: IndexRange) -> &[ExprId] {
        &self.expr_ids[range.range()]
    }

    #[must_use]
    pub fn type_expr_list(&self, range: IndexRange) -> &[TypeExprId] {
        &self.type_expr_ids[range.range()]
    }

    #[must_use]
    pub fn stmt_list(&self, range: IndexRange) -> &[StmtId] {
        &self.stmt_ids[range.range()]
    }

    #[must_use]
    pub fn field_init_list(&self, range: IndexRange) -> &[FieldInitId] {
        &self.field_init_ids[range.range()]
    }

    #[must_use]
    pub fn lambda_param_list(&self, range: IndexRange) -> &[LambdaParamId] {
        &self.lambda_param_ids[range.range()]
    }

    #[must_use]
    pub fn string_part_list(&self, range: IndexRange) -> &[StringPartId] {
        &self.string_part_ids[range.range()]
    }

    #[must_use]
    pub fn match_arm_list(&self, range: IndexRange) -> &[MatchArmId] {
        &self.match_arm_ids[range.range()]
    }

    #[must_use]
    pub fn pattern_list(&self, range: IndexRange) -> &[PatternId] {
        &self.pattern_ids[range.range()]
    }

    #[must_use]
    pub fn field_pattern_list(&self, range: IndexRange) -> &[FieldPatternId] {
        &self.field_pattern_ids[range.range()]
    }
}
