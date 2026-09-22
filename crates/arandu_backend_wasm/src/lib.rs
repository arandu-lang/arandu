//! WebAssembly backend for the Arandu compiler.
//!
//! Translates a fully optimized [`AmirProgram`] directly to WebAssembly binary
//! bytecode via `wasm-encoder`, without LLVM or Emscripten intermediaries.
//!
//! # Architecture
//!
//! ```text
//! AmirProgram
//!   └── WasmModuleBuilder (emit.rs)
//!         ├── Type section      (types.rs)
//!         ├── Memory section    (memory.rs)
//!         ├── Code section
//!         │     └── per func: FuncTranslator (translator/mod.rs)
//!         │           ├── stackify CFG (stackify.rs)
//!         │           └── emit rvalues (translator/expr.rs)
//!         └── Export section
//! ```
//!
//! # Invariants (RFC 0014 §5.1)
//! - Pure consumer of Salsa results — no queries executed here.
//! - Generation is deterministic: same `(AmirProgram, DataLayout)` → identical
//!   bytes (reproducible builds, BLAKE3 content addressing).
//! - All error paths use [`Diagnostic::ice`]; no `panic!`, `unwrap`, or
//!   `expect` in production code.
//! - `DataLayout::ptr_width(4)` is the canonical target for wasm32.

pub mod canonical;
pub mod emit;
pub mod memory;
pub mod stackify;
pub mod translator;
pub mod types;
pub mod wit_gen;

use arandu_codegen::{CodegenBackend, CompiledCode};
use arandu_middle::Diagnostic;
use arandu_middle::amir::AmirProgram;
use arandu_middle::layout::{DataLayout, StructLayoutProvider};
use arandu_middle::types::TypeInterner;
use arandu_semantics::SymbolTable;

use crate::emit::{WasmModuleBuilder, validate_place_ops};
pub use emit::{is_component, is_core_module, validate_magic};

/// First diagnostic from the shared pre-translation validation passes.
///
/// Both entry points reject a program that violates the AMIR contract or uses a
/// place operation the wasm memory model cannot represent, before any code is
/// emitted.
fn first_validation_error(
    program: &AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
) -> Option<Diagnostic> {
    arandu_middle::validate_amir_program(program, symbols, interner)
        .into_iter()
        .next()
        .or_else(|| {
            validate_place_ops(program, symbols, interner)
                .into_iter()
                .next()
        })
}

/// Emit a WebAssembly core module binary from `program`.
///
/// Uses `DataLayout::ptr_width(4)` (wasm32) by default; pass an explicit
/// `data_layout` to override. `provider` supplies structural layout metadata
/// (struct fields, enum variants) flowing from `type_check`; pass the same
/// `TypeInfo` instance whose interner produced `program`'s types.
///
/// # Errors
/// Returns a [`Diagnostic`] on ICE / unsupported program structure.
pub fn emit_wasm(
    program: &AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    data_layout: DataLayout,
) -> Result<Vec<u8>, Diagnostic> {
    if let Some(issue) = first_validation_error(program, symbols, interner) {
        return Err(issue);
    }
    WasmModuleBuilder::new(program, symbols, interner, provider, data_layout).build()
}

/// Emit a WebAssembly **Component** binary from `program`.
///
/// The component wraps the core module with a WIT-derived interface, allowing
/// Component Model hosts (Wasmtime, browsers, etc.) to exchange strings, lists,
/// and structs without JavaScript glue code.
///
/// `pkg_name` is the WIT package name (e.g. `"my-app"`).  If the program has
/// no public functions, the plain core module is returned.
///
/// # Errors
/// Returns a [`Diagnostic`] on ICE / unsupported program structure or if the
/// component encoding step fails.
pub fn emit_component(
    program: &AmirProgram,
    symbols: &SymbolTable,
    interner: &TypeInterner,
    provider: &dyn StructLayoutProvider,
    data_layout: DataLayout,
    pkg_name: &str,
) -> Result<Vec<u8>, Diagnostic> {
    if let Some(issue) = first_validation_error(program, symbols, interner) {
        return Err(issue);
    }
    WasmModuleBuilder::new(program, symbols, interner, provider, data_layout)
        .build_component(pkg_name)
}

/// Opaque handle for the compiled Wasm bytes.
///
/// Wasm bytecode cannot be called in-process; use `WasmModule::bytes()` to
/// obtain the raw binary for embedding or writing to disk.
#[derive(Debug, Clone)]
pub struct WasmModule(pub Vec<u8>);

impl WasmModule {
    /// Access the raw WebAssembly bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl CompiledCode for WasmModule {
    unsafe fn get_fn<F>(&self, _name: &str) -> Option<F> {
        None
    }
}

/// [`CodegenBackend`] adapter for the Wasm emitter.
///
/// `TargetConfig` is [`arandu_semantics::TypeInfo`] to match the other
/// backends. The `data_layout` field must be `DataLayout::ptr_width(4)` for
/// wasm32 targets.
pub struct WasmEmitBackend {
    pub data_layout: DataLayout,
}

impl WasmEmitBackend {
    /// Create a new backend with the canonical wasm32 layout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            data_layout: DataLayout::ptr_width(4),
        }
    }
}

impl Default for WasmEmitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CodegenBackend for WasmEmitBackend {
    type TargetConfig = arandu_semantics::TypeInfo;
    type CompilationOutput = WasmModule;

    fn compile(
        self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        config: &Self::TargetConfig,
    ) -> Result<Self::CompilationOutput, Diagnostic> {
        emit_wasm(
            program,
            symbols,
            &config.type_interner,
            config,
            self.data_layout,
        )
        .map(WasmModule)
    }
}
