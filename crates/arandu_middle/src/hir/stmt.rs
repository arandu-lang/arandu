//! High-level Intermediate Representation (HIR) statements, blocks, and places.

use crate::hir::expr::HirMatchArmBody;
use crate::hir::pool::{HirBlockId, HirExprId, HirPatternId, HirPool, IndexRange};
use crate::hir::symbol_name;
use crate::hir::validation::check_span;
use crate::ops::SetOp;
use crate::types::TypeId;
use crate::{SymbolId, SymbolTable};
use arandu_lexer::Span;
use smallvec::SmallVec;
use smol_str::SmolStr;

#[derive(Debug, Clone)]
pub struct HirBlock {
    pub statements: IndexRange,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirStmt {
    pub kind: HirStmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirStmtKind {
    VarDecl {
        bindings: IndexRange,
        value: HirExprId,
    },
    Set {
        places: IndexRange,
        op: SetOp,
        value: HirExprId,
    },
    Return {
        values: IndexRange,
    },
    Break,
    Continue,
    Free(HirExprId),
    Expr(HirExprId),
    If {
        condition: HirCondition,
        then_block: HirBlockId,
        else_block: Option<HirBlockId>,
    },
    For {
        clause: HirForClause,
        body: HirBlockId,
    },
    While {
        condition: HirCondition,
        body: HirBlockId,
    },
    Match {
        value: HirExprId,
        arms: IndexRange,
    },
    Defer(HirBlockId),
    ErrDefer(HirBlockId),
    Unsafe(HirBlockId),
    Error,
}

#[derive(Debug, Clone)]
pub enum HirCondition {
    Expr(HirExprId),
    Is {
        expr: HirExprId,
        pattern: HirPatternId,
    },
    And(Box<[HirCondition]>),
}

#[derive(Debug, Clone)]
pub enum HirForClause {
    In {
        span: Span,
        bindings: IndexRange,
        iterable: HirExprId,
    },
    CStyle {
        span: Span,
        init: Option<HirSimpleStmt>,
        condition: Option<HirExprId>,
        step: Option<HirSimpleStmt>,
    },
}

#[derive(Debug, Clone)]
pub struct HirForBinding {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirSimpleStmt {
    VarDecl {
        bindings: IndexRange,
        value: HirExprId,
    },
    Set {
        places: IndexRange,
        op: SetOp,
        value: HirExprId,
    },
    Expr(HirExprId),
}

#[derive(Debug, Clone)]
pub struct HirBindingItem {
    pub symbol: SymbolId,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirPlace {
    pub root_symbol: SymbolId,
    pub suffixes: SmallVec<[HirPlaceSuffix; 2]>,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirPlaceSuffix {
    Field {
        span: Span,
        name: SmolStr,
        field_symbol: Option<SymbolId>,
        ty: TypeId,
    },
    Index {
        span: Span,
        expr: HirExprId,
        ty: TypeId,
    },
}

impl HirBlock {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        check_span(self.span)?;
        let mut last_start = 0;
        for stmt_id in pool.stmt_list(self.statements) {
            let stmt = pool.stmt(*stmt_id);
            check_span(stmt.span)?;
            if stmt.span.start != 0 || stmt.span.end != 0 {
                if stmt.span.start < last_start {
                    return Err(format!(
                        "Block statement order out of sequence: span start {} is less than last start {}",
                        stmt.span.start, last_start
                    ));
                }
                last_start = stmt.span.start;
            }
            stmt.validate_invariants(pool, symbols)?;
        }
        Ok(())
    }
}

impl HirStmt {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        check_span(self.span)?;
        match &self.kind {
            HirStmtKind::VarDecl { bindings, value } => {
                for b in pool.bindings_list(*bindings) {
                    check_span(b.span)?;
                    if b.ty == crate::types::TypeInterner::preinterned_error_id() {
                        return Err(format!(
                            "Variable declaration binding '{}' has Error type",
                            symbol_name(symbols, b.symbol)
                        ));
                    }
                }
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Set {
                places,
                op: _,
                value,
            } => {
                let places_slice = pool.places_list(*places);
                if places_slice.is_empty() {
                    return Err("Set statement has no target places".to_string());
                }
                for p in places_slice {
                    check_span(p.span)?;
                    if p.ty == crate::types::TypeInterner::preinterned_error_id() {
                        return Err(format!(
                            "Set target place '{}' has Error type",
                            symbol_name(symbols, p.root_symbol)
                        ));
                    }
                }
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Return { values } => {
                for &v in pool.expr_list(*values) {
                    pool.expr(v).validate_invariants(pool, symbols)?;
                }
            }
            HirStmtKind::Break | HirStmtKind::Continue => {}
            HirStmtKind::Free(expr) => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Expr(expr) => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::If {
                condition,
                then_block,
                else_block,
            } => {
                condition.validate_invariants(pool, symbols)?;
                pool.block(*then_block).validate_invariants(pool, symbols)?;
                if let Some(eb) = else_block {
                    pool.block(*eb).validate_invariants(pool, symbols)?;
                }
            }
            HirStmtKind::For { clause, body } => {
                match clause {
                    HirForClause::In {
                        bindings, iterable, ..
                    } => {
                        for b in pool.for_bindings_list(*bindings) {
                            check_span(b.span)?;
                        }
                        pool.expr(*iterable).validate_invariants(pool, symbols)?;
                    }
                    HirForClause::CStyle {
                        init,
                        condition,
                        step,
                        ..
                    } => {
                        if let Some(i) = init {
                            i.validate_invariants(pool, symbols)?;
                        }
                        if let Some(c) = condition {
                            pool.expr(*c).validate_invariants(pool, symbols)?;
                        }
                        if let Some(s) = step {
                            s.validate_invariants(pool, symbols)?;
                        }
                    }
                }
                pool.block(*body).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::While { condition, body } => {
                condition.validate_invariants(pool, symbols)?;
                pool.block(*body).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Match { value, arms } => {
                pool.expr(*value).validate_invariants(pool, symbols)?;
                for arm in pool.match_arms_list(*arms) {
                    check_span(arm.span)?;
                    if let Some(g) = &arm.guard {
                        pool.expr(*g).validate_invariants(pool, symbols)?;
                    }
                    match &arm.body {
                        HirMatchArmBody::Expr(e) => {
                            pool.expr(*e).validate_invariants(pool, symbols)?
                        }
                        HirMatchArmBody::Block(b) => {
                            pool.block(*b).validate_invariants(pool, symbols)?
                        }
                    }
                }
            }
            HirStmtKind::Defer(b) | HirStmtKind::ErrDefer(b) | HirStmtKind::Unsafe(b) => {
                pool.block(*b).validate_invariants(pool, symbols)?;
            }
            HirStmtKind::Error => {}
        }
        Ok(())
    }
}

impl HirCondition {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        match self {
            HirCondition::Expr(expr) => pool.expr(*expr).validate_invariants(pool, symbols),
            HirCondition::Is { expr, .. } => pool.expr(*expr).validate_invariants(pool, symbols),
            HirCondition::And(conditions) => {
                for condition in conditions {
                    condition.validate_invariants(pool, symbols)?;
                }
                Ok(())
            }
        }
    }
}

impl HirSimpleStmt {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        match self {
            HirSimpleStmt::VarDecl { bindings, value } => {
                for b in pool.bindings_list(*bindings) {
                    check_span(b.span)?;
                    if b.ty == crate::types::TypeInterner::preinterned_error_id() {
                        return Err(format!(
                            "Variable declaration binding '{}' has Error type",
                            symbol_name(symbols, b.symbol)
                        ));
                    }
                }
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirSimpleStmt::Set {
                places,
                op: _,
                value,
            } => {
                for p in pool.places_list(*places) {
                    check_span(p.span)?;
                }
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirSimpleStmt::Expr(expr) => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
        }
        Ok(())
    }
}
