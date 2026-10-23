//! C preamble, headers, string runtime, coroutine polling, and memory helpers.

pub(super) mod coroutine;
pub(super) mod env;
mod features;
pub(super) mod fs;
pub(super) mod memory;
mod native_thread;
mod pal;
mod phase_lock;
pub(super) mod prelude;
pub(super) mod string;
pub(super) mod task;

use std::fmt::Write;

use super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_headers(&mut self, needs_str: bool) {
        let features = self.runtime_features();
        self.arena_runtime = features.arena;
        let mut rotate_widths = [false; 2];
        for function in &self.program.funcs {
            if !self.live_functions.contains(&function.symbol) {
                continue;
            }
            let (rotations, _) =
                super::rotations::plan(function, &self.program.literal_pool, self.interner);
            for rotation in rotations.into_iter().flatten() {
                rotate_widths[usize::from(rotation.width == 64)] = true;
            }
        }
        let _ = writeln!(
            &mut self.output,
            "#if !defined(_WIN32) && !defined(_DEFAULT_SOURCE)\n#define _DEFAULT_SOURCE\n#endif\n#if defined(__APPLE__) && !defined(_DARWIN_C_SOURCE)\n#define _DARWIN_C_SOURCE\n#endif\n#if !defined(_WIN32) && !defined(_POSIX_C_SOURCE)\n#define _POSIX_C_SOURCE 200112L\n#endif"
        );
        let _ = writeln!(&mut self.output, "#include <stdint.h>");
        let _ = writeln!(&mut self.output, "#include <stdbool.h>");
        for (index, needed) in rotate_widths.into_iter().enumerate() {
            if needed {
                let width = if index == 0 { 32 } else { 64 };
                let _ = writeln!(
                    self.output,
                    "static inline uint{width}_t ar_rotr{width}(uint{width}_t x, unsigned n) {{ return (x >> n) | (x << ({width} - n)); }}"
                );
            }
        }
        let _ = writeln!(&mut self.output, "#include <stdlib.h>");
        let _ = writeln!(&mut self.output, "#include <string.h>");
        let _ = writeln!(&mut self.output, "#include <stdio.h>");
        let _ = writeln!(&mut self.output, "#include <errno.h>");
        // memcpy preserves IEEE encodings without strict-aliasing violations,
        // decimal round trips, or arithmetic that could quiet/canonicalize NaN.
        if features.float_bits[0] {
            let _ = writeln!(
                &mut self.output,
                "static inline float ar_f32_from_bits(uint32_t bits) {{ float value; memcpy(&value, &bits, sizeof(value)); return value; }}"
            );
        }
        if features.float_bits[1] {
            let _ = writeln!(
                &mut self.output,
                "static inline double ar_f64_from_bits(uint64_t bits) {{ double value; memcpy(&value, &bits, sizeof(value)); return value; }}"
            );
        }
        if features.tasks {
            let _ = writeln!(&mut self.output, "#include <stdatomic.h>");
        }
        let needs_pal = features.tasks || features.native_threads || features.fs || features.env;
        // Net PAL is emitted first so winsock2.h precedes windows.h inside pal.c.
        if features.net {
            self.emit_pal_net_runtime();
        }
        if needs_pal {
            self.emit_pal_runtime();
        } else if features.buffers {
            let _ = writeln!(
                &mut self.output,
                "#if defined(_WIN32)\n#include <malloc.h>\n#include <windows.h>\n#endif"
            );
        }
        if features.tasks {
            let _ = writeln!(
                self.output,
                "#if !defined(_WIN32) || defined(__MINGW32__)\n#include <pthread.h>\n#include <unistd.h>\n#include <sched.h>\n#endif"
            );
            let _ = writeln!(
                &mut self.output,
                "#if defined(__APPLE__)\n#include <sys/sysctl.h>\n#endif"
            );
        }
        if features.fs {
            let _ = writeln!(self.output, "#include <stdatomic.h>");
            let _ = writeln!(self.output, "#include <sys/types.h>");
            let _ = writeln!(
                self.output,
                "#if !defined(_WIN32) || defined(__MINGW32__)\n#include <unistd.h>\n#endif"
            );
            let _ = writeln!(&mut self.output, "#include <sys/stat.h>");
            let _ = writeln!(
                &mut self.output,
                "#ifndef S_ISDIR\n#define S_ISDIR(mode) (((mode) & S_IFMT) == S_IFDIR)\n#endif"
            );
            let _ = writeln!(
                &mut self.output,
                "#if !defined(_WIN32)\n#include <fcntl.h>\n#endif"
            );
        }
        if features.net && !features.fs {
            let _ = writeln!(self.output, "#include <stdatomic.h>");
        }
        let _ = writeln!(
            &mut self.output,
            "#if defined(__GNUC__) || defined(__clang__)\n#define AR_MAY_ALIAS __attribute__((__may_alias__))\n#else\n#define AR_MAY_ALIAS\n#endif"
        );
        // The concat runtime (and its variadic `ar_str_concat_n`) is also
        // emitted for path-only programs; see `emit_str_concat_runtime` below.
        if features.concat || features.path {
            let _ = writeln!(&mut self.output, "#include <stdarg.h>");
        }
        if features.formats.iter().any(|&f| f) {
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
        if features.black_box[0] {
            let _ = writeln!(
                &mut self.output,
                "static AR_BENCH_NOINLINE int64_t ar_bench_black_box_i64(int64_t value) {{ volatile int64_t opaque = value; return opaque; }}"
            );
        }
        if features.black_box[1] {
            let _ = writeln!(
                &mut self.output,
                "static AR_BENCH_NOINLINE double ar_bench_black_box_f64(double value) {{ volatile double opaque = value; return opaque; }}"
            );
        }
        if features.black_box[3] {
            let _ = writeln!(
                self.output,
                "static AR_BENCH_NOINLINE float ar_bench_black_box_f32(float value) {{ volatile float opaque = value; return opaque; }}"
            );
        }
        if features.black_box[2] {
            let _ = writeln!(
                &mut self.output,
                "static AR_BENCH_NOINLINE void *ar_bench_black_box_ptr(void *value) {{ void * volatile opaque = value; return opaque; }}"
            );
        }
        // Keep the shared descriptor ABI available to nested type declarations.
        // Runtime helpers below are selected independently from that ABI type.
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
        if features.arena {
            self.emit_gen_arena_runtime();
        }
        // Pure-buffer host used by std.alloc.vec / gen_arena product surface.
        let uint_c_ty = if self.layout.pointer_width() == 4 {
            "uint32_t"
        } else {
            "uint64_t"
        };
        if features.buffers {
            self.emit_vec_buf_runtime(uint_c_ty);
        }
        // A3.6: poll / block_on for coroutine state blobs (disc@0, payload@8).
        if features.coroutine {
            self.emit_co_poll_runtime();
        }
        // SL_R.0: cooperative task table mirror (SyncExecutor host surface).
        if features.tasks {
            self.emit_task_runtime();
            self.emit_phase_lock_runtime();
        }
        if features.native_threads {
            self.emit_native_thread_runtime();
        }
        // std.fs / std.env runtime hosts.
        if features.fs || features.env {
            self.emit_c_string_runtime();
        }
        if features.fs {
            self.emit_fs_runtime(uint_c_ty, len_c_ty);
        }
        if features.net {
            self.emit_net_runtime(uint_c_ty);
        }
        if features.env {
            self.emit_env_runtime(len_c_ty);
        }
        let _ = writeln!(&mut self.output);
        if !needs_str {
            return;
        }
        // PROMOTE-L4 path hosts (need ArStr + ar_str_pack).
        if features.path {
            self.emit_path_runtime(len_c_ty);
        }
        if features.string_eq {
            self.emit_str_eq_runtime();
        }
        if features.concat || features.path {
            self.emit_str_concat_runtime();
        }
        if features.formats.iter().any(|&f| f) {
            self.emit_str_format_runtime();
        }
    }
}
