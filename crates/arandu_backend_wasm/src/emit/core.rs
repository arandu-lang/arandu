use rustc_hash::{FxHashMap, FxHashSet};
use wasm_encoder::{
    CodeSection, EntityType, FunctionSection, GlobalSection, GlobalType, ImportSection,
    MemorySection, MemoryType, Module, ValType,
};

use arandu_middle::amir::value::AmirOperand;
use arandu_middle::{Diagnostic, SymbolId};

use crate::canonical::CabiSupport;

use super::{ExportStyle, WasmModuleBuilder};

impl<'a> WasmModuleBuilder<'a> {
    /// Build the core module with a specific export naming style.
    pub(super) fn build_core(self, style: ExportStyle<'a>) -> Result<Vec<u8>, Diagnostic> {
        let mut module = Module::new();

        let imports = self.collect_imports(style);
        for import in &imports {
            if import.field == "ar_rt_thread_park_timeout" {
                let span = self
                    .symbols
                    .try_get(import.symbol)
                    .map_or(arandu_middle::Span::new(0, 0, 0), |s| s.span);
                return Err(Diagnostic::error(
                    arandu_middle::diagnostics::DiagCode::U001FeatureNotSupported,
                    "`parkTimeout` requires target thread support to wake on unpark, which is unavailable on wasm32; use `std.thread.sleep` for a non-interruptible delay",
                    span,
                ));
            }
            if matches!(
                import.field.as_str(),
                "ar_rt_thread_park" | "ar_rt_thread_unpark" | "ar_rt_native_thread_spawn"
            ) {
                let span = self
                    .symbols
                    .try_get(import.symbol)
                    .map_or(arandu_middle::Span::new(0, 0, 0), |s| s.span);
                return Err(Diagnostic::error(
                    arandu_middle::diagnostics::DiagCode::U001FeatureNotSupported,
                    "thread synchronization and spawning are unsupported on single-threaded WebAssembly targets; use cooperative tasks or async execution instead",
                    span,
                ));
            }
        }
        let num_imported_funcs = imports.len() as u32;

        // Helpers follow all program definitions. Translate first so size mode
        // can decide from actual calls, including implicit heap-backed frames.
        let cabi_first_func_idx = num_imported_funcs + self.functions.len() as u32;
        let canonical_abi = !self.optimize_for_size || matches!(style, ExportStyle::Component(_));

        // ── 1. Type section ───────────────────────────────────────────────
        let (mut type_section, import_type_indices, func_type_indices, mut next_type_idx) =
            self.build_type_section_raw(&imports, style);

        // ── 2. Import section ─────────────────────────────────────────────
        let mut import_section = ImportSection::new();
        if !imports.is_empty() {
            for (i, import) in imports.iter().enumerate() {
                let type_idx = import_type_indices[i];
                import_section.import(
                    &import.module,
                    &import.field,
                    EntityType::Function(type_idx),
                );
            }
        }

        // ── 3. Function section ───────────────────────────────────────────
        let mut func_section = FunctionSection::new();
        let mut func_index_map: FxHashMap<SymbolId, u32> = FxHashMap::default();

        for (i, import) in imports.iter().enumerate() {
            func_index_map.insert(import.symbol, i as u32);
        }

        for (i, func) in self.functions.iter().enumerate() {
            let type_idx = func_type_indices[i];
            func_section.function(type_idx);
            func_index_map.insert(func.symbol, num_imported_funcs + i as u32);
        }

        // ── 4. Table section (empty for MVP) ──────────────────────────────

        // ── 5. Memory section ─────────────────────────────────────────────
        let mut used_literals = FxHashSet::default();
        let mut remember = |operand: &AmirOperand| {
            if let AmirOperand::Constant(arandu_middle::amir::AmirConstant::Pool(id)) = operand {
                used_literals.insert(*id);
            }
        };
        for func in &self.functions {
            for block in &func.blocks {
                for stmt in func.block_stmts(block.id) {
                    arandu_middle::amir::visit::for_each_stmt_operand(stmt, &mut remember);
                }
                arandu_middle::amir::visit::for_each_terminator_operand(
                    &block.terminator,
                    &mut remember,
                );
            }
        }
        let mut rodata = crate::memory::RodataTable::from_used_literals(
            &self.program.literal_pool,
            &used_literals,
        );
        let mut static_offsets = FxHashMap::default();
        let mut static_sources: FxHashMap<Vec<u8>, (u32, u32)> = FxHashMap::default();
        for func in &self.functions {
            let mut initializers = arandu_mir::static_data::static_initializers(
                func,
                self.interner,
                self.layout_provider,
                &self.program.literal_pool,
                self.layout,
                true,
            );
            // Statement order, not hash iteration, determines artifact bytes.
            for id in func.stmts.iter_ids() {
                let Some(initializer) = initializers.remove(&id) else {
                    continue;
                };
                // Offsets are relative to RODATA_BASE, so they can honour any
                // power-of-two alignment up to that base's own alignment.
                let base_align = 1u64 << crate::memory::RODATA_BASE.trailing_zeros();
                if !initializer.alignment.is_power_of_two() || initializer.alignment > base_align {
                    continue;
                }
                // Every rodata blob starts on at least a 16-byte boundary.
                let align_mask = usize::try_from(initializer.alignment.max(16))
                    .map_or(usize::MAX, |align| align - 1);
                if let Some(&source) = static_sources.get(&initializer.bytes) {
                    static_offsets.insert((func.symbol, id), source);
                    continue;
                }
                let aligned = rodata
                    .bytes
                    .len()
                    .checked_add(align_mask)
                    .map(|length| length & !align_mask);
                let offset = aligned.and_then(|length| {
                    usize::try_from(crate::memory::RODATA_BASE)
                        .ok()?
                        .checked_add(length)
                });
                let Some((aligned, offset, size)) =
                    aligned.zip(offset).and_then(|(aligned, offset)| {
                        Some((
                            aligned,
                            u32::try_from(offset).ok()?,
                            u32::try_from(initializer.bytes.len()).ok()?,
                        ))
                    })
                else {
                    return Err(Diagnostic::ice(
                        arandu_middle::diagnostics::DiagCode::ICEGEN002,
                        "Wasm static aggregate data exceeds the supported address space",
                        arandu_middle::Span::new(0, 0, 0),
                    ));
                };
                rodata.bytes.resize(aligned, 0);
                rodata.bytes.extend_from_slice(&initializer.bytes);
                static_sources.insert(initializer.bytes, (offset, size));
                static_offsets.insert((func.symbol, id), (offset, size));
            }
        }
        let (heap_base, initial_pages) = rodata.memory_regions().ok_or_else(|| {
            Diagnostic::ice(
                arandu_middle::diagnostics::DiagCode::ICEGEN002,
                "Wasm rodata and shadow stack exceed the supported address space",
                arandu_middle::Span::new(0, 0, 0),
            )
        })?;
        let mut mem_section = MemorySection::new();
        mem_section.memory(MemoryType {
            minimum: initial_pages,
            maximum: crate::memory::MAX_PAGES,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });

        // ── 6. Global section ─────────────────────────────────────────────
        let mut global_section = GlobalSection::new();
        // Global 0: __stack_pointer (mutable i32, init = STACK_BASE).
        global_section.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &wasm_encoder::ConstExpr::i32_const(heap_base),
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
            &wasm_encoder::ConstExpr::i32_const(heap_base),
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

        // ── 8. Element section (empty for MVP) ────────────────────────────

        // ── 9. Code section ───────────────────────────────────────────────
        let ctx = crate::translator::TranslationContext {
            functions: &self.program.funcs,
            extern_functions: &self.program.extern_funcs,
            symbols: self.symbols,
            interner: self.interner,
            layout_provider: self.layout_provider,
            data_layout: self.layout,
            literal_pool: &self.program.literal_pool,
            rodata_offsets: &rodata.offsets,
            static_offsets: &static_offsets,
            func_index_map: &func_index_map,
            alloc_func_idx: cabi_first_func_idx,
            free_func_idx: cabi_first_func_idx + 1,
        };

        let mut code_section = CodeSection::new();
        let mut needs_allocator = canonical_abi;
        for func in &self.functions {
            let (wasm_func, uses_allocator) = self.translate_func(func, &ctx, style)?;
            needs_allocator |= uses_allocator;
            code_section.function(&wasm_func);
        }
        let mut cabi = CabiSupport::new();
        if needs_allocator {
            cabi.add_allocator();
        }
        if canonical_abi {
            cabi.add_realloc(cabi_first_func_idx, cabi_first_func_idx + 1);
            if let ExportStyle::Component(names) = &style {
                for func in &self.functions {
                    if let Some((export_name, sig)) = names.get(&func.symbol)
                        && sig.retptr
                    {
                        cabi.add_post_return(
                            export_name,
                            vec![ValType::I32],
                            cabi_first_func_idx + 1,
                        );
                    }
                }
            }
        }
        let cabi_type_indices = cabi.append_types(&mut type_section, &mut next_type_idx);
        cabi.append_function_section(&mut func_section, &cabi_type_indices);
        cabi.append_code(&mut code_section);
        let export_section =
            self.build_export_section(&func_index_map, &cabi, cabi_first_func_idx, style);

        // Serialize in Wasm section order after all dependencies are known.
        module.section(&type_section);
        if !imports.is_empty() {
            module.section(&import_section);
        }
        module.section(&func_section);
        module.section(&mem_section);
        module.section(&global_section);
        module.section(&export_section);
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
}
