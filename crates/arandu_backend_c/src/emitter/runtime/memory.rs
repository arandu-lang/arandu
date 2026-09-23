//! C runtime helpers for generational arena allocation and pure dynamic buffers.

use std::fmt::Write;

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    pub(super) fn emit_gen_arena_runtime(&mut self) {
        let _ = writeln!(
            &mut self.output,
            r#"/* G4 type-erased GenRef ABI: monotonic tokens, target layout, ordered drops. */
typedef void (*ar_gen_drop_fn)(void *);
typedef struct ar_gen_entry {{
    uint64_t token;
    void *data;
    void *allocation;
    size_t size;
    size_t align;
    ar_gen_drop_fn drop;
    struct ar_gen_entry *next;
}} ar_gen_entry;
static ar_gen_entry *ar_gen_head = NULL;
static ar_gen_entry *ar_gen_tail = NULL;
static uint64_t ar_gen_next_token = 0;
static uint64_t ar_gen_len = 0;
static int ar_gen_valid_layout(size_t size, size_t align) {{
    return align != 0 && (align & (align - 1)) == 0 && size <= SIZE_MAX - (align - 1);
}}
static void *ar_gen_alloc_aligned(size_t size, size_t align, void **allocation) {{
    if (!ar_gen_valid_layout(size, align)) return NULL;
    size_t bytes = size == 0 ? 1 : size;
    if (bytes > SIZE_MAX - (align - 1)) return NULL;
    void *raw = malloc(bytes + align - 1);
    if (!raw) return NULL;
    uintptr_t base = (uintptr_t)raw;
    uintptr_t aligned = (base + (align - 1)) & ~(uintptr_t)(align - 1);
    *allocation = raw;
    return (void *)aligned;
}}
static ar_gen_entry *ar_gen_find(uint64_t token) {{
    uint64_t visited = 0;
    for (ar_gen_entry *entry = ar_gen_head; entry && visited < ar_gen_len;
         entry = entry->next, visited++)
        if (entry->token == token) return entry;
    return NULL;
}}
static uint64_t ar_gen_insert_raw(void *source, size_t size, size_t align, ar_gen_drop_fn drop) {{
    if (!source || ar_gen_next_token == UINT64_MAX || ar_gen_len == UINT64_MAX) return 0;
    ar_gen_entry *entry = (ar_gen_entry *)malloc(sizeof(ar_gen_entry));
    if (!entry) return 0;
    entry->data = ar_gen_alloc_aligned(size, align, &entry->allocation);
    if (!entry->data) {{ free(entry); return 0; }}
    if (size != 0) memcpy(entry->data, source, size);
    entry->token = ++ar_gen_next_token;
    entry->size = size; entry->align = align; entry->drop = drop; entry->next = NULL;
    if (ar_gen_tail) ar_gen_tail->next = entry; else ar_gen_head = entry;
    ar_gen_tail = entry;
    ar_gen_len++;
    return entry->token;
}}
static int ar_gen_get_raw(uint64_t token, void *destination, size_t size, size_t align) {{
    ar_gen_entry *entry = ar_gen_find(token);
    if (!entry || !destination || entry->size != size || entry->align != align) return 0;
    if (size != 0) memcpy(destination, entry->data, size);
    return 1;
}}
static int ar_gen_set_raw(uint64_t token, void *source, size_t size, size_t align, ar_gen_drop_fn drop) {{
    ar_gen_entry *entry = ar_gen_find(token);
    if (!entry || !source || entry->size != size || entry->align != align) return 0;
    void *new_allocation = NULL;
    void *new_data = ar_gen_alloc_aligned(size, align, &new_allocation);
    if (!new_data) return 0;
    if (size != 0) memcpy(new_data, source, size);
    void *old_data = entry->data; void *old_allocation = entry->allocation;
    ar_gen_drop_fn old_drop = entry->drop;
    entry->data = new_data; entry->allocation = new_allocation; entry->drop = drop;
    if (old_drop) old_drop(old_data);
    free(old_allocation);
    return 1;
}}
static uint64_t ar_gen_upsert_raw(uint64_t token, void *source, size_t size, size_t align, ar_gen_drop_fn drop) {{
    if (token == 0) return ar_gen_insert_raw(source, size, align, drop);
    return ar_gen_set_raw(token, source, size, align, drop) ? token : 0;
}}
static int ar_gen_remove_raw(uint64_t token, void *destination, size_t size, size_t align) {{
    ar_gen_entry **link = &ar_gen_head;
    uint64_t visited = 0;
    while (*link && (*link)->token != token && visited < ar_gen_len) {{
        link = &(*link)->next;
        visited++;
    }}
    if (*link && visited == ar_gen_len) return 0; /* malformed/cyclic chain */
    ar_gen_entry *entry = *link;
    if (!entry || !destination || entry->size != size || entry->align != align) return 0;
    *link = entry->next;
    ar_gen_len--;
    if (ar_gen_tail == entry) {{
        ar_gen_tail = NULL;
        uint64_t remaining = ar_gen_len;
        for (ar_gen_entry *cursor = ar_gen_head; cursor && remaining > 0;
             cursor = cursor->next, remaining--) ar_gen_tail = cursor;
    }}
    if (size != 0) memcpy(destination, entry->data, size);
    free(entry->allocation); free(entry);
    return 1;
}}
static void ar_gen_shutdown_raw(void) {{
    /* Corruption can form a cycle. Refuse to chase or double-free a malformed
       chain during teardown; normal insertion only ever creates a list. */
    ar_gen_entry *slow = ar_gen_head;
    ar_gen_entry *fast = ar_gen_head;
    while (fast && fast->next) {{
        slow = slow->next;
        fast = fast->next->next;
        if (slow == fast) return;
    }}
    ar_gen_entry *entries = ar_gen_head;
    ar_gen_head = NULL; ar_gen_tail = NULL;
    uint64_t remaining = ar_gen_len;
    ar_gen_len = 0;
    while (entries && remaining > 0) {{
        ar_gen_entry *next = entries->next;
        if (entries->drop) entries->drop(entries->data);
        free(entries->allocation); free(entries);
        entries = next;
        remaining--;
    }}
}}"#
        );
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
}}"#
        );
    }
}
