//! Backend-independent function closure, including address-taken callbacks and drops.

use crate::amir::visit::{for_each_stmt_operand, for_each_terminator_operand};
use crate::amir::{AmirFunc, AmirOperand, AmirProgram, AmirRvalue, AmirStmt};
use crate::layout::{
    StructLayoutProvider, instantiated_enum_variant_payload_type, instantiated_field_type,
};
use crate::types::{ArType, TypeInterner, TypeShape};
use crate::{DiagCode, Diagnostic, Span, SymbolId, SymbolKind, SymbolTable};
use arandu_middle::Visibility;
use rustc_hash::{FxHashMap, FxHashSet};

/// Retain executable roots and their dependencies. Without `main`, retain all
/// definitions: library inspection and host lookup can intentionally use private
/// functions. The result also contains referenced external symbols.
pub fn live_functions(
    program: &AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
) -> FxHashSet<SymbolId> {
    let indices: FxHashMap<_, _> = program
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| (f.symbol, i))
        .collect();
    let library = !program.funcs.iter().any(|f| {
        symbols
            .try_get(f.symbol)
            .is_some_and(|s| symbols.host_func_name(s) == "main")
    });
    let mut pending: Vec<_> = program
        .funcs
        .iter()
        .filter_map(|f| {
            let symbol = symbols.try_get(f.symbol)?;
            (library
                || matches!(symbol.visibility, Visibility::Public | Visibility::Internal)
                || symbols.host_func_name(symbol) == "main"
                || symbols.tooling_entrypoints.contains(&f.symbol))
            .then_some(f.symbol)
        })
        .collect();
    let mut live = FxHashSet::default();
    while let Some(symbol) = pending.pop() {
        if !live.insert(symbol) {
            continue;
        }
        let Some(&index) = indices.get(&symbol) else {
            continue;
        };
        match function_dependencies(&program.funcs[index], program, symbols, interner, provider) {
            Ok(dependencies) => pending.extend(dependencies),
            // Pruning is optional: incomplete metadata must never discard a
            // definition or import needed by backend-generated cleanup.
            Err(_) => {
                live.extend(program.funcs.iter().map(|function| function.symbol));
                live.extend(program.extern_funcs.keys().copied());
                return live;
            }
        }
    }
    live
}

/// Dependencies of one body, including implicit cleanup and namespace aliases.
/// The provider is the canonical source of concrete struct/enum metadata; this
/// analysis does not depend on the type checker or infer destructors by name.
/// Returned identities are sorted independently of hash-map iteration order.
pub fn function_dependencies(
    function: &AmirFunc,
    program: &AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
) -> Result<Vec<SymbolId>, Diagnostic> {
    let invalid_dependencies = || {
        Diagnostic::ice(
            DiagCode::ICEGEN001,
            "function type dependencies exceed their structural bounds or have inconsistent metadata",
            Span::new(0, 0, 0),
        )
    };
    let mut referenced = FxHashSet::default();
    let mut visit = |operand: &AmirOperand| {
        if let AmirOperand::FunctionRef(symbol) = operand {
            referenced.insert(*symbol);
        }
    };
    let mut types = vec![function.return_type];
    types.extend(function.temps.iter().map(|temp| temp.ty));
    types.extend(function.locals.iter().map(|local| local.ty));
    types.extend(function.block_params.iter().map(|param| param.ty));
    // Backend-generated Gen drop shims inspect the statement table, including
    // entries not assigned to a block; retain the same declaration contract.
    for id in function.stmts.iter_ids() {
        if let Some(statement) = function.stmts.get(id) {
            for_each_stmt_operand(statement, &mut visit);
            if let AmirStmt::Assign { rhs, .. } = statement {
                // Drop shims use semantic payload metadata even when no result
                // temporary has this type (the result can be a GenRef handle).
                match rhs {
                    AmirRvalue::GenInsert { payload_ty, .. }
                    | AmirRvalue::GenSet { payload_ty, .. }
                    | AmirRvalue::GenUpsert { payload_ty, .. } => types.push(*payload_ty),
                    AmirRvalue::Use(_)
                    | AmirRvalue::Binary { .. }
                    | AmirRvalue::Unary { .. }
                    | AmirRvalue::FieldAccess { .. }
                    | AmirRvalue::IndexAccess { .. }
                    | AmirRvalue::StructLiteral { .. }
                    | AmirRvalue::Array { .. }
                    | AmirRvalue::Tuple { .. }
                    | AmirRvalue::EnumConstruct { .. }
                    | AmirRvalue::Discriminant { .. }
                    | AmirRvalue::EnumPayload { .. }
                    | AmirRvalue::Len(_)
                    | AmirRvalue::SliceData(_)
                    | AmirRvalue::SliceView { .. }
                    | AmirRvalue::SliceSubslice { .. }
                    | AmirRvalue::StrBytes { .. }
                    | AmirRvalue::StrView { .. }
                    | AmirRvalue::Alloc(_)
                    | AmirRvalue::Load(_)
                    | AmirRvalue::Borrow(_)
                    | AmirRvalue::BorrowMut(_)
                    | AmirRvalue::RelativeBorrow { .. }
                    | AmirRvalue::CoroutineReady { .. }
                    | AmirRvalue::GenGet { .. }
                    | AmirRvalue::GenRemove { .. }
                    | AmirRvalue::StringInterp { .. }
                    | AmirRvalue::ToStr { .. }
                    | AmirRvalue::BlackBox { .. } => {}
                }
            }
        }
    }
    for block in &function.blocks {
        for_each_terminator_operand(&block.terminator, &mut visit);
    }
    let mut visited = FxHashSet::default();
    while let Some(ty) = types.pop() {
        if !visited.insert(ty) {
            continue;
        }
        if visited.len() > 4096 || TypeShape::from_id(ty, interner).is_err() {
            return Err(invalid_dependencies());
        }
        if let Some(destructor) = provider.destructor_for_type(ty) {
            referenced.insert(destructor);
        }
        match interner.resolve(ty) {
            nominal @ ArType::Named(_, arguments) => {
                types.extend(interner.type_args(arguments));
                if let Some(fields) = provider.get_struct_fields_for_type(&nominal, interner) {
                    for field in fields.iter() {
                        types.push(
                            instantiated_field_type(&nominal, &field.name, interner, provider)
                                .ok_or_else(invalid_dependencies)?,
                        );
                    }
                }
                if let Some(variants) = provider.get_enum_variants_for_type(&nominal, interner) {
                    for (tag, variant) in variants.iter().enumerate() {
                        if variant.payload_ty.is_some() {
                            types.push(
                                instantiated_enum_variant_payload_type(
                                    &nominal, tag, interner, provider,
                                )
                                .ok_or_else(invalid_dependencies)?,
                            );
                        }
                    }
                }
            }
            ArType::Func(arguments, result) => {
                types.extend(interner.type_args(arguments));
                types.push(result);
            }
            ArType::Tuple(arguments) => types.extend(interner.type_args(arguments)),
            ArType::Nullable(inner)
            | ArType::Slice(inner)
            | ArType::Array(_, inner)
            | ArType::ConstArray(_, inner)
            | ArType::Ptr(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner)
            | ArType::Option(inner)
            | ArType::Coroutine(inner)
            | ArType::Poll(inner)
            | ArType::Range(inner) => types.push(inner),
            ArType::Result(ok, error) => types.extend([ok, error]),
            ArType::Primitive(_)
            | ArType::FrozenConst(_)
            | ArType::Const(_)
            | ArType::ConstParam(_)
            | ArType::GenRef
            | ArType::Err
            | ArType::Void
            | ArType::IntLiteral
            | ArType::FloatLiteral
            | ArType::Error => {}
        }
    }
    // Resolve the same alias identities accepted by native call lowering.
    let aliases = referenced
        .iter()
        .filter_map(|symbol| symbols.try_get(*symbol))
        .filter(|symbol| symbol.kind == SymbolKind::NamespaceMember)
        .collect::<Vec<_>>();
    if !aliases.is_empty() {
        for symbol in program
            .funcs
            .iter()
            .map(|function| function.symbol)
            .chain(program.extern_funcs.keys().copied())
        {
            if let Some(declaration) = symbols.try_get(symbol)
                && aliases.iter().any(|alias| {
                    alias.span == declaration.span
                        && alias.name.len() > declaration.name.len()
                        && alias.name.ends_with(&*declaration.name)
                        && alias.name.as_bytes()[alias.name.len() - declaration.name.len() - 1]
                            == b'.'
                })
            {
                referenced.insert(symbol);
            }
        }
    }
    let mut dependencies = referenced.into_iter().collect::<Vec<_>>();
    dependencies.sort_by_key(|symbol| (symbol.file_id, symbol.local_id.0));
    Ok(dependencies)
}

#[cfg(test)]
mod tests;
