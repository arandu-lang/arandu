use arandu_middle::Diagnostic;
use cranelift_codegen::ir::types::{F64, I8, I64};
use cranelift_codegen::ir::{AbiParam, Signature, Type};
use cranelift_codegen::isa::CallConv;
use cranelift_module::Linkage;
use rustc_hash::FxHashMap;

use super::codegen_ice;
use super::insert_sym;

pub(super) fn declare_bench_gen_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // SL_T.4 opaque optimization barriers. Signatures deliberately match
    // the machine representation selected by lowering.
    for (name, ty) in [
        ("ar_bench_black_box_i64", I64),
        ("ar_bench_black_box_f64", F64),
        ("ar_bench_black_box_ptr", ptr_type),
    ] {
        let mut signature = Signature::new(default_call_conv);
        signature.params.push(AbiParam::new(ty));
        signature.returns.push(AbiParam::new(ty));
        let id = module
            .declare_function(name, Linkage::Import, &signature)
            .map_err(|error| codegen_ice(format!("failed to declare {name}: {error:?}")))?;
        insert_sym(func_ids, name, id);
    }

    // G4 raw ABI: pointers and layout widths follow the JIT target.
    let usize_ty = ptr_type;
    let mut insert_sig = Signature::new(default_call_conv);
    for ty in [ptr_type, usize_ty, usize_ty, ptr_type] {
        insert_sig.params.push(AbiParam::new(ty));
    }
    insert_sig.returns.push(AbiParam::new(I64));

    let mut get_sig = Signature::new(default_call_conv);
    for ty in [I64, ptr_type, usize_ty, usize_ty] {
        get_sig.params.push(AbiParam::new(ty));
    }
    get_sig.returns.push(AbiParam::new(I8));

    let mut set_sig = Signature::new(default_call_conv);
    let mut upsert_sig = Signature::new(default_call_conv);
    for ty in [I64, ptr_type, usize_ty, usize_ty, ptr_type] {
        set_sig.params.push(AbiParam::new(ty));
        upsert_sig.params.push(AbiParam::new(ty));
    }
    set_sig.returns.push(AbiParam::new(I8));
    upsert_sig.returns.push(AbiParam::new(I64));

    for (name, signature) in [
        ("ar_gen_insert_raw", &insert_sig),
        ("ar_gen_get_raw", &get_sig),
        ("ar_gen_set_raw", &set_sig),
        ("ar_gen_upsert_raw", &upsert_sig),
        ("ar_gen_remove_raw", &get_sig),
    ] {
        let id = module
            .declare_function(name, Linkage::Import, signature)
            .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
        insert_sym(func_ids, name, id);
    }

    let shutdown_sig = Signature::new(default_call_conv);
    let shutdown_id = module
        .declare_function("ar_gen_shutdown_raw", Linkage::Import, &shutdown_sig)
        .map_err(|err| codegen_ice(format!("failed to declare ar_gen_shutdown_raw: {err:?}")))?;
    insert_sym(func_ids, "ar_gen_shutdown_raw", shutdown_id);
    Ok(())
}
