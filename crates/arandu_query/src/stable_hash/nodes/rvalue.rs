use super::super::symbols::{hash_str, hash_symbol_id};
use super::operand::{hash_amir_operand, hash_gen_meta, hash_operands, hash_place};
use super::ty::hash_id;
use blake3::Hasher;

pub(in crate::stable_hash) fn hash_rvalue(
    hasher: &mut Hasher,
    value: &arandu_middle::amir::AmirRvalue,
) {
    use arandu_middle::amir::AmirRvalue as R;
    match value {
        R::VectorSelect {
            mask,
            if_true,
            if_false,
        } => {
            hasher.update(&[36]);
            hash_amir_operand(hasher, mask);
            hash_amir_operand(hasher, if_true);
            hash_amir_operand(hasher, if_false);
        }
        R::VectorCompare { op, left, right } => {
            hasher.update(&[37, op.stable_tag()]);
            hash_amir_operand(hasher, left);
            hash_amir_operand(hasher, right);
        }
        R::VectorOp { op, left, right } => {
            hasher.update(&[31, op.stable_tag()]);
            hash_amir_operand(hasher, left);
            hash_amir_operand(hasher, right);
        }
        R::VectorSplat { value } => {
            hasher.update(&[32]);
            hash_amir_operand(hasher, value);
        }
        R::VectorShuffle { left, right, mask } => {
            hasher.update(&[33]);
            hash_amir_operand(hasher, left);
            hash_amir_operand(hasher, right);
            hasher.update(mask);
        }
        R::VectorExtractLane { vector, lane } => {
            hasher.update(&[34, *lane]);
            hash_amir_operand(hasher, vector);
        }
        R::VectorInsertLane {
            vector,
            lane,
            value,
        } => {
            hasher.update(&[35, *lane]);
            hash_amir_operand(hasher, vector);
            hash_amir_operand(hasher, value);
        }
        R::Use(op) => {
            hasher.update(&[0]);
            hash_amir_operand(hasher, op);
        }
        R::Binary { op, left, right } => {
            hasher.update(&[1, op.stable_tag()]);
            hash_amir_operand(hasher, left);
            hash_amir_operand(hasher, right);
        }
        R::Unary { op, operand } => {
            hasher.update(&[2, op.stable_tag()]);
            hash_amir_operand(hasher, operand);
        }
        R::FieldAccess { base, field } => {
            hasher.update(&[3]);
            hash_amir_operand(hasher, base);
            hash_id(hasher, *field);
        }
        R::StructLiteral {
            struct_symbol,
            fields,
        } => {
            hasher.update(&[4]);
            hash_symbol_id(hasher, *struct_symbol);
            hash_id(hasher, fields.len());
            for (name, operand) in fields {
                hash_str(hasher, name);
                hash_amir_operand(hasher, operand);
            }
        }
        R::IndexAccess { base, index } => {
            hasher.update(&[5]);
            hash_amir_operand(hasher, base);
            hash_amir_operand(hasher, index);
        }
        R::Array { items } => {
            hasher.update(&[6]);
            hash_operands(hasher, items);
        }
        R::Tuple { items } => {
            hasher.update(&[7]);
            hash_operands(hasher, items);
        }
        R::Discriminant { value } => {
            hasher.update(&[8]);
            hash_amir_operand(hasher, value);
        }
        R::EnumPayload {
            value,
            variant,
            index,
            variant_tag,
            field_ty,
            tuple_ty,
        } => {
            hasher.update(&[9]);
            hash_amir_operand(hasher, value);
            hash_symbol_id(hasher, *variant);
            hash_id(hasher, *variant_tag);
            hash_id(hasher, *index);
            hash_id(hasher, field_ty.as_usize());
            match tuple_ty {
                Some(tuple_ty) => {
                    hasher.update(&[1]);
                    hash_id(hasher, tuple_ty.as_usize());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
        R::EnumConstruct {
            variant_tag,
            payload,
        } => {
            hasher.update(&[10]);
            hash_id(hasher, *variant_tag);
            match payload {
                Some(op) => {
                    hasher.update(&[1]);
                    hash_amir_operand(hasher, op);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
        R::Len(op) => {
            hasher.update(&[11]);
            hash_amir_operand(hasher, op);
        }
        R::SliceData(op) => {
            hasher.update(&[12]);
            hash_amir_operand(hasher, op);
        }
        R::SliceView { owner, data, len } => {
            hasher.update(&[13]);
            hash_amir_operand(hasher, owner);
            hash_amir_operand(hasher, data);
            hash_amir_operand(hasher, len);
        }
        R::SliceSubslice { slice, start, len } => {
            hasher.update(&[14]);
            hash_amir_operand(hasher, slice);
            hash_amir_operand(hasher, start);
            hash_amir_operand(hasher, len);
        }
        R::StrBytes { source } => {
            hasher.update(&[15]);
            hash_amir_operand(hasher, source);
        }
        R::StrView { owner } => {
            hasher.update(&[16]);
            hash_amir_operand(hasher, owner);
        }
        R::Alloc(op) => {
            hasher.update(&[17]);
            hash_amir_operand(hasher, op);
        }
        R::Load(place) => {
            hasher.update(&[18]);
            hash_place(hasher, place);
        }
        R::Borrow(place) => {
            hasher.update(&[19]);
            hash_place(hasher, place);
        }
        R::BorrowMut(place) => {
            hasher.update(&[20]);
            hash_place(hasher, place);
        }
        R::RelativeBorrow { local, mutable } => {
            hasher.update(&[21, u8::from(*mutable)]);
            hash_id(hasher, local.as_usize());
        }
        R::CoroutineReady {
            value,
            payload_ty,
            stack,
        } => {
            hasher.update(&[22, u8::from(*stack)]);
            hash_amir_operand(hasher, value);
            hash_id(hasher, payload_ty.as_usize());
        }
        R::GenInsert {
            value,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[23]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, value);
        }
        R::GenGet {
            gen_ref,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[24]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
        }
        R::GenSet {
            gen_ref,
            value,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[25]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
            hash_amir_operand(hasher, value);
        }
        R::GenUpsert {
            gen_ref,
            value,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[26]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
            hash_amir_operand(hasher, value);
        }
        R::GenRemove {
            gen_ref,
            payload_ty,
            arena,
            origin,
        } => {
            hasher.update(&[27]);
            hash_gen_meta(hasher, *payload_ty, *arena, *origin);
            hash_amir_operand(hasher, gen_ref);
        }
        R::StringInterp { parts } => {
            hasher.update(&[28]);
            hash_operands(hasher, parts);
        }
        R::ToStr { value, src_ty } => {
            hasher.update(&[29]);
            hash_amir_operand(hasher, value);
            hash_id(hasher, src_ty.as_usize());
        }
        R::BlackBox { value, value_ty } => {
            hasher.update(&[30]);
            hash_amir_operand(hasher, value);
            hash_id(hasher, value_ty.as_usize());
        }
    }
}
