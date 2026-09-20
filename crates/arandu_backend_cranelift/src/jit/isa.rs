//! Process-global host ISA configuration and ICE diagnostics for Cranelift codegen.

use std::sync::OnceLock;

use arandu_base::span::Span;
use arandu_semantics::{DiagCode, Diagnostic};
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable};

pub(crate) fn codegen_ice(message: impl Into<String>) -> Diagnostic {
    Diagnostic::ice(DiagCode::ICEGEN001, message, Span::new(0, 0, 0))
}

/// Host ISA is process-global and immutable. Building it once avoids re-running
/// `cranelift_native` + flag setup on every `run` / compile (debug: tens of ms).
pub(crate) fn cached_host_isa() -> Result<OwnedTargetIsa, Diagnostic> {
    static ISA: OnceLock<Result<OwnedTargetIsa, String>> = OnceLock::new();
    match ISA.get_or_init(|| {
        let mut flag_builder = settings::builder();
        for (key, val) in [
            ("use_colocated_libcalls", "false"),
            ("is_pic", "false"),
            // Fastest compile for interactive JIT; release optimizers live elsewhere.
            ("opt_level", "none"),
            // PAN / Invariant 5: zero-metadata runtime without stack unwinding tables.
            ("unwind_info", "false"),
        ] {
            if let Err(e) = flag_builder.set(key, val) {
                return Err(format!("failed to set Cranelift flag {key}={val}: {e}"));
            }
        }
        let isa_builder = match cranelift_native::builder() {
            Ok(b) => b,
            Err(e) => return Err(format!("Failed to create Cranelift isa builder: {e}")),
        };
        match isa_builder.finish(settings::Flags::new(flag_builder)) {
            Ok(isa) => Ok(isa),
            Err(e) => Err(format!("Failed to build Cranelift isa: {e}")),
        }
    }) {
        Ok(isa) => Ok(std::sync::Arc::clone(isa)),
        Err(msg) => Err(codegen_ice(msg.clone())),
    }
}
