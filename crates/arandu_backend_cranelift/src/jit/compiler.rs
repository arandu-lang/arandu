//! Two-phase module compilation for JIT and AOT targets.

use arandu_semantics::amir::AmirProgram;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use arandu_semantics::{Diagnostic, SymbolId, SymbolKind, SymbolTable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::JITModule;
use cranelift_module::{FuncId, Linkage, Module};
use rustc_hash::FxHashMap;

use super::builder::create_jit_builder;
use super::execution::CompiledModule;
use super::isa::codegen_ice;
use super::symbols::declare_runtime_imports;
use crate::abi::{build_signature, build_signature_with_classifier, target_abi_for_triple};
use crate::translator::FunctionTranslator;
use arandu_semantics::layout::TargetAbiClassifier;

/// Stateful Cranelift JIT context.
///
/// Wraps a [`JITModule`] and orchestrates the full compilation of an
/// [`AmirProgram`]: function declaration, translation, and memory finalization.
/// Consumed by [`AranduJit::compile_program`] — create a fresh instance for
/// each compilation.
pub struct AranduModule<M> {
    pub module: M,
}

/// Host-JIT specialization of the shared Cranelift module compiler.
pub type AranduJit = AranduModule<JITModule>;

impl AranduModule<JITModule> {
    /// Creates a new [`AranduJit`] with default Cranelift settings.
    ///
    /// Reuses a process-cached host [`cranelift_codegen::isa::OwnedTargetIsa`] (Arc clone). Each call
    /// still builds a fresh [`JITModule`] — modules cannot be reset after finalize.
    pub fn try_new() -> Result<Self, Diagnostic> {
        let builder = create_jit_builder()?;
        let module = JITModule::new(builder);
        Ok(Self { module })
    }

    /// Compile and finalize a callable host JIT module.
    pub fn compile_program(
        mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &arandu_semantics::TypeInfo,
    ) -> Result<CompiledModule, Diagnostic> {
        let func_ids = self.compile_module(program, symbols, type_info)?;
        self.module
            .finalize_definitions()
            .map_err(|err| codegen_ice(format!("failed to finalize JIT definitions: {err:?}")))?;
        Ok(CompiledModule::new(self.module, func_ids))
    }
}

impl<M: Module> AranduModule<M> {
    /// Compiles all functions in `program` to native machine code.
    ///
    /// Two-phase compilation shared by the JIT and object backends:
    /// 1. Declare all functions (enabling mutual recursion).
    /// 2. Define/translate each function body via [`FunctionTranslator`].
    ///
    /// The caller owns the module-specific finalization step.
    #[tracing::instrument(
        level = "trace",
        target = "arandu_backend_cranelift",
        skip(self, program, symbols, type_info)
    )]
    pub(crate) fn compile_module(
        &mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &arandu_semantics::TypeInfo,
    ) -> Result<FxHashMap<String, FuncId>, Diagnostic> {
        self.compile_filtered_module(program, symbols, type_info, None)
    }

    #[tracing::instrument(
        level = "trace",
        target = "arandu_backend_cranelift",
        skip(self, program, symbols, type_info)
    )]
    pub(crate) fn compile_filtered_module(
        &mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &arandu_semantics::TypeInfo,
        unit_func_symbols: Option<&[SymbolId]>,
    ) -> Result<FxHashMap<String, FuncId>, Diagnostic> {
        if let Some(issue) =
            arandu_semantics::validate_amir_program(program, symbols, &type_info.type_interner)
                .into_iter()
                .next()
        {
            return Err(issue);
        }
        let is_unit_func = |sym: SymbolId| unit_func_symbols.is_none_or(|set| set.contains(&sym));
        let mut func_ids = FxHashMap::default();
        let default_call_conv = self.module.isa().default_call_conv();
        let ptr_type = self.module.target_config().pointer_type();
        let target_abi = target_abi_for_triple(self.module.isa().triple());
        let pointer_width = ptr_type.bytes() as u64;
        let classifier = TargetAbiClassifier::new(target_abi, pointer_width);

        declare_runtime_imports(&mut self.module, &mut func_ids, default_call_conv, ptr_type)?;

        // 1. Declare all functions first to support cross-calls
        for func in &program.funcs {
            let sym = symbols.get(func.symbol);
            let host_name = symbols.host_func_name(sym);
            let param_types: Vec<_> = func
                .params
                .iter()
                .map(|&p| type_info.type_interner.resolve(func.temps[p.as_usize()].ty))
                .collect();
            let ret_ty = type_info.type_interner.resolve(func.return_type);
            let sig = build_signature_with_classifier(
                &param_types,
                &ret_ty,
                default_call_conv,
                ptr_type,
                &classifier,
                &type_info.type_interner,
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
                let arandu_semantics::amir::AmirStmt::Assign { rhs, .. } = stmt else {
                    continue;
                };
                let payload_ty = match rhs {
                    arandu_semantics::amir::AmirRvalue::GenInsert { payload_ty, .. }
                    | arandu_semantics::amir::AmirRvalue::GenSet { payload_ty, .. }
                    | arandu_semantics::amir::AmirRvalue::GenUpsert { payload_ty, .. } => {
                        *payload_ty
                    }
                    _ => continue,
                };
                let ArType::Named(_, _) = type_info.resolve_type_id(payload_ty) else {
                    continue;
                };
                let Some(&destructor_symbol) = type_info.destructor_instances.get(&payload_ty)
                else {
                    continue;
                };
                let name = format!(
                    "__ar_drop_{}_{}",
                    destructor_symbol.file_id, destructor_symbol.local_id.0
                );
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
            let sym = symbols.get(symbol_id);
            if func_ids.contains_key(sym.name.as_str()) {
                continue;
            }
            let c_name = sym.name.split('.').next_back().unwrap_or(&sym.name);
            let func_id = if let Some(&existing_id) = func_ids.get(c_name) {
                existing_id
            } else {
                let sig = build_signature_with_classifier(
                    param_types,
                    return_type,
                    default_call_conv,
                    ptr_type,
                    &classifier,
                    &type_info.type_interner,
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
        if !func_ids.contains_key("io.println") {
            let sig = build_signature(
                std::slice::from_ref(&str_ty),
                &void_ty,
                default_call_conv,
                ptr_type,
            );
            let id = self
                .module
                .declare_function("io.println", Linkage::Import, &sig)
                .map_err(|err| codegen_ice(format!("failed to declare io.println: {err:?}")))?;
            func_ids.insert("io.println".to_string(), id);
        }
        // `err.new(str) -> Err` (Err = message pointer handle).
        if !func_ids.contains_key("err.new") {
            let sig = build_signature(
                std::slice::from_ref(&str_ty),
                &err_ty,
                default_call_conv,
                ptr_type,
            );
            let id = self
                .module
                .declare_function("err.new", Linkage::Import, &sig)
                .map_err(|err| codegen_ice(format!("failed to declare err.new: {err:?}")))?;
            func_ids.insert("err.new".to_string(), id);
        }

        // 2. Define/compile each function
        let mut context = self.module.make_context();

        for func in &program.funcs {
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
                .map(|&p| type_info.type_interner.resolve(func.temps[p.as_usize()].ty))
                .collect();
            let ret_ty = type_info.type_interner.resolve(func.return_type);
            let sig = build_signature_with_classifier(
                &param_types,
                &ret_ty,
                default_call_conv,
                ptr_type,
                &classifier,
                &type_info.type_interner,
                type_info,
            );
            context.func.signature = sig;

            {
                let builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
                let mut translator = FunctionTranslator::new(
                    builder,
                    &mut self.module,
                    symbols,
                    &func_ids,
                    ptr_type,
                    &program.literal_pool,
                    func,
                    type_info,
                );
                translator.translate()?;
            }

            self.module
                .define_function(func_id, &mut context)
                .map_err(|err| {
                    codegen_ice(format!("failed to define function '{}': {err:?}", sym.name))
                })?;
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
                    &type_info.type_interner.resolve(payload_ty),
                    &type_info.type_interner,
                    type_info,
                );
                match arg_abi {
                    arandu_semantics::layout::ArgAbi::ZeroSized => {
                        builder.ins().call(destructor, &[]);
                    }
                    arandu_semantics::layout::ArgAbi::Direct(direct) => {
                        let mut call_args = Vec::with_capacity(direct.slots.len());
                        for abi_slot in &direct.slots {
                            let chunk_ty = crate::abi::abi_scalar_to_clif(abi_slot.scalar);
                            let chunk_val = builder.ins().load(
                                chunk_ty,
                                cranelift_codegen::ir::MemFlagsData::new(),
                                raw,
                                abi_slot.offset as i32,
                            );
                            call_args.push(chunk_val);
                        }
                        builder.ins().call(destructor, &call_args);
                    }
                    arandu_semantics::layout::ArgAbi::Indirect => {
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
