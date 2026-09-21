//! Host-JIT builder configuration and symbol table registration.

use arandu_semantics::Diagnostic;
use cranelift_jit::JITBuilder;

use super::isa::cached_host_isa;

pub(crate) fn create_jit_builder() -> Result<JITBuilder, Diagnostic> {
    let isa = cached_host_isa()?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());

    // ToStr v0.1 host helpers (malloc-backed fat strings).
    builder.symbol(
        "ar_jit_i64_to_str",
        crate::to_str_runtime::ar_jit_i64_to_str as *const u8,
    );
    builder.symbol(
        "ar_jit_u64_to_str",
        crate::to_str_runtime::ar_jit_u64_to_str as *const u8,
    );
    builder.symbol(
        "ar_jit_f64_to_str",
        crate::to_str_runtime::ar_jit_f64_to_str as *const u8,
    );
    builder.symbol(
        "ar_jit_bool_to_str",
        crate::to_str_runtime::ar_jit_bool_to_str as *const u8,
    );
    builder.symbol(
        "ar_jit_char_to_str",
        crate::to_str_runtime::ar_jit_char_to_str as *const u8,
    );

    // Prelude `io.println` (fat-pointer ABI: ptr + i64 len).
    builder.symbol("abort", std::process::abort as *const u8);
    builder.symbol(
        "io.println",
        crate::to_str_runtime::ar_jit_println as *const u8,
    );
    // Prelude `err.new(str) -> Err` (message handle = non-null ptr; fat-pointer str arg).
    builder.symbol(
        "err.new",
        crate::to_str_runtime::ar_jit_err_new as *const u8,
    );
    builder.symbol(
        "ar_jit_err_to_str",
        crate::to_str_runtime::ar_jit_err_to_str as *const u8,
    );

    // G4 type-erased compiler-managed GenRef ABI.
    builder.symbol(
        "ar_gen_insert_raw",
        arandu_runtime::gen_runtime_gold::ar_gen_insert_raw as *const u8,
    );
    builder.symbol(
        "ar_gen_get_raw",
        arandu_runtime::gen_runtime_gold::ar_gen_get_raw as *const u8,
    );
    builder.symbol(
        "ar_gen_set_raw",
        arandu_runtime::gen_runtime_gold::ar_gen_set_raw as *const u8,
    );
    builder.symbol(
        "ar_gen_upsert_raw",
        arandu_runtime::gen_runtime_gold::ar_gen_upsert_raw as *const u8,
    );
    builder.symbol(
        "ar_gen_remove_raw",
        arandu_runtime::gen_runtime_gold::ar_gen_remove_raw as *const u8,
    );
    builder.symbol(
        "ar_gen_shutdown_raw",
        arandu_runtime::gen_runtime_gold::ar_gen_shutdown_raw as *const u8,
    );

    // A3.6: coroutine poll / block_on (i64 payload MVP).
    builder.symbol(
        "ar_co_block_on_i64",
        crate::poll_runtime::ar_co_block_on_i64 as *const u8,
    );
    builder.symbol(
        "ar_co_poll_i64",
        crate::poll_runtime::ar_co_poll_i64 as *const u8,
    );
    builder.symbol(
        "ar_co_pending_once_i64",
        crate::poll_runtime::ar_co_pending_once_i64 as *const u8,
    );
    builder.symbol(
        "ar_co_make_ready_i64",
        crate::poll_runtime::ar_co_make_ready_i64 as *const u8,
    );
    builder.symbol("ar_co_free", crate::poll_runtime::ar_co_free as *const u8);

    // SL_R.0 cooperative runtime + SL_S path helpers
    builder.symbol(
        "ar_rt_spawn_i64",
        crate::rt_runtime::ar_rt_spawn_i64 as *const u8,
    );
    builder.symbol(
        "ar_rt_join_i64",
        crate::rt_runtime::ar_rt_join_i64 as *const u8,
    );
    builder.symbol(
        "ar_rt_block_on_i64",
        crate::rt_runtime::ar_rt_block_on_i64 as *const u8,
    );
    builder.symbol(
        "ar_rt_cancel_i64",
        crate::rt_runtime::ar_rt_cancel_i64 as *const u8,
    );
    builder.symbol(
        "ar_rt_parallel_fold_run",
        arandu_runtime::worker_scheduler::ar_rt_parallel_fold_run as *const u8,
    );
    builder.symbol(
        "ar_path_is_absolute",
        crate::rt_runtime::ar_path_is_absolute as *const u8,
    );
    builder.symbol(
        "ar_path_is_empty",
        crate::rt_runtime::ar_path_is_empty as *const u8,
    );
    builder.symbol("ar_path_join", crate::rt_runtime::ar_path_join as *const u8);
    builder.symbol(
        "ar_path_file_name",
        crate::rt_runtime::ar_path_file_name as *const u8,
    );
    builder.symbol(
        "ar_str_concat",
        crate::rt_runtime::ar_str_concat as *const u8,
    );
    builder.symbol(
        "ar_str_split_last",
        crate::rt_runtime::ar_str_split_last as *const u8,
    );

    // Minimal 0.1 optional OS surface (process / time / env)
    builder.symbol(
        "ar_process_exit",
        crate::os_runtime::ar_process_exit as *const u8,
    );
    builder.symbol(
        "ar_time_monotonic_ns",
        crate::os_runtime::ar_time_monotonic_ns as *const u8,
    );
    builder.symbol(
        "ar_env_args_len",
        crate::os_runtime::ar_env_args_len as *const u8,
    );
    builder.symbol("ar_env_arg", crate::os_runtime::ar_env_arg as *const u8);
    builder.symbol(
        "ar_env_var_is_set",
        crate::os_runtime::ar_env_var_is_set as *const u8,
    );
    // FS hosts (owned, zero-copy buffers; see arandu_runtime::fs_runtime).
    builder.symbol(
        "ar_fs_read_all",
        crate::fs_runtime::ar_fs_read_all as *const u8,
    );
    builder.symbol(
        "ar_fs_readdir",
        crate::fs_runtime::ar_fs_readdir as *const u8,
    );
    builder.symbol("ar_fs_open", crate::fs_runtime::ar_fs_open as *const u8);
    builder.symbol("ar_fs_read", crate::fs_runtime::ar_fs_read as *const u8);
    builder.symbol("ar_fs_write", crate::fs_runtime::ar_fs_write as *const u8);
    builder.symbol("ar_fs_close", crate::fs_runtime::ar_fs_close as *const u8);
    builder.symbol("exists", crate::fs_runtime::exists as *const u8);

    // SL_T.3: std.testing expectation and runner symbols
    builder.symbol(
        "ar_test_set_span",
        crate::testing_runtime::ar_test_set_span as *const u8,
    );
    builder.symbol(
        "ar_bench_black_box_i64",
        crate::testing_runtime::ar_bench_black_box_i64 as *const u8,
    );
    builder.symbol(
        "ar_bench_black_box_f64",
        crate::testing_runtime::ar_bench_black_box_f64 as *const u8,
    );
    builder.symbol(
        "ar_bench_black_box_ptr",
        crate::testing_runtime::ar_bench_black_box_ptr as *const u8,
    );
    builder.symbol(
        "ar_bench_loop",
        crate::testing_runtime::ar_bench_loop as *const u8,
    );
    builder.symbol(
        "ar_test_expect",
        crate::testing_runtime::ar_test_expect as *const u8,
    );
    builder.symbol(
        "ar_test_expect_equal_i64",
        crate::testing_runtime::ar_test_expect_equal_i64 as *const u8,
    );
    builder.symbol(
        "ar_test_expect_equal_f64",
        crate::testing_runtime::ar_test_expect_equal_f64 as *const u8,
    );
    builder.symbol(
        "ar_test_expect_equal_bool",
        crate::testing_runtime::ar_test_expect_equal_bool as *const u8,
    );
    builder.symbol(
        "ar_test_expect_equal_str",
        crate::testing_runtime::ar_test_expect_equal_str as *const u8,
    );
    builder.symbol(
        "ar_test_fail",
        crate::testing_runtime::ar_test_fail as *const u8,
    );
    builder.symbol(
        "ar_test_skip",
        crate::testing_runtime::ar_test_skip as *const u8,
    );
    builder.symbol(
        "ar_test_log",
        crate::testing_runtime::ar_test_log as *const u8,
    );
    builder.symbol(
        "ar_test_temp_dir",
        crate::testing_runtime::ar_test_temp_dir as *const u8,
    );

    // std.alloc.vec:
    // - Product path (pure-buffer): malloc / realloc / buf_free only.
    // - Handle API (new/push/get/…): unit-test / legacy GenArena-style table.
    builder.symbol(
        "ar_vec_malloc",
        crate::vec_runtime::ar_vec_malloc as *const u8,
    );
    builder.symbol(
        "ar_vec_buf_free",
        crate::vec_runtime::ar_vec_buf_free as *const u8,
    );
    builder.symbol(
        "ar_vec_realloc",
        crate::vec_runtime::ar_vec_realloc as *const u8,
    );
    builder.symbol(
        "ar_rt_copy_value",
        crate::vec_runtime::ar_rt_copy_value as *const u8,
    );
    builder.symbol(
        "ar_rt_alloc_aligned",
        crate::vec_runtime::ar_rt_alloc_aligned as *const u8,
    );
    builder.symbol(
        "ar_rt_free_aligned",
        crate::vec_runtime::ar_rt_free_aligned as *const u8,
    );
    builder.symbol(
        "ar_path_join_owned",
        crate::rt_runtime::ar_path_join_owned as *const u8,
    );
    builder.symbol(
        "ar_string_push_str",
        crate::vec_runtime::ar_string_push_str as *const u8,
    );
    builder.symbol("ar_vec_new", crate::vec_runtime::ar_vec_new as *const u8);
    builder.symbol("ar_vec_push", crate::vec_runtime::ar_vec_push as *const u8);
    builder.symbol("ar_vec_len", crate::vec_runtime::ar_vec_len as *const u8);
    builder.symbol("ar_vec_has", crate::vec_runtime::ar_vec_has as *const u8);
    builder.symbol("ar_vec_get", crate::vec_runtime::ar_vec_get as *const u8);
    builder.symbol("ar_vec_put", crate::vec_runtime::ar_vec_put as *const u8);
    builder.symbol("ar_vec_pop", crate::vec_runtime::ar_vec_pop as *const u8);
    builder.symbol(
        "ar_vec_clear",
        crate::vec_runtime::ar_vec_clear as *const u8,
    );
    builder.symbol(
        "ar_vec_destroy",
        crate::vec_runtime::ar_vec_destroy as *const u8,
    );

    // SL_R.2 reactor (epoll + timerfd)
    builder.symbol(
        "ar_rt_reactor_create",
        crate::reactor_runtime::ar_rt_reactor_create as *const u8,
    );
    builder.symbol(
        "ar_rt_reactor_destroy",
        crate::reactor_runtime::ar_rt_reactor_destroy as *const u8,
    );
    builder.symbol(
        "ar_rt_reactor_sleep_ms",
        crate::reactor_runtime::ar_rt_reactor_sleep_ms as *const u8,
    );
    builder.symbol(
        "ar_rt_reactor_arm_timer_ms",
        crate::reactor_runtime::ar_rt_reactor_arm_timer_ms as *const u8,
    );
    builder.symbol(
        "ar_rt_reactor_poll_ms",
        crate::reactor_runtime::ar_rt_reactor_poll_ms as *const u8,
    );
    builder.symbol(
        "ar_rt_reactor_backend",
        crate::reactor_runtime::ar_rt_reactor_backend as *const u8,
    );
    builder.symbol(
        "ar_rt_reactor_register_socket",
        crate::reactor_runtime::ar_rt_reactor_register_socket as *const u8,
    );

    // SL_R Waker
    builder.symbol(
        "ar_rt_waker_create",
        crate::waker_runtime::ar_rt_waker_create as *const u8,
    );
    builder.symbol(
        "ar_rt_waker_wake",
        crate::waker_runtime::ar_rt_waker_wake as *const u8,
    );
    builder.symbol(
        "ar_rt_waker_wait",
        crate::waker_runtime::ar_rt_waker_wait as *const u8,
    );
    builder.symbol(
        "ar_rt_waker_destroy",
        crate::waker_runtime::ar_rt_waker_destroy as *const u8,
    );

    // SL_R sockets
    builder.symbol(
        "ar_rt_tcp_listen",
        crate::socket_runtime::ar_rt_tcp_listen as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_accept",
        crate::socket_runtime::ar_rt_tcp_accept as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_connect",
        crate::socket_runtime::ar_rt_tcp_connect as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_read",
        crate::socket_runtime::ar_rt_tcp_read as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_write",
        crate::socket_runtime::ar_rt_tcp_write as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_close",
        crate::socket_runtime::ar_rt_tcp_close as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_set_nonblocking",
        crate::socket_runtime::ar_rt_tcp_set_nonblocking as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_wait",
        crate::socket_runtime::ar_rt_tcp_wait as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_wait_wake",
        crate::socket_runtime::ar_rt_tcp_wait_wake as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_read_async",
        crate::socket_runtime::ar_rt_tcp_read_async as *const u8,
    );
    builder.symbol(
        "ar_rt_tcp_write_async",
        crate::socket_runtime::ar_rt_tcp_write_async as *const u8,
    );

    // SL_R.1 supervisor
    builder.symbol(
        "ar_rt_supervisor_create",
        crate::supervisor_runtime::ar_rt_supervisor_create as *const u8,
    );
    builder.symbol(
        "ar_rt_supervisor_destroy",
        crate::supervisor_runtime::ar_rt_supervisor_destroy as *const u8,
    );
    builder.symbol(
        "ar_rt_supervisor_spawn",
        crate::supervisor_runtime::ar_rt_supervisor_spawn as *const u8,
    );
    builder.symbol(
        "ar_rt_supervisor_spawn_str",
        crate::supervisor_runtime::ar_rt_supervisor_spawn_str as *const u8,
    );
    builder.symbol(
        "ar_rt_supervisor_poll",
        crate::supervisor_runtime::ar_rt_supervisor_poll as *const u8,
    );
    builder.symbol(
        "ar_rt_supervisor_wait",
        crate::supervisor_runtime::ar_rt_supervisor_wait as *const u8,
    );
    builder.symbol(
        "ar_rt_supervisor_kill",
        crate::supervisor_runtime::ar_rt_supervisor_kill as *const u8,
    );

    Ok(builder)
}
