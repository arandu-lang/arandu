//! Cranelift native-code backend for Arandu.
//!
//! Exposes [`CraneliftBackend`] which implements the [`CodegenBackend`] trait.
//! The backend compiles an [`AmirProgram`] to native machine code in memory
//! via Cranelift and returns a [`CompiledModule`] whose functions can be
//! called directly through raw function pointers.
//!
//! Host runtime routines (SL_R) live in `arandu_runtime`; this crate registers
//! them as JIT imports through the re-exports below, so existing
//! `arandu_backend_cranelift::<module>_runtime` paths keep working.

pub mod abi;
pub mod aot;
pub mod cgu;
mod debug;
pub mod jit;
pub mod translator;
pub mod types;

// Host runtime routines moved to `arandu_runtime`; re-exported so the JIT
// symbol table (`crate::<module>_runtime::*`) and external paths keep working.
pub use arandu_runtime::{
    fs_runtime, gen_runtime, os_runtime, poll_runtime, reactor_runtime, rt_runtime, socket_runtime,
    supervisor_runtime, testing_runtime, to_str_runtime, vec_runtime, waker_runtime,
};

pub use crate::aot::{
    AotOptimization, CraneliftObjectBackend, ObjectArtifact, aot_triple_for_pointer_width,
};
pub use crate::cgu::{CodegenUnit, compile_cgu, compute_cgu_hash, partition_program};
pub use crate::debug::DebugSource;
pub use crate::jit::CompiledModule;
pub use cranelift_object::object;
pub use target_lexicon::{Architecture as TargetArchitecture, Triple};

use crate::jit::AranduJit;
use arandu_codegen::{CodegenBackend, CompiledCode};
use arandu_semantics::amir::AmirProgram;
use arandu_semantics::{Diagnostic, SymbolTable, TypeInfo};

/// Entry point for the Cranelift JIT backend.
///
/// Implements [`CodegenBackend`]; use [`CraneliftBackend::try_new`] and then
/// [`CraneliftBackend::compile`] to JIT-compile an [`AmirProgram`].
pub struct CraneliftBackend {
    jit: AranduJit,
}

impl CraneliftBackend {
    /// Creates a new `CraneliftBackend` with a freshly initialized JIT context.
    pub fn try_new() -> Result<Self, Diagnostic> {
        Ok(Self {
            jit: AranduJit::try_new()?,
        })
    }

    /// Compiles `program` to native code and returns the [`CompiledModule`].
    ///
    /// This is a convenience wrapper around [`CodegenBackend::compile`].
    pub fn compile(
        self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &TypeInfo,
    ) -> Result<CompiledModule, Diagnostic> {
        CodegenBackend::compile(self, program, symbols, type_info)
    }
}

impl CodegenBackend for CraneliftBackend {
    type TargetConfig = TypeInfo;
    type CompilationOutput = CompiledModule;

    fn compile(
        self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        config: &Self::TargetConfig,
    ) -> Result<Self::CompilationOutput, Diagnostic> {
        self.jit.compile_program(program, symbols, config)
    }
}

impl CompiledCode for CompiledModule {
    unsafe fn get_fn<F>(&self, name: &str) -> Option<F> {
        unsafe { self.get_fn(name) }
    }
}
