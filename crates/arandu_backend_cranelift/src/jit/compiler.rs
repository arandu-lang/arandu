//! Two-phase module compilation for JIT and AOT targets.

use arandu_semantics::amir::AmirProgram;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use arandu_semantics::{Diagnostic, SymbolId, SymbolKind, SymbolTable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::JITModule;
use cranelift_module::{FuncId, Linkage, Module};
use rustc_hash::FxHashMap;

use super::block_coverage::BlockCoverageSession;
use super::builder::{
    create_jit_builder, create_jit_builder_with_io_and_process_args,
    create_jit_builder_with_io_println, create_jit_builder_with_io_println_and_args_len,
    create_jit_builder_with_process_args, register_block_coverage_symbol,
};
use super::execution::CompiledModule;
use super::isa::codegen_ice;
use super::symbols::{gen_shutdown_required, runtime_imports};
use crate::abi::{
    build_internal_signature, build_signature, build_signature_with_classifier,
    target_abi_for_triple,
};
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
    pub(crate) debug: Option<DebugCompilation>,
}

#[derive(Debug, Default)]
pub(crate) struct DebugCompilation {
    pub source_locations: Vec<arandu_base::span::Span>,
    pub functions: Vec<DebugFunction>,
}

#[derive(Debug)]
pub(crate) struct DebugFunction {
    pub func_id: FuncId,
    pub symbol: SymbolId,
    pub code_size: u32,
    pub ranges: Vec<DebugCodeRange>,
    pub locals: Vec<DebugLocal>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DebugCodeRange {
    pub start: u32,
    pub end: u32,
    pub span: arandu_base::span::Span,
}

#[derive(Debug)]
pub(crate) struct DebugLocal {
    pub symbol: SymbolId,
    pub ty: arandu_semantics::types::TypeId,
    pub span: arandu_base::span::Span,
    pub is_parameter: bool,
    pub ranges: Vec<DebugValueRange>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DebugValueRange {
    pub start: u32,
    pub end: u32,
    pub location: DebugValueLocation,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum DebugValueLocation {
    Register(u16),
    CfaOffset(i64),
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
        Ok(Self {
            module,
            debug: None,
        })
    }

    /// Creates a JIT with a caller-provided implementation of the prelude
    /// `io.println(str)` host import.
    pub fn try_new_with_io_println(
        io_println: extern "C" fn(*const u8, i64),
    ) -> Result<Self, Diagnostic> {
        let builder = create_jit_builder_with_io_println(io_println as *const u8)?;
        let module = JITModule::new(builder);
        Ok(Self {
            module,
            debug: None,
        })
    }

    /// Creates a JIT with caller-provided `io.println` and `std.env.argsLen`
    /// host imports.
    pub fn try_new_with_io_println_and_args_len(
        io_println: extern "C" fn(*const u8, i64),
        args_len: extern "C" fn() -> i64,
    ) -> Result<Self, Diagnostic> {
        let builder = create_jit_builder_with_io_println_and_args_len(
            io_println as *const u8,
            args_len as *const u8,
        )?;
        let module = JITModule::new(builder);
        Ok(Self {
            module,
            debug: None,
        })
    }

    /// Creates a JIT with caller-provided process-argument host callbacks.
    pub fn try_new_with_process_args(
        io_println: extern "C" fn(*const u8, i64),
        args_len: extern "C" fn() -> i64,
        arg: crate::EnvArgHandler,
    ) -> Result<Self, Diagnostic> {
        let builder = create_jit_builder_with_process_args(
            io_println as *const u8,
            args_len as *const u8,
            arg as *const u8,
        )?;
        let module = JITModule::new(builder);
        Ok(Self {
            module,
            debug: None,
        })
    }

    /// Creates a JIT with caller-provided stdout, stderr, and process-argument
    /// host callbacks.
    pub fn try_new_with_io_and_process_args(
        io_println: extern "C" fn(*const u8, i64),
        io_print: extern "C" fn(*const u8, i64),
        io_eprint: extern "C" fn(*const u8, i64),
        args_len: extern "C" fn() -> i64,
        arg: crate::EnvArgHandler,
    ) -> Result<Self, Diagnostic> {
        let builder = create_jit_builder_with_io_and_process_args(
            io_println as *const u8,
            io_print as *const u8,
            io_eprint as *const u8,
            args_len as *const u8,
            arg as *const u8,
        )?;
        let module = JITModule::new(builder);
        Ok(Self {
            module,
            debug: None,
        })
    }

    /// Creates a JIT with caller-provided I/O and opt-in block profiling.
    pub fn try_new_with_block_coverage_and_io_and_process_args(
        io_println: extern "C" fn(*const u8, i64),
        io_print: extern "C" fn(*const u8, i64),
        io_eprint: extern "C" fn(*const u8, i64),
        args_len: extern "C" fn() -> i64,
        arg: crate::EnvArgHandler,
    ) -> Result<Self, Diagnostic> {
        let mut builder = create_jit_builder_with_io_and_process_args(
            io_println as *const u8,
            io_print as *const u8,
            io_eprint as *const u8,
            args_len as *const u8,
            arg as *const u8,
        )?;
        register_block_coverage_symbol(&mut builder);
        let module = JITModule::new(builder);
        Ok(Self {
            module,
            debug: None,
        })
    }

    /// Compile and finalize a callable host JIT module.
    pub fn compile_program(
        mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &arandu_semantics::TypeInfo,
    ) -> Result<CompiledModule, Diagnostic> {
        let mut func_ids = self.compile_module(program, symbols, type_info)?;
        self.adapt_host_results(program, symbols, type_info, &mut func_ids)?;
        self.module
            .finalize_definitions()
            .map_err(|err| codegen_ice(format!("failed to finalize JIT definitions: {err:?}")))?;
        Ok(CompiledModule::new(self.module, func_ids, None))
    }

    /// Compiles a host JIT module with opt-in reporting of executed AMIR blocks.
    pub fn compile_program_with_block_coverage(
        mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &arandu_semantics::TypeInfo,
    ) -> Result<CompiledModule, Diagnostic> {
        let coverage = BlockCoverageSession::new(program)
            .ok_or_else(|| codegen_ice("AMIR block coverage session ID space exhausted"))?;
        let mut func_ids =
            self.compile_module_with_block_coverage(program, symbols, type_info, &coverage)?;
        self.adapt_host_results(program, symbols, type_info, &mut func_ids)?;
        self.module
            .finalize_definitions()
            .map_err(|err| codegen_ice(format!("failed to finalize JIT definitions: {err:?}")))?;
        Ok(CompiledModule::new(self.module, func_ids, Some(coverage)))
    }

    /// JIT lookups retain the host ABI. Internal calls have already been
    /// compiled against caller-owned result destinations and must not be
    /// redirected through these allocating adapters.
    fn adapt_host_results(
        &mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        info: &arandu_semantics::TypeInfo,
        functions: &mut FxHashMap<String, FuncId>,
    ) -> Result<(), Diagnostic> {
        use cranelift_codegen::ir::InstBuilder;
        let pointer = self.module.target_config().pointer_type();
        let convention = self.module.isa().default_call_conv();
        let classifier = TargetAbiClassifier::new(
            target_abi_for_triple(self.module.isa().triple()),
            u64::from(pointer.bytes()),
        );
        let layout = arandu_semantics::layout::LayoutEngine::new(u64::from(pointer.bytes()));
        let mut context = self.module.make_context();
        for function in &program.funcs {
            let symbol = symbols.get(function.symbol);
            let name = symbols.host_func_name(symbol);
            let result = info.resolve_type_id(function.return_type);
            let parameters: Vec<_> = function
                .params
                .iter()
                .map(|parameter| info.resolve_type_id(function.temps[parameter.as_usize()].ty))
                .collect();
            let host = build_signature_with_classifier(
                &parameters,
                &result,
                convention,
                pointer,
                &classifier,
                &info.type_interner,
                info,
            );
            let internal = build_internal_signature(
                &parameters,
                &result,
                convention,
                pointer,
                &classifier,
                &info.type_interner,
                info,
            );
            if host == internal {
                continue;
            }
            // The only differing contract is an indirect aggregate result.
            if host.returns.len() != 1 || host.returns[0].value_type != pointer {
                return Err(codegen_ice("unsupported JIT host result adapter"));
            }
            let size = layout
                .layout_of_type(&result, &info.type_interner, info)
                .map_err(|error| codegen_ice(format!("JIT host result layout: {error:?}")))?
                .size
                .max(1);
            let size = i64::try_from(size)
                .map_err(|_| codegen_ice("JIT host result exceeds allocator range"))?;
            let implementation = functions
                .get(name)
                .copied()
                .ok_or_else(|| codegen_ice("missing JIT host result implementation"))?;
            let malloc = functions
                .get("malloc")
                .copied()
                .ok_or_else(|| codegen_ice("missing JIT host result allocator"))?;
            let adapter = self
                .module
                .declare_anonymous_function(&host)
                .map_err(|error| {
                    codegen_ice(format!("declare JIT host result adapter: {error:?}"))
                })?;
            context.func.signature = host;
            let mut builder_context = FunctionBuilderContext::new();
            {
                let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
                let entry = builder.create_block();
                builder.append_block_params_for_function_params(entry);
                builder.switch_to_block(entry);
                let arguments = builder.block_params(entry).to_vec();
                let allocator = self.module.declare_func_in_func(malloc, builder.func);
                let bytes = builder.ins().iconst(pointer, size);
                let allocation = builder.ins().call(allocator, &[bytes]);
                let destination = builder.inst_results(allocation)[0];
                let mut arguments = arguments;
                arguments.insert(0, destination);
                let target = self
                    .module
                    .declare_func_in_func(implementation, builder.func);
                builder.ins().call(target, &arguments);
                builder.ins().return_(&[destination]);
                builder.seal_all_blocks();
                builder.finalize(self.module.target_config());
            }
            self.module
                .define_function(adapter, &mut context)
                .map_err(|error| {
                    codegen_ice(format!("define JIT host result adapter: {error:?}"))
                })?;
            self.module.clear_context(&mut context);
            for id in functions.values_mut() {
                if *id == implementation {
                    *id = adapter;
                }
            }
        }
        Ok(())
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

    pub(crate) fn compile_module_with_block_coverage(
        &mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &arandu_semantics::TypeInfo,
        coverage: &BlockCoverageSession,
    ) -> Result<FxHashMap<String, FuncId>, Diagnostic> {
        self.compile_filtered_module_inner(program, symbols, type_info, None, Some(coverage))
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
        self.compile_filtered_module_inner(program, symbols, type_info, unit_func_symbols, None)
    }

    fn compile_filtered_module_inner(
        &mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &arandu_semantics::TypeInfo,
        unit_func_symbols: Option<&[SymbolId]>,
        block_coverage: Option<&BlockCoverageSession>,
    ) -> Result<FxHashMap<String, FuncId>, Diagnostic> {
        if let Some(issue) =
            arandu_semantics::validate_amir_program(program, symbols, &type_info.type_interner)
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
                .map(|&p| type_info.type_interner.resolve(func.temps[p.as_usize()].ty))
                .collect();
            let ret_ty = type_info.type_interner.resolve(func.return_type);
            let sig = build_internal_signature(
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
            // PAN / Invariant 5: abort intrinsics are inlined to native CPU traps without importing libc abort.
            if arandu_semantics::IntrinsicKind::from_declared_name(c_name, sym.lang_item)
                == Some(arandu_semantics::IntrinsicKind::Abort)
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
                        &type_info.type_interner,
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
        if !func_ids.contains_key("io.print") && !runtime_imports.contains("io.print") {
            let sig = build_signature_with_classifier(
                std::slice::from_ref(&str_ty),
                &void_ty,
                default_call_conv,
                ptr_type,
                &classifier,
                &type_info.type_interner,
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
                .map(|&p| type_info.type_interner.resolve(func.temps[p.as_usize()].ty))
                .collect();
            let ret_ty = type_info.type_interner.resolve(func.return_type);
            let sig = build_internal_signature(
                &param_types,
                &ret_ty,
                default_call_conv,
                ptr_type,
                &classifier,
                &type_info.type_interner,
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
                        let payload = type_info.type_interner.resolve(payload_ty);
                        let layout = arandu_semantics::layout::LayoutEngine::new(u64::from(
                            ptr_type.bytes(),
                        ))
                        .layout_of_type(&payload, &type_info.type_interner, type_info)
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
