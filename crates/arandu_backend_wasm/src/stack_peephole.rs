//! Preserve operand-stack flow across bounded, independent scalar expressions.
use wasm_encoder::Instruction;

// Bound scanning independently of function size. Calls, control, memory and
// potentially trapping arithmetic remain barriers; no expression is reordered.
const STACK_WINDOW: usize = 32;

fn independent_stack_effect(instruction: &Instruction<'_>) -> Option<(usize, usize)> {
    use Instruction::*;
    match instruction {
        I32Const(_) | I64Const(_) | F32Const(_) | F64Const(_) | LocalGet(_) => Some((0, 1)),
        LocalSet(_) | Drop => Some((1, 0)),
        LocalTee(_) | I32Eqz | I64Eqz | I32Clz | I32Ctz | I32Popcnt | I64Clz | I64Ctz
        | I64Popcnt | I32WrapI64 | I64ExtendI32S | I64ExtendI32U | I32Extend8S | I32Extend16S
        | I64Extend8S | I64Extend16S | I64Extend32S => Some((1, 1)),
        I32Add | I32Sub | I32Mul | I32And | I32Or | I32Xor | I32Shl | I32ShrS | I32ShrU
        | I32Rotl | I32Rotr | I32Eq | I32Ne | I32LtS | I32LtU | I32GtS | I32GtU | I32LeS
        | I32LeU | I32GeS | I32GeU | I64Add | I64Sub | I64Mul | I64And | I64Or | I64Xor
        | I64Shl | I64ShrS | I64ShrU | I64Rotl | I64Rotr | I64Eq | I64Ne | I64LtS | I64LtU
        | I64GtS | I64GtU | I64LeS | I64LeU | I64GeS | I64GeU => Some((2, 1)),
        // New instructions are barriers until their stack/effect contract is
        // explicitly admitted here. This is an optimization, not an IR visitor.
        _ => None,
    }
}

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
    let mut removed = vec![false; code.len()];
    for index in 0..code.len() {
        if removed[index] {
            continue;
        }
        let Instruction::LocalSet(local) = code[index] else {
            continue;
        };
        let mut height = 0;
        let end = code.len().min(index.saturating_add(STACK_WINDOW + 1));
        for next in index + 1..end {
            if removed[next] {
                continue;
            }
            if matches!(code[next], Instruction::LocalGet(slot) if slot == local) {
                // The saved value may remain below independent expressions only
                // when those expressions leave no outstanding operands. Keeping
                // it when height > 0 would reverse the consumer's operand order.
                if height == 0 {
                    removed[next] = true;
                    if accesses.get(&local) == Some(&(1, 1)) {
                        removed[index] = true;
                    } else {
                        code[index] = Instruction::LocalTee(local);
                    }
                }
                break;
            }
            if matches!(code[next], Instruction::LocalSet(slot) | Instruction::LocalTee(slot) if slot == local)
            {
                break;
            }
            let Some((pops, pushes)) = independent_stack_effect(&code[next]) else {
                break;
            };
            // Never let an intervening instruction consume the saved value or
            // any operand belonging to the enclosing expression.
            if pops > height {
                break;
            }
            height = height - pops + pushes;
        }
    }
    let mut index = 0;
    code.retain(|_| {
        let keep = !removed[index];
        index += 1;
        keep
    });
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

    #[test]
    fn retains_value_across_balanced_independent_expression() {
        let mut code = vec![
            Instruction::I32Const(7),
            Instruction::LocalSet(0),
            Instruction::I32Const(2),
            Instruction::I32Const(3),
            Instruction::I32Mul,
            Instruction::LocalSet(1),
            Instruction::LocalGet(0),
            Instruction::LocalGet(1),
            Instruction::I32Sub,
        ];
        stackify_locals(&mut code);
        assert!(
            !code
                .iter()
                .any(|op| matches!(op, Instruction::LocalSet(0) | Instruction::LocalGet(0)))
        );
        assert!(matches!(code.last(), Some(Instruction::I32Sub)));
    }

    #[test]
    fn outstanding_operands_overwrites_and_effects_are_barriers() {
        for middle in [
            vec![Instruction::I32Const(2)],
            vec![Instruction::Call(0)],
            vec![Instruction::GlobalGet(0), Instruction::Drop],
            vec![
                Instruction::I32Const(2),
                Instruction::I32Const(0),
                Instruction::I32DivS,
                Instruction::Drop,
            ],
            vec![Instruction::I32Add, Instruction::Drop],
        ] {
            let mut code = vec![Instruction::LocalSet(0)];
            code.extend(middle);
            code.push(Instruction::LocalGet(0));
            stackify_locals(&mut code);
            assert!(matches!(code.first(), Some(Instruction::LocalSet(0))));
            assert!(matches!(code.last(), Some(Instruction::LocalGet(0))));
        }
        let mut overwrite = vec![
            Instruction::LocalSet(0),
            Instruction::I32Const(2),
            Instruction::LocalSet(0),
            Instruction::LocalGet(0),
        ];
        stackify_locals(&mut overwrite);
        assert!(matches!(overwrite.first(), Some(Instruction::LocalSet(0))));
    }

    #[test]
    fn scanning_window_is_bounded_even_for_balanced_expressions() {
        let mut code = vec![Instruction::I32Const(7), Instruction::LocalSet(0)];
        for _ in 0..STACK_WINDOW {
            code.extend([Instruction::I32Const(1), Instruction::Drop]);
        }
        code.push(Instruction::LocalGet(0));
        let before = code.len();
        stackify_locals(&mut code);
        assert_eq!(code.len(), before);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn execution_preserves_operand_order_and_reused_local_values() {
        use wasm_encoder::{
            CodeSection, ExportKind, ExportSection, Function, FunctionSection, Module, TypeSection,
            ValType,
        };
        let engine = wasmtime::Engine::default();
        let compile = |instructions: &[Instruction<'static>]| {
            let mut module = Module::new();
            let mut types = TypeSection::new();
            types
                .ty()
                .function([ValType::I32, ValType::I32], [ValType::I32]);
            module.section(&types);
            let mut functions = FunctionSection::new();
            functions.function(0);
            module.section(&functions);
            let mut exports = ExportSection::new();
            exports.export("run", ExportKind::Func, 0);
            module.section(&exports);
            let mut body = Function::new([(3, ValType::I32)]);
            for instruction in instructions {
                body.instruction(instruction);
            }
            body.instruction(&Instruction::End);
            let mut code = CodeSection::new();
            code.function(&body);
            module.section(&code);
            wasmtime::Module::new(&engine, module.finish()).unwrap()
        };
        let programs = [
            vec![
                Instruction::LocalGet(0),
                Instruction::LocalSet(2),
                Instruction::LocalGet(1),
                Instruction::I32Const(3),
                Instruction::I32Mul,
                Instruction::LocalSet(3),
                Instruction::LocalGet(2),
                Instruction::LocalGet(3),
                Instruction::I32Sub,
                Instruction::LocalGet(2),
                Instruction::I32Add,
            ],
            vec![
                Instruction::LocalGet(0),
                Instruction::LocalSet(2),
                Instruction::LocalGet(1),
                Instruction::LocalGet(2),
                Instruction::I32Sub,
            ],
            vec![
                Instruction::LocalGet(0),
                Instruction::LocalSet(2),
                Instruction::LocalGet(1),
                Instruction::LocalSet(2),
                Instruction::LocalGet(2),
            ],
            vec![
                Instruction::LocalGet(0),
                Instruction::LocalSet(2),
                Instruction::LocalGet(1),
                Instruction::If(wasm_encoder::BlockType::Empty),
                Instruction::I32Const(7),
                Instruction::LocalSet(2),
                Instruction::End,
                Instruction::LocalGet(2),
            ],
        ];
        for original in programs {
            let mut optimized = original.clone();
            stackify_locals(&mut optimized);
            let mut store = wasmtime::Store::new(&engine, ());
            let before = wasmtime::Instance::new(&mut store, &compile(&original), &[])
                .unwrap()
                .get_typed_func::<(i32, i32), i32>(&mut store, "run")
                .unwrap();
            let after = wasmtime::Instance::new(&mut store, &compile(&optimized), &[])
                .unwrap()
                .get_typed_func::<(i32, i32), i32>(&mut store, "run")
                .unwrap();
            for x in [i32::MIN, -1, 0, 1, i32::MAX] {
                for y in [i32::MIN, -1, 0, 1, i32::MAX] {
                    assert_eq!(
                        before.call(&mut store, (x, y)).unwrap(),
                        after.call(&mut store, (x, y)).unwrap()
                    );
                }
            }
        }
    }
}
