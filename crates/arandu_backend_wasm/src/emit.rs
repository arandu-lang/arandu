//! WebAssembly module builder.
//!
//! [`WasmModuleBuilder`] orchestrates all Wasm sections in the canonical order
//! required by the spec: Type → Import → Function → Table → Memory → Global
//! → Export → Element → Code → Data.
//!
//! It drives the per-function translation via the [`crate::translator`] module
//! and emits a complete, valid `.wasm` binary as `Vec<u8>`.
//!
//! The module always includes `cabi_realloc` and exports `memory` so that
//! Component Model hosts can safely lower strings and lists into the guest.

mod component;
mod validate;

pub(crate) use validate::validate_place_ops;

use rustc_hash::FxHashMap;
use smol_str::SmolStr;
use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, FunctionSection, GlobalSection, GlobalType,
    ImportSection, MemorySection, MemoryType, Module, TypeSection, ValType,
};
use wit_parser::abi::{WasmSignature, WasmType};

use arandu_middle::amir::{AmirFunc, AmirProgram};
use arandu_middle::layout::{DataLayout, StructLayoutProvider};
use arandu_middle::types::TypeInterner;
use arandu_middle::{Diagnostic, SymbolId};
use arandu_semantics::SymbolTable;

use arandu_middle::amir::stmt::AmirStmt;
use arandu_middle::amir::value::AmirOperand;

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
}

impl<'a> WasmModuleBuilder<'a> {
    #[must_use]
    pub fn new(
        program: &'a AmirProgram,
        symbols: &'a SymbolTable,
        interner: &'a TypeInterner,
        layout_provider: &'a dyn StructLayoutProvider,
        layout: DataLayout,
    ) -> Self {
        Self {
            program,
            symbols,
            interner,
            layout_provider,
            layout,
        }
    }

    /// Build all sections and return the encoded Wasm core module bytes.
    ///
    /// The returned binary always includes `cabi_realloc` (exported) and
    /// `memory` (exported) so that Component Model hosts can lower strings and
    /// lists into the guest's linear memory.
    pub fn build(self) -> Result<Vec<u8>, Diagnostic> {
        self.build_core(ExportStyle::Host)
    }

    /// Build the core module with a specific export naming style.
    fn build_core(self, style: ExportStyle<'a>) -> Result<Vec<u8>, Diagnostic> {
        let mut module = Module::new();

        let imports = self.collect_imports(style);
        let num_imported_funcs = imports.len() as u32;

        // Build Canonical ABI helpers + the shared heap allocator (always
        // emitted: __arandu_alloc, __arandu_free, cabi_realloc).
        let mut cabi = CabiSupport::new();
        // Allocator funcs are the first two entries, so their absolute wasm
        // function indices are `cabi_first_func_idx + 0/+1`.
        cabi.add_allocator();
        let cabi_first_func_idx = num_imported_funcs + self.program.funcs.len() as u32;
        cabi.add_realloc(cabi_first_func_idx, cabi_first_func_idx + 1);
        // Under the Component Model, public functions returning via retptr
        // (records >1 field, strings, lists) export a post-return stub
        // so the host can reclaim the temporary buffer via `__arandu_free`.
        if let ExportStyle::Component(names) = &style {
            for func in &self.program.funcs {
                if let Some((export_name, sig)) = names.get(&func.symbol)
                    && sig.retptr
                {
                    cabi.add_post_return(export_name, vec![ValType::I32], cabi_first_func_idx + 1);
                }
            }
        }

        // ── 1. Type section ───────────────────────────────────────────────
        let (mut type_section, import_type_indices, func_type_indices, mut next_type_idx) =
            self.build_type_section_raw(&imports, style);
        let cabi_type_indices = cabi.append_types(&mut type_section, &mut next_type_idx);
        module.section(&type_section);

        // ── 2. Import section ─────────────────────────────────────────────
        if !imports.is_empty() {
            let mut import_section = ImportSection::new();
            for (i, import) in imports.iter().enumerate() {
                let type_idx = import_type_indices[i];
                import_section.import(
                    &import.module,
                    &import.field,
                    EntityType::Function(type_idx),
                );
            }
            module.section(&import_section);
        }

        // ── 3. Function section ───────────────────────────────────────────
        let mut func_section = FunctionSection::new();
        let mut func_index_map: FxHashMap<SymbolId, u32> = FxHashMap::default();

        for (i, import) in imports.iter().enumerate() {
            func_index_map.insert(import.symbol, i as u32);
        }

        for (i, func) in self.program.funcs.iter().enumerate() {
            let type_idx = func_type_indices[i];
            func_section.function(type_idx);
            func_index_map.insert(func.symbol, num_imported_funcs + i as u32);
        }
        // Canonical ABI + allocator functions come right after program functions.
        cabi.append_function_section(&mut func_section, &cabi_type_indices);
        module.section(&func_section);

        // ── 4. Table section (empty for MVP) ──────────────────────────────

        // ── 5. Memory section ─────────────────────────────────────────────
        let mut mem_section = MemorySection::new();
        mem_section.memory(MemoryType {
            minimum: crate::memory::INITIAL_PAGES,
            maximum: crate::memory::MAX_PAGES,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        module.section(&mem_section);

        // Build the static rodata table from literal pool.
        let rodata = crate::memory::RodataTable::from_literal_pool(&self.program.literal_pool);
        let heap_base = rodata.heap_base();

        // ── 6. Global section ─────────────────────────────────────────────
        let mut global_section = GlobalSection::new();
        // Global 0: __stack_pointer (mutable i32, init = STACK_BASE).
        global_section.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &wasm_encoder::ConstExpr::i32_const(crate::memory::STACK_BASE),
        );
        // Global 1: __heap_base (immutable i32, init = heap_base).
        global_section.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: false,
                shared: false,
            },
            &wasm_encoder::ConstExpr::i32_const(heap_base),
        );
        // Global 2: __heap_ptr (mutable i32, init = heap_base).
        global_section.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &wasm_encoder::ConstExpr::i32_const(heap_base),
        );
        // Global 3: __stack_base (immutable i32, init = STACK_BASE).
        global_section.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: false,
                shared: false,
            },
            &wasm_encoder::ConstExpr::i32_const(crate::memory::STACK_BASE),
        );
        // Global 4: __freelist_head (mutable i32, init = 0 — empty free list).
        global_section.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &wasm_encoder::ConstExpr::i32_const(0),
        );
        module.section(&global_section);

        // ── 7. Export section ─────────────────────────────────────────────
        let export_section =
            self.build_export_section(&func_index_map, &cabi, cabi_first_func_idx, style);
        module.section(&export_section);

        // ── 8. Element section (empty for MVP) ────────────────────────────

        // ── 9. Code section ───────────────────────────────────────────────
        let ctx = crate::translator::TranslationContext {
            symbols: self.symbols,
            interner: self.interner,
            layout_provider: self.layout_provider,
            data_layout: self.layout,
            literal_pool: &self.program.literal_pool,
            rodata_offsets: &rodata.offsets,
            func_index_map: &func_index_map,
            alloc_func_idx: cabi_first_func_idx,
            free_func_idx: cabi_first_func_idx + 1,
        };

        let mut code_section = CodeSection::new();
        for func in &self.program.funcs {
            let wasm_func = self.translate_func(func, &ctx, style)?;
            code_section.function(&wasm_func);
        }
        cabi.append_code(&mut code_section);
        module.section(&code_section);

        // ── 10. Data section ──────────────────────────────────────────────
        if !rodata.bytes.is_empty() {
            let mut data_section = wasm_encoder::DataSection::new();
            data_section.segment(wasm_encoder::DataSegment {
                mode: wasm_encoder::DataSegmentMode::Active {
                    memory_index: 0,
                    offset: &wasm_encoder::ConstExpr::i32_const(crate::memory::RODATA_BASE),
                },
                data: rodata.bytes.iter().copied(),
            });
            module.section(&data_section);
        }

        Ok(module.finish())
    }

    /// Translate a single AMIR function into a wasm Function.
    fn translate_func(
        &self,
        func: &AmirFunc,
        ctx: &crate::translator::TranslationContext<'_>,
        style: ExportStyle<'a>,
    ) -> Result<wasm_encoder::Function, Diagnostic> {
        let ops = stackify::stackify(func)?;
        let component_sig = match style {
            ExportStyle::Host => None,
            ExportStyle::Component(names) => names.get(&func.symbol).map(|(_, sig)| sig),
        };
        let mut translator = FuncTranslator::new(func, ctx, component_sig);
        Ok(translator.translate(ops))
    }

    /// Collect all imported extern functions deterministically.
    fn collect_imports(&self, style: ExportStyle<'a>) -> Vec<WasmImportInfo> {
        let mut sorted_extern: Vec<_> = self.program.extern_funcs.iter().collect();
        sorted_extern.sort_by_key(|(sym, _)| (sym.file_id, sym.local_id.0));

        let mut import_infos = Vec::with_capacity(sorted_extern.len());
        for (&sym_id, (params, ret)) in sorted_extern {
            let Some(sym) = self.symbols.try_get(sym_id) else {
                continue;
            };
            if arandu_middle::IntrinsicKind::from_name(&sym.name).is_some() {
                continue;
            }
            let (module, field) = match style {
                ExportStyle::Component(_) => {
                    if let Some((m, f)) = sym.name.split_once('.') {
                        (
                            SmolStr::new(m),
                            SmolStr::new(crate::wit_gen::to_wit_ident(f)),
                        )
                    } else {
                        (
                            SmolStr::new("$root"),
                            SmolStr::new(crate::wit_gen::to_wit_ident(&sym.name)),
                        )
                    }
                }
                ExportStyle::Host => {
                    if let Some((m, f)) = sym.name.split_once('.') {
                        (SmolStr::new(m), SmolStr::new(f))
                    } else {
                        (SmolStr::new("env"), sym.name.clone())
                    }
                }
            };

            let mut wasm_params = Vec::new();
            for p in params {
                wasm_params.extend(crate::types::ar_type_valtypes(
                    p,
                    self.interner,
                    self.layout,
                ));
            }
            let wasm_results = crate::types::ar_type_valtypes(ret, self.interner, self.layout);

            import_infos.push(WasmImportInfo {
                symbol: sym_id,
                module,
                field,
                params: wasm_params,
                results: wasm_results,
            });
        }

        // Detect prelude functions (e.g. `io.println`) called in the program
        // but not declared in `program.extern_funcs`.
        let mut called_prelude_symbols: Vec<SymbolId> = Vec::new();
        for func in &self.program.funcs {
            for stmt in func.stmts.payloads.iter() {
                if let AmirStmt::Call {
                    callee: AmirOperand::FunctionRef(sym),
                    ..
                } = stmt
                    && !self.program.extern_funcs.contains_key(sym)
                    && !self.program.funcs.iter().any(|f| f.symbol == *sym)
                    && !called_prelude_symbols.contains(sym)
                    && let Some(sym_def) = self.symbols.try_get(*sym)
                    && sym_def.name == "io.println"
                {
                    called_prelude_symbols.push(*sym);
                }
            }
        }
        called_prelude_symbols.sort_by_key(|sym| (sym.file_id, sym.local_id.0));
        for sym_id in called_prelude_symbols {
            let str_ty =
                arandu_middle::types::ArType::Primitive(arandu_middle::types::Primitive::Str);
            let void_ty = arandu_middle::types::ArType::Void;
            let mut wasm_params = Vec::new();
            wasm_params.extend(crate::types::ar_type_valtypes(
                &str_ty,
                self.interner,
                self.layout,
            ));
            let wasm_results = crate::types::ar_type_valtypes(&void_ty, self.interner, self.layout);

            import_infos.push(WasmImportInfo {
                symbol: sym_id,
                module: SmolStr::new("io"),
                field: SmolStr::new("println"),
                params: wasm_params,
                results: wasm_results,
            });
        }

        import_infos
    }

    /// Build the type section with deduplication for both imports and defined functions.
    ///
    /// Returns `(TypeSection, import type indices, per-program-func type indices, next_type_idx)`.
    /// The caller appends Canonical ABI types after.
    fn build_type_section_raw(
        &self,
        imports: &[WasmImportInfo],
        style: ExportStyle<'a>,
    ) -> (TypeSection, Vec<u32>, Vec<u32>, u32) {
        let mut type_section = TypeSection::new();
        let mut type_cache: FxHashMap<(Vec<ValType>, Vec<ValType>), u32> = FxHashMap::default();
        let mut import_type_indices = Vec::with_capacity(imports.len());
        let mut type_indices = Vec::with_capacity(self.program.funcs.len());
        let mut next_type_idx: u32 = 0;

        for import in imports {
            let key = (import.params.clone(), import.results.clone());
            let type_idx = match type_cache.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let (p, r) = e.key();
                    type_section.ty().function(p.clone(), r.clone());
                    let idx = next_type_idx;
                    next_type_idx += 1;
                    *e.insert(idx)
                }
            };
            import_type_indices.push(type_idx);
        }

        for func in &self.program.funcs {
            let (params, results) = match style {
                ExportStyle::Component(names) if names.contains_key(&func.symbol) => {
                    let (_, sig) = &names[&func.symbol];
                    let params = sig
                        .params
                        .iter()
                        .copied()
                        .map(wasm_type_to_valtype)
                        .collect();
                    let results = sig
                        .results
                        .iter()
                        .copied()
                        .map(wasm_type_to_valtype)
                        .collect();
                    (params, results)
                }
                _ => {
                    let params = func_wasm_params(func, self.interner, self.layout);
                    let results = func_wasm_results(func, self.interner, self.layout);
                    (params, results)
                }
            };

            let key = (params, results);
            let type_idx = match type_cache.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let (p, r) = e.key();
                    type_section.ty().function(p.clone(), r.clone());
                    let idx = next_type_idx;
                    next_type_idx += 1;
                    *e.insert(idx)
                }
            };
            type_indices.push(type_idx);
        }

        (
            type_section,
            import_type_indices,
            type_indices,
            next_type_idx,
        )
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

        for func in &self.program.funcs {
            if let Some(&wasm_idx) = func_index_map.get(&func.symbol) {
                let Some(sym) = self.symbols.try_get(func.symbol) else {
                    continue;
                };
                let name: std::borrow::Cow<'_, str> = match style {
                    ExportStyle::Host => std::borrow::Cow::Borrowed(sym.name.as_str()),
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
