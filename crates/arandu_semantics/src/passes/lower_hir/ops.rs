//! Parser operators mapped onto the stable HIR/AMIR operator enums.

use crate::hir::{BinaryOp, SetOp, UnaryOp};
use arandu_parser as ast;

pub(super) fn lower_unary_op(op: ast::UnaryOp) -> UnaryOp {
    match op {
        ast::UnaryOp::Neg => UnaryOp::Neg,
        ast::UnaryOp::Not => UnaryOp::Not,
        ast::UnaryOp::BitNot => UnaryOp::BitNot,
        ast::UnaryOp::Await => UnaryOp::Await,
        ast::UnaryOp::Ref => UnaryOp::Ref,
        ast::UnaryOp::RefMut => UnaryOp::RefMut,
        ast::UnaryOp::Deref => UnaryOp::Deref,
    }
}

pub(super) fn lower_binary_op(op: ast::BinaryOp) -> BinaryOp {
    match op {
        ast::BinaryOp::Or => BinaryOp::Or,
        ast::BinaryOp::And => BinaryOp::And,
        ast::BinaryOp::Equal => BinaryOp::Equal,
        ast::BinaryOp::NotEqual => BinaryOp::NotEqual,
        ast::BinaryOp::Lt => BinaryOp::Lt,
        ast::BinaryOp::Gt => BinaryOp::Gt,
        ast::BinaryOp::LtEqual => BinaryOp::LtEqual,
        ast::BinaryOp::GtEqual => BinaryOp::GtEqual,
        ast::BinaryOp::Add => BinaryOp::Add,
        ast::BinaryOp::Sub => BinaryOp::Sub,
        ast::BinaryOp::Mul => BinaryOp::Mul,
        ast::BinaryOp::Div => BinaryOp::Div,
        ast::BinaryOp::Mod => BinaryOp::Mod,
        ast::BinaryOp::BitOr => BinaryOp::BitOr,
        ast::BinaryOp::BitXor => BinaryOp::BitXor,
        ast::BinaryOp::BitAnd => BinaryOp::BitAnd,
        ast::BinaryOp::ShiftLeft => BinaryOp::ShiftLeft,
        ast::BinaryOp::ShiftRight => BinaryOp::ShiftRight,
        ast::BinaryOp::NullCoalesce => BinaryOp::NullCoalesce,
        ast::BinaryOp::RangeExclusive => BinaryOp::RangeExclusive,
        ast::BinaryOp::RangeInclusive => BinaryOp::RangeInclusive,
    }
}

pub(super) fn lower_set_op(op: ast::SetOp) -> SetOp {
    match op {
        ast::SetOp::Assign => SetOp::Assign,
        ast::SetOp::AddAssign => SetOp::AddAssign,
        ast::SetOp::SubAssign => SetOp::SubAssign,
        ast::SetOp::MulAssign => SetOp::MulAssign,
        ast::SetOp::DivAssign => SetOp::DivAssign,
        ast::SetOp::ModAssign => SetOp::ModAssign,
        ast::SetOp::BitAndAssign => SetOp::BitAndAssign,
        ast::SetOp::BitOrAssign => SetOp::BitOrAssign,
        ast::SetOp::BitXorAssign => SetOp::BitXorAssign,
        ast::SetOp::ShiftLeftAssign => SetOp::ShiftLeftAssign,
        ast::SetOp::ShiftRightAssign => SetOp::ShiftRightAssign,
    }
}
