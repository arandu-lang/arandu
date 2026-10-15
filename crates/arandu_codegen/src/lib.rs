//! Backend-agnostic codegen contracts (the `rustc_codegen_ssa` role in this
//! workspace).
//!
//! Owns the trait interface every Arandu codegen backend implements —
//! [`CodegenBackend`] turns a validated AMIR program into a backend-specific
//! artifact, and [`CompiledCode`] is how callers obtain entry points from that
//! artifact. Concrete backends live in `arandu_backend_cranelift` (host JIT)
//! and `arandu_backend_c` (portable C emission); neither owns this contract.

use arandu_middle::amir::AmirProgram;
use arandu_middle::diagnostics::Diagnostic;
use arandu_middle::symbol_table::SymbolTable;

pub mod string_interp;
pub mod temp_slots;
pub mod testing;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JitError {
    NotFound,
    SignatureMismatch { expected: String, actual: String },
}

pub trait CompiledCode {
    /// # Safety
    /// The caller must ensure that the function signature (F) matches
    /// exactly the signature of the compiled function.
    unsafe fn get_fn<F>(&self, name: &str) -> Option<F>;
}

pub trait CodegenBackend {
    type TargetConfig;
    type CompilationOutput: CompiledCode;

    fn compile(
        self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        config: &Self::TargetConfig,
    ) -> Result<Self::CompilationOutput, Diagnostic>;
}
