//! High-level Intermediate Representation (HIR) expressions and operands.

use crate::hir::pool::{HirBlockId, HirExprId, HirPatternId, HirPool, IndexRange};
use crate::hir::stmt::HirCondition;
use crate::hir::validation::check_span;
use crate::ops::{BinaryOp, UnaryOp};
use crate::types::TypeId;
use crate::{SymbolId, SymbolTable};
use arandu_lexer::Span;
pub use arandu_parser::LayoutQuery;
use smol_str::SmolStr;

#[derive(Debug, Clone)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: TypeId,
    pub span: Span,
}

/// Builtin `Result.Ok` / `Result.Err` / `Option.Some` constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCtorVariant {
    Ok,
    Err,
    Some,
    /// Unit `Option.None` (no payload).
    None,
    /// A3.6: `Poll.Ready(v)`
    PollReady,
    /// A3.6: `Poll.Pending`
    PollPending,
}

impl ResultCtorVariant {
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let bare = name.rsplit('.').next().unwrap_or(name);
        match bare {
            "Ok" => Some(Self::Ok),
            "Err" => Some(Self::Err),
            "Some" => Some(Self::Some),
            "None" => Some(Self::None),
            "Ready" => Some(Self::PollReady),
            "Pending" => Some(Self::PollPending),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum HirExprKind {
    /// Remains typed until AMIR lowering, where the complete target ABI is
    /// available. Monomorphization substitutes the operand type, not its name.
    Layout {
        query: LayoutQuery,
        operand_ty: TypeId,
    },
    /// Exact frozen IEEE encoding produced by CTFE, not a source lexeme.
    FloatBits(crate::ctfe::ConstFloat),
    /// Frozen byte view over owned immutable UTF-8 literal storage. Lowering
    /// re-interns the backing in the destination; no VM pointer can escape.
    FrozenBytes(crate::ctfe::ConstBytes),
    Path {
        symbol: SymbolId,
    },
    TypePath {
        type_symbol: SymbolId,
        member_symbol: SymbolId,
    },
    Generic {
        callee: HirExprId,
        args: Vec<TypeId>,
    },
    Field {
        base: HirExprId,
        field: SmolStr,
    },
    SafeField {
        base: HirExprId,
        field: SmolStr,
    },
    Index {
        base: HirExprId,
        index: HirExprId,
    },
    SafeIndex {
        base: HirExprId,
        index: HirExprId,
    },
    Try {
        expr: HirExprId,
    },
    Call {
        callee: HirExprId,
        args: IndexRange,
        trailing_block: Option<HirBlockId>,
    },
    ResultCtor {
        variant: ResultCtorVariant,
        value: HirExprId,
    },
    StructLiteral {
        struct_symbol: SymbolId,
        fields: IndexRange,
    },
    Array {
        items: IndexRange,
    },
    /// Evaluate the initializer once. The concrete/symbolic length is in `ty`.
    ArrayRepeat {
        value: HirExprId,
    },
    /// Semantic product value (including frozen multiple-return CTFE values).
    Tuple {
        items: IndexRange,
    },
    Lambda {
        params: IndexRange,
        body: HirLambdaBody,
    },
    Alloc {
        expr: HirExprId,
    },
    AsyncBlock {
        block: HirBlockId,
    },
    /// An isolated compile-time block with a local return target.
    ValueBlock {
        block: HirBlockId,
    },
    UnsafeBlock {
        block: HirBlockId,
    },
    If {
        condition: HirCondition,
        then_block: HirBlockId,
        else_block: HirBlockId,
    },
    Match {
        value: HirExprId,
        arms: IndexRange,
    },
    Catch {
        expr: HirExprId,
        handler: HirCatchHandler,
    },
    NullCoalesce {
        left: HirExprId,
        right: HirExprId,
    },
    Cast {
        expr: HirExprId,
        target_ty: TypeId,
    },
    Unary {
        op: UnaryOp,
        expr: HirExprId,
    },
    Binary {
        op: BinaryOp,
        left: HirExprId,
        right: HirExprId,
    },
    Int(SmolStr),
    Float(SmolStr),
    Bool(bool),
    Char(SmolStr),
    Str(SmolStr),
    /// String interpolation: a sequence of literal text segments and sub-expressions
    /// that are concatenated at runtime to produce a `str` value.
    StringInterp {
        parts: Vec<HirStringPart>,
    },
    /// Compiler intrinsic: format a ToStr-v0.1 value as `str` (`x.to_str()`).
    ToStr {
        value: HirExprId,
    },
    Nil,
    Error,
}

/// One segment of an interpolated string.
#[derive(Debug, Clone)]
pub enum HirStringPart {
    /// A literal text segment (already known at compile time).
    Text(SmolStr),
    /// A sub-expression whose runtime value is converted to string and concatenated.
    Expr(HirExprId),
}

#[derive(Debug, Clone)]
pub struct HirFieldInit {
    pub span: Span,
    pub name: SmolStr,
    pub value: HirExprId,
}

#[derive(Debug, Clone)]
pub struct HirLambdaParam {
    pub span: Span,
    pub symbol: SymbolId,
    pub ty: TypeId,
}

#[derive(Debug, Clone)]
pub enum HirLambdaBody {
    Expr(HirExprId),
    Block(HirBlockId),
}

#[derive(Debug, Clone)]
pub struct HirMatchArm {
    pub span: Span,
    pub pattern: HirPatternId,
    pub guard: Option<HirExprId>,
    pub body: HirMatchArmBody,
}

#[derive(Debug, Clone)]
pub enum HirMatchArmBody {
    Expr(HirExprId),
    Block(HirBlockId),
}

#[derive(Debug, Clone)]
pub enum HirCatchHandler {
    Expr(HirExprId),
    Block {
        error_symbol: Option<SymbolId>,
        error_name: Option<String>,
        block: HirBlockId,
    },
}

impl HirExpr {
    pub fn validate_invariants(&self, pool: &HirPool, symbols: &SymbolTable) -> Result<(), String> {
        check_span(self.span)?;
        if matches!(self.kind, HirExprKind::Error) {
            return Ok(());
        }
        if self.ty == crate::types::TypeInterner::preinterned_error_id() {
            return Err(format!(
                "Expression has Error type at byte {}",
                self.span.start
            ));
        }
        match &self.kind {
            HirExprKind::Path { symbol } => {
                let _sym = symbols.get(*symbol);
            }
            HirExprKind::TypePath {
                type_symbol,
                member_symbol,
                ..
            } => {
                let _t_sym = symbols.get(*type_symbol);
                let _m_sym = symbols.get(*member_symbol);
            }
            HirExprKind::Generic { callee, .. } => {
                pool.expr(*callee).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Field { base, .. } | HirExprKind::SafeField { base, .. } => {
                let base_node = pool.expr(*base);
                base_node.validate_invariants(pool, symbols)?;
                if base_node.ty == crate::types::TypeInterner::preinterned_error_id() {
                    return Err("Field access base expression has Error type".to_string());
                }
            }
            HirExprKind::Index { base, index } | HirExprKind::SafeIndex { base, index } => {
                pool.expr(*base).validate_invariants(pool, symbols)?;
                pool.expr(*index).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Try { expr } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Call {
                callee,
                args,
                trailing_block,
            } => {
                let callee_node = pool.expr(*callee);
                callee_node.validate_invariants(pool, symbols)?;
                if callee_node.ty == crate::types::TypeInterner::preinterned_error_id() {
                    return Err("Call callee expression has Error type".to_string());
                }
                for &arg in pool.expr_list(*args) {
                    pool.expr(arg).validate_invariants(pool, symbols)?;
                }
                if let Some(tb) = trailing_block {
                    pool.block(*tb).validate_invariants(pool, symbols)?;
                }
            }
            HirExprKind::ResultCtor { value, .. } => {
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirExprKind::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let _sym = symbols.get(*struct_symbol);
                for f in pool.field_inits_list(*fields) {
                    check_span(f.span)?;
                    pool.expr(f.value).validate_invariants(pool, symbols)?;
                }
            }
            HirExprKind::Array { items } | HirExprKind::Tuple { items } => {
                for &item in pool.expr_list(*items) {
                    pool.expr(item).validate_invariants(pool, symbols)?;
                }
            }
            HirExprKind::ArrayRepeat { value } => {
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Lambda { params, body } => {
                for p in pool.lambda_params_list(*params) {
                    check_span(p.span)?;
                    let _sym = symbols.get(p.symbol);
                }
                match body {
                    HirLambdaBody::Expr(e) => pool.expr(*e).validate_invariants(pool, symbols)?,
                    HirLambdaBody::Block(b) => pool.block(*b).validate_invariants(pool, symbols)?,
                }
            }
            HirExprKind::Alloc { expr } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirExprKind::AsyncBlock { block }
            | HirExprKind::UnsafeBlock { block }
            | HirExprKind::ValueBlock { block } => {
                pool.block(*block).validate_invariants(pool, symbols)?;
            }
            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                condition.validate_invariants(pool, symbols)?;
                pool.block(*then_block).validate_invariants(pool, symbols)?;
                pool.block(*else_block).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Match { value, arms } => {
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
            HirExprKind::Catch { expr, handler } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
                match handler {
                    HirCatchHandler::Expr(e) => pool.expr(*e).validate_invariants(pool, symbols)?,
                    HirCatchHandler::Block { block, .. } => {
                        pool.block(*block).validate_invariants(pool, symbols)?
                    }
                }
            }
            HirExprKind::NullCoalesce { left, right } => {
                pool.expr(*left).validate_invariants(pool, symbols)?;
                pool.expr(*right).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Cast { expr, .. } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Unary { expr, .. } => {
                pool.expr(*expr).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Binary { left, right, .. } => {
                pool.expr(*left).validate_invariants(pool, symbols)?;
                pool.expr(*right).validate_invariants(pool, symbols)?;
            }
            HirExprKind::StringInterp { parts } => {
                for part in parts {
                    if let HirStringPart::Expr(e) = part {
                        pool.expr(*e).validate_invariants(pool, symbols)?;
                    }
                }
            }
            HirExprKind::ToStr { value } => {
                pool.expr(*value).validate_invariants(pool, symbols)?;
            }
            HirExprKind::Layout { .. }
            | HirExprKind::Int(_)
            | HirExprKind::Float(_)
            | HirExprKind::FloatBits(_)
            | HirExprKind::FrozenBytes(_)
            | HirExprKind::Bool(_)
            | HirExprKind::Char(_)
            | HirExprKind::Str(_)
            | HirExprKind::Nil
            | HirExprKind::Error => {}
        }
        Ok(())
    }
}
