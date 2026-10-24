use arandu_middle::Diagnostic;
use cranelift_codegen::ir::types::{F64, I32, I64};
use cranelift_codegen::ir::{AbiParam, Signature, Type};
use cranelift_codegen::isa::CallConv;
use cranelift_module::Linkage;
use rustc_hash::FxHashMap;

use super::codegen_ice;
use super::insert_sym;

pub(super) fn declare_memory_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // Declare malloc as import
    let mut malloc_sig = Signature::new(default_call_conv);
    malloc_sig.params.push(AbiParam::new(ptr_type));
    malloc_sig.returns.push(AbiParam::new(ptr_type));
    let malloc_id = module
        .declare_function("ar_rt_raw_malloc", Linkage::Import, &malloc_sig)
        .map_err(|err| codegen_ice(format!("failed to declare malloc: {err:?}")))?;
    insert_sym(func_ids, "malloc", malloc_id);

    // Declare free as import
    let mut free_sig = Signature::new(default_call_conv);
    free_sig.params.push(AbiParam::new(ptr_type));
    let free_id = module
        .declare_function("ar_rt_raw_free", Linkage::Import, &free_sig)
        .map_err(|err| codegen_ice(format!("failed to declare free: {err:?}")))?;
    insert_sym(func_ids, "free", free_id);

    // PAN / Invariant 5: `abort` and `abortGenerationalMismatch` are compiler intrinsics.
    // Unix lowers them to a hardware trap (UD2 / BRK) with no libc `abort` import.
    // Windows JIT frames have no unwind info for that trap and report a stack
    // overflow on stderr, so abort calls `ar_rt_trap` (`ExitProcess`) instead.
    #[cfg(windows)]
    {
        let trap_sig = Signature::new(default_call_conv);
        let id = module
            .declare_function("ar_rt_trap", Linkage::Import, &trap_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_rt_trap: {err:?}")))?;
        insert_sym(func_ids, "ar_rt_trap", id);
    }
    Ok(())
}

pub(super) fn declare_libc_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // Libc imports: fmod, memcpy, memmove, memcmp
    let mut fmod_sig = Signature::new(default_call_conv);
    fmod_sig.params.push(AbiParam::new(F64));
    fmod_sig.params.push(AbiParam::new(F64));
    fmod_sig.returns.push(AbiParam::new(F64));
    let fmod_id = module
        .declare_function("fmod", Linkage::Import, &fmod_sig)
        .map_err(|err| codegen_ice(format!("failed to declare fmod: {err:?}")))?;
    insert_sym(func_ids, "fmod", fmod_id);

    let mut memcpy_sig = Signature::new(default_call_conv);
    memcpy_sig.params.push(AbiParam::new(ptr_type));
    memcpy_sig.params.push(AbiParam::new(ptr_type));
    memcpy_sig.params.push(AbiParam::new(ptr_type));
    memcpy_sig.returns.push(AbiParam::new(ptr_type));
    let memcpy_id = module
        .declare_function("memcpy", Linkage::Import, &memcpy_sig)
        .map_err(|err| codegen_ice(format!("failed to declare memcpy: {err:?}")))?;
    insert_sym(func_ids, "memcpy", memcpy_id);

    let memmove_id = module
        .declare_function("memmove", Linkage::Import, &memcpy_sig)
        .map_err(|err| codegen_ice(format!("failed to declare memmove: {err:?}")))?;
    insert_sym(func_ids, "memmove", memmove_id);

    let mut memcmp_sig = Signature::new(default_call_conv);
    memcmp_sig.params.push(AbiParam::new(ptr_type));
    memcmp_sig.params.push(AbiParam::new(ptr_type));
    memcmp_sig.params.push(AbiParam::new(ptr_type));
    memcmp_sig.returns.push(AbiParam::new(I32));
    let memcmp_id = module
        .declare_function("memcmp", Linkage::Import, &memcmp_sig)
        .map_err(|err| codegen_ice(format!("failed to declare memcmp: {err:?}")))?;
    insert_sym(func_ids, "memcmp", memcmp_id);

    for name in ["ar_rt_i64_write_digits", "ar_rt_u64_write_digits"] {
        let mut sig = Signature::new(default_call_conv);
        sig.params.push(AbiParam::new(I64));
        sig.params.push(AbiParam::new(ptr_type));
        sig.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
        insert_sym(func_ids, name, id);
    }
    Ok(())
}
