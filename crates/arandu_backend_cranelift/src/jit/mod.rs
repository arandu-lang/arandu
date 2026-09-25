//! Low-level Cranelift JIT compilation internals.
//!
//! [`AranduJit`] drives the Cranelift JIT module lifecycle: it declares all
//! functions, defines each one via [`crate::translator::FunctionTranslator`], finalizes the
//! in-memory compilation, and returns a [`CompiledModule`] ready for execution.

mod block_coverage;
pub mod builder;
pub mod compiler;
pub mod execution;
pub mod isa;
pub mod symbols;

pub use block_coverage::{BlockCoverage, BlockCoverageHit};
pub use compiler::{AranduJit, AranduModule};
pub use execution::CompiledModule;
pub(crate) use isa::codegen_ice;
