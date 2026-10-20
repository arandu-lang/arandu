//! C runtime helpers for generational arena allocation and pure dynamic buffers.

use std::fmt::Write;

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_gen_arena_runtime(&mut self) {
        self.output.push_str(include_str!("gen_arena.c"));
    }

    /// Raw buffer hosts for `std.alloc.vec` / `std.alloc.gen_arena` pure-buffer path.
    pub(super) fn emit_vec_buf_runtime(&mut self, uint_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* Pure-buffer alloc (Vec / GenArena thin) — mirrors JIT ar_vec_*. */
static void *ar_vec_malloc({uint_c_ty} size) {{
    if (size == 0) return NULL;
    void *p = malloc((size_t)size);
    return p;
}}
static void ar_vec_buf_free(void *p, {uint_c_ty} size) {{
    (void)size;
    free(p);
}}
static void *ar_vec_realloc(void *p, {uint_c_ty} old_size, {uint_c_ty} new_size) {{
    if (new_size == 0) {{ free(p); return NULL; }}
    void *q = realloc(p, (size_t)new_size);
    (void)old_size;
    return q;
}}
typedef struct {{ uint8_t *data; {uint_c_ty} len; {uint_c_ty} capacity; }} ArOwnedStringRuntime;
static bool ar_string_push_str(void *raw, const uint8_t *value_ptr, int64_t value_len) {{
    ArOwnedStringRuntime *s = (ArOwnedStringRuntime*)raw;
    if (!s || value_len < 0 || (value_len > 0 && !value_ptr)) return false;
    {uint_c_ty} n = ({uint_c_ty})value_len;
    if (n > UINT32_MAX || s->len > UINT32_MAX - n) return false;
    {uint_c_ty} required = s->len + n;
    if (required > s->capacity) {{
        {uint_c_ty} capacity = s->capacity < 8 ? 8 : s->capacity;
        while (capacity < required) {{
            capacity = capacity > UINT32_MAX / 2 ? UINT32_MAX : capacity * 2;
            if (capacity == UINT32_MAX && capacity < required) return false;
        }}
        uint8_t *replacement = (uint8_t*)ar_vec_realloc(s->data, s->capacity, capacity);
        if (!replacement) return false;
        s->data = replacement;
        s->capacity = capacity;
    }}
    if (n > 0) memcpy(s->data + s->len, value_ptr, (size_t)n);
    s->len = required;
    return true;
}}

static void ar_rt_copy_value(void *dest, const void *source, size_t size) {{
    if (size != 0 && dest != NULL && source != NULL && dest != source) {{
        memcpy(dest, source, size);
    }}
}}

static uint8_t *ar_rt_alloc_aligned(size_t size, size_t align) {{
    if (size == 0 || align == 0 || (align & (align - 1)) != 0) {{
        return NULL;
    }}
    size_t extra = align - 1 + sizeof(void*);
    if (size > SIZE_MAX - extra) {{
        return NULL;
    }}
    void *raw = malloc(size + extra);
    if (!raw) {{
        return NULL;
    }}
    uintptr_t aligned = ((uintptr_t)raw + sizeof(void*) + (align - 1)) & ~(uintptr_t)(align - 1);
    ((void**)aligned)[-1] = raw;
    return (uint8_t*)aligned;
}}

static void ar_rt_free_aligned(uint8_t *pointer, size_t size, size_t align) {{
    if (!pointer || size == 0 || align == 0 || (align & (align - 1)) != 0) {{
        return;
    }}
    free(((void**)pointer)[-1]);
}}"#
        );
    }
}
