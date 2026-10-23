/* G4 type-erased GenRef ABI: monotonic tokens, target layout, ordered drops.
   A doubly linked list keeps insertion order for shutdown drops; a chained
   token index makes find/remove O(1) expected instead of a list walk. */
typedef void (*ar_gen_drop_fn)(void *);
typedef struct ar_gen_entry {
    uint64_t token;
    void *data;
    void *allocation;
    size_t size;
    size_t align;
    ar_gen_drop_fn drop;
    struct ar_gen_entry *next, *prev;
    struct ar_gen_entry *bucket_next;
} ar_gen_entry;
static ar_gen_entry *ar_gen_head = NULL;
static ar_gen_entry *ar_gen_tail = NULL;
static ar_gen_entry **ar_gen_buckets = NULL;
static size_t ar_gen_bucket_count = 0; /* zero or a power of two */
static uint64_t ar_gen_next_token = 0;
static uint64_t ar_gen_len = 0;
static int ar_gen_valid_layout(size_t size, size_t align) {
    return align != 0 && (align & (align - 1)) == 0 && size <= SIZE_MAX - (align - 1);
}
static void *ar_gen_alloc_aligned(size_t size, size_t align, void **allocation) {
    if (!ar_gen_valid_layout(size, align)) return NULL;
    size_t bytes = size == 0 ? 1 : size;
    if (bytes > SIZE_MAX - (align - 1)) return NULL;
    void *raw = malloc(bytes + align - 1);
    if (!raw) return NULL;
    uintptr_t base = (uintptr_t)raw;
    uintptr_t aligned = (base + (align - 1)) & ~(uintptr_t)(align - 1);
    *allocation = raw;
    return (void *)aligned;
}
static size_t ar_gen_bucket_of(uint64_t token, size_t count) {
    return (size_t)((token * UINT64_C(0x9E3779B97F4A7C15)) >> 32) & (count - 1);
}
/* Keeps load <= 1. Growth failure keeps the current index: lookups stay
   correct, only chains get longer. Fails only when no index exists yet. */
static int ar_gen_reserve_index(void) {
    if (ar_gen_bucket_count != 0 && ar_gen_len < ar_gen_bucket_count) return 1;
    size_t count = ar_gen_bucket_count == 0 ? 16 : ar_gen_bucket_count * 2;
    if (count < ar_gen_bucket_count || count > SIZE_MAX / sizeof(ar_gen_entry *))
        return ar_gen_bucket_count != 0;
    ar_gen_entry **buckets = (ar_gen_entry **)calloc(count, sizeof(ar_gen_entry *));
    if (!buckets) return ar_gen_bucket_count != 0;
    for (ar_gen_entry *entry = ar_gen_head; entry; entry = entry->next) {
        size_t at = ar_gen_bucket_of(entry->token, count);
        entry->bucket_next = buckets[at];
        buckets[at] = entry;
    }
    free(ar_gen_buckets);
    ar_gen_buckets = buckets;
    ar_gen_bucket_count = count;
    return 1;
}
static ar_gen_entry **ar_gen_bucket_link(uint64_t token) {
    if (ar_gen_bucket_count == 0) return NULL;
    ar_gen_entry **link = &ar_gen_buckets[ar_gen_bucket_of(token, ar_gen_bucket_count)];
    while (*link && (*link)->token != token) link = &(*link)->bucket_next;
    return link;
}
static ar_gen_entry *ar_gen_find(uint64_t token) {
    ar_gen_entry **link = ar_gen_bucket_link(token);
    return link ? *link : NULL;
}
static uint64_t ar_gen_insert_raw(void *source, size_t size, size_t align, ar_gen_drop_fn drop) {
    if (!source || ar_gen_next_token == UINT64_MAX || ar_gen_len == UINT64_MAX) return 0;
    ar_gen_entry *entry = (ar_gen_entry *)malloc(sizeof(ar_gen_entry));
    if (!entry) return 0;
    entry->data = ar_gen_alloc_aligned(size, align, &entry->allocation);
    if (!entry->data) { free(entry); return 0; }
    /* Reserve every fallible resource before taking the payload bytes. */
    if (!ar_gen_reserve_index()) { free(entry->allocation); free(entry); return 0; }
    if (size != 0) memcpy(entry->data, source, size);
    entry->token = ++ar_gen_next_token;
    entry->size = size; entry->align = align; entry->drop = drop;
    entry->next = NULL; entry->prev = ar_gen_tail;
    if (ar_gen_tail) ar_gen_tail->next = entry; else ar_gen_head = entry;
    ar_gen_tail = entry;
    size_t at = ar_gen_bucket_of(entry->token, ar_gen_bucket_count);
    entry->bucket_next = ar_gen_buckets[at];
    ar_gen_buckets[at] = entry;
    ar_gen_len++;
    return entry->token;
}
static int ar_gen_get_raw(uint64_t token, void *destination, size_t size, size_t align) {
    ar_gen_entry *entry = ar_gen_find(token);
    if (!entry || !destination || entry->size != size || entry->align != align) return 0;
    if (size != 0) memcpy(destination, entry->data, size);
    return 1;
}
static int ar_gen_set_raw(uint64_t token, void *source, size_t size, size_t align, ar_gen_drop_fn drop) {
    ar_gen_entry *entry = ar_gen_find(token);
    if (!entry || !source || entry->size != size || entry->align != align) return 0;
    void *new_allocation = NULL;
    void *new_data = ar_gen_alloc_aligned(size, align, &new_allocation);
    if (!new_data) return 0;
    if (size != 0) memcpy(new_data, source, size);
    void *old_data = entry->data; void *old_allocation = entry->allocation;
    ar_gen_drop_fn old_drop = entry->drop;
    entry->data = new_data; entry->allocation = new_allocation; entry->drop = drop;
    /* The entry is consistent before drop glue runs, so a reentrant remove of
       this token is safe; the entry is not touched afterwards. */
    if (old_drop) old_drop(old_data);
    free(old_allocation);
    return 1;
}
static uint64_t ar_gen_upsert_raw(uint64_t token, void *source, size_t size, size_t align, ar_gen_drop_fn drop) {
    if (token == 0) return ar_gen_insert_raw(source, size, align, drop);
    return ar_gen_set_raw(token, source, size, align, drop) ? token : 0;
}
static int ar_gen_remove_raw(uint64_t token, void *destination, size_t size, size_t align) {
    ar_gen_entry **link = ar_gen_bucket_link(token);
    ar_gen_entry *entry = link ? *link : NULL;
    if (!entry || !destination || entry->size != size || entry->align != align) return 0;
    *link = entry->bucket_next;
    if (entry->prev) entry->prev->next = entry->next; else ar_gen_head = entry->next;
    if (entry->next) entry->next->prev = entry->prev; else ar_gen_tail = entry->prev;
    ar_gen_len--;
    if (size != 0) memcpy(destination, entry->data, size);
    free(entry->allocation); free(entry);
    return 1;
}
static void ar_gen_shutdown_raw(void) {
    /* Detach everything first: drop glue that reenters the arena sees an
       empty registry instead of entries that are being freed. */
    ar_gen_entry *entries = ar_gen_head;
    uint64_t remaining = ar_gen_len;
    ar_gen_head = NULL; ar_gen_tail = NULL; ar_gen_len = 0;
    free(ar_gen_buckets);
    ar_gen_buckets = NULL; ar_gen_bucket_count = 0;
    while (entries && remaining > 0) {
        ar_gen_entry *next = entries->next;
        if (entries->drop) entries->drop(entries->data);
        free(entries->allocation); free(entries);
        entries = next;
        remaining--;
    }
}
