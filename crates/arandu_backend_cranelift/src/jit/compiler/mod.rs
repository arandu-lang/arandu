//! Two-phase module compilation for JIT and AOT targets.

use arandu_middle::amir::AmirProgram;
use arandu_middle::{Diagnostic, SymbolId, SymbolTable};
use cranelift_jit::JITModule;
use cranelift_module::{FuncId, Module};
use rustc_hash::FxHashMap;

use super::block_coverage::BlockCoverageSession;
use super::builder::{
    create_jit_builder, create_jit_builder_with_io_and_process_args,
    create_jit_builder_with_io_println, create_jit_builder_with_io_println_and_args_len,
    create_jit_builder_with_process_args, register_block_coverage_symbol,
};
use super::execution::CompiledModule;
use super::isa::codegen_ice;

mod adapt;
mod compile;

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
    pub ty: arandu_middle::types::TypeId,
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
        type_info: &dyn arandu_middle::type_context::CodegenTypeContext,
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
        type_info: &dyn arandu_middle::type_context::CodegenTypeContext,
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
        type_info: &dyn arandu_middle::type_context::CodegenTypeContext,
    ) -> Result<FxHashMap<String, FuncId>, Diagnostic> {
        self.compile_filtered_module(program, symbols, type_info, None)
    }

    pub(crate) fn compile_module_with_block_coverage(
        &mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &dyn arandu_middle::type_context::CodegenTypeContext,
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
        type_info: &dyn arandu_middle::type_context::CodegenTypeContext,
        unit_func_symbols: Option<&[SymbolId]>,
    ) -> Result<FxHashMap<String, FuncId>, Diagnostic> {
        self.compile_filtered_module_inner(program, symbols, type_info, unit_func_symbols, None)
    }
}
