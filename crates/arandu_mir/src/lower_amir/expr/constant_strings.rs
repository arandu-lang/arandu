//! Literal interpolation folding precedes ownership and allocation lowering.

use super::LowerCtx;
use crate::hir::{HirExprId, HirExprKind, HirStringPart};
use crate::ops::UnaryOp;
use crate::types::ArType;
use arandu_middle::ctfe::{ConstInt, IntegerType};
use std::fmt::Write;

impl LowerCtx<'_> {
    pub(super) fn constant_interpolation(&self, parts: &[HirStringPart]) -> Option<String> {
        let mut text = String::new();
        self.append_constant_parts(parts, &mut text, 0)?;
        Some(text)
    }

    fn append_constant_parts(
        &self,
        parts: &[HirStringPart],
        text: &mut String,
        depth: usize,
    ) -> Option<()> {
        for part in parts {
            match part {
                HirStringPart::Text(value) => text.push_str(value),
                HirStringPart::Expr(id) => self.append_constant_expr(*id, text, depth)?,
            }
            // Fall back to ordinary lowering for unusually large literals.
            if text.len() > 1 << 20 {
                return None;
            }
        }
        Some(())
    }

    fn append_constant_expr(&self, id: HirExprId, text: &mut String, depth: usize) -> Option<()> {
        if depth >= 32 {
            return None;
        }
        match &self.hir.pool.expr(id).kind {
            HirExprKind::Str(value) => text.push_str(value),
            HirExprKind::Int(value) => {
                self.append_integer(id, crate::literal_pool::parse_int_literal(value)?, text)?;
            }
            HirExprKind::Bool(value) => text.push_str(if *value { "true" } else { "false" }),
            HirExprKind::Char(value) => text.push(arandu_lexer::decode_char_content(value)?),
            HirExprKind::Unary {
                op: UnaryOp::Neg,
                expr,
            } => {
                let HirExprKind::Int(value) = &self.hir.pool.expr(*expr).kind else {
                    return None;
                };
                self.append_integer(
                    id,
                    crate::literal_pool::parse_int_literal(value)?.checked_neg()?,
                    text,
                )?;
            }
            HirExprKind::ToStr { value } => self.append_constant_expr(*value, text, depth + 1)?,
            HirExprKind::StringInterp { parts } => {
                self.append_constant_parts(parts, text, depth + 1)?
            }
            // Floating-point ToStr policies are not yet identical in every
            // backend. Preserve their existing runtime formatting and effects.
            _ => return None,
        }
        Some(())
    }

    fn append_integer(&self, id: HirExprId, value: i128, text: &mut String) -> Option<()> {
        let ArType::Primitive(primitive) = self
            .tc
            .type_info
            .type_interner
            .resolve(self.hir.pool.expr(id).ty)
        else {
            return None;
        };
        let ty = IntegerType::new(primitive, self.layout).ok()?;
        let value = ConstInt::new(ty, value).ok()?.value();
        write!(text, "{value}").ok()
    }
}
