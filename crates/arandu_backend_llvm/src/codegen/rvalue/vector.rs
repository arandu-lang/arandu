use super::super::{FuncEmitter, ice};
use arandu_middle::Diagnostic;
use arandu_middle::amir::{AmirOperand, VectorCompareOp, VectorOp};
use arandu_middle::types::{ArType, VectorType};

impl<'a, 'm> FuncEmitter<'a, 'm> {
    pub(super) fn emit_vector_op(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: VectorOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let (_, _, lane) = self.module.vector_info(lhs_ty).ok_or_else(|| {
            ice(format!(
                "vector operation requires a vector, got {lhs_ty:?}"
            ))
        })?;
        let left = self.operand(left, lhs_ty)?;
        let right = self.operand(right, lhs_ty)?;
        let float = lane.is_float();
        let signed = lane.is_signed();
        let instruction = match op {
            VectorOp::Add => {
                if float {
                    "fadd"
                } else {
                    "add"
                }
            }
            VectorOp::Sub => {
                if float {
                    "fsub"
                } else {
                    "sub"
                }
            }
            VectorOp::Mul => {
                if float {
                    "fmul"
                } else {
                    "mul"
                }
            }
            VectorOp::Div => {
                if float {
                    "fdiv"
                } else if signed {
                    "sdiv"
                } else {
                    "udiv"
                }
            }
            VectorOp::BitAnd => "and",
            VectorOp::BitOr => "or",
            VectorOp::BitXor => "xor",
        };
        self.writer.line(format!(
            "{} = {instruction} {}, {}",
            result,
            left.render(),
            right.value
        ));
        Ok(())
    }

    pub(super) fn emit_vector_compare(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        op: VectorCompareOp,
        left: &AmirOperand,
        right: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let ty = self.module.operand_type(left, self.func);
        let (_, lanes, lane) = self
            .module
            .vector_info(&ty)
            .ok_or_else(|| ice(format!("vector comparison requires a vector, got {ty:?}")))?;
        let left = self.operand(left, &ty)?;
        let right = self.operand(right, &ty)?;
        let lane_ty = ArType::Primitive(lane);
        let lane_text = self.module.llvm_ty(&lane_ty)?;
        let predicate = if lane.is_float() {
            match op {
                VectorCompareOp::Eq => "oeq",
                VectorCompareOp::Ne => "une",
                VectorCompareOp::Lt => "olt",
                VectorCompareOp::Le => "ole",
                VectorCompareOp::Gt => "ogt",
                VectorCompareOp::Ge => "oge",
            }
        } else {
            match op {
                VectorCompareOp::Eq => "eq",
                VectorCompareOp::Ne => "ne",
                VectorCompareOp::Lt => {
                    if lane.is_signed() {
                        "slt"
                    } else {
                        "ult"
                    }
                }
                VectorCompareOp::Le => {
                    if lane.is_signed() {
                        "sle"
                    } else {
                        "ule"
                    }
                }
                VectorCompareOp::Gt => {
                    if lane.is_signed() {
                        "sgt"
                    } else {
                        "ugt"
                    }
                }
                VectorCompareOp::Ge => {
                    if lane.is_signed() {
                        "sge"
                    } else {
                        "uge"
                    }
                }
            }
        };
        let instruction = if lane.is_float() { "fcmp" } else { "icmp" };
        let mask = self.new_value();
        self.writer.line(format!(
            "{mask} = {instruction} {predicate} {}, {}",
            left.render(),
            right.value
        ));
        let extended = self.new_value();
        self.writer.line(format!(
            "{extended} = zext <{lanes} x i1> {mask} to <{lanes} x {lane_text}>"
        ));
        let negated = self.new_value();
        self.writer.line(format!(
            "{negated} = sub <{lanes} x {lane_text}> zeroinitializer, {extended}"
        ));
        let result_ty = self.module.llvm_ty(lhs_ty)?;
        self.writer.line(format!(
            "{result} = bitcast <{lanes} x {lane_text}> {negated} to {result_ty}"
        ));
        Ok(())
    }

    pub(super) fn emit_vector_select(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        mask: &AmirOperand,
        if_true: &AmirOperand,
        if_false: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let mask_ty = ArType::Vector(VectorType::U8x16);
        let mask = self.operand(mask, &mask_ty)?;
        let if_true = self.operand(if_true, lhs_ty)?;
        let if_false = self.operand(if_false, lhs_ty)?;
        let mask_bytes = mask.value;
        let true_bytes = self.new_value();
        self.writer.line(format!(
            "{true_bytes} = bitcast {} to <16 x i8>",
            if_true.render()
        ));
        let false_bytes = self.new_value();
        self.writer.line(format!(
            "{false_bytes} = bitcast {} to <16 x i8>",
            if_false.render()
        ));
        let inverse = self.new_value();
        self.writer.line(format!(
            "{inverse} = xor <16 x i8> {mask_bytes}, <i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1, i8 -1>"
        ));
        let selected_true = self.new_value();
        self.writer.line(format!(
            "{selected_true} = and <16 x i8> {mask_bytes}, {true_bytes}"
        ));
        let selected_false = self.new_value();
        self.writer.line(format!(
            "{selected_false} = and <16 x i8> {inverse}, {false_bytes}"
        ));
        let combined = self.new_value();
        self.writer.line(format!(
            "{combined} = or <16 x i8> {selected_true}, {selected_false}"
        ));
        let result_ty = self.module.llvm_ty(lhs_ty)?;
        self.writer.line(format!(
            "{result} = bitcast <16 x i8> {combined} to {result_ty}"
        ));
        Ok(())
    }

    pub(super) fn emit_vector_splat(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        value: &AmirOperand,
    ) -> Result<(), Diagnostic> {
        let (_, lanes, lane) = self
            .module
            .vector_info(lhs_ty)
            .ok_or_else(|| ice(format!("vector splat requires a vector, got {lhs_ty:?}")))?;
        let lane_ty = ArType::Primitive(lane);
        let value = self.operand(value, &lane_ty)?;
        let vector = self.module.llvm_ty(lhs_ty)?;
        let inserted = self.new_value();
        self.writer.line(format!(
            "{inserted} = insertelement {vector} undef, {}, i32 0",
            value.render()
        ));
        self.writer.line(format!(
            "{} = shufflevector {vector} {inserted}, {vector} undef, <{lanes} x i32> zeroinitializer",
            result
        ));
        Ok(())
    }

    pub(super) fn emit_vector_shuffle(
        &mut self,
        result: &str,
        lhs_ty: &ArType,
        left: &AmirOperand,
        right: &AmirOperand,
        mask: &[u8; 16],
    ) -> Result<(), Diagnostic> {
        let ty = self.module.operand_type(left, self.func);
        let left = self.operand(left, &ty)?;
        let right = self.operand(right, &ty)?;
        let left_bytes = self.new_value();
        self.writer.line(format!(
            "{left_bytes} = bitcast {} to <16 x i8>",
            left.render()
        ));
        let right_bytes = self.new_value();
        self.writer.line(format!(
            "{right_bytes} = bitcast {} to <16 x i8>",
            right.render()
        ));
        let mask = mask
            .iter()
            .map(|index| format!("i32 {index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let shuffled = self.new_value();
        self.writer.line(format!(
            "{shuffled} = shufflevector <16 x i8> {left_bytes}, <16 x i8> {right_bytes}, <16 x i32> <{mask}>"
        ));
        let result_ty = self.module.llvm_ty(lhs_ty)?;
        self.writer.line(format!(
            "{result} = bitcast <16 x i8> {shuffled} to {result_ty}"
        ));
        Ok(())
    }
}
