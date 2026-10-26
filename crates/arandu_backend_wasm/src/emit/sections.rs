use rustc_hash::FxHashMap;
use smol_str::SmolStr;
use wasm_encoder::{TypeSection, ValType};

use arandu_middle::SymbolId;
use arandu_middle::amir::stmt::AmirStmt;
use arandu_middle::amir::value::AmirOperand;

use super::{
    ExportStyle, WasmImportInfo, WasmModuleBuilder, func_wasm_params, func_wasm_results,
    wasm_type_to_valtype,
};

impl<'a> WasmModuleBuilder<'a> {
    /// Collect all imported extern functions deterministically.
    pub(super) fn collect_imports(&self, style: ExportStyle<'a>) -> Vec<WasmImportInfo> {
        let mut sorted_extern: Vec<_> = self
            .program
            .extern_funcs
            .iter()
            .filter(|(symbol, _)| self.live.contains(symbol))
            .collect();
        sorted_extern.sort_by_key(|(sym, _)| (sym.file_id, sym.local_id.0));

        let mut import_infos = Vec::with_capacity(sorted_extern.len());
        for (&sym_id, (params, ret)) in sorted_extern {
            let Some(sym) = self.symbols.try_get(sym_id) else {
                continue;
            };
            if arandu_middle::IntrinsicKind::from_declared_name(&sym.name, sym.lang_item).is_some()
                || matches!(
                    sym.name.as_str(),
                    "ar_rt_alloc_aligned" | "ar_rt_free_aligned"
                )
            {
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

        // Detect prelude functions called in the program but not declared as
        // externs. Their host imports are supplied by the runtime.
        let mut called_prelude_symbols: Vec<(SymbolId, &'static str)> = Vec::new();
        for func in &self.functions {
            for stmt in func.stmts.payloads.iter() {
                if let AmirStmt::Call {
                    callee: AmirOperand::FunctionRef(sym),
                    ..
                } = stmt
                    && !self.program.extern_funcs.contains_key(sym)
                    && !self.program.funcs.iter().any(|f| f.symbol == *sym)
                    && let Some(sym_def) = self.symbols.try_get(*sym)
                    && let Some(field) = match sym_def.name.as_str() {
                        "io.println" => Some("println"),
                        "io.print" => Some("print"),
                        "io.eprint" => Some("eprint"),
                        _ => None,
                    }
                    && !called_prelude_symbols
                        .iter()
                        .any(|(existing, _)| existing == sym)
                {
                    called_prelude_symbols.push((*sym, field));
                }
            }
        }
        called_prelude_symbols.sort_by_key(|(sym, _)| (sym.file_id, sym.local_id.0));
        for (sym_id, field) in called_prelude_symbols {
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
                field: SmolStr::new(field),
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
    pub(super) fn build_type_section_raw(
        &self,
        imports: &[WasmImportInfo],
        style: ExportStyle<'a>,
    ) -> (TypeSection, Vec<u32>, Vec<u32>, u32) {
        let mut type_section = TypeSection::new();
        let mut type_cache: FxHashMap<(Vec<ValType>, Vec<ValType>), u32> = FxHashMap::default();
        let mut import_type_indices = Vec::with_capacity(imports.len());
        let mut type_indices = Vec::with_capacity(self.functions.len());
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

        for func in &self.functions {
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
}
