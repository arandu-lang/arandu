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
pub use crate::jit::{BlockCoverage, BlockCoverageHit};
pub use cranelift_object::object;
pub use target_lexicon::{Architecture as TargetArchitecture, Triple};

/// Host callback ABI used by the JIT for `std.env.arg(index)`.
#[cfg(not(windows))]
pub type EnvArgHandler = unsafe extern "C" fn(isize) -> arandu_runtime::rt_runtime::ArFatStr;

/// Host callback ABI used by the JIT for `std.env.arg(index)` on Windows.
#[cfg(windows)]
pub type EnvArgHandler = unsafe extern "sysv64" fn(isize) -> arandu_runtime::rt_runtime::ArFatStr;

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

    /// Creates a JIT whose `io.println(str)` import calls `io_println`.
    ///
    /// The callback receives a pointer and byte length using the Arandu string
    /// ABI, and the JIT calls it synchronously on the thread executing the
    /// generated program. The default [`Self::try_new`] remains connected to
    /// the host stdout implementation.
    pub fn try_new_with_io_println(
        io_println: extern "C" fn(*const u8, i64),
    ) -> Result<Self, Diagnostic> {
        Ok(Self {
            jit: AranduJit::try_new_with_io_println(io_println)?,
        })
    }

    /// Creates a JIT with test-controlled `io.println` and `std.env.argsLen`
    /// host imports.
    ///
    /// The argument-count callback follows `std.env.argsLen`: it includes the
    /// executable path as element zero. This is useful for deterministic
    /// execution harnesses that need to model a process invocation.
    pub fn try_new_with_io_println_and_args_len(
        io_println: extern "C" fn(*const u8, i64),
        args_len: extern "C" fn() -> i64,
    ) -> Result<Self, Diagnostic> {
        Ok(Self {
            jit: AranduJit::try_new_with_io_println_and_args_len(io_println, args_len)?,
        })
    }

    /// Creates a JIT with caller-provided `io.println`, `std.env.argsLen`, and
    /// `std.env.arg` host imports.
    pub fn try_new_with_process_args(
        io_println: extern "C" fn(*const u8, i64),
        args_len: extern "C" fn() -> i64,
        arg: EnvArgHandler,
    ) -> Result<Self, Diagnostic> {
        Ok(Self {
            jit: AranduJit::try_new_with_process_args(io_println, args_len, arg)?,
        })
    }

    /// Creates a JIT with caller-provided `io.println`, `io.eprint`,
    /// `std.env.argsLen`, and `std.env.arg` host imports.
    pub fn try_new_with_io_and_process_args(
        io_println: extern "C" fn(*const u8, i64),
        io_eprint: extern "C" fn(*const u8, i64),
        args_len: extern "C" fn() -> i64,
        arg: EnvArgHandler,
    ) -> Result<Self, Diagnostic> {
        Ok(Self {
            jit: AranduJit::try_new_with_io_and_process_args(io_println, io_eprint, args_len, arg)?,
        })
    }

    /// Creates a JIT with caller-provided I/O and block coverage enabled.
    pub fn try_new_with_block_coverage_and_io_and_process_args(
        io_println: extern "C" fn(*const u8, i64),
        io_eprint: extern "C" fn(*const u8, i64),
        args_len: extern "C" fn() -> i64,
        arg: EnvArgHandler,
    ) -> Result<Self, Diagnostic> {
        Ok(Self {
            jit: AranduJit::try_new_with_block_coverage_and_io_and_process_args(
                io_println, io_eprint, args_len, arg,
            )?,
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

    /// Compiles `program` with opt-in runtime recording of entered AMIR blocks.
    ///
    /// Use [`CompiledModule::take_block_coverage`] after execution to retrieve
    /// hits from all threads sharing this module. Normal compilation through
    /// [`Self::compile`] emits no coverage calls.
    pub fn compile_with_block_coverage(
        self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &TypeInfo,
    ) -> Result<CompiledModule, Diagnostic> {
        self.jit
            .compile_program_with_block_coverage(program, symbols, type_info)
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
