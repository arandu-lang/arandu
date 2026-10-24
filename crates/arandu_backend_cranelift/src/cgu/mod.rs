//! Partitioned Ahead-Of-Time (AOT) Code Generation Units (CGUs).
//!
//! A CGU digest is a versioned, canonical encoding of machine-code inputs. It
//! deliberately excludes source spans and diagnostics, while including the
//! compiler identity, target, optimization policy, AMIR, concrete layouts and
//! direct-callee signatures.

use crate::aot::{AotOptimization, CraneliftObjectBackend};
use crate::jit::isa::codegen_ice;
use arandu_middle::amir::{AmirFunc, AmirProgram};
use arandu_middle::type_context::CodegenTypeContext;
use arandu_middle::{Diagnostic, SymbolId, SymbolTable};
use target_lexicon::Triple;

pub(crate) mod dependencies;

mod hash_func;
mod hash_rvalue;
mod hash_stmt;
mod hash_type;

const CGU_HASH_SCHEMA: &[u8] = b"arandu-cgu-input-v5\0";
const CRANELIFT_IDENTITY: &[u8] = b"cranelift-0.136.1\0";

/// A discrete compilation unit corresponding to one function or item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenUnit {
    /// Human-readable identity of this unit (e.g. `main` or `math_add`).
    pub name: String,
    /// Canonical `SymbolId` of the primary function.
    pub symbol: SymbolId,
    /// BLAKE3 digest of every machine-code input represented by this CGU.
    pub hash: String,
}

/// Partition `program` into individual [`CodegenUnit`]s with deterministic hashes.
pub fn partition_program(
    program: &AmirProgram,
    symbols: &SymbolTable,
    type_info: &dyn CodegenTypeContext,
    target: &Triple,
    optimization: AotOptimization,
    toolchain_fingerprint: &str,
) -> Vec<CodegenUnit> {
    let mut units = Vec::with_capacity(program.funcs.len());

    for func in &program.funcs {
        let name = symbols.try_get(func.symbol).map_or_else(
            || format!("symbol_{}_{}", func.symbol.file_id, func.symbol.local_id.0),
            |symbol| symbols.host_func_name(symbol).to_owned(),
        );
        let hash = compute_cgu_hash(
            func,
            program,
            symbols,
            type_info,
            target,
            optimization,
            toolchain_fingerprint,
        );
        units.push(CodegenUnit {
            name,
            symbol: func.symbol,
            hash,
        });
    }

    units
}

/// Compute a stable digest for one function's complete machine-code inputs.
pub fn compute_cgu_hash(
    func: &AmirFunc,
    program: &AmirProgram,
    symbols: &SymbolTable,
    type_info: &dyn CodegenTypeContext,
    target: &Triple,
    optimization: AotOptimization,
    toolchain_fingerprint: &str,
) -> String {
    let mut hash = StableHash::new();
    hash.bytes(CGU_HASH_SCHEMA);
    hash.bytes(CRANELIFT_IDENTITY);
    hash.str(toolchain_fingerprint);
    hash.str(&target.to_string());
    hash.tag(match optimization {
        AotOptimization::Baseline => 0,
        AotOptimization::Speed => 1,
        AotOptimization::Size => 2,
    });

    let mut context = HashContext {
        hash: &mut hash,
        program,
        symbols,
        type_info,
        named_type_stack: Vec::new(),
    };
    context.func(func);
    context.direct_callee_signatures(func);
    if symbols
        .try_get(func.symbol)
        .is_some_and(|symbol| symbol.name == "main")
    {
        context
            .hash
            .bool(crate::jit::symbols::gen_shutdown_required(
                program, symbols, type_info,
            ));
    }
    // Object emission uses this same function-local declaration closure.
    // ABI/layout and implicit destructor dependencies remain in the digest;
    // unrelated declarations no longer affect object metadata or the key.
    hash.finish()
}

struct StableHash(blake3::Hasher);

impl StableHash {
    fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    fn tag(&mut self, value: u8) {
        self.0.update(&[value]);
    }

    fn bool(&mut self, value: bool) {
        self.tag(u8::from(value));
    }

    fn u32(&mut self, value: u32) {
        self.0.update(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(&value.to_le_bytes());
    }

    fn usize(&mut self, value: usize) {
        match u64::try_from(value) {
            Ok(value) => {
                self.tag(0);
                self.u64(value);
            }
            Err(_) => self.tag(1),
        }
    }

    fn i128(&mut self, value: i128) {
        self.0.update(&value.to_le_bytes());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.usize(value.len());
        self.0.update(value);
    }

    fn str(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn finish(self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

struct HashContext<'a> {
    hash: &'a mut StableHash,
    program: &'a AmirProgram,
    symbols: &'a SymbolTable,
    type_info: &'a dyn CodegenTypeContext,
    named_type_stack: Vec<SymbolId>,
}

/// Compile a single [`CodegenUnit`] into an in-memory relocatable object.
pub fn compile_cgu(
    unit: &CodegenUnit,
    program: &AmirProgram,
    symbols: &SymbolTable,
    type_info: &dyn CodegenTypeContext,
    target: &Triple,
    optimization: AotOptimization,
) -> Result<Vec<u8>, Diagnostic> {
    let backend =
        CraneliftObjectBackend::for_target_with_optimization(target.clone(), optimization)?;
    let mut compiler = backend.into_compiler();

    compiler.compile_filtered_module(program, symbols, type_info, Some(&[unit.symbol]))?;

    let product = compiler.module.finish();
    product.emit().map_err(|error| {
        codegen_ice(format!(
            "failed to emit CGU object '{}' for target '{target}': {error}",
            unit.name
        ))
    })
}
