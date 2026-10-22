use arandu_semantics::Diagnostic;
use cranelift_codegen::ir::types::{F64, I8, I32, I64};
use cranelift_codegen::ir::{AbiParam, Signature, Type};
use cranelift_codegen::isa::CallConv;
use cranelift_module::Linkage;
use rustc_hash::FxHashMap;

use super::codegen_ice;
use super::insert_sym;

pub(super) fn declare_os_path_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
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
    Ok(())
}

pub(super) fn declare_text_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
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

pub(super) fn declare_fs_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // RFC 0016 capability FS
    {
        let mut dir_open_sig = Signature::new(default_call_conv);
        dir_open_sig.params.push(AbiParam::new(ptr_type));
        dir_open_sig.params.push(AbiParam::new(ptr_type));
        dir_open_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_fs_dir_open", Linkage::Import, &dir_open_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_fs_dir_open: {err:?}")))?;
        insert_sym(func_ids, "ar_fs_dir_open", id);

        let mut openat_sig = Signature::new(default_call_conv);
        openat_sig.params.push(AbiParam::new(I32));
        openat_sig.params.push(AbiParam::new(ptr_type));
        openat_sig.params.push(AbiParam::new(ptr_type));
        openat_sig.params.push(AbiParam::new(I8));
        openat_sig.params.push(AbiParam::new(I8));
        openat_sig.params.push(AbiParam::new(I8));
        openat_sig.params.push(AbiParam::new(I8));
        openat_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_fs_openat", Linkage::Import, &openat_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_fs_openat: {err:?}")))?;
        insert_sym(func_ids, "ar_fs_openat", id);

        let mut dir_openat_sig = Signature::new(default_call_conv);
        dir_openat_sig.params.extend([
            AbiParam::new(I32),
            AbiParam::new(ptr_type),
            AbiParam::new(ptr_type),
        ]);
        dir_openat_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_fs_dir_openat", Linkage::Import, &dir_openat_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_fs_dir_openat: {err:?}")))?;
        insert_sym(func_ids, "ar_fs_dir_openat", id);

        let mut mkdirat_sig = Signature::new(default_call_conv);
        mkdirat_sig.params.push(AbiParam::new(I32));
        mkdirat_sig.params.push(AbiParam::new(ptr_type));
        mkdirat_sig.params.push(AbiParam::new(ptr_type));
        mkdirat_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_fs_mkdirat", Linkage::Import, &mkdirat_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_fs_mkdirat: {err:?}")))?;
        insert_sym(func_ids, "ar_fs_mkdirat", id);

        let mut unlinkat_sig = Signature::new(default_call_conv);
        unlinkat_sig.params.push(AbiParam::new(I32));
        unlinkat_sig.params.push(AbiParam::new(ptr_type));
        unlinkat_sig.params.push(AbiParam::new(ptr_type));
        unlinkat_sig.params.push(AbiParam::new(I8));
        unlinkat_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_fs_unlinkat", Linkage::Import, &unlinkat_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_fs_unlinkat: {err:?}")))?;
        insert_sym(func_ids, "ar_fs_unlinkat", id);
    }
    Ok(())
}
