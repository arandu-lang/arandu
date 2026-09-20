use super::{Expr, IndexRange, TypeExprId};
use arandu_lexer::Span;
use smol_str::SmolStr;

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub span: Span,
    pub statements: IndexRange,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    VarDecl {
        span: Span,
        bindings: Vec<BindingItem>,
        value: Expr,
    },
    Set {
        span: Span,
        places: Vec<Place>,
        op: SetOp,
        value: Expr,
    },
    Return {
        span: Span,
        values: Vec<Expr>,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    Free {
        span: Span,
        expr: Expr,
    },
    Expr {
        span: Span,
        expr: Expr,
        has_semi: bool,
    },
    If {
        span: Span,
        condition: Condition,
        then_block: Block,
        else_block: Option<Block>,
    },
    For {
        span: Span,
        clause: ForClause,
        body: Block,
    },
    While {
        span: Span,
        condition: Condition,
        body: Block,
    },
    Match {
        span: Span,
        expr: Expr,
    },
    Defer {
        span: Span,
        body: DeferBody,
    },
    ErrDefer {
        span: Span,
        body: DeferBody,
    },
    Unsafe {
        span: Span,
        block: Block,
    },
    Error(Span),
}

impl Stmt {
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Stmt::VarDecl { span, .. }
            | Stmt::Set { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::Free { span, .. }
            | Stmt::Expr { span, .. }
            | Stmt::If { span, .. }
            | Stmt::For { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Match { span, .. }
            | Stmt::Defer { span, .. }
            | Stmt::ErrDefer { span, .. }
            | Stmt::Unsafe { span, .. }
            | Stmt::Error(span) => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    Expr {
        span: Span,
        expr: Expr,
    },
    Is {
        span: Span,
        expr: Expr,
        pattern: super::PatternId,
    },
    /// Left-to-right short-circuit conjunction. Pattern bindings from a
    /// successful clause are visible to subsequent clauses and the body.
    And {
        span: Span,
        conditions: Box<[Condition]>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ForClause {
    In {
        span: Span,
        bindings: Vec<ForBinding>,
        iterable: Expr,
    },
    CStyle {
        span: Span,
        init: Option<SimpleStmt>,
        condition: Option<Expr>,
        step: Option<SimpleStmt>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForBinding {
    pub span: Span,
    pub mutable: bool,
    pub name: SmolStr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SimpleStmt {
    VarDecl {
        span: Span,
        bindings: Vec<BindingItem>,
        value: Expr,
    },
    Set {
        span: Span,
        places: Vec<Place>,
        op: SetOp,
        value: Expr,
    },
    Expr {
        span: Span,
        expr: Expr,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeferBody {
    Expr { span: Span, expr: Expr },
    Block { span: Span, block: Block },
}

#[derive(Debug, Clone, PartialEq)]
pub struct BindingItem {
    pub span: Span,
    pub mutable: bool,
    pub name: SmolStr,
    pub ty: Option<TypeExprId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub span: Span,
    pub root: SmolStr,
    pub suffixes: Vec<PlaceSuffix>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaceSuffix {
    Field { span: Span, name: SmolStr },
    Index { span: Span, expr: Expr },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    ShiftLeftAssign,
    ShiftRightAssign,
}
