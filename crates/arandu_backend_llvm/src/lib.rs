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
            assert_eq!(llvm_type(&ty, &interner, 8).as_deref(), Ok(expected));
        }
        assert_eq!(
            llvm_type(&ArType::Primitive(Primitive::Str), &interner, 8).as_deref(),
            Ok("{ ptr, i64 }")
        );
        assert_eq!(
            llvm_type(&ArType::Primitive(Primitive::Str), &interner, 4).as_deref(),
            Ok("{ ptr, i32 }")
        );
        assert_eq!(
            llvm_type(&ArType::Primitive(Primitive::USize), &interner, 4).as_deref(),
            Ok("i32")
        );
        assert_eq!(
            llvm_type(&ArType::Primitive(Primitive::ISize), &interner, 8).as_deref(),
            Ok("i64")
        );
    }

    #[test]
    fn embedded_module_header_uses_the_arm_datalayout() {
        let program = arandu_middle::amir::AmirProgram {
            funcs: Vec::new(),
            literal_pool: Default::default(),
            extern_funcs: Default::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        let symbols = arandu_semantics::SymbolTable::default();
        let mut info = arandu_semantics::TypeInfo::new();
        info.target_identity =
            arandu_middle::db::TargetIdentity::from_triple("thumbv7em-none-eabihf")
                .expect("canonical triple");
        info.target_layout = arandu_middle::DataLayout::ptr_width(4);
        let ir = crate::emit_llvm_ir(&program, &symbols, &info).expect("empty module");
        assert!(ir.contains("target triple = \"thumbv7em-none-eabihf\"\n"));
        assert!(ir.contains(
            "target datalayout = \"e-m:e-p:32:32-Fi8-i64:64-v128:64:128-a:0:32-n32-S64\"\n"
        ));
        assert!(ir.contains("source_filename = \"arandu\"\n"));
    }

    #[test]
    fn cortex_m0_module_header_uses_the_arm_datalayout() {
        let program = arandu_middle::amir::AmirProgram {
            funcs: Vec::new(),
            literal_pool: Default::default(),
            extern_funcs: Default::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        let symbols = arandu_semantics::SymbolTable::default();
        let mut info = arandu_semantics::TypeInfo::new();
        info.target_identity = arandu_middle::db::TargetIdentity::from_triple("thumbv6m-none-eabi")
            .expect("canonical triple");
        info.target_layout = arandu_middle::DataLayout::ptr_width(4);
        let ir = crate::emit_llvm_ir(&program, &symbols, &info).expect("empty module");
        assert!(ir.contains("target triple = \"thumbv6m-none-eabi\"\n"));
        assert!(ir.contains(
            "target datalayout = \"e-m:e-p:32:32-Fi8-i64:64-v128:64:128-a:0:32-n32-S64\"\n"
        ));
    }

    #[test]
    fn host_module_header_omits_target_datalayout() {
        let program = arandu_middle::amir::AmirProgram {
            funcs: Vec::new(),
            literal_pool: Default::default(),
            extern_funcs: Default::default(),
            debug_bindings: Vec::new(),
            debug_blocks: Vec::new(),
        };
        let symbols = arandu_semantics::SymbolTable::default();
        let info = arandu_semantics::TypeInfo::new();
        let ir = crate::emit_llvm_ir(&program, &symbols, &info).expect("empty module");
        assert!(ir.contains("source_filename = \"arandu\"\n"));
        assert!(!ir.contains("target datalayout"));
        assert!(!ir.contains("target triple"));
    }
}
