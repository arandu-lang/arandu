use arandu_middle::amir::AmirProgram;
use arandu_middle::layout::TargetAbiClassifier;
use arandu_middle::types::{ArType, Primitive};
use arandu_middle::{Diagnostic, SymbolId, SymbolKind, SymbolTable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use rustc_hash::FxHashMap;

use super::super::block_coverage::BlockCoverageSession;
use super::super::isa::codegen_ice;
use super::super::symbols::{gen_shutdown_required, runtime_imports};
use super::{
    AranduModule, DebugCodeRange, DebugFunction, DebugLocal, DebugValueLocation, DebugValueRange,
};
use crate::abi::{
    build_internal_signature, build_signature, build_signature_with_classifier,
    target_abi_for_triple,
};
use crate::translator::FunctionTranslator;

impl<M: Module> AranduModule<M> {
    pub(super) fn compile_filtered_module_inner(
        &mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &dyn arandu_middle::type_context::CodegenTypeContext,
        unit_func_symbols: Option<&[SymbolId]>,
        block_coverage: Option<&BlockCoverageSession>,
    ) -> Result<FxHashMap<String, FuncId>, Diagnostic> {
        if let Some(issue) =
            arandu_middle::validate_amir_program(program, symbols, type_info.type_interner())
                .into_iter()
                .next()
        {
            return Err(issue);
        }
        let is_unit_func = |sym: SymbolId| unit_func_symbols.is_none_or(|set| set.contains(&sym));
        let declarations = unit_func_symbols
            .map(|definitions| {
                crate::cgu::dependencies::declaration_closure(
                    program,
                    symbols,
                    type_info,
                    definitions,
                )
            })
            .transpose()?;
        let needs_declaration =
            |sym: SymbolId| declarations.as_ref().is_none_or(|set| set.contains(&sym));
        let mut func_ids = FxHashMap::default();
        let default_call_conv = self.module.isa().default_call_conv();
        let ptr_type = self.module.target_config().pointer_type();
        let target_abi = target_abi_for_triple(self.module.isa().triple());
        let pointer_width = ptr_type.bytes() as u64;
        let classifier = TargetAbiClassifier::new(target_abi, pointer_width);

        let mut runtime_imports = runtime_imports(default_call_conv, ptr_type)?;
        let eager_runtime = unit_func_symbols.is_none();
        if eager_runtime {
            runtime_imports.declare_all(&mut self.module, &mut func_ids)?;
        }
        if block_coverage.is_some() {
            let mut signature = cranelift_codegen::ir::Signature::new(default_call_conv);
            signature.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            signature.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            signature.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("arandu_smith_record_block_hit", Linkage::Import, &signature)
                .map_err(|err| {
                    codegen_ice(format!("failed to declare AMIR coverage callback: {err:?}"))
                })?;
            func_ids.insert("arandu_smith_record_block_hit".to_owned(), id);
        }

        // 1. Declare all functions first to support cross-calls
        for func in &program.funcs {
            if !needs_declaration(func.symbol) {
                continue;
            }
            let sym = symbols.get(func.symbol);
            let host_name = symbols.host_func_name(sym);
            let param_types: Vec<_> = func
                .params
                .iter()
                .map(|&p| {
                    type_info
                        .type_interner()
                        .resolve(func.temps[p.as_usize()].ty)
                })
                .collect();
            let ret_ty = type_info.type_interner().resolve(func.return_type);
            let sig = build_internal_signature(
                &param_types,
                &ret_ty,
                default_call_conv,
                ptr_type,
                &classifier,
                type_info.type_interner(),
                type_info,
            );

            let linkage = if is_unit_func(func.symbol) {
                Linkage::Export
            } else {
                Linkage::Import
            };

            let func_id = self
                .module
                .declare_function(host_name, linkage, &sig)
                .map_err(|err| {
                    codegen_ice(format!(
                        "failed to declare function '{}': {err:?}",
                        sym.name
                    ))
                })?;
            func_ids.insert(host_name.to_string(), func_id);

            // Also find all NamespaceMember symbols that refer to this function (by matching name ending and span)
            // and map them to the same func_id!
            for s in symbols.iter() {
                if s.kind == SymbolKind::NamespaceMember
                    && s.name.ends_with(&format!(".{}", sym.name))
                    && s.span == sym.span
                {
                    func_ids.insert(s.name.to_string(), func_id);
                }
            }
        }

        // Declare one target-native drop shim per explicitly destructible
        // GenRef payload. The shim ABI is always `(ptr) -> void`; it forwards
        // the address-owned representation to the Arandu destructor without
        // deallocating the runtime-owned payload storage.
        let mut drop_shims = std::collections::BTreeMap::new();
        for func in &program.funcs {
            if !is_unit_func(func.symbol) {
                continue;
            }
            for stmt in func.stmts.payloads.iter() {
                let arandu_middle::amir::AmirStmt::Assign { rhs, .. } = stmt else {
                    continue;
                };
                let payload_ty = match rhs {
                    arandu_middle::amir::AmirRvalue::GenInsert { payload_ty, .. }
                    | arandu_middle::amir::AmirRvalue::GenSet { payload_ty, .. }
                    | arandu_middle::amir::AmirRvalue::GenUpsert { payload_ty, .. } => *payload_ty,
                    _ => continue,
                };
                let ArType::Named(_, _) = type_info.resolve_type_id(payload_ty) else {
                    continue;
                };
                let Some(destructor_symbol) = type_info.destructor_for_type(payload_ty) else {
                    continue;
                };
                let name = crate::cgu::dependencies::drop_shim_name(symbols, destructor_symbol)
                    .ok_or_else(|| codegen_ice("drop shim destructor has no symbol"))?;
                if drop_shims.contains_key(&name) {
                    continue;
                }
                let mut signature = cranelift_codegen::ir::Signature::new(default_call_conv);
                signature
                    .params
                    .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
                let shim_id = self
                    .module
                    .declare_function(&name, Linkage::Local, &signature)
                    .map_err(|err| {
                        codegen_ice(format!("failed to declare drop shim '{name}': {err:?}"))
                    })?;
                func_ids.insert(name.clone(), shim_id);
                drop_shims.insert(name, (shim_id, destructor_symbol, signature, payload_ty));
            }
        }

        // Declare all extern functions as imports
        for (&symbol_id, (param_types, return_type)) in &program.extern_funcs {
            if !needs_declaration(symbol_id) {
                continue;
            }
            let sym = symbols.get(symbol_id);
            if func_ids.contains_key(sym.name.as_str()) {
                continue;
            }
            let c_name = sym.name.split('.').next_back().unwrap_or(&sym.name);
            // PAN / Invariant 5: do not import libc `abort`. Unix inlines a CPU
            // trap; Windows calls `ar_rt_trap` from the translator.
            if arandu_middle::IntrinsicKind::from_declared_name(c_name, sym.lang_item)
                == Some(arandu_middle::IntrinsicKind::Abort)
            {
                continue;
            }
            if !eager_runtime {
                if !runtime_imports.alias(&sym.name, c_name) {
                    let sig = build_signature_with_classifier(
                        param_types,
                        return_type,
                        default_call_conv,
                        ptr_type,
                        &classifier,
                        type_info.type_interner(),
                        type_info,
                    );
                    runtime_imports.insert(&sym.name, c_name, sig);
                    runtime_imports.alias(c_name, &sym.name);
                }
                continue;
            }
            let func_id = if let Some(&existing_id) = func_ids.get(c_name) {
                existing_id
            } else {
                let sig = build_signature_with_classifier(
                    param_types,
                    return_type,
                    default_call_conv,
                    ptr_type,
                    &classifier,
                    type_info.type_interner(),
                    type_info,
                );
                self.module
                    .declare_function(c_name, Linkage::Import, &sig)
                    .map_err(|err| {
                        codegen_ice(format!(
                            "failed to declare extern function '{}': {err:?}",
                            c_name
                        ))
                    })?
            };
            func_ids.insert(sym.name.to_string(), func_id);
            if c_name != sym.name {
                func_ids.insert(c_name.to_string(), func_id);
            }
        }

        // Builtin prelude host imports (fat-pointer `str` args).
        let str_ty = ArType::Primitive(Primitive::Str);
        let void_ty = ArType::Void;
        let err_ty = ArType::Err;
        if !func_ids.contains_key("io.print") && !runtime_imports.contains("io.print") {
            let sig = build_signature_with_classifier(
                std::slice::from_ref(&str_ty),
                &void_ty,
                default_call_conv,
                ptr_type,
                &classifier,
                type_info.type_interner(),
                type_info,
            );
            runtime_imports.insert("io.print", "io.print", sig);
        }
        if !func_ids.contains_key("io.println") && !runtime_imports.contains("io.println") {
            let sig = build_signature(
                std::slice::from_ref(&str_ty),
                &void_ty,
                default_call_conv,
                ptr_type,
            );
            runtime_imports.insert("io.println", "io.println", sig);
        }
        if !func_ids.contains_key("eprint")
            && !func_ids.contains_key("io.eprint")
            && !runtime_imports.contains("eprint")
            && !runtime_imports.contains("io.eprint")
        {
            let sig = build_signature(
                std::slice::from_ref(&str_ty),
                &void_ty,
                default_call_conv,
                ptr_type,
            );
            runtime_imports.insert("io.eprint", "eprint", sig.clone());
            runtime_imports.insert("eprint", "eprint", sig);
        }
        // `err.new(str) -> Err` (Err = message pointer handle).
        if !func_ids.contains_key("err.new") && !runtime_imports.contains("err.new") {
            let sig = build_signature(
                std::slice::from_ref(&str_ty),
                &err_ty,
                default_call_conv,
                ptr_type,
            );
            runtime_imports.insert("err.new", "err.new", sig);
        }

        if eager_runtime {
            runtime_imports.declare_all(&mut self.module, &mut func_ids)?;
        }
        let gen_shutdown = eager_runtime || gen_shutdown_required(program, symbols, type_info);

        // 2. Define/compile each function
        let mut context = self.module.make_context();

        for (function_index, func) in program.funcs.iter().enumerate() {
            if !is_unit_func(func.symbol) {
                continue;
            }
            let mut builder_context = FunctionBuilderContext::new();
            let sym = symbols.get(func.symbol);
            let host_name = symbols.host_func_name(sym);
            let func_id = func_ids[host_name];

            let param_types: Vec<_> = func
                .params
                .iter()
                .map(|&p| {
                    type_info
                        .type_interner()
                        .resolve(func.temps[p.as_usize()].ty)
                })
                .collect();
            let ret_ty = type_info.type_interner().resolve(func.return_type);
            let sig = build_internal_signature(
                &param_types,
                &ret_ty,
                default_call_conv,
                ptr_type,
                &classifier,
                type_info.type_interner(),
                type_info,
            );
            context.func.signature = sig;

            if self.debug.is_some() {
                context.func.dfg.collect_debug_info();
            }

            {
                let builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
                let debug_locations = self.debug.as_mut().map(|debug| &mut debug.source_locations);
                let mut translator = FunctionTranslator::new(
                    builder,
                    &mut self.module,
                    symbols,
                    &func_ids,
                    &runtime_imports,
                    gen_shutdown,
                    ptr_type,
                    &program.literal_pool,
                    func,
                    type_info,
                    debug_locations,
                    &program.debug_bindings,
                    if let Some(coverage) = block_coverage {
                        let function_index = u32::try_from(function_index).map_err(|_| {
                            codegen_ice("AMIR function index exceeds the JIT coverage ABI")
                        })?;
                        Some((
                            func_ids["arandu_smith_record_block_hit"],
                            function_index,
                            coverage.id(),
                        ))
                    } else {
                        None
                    },
                );
                translator.translate()?;
            }

            self.module
                .define_function(func_id, &mut context)
                .map_err(|err| {
                    codegen_ice(format!("failed to define function '{}': {err:?}", sym.name))
                })?;
            if let Some(debug) = &mut self.debug
                && let Some(code) = context.compiled_code()
            {
                let ranges = code
                    .buffer
                    .get_srclocs_sorted()
                    .iter()
                    .filter_map(|range| {
                        debug
                            .source_locations
                            .get(range.loc.bits() as usize)
                            .copied()
                            .map(|span| DebugCodeRange {
                                start: range.start,
                                end: range.end,
                                span,
                            })
                    })
                    .collect();
                let locals = func
                    .locals
                    .iter()
                    .filter_map(|local| {
                        let symbol = local.symbol?;
                        let label = u32::try_from(local.id.as_usize()).ok()?;
                        let label = cranelift_codegen::ir::ValueLabel::from_u32(label);
                        let value_ranges = code.value_labels_ranges.get(&label)?;
                        let ranges = value_ranges
                            .iter()
                            .filter_map(|range| {
                                let location = match range.loc {
                                    cranelift_codegen::LabelValueLoc::Reg(register) => self
                                        .module
                                        .isa()
                                        .map_regalloc_reg_to_dwarf(register)
                                        .ok()
                                        .map(DebugValueLocation::Register)?,
                                    cranelift_codegen::LabelValueLoc::CFAOffset(offset) => {
                                        DebugValueLocation::CfaOffset(offset)
                                    }
                                };
                                Some(DebugValueRange {
                                    start: range.start,
                                    end: range.end,
                                    location,
                                })
                            })
                            .collect::<Vec<_>>();
                        (!ranges.is_empty()).then_some(DebugLocal {
                            symbol,
                            ty: local.ty,
                            span: local.span,
                            is_parameter: func.params.iter().any(|parameter| {
                                program.debug_bindings.iter().any(|binding| {
                                    binding.function == func.symbol
                                        && binding.temp == *parameter
                                        && binding.local == local.id
                                })
                            }),
                            ranges,
                        })
                    })
                    .collect();
                debug.functions.push(DebugFunction {
                    func_id,
                    symbol: func.symbol,
                    code_size: code.code_info().total_size,
                    ranges,
                    locals,
                });
            }
            self.module.clear_context(&mut context);
        }

        for (name, (shim_id, destructor_symbol, signature, payload_ty)) in drop_shims {
            let destructor = symbols.get(destructor_symbol);
            let destructor_name = symbols.host_func_name(destructor);
            let Some(&destructor_id) = func_ids.get(destructor_name) else {
                return Err(codegen_ice(format!(
                    "drop shim '{name}' references unavailable destructor '{}'",
                    destructor.name
                )));
            };
            context.func.signature = signature;
            let mut builder_context = FunctionBuilderContext::new();
            {
                use cranelift_codegen::ir::InstBuilder;
                let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
                let entry = builder.create_block();
                builder.append_block_params_for_function_params(entry);
                builder.switch_to_block(entry);
                builder.seal_block(entry);
                let raw = builder.block_params(entry)[0];
                let destructor = self
                    .module
                    .declare_func_in_func(destructor_id, builder.func);
                let arg_abi = classifier.classify_type(
                    &type_info.type_interner().resolve(payload_ty),
                    type_info.type_interner(),
                    type_info,
                );
                match arg_abi {
                    arandu_middle::layout::ArgAbi::ZeroSized => {
                        builder.ins().call(destructor, &[]);
                    }
                    arandu_middle::layout::ArgAbi::Direct(direct) => {
                        let mut call_args = Vec::with_capacity(direct.slots.len());
                        let payload = type_info.type_interner().resolve(payload_ty);
                        let layout =
                            arandu_middle::layout::LayoutEngine::new(u64::from(ptr_type.bytes()))
                                .layout_of_type(&payload, type_info.type_interner(), type_info)
                                .map_err(|error| {
                                    codegen_ice(format!("invalid drop shim layout: {error}"))
                                })?;
                        let little = self.module.isa().endianness()
                            == cranelift_codegen::ir::Endianness::Little;
                        for abi_slot in &direct.slots {
                            let chunk_val = crate::abi::load_aggregate_slot(
                                &mut builder,
                                raw,
                                abi_slot,
                                layout.size,
                                little,
                            )
                            .map_err(|message| {
                                codegen_ice(format!("invalid drop shim '{name}': {message}"))
                            })?;
                            call_args.push(chunk_val);
                        }
                        builder.ins().call(destructor, &call_args);
                    }
                    arandu_middle::layout::ArgAbi::Indirect => {
                        builder.ins().call(destructor, &[raw]);
                    }
                }
                builder.ins().return_(&[]);
                builder.seal_all_blocks();
            }
            self.module
                .define_function(shim_id, &mut context)
                .map_err(|err| {
                    codegen_ice(format!("failed to define drop shim '{name}': {err:?}"))
                })?;
            self.module.clear_context(&mut context);
        }

        Ok(func_ids)
    }
}
