//! Finalized JIT execution handle and typed function lookup.

use cranelift_jit::JITModule;
use cranelift_module::{FuncId, Module};
use rustc_hash::FxHashMap;
use std::sync::Arc;

use super::block_coverage::{BlockCoverage, BlockCoverageSession};

/// The result of a successful JIT compilation.
///
/// Holds the finalized [`JITModule`] and the mapping from function names to
/// their [`FuncId`]s. Use [`CompiledModule::get_fn`] to obtain callable
/// function pointers.
pub struct CompiledModule {
    pub(crate) module: JITModule,
    pub(crate) func_ids: FxHashMap<String, FuncId>,
    block_coverage: Option<Arc<BlockCoverageSession>>,
}

impl Drop for CompiledModule {
    fn drop(&mut self) {
        // Definitions include compiler-generated thread entry/drop thunks.
        // Keep the module and coverage metadata alive through all descendants
        // and destruction of unclaimed results. External function-pointer
        // callers must already have stopped, as required by get_fn's contract.
        let definitions: std::collections::HashSet<_> = self
            .func_ids
            .values()
            .filter(|&&id| {
                self.module.declarations().get_function_decl(id).linkage
                    != cranelift_module::Linkage::Import
            })
            .map(|&id| self.module.get_finalized_function(id).addr())
            .collect();
        arandu_runtime::native_thread::join_module_threads(|callback| {
            definitions.contains(&callback)
        });
    }
}

impl CompiledModule {
    pub(crate) fn new(
        module: JITModule,
        func_ids: FxHashMap<String, FuncId>,
        block_coverage: Option<Arc<BlockCoverageSession>>,
    ) -> Self {
        Self {
            module,
            func_ids,
            block_coverage,
        }
    }

    /// Takes the bounded block trace collected by this module, if coverage was enabled.
    #[must_use]
    pub fn take_block_coverage(&self) -> Option<BlockCoverage> {
        self.block_coverage.as_ref().map(|session| session.take())
    }

    /// Returns a callable function pointer for the named function.
    ///
    /// # Safety
    /// The caller must guarantee that the type `F` exactly matches the
    /// signature of the compiled function. Mismatched signatures cause
    /// undefined behaviour. Keep this CompiledModule alive throughout every
    /// external call and stop all such calls before dropping it. Function
    /// pointers must not be invoked after module destruction. Native threads
    /// admitted by std.thread are drained separately by the module's drop.
    pub unsafe fn get_fn<F>(&self, name: &str) -> Option<F> {
        let id = self.func_ids.get(name)?;
        let ptr = self.module.get_finalized_function(*id);
        assert_eq!(
            std::mem::size_of::<F>(),
            std::mem::size_of::<*const u8>(),
            "Type F must be the size of a function pointer"
        );
        Some(unsafe { std::mem::transmute_copy(&ptr) })
    }

    /// Returns a callable function pointer for the named function, but first checks
    /// that the full signature (types and arity) matches the expected signature.
    ///
    /// # Safety
    /// The caller must still guarantee that the type `F` matches the signature's
    /// types and obey the module lifetime contract documented on [`Self::get_fn`].
    pub unsafe fn get_fn_checked<F>(
        &self,
        name: &str,
        expected_sig: &cranelift_codegen::ir::Signature,
    ) -> Result<F, arandu_codegen::JitError> {
        let id = self
            .func_ids
            .get(name)
            .ok_or(arandu_codegen::JitError::NotFound)?;
        let decl = self.module.declarations().get_function_decl(*id);
        if decl.signature != *expected_sig {
            return Err(arandu_codegen::JitError::SignatureMismatch {
                expected: format!("{:?}", expected_sig),
                actual: format!("{:?}", decl.signature),
            });
        }

        unsafe { self.get_fn::<F>(name) }.ok_or(arandu_codegen::JitError::NotFound)
    }
}
