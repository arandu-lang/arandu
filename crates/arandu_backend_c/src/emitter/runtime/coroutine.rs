//! C runtime helpers for coroutine polling, heap creation, and typed awaits.

use std::fmt::Write;

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_co_poll_runtime(&mut self) {
        // Typed await in expr.rs inlines disc/payload loads for the real C type.
        // Keep i64 helpers only for host/test parity paths that still use them.
        let _ = writeln!(
            &mut self.output,
            r#"/* A3.6: disc 0=Ready payload@8; disc 1=PendingOnce then Ready.
 * Prefer typed inline await (no i64 cast). i64 helpers remain for MVP host tests. */
static int ar_co_poll_i64(uint8_t *state, int64_t *out) {{
    uint32_t disc = *(uint32_t*)state;
    if (disc == 0) {{ *out = *(int64_t*)(state + 8); return 0; }}
    if (disc == 1) {{ *(uint32_t*)state = 0; return 1; }}
    *out = *(int64_t*)(state + 8); return 0;
}}
static int64_t ar_co_block_on_i64(uint8_t *state) {{
    int64_t out = 0;
    for (;;) {{
        if (ar_co_poll_i64(state, &out) == 0) return out;
    }}
}}
static void ar_co_free(uint8_t *state) {{
    if (!state) return;
    if (*(uint32_t*)(state + 4) != 0x4152434fu) abort(); /* fail-closed magic */
    free(state);
}}

/* Standard C99 Range and Coroutine helper functions */
static inline void** ar_make_range(intptr_t left, intptr_t right) {{
    void** r = (void**)malloc(sizeof(void*) * 2);
    if (!r) abort();
    r[0] = (void*)left;
    r[1] = (void*)right;
    return r;
}}

static inline void* ar_co_make_ready_heap(size_t size, void* val_ptr, size_t val_size) {{
    uint8_t* co = (uint8_t*)malloc(size);
    if (!co) abort();
    *(uint32_t*)co = 0;
    *(uint32_t*)(co + 4) = 0x4152434f;
    if (val_size > 0 && val_ptr) {{
        memcpy(co + 8, val_ptr, val_size);
    }}
    return (void*)co;
}}

static inline int64_t ar_co_await_i64(uint8_t* aw) {{
    for (;;) {{
        uint32_t d = *(uint32_t*)aw;
        if (d == 0) return *(int64_t*)(aw + 8);
        if (d == 1) {{ *(uint32_t*)aw = 0; continue; }}
        return *(int64_t*)(aw + 8);
    }}
}}

static inline double ar_co_await_f64(uint8_t* aw) {{
    for (;;) {{
        uint32_t d = *(uint32_t*)aw;
        if (d == 0) return *(double*)(aw + 8);
        if (d == 1) {{ *(uint32_t*)aw = 0; continue; }}
        return *(double*)(aw + 8);
    }}
}}

static inline void* ar_co_await_ptr(uint8_t* aw) {{
    for (;;) {{
        uint32_t d = *(uint32_t*)aw;
        if (d == 0) return *(void**)(aw + 8);
        if (d == 1) {{ *(uint32_t*)aw = 0; continue; }}
        return *(void**)(aw + 8);
    }}
}}"#
        );
    }
}
