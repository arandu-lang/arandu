//! C runtime helpers for environment variables and command line arguments (std.env).

use std::fmt::Write;

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_env_runtime(&mut self, int_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* std.env host helpers (mirrors JIT ar_env_* runtime).
   Process arguments and the monotonic clock live in the PAL. */
static void ar_init_env_args_if_needed(void) {{
    ar_pal_ensure_process_args();
}}

static {int_c_ty} ar_env_args_len(void) {{
    ar_init_env_args_if_needed();
    return ({int_c_ty})ar_env_c_argc;
}}

static ArStr ar_env_arg({int_c_ty} index) {{
    ar_init_env_args_if_needed();
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

static int64_t ar_time_monotonic_ns(void) {{
    return ar_pal_monotonic_ns();
}}

static void ar_process_exit({int_c_ty} code) {{
    int status = code < 0 ? 255 : (code > 255 ? (int)(code & 0xff) : (int)code);
    exit(status);
}}"#
        );
    }
}
