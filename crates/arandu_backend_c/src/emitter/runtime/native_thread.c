
/* Keep in sync with arandu_runtime::native_thread. No language unwinding. */
typedef int32_t (*ar_native_entry)(uint8_t*, uint8_t*);
typedef void (*ar_native_drop)(uint8_t*);
typedef struct ar_native_thread {
    size_t id;
    uint8_t *context, *result;
    size_t context_size, context_align, result_size, result_align;
    ar_native_entry entry;
    ar_native_drop drop_context, drop_result;
    int32_t status;
    atomic_bool finished;
    ArPalThread thread;
    struct ar_native_thread *next; /* id index chain */
} ar_native_thread;
/* Handles are looked up by id on join/drop/is_finished: index them so each
   lookup is O(1) expected instead of a walk over every live thread. */
#define AR_NATIVE_BUCKETS 1024
static ar_native_thread *ar_native_threads[AR_NATIVE_BUCKETS];
static ar_native_thread **ar_native_bucket(size_t id) {
    uint64_t mixed = (uint64_t)id * UINT64_C(0x9E3779B97F4A7C15);
    return &ar_native_threads[(size_t)(mixed >> 32) % AR_NATIVE_BUCKETS];
}
static size_t ar_native_next_id = 1;
static atomic_bool ar_native_table_lock = false;
static void ar_native_spin_hint(void) {
    ar_pal_spin_hint();
}
static void ar_native_yield(void) {
    ar_pal_yield();
}
static void ar_native_lock(void) {
    unsigned int spins = 0;
    for (;;) {
        if (!atomic_load_explicit(&ar_native_table_lock, memory_order_relaxed)
            && !atomic_exchange_explicit(&ar_native_table_lock, true, memory_order_acquire))
            return;
        ar_native_spin_hint();
        if (++spins == 64) {
            ar_native_yield();
            spins = 0;
        }
    }
}
static void ar_native_unlock(void) {
    atomic_store_explicit(&ar_native_table_lock, false, memory_order_release);
}
static void ar_native_execute(ar_native_thread *task) {
    task->status = task->entry(task->context, task->result);
    ar_rt_free_aligned(task->context, task->context_size, task->context_align);
    task->context = NULL;
#if defined(AR_GEN_ARENA_RUNTIME)
    /* Mirrors the JIT thread_local! registry drop at thread exit. */
    ar_gen_shutdown_raw();
#endif
    atomic_store_explicit(&task->finished, true, memory_order_release);
}
static void ar_native_start(void *raw) {
    ar_native_execute((ar_native_thread *)raw);
}
static bool ar_native_valid_buffer(uint8_t *pointer, size_t size, size_t align) {
    return pointer && size && align && !(align & (align - 1))
        && size <= PTRDIFF_MAX && !((uintptr_t)pointer & (align - 1));
}
static size_t ar_rt_native_thread_spawn(
    uint8_t *context, size_t context_size, size_t context_align,
    uint8_t *entry, uint8_t *drop_context,
    uint8_t *result, size_t result_size, size_t result_align, uint8_t *drop_result
) {
    if (!ar_native_valid_buffer(context, context_size, context_align)
        || !ar_native_valid_buffer(result, result_size, result_align)
        || !entry || !drop_context || !drop_result) return 0;
    ar_native_thread *task = (ar_native_thread*)calloc(1, sizeof(*task));
    if (!task) {
        ((ar_native_drop)drop_context)(context);
        ar_rt_free_aligned(context, context_size, context_align);
        ar_rt_free_aligned(result, result_size, result_align);
        return 0;
    }
    task->context = context; task->context_size = context_size; task->context_align = context_align;
    task->result = result; task->result_size = result_size; task->result_align = result_align;
    task->entry = (ar_native_entry)entry;
    task->drop_context = (ar_native_drop)drop_context;
    task->drop_result = (ar_native_drop)drop_result;
    atomic_init(&task->finished, false);
    ar_native_lock();
    size_t id = ar_native_next_id;
    if (id) ar_native_next_id = id == SIZE_MAX ? 0 : id + 1;
    ar_native_unlock();
    if (!id) goto failed;
    task->id = id;
    if (ar_pal_thread_spawn(&task->thread, ar_native_start, task) != 0) goto failed;
    ar_native_lock();
    ar_native_thread **bucket = ar_native_bucket(id);
    task->next = *bucket;
    *bucket = task;
    ar_native_unlock();
    return id;
failed:
    task->drop_context(task->context);
    ar_rt_free_aligned(task->context, task->context_size, task->context_align);
    ar_rt_free_aligned(task->result, task->result_size, task->result_align);
    free(task);
    return 0;
}
static ar_native_thread *ar_native_take(size_t id) {
    ar_native_lock();
    ar_native_thread **slot = ar_native_bucket(id);
    while (*slot && (*slot)->id != id) slot = &(*slot)->next;
    ar_native_thread *task = *slot;
    if (task) *slot = task->next;
    ar_native_unlock();
    if (task) {
        ar_pal_thread_join(&task->thread);
    }
    return task;
}
static bool ar_native_is_current_thread(size_t id) {
    ar_native_lock();
    ar_native_thread *task = *ar_native_bucket(id);
    while (task && task->id != id) task = task->next;
    bool is_current = task && ar_pal_thread_is_current(&task->thread);
    ar_native_unlock();
    return is_current;
}
static uint8_t *ar_rt_native_thread_join(size_t id) {
    ar_native_thread *task = ar_native_take(id);
    if (!task) return NULL;
    uint8_t *result = task->result;
    if (task->status != 0) {
        ar_rt_free_aligned(result, task->result_size, task->result_align);
        result = (uint8_t*)1;
    }
    free(task);
    return result;
}
static uint8_t *ar_rt_native_thread_panicked_sentinel(void) {
    return (uint8_t*)1;
}
static bool ar_rt_native_thread_is_finished(size_t id) {
    ar_native_lock();
    ar_native_thread *task = *ar_native_bucket(id);
    while (task && task->id != id) task = task->next;
    bool finished = task && atomic_load_explicit(&task->finished, memory_order_acquire);
    ar_native_unlock();
    return finished;
}
static void ar_rt_native_thread_drop(size_t id) {
    /* Self-drop leaves the handle registered for its owner or the JIT barrier. */
    if (ar_native_is_current_thread(id)) return;
    ar_native_thread *task = ar_native_take(id);
    if (!task) return;
    if (task->status == 0) task->drop_result(task->result);
    ar_rt_free_aligned(task->result, task->result_size, task->result_align);
    free(task);
}
