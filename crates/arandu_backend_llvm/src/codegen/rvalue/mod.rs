use super::{FuncEmitter, ice};
use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirRvalue, TempId};
use arandu_middle::types::ArType;

mod aggregate;
mod binary;
mod memory;
mod vector;

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_rvalue(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        rvalue: &AmirRvalue,
        lhs: Option<TempId>,
    ) -> Result<(), Diagnostic> {
        match rvalue {
            AmirRvalue::Use(operand) => {
                let value = self.operand(operand, lhs_ty)?;
                let rendered = value.render();
                self.writer
                    .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
                Ok(())
            }
            AmirRvalue::Binary { op, left, right } => {
                self.emit_binary(result, lhs_ty, *op, left, right)
            }
            AmirRvalue::Unary { op, operand } => self.emit_unary(result, lhs_ty, *op, operand),
            AmirRvalue::Load(_)
            | AmirRvalue::Borrow(_)
            | AmirRvalue::BorrowMut(_)
            | AmirRvalue::RelativeBorrow { .. }
            | AmirRvalue::Alloc(_)
            | AmirRvalue::Len(_)
            | AmirRvalue::StrView { .. }
            | AmirRvalue::SliceView { .. }
            | AmirRvalue::SliceData(_)
            | AmirRvalue::SliceSubslice { .. }
            | AmirRvalue::StrBytes { .. } => self.emit_memory_rvalue(result, lhs_ty, rvalue),
            AmirRvalue::StructLiteral { .. }
            | AmirRvalue::Tuple { .. }
            | AmirRvalue::Array { .. }
            | AmirRvalue::EnumConstruct { .. }
            | AmirRvalue::EnumPayload { .. }
            | AmirRvalue::FieldAccess { .. }
            | AmirRvalue::IndexAccess { .. } => {
                self.emit_aggregate_rvalue(result, lhs_ty, rvalue, lhs)
            }
            AmirRvalue::Discriminant { value } => self.emit_discriminant(result, lhs_ty, value),
            AmirRvalue::CoroutineReady {
                value, payload_ty, ..
            } => self.emit_coroutine_ready(result, value, *payload_ty),
            AmirRvalue::GenInsert { .. }
            | AmirRvalue::GenGet { .. }
            | AmirRvalue::GenSet { .. }
            | AmirRvalue::GenUpsert { .. }
            | AmirRvalue::GenRemove { .. } => self.emit_gen_operation(result, lhs_ty, rvalue),
            AmirRvalue::StringInterp { parts } => self.emit_string_interp(result, lhs_ty, parts),
            AmirRvalue::ToStr { value, src_ty } => self.emit_to_str(result, lhs_ty, value, *src_ty),
            AmirRvalue::VectorOp { op, left, right } => {
                self.emit_vector_op(result, lhs_ty, *op, left, right)
            }
            AmirRvalue::VectorCompare { op, left, right } => {
                self.emit_vector_compare(result, lhs_ty, *op, left, right)
            }
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => self.emit_vector_select(result, lhs_ty, mask, if_true, if_false),
            AmirRvalue::VectorSplat { value } => self.emit_vector_splat(result, lhs_ty, value),
            AmirRvalue::VectorShuffle { left, right, mask } => {
                self.emit_vector_shuffle(result, lhs_ty, left, right, mask)
            }
            AmirRvalue::VectorExtractLane { vector, lane } => {
                let ty = self.module.operand_type(vector, self.func);
                let vector = self.operand(vector, &ty)?;
                let (_, _, lane_ty) = self.module.vector_info(&ty).ok_or_else(|| {
                    ice(format!(
                        "vector lane extraction requires a vector, got {ty:?}"
                    ))
                })?;
                let lane_ty = ArType::Primitive(lane_ty);
                let lane_text = self.module.llvm_ty(&lane_ty)?;
                self.writer.line(format!(
                    "{} = extractelement {} {}, i32 {lane}",
                    result, vector.ty, vector.value
                ));
                let _ = lane_text;
                Ok(())
            }
            AmirRvalue::VectorInsertLane {
                vector,
                lane,
                value,
            } => {
                let ty = self.module.operand_type(vector, self.func);
                let vector = self.operand(vector, &ty)?;
                let (_, _, lane_ty) = self.module.vector_info(&ty).ok_or_else(|| {
                    ice(format!("vector lane insert requires a vector, got {ty:?}"))
                })?;
                let lane_ty = ArType::Primitive(lane_ty);
                let value = self.operand(value, &lane_ty)?;
                self.writer.line(format!(
                    "{} = insertelement {} {}, {} {}, i32 {lane}",
                    result, vector.ty, vector.value, value.ty, value.value
                ));
                Ok(())
            }
            AmirRvalue::BlackBox { value, .. } => {
                let value = self.operand(value, lhs_ty)?;
                let rendered = value.render();
                self.writer
                    .line(format!("{result} = select i1 true, {rendered}, {rendered}"));
                Ok(())
            }
        }
    }
}
