//! Preserve operand-stack flow across immediately adjacent local stores/loads.
use wasm_encoder::Instruction;

pub(crate) fn stackify_locals(code: &mut Vec<Instruction<'static>>) {
    let mut accesses = std::collections::BTreeMap::<u32, (usize, usize)>::new();
    for instruction in code.iter() {
        match instruction {
            Instruction::LocalGet(local) => accesses.entry(*local).or_default().0 += 1,
            Instruction::LocalSet(local) | Instruction::LocalTee(local) => {
                accesses.entry(*local).or_default().1 += 1;
            }
            _ => {}
        }
    }
    let original = std::mem::take(code);
    let mut instructions = original.into_iter().peekable();
    while let Some(instruction) = instructions.next() {
        if let Instruction::LocalSet(local) = instruction
            && matches!(instructions.peek(), Some(Instruction::LocalGet(next)) if *next == local)
        {
            instructions.next();
            // A single store/read pair has no observable local state. Other
            // locals retain their write via tee, including loop-carried values.
            if accesses.get(&local) != Some(&(1, 1)) {
                code.push(Instruction::LocalTee(local));
            }
        } else {
            code.push(instruction);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_use_flows_on_stack_and_shared_values_keep_their_store() {
        let mut code = vec![
            Instruction::I32Const(7),
            Instruction::LocalSet(0),
            Instruction::LocalGet(0),
            Instruction::LocalSet(1),
            Instruction::LocalGet(1),
            Instruction::Drop,
            Instruction::LocalGet(1),
        ];
        stackify_locals(&mut code);
        assert!(matches!(
            code.as_slice(),
            [
                Instruction::I32Const(7),
                Instruction::LocalTee(1),
                Instruction::Drop,
                Instruction::LocalGet(1)
            ]
        ));
        let before = code.len();
        stackify_locals(&mut code);
        assert_eq!(code.len(), before);
    }
    #[test]
    fn never_crosses_control_or_other_instructions() {
        let mut code = vec![
            Instruction::LocalSet(0),
            Instruction::Block(wasm_encoder::BlockType::Empty),
            Instruction::LocalGet(0),
            Instruction::End,
        ];
        stackify_locals(&mut code);
        assert_eq!(code.len(), 4);
    }
}
