//! Stable operator enums shared by HIR and AMIR (decoupled from the parser AST).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
    Await,
    /// Shared address-of: `&expr` (F2.0).
    Ref,
    /// Exclusive address-of: `&mut expr` (F2.0).
    RefMut,
    /// Dereference `*expr` (safe for refs; raw ptr needs unsafe).
    Deref,
}

impl UnaryOp {
    /// Stable, versioned hashing tag. Keep exhaustive so new operators cannot
    /// silently reuse an existing codegen cache entry.
    #[must_use]
    pub const fn stable_tag(self) -> u8 {
        match self {
            Self::Neg => 0,
            Self::Not => 1,
            Self::BitNot => 2,
            Self::Await => 3,
            Self::Ref => 4,
            Self::RefMut => 5,
            Self::Deref => 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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

impl BinaryOp {
    /// Stable, versioned hashing tag. Keep exhaustive so new operators cannot
    /// silently reuse an existing codegen cache entry.
    #[must_use]
    pub const fn stable_tag(self) -> u8 {
        match self {
            Self::Or => 0,
            Self::And => 1,
            Self::Equal => 2,
            Self::NotEqual => 3,
            Self::Lt => 4,
            Self::Gt => 5,
            Self::LtEqual => 6,
            Self::GtEqual => 7,
            Self::Add => 8,
            Self::Sub => 9,
            Self::Mul => 10,
            Self::Div => 11,
            Self::Mod => 12,
            Self::BitOr => 13,
            Self::BitXor => 14,
            Self::BitAnd => 15,
            Self::ShiftLeft => 16,
            Self::ShiftRight => 17,
            Self::NullCoalesce => 18,
            Self::RangeExclusive => 19,
            Self::RangeInclusive => 20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
