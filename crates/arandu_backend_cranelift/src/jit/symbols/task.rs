use arandu_semantics::Diagnostic;
use cranelift_codegen::ir::types::{I32, I64};
use cranelift_codegen::ir::{AbiParam, Signature, Type};
use cranelift_codegen::isa::CallConv;
use cranelift_module::Linkage;
use rustc_hash::FxHashMap;

use super::codegen_ice;
use super::insert_sym;

pub(super) fn declare_coroutine_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // A3.6: block_on(state) -> i64
    let mut block_on_sig = Signature::new(default_call_conv);
    block_on_sig.params.push(AbiParam::new(ptr_type));
    block_on_sig.returns.push(AbiParam::new(I64));
    let block_on_id = module
        .declare_function("ar_co_block_on_i64", Linkage::Import, &block_on_sig)
        .map_err(|err| codegen_ice(format!("failed to declare ar_co_block_on_i64: {err:?}")))?;
    insert_sym(func_ids, "ar_co_block_on_i64", block_on_id);

    // A3.6: poll(state, *out) -> i32
    let mut poll_sig = Signature::new(default_call_conv);
    poll_sig.params.push(AbiParam::new(ptr_type));
    poll_sig.params.push(AbiParam::new(ptr_type));
    poll_sig.returns.push(AbiParam::new(I32));
    let poll_id = module
        .declare_function("ar_co_poll_i64", Linkage::Import, &poll_sig)
        .map_err(|err| codegen_ice(format!("failed to declare ar_co_poll_i64: {err:?}")))?;
    insert_sym(func_ids, "ar_co_poll_i64", poll_id);

    // A3.6 / SL_R tests: make_ready(payload:i64) -> *u8
    let mut make_ready_sig = Signature::new(default_call_conv);
    make_ready_sig.params.push(AbiParam::new(I64));
    make_ready_sig.returns.push(AbiParam::new(ptr_type));
    let make_ready_id = module
        .declare_function("ar_co_make_ready_i64", Linkage::Import, &make_ready_sig)
        .map_err(|err| codegen_ice(format!("failed to declare ar_co_make_ready_i64: {err:?}")))?;
    insert_sym(func_ids, "ar_co_make_ready_i64", make_ready_id);
    Ok(())
}

pub(super) fn declare_task_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // Threads, Condvars, and Entropy
    {
        let mut sleep_sig = Signature::new(default_call_conv);
        sleep_sig.params.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_thread_sleep_ms", Linkage::Import, &sleep_sig)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_rt_thread_sleep_ms: {err:?}"))
            })?;
        insert_sym(func_ids, "ar_rt_thread_sleep_ms", id);

        let id = module
            .declare_function("ar_rt_thread_park_timeout", Linkage::Import, &sleep_sig)
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to declare ar_rt_thread_park_timeout: {err:?}"
                ))
            })?;
        insert_sym(func_ids, "ar_rt_thread_park_timeout", id);

        let void_noarg = Signature::new(default_call_conv);
        for name in ["ar_rt_thread_yield", "ar_rt_thread_park"] {
            let id = module
                .declare_function(name, Linkage::Import, &void_noarg)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut get_id_sig = Signature::new(default_call_conv);
        get_id_sig.returns.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_rt_thread_current_id", Linkage::Import, &get_id_sig)
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to declare ar_rt_thread_current_id: {err:?}"
                ))
            })?;
        insert_sym(func_ids, "ar_rt_thread_current_id", id);

        let mut wait_sig = Signature::new(default_call_conv);
        wait_sig.params.push(AbiParam::new(ptr_type));
        wait_sig.params.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_rt_condvar_wait", Linkage::Import, &wait_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_rt_condvar_wait: {err:?}")))?;
        insert_sym(func_ids, "ar_rt_condvar_wait", id);

        let mut timeout_sig = wait_sig.clone();
        timeout_sig.params.push(AbiParam::new(I64));
        timeout_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_rt_condvar_wait_timeout", Linkage::Import, &timeout_sig)
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to declare ar_rt_condvar_wait_timeout: {err:?}"
                ))
            })?;
        insert_sym(func_ids, "ar_rt_condvar_wait_timeout", id);

        let mut void_ptr = Signature::new(default_call_conv);
        void_ptr.params.push(AbiParam::new(ptr_type));
        for name in [
            "ar_rt_thread_unpark",
            "ar_rt_condvar_notify_one",
            "ar_rt_condvar_notify_all",
        ] {
            let id = module
                .declare_function(name, Linkage::Import, &void_ptr)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut entropy_sig = Signature::new(default_call_conv);
        entropy_sig.params.push(AbiParam::new(ptr_type));
        entropy_sig.params.push(AbiParam::new(ptr_type));
        entropy_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_rt_get_entropy", Linkage::Import, &entropy_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_rt_get_entropy: {err:?}")))?;
        insert_sym(func_ids, "ar_rt_get_entropy", id);
    }
    Ok(())
}
