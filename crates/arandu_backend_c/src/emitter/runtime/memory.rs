//! C runtime helpers for generational arena allocation and pure dynamic buffers.

use std::fmt::Write;

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_gen_arena_runtime(&mut self) {
        self.output.push_str(include_str!("gen_arena.c"));
    }

    pub(super) fn emit_task_arena_runtime(&mut self) {
        let _ = writeln!(
            &mut self.output,
            r#"/* RFC 0043: Dual-Locus linear TaskArena runtime */
#define AR_TASK_ARENA_DEFAULT_SLAB_CAP (64u * 1024u)
typedef struct {{
    uint8_t *raw;
    uint8_t *base;
    size_t capacity;
}} ArTaskArenaSlab;
static struct {{
    ArTaskArenaSlab *slabs;
    size_t count;
    size_t cap;
    size_t slab_idx;
    size_t offset;
}} ar_task_arena = {{ NULL, 0u, 0u, 0u, 0u }};

static bool ar_task_arena_push_slab(size_t min_cap, size_t min_align) {{
    size_t cap = min_cap > AR_TASK_ARENA_DEFAULT_SLAB_CAP ? min_cap : AR_TASK_ARENA_DEFAULT_SLAB_CAP;
    size_t align = min_align > 64u ? min_align : 64u;
    if ((align & (align - 1u)) != 0u) return false;
    if (ar_task_arena.count == ar_task_arena.cap) {{
        size_t new_cap = ar_task_arena.cap == 0u ? 8u : ar_task_arena.cap * 2u;
        ArTaskArenaSlab *next = (ArTaskArenaSlab *)realloc(
            ar_task_arena.slabs, new_cap * sizeof(ArTaskArenaSlab));
        if (!next) return false;
        ar_task_arena.slabs = next;
        ar_task_arena.cap = new_cap;
    }}
    size_t extra = align - 1u;
    if (cap > SIZE_MAX - extra) return false;
    uint8_t *raw = (uint8_t *)calloc(1u, cap + extra);
    if (!raw) return false;
    uintptr_t aligned = ((uintptr_t)raw + extra) & ~(uintptr_t)extra;
    ar_task_arena.slabs[ar_task_arena.count].raw = raw;
    ar_task_arena.slabs[ar_task_arena.count].base = (uint8_t *)aligned;
    ar_task_arena.slabs[ar_task_arena.count].capacity = cap;
    ar_task_arena.count += 1u;
    return true;
}}

static void *ar_task_arena_alloc(size_t size, size_t align) {{
    size_t actual_size = size == 0u ? 1u : size;
    size_t actual_align = align == 0u ? 1u : align;
    if ((actual_align & (actual_align - 1u)) != 0u) {{
        ar_trap("panic: out of memory\n");
    }}
    if (ar_task_arena.count == 0u) {{
        if (!ar_task_arena_push_slab(actual_size, actual_align)) ar_trap("panic: out of memory\n");
        ar_task_arena.slab_idx = 0u;
        ar_task_arena.offset = 0u;
    }}
    for (;;) {{
        ArTaskArenaSlab *slab = &ar_task_arena.slabs[ar_task_arena.slab_idx];
        uintptr_t base_addr = (uintptr_t)slab->base;
        uintptr_t cur_addr = base_addr + ar_task_arena.offset;
        uintptr_t mask = (uintptr_t)(actual_align - 1u);
        uintptr_t aligned_addr = (cur_addr + mask) & ~mask;
        size_t new_offset = (size_t)(aligned_addr - base_addr) + actual_size;
        if (new_offset <= slab->capacity) {{
            ar_task_arena.offset = new_offset;
            return (void *)aligned_addr;
        }}
        if (ar_task_arena.slab_idx + 1u < ar_task_arena.count) {{
            ar_task_arena.slab_idx += 1u;
            ar_task_arena.offset = 0u;
            continue;
        }}
        if (!ar_task_arena_push_slab(actual_size, actual_align)) ar_trap("panic: out of memory\n");
        ar_task_arena.slab_idx = ar_task_arena.count - 1u;
        ar_task_arena.offset = 0u;
    }}
}}

static uint64_t ar_task_arena_checkpoint(void) {{
    return ((uint64_t)(uint32_t)ar_task_arena.slab_idx << 32)
        | (uint64_t)(uint32_t)ar_task_arena.offset;
}}

static void ar_task_arena_rewind(uint64_t checkpoint) {{
    size_t target_slab = (size_t)(checkpoint >> 32);
    size_t target_offset = (size_t)(uint32_t)checkpoint;
    if (ar_task_arena.count == 0u) return;
    if (target_slab < ar_task_arena.count) {{
        size_t cap = ar_task_arena.slabs[target_slab].capacity;
        ar_task_arena.slab_idx = target_slab;
        ar_task_arena.offset = target_offset <= cap ? target_offset : cap;
    }}
}}

static void ar_task_arena_reset(void) {{
    ar_task_arena.slab_idx = 0u;
    ar_task_arena.offset = 0u;
}}

static void ar_task_arena_shutdown(void) {{
    for (size_t i = 0u; i < ar_task_arena.count; ++i) {{
        free(ar_task_arena.slabs[i].raw);
    }}
    free(ar_task_arena.slabs);
    ar_task_arena.slabs = NULL;
    ar_task_arena.count = 0u;
    ar_task_arena.cap = 0u;
    ar_task_arena.slab_idx = 0u;
    ar_task_arena.offset = 0u;
}}"#
        );
    }

    /// Raw buffer hosts for `std.alloc.vec` / `std.alloc.gen_arena` pure-buffer path.
    pub(super) fn emit_vec_buf_runtime(&mut self, uint_c_ty: &str) {
        let _ = writeln!(
            &mut self.output,
            r#"/* Pure-buffer alloc (Vec / GenArena thin) — mirrors JIT ar_vec_*.
   malloc's alignment is only the host pointer alignment. SIMD vectors need
   16 bytes, so the payload pointer is over-aligned and the raw block is
   stored immediately before it. realloc keeps the same contract. */
#define AR_VEC_ALIGN 16
static void *ar_vec_malloc({uint_c_ty} size) {{
    if (size == 0) return NULL;
    size_t payload = (size_t)size;
    size_t extra = (size_t)AR_VEC_ALIGN - 1 + sizeof(void *);
    if (payload > SIZE_MAX - extra) return NULL;
    void *raw = malloc(payload + extra);
    if (!raw) return NULL;
    uintptr_t aligned = ((uintptr_t)raw + sizeof(void *) + ((size_t)AR_VEC_ALIGN - 1))
        & ~(uintptr_t)((size_t)AR_VEC_ALIGN - 1);
    ((void **)aligned)[-1] = raw;
    return (void *)aligned;
}}
static void ar_vec_buf_free(void *p, {uint_c_ty} size) {{
    (void)size;
    if (p) free(((void **)p)[-1]);
}}
static void *ar_vec_realloc(void *p, {uint_c_ty} old_size, {uint_c_ty} new_size) {{
    if (new_size == 0) {{ ar_vec_buf_free(p, old_size); return NULL; }}
    void *q = ar_vec_malloc(new_size);
    if (!q) return NULL;
    if (p && old_size > 0) {{
        size_t copied = (size_t)(old_size < new_size ? old_size : new_size);
        memcpy(q, p, copied);
        ar_vec_buf_free(p, old_size);
    }}
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
