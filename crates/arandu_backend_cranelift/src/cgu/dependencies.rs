//! The declaration closure used by both object emission and CGU hashing.
//! Bodies of callees are deliberately not dependencies of an object caller.

use arandu_middle::amir::{AmirFunc, AmirProgram};
use arandu_middle::type_context::CodegenTypeContext;
use arandu_middle::{Diagnostic, SymbolId, SymbolTable};
use rustc_hash::FxHashSet;

use crate::jit::isa::codegen_ice;

pub(crate) fn function_dependencies(
    function: &AmirFunc,
    program: &AmirProgram,
    symbols: &SymbolTable,
    info: &dyn CodegenTypeContext,
) -> Result<Vec<SymbolId>, Diagnostic> {
    let mut dependencies = arandu_mir::reachability::function_dependencies(
        function,
        program,
        symbols,
        info.type_interner(),
        info,
    )?;
    // Preserve CGU hash ordering by ABI name, then source identity.
    dependencies.sort_by(|a, b| {
        let name = |id| {
            symbols
                .try_get(id)
                .map(|symbol| symbols.host_func_name(symbol))
        };
        name(*a)
            .cmp(&name(*b))
            .then_with(|| (a.file_id, a.local_id.0).cmp(&(b.file_id, b.local_id.0)))
    });
    Ok(dependencies)
}

pub(crate) fn declaration_closure(
    program: &AmirProgram,
    symbols: &SymbolTable,
    info: &dyn CodegenTypeContext,
    definitions: &[SymbolId],
) -> Result<FxHashSet<SymbolId>, Diagnostic> {
    let mut declarations = definitions.iter().copied().collect::<FxHashSet<_>>();
    for symbol in definitions {
        let function = program
            .funcs
            .iter()
            .find(|function| function.symbol == *symbol)
            .ok_or_else(|| codegen_ice("CGU definition has no AMIR body"))?;
        declarations.extend(function_dependencies(function, program, symbols, info)?);
    }
    Ok(declarations)
}

/// Stable local object name: composition-local numeric IDs are not ABI names.
pub(crate) fn drop_shim_name(symbols: &SymbolTable, destructor: SymbolId) -> Option<String> {
    symbols
        .try_get(destructor)
        .map(|symbol| format!("__ar_drop_{}", symbols.host_func_name(symbol)))
}
