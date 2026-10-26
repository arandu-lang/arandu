use super::super::symbols::hash_return_borrow_summary;
use super::operand::{hash_amir_operand, hash_operands, hash_place};
use super::rvalue::hash_rvalue;
use super::ty::hash_id;
use blake3::Hasher;

pub(in crate::stable_hash) fn hash_stmt(hasher: &mut Hasher, stmt: &arandu_middle::amir::AmirStmt) {
    use arandu_middle::amir::AmirStmt;
    match stmt {
        AmirStmt::Assign { lhs, rhs } => {
            hasher.update(&[0]);
            hash_id(hasher, lhs.as_usize());
            hash_rvalue(hasher, rhs);
        }
        AmirStmt::Store { lhs, rhs } => {
            hasher.update(&[1]);
            hash_place(hasher, lhs);
            hash_amir_operand(hasher, rhs);
        }
        AmirStmt::Call {
            lhs,
            callee,
            args,
            return_borrow,
        } => {
            hasher.update(&[2]);
            match lhs {
                Some(id) => {
                    hasher.update(&[1]);
                    hash_id(hasher, id.as_usize());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
            hash_amir_operand(hasher, callee);
            hash_operands(hasher, args);
            match return_borrow {
                Some(summary) => {
                    hasher.update(&[1]);
                    hash_return_borrow_summary(hasher, summary);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
        AmirStmt::Free(value) => {
            hasher.update(&[3]);
            hash_amir_operand(hasher, value);
        }
        AmirStmt::StorageLive(local) => {
            hasher.update(&[4]);
            hash_id(hasher, local.as_usize());
        }
        AmirStmt::StorageDead(local) => {
            hasher.update(&[5]);
            hash_id(hasher, local.as_usize());
        }
        AmirStmt::Destroy(place) => {
            hasher.update(&[6]);
            hash_place(hasher, place);
        }
        AmirStmt::Nop => {
            hasher.update(&[7]);
        }
    }
}
