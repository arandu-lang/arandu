//! Ahead-of-time emission of relocatable native object files.
//!
//! This backend deliberately shares AMIR lowering with the JIT. Its target ISA
//! uses Cranelift's baseline feature set for the selected triple instead of
//! probing the build machine, so an artifact never silently inherits host-only
//! CPU instructions.

use crate::debug::{self, DebugSource};
use crate::jit::{AranduModule, codegen_ice};
use arandu_semantics::amir::AmirProgram;
use arandu_semantics::{Diagnostic, SymbolTable, TypeInfo};
use std::str::FromStr;

use cranelift_codegen::settings::{self, Configurable};
use cranelift_module::default_libcall_names;
use cranelift_object::{ObjectBuilder, ObjectModule};
use target_lexicon::{OperatingSystem, Triple};

/// AOT target triple for a data-layout pointer width (in bytes).
///
/// A width matching the host maps to [`Triple::host`]. Width `4` maps to the
/// host OS's 32-bit x86 triple (i686). Returns `None` for widths that have no
/// AOT encoding on this host OS (32-bit on macOS, or an exotic width).
#[must_use]
pub fn aot_triple_for_pointer_width(pointer_width: u64) -> Option<Triple> {
    if pointer_width == std::mem::size_of::<*const ()>() as u64 {
        return Some(Triple::host());
    }
    if pointer_width != 4 {
        return None;
    }
    match Triple::host().operating_system {
        OperatingSystem::Windows => Triple::from_str("i686-pc-windows-msvc").ok(),
        OperatingSystem::Linux => Triple::from_str("i686-unknown-linux-gnu").ok(),
        // macOS (and other hosts) dropped 32-bit ABIs; no cross target here.
        _ => None,
    }
}

/// A relocatable object emitted for a specific target triple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectArtifact {
    bytes: Vec<u8>,
    target: Triple,
}

impl ObjectArtifact {
    /// Returns the complete object-file payload.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the artifact and returns the object-file payload.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the target triple encoded by the object format and ISA.
    pub fn target(&self) -> &Triple {
        &self.target
    }
}

/// Cranelift AOT backend that emits one relocatable object file.
pub struct CraneliftObjectBackend {
    target: Triple,
    compiler: AranduModule<ObjectModule>,
}

/// Code-generation policy for native object emission.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AotOptimization {
    /// Minimize compiler latency for the edit/build cycle.
    #[default]
    Baseline,
    /// Ask Cranelift to optimize generated machine code for execution speed.
    Speed,
}

impl AotOptimization {
    fn cranelift_value(self) -> &'static str {
        match self {
            Self::Baseline => "none",
            Self::Speed => "speed",
        }
    }
}

impl CraneliftObjectBackend {
    /// Creates a baseline backend for the current Rust host target.
    ///
    /// "Baseline" is important here: unlike the interactive JIT, this path
    /// does not call `cranelift_native`, which could select instructions only
    /// available on the machine performing the build.
    pub fn host_baseline() -> Result<Self, Diagnostic> {
        Self::for_target(Triple::host())
    }

    /// Creates a speed-optimized backend for release builds on the host.
    pub fn host_release() -> Result<Self, Diagnostic> {
        Self::for_target_with_optimization(Triple::host(), AotOptimization::Speed)
    }

    /// Creates a baseline backend for `target`.
    ///
    /// A target is accepted only when this Cranelift build contains its ISA.
    /// Linking and target runtime selection are intentionally later stages.
    pub fn for_target(target: Triple) -> Result<Self, Diagnostic> {
        Self::for_target_with_optimization(target, AotOptimization::Baseline)
    }

    /// Creates a backend for `target` using an explicit code-generation policy.
    pub fn for_target_with_optimization(
        target: Triple,
        optimization: AotOptimization,
    ) -> Result<Self, Diagnostic> {
        let mut flag_builder = settings::builder();
        for (key, value) in [
            ("enable_verifier", "true"),
            ("use_colocated_libcalls", "false"),
            (
                "is_pic",
                if target.operating_system == target_lexicon::OperatingSystem::Windows {
                    "false"
                } else {
                    "true"
                },
            ),
            ("opt_level", optimization.cranelift_value()),
            ("preserve_frame_pointers", "true"),
            // PAN / Invariant 5: zero-metadata runtime without stack unwinding tables (.eh_frame).
            ("unwind_info", "false"),
        ] {
            flag_builder.set(key, value).map_err(|err| {
                codegen_ice(format!(
                    "failed to configure Cranelift AOT flag {key}={value}: {err}"
                ))
            })?;
        }

        let isa_builder = cranelift_codegen::isa::lookup(target.clone()).map_err(|err| {
            codegen_ice(format!(
                "Cranelift does not support AOT target '{target}': {err}"
            ))
        })?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to build baseline ISA for AOT target '{target}': {err}"
                ))
            })?;
        let builder = ObjectBuilder::new(isa, b"arandu".to_vec(), default_libcall_names())
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to create object module for target '{target}': {err}"
                ))
            })?;

        Ok(Self {
            target,
            compiler: AranduModule {
                module: ObjectModule::new(builder),
                debug: None,
            },
        })
    }

    /// Lowers `program` and emits a relocatable native object.
    pub fn compile(
        self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &TypeInfo,
    ) -> Result<ObjectArtifact, Diagnostic> {
        self.compile_impl(program, symbols, type_info, &[])
    }

    /// Lowers `program` and emits a DWARF v5 object using the provided source
    /// closure. Source files are shared by `Arc`; their text is never copied.
    pub fn compile_with_debug_sources(
        mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &TypeInfo,
        sources: &[DebugSource],
    ) -> Result<ObjectArtifact, Diagnostic> {
        self.compiler.debug = Some(Default::default());
        self.compile_impl(program, symbols, type_info, sources)
    }

    fn compile_impl(
        mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &TypeInfo,
        sources: &[DebugSource],
    ) -> Result<ObjectArtifact, Diagnostic> {
        self.compiler.compile_module(program, symbols, type_info)?;
        let debug_compilation = self.compiler.debug.take();
        let mut product = self.compiler.module.finish();
        if let Some(debug_compilation) = debug_compilation {
            debug::emit_dwarf(
                &mut product,
                &debug_compilation,
                sources,
                symbols,
                type_info,
                self.target.endianness(),
            )?;
        }
        let bytes = product.emit().map_err(|err| {
            codegen_ice(format!(
                "failed to serialize object for target '{}': {err}",
                self.target
            ))
        })?;

        Ok(ObjectArtifact {
            bytes,
            target: self.target,
        })
    }

    /// Consumes the backend and returns the underlying compiler context.
    pub fn into_compiler(self) -> AranduModule<ObjectModule> {
        self.compiler
    }
}
