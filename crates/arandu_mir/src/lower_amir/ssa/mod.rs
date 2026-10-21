mod phi;
mod read;
mod rename;

#[cfg(test)]
use super::LowerCtx;
#[cfg(test)]
use crate::amir::{AmirOperand, AmirRvalue, TempId};
#[cfg(test)]
use rustc_hash::FxHashMap;

#[cfg(test)]
mod vector_tests {
    use super::*;

    #[test]
    fn redirected_vector_operands_preserve_move_kind_and_follow_phi_chains() {
        let redirected = FxHashMap::from_iter([
            (TempId(0), AmirOperand::Copy(TempId(2))),
            (TempId(1), AmirOperand::Copy(TempId(3))),
            (TempId(2), AmirOperand::Copy(TempId(10))),
            (TempId(3), AmirOperand::Copy(TempId(11))),
        ]);
        let mut value = AmirRvalue::VectorInsertLane {
            vector: AmirOperand::Copy(TempId(0)),
            lane: 2,
            value: AmirOperand::Move(TempId(1)),
        };
        LowerCtx::resolve_rvalue(&redirected, &mut value);
        let AmirRvalue::VectorInsertLane {
            vector,
            lane,
            value,
        } = value
        else {
            unreachable!("SSA substitution must retain the rvalue variant");
        };
        assert_eq!(vector, AmirOperand::Copy(TempId(10)));
        assert_eq!(value, AmirOperand::Move(TempId(11)));
        assert_eq!(lane, 2);
    }
}
