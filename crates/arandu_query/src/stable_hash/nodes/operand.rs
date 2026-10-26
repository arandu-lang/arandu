use super::super::symbols::{hash_symbol_id, u32_le, u64_le};
use super::ty::{hash_id, hash_span};
use blake3::Hasher;

pub(in crate::stable_hash) fn hash_operands(
    hasher: &mut Hasher,
    operands: &[arandu_middle::amir::AmirOperand],
) {
    hash_id(hasher, operands.len());
    for operand in operands {
        hash_amir_operand(hasher, operand);
    }
}

pub(in crate::stable_hash) fn hash_place(
    hasher: &mut Hasher,
    place: &arandu_middle::amir::AmirPlace,
) {
    use arandu_middle::amir::AmirProjection;
    hash_id(hasher, place.local.as_usize());
    hash_id(hasher, place.projections.len());
    for projection in &place.projections {
        match projection {
            AmirProjection::Field(symbol) => {
                hasher.update(&[0]);
                hash_symbol_id(hasher, *symbol);
            }
            AmirProjection::Index(operand) => {
                hasher.update(&[1]);
                hash_amir_operand(hasher, operand);
            }
            AmirProjection::IndexConstant(index) => {
                hasher.update(&[6]);
                hash_id(hasher, *index);
            }
            AmirProjection::Deref => {
                hasher.update(&[2]);
            }
            AmirProjection::Variant(tag) => {
                hasher.update(&[3]);
                hash_id(hasher, *tag);
            }
            AmirProjection::Payload {
                variant_tag,
                index,
                field_ty,
                tuple_ty,
            } => {
                hasher.update(&[4]);
                hash_id(hasher, *variant_tag);
                hash_id(hasher, *index);
                hash_id(hasher, field_ty.as_usize());
                match tuple_ty {
                    Some(ty) => {
                        hasher.update(&[1]);
                        hash_id(hasher, ty.as_usize());
                    }
                    None => {
                        hasher.update(&[0]);
                    }
                }
            }
            AmirProjection::TupleField(index) => {
                hasher.update(&[5]);
                hash_id(hasher, *index);
            }
        }
    }
}

pub(in crate::stable_hash) fn hash_gen_meta(
    hasher: &mut Hasher,
    ty: arandu_middle::types::TypeId,
    arena: arandu_middle::amir::GenArenaDomain,
    origin: arandu_lexer::Span,
) {
    hasher.update(&[match arena {
        arandu_middle::amir::GenArenaDomain::CompilerManaged => 0,
    }]);
    hash_id(hasher, ty.as_usize());
    hash_span(hasher, origin);
}

pub(in crate::stable_hash) fn hash_amir_operand(
    hasher: &mut Hasher,
    operand: &arandu_middle::amir::AmirOperand,
) {
    use arandu_middle::amir::{AmirConstant, AmirOperand};

    match operand {
        AmirOperand::Copy(temp) => {
            hasher.update(&[0]);
            hasher.update(&u64_le(temp.as_usize() as u64));
        }
        AmirOperand::Move(temp) => {
            hasher.update(&[1]);
            hasher.update(&u64_le(temp.as_usize() as u64));
        }
        AmirOperand::Constant(AmirConstant::Pool(id)) => {
            hasher.update(&[2]);
            hasher.update(&u32_le(id.0));
        }
        AmirOperand::Constant(AmirConstant::Bool(value)) => {
            hasher.update(&[3, u8::from(*value)]);
        }
        AmirOperand::Constant(AmirConstant::Nil) => {
            hasher.update(&[4]);
        }
        AmirOperand::FunctionRef(symbol) => {
            hasher.update(&[5]);
            hash_symbol_id(hasher, *symbol);
        }
        AmirOperand::GlobalRef(symbol) => {
            hasher.update(&[6]);
            hash_symbol_id(hasher, *symbol);
        }
    }
}
