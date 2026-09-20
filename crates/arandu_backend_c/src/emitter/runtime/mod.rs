//! C preamble, headers, string runtime, coroutine polling, and memory helpers.

pub(super) mod coroutine;
pub(super) mod env;
pub(super) mod fs;
pub(super) mod memory;
pub(super) mod prelude;
pub(super) mod task;

use std::fmt::Write;

use super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_headers(&mut self, needs_str: bool) {
        let _ = writeln!(
            &mut self.output,
            "#if defined(__APPLE__) && !defined(_DARWIN_C_SOURCE)\n#define _DARWIN_C_SOURCE\n#endif\n#if !defined(_WIN32) && !defined(_POSIX_C_SOURCE)\n#define _POSIX_C_SOURCE 200112L\n#endif"
        );
        let _ = writeln!(&mut self.output, "#include <stdint.h>");
        let _ = writeln!(&mut self.output, "#include <stdbool.h>");
        let _ = writeln!(&mut self.output, "#include <stdlib.h>");
        let _ = writeln!(&mut self.output, "#include <string.h>");
        let _ = writeln!(&mut self.output, "#include <stdio.h>");
        let _ = writeln!(&mut self.output, "#include <errno.h>");
        let _ = writeln!(&mut self.output, "#include <stdatomic.h>");
        let _ = writeln!(&mut self.output, "#include <sys/types.h>");
        let _ = writeln!(
            &mut self.output,
            "#if defined(_WIN32)\n#include <malloc.h>\n#include <windows.h>\n#endif\n#if !defined(_WIN32) || defined(__MINGW32__)\n#include <pthread.h>\n#include <unistd.h>\n#endif"
        );
        let _ = writeln!(
            &mut self.output,
            "#if defined(__APPLE__)\n#include <sys/sysctl.h>\n#endif"
        );
        let _ = writeln!(&mut self.output, "#include <sys/stat.h>");
        let _ = writeln!(
            &mut self.output,
            "#ifndef S_ISDIR\n#define S_ISDIR(mode) (((mode) & S_IFMT) == S_IFDIR)\n#endif"
        );
        let _ = writeln!(
            &mut self.output,
            "#if !defined(_WIN32) || defined(__MINGW32__)\n#include <dirent.h>\n#endif"
        );
        let _ = writeln!(
            &mut self.output,
            "#if defined(__GNUC__) || defined(__clang__)\n#define AR_MAY_ALIAS __attribute__((__may_alias__))\n#else\n#define AR_MAY_ALIAS\n#endif"
        );
        if needs_str {
            let _ = writeln!(&mut self.output, "#include <stdarg.h>");
            let _ = writeln!(&mut self.output, "#include <math.h>");
        }
        let _ = writeln!(
            &mut self.output,
            "#ifndef AR_ABORT\n#if defined(__GNUC__) || defined(__clang__)\n#define AR_ABORT() __builtin_trap()\n#else\n#define AR_ABORT() abort()\n#endif\n#endif"
        );
        let _ = writeln!(
            &mut self.output,
            "#ifndef AR_UNREACHABLE\n#if defined(__GNUC__) || defined(__clang__)\n#define AR_UNREACHABLE() __builtin_trap()\n#else\n#define AR_UNREACHABLE() abort()\n#endif\n#endif"
        );
        let _ = writeln!(
            &mut self.output,
            "#if defined(__GNUC__) || defined(__clang__)\n#define AR_BENCH_NOINLINE __attribute__((noinline))\n#elif defined(_MSC_VER)\n#define AR_BENCH_NOINLINE __declspec(noinline)\n#else\n#define AR_BENCH_NOINLINE\n#endif"
        );
        let _ = writeln!(
            &mut self.output,
            "static AR_BENCH_NOINLINE int64_t ar_bench_black_box_i64(int64_t value) {{ volatile int64_t opaque = value; return opaque; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "static AR_BENCH_NOINLINE double ar_bench_black_box_f64(double value) {{ volatile double opaque = value; return opaque; }}"
        );
        let _ = writeln!(
            &mut self.output,
            "static AR_BENCH_NOINLINE void *ar_bench_black_box_ptr(void *value) {{ void * volatile opaque = value; return opaque; }}"
        );
        // `ArStr` is also part of the owned-string runtime ABI.  Keep the
        // two-word descriptor available even when the user program contains
        // no `str` value: `String.pushStr` is emitted with this ABI and the
        // allocation runtime below is unconditional.
        let len_c_ty = if self.layout.pointer_width() == 4 {
            "int32_t"
        } else {
            "int64_t"
        };
        let _ = writeln!(
            &mut self.output,
            "typedef struct {{ const uint8_t *ptr; {len_c_ty} len; }} ArStr;"
        );
        self.emitted_types.insert("ArStr".to_string());
        // Runtime helpers for fat-pointer strings (string interpolation & hosts).
        let _ = writeln!(
            &mut self.output,
            "static inline void ar_str_unpack(ArStr s, const uint8_t **ptr, {len_c_ty} *len) {{"
        );
        let _ = writeln!(&mut self.output, "    *ptr = s.ptr;");
        let _ = writeln!(&mut self.output, "    *len = s.len;");
        let _ = writeln!(&mut self.output, "}}");
        let _ = writeln!(
            &mut self.output,
            "static inline ArStr ar_str_pack(const uint8_t *ptr, {len_c_ty} len) {{"
        );
        let _ = writeln!(
            &mut self.output,
            "    return (ArStr){{ .ptr = ptr, .len = len }};"
        );
        let _ = writeln!(&mut self.output, "}}");
        // F2.3.runtime: process-lifetime gen arena (i64 payload MVP; mirrors JIT host).
        self.emit_gen_arena_runtime();
        // Pure-buffer host used by std.alloc.vec / gen_arena product surface.
        let uint_c_ty = if self.layout.pointer_width() == 4 {
            "uint32_t"
        } else {
            "uint64_t"
        };
        self.emit_vec_buf_runtime(uint_c_ty);
        // A3.6: poll / block_on for coroutine state blobs (disc@0, payload@8).
        self.emit_co_poll_runtime();
        // SL_R.0: cooperative task table mirror (SyncExecutor host surface).
        self.emit_task_runtime();
        // std.fs / std.env runtime hosts.
        self.emit_fs_runtime(uint_c_ty, len_c_ty);
        self.emit_env_runtime(len_c_ty);
        let _ = writeln!(&mut self.output);
        if !needs_str {
            return;
        }
        // PROMOTE-L4 path hosts (need ArStr + ar_str_pack).
        self.emit_path_runtime(len_c_ty);
        self.emit_str_converters(len_c_ty);
    }
}
