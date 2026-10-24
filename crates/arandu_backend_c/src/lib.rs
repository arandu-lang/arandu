//! C backend: AMIR → portable C translation unit.
//!
//! Target layout is explicit via [`arandu_middle::layout::DataLayout`]. Use
//! [`emit_c`] with [`LayoutEngine::host`] for host parity or
//! [`DataLayout::i686_sysv`] / [`DataLayout::ptr_width`] for cross targets.
//!
//! [`CEmitBackend`] adapts the same emission to the backend-agnostic
//! [`CodegenBackend`] contract from `arandu_codegen`.

pub mod emitter;

pub use emitter::CEmitter;

use arandu_codegen::{CodegenBackend, CompiledCode};
use arandu_middle::Diagnostic;
use arandu_middle::SymbolTable;
use arandu_middle::amir::AmirProgram;
use arandu_middle::layout::{DataLayout, LayoutEngine, StructLayoutProvider};
use arandu_middle::types::TypeInterner;

/// Emit a full C translation unit for `program` under `data_layout`.
///
/// Cranelift does not use this path; call with [`DataLayout::host`] only when
/// comparing host C to host JIT. For 32-bit / i686 portable C, pass the
/// matching [`DataLayout`] (emit-only unless a matching C toolchain is used).
pub fn emit_c(
    program: &AmirProgram,
    symbols: &SymbolTable,
    provider: &dyn StructLayoutProvider,
    interner: &TypeInterner,
    data_layout: DataLayout,
) -> Result<String, Diagnostic> {
    if let Some(issue) = arandu_middle::validate_amir_program(program, symbols, interner)
        .into_iter()
        .next()
    {
        return Err(issue);
    }
    let engine = LayoutEngine::from_data_layout(data_layout);
    CEmitter::new(program, symbols, &engine, provider, interner).emit()
}

/// [`CodegenBackend`] adapter around [`emit_c`].
///
/// Holds the explicit target [`DataLayout`]; the type information supplied as
/// `TargetConfig` provides both the struct-layout provider and the interner,
/// mirroring how the CLI calls [`emit_c`] today.
pub struct CEmitBackend {
    data_layout: DataLayout,
}

impl CEmitBackend {
    #[must_use]
    pub fn new(data_layout: DataLayout) -> Self {
        Self { data_layout }
    }
}

/// Source-level compilation output: a full C translation unit.
#[derive(Debug, Clone)]
pub struct CTranslationUnit(pub String);

impl CompiledCode for CTranslationUnit {
    unsafe fn get_fn<F>(&self, _name: &str) -> Option<F> {
        // Source output carries no callable handles; execution parity comes
        // from compiling the unit with an external C toolchain (see
        // `tests/parity_tests.rs`).
        None
    }
}

impl CodegenBackend for CEmitBackend {
    type TargetConfig = arandu_semantics::TypeInfo;
    type CompilationOutput = CTranslationUnit;

    fn compile(
        self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        config: &Self::TargetConfig,
    ) -> Result<Self::CompilationOutput, Diagnostic> {
        emit_c(
            program,
            symbols,
            config,
            &config.type_interner,
            self.data_layout,
        )
        .map(CTranslationUnit)
    }
}
