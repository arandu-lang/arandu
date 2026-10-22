use arandu_semantics::Diagnostic;
use cranelift_codegen::ir::types::I64;
use cranelift_codegen::ir::{AbiParam, Signature, Type};
use cranelift_codegen::isa::CallConv;
use cranelift_module::Linkage;
use rustc_hash::FxHashMap;

use super::codegen_ice;
use super::insert_sym;

pub(super) fn declare_vec_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // Vec host
    {
        let mut noarg_i64 = Signature::new(default_call_conv);
        noarg_i64.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_vec_new", Linkage::Import, &noarg_i64)
            .map_err(|err| codegen_ice(format!("failed to declare ar_vec_new: {err:?}")))?;
        insert_sym(func_ids, "ar_vec_new", id);

        let mut one_i64 = Signature::new(default_call_conv);
        one_i64.params.push(AbiParam::new(I64));
        for name in ["ar_vec_destroy", "ar_vec_clear"] {
            let id = module
                .declare_function(name, Linkage::Import, &one_i64)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut one_ret = Signature::new(default_call_conv);
        one_ret.params.push(AbiParam::new(I64));
        one_ret.returns.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_vec_len", Linkage::Import, &one_ret)
            .map_err(|err| codegen_ice(format!("failed to declare ar_vec_len: {err:?}")))?;
        insert_sym(func_ids, "ar_vec_len", id);

        let mut two = Signature::new(default_call_conv);
        two.params.push(AbiParam::new(I64));
        two.params.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_vec_push", Linkage::Import, &two)
            .map_err(|err| codegen_ice(format!("failed to declare ar_vec_push: {err:?}")))?;
        insert_sym(func_ids, "ar_vec_push", id);

        let mut two_ret = Signature::new(default_call_conv);
        two_ret.params.push(AbiParam::new(I64));
        two_ret.params.push(AbiParam::new(I64));
        two_ret.returns.push(AbiParam::new(I64));
        for name in ["ar_vec_has", "ar_vec_get"] {
            let id = module
                .declare_function(name, Linkage::Import, &two_ret)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut three_ret = Signature::new(default_call_conv);
        three_ret.params.push(AbiParam::new(I64));
        three_ret.params.push(AbiParam::new(I64));
        three_ret.params.push(AbiParam::new(I64));
        three_ret.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_vec_put", Linkage::Import, &three_ret)
            .map_err(|err| codegen_ice(format!("failed to declare ar_vec_put: {err:?}")))?;
        insert_sym(func_ids, "ar_vec_put", id);

        let mut pop_sig = Signature::new(default_call_conv);
        pop_sig.params.push(AbiParam::new(I64));
        pop_sig.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_vec_pop", Linkage::Import, &pop_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_vec_pop: {err:?}")))?;
        insert_sym(func_ids, "ar_vec_pop", id);
    }

    // Raw buffer helpers for pure-Arandu Vec
    {
        let mut malloc_sig = Signature::new(default_call_conv);
        malloc_sig.params.push(AbiParam::new(ptr_type));
        malloc_sig.returns.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_vec_malloc", Linkage::Import, &malloc_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_vec_malloc: {err:?}")))?;
        insert_sym(func_ids, "ar_vec_malloc", id);

        let mut free_sig = Signature::new(default_call_conv);
        free_sig.params.push(AbiParam::new(ptr_type));
        free_sig.params.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_vec_buf_free", Linkage::Import, &free_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_vec_buf_free: {err:?}")))?;
        insert_sym(func_ids, "ar_vec_buf_free", id);

        let mut copy_value_sig = Signature::new(default_call_conv);
        copy_value_sig.params.push(AbiParam::new(ptr_type));
        copy_value_sig.params.push(AbiParam::new(ptr_type));
        copy_value_sig.params.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_rt_copy_value", Linkage::Import, &copy_value_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_rt_copy_value: {err:?}")))?;
        insert_sym(func_ids, "ar_rt_copy_value", id);

        let mut aligned_alloc_sig = Signature::new(default_call_conv);
        aligned_alloc_sig.params.push(AbiParam::new(ptr_type));
        aligned_alloc_sig.params.push(AbiParam::new(ptr_type));
        aligned_alloc_sig.returns.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_rt_alloc_aligned", Linkage::Import, &aligned_alloc_sig)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_rt_alloc_aligned: {err:?}"))
            })?;
        insert_sym(func_ids, "ar_rt_alloc_aligned", id);

        let mut aligned_free_sig = Signature::new(default_call_conv);
        aligned_free_sig.params.push(AbiParam::new(ptr_type));
        aligned_free_sig.params.push(AbiParam::new(ptr_type));
        aligned_free_sig.params.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_rt_free_aligned", Linkage::Import, &aligned_free_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_rt_free_aligned: {err:?}")))?;
        insert_sym(func_ids, "ar_rt_free_aligned", id);

        let mut realloc_sig = Signature::new(default_call_conv);
        realloc_sig.params.push(AbiParam::new(ptr_type));
        realloc_sig.params.push(AbiParam::new(ptr_type));
        realloc_sig.params.push(AbiParam::new(ptr_type));
        realloc_sig.returns.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_vec_realloc", Linkage::Import, &realloc_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_vec_realloc: {err:?}")))?;
        insert_sym(func_ids, "ar_vec_realloc", id);

        let mut push_str_sig = Signature::new(default_call_conv);
        push_str_sig.params.push(AbiParam::new(ptr_type));
        push_str_sig.params.push(AbiParam::new(ptr_type));
        push_str_sig.params.push(AbiParam::new(ptr_type));
        push_str_sig
            .returns
            .push(AbiParam::new(cranelift_codegen::ir::types::I8));
        let id = module
            .declare_function("ar_string_push_str", Linkage::Import, &push_str_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_string_push_str: {err:?}")))?;
        insert_sym(func_ids, "ar_string_push_str", id);
    }
    Ok(())
}
