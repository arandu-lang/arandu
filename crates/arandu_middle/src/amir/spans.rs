//! Every source position an AMIR function carries, for moving it between item
//! and file coordinates (RFC 0042). Every struct is destructured and every
//! rvalue matched without a wildcard, so a new field or variant that carries a
//! span does not compile until it is visited here.

use super::{AmirFunc, AmirLocal, AmirRvalue, AmirStmt, AmirTemp};
use crate::Span;

impl AmirFunc {
    /// Replaces every span with `f(span)`.
    pub fn map_spans(&mut self, f: &mut impl FnMut(Span) -> Span) {
        let AmirFunc {
            symbol: _,
            return_type: _,
            receiver: _,
            params: _,
            locals,
            temps,
            blocks: _,
            block_params: _,
            stmts,
            cfg: _,
        } = self;
        for AmirLocal {
            id: _,
            ty: _,
            is_memory: _,
            symbol: _,
            span,
            use_span,
        } in locals.iter_mut()
        {
            *span = f(*span);
            if let Some(use_span) = use_span {
                *use_span = f(*use_span);
            }
        }
        for AmirTemp {
            id: _,
            ty: _,
            is_copy: _,
            is_nullable: _,
            span,
        } in temps.iter_mut()
        {
            *span = f(*span);
        }
        for statement in stmts.payloads.iter_mut() {
            match statement {
                AmirStmt::Assign { lhs: _, rhs } => rvalue_spans(rhs, f),
                AmirStmt::Store { .. }
                | AmirStmt::Call { .. }
                | AmirStmt::Free(_)
                | AmirStmt::StorageLive(_)
                | AmirStmt::StorageDead(_)
                | AmirStmt::Destroy(_)
                | AmirStmt::Nop => {}
            }
        }
    }
}

fn rvalue_spans(rvalue: &mut AmirRvalue, f: &mut impl FnMut(Span) -> Span) {
    match rvalue {
        AmirRvalue::GenInsert { origin, .. }
        | AmirRvalue::GenGet { origin, .. }
        | AmirRvalue::GenSet { origin, .. }
        | AmirRvalue::GenUpsert { origin, .. }
        | AmirRvalue::GenRemove { origin, .. } => *origin = f(*origin),
        AmirRvalue::Use(_)
        | AmirRvalue::VectorOp { .. }
        | AmirRvalue::VectorSelect { .. }
        | AmirRvalue::VectorCompare { .. }
        | AmirRvalue::VectorSplat { .. }
        | AmirRvalue::VectorShuffle { .. }
        | AmirRvalue::VectorExtractLane { .. }
        | AmirRvalue::VectorInsertLane { .. }
        | AmirRvalue::Binary { .. }
        | AmirRvalue::Unary { .. }
        | AmirRvalue::FieldAccess { .. }
        | AmirRvalue::StructLiteral { .. }
        | AmirRvalue::IndexAccess { .. }
        | AmirRvalue::Array { .. }
        | AmirRvalue::Tuple { .. }
        | AmirRvalue::Discriminant { .. }
        | AmirRvalue::EnumPayload { .. }
        | AmirRvalue::EnumConstruct { .. }
        | AmirRvalue::Len(_)
        | AmirRvalue::SliceData(_)
        | AmirRvalue::SliceView { .. }
        | AmirRvalue::SliceSubslice { .. }
        | AmirRvalue::StrBytes { .. }
        | AmirRvalue::StrView { .. }
        | AmirRvalue::Alloc(_)
        | AmirRvalue::Load(_)
        | AmirRvalue::Borrow(_)
        | AmirRvalue::BorrowMut(_)
        | AmirRvalue::RelativeBorrow { .. }
        | AmirRvalue::CoroutineReady { .. }
        | AmirRvalue::StringInterp { .. }
        | AmirRvalue::ToStr { .. }
        | AmirRvalue::BlackBox { .. } => {}
    }
}
