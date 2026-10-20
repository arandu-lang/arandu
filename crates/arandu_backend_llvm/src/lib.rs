//! Pure Rust, direct textual LLVM IR emission from validated Arandu AMIR.
//!
//! The initial implementation supports scalar SSA functions, CFG block
//! parameters, direct calls, and a conservative subset of arithmetic. It does
//! not invoke Clang or LLVM; the resulting `.ll` can be inspected or passed to
//! an LLVM toolchain by the caller.

mod codegen;
mod types;
mod writer;

use arandu_codegen::{CodegenBackend, CompiledCode};
use arandu_middle::Diagnostic;
use arandu_middle::amir::AmirProgram;
use arandu_semantics::{SymbolTable, TypeInfo};

pub use codegen::emit_llvm_ir;

/// Codegen contract adapter for callers that want LLVM IR text.
pub struct LlvmBackend;

impl LlvmBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for LlvmBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// LLVM IR text has no in-process callable function handles.
pub struct LlvmModule(pub String);

impl CompiledCode for LlvmModule {
    unsafe fn get_fn<F>(&self, _name: &str) -> Option<F> {
        None
    }
}

impl CodegenBackend for LlvmBackend {
    type TargetConfig = TypeInfo;
    type CompilationOutput = LlvmModule;

    fn compile(
        self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        config: &Self::TargetConfig,
    ) -> Result<Self::CompilationOutput, Diagnostic> {
        emit_llvm_ir(program, symbols, config).map(LlvmModule)
    }
}

#[cfg(test)]
mod tests {
    use arandu_middle::types::{ArType, Primitive, TypeInterner, VectorType};

    use crate::types::llvm_type;

    #[test]
    fn maps_requested_scalar_and_vector_types() {
        let interner = TypeInterner::new();
        for (ty, expected) in [
            (ArType::Primitive(Primitive::Bool), "i1"),
            (ArType::Primitive(Primitive::Int), "i32"),
            (ArType::Primitive(Primitive::I64), "i64"),
            (ArType::Primitive(Primitive::Float), "double"),
            (
                ArType::Ptr(interner.intern(ArType::Primitive(Primitive::Int))),
                "ptr",
            ),
            (ArType::Vector(VectorType::I32x4), "<4 x i32>"),
            (ArType::Vector(VectorType::F32x4), "<4 x float>"),
        ] {
            assert_eq!(llvm_type(&ty, &interner).as_deref(), Ok(expected));
        }
    }
}
