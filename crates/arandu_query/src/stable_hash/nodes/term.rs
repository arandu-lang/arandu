use super::operand::{hash_amir_operand, hash_operands};
use super::ty::hash_id;
use blake3::Hasher;

pub(in crate::stable_hash) fn hash_terminator(
    hasher: &mut Hasher,
    term: &arandu_middle::amir::AmirTerminator,
) {
    use arandu_middle::amir::AmirTerminator;
    match term {
        AmirTerminator::Return => {
            hasher.update(&[0]);
        }
        AmirTerminator::Unreachable => {
            hasher.update(&[1]);
        }
        AmirTerminator::Goto { target, args } => {
            hasher.update(&[2]);
            hash_id(hasher, target.as_usize());
            hash_operands(hasher, args);
        }
        AmirTerminator::Branch {
            condition,
            if_true,
            true_args,
            if_false,
            false_args,
        } => {
            hasher.update(&[3]);
            hash_amir_operand(hasher, condition);
            hash_id(hasher, if_true.as_usize());
            hash_operands(hasher, true_args);
            hash_id(hasher, if_false.as_usize());
            hash_operands(hasher, false_args);
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
        } => {
            hasher.update(&[4]);
            hash_amir_operand(hasher, discriminant);
            hash_id(hasher, targets.len());
            for (value, target, args) in targets {
                hasher.update(&value.to_le_bytes());
                hash_id(hasher, target.as_usize());
                hash_operands(hasher, args);
            }
            hash_id(hasher, otherwise.0.as_usize());
            hash_operands(hasher, &otherwise.1);
        }
        AmirTerminator::Suspend {
            future,
            resume,
            args,
        } => {
            hasher.update(&[5]);
            hash_amir_operand(hasher, future);
            hash_id(hasher, resume.as_usize());
            hash_operands(hasher, args);
        }
    }
}
