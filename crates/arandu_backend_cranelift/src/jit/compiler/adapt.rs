use arandu_semantics::amir::AmirProgram;
use arandu_semantics::layout::TargetAbiClassifier;
use arandu_semantics::{Diagnostic, SymbolTable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::JITModule;
use cranelift_module::{FuncId, Module};
use rustc_hash::FxHashMap;

use super::super::isa::codegen_ice;
use super::AranduModule;
use crate::abi::{
    build_internal_signature, build_signature_with_classifier, target_abi_for_triple,
};

impl AranduModule<JITModule> {
    /// JIT lookups retain the host ABI. Internal calls have already been
    /// compiled against caller-owned result destinations and must not be
    /// redirected through these allocating adapters.
    pub(super) fn adapt_host_results(
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
