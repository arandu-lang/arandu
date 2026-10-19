//! C runtime helpers for environment variables and command line arguments (std.env).

use std::fmt::Write;

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_env_runtime(&mut self, int_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.env host helpers (mirrors JIT ar_env_* runtime). */
static int ar_env_c_argc = 0;
static char **ar_env_c_argv = NULL;

#if defined(_WIN32)
static void ar_init_env_args_if_needed(void) {{
    if (!ar_env_c_argv) {{
        ar_env_c_argc = __argc;
        ar_env_c_argv = __argv;
    }}
}}
#elif defined(__GNUC__) || defined(__clang__)
__attribute__((constructor)) static void ar_capture_env_args(int argc, char **argv) {{
    ar_env_c_argc = argc;
    ar_env_c_argv = argv;
}}
#endif

static {int_c_ty} ar_env_args_len(void) {{
#if defined(_WIN32)
    ar_init_env_args_if_needed();
#endif
    return ({int_c_ty})ar_env_c_argc;
}}

static ArStr ar_env_arg({int_c_ty} index) {{
#if defined(_WIN32)
    ar_init_env_args_if_needed();
#endif
    if (index < 0 || index >= ({int_c_ty})ar_env_c_argc || !ar_env_c_argv) {{
        return ar_str_pack((const uint8_t*)"", 0);
    }}
    const char *s = ar_env_c_argv[index];
    size_t len = s ? strlen(s) : 0;
    return ar_str_pack((const uint8_t*)s, ({int_c_ty})len);
}}

static {int_c_ty} ar_env_var_is_set(ArStr name) {{
    if (name.len <= 0 || !name.ptr) return 0;
    char stack_buf[256];
    char *c_name = ar_str_to_c_path(name, stack_buf, sizeof(stack_buf));
    if (!c_name) return 0;
    char *val = getenv(c_name);
    if (c_name != stack_buf) free(c_name);
    return val != NULL ? 1 : 0;
}}

#if !defined(_WIN32)
#include <time.h>
#endif

static int64_t ar_time_monotonic_ns(void) {{
#if defined(_WIN32)
    static LARGE_INTEGER ar_qpc_origin;
    static LARGE_INTEGER ar_qpc_freq;
    static LONG ar_qpc_init = 0;
    while (InterlockedCompareExchange(&ar_qpc_init, 1, 0) != 0) {{
        if (InterlockedCompareExchange(&ar_qpc_init, 2, 2) == 2) break;
        SwitchToThread();
    }}
    if (ar_qpc_freq.QuadPart == 0) {{
        QueryPerformanceFrequency(&ar_qpc_freq);
        QueryPerformanceCounter(&ar_qpc_origin);
        InterlockedExchange(&ar_qpc_init, 2);
    }}
    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    uint64_t ticks = (uint64_t)(now.QuadPart - ar_qpc_origin.QuadPart);
    uint64_t freq = (uint64_t)ar_qpc_freq.QuadPart;
    uint64_t secs = ticks / freq;
    uint64_t rem = ticks % freq;
    if (secs > (uint64_t)(INT64_MAX / 1000000000LL)) return INT64_MAX;
    uint64_t ns = secs * 1000000000ULL + (rem * 1000000000ULL) / freq;
    return ns > (uint64_t)INT64_MAX ? INT64_MAX : (int64_t)ns;
#else
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) abort();
    uint64_t secs = (uint64_t)ts.tv_sec;
    if (secs > (uint64_t)(INT64_MAX / 1000000000LL)) return INT64_MAX;
    uint64_t ns = secs * 1000000000ULL + (uint64_t)ts.tv_nsec;
    return ns > (uint64_t)INT64_MAX ? INT64_MAX : (int64_t)ns;
#endif
}}

static void ar_process_exit({int_c_ty} code) {{
    int status = code < 0 ? 255 : (code > 255 ? (int)(code & 0xff) : (int)code);
    exit(status);
}}"#
        );
    }
}
