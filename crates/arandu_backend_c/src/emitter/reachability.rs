//! Preserve entrypoints, exported linkage, callbacks and implicit destruction.
use arandu_middle::amir::visit::{for_each_stmt_operand, for_each_terminator_operand};
use arandu_middle::amir::{AmirOperand, AmirProgram};
use arandu_middle::layout::StructLayoutProvider;
use arandu_middle::{SymbolId, SymbolTable, Visibility};
use rustc_hash::{FxHashMap, FxHashSet};

pub(super) fn live_functions(
    program: &AmirProgram,
    symbols: &SymbolTable,
    provider: &dyn StructLayoutProvider,
) -> FxHashSet<SymbolId> {
    let indices: FxHashMap<_, _> = program
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| (f.symbol, i))
        .collect();
    // Inspection of a library can intentionally expose private test entrypoints.
    if !program
        .funcs
        .iter()
        .any(|f| symbols.host_func_name(symbols.get(f.symbol)) == "main")
    {
        return indices.keys().copied().collect();
    }
    let mut pending = Vec::new();
    for func in &program.funcs {
        let symbol = symbols.get(func.symbol);
        if matches!(symbol.visibility, Visibility::Public | Visibility::Internal)
            || symbols.host_func_name(symbol) == "main"
            || symbols.tooling_entrypoints.contains(&func.symbol)
        {
            pending.push(func.symbol);
        }
        for ty in func
            .temps
            .iter()
            .map(|t| t.ty)
            .chain(func.locals.iter().map(|l| l.ty))
            .chain(std::iter::once(func.return_type))
        {
            if let Some(destructor) = provider.destructor_for_type(ty) {
                pending.push(destructor);
            }
        }
    }
    let mut live = FxHashSet::default();
    while let Some(symbol) = pending.pop() {
        if !live.insert(symbol) {
            continue;
        }
        let Some(&index) = indices.get(&symbol) else {
            continue;
        };
        let mut remember = |operand: &AmirOperand| {
            if let AmirOperand::FunctionRef(target) = operand {
                pending.push(*target);
            }
        };
        let function = &program.funcs[index];
        for stmt in function.stmts.payloads.iter() {
            for_each_stmt_operand(stmt, &mut remember);
        }
        for block in &function.blocks {
            for_each_terminator_operand(&block.terminator, &mut remember);
        }
    }
    live
}
