//! Runtime function declaration and external import mapping for Cranelift modules.

use arandu_middle::Diagnostic;
use cranelift_codegen::ir::types::I64;
use cranelift_codegen::ir::{Signature, Type};
use cranelift_codegen::isa::CallConv;
use cranelift_module::{FuncId, Linkage, Module};
use rustc_hash::FxHashMap;

use super::isa::codegen_ice;

mod bench_gen;
mod memory;
mod os_path;
mod reactor_socket;
mod task;
mod vec;

#[inline]
fn insert_sym(func_ids: &mut FxHashMap<String, usize>, name: &str, id: usize) {
    func_ids.insert(name.to_string(), id);
}

/// Target signatures without module declarations. Object emission resolves a
/// helper only when lowering emits a call or address, including implicit helpers.
#[derive(Default)]
pub(crate) struct RuntimeImports {
    declarations: Vec<(String, Signature)>,
    aliases: FxHashMap<String, usize>,
}

impl RuntimeImports {
    fn declare_function(
        &mut self,
        name: &str,
        linkage: Linkage,
        signature: &Signature,
    ) -> Result<usize, &'static str> {
        if linkage != Linkage::Import {
            return Err("runtime catalog only accepts imports");
        }
        if let Some(index) = self
            .declarations
            .iter()
            .position(|(entry, _)| entry == name)
        {
            if self.declarations[index].1 != *signature {
                return Err("conflicting runtime import signature");
            }
            return Ok(index);
        }
        let index = self.declarations.len();
        self.declarations.push((name.to_owned(), signature.clone()));
        Ok(index)
    }

    pub(crate) fn insert(&mut self, alias: &str, name: &str, signature: Signature) {
        let index = self.declarations.len();
        self.declarations.push((name.to_owned(), signature));
        self.aliases.insert(alias.to_owned(), index);
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.aliases.contains_key(name) || self.declarations.iter().any(|(entry, _)| entry == name)
    }

    pub(crate) fn alias(&mut self, alias: &str, existing: &str) -> bool {
        let index = self.aliases.get(existing).copied().or_else(|| {
            self.declarations
                .iter()
                .position(|(name, _)| name == existing)
        });
        if let Some(index) = index {
            self.aliases.insert(alias.to_owned(), index);
            true
        } else {
            false
        }
    }

    pub(crate) fn resolve<M: Module>(
        &self,
        module: &mut M,
        alias: &str,
    ) -> Result<Option<FuncId>, Diagnostic> {
        let Some(&index) = self.aliases.get(alias) else {
            return Ok(None);
        };
        let (name, signature) = &self.declarations[index];
        module
            .declare_function(name, Linkage::Import, signature)
            .map(Some)
            .map_err(|error| codegen_ice(format!("declare runtime import {name}: {error:?}")))
    }

    /// The interactive JIT keeps its existing eager symbol lookup contract.
    pub(crate) fn declare_all<M: Module>(
        &self,
        module: &mut M,
        func_ids: &mut FxHashMap<String, FuncId>,
    ) -> Result<(), Diagnostic> {
        let mut ids = Vec::with_capacity(self.declarations.len());
        for (name, signature) in &self.declarations {
            ids.push(
                module
                    .declare_function(name, Linkage::Import, signature)
                    .map_err(|error| {
                        codegen_ice(format!("declare runtime import {name}: {error:?}"))
                    })?,
            );
        }
        for (alias, &index) in &self.aliases {
            func_ids.entry(alias.clone()).or_insert(ids[index]);
        }
        Ok(())
    }
}

/// Main's cleanup policy is a property of the complete reachable program,
/// including public/tooling roots, callback addresses and implicit destructors.
/// Unknown foreign code or indirect calls conservatively retain cleanup.
pub(crate) fn gen_shutdown_required(
    program: &arandu_middle::amir::AmirProgram,
    symbols: &arandu_middle::SymbolTable,
    type_info: &dyn arandu_middle::type_context::CodegenTypeContext,
) -> bool {
    use arandu_middle::amir::{AmirOperand, AmirRvalue, AmirStmt};

    let live = arandu_mir::reachability::live_functions(
        program,
        symbols,
        type_info.type_interner(),
        type_info,
    );
    // Only names are inspected here; signatures used in lowering are always
    // rebuilt with the actual target ABI and pointer width.
    let Ok(catalog) = runtime_imports(CallConv::SystemV, I64) else {
        return true;
    };
    for &symbol in program
        .extern_funcs
        .keys()
        .filter(|symbol| live.contains(symbol))
    {
        let Some(symbol) = symbols.try_get(symbol) else {
            return true;
        };
        let name = symbol.name.rsplit('.').next().unwrap_or(&symbol.name);
        if name.starts_with("ar_gen_") || !catalog.contains(name) {
            return true;
        }
    }
    program
        .funcs
        .iter()
        .filter(|func| live.contains(&func.symbol))
        .any(|func| {
            if func.temps.iter().any(|temp| {
                type_info.resolve_type_id(temp.ty) == arandu_middle::types::ArType::GenRef
            }) {
                return true;
            }
            func.stmts.payloads.iter().any(|stmt| match stmt {
                AmirStmt::Assign { rhs, .. } => matches!(
                    rhs,
                    AmirRvalue::GenInsert { .. }
                        | AmirRvalue::GenGet { .. }
                        | AmirRvalue::GenSet { .. }
                        | AmirRvalue::GenUpsert { .. }
                        | AmirRvalue::GenRemove { .. }
                ),
                AmirStmt::Call { callee, .. } => !matches!(callee, AmirOperand::FunctionRef(_)),
                AmirStmt::Store { .. }
                | AmirStmt::Free(_)
                | AmirStmt::StorageLive(_)
                | AmirStmt::StorageDead(_)
                | AmirStmt::Destroy(_)
                | AmirStmt::Nop => false,
            })
        })
}

pub(crate) fn runtime_imports(
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<RuntimeImports, Diagnostic> {
    let mut catalog = RuntimeImports::default();
    let mut aliases = FxHashMap::default();
    memory::declare_memory_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    bench_gen::declare_bench_gen_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    task::declare_coroutine_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    os_path::declare_os_path_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    vec::declare_vec_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    reactor_socket::declare_reactor_imports(
        &mut catalog,
        &mut aliases,
        default_call_conv,
        ptr_type,
    )?;
    memory::declare_libc_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    os_path::declare_text_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    os_path::declare_fs_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    task::declare_task_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    reactor_socket::declare_net_imports(&mut catalog, &mut aliases, default_call_conv, ptr_type)?;
    catalog.aliases = aliases;
    Ok(catalog)
}
