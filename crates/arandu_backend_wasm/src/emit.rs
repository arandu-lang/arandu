//! WebAssembly module builder.
//!
//! [`WasmModuleBuilder`] orchestrates all Wasm sections in the canonical order
//! required by the spec: Type → Import → Function → Table → Memory → Global
//! → Export → Element → Code → Data.
//!
//! It drives the per-function translation via the [`crate::translator`] module
//! and emits a complete, valid `.wasm` binary as `Vec<u8>`.
//!
//! Default emission includes `cabi_realloc` and exports `memory`. Size-oriented
//! core modules omit unused allocation helpers; components retain their ABI.

mod component;
mod core;
mod sections;
mod validate;

pub(crate) use validate::validate_place_ops;

use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;
use wasm_encoder::{ExportKind, ExportSection, ValType};
use wit_parser::abi::{WasmSignature, WasmType};

use arandu_middle::amir::{AmirFunc, AmirProgram};
use arandu_middle::layout::{DataLayout, StructLayoutProvider};
use arandu_middle::types::TypeInterner;
use arandu_middle::{Diagnostic, SymbolId};
use arandu_semantics::SymbolTable;

use crate::canonical::CabiSupport;
use crate::stackify;
use crate::translator::FuncTranslator;
use crate::types::{self, Shape};

/// Canonical ABI export mapping: `SymbolId` → (mangled core export name, Canonical ABI signature).
pub type ComponentExportMap = FxHashMap<SymbolId, (String, WasmSignature)>;

/// Metadata for an imported function in the WebAssembly module.
#[derive(Debug)]
struct WasmImportInfo {
    symbol: SymbolId,
    module: SmolStr,
    field: SmolStr,
    params: Vec<ValType>,
    results: Vec<ValType>,
}

/// How the core module names its exports.
///
/// * [`ExportStyle::Host`] — plain names (`main`, `memory`, `cabi_realloc`)
///   for host tooling that consumes a bare core module.
/// * [`ExportStyle::Component`] — Canonical ABI names and signatures expected by
///   the `wit-component` decoder. The map carries the exact mangled name and
///   signature per public symbol, derived from the parsed WIT `Resolve`.
#[derive(Clone, Copy)]
enum ExportStyle<'a> {
    Host,
    Component(&'a ComponentExportMap),
}

/// Builds a WebAssembly core module from an [`AmirProgram`].
pub struct WasmModuleBuilder<'a> {
    program: &'a AmirProgram,
    symbols: &'a SymbolTable,
    interner: &'a TypeInterner,
    layout_provider: &'a dyn StructLayoutProvider,
    layout: DataLayout,
    functions: Vec<&'a AmirFunc>,
    live: FxHashSet<SymbolId>,
    optimize_for_size: bool,
}

impl<'a> WasmModuleBuilder<'a> {
    /// Names a host export without conflating an associated method with a
    /// same-named free function. Internal calls continue to use SymbolIds; this
    /// name is only the external core-module surface.
    fn host_export_name(&self, symbol: &arandu_middle::symbol_table::Symbol) -> String {
        if let Some(((owner, method), _)) = self
            .symbols
            .associated_members
            .iter()
            .filter(|(_, member_id)| **member_id == symbol.id)
            .min_by_key(|((owner, method), _)| (owner.file_id, owner.local_id.0, method.as_str()))
        {
            let module_prefix = self
                .symbols
                .host_func_name(symbol)
                .rsplit_once('.')
                .map_or("", |(module, _)| module);
            let owner_name = self
                .symbols
                .try_get(*owner)
                .map_or("type", |owner| owner.name.as_str());
            if module_prefix.is_empty() {
                format!("{owner_name}.{method}")
            } else {
                format!("{module_prefix}.{owner_name}.{method}")
            }
        } else {
            self.symbols.host_func_name(symbol).to_owned()
        }
    }

    #[must_use]
    pub fn new(
        program: &'a AmirProgram,
        symbols: &'a SymbolTable,
        interner: &'a TypeInterner,
        layout_provider: &'a dyn StructLayoutProvider,
        layout: DataLayout,
    ) -> Self {
        // Host imports are part of the module's linking contract even when
        // never called. Apply the same dependency closure in every profile so
        // an unreachable private body cannot introduce a host requirement.
        let live = arandu_semantics::passes::reachability::live_functions(
            program,
            symbols,
            interner,
            layout_provider,
        );
        Self {
            program,
            symbols,
            interner,
            layout_provider,
            layout,
            functions: program
                .funcs
                .iter()
                .filter(|function| live.contains(&function.symbol))
                .collect(),
            live,
            optimize_for_size: false,
        }
    }

    /// Omit unused core-module ABI helpers in addition to the shared pruning.
    /// Components still retain their mandatory Canonical ABI support.
    #[must_use]
    pub fn optimize_for_size(mut self) -> Self {
        self.optimize_for_size = true;
        self
    }

    /// Build all sections and return the encoded Wasm core module bytes.
    ///
    /// Exports `memory` and, unless size optimization was selected, `cabi_realloc`
    /// for hosts lowering strings and lists into the guest's linear memory.
    pub fn build(self) -> Result<Vec<u8>, Diagnostic> {
        self.build_core(ExportStyle::Host)
    }

    /// Translate a single AMIR function into a wasm Function.
    fn translate_func(
        &self,
        func: &AmirFunc,
        ctx: &crate::translator::TranslationContext<'_>,
        style: ExportStyle<'a>,
    ) -> Result<(wasm_encoder::Function, bool), Diagnostic> {
        let ops = stackify::stackify(func)?;
        let component_sig = match style {
            ExportStyle::Host => None,
            ExportStyle::Component(names) => names.get(&func.symbol).map(|(_, sig)| sig),
        };
        let mut translator = FuncTranslator::new(func, ctx, component_sig);
        let function = translator.translate(ops);
        let uses_allocator = translator.code.iter().any(|instruction| {
            matches!(instruction, wasm_encoder::Instruction::Call(index)
                if *index == ctx.alloc_func_idx || *index == ctx.free_func_idx)
        });
        Ok((function, uses_allocator))
    }

    /// Build the export section, exporting all functions and linear memory.
    ///
    /// Canonical ABI functions (`cabi_realloc`, etc.) are appended after the
    /// program functions. Under [`ExportStyle::Component`] the public functions
    /// are exported with the Canonical ABI names carried in the map.
    fn build_export_section(
        &self,
        func_index_map: &FxHashMap<SymbolId, u32>,
        cabi: &CabiSupport,
        cabi_first_func_idx: u32,
        style: ExportStyle,
    ) -> ExportSection {
        let mut export_section = ExportSection::new();

        for func in &self.functions {
            if let Some(&wasm_idx) = func_index_map.get(&func.symbol) {
                let Some(sym) = self.symbols.try_get(func.symbol) else {
                    continue;
                };
                let name: std::borrow::Cow<'_, str> = match style {
                    ExportStyle::Host => std::borrow::Cow::Owned(self.host_export_name(sym)),
                    ExportStyle::Component(names) => names
                        .get(&func.symbol)
                        .map(|(mangled, _)| std::borrow::Cow::Borrowed(mangled.as_str()))
                        .unwrap_or(std::borrow::Cow::Borrowed(sym.name.as_str())),
                };
                export_section.export(&name, ExportKind::Func, wasm_idx);
            }
        }

        // Export the linear memory.
        export_section.export("memory", ExportKind::Memory, 0);

        // Export Canonical ABI functions.
        cabi.append_exports(&mut export_section, cabi_first_func_idx);

        export_section
    }
}

/// Compute the wasm parameter types for a function.
fn func_wasm_params(func: &AmirFunc, interner: &TypeInterner, layout: DataLayout) -> Vec<ValType> {
    let mut params = Vec::new();
    // The receiver is materialized as `func.params[0]`; only emit it separately
    // for a receiver that is somehow absent from the parameter list, otherwise
    // it would be counted twice in the wasm signature.
    if let Some(recv) = func.receiver
        && !func.params.contains(&recv.temp)
    {
        let ty = func.temps[recv.temp.as_usize()].ty;
        let shape = types::shape(ty, interner, layout);
        match shape {
            Shape::Empty => {}
            Shape::Scalar => {
                params.push(types::scalar_valtype_for(ty, interner, layout).unwrap_or(ValType::I32))
            }
            Shape::Fat => {
                params.push(ValType::I32);
                params.push(ValType::I32);
            }
        }
    }
    for &param in &func.params {
        let ty = func.temps[param.as_usize()].ty;
        let shape = types::shape(ty, interner, layout);
        match shape {
            Shape::Empty => {}
            Shape::Scalar => {
                params.push(types::scalar_valtype_for(ty, interner, layout).unwrap_or(ValType::I32))
            }
            Shape::Fat => {
                params.push(ValType::I32);
                params.push(ValType::I32);
            }
        }
    }
    params
}

/// Convert a WIT ABI wasm type to a `wasm_encoder::ValType`.
pub(crate) fn wasm_type_to_valtype(w: WasmType) -> ValType {
    match w {
        WasmType::I32 => ValType::I32,
        WasmType::I64 => ValType::I64,
        WasmType::F32 => ValType::F32,
        WasmType::F64 => ValType::F64,
        WasmType::Pointer => ValType::I32,
        WasmType::PointerOrI64 => ValType::I64,
        WasmType::Length => ValType::I32,
    }
}

/// Compute the wasm result types for a function.
fn func_wasm_results(func: &AmirFunc, interner: &TypeInterner, layout: DataLayout) -> Vec<ValType> {
    let shape = types::shape(func.return_type, interner, layout);
    match shape {
        Shape::Empty => vec![],
        Shape::Scalar => {
            vec![
                types::scalar_valtype_for(func.return_type, interner, layout)
                    .unwrap_or(ValType::I32),
            ]
        }
        Shape::Fat => vec![ValType::I32, ValType::I32],
    }
}

/// Validate that the emitted bytes start with the WebAssembly core module header.
#[must_use]
pub fn is_core_module(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00])
}

/// Validate that the emitted bytes start with the WebAssembly Component Model header.
#[must_use]
pub fn is_component(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00])
}

/// Validate that the emitted bytes start with a valid WebAssembly magic + version
/// (either a core module or a component).
#[must_use]
pub fn validate_magic(bytes: &[u8]) -> bool {
    is_core_module(bytes) || is_component(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_validator_accepts_valid_header() {
        let header = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0xff];
        assert!(validate_magic(&header));
        assert!(is_core_module(&header));
        assert!(!is_component(&header));
    }

    #[test]
    fn magic_validator_accepts_component_header() {
        let comp = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00, 0xff];
        assert!(validate_magic(&comp));
        assert!(is_component(&comp));
        assert!(!is_core_module(&comp));
    }

    #[test]
    fn magic_validator_rejects_empty() {
        assert!(!validate_magic(&[]));
        assert!(!is_core_module(&[]));
        assert!(!is_component(&[]));
    }

    #[test]
    fn magic_validator_rejects_wrong_magic() {
        let bad = [0x00, 0x61, 0x73, 0x6e, 0x01, 0x00, 0x00, 0x00];
        assert!(!validate_magic(&bad));
        assert!(!is_core_module(&bad));
        assert!(!is_component(&bad));
    }
}
