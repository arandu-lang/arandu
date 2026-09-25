//! Runtime function declaration and external import mapping for Cranelift modules.

use arandu_semantics::Diagnostic;
use cranelift_codegen::ir::types::{F64, I8, I32, I64};
use cranelift_codegen::ir::{AbiParam, Signature, Type};
use cranelift_codegen::isa::CallConv;
use cranelift_module::{FuncId, Linkage, Module};
use rustc_hash::FxHashMap;

use super::isa::codegen_ice;

#[inline]
fn insert_sym(func_ids: &mut FxHashMap<String, FuncId>, name: &str, id: FuncId) {
    func_ids.insert(name.to_string(), id);
}

pub(crate) fn declare_runtime_imports<M: Module>(
    module: &mut M,
    func_ids: &mut FxHashMap<String, FuncId>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // Declare malloc as import
    let mut malloc_sig = Signature::new(default_call_conv);
    malloc_sig.params.push(AbiParam::new(ptr_type));
    malloc_sig.returns.push(AbiParam::new(ptr_type));
    let malloc_id = module
        .declare_function("malloc", Linkage::Import, &malloc_sig)
        .map_err(|err| codegen_ice(format!("failed to declare malloc: {err:?}")))?;
    insert_sym(func_ids, "malloc", malloc_id);

    // Declare free as import
    let mut free_sig = Signature::new(default_call_conv);
    free_sig.params.push(AbiParam::new(ptr_type));
    let free_id = module
        .declare_function("free", Linkage::Import, &free_sig)
        .map_err(|err| codegen_ice(format!("failed to declare free: {err:?}")))?;
    insert_sym(func_ids, "free", free_id);

    // PAN / Invariant 5: `abort` and `abortGenerationalMismatch` are compiler intrinsics
    // that lower directly to native hardware traps (UD2 on x86_64, BRK on AArch64)
    // with zero unwinding metadata overhead, without importing libc abort.

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

    // SL_R.0 + SL_S path host imports
    let mut rt_block_sig = Signature::new(default_call_conv);
    rt_block_sig.params.push(AbiParam::new(ptr_type));
    rt_block_sig.returns.push(AbiParam::new(I64));
    for name in ["ar_rt_block_on_i64", "ar_co_block_on_i64"] {
        if !func_ids.contains_key(name) {
            let id = module
                .declare_function(name, Linkage::Import, &rt_block_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }
    }

    let mut rt_spawn_sig = Signature::new(default_call_conv);
    rt_spawn_sig.params.push(AbiParam::new(ptr_type));
    rt_spawn_sig.returns.push(AbiParam::new(I64));
    let name = "ar_rt_spawn_i64";
    let id = module
        .declare_function(name, Linkage::Import, &rt_spawn_sig)
        .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
    insert_sym(func_ids, name, id);

    let mut rt_join_sig = Signature::new(default_call_conv);
    rt_join_sig.params.push(AbiParam::new(I64));
    rt_join_sig.returns.push(AbiParam::new(I64));
    let name = "ar_rt_join_i64";
    let id = module
        .declare_function(name, Linkage::Import, &rt_join_sig)
        .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
    insert_sym(func_ids, name, id);

    let mut rt_cancel_sig = Signature::new(default_call_conv);
    rt_cancel_sig.params.push(AbiParam::new(I64));
    let cancel_id = module
        .declare_function("ar_rt_cancel_i64", Linkage::Import, &rt_cancel_sig)
        .map_err(|err| codegen_ice(format!("failed to declare ar_rt_cancel_i64: {err:?}")))?;
    insert_sym(func_ids, "ar_rt_cancel_i64", cancel_id);

    let mut par_fold_sig = Signature::new(default_call_conv);
    par_fold_sig.params.push(AbiParam::new(I64));
    par_fold_sig.params.push(AbiParam::new(ptr_type));
    par_fold_sig.params.push(AbiParam::new(ptr_type));
    par_fold_sig.params.push(AbiParam::new(ptr_type));
    par_fold_sig.params.push(AbiParam::new(I64));
    par_fold_sig.params.push(AbiParam::new(ptr_type));
    par_fold_sig.returns.push(AbiParam::new(I32));
    let par_fold_id = module
        .declare_function("ar_rt_parallel_fold_run", Linkage::Import, &par_fold_sig)
        .map_err(|err| {
            codegen_ice(format!(
                "failed to declare ar_rt_parallel_fold_run: {err:?}"
            ))
        })?;
    insert_sym(func_ids, "ar_rt_parallel_fold_run", par_fold_id);

    let mut path_sig = Signature::new(default_call_conv);
    path_sig.params.push(AbiParam::new(ptr_type));
    path_sig.params.push(AbiParam::new(ptr_type));
    path_sig.returns.push(AbiParam::new(ptr_type));
    for name in [
        "ar_path_is_absolute",
        "ar_path_is_empty",
        "ar_env_var_is_set",
    ] {
        let id = module
            .declare_function(name, Linkage::Import, &path_sig)
            .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
        insert_sym(func_ids, name, id);
    }

    // path join / file_name / str thin hosts
    {
        let mut join_sig = Signature::new(default_call_conv);
        #[cfg(windows)]
        {
            join_sig.call_conv = CallConv::SystemV;
        }
        for _ in 0..2 {
            join_sig.params.push(AbiParam::new(ptr_type));
            join_sig.params.push(AbiParam::new(ptr_type));
        }
        join_sig.returns.push(AbiParam::new(ptr_type));
        join_sig.returns.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_path_join", Linkage::Import, &join_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_path_join: {err:?}")))?;
        insert_sym(func_ids, "ar_path_join", id);

        let mut owned_join_sig = Signature::new(default_call_conv);
        for _ in 0..2 {
            owned_join_sig.params.push(AbiParam::new(ptr_type));
            owned_join_sig.params.push(AbiParam::new(ptr_type));
        }
        for _ in 0..3 {
            owned_join_sig.params.push(AbiParam::new(ptr_type));
        }
        owned_join_sig.returns.push(AbiParam::new(I8));
        let id = module
            .declare_function("ar_path_join_owned", Linkage::Import, &owned_join_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_path_join_owned: {err:?}")))?;
        insert_sym(func_ids, "ar_path_join_owned", id);

        let mut file_sig = Signature::new(default_call_conv);
        #[cfg(windows)]
        {
            file_sig.call_conv = CallConv::SystemV;
        }
        file_sig.params.push(AbiParam::new(ptr_type));
        file_sig.params.push(AbiParam::new(ptr_type));
        file_sig.returns.push(AbiParam::new(ptr_type));
        file_sig.returns.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_path_file_name", Linkage::Import, &file_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_path_file_name: {err:?}")))?;
        insert_sym(func_ids, "ar_path_file_name", id);

        let id = module
            .declare_function("ar_str_concat", Linkage::Import, &join_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_str_concat: {err:?}")))?;
        insert_sym(func_ids, "ar_str_concat", id);

        let id = module
            .declare_function("ar_str_split_last", Linkage::Import, &join_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_str_split_last: {err:?}")))?;
        insert_sym(func_ids, "ar_str_split_last", id);
    }

    // Minimal OS: exit(void), monotonic_ns/args_len
    {
        let mut exit_sig = Signature::new(default_call_conv);
        exit_sig.params.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_process_exit", Linkage::Import, &exit_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_process_exit: {err:?}")))?;
        insert_sym(func_ids, "ar_process_exit", id);

        let mut noarg_i64 = Signature::new(default_call_conv);
        noarg_i64.returns.push(AbiParam::new(I64));
        for name in ["ar_time_monotonic_ns", "ar_env_args_len"] {
            let id = module
                .declare_function(name, Linkage::Import, &noarg_i64)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }
    }

    // `ar_env_arg(i64) -> str`: fat-string return needs SystemV even on Windows
    // (matches the `extern "sysv64"` host in os_runtime, like ar_path_join).
    {
        let mut arg_sig = Signature::new(default_call_conv);
        #[cfg(windows)]
        {
            arg_sig.call_conv = CallConv::SystemV;
        }
        arg_sig.params.push(AbiParam::new(ptr_type));
        arg_sig.returns.push(AbiParam::new(ptr_type));
        arg_sig.returns.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function("ar_env_arg", Linkage::Import, &arg_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_env_arg: {err:?}")))?;
        insert_sym(func_ids, "ar_env_arg", id);
    }

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

    // SL_R.2 reactor host imports
    {
        let mut create_sig = Signature::new(default_call_conv);
        create_sig.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_reactor_create", Linkage::Import, &create_sig)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_rt_reactor_create: {err:?}"))
            })?;
        insert_sym(func_ids, "ar_rt_reactor_create", id);

        let mut destroy_sig = Signature::new(default_call_conv);
        destroy_sig.params.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_reactor_destroy", Linkage::Import, &destroy_sig)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_rt_reactor_destroy: {err:?}"))
            })?;
        insert_sym(func_ids, "ar_rt_reactor_destroy", id);

        let mut two_i64_ret_i64 = Signature::new(default_call_conv);
        two_i64_ret_i64.params.push(AbiParam::new(I64));
        two_i64_ret_i64.params.push(AbiParam::new(I64));
        two_i64_ret_i64.returns.push(AbiParam::new(I64));
        for name in [
            "ar_rt_reactor_sleep_ms",
            "ar_rt_reactor_arm_timer_ms",
            "ar_rt_reactor_poll_ms",
            "ar_rt_waker_wait",
            "ar_rt_supervisor_poll",
            "ar_rt_supervisor_wait",
            "ar_rt_supervisor_kill",
        ] {
            let id = module
                .declare_function(name, Linkage::Import, &two_i64_ret_i64)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut sig = Signature::new(default_call_conv);
        for _ in 0..4 {
            sig.params.push(AbiParam::new(I64));
        }
        sig.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_reactor_register_socket", Linkage::Import, &sig)
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to declare ar_rt_reactor_register_socket: {err:?}"
                ))
            })?;
        insert_sym(func_ids, "ar_rt_reactor_register_socket", id);

        let mut zero_ret = Signature::new(default_call_conv);
        zero_ret.returns.push(AbiParam::new(I64));
        for name in [
            "ar_rt_reactor_backend",
            "ar_rt_waker_create",
            "ar_rt_supervisor_create",
        ] {
            if !func_ids.contains_key(name) {
                let id = module
                    .declare_function(name, Linkage::Import, &zero_ret)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                insert_sym(func_ids, name, id);
            }
        }

        let mut void_sig = Signature::new(default_call_conv);
        void_sig.params.push(AbiParam::new(I64));
        for name in [
            "ar_rt_waker_wake",
            "ar_rt_waker_destroy",
            "ar_rt_supervisor_destroy",
            "ar_rt_tcp_close",
        ] {
            let id = module
                .declare_function(name, Linkage::Import, &void_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut ret_sig = Signature::new(default_call_conv);
        ret_sig.params.push(AbiParam::new(I64));
        ret_sig.returns.push(AbiParam::new(I64));
        for name in ["ar_rt_tcp_listen", "ar_rt_tcp_accept", "ar_rt_tcp_connect"] {
            let id = module
                .declare_function(name, Linkage::Import, &ret_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut rw_sig = Signature::new(default_call_conv);
        rw_sig.params.push(AbiParam::new(I64));
        rw_sig.params.push(AbiParam::new(ptr_type));
        rw_sig.params.push(AbiParam::new(I64));
        rw_sig.returns.push(AbiParam::new(I64));
        for name in [
            "ar_rt_tcp_read",
            "ar_rt_tcp_write",
            "ar_rt_tcp_read_async",
            "ar_rt_tcp_write_async",
        ] {
            let id = module
                .declare_function(name, Linkage::Import, &rw_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut two = Signature::new(default_call_conv);
        two.params.push(AbiParam::new(I64));
        two.params.push(AbiParam::new(I64));
        two.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_tcp_set_nonblocking", Linkage::Import, &two)
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to declare ar_rt_tcp_set_nonblocking: {err:?}"
                ))
            })?;
        insert_sym(func_ids, "ar_rt_tcp_set_nonblocking", id);

        let mut three = Signature::new(default_call_conv);
        for _ in 0..3 {
            three.params.push(AbiParam::new(I64));
        }
        three.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_tcp_wait", Linkage::Import, &three)
            .map_err(|err| codegen_ice(format!("failed to declare ar_rt_tcp_wait: {err:?}")))?;
        insert_sym(func_ids, "ar_rt_tcp_wait", id);

        let mut four = Signature::new(default_call_conv);
        for _ in 0..4 {
            four.params.push(AbiParam::new(I64));
        }
        four.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_tcp_wait_wake", Linkage::Import, &four)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_rt_tcp_wait_wake: {err:?}"))
            })?;
        insert_sym(func_ids, "ar_rt_tcp_wait_wake", id);

        let mut sup_sig = Signature::new(default_call_conv);
        sup_sig.params.push(AbiParam::new(I64));
        sup_sig.params.push(AbiParam::new(ptr_type));
        sup_sig.params.push(AbiParam::new(I64));
        sup_sig.params.push(AbiParam::new(I64));
        sup_sig.returns.push(AbiParam::new(I64));
        for name in ["ar_rt_supervisor_spawn", "ar_rt_supervisor_spawn_str"] {
            let id = module
                .declare_function(name, Linkage::Import, &sup_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }
    }

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

    // ToStr v0.1 host helpers
    for (name, val_ty) in [
        ("ar_jit_i64_to_str", I64),
        ("ar_jit_u64_to_str", I64),
        ("ar_jit_f64_to_str", F64),
        ("ar_jit_bool_to_str", I8),
        ("ar_jit_char_to_str", I32),
        ("ar_jit_err_to_str", ptr_type),
    ] {
        let mut sig = Signature::new(default_call_conv);
        sig.params.push(AbiParam::new(val_ty));
        sig.params.push(AbiParam::new(ptr_type));
        sig.returns.push(AbiParam::new(ptr_type));
        let id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
        insert_sym(func_ids, name, id);
    }

    Ok(())
}
