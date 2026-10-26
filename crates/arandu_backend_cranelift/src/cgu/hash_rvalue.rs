use super::HashContext;
use arandu_middle::amir::AmirRvalue;

impl HashContext<'_> {
    pub(super) fn rvalue(&mut self, rvalue: &AmirRvalue) {
        match rvalue {
            AmirRvalue::VectorSelect {
                mask,
                if_true,
                if_false,
            } => {
                self.hash.tag(34);
                self.operand(mask);
                self.operand(if_true);
                self.operand(if_false);
            }
            AmirRvalue::VectorCompare { op, left, right } => {
                self.hash.tag(35);
                self.hash.tag(op.stable_tag());
                self.operand(left);
                self.operand(right);
            }
            AmirRvalue::VectorOp { op, left, right } => {
                self.hash.tag(29);
                self.hash.tag(op.stable_tag());
                self.operand(left);
                self.operand(right);
            }
            AmirRvalue::VectorSplat { value } => {
                self.hash.tag(30);
                self.operand(value);
            }
            AmirRvalue::VectorShuffle { left, right, mask } => {
                self.hash.tag(31);
                self.operand(left);
                self.operand(right);
                for index in mask {
                    self.hash.tag(*index);
                }
            }
            AmirRvalue::VectorExtractLane { vector, lane } => {
                self.hash.tag(32);
                self.operand(vector);
                self.hash.tag(*lane);
            }
            AmirRvalue::VectorInsertLane {
                vector,
                lane,
                value,
            } => {
                self.hash.tag(33);
                self.operand(vector);
                self.hash.tag(*lane);
                self.operand(value);
            }
            AmirRvalue::Use(operand) => {
                self.hash.tag(0);
                self.operand(operand);
            }
            AmirRvalue::Binary { op, left, right } => {
                self.hash.tag(1);
                self.hash.tag(op.stable_tag());
                self.operand(left);
                self.operand(right);
            }
            AmirRvalue::Unary { op, operand } => {
                self.hash.tag(2);
                self.hash.tag(op.stable_tag());
                self.operand(operand);
            }
            AmirRvalue::FieldAccess { base, field } => {
                self.hash.tag(3);
                self.operand(base);
                self.hash.usize(*field);
            }
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                self.hash.tag(4);
                self.symbol(*struct_symbol);
                self.hash.usize(fields.len());
                for (name, operand) in fields {
                    self.hash.str(name);
                    self.operand(operand);
                }
            }
            AmirRvalue::IndexAccess {
                base,
                index,
                in_bounds,
            } => {
                self.hash.tag(5);
                self.operand(base);
                self.operand(index);
                self.hash.tag(u8::from(*in_bounds));
            }
            AmirRvalue::Array { items } => {
                self.hash.tag(6);
                self.operands(items);
            }
            AmirRvalue::Tuple { items } => {
                self.hash.tag(7);
                self.operands(items);
            }
            AmirRvalue::Discriminant { value } => {
                self.hash.tag(8);
                self.operand(value);
            }
            AmirRvalue::EnumPayload {
                value,
                variant,
                variant_tag,
                index,
                field_ty,
                tuple_ty,
            } => {
                self.hash.tag(9);
                self.operand(value);
                self.symbol(*variant);
                self.hash.usize(*variant_tag);
                self.hash.usize(*index);
                self.type_id(*field_ty);
                if let Some(tuple_ty) = tuple_ty {
                    self.hash.tag(1);
                    self.type_id(*tuple_ty);
                } else {
                    self.hash.tag(0);
                }
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => {
                self.hash.tag(10);
                self.hash.usize(*variant_tag);
                if let Some(payload) = payload {
                    self.hash.tag(1);
                    self.operand(payload);
                } else {
                    self.hash.tag(0);
                }
            }
            AmirRvalue::Len(operand) => {
                self.hash.tag(11);
                self.operand(operand);
            }
            AmirRvalue::SliceView { owner, data, len } => {
                self.hash.tag(12);
                self.operand(owner);
                self.operand(data);
                self.operand(len);
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                self.hash.tag(13);
                self.operand(slice);
                self.operand(start);
                self.operand(len);
            }
            AmirRvalue::SliceData(operand) => {
                self.hash.tag(29);
                self.operand(operand);
            }
            AmirRvalue::StrBytes { source } => {
                self.hash.tag(31);
                self.operand(source);
            }
            AmirRvalue::StrView { owner } => {
                self.hash.tag(14);
                self.operand(owner);
            }
            AmirRvalue::Alloc(operand) => {
                self.hash.tag(15);
                self.operand(operand);
            }
            AmirRvalue::Load(place) => {
                self.hash.tag(16);
                self.place(place);
            }
            AmirRvalue::Borrow(place) => {
                self.hash.tag(17);
                self.place(place);
            }
            AmirRvalue::BorrowMut(place) => {
                self.hash.tag(18);
                self.place(place);
            }
            AmirRvalue::RelativeBorrow { local, mutable } => {
                self.hash.tag(19);
                self.hash.usize(local.as_usize());
                self.hash.bool(*mutable);
            }
            AmirRvalue::CoroutineReady {
                value,
                payload_ty,
                stack,
            } => {
                self.hash.tag(20);
                self.operand(value);
                self.type_id(*payload_ty);
                self.hash.bool(*stack);
            }
            AmirRvalue::GenInsert {
                value, payload_ty, ..
            } => {
                self.hash.tag(21);
                self.operand(value);
                self.type_id(*payload_ty);
            }
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            } => {
                self.hash.tag(22);
                self.operand(gen_ref);
                self.type_id(*payload_ty);
            }
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                self.hash.tag(23);
                self.operand(gen_ref);
                self.operand(value);
                self.type_id(*payload_ty);
            }
            AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                self.hash.tag(24);
                self.operand(gen_ref);
                self.operand(value);
                self.type_id(*payload_ty);
            }
            AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                ..
            } => {
                self.hash.tag(25);
                self.operand(gen_ref);
                self.type_id(*payload_ty);
            }
            AmirRvalue::StringInterp { parts } => {
                self.hash.tag(26);
                self.operands(parts);
            }
            AmirRvalue::ToStr { value, src_ty } => {
                self.hash.tag(27);
                self.operand(value);
                self.type_id(*src_ty);
            }
            AmirRvalue::BlackBox { value, value_ty } => {
                self.hash.tag(28);
                self.operand(value);
                self.type_id(*value_ty);
            }
        }
    }
}
