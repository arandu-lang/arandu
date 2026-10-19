
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
#if defined(_WIN32)
    HANDLE thread;
#else
    pthread_t thread;
#endif
    struct ar_native_thread *next;
} ar_native_thread;
static ar_native_thread *ar_native_threads = NULL;
static size_t ar_native_next_id = 1;
static atomic_flag ar_native_table_lock = ATOMIC_FLAG_INIT;
static void ar_native_lock(void) {
    while (atomic_flag_test_and_set_explicit(&ar_native_table_lock, memory_order_acquire)) {}
}
static void ar_native_unlock(void) {
    atomic_flag_clear_explicit(&ar_native_table_lock, memory_order_release);
}
static void ar_native_execute(ar_native_thread *task) {
    task->status = task->entry(task->context, task->result);
    ar_rt_free_aligned(task->context, task->context_size, task->context_align);
    task->context = NULL;
    atomic_store_explicit(&task->finished, true, memory_order_release);
}
#if defined(_WIN32)
static DWORD WINAPI ar_native_start(LPVOID raw) {
    ar_native_execute((ar_native_thread*)raw);
    return 0;
}
#else
static void *ar_native_start(void *raw) {
    ar_native_execute((ar_native_thread*)raw);
    return NULL;
}
#endif
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
#if defined(_WIN32)
    task->thread = CreateThread(NULL, 0, ar_native_start, task, 0, NULL);
    if (!task->thread) goto failed;
#else
    if (pthread_create(&task->thread, NULL, ar_native_start, task)) goto failed;
#endif
    ar_native_lock();
    task->next = ar_native_threads;
    ar_native_threads = task;
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
    ar_native_thread **slot = &ar_native_threads;
    while (*slot && (*slot)->id != id) slot = &(*slot)->next;
    ar_native_thread *task = *slot;
    if (task) *slot = task->next;
    ar_native_unlock();
    if (task) {
#if defined(_WIN32)
        if (WaitForSingleObject(task->thread, INFINITE) != WAIT_OBJECT_0) abort();
        if (!CloseHandle(task->thread)) abort();
#else
        if (pthread_join(task->thread, NULL)) abort();
#endif
    }
    return task;
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
    ar_native_thread *task = ar_native_threads;
    while (task && task->id != id) task = task->next;
    bool finished = task && atomic_load_explicit(&task->finished, memory_order_acquire);
    ar_native_unlock();
    return finished;
}
static void ar_rt_native_thread_drop(size_t id) {
    ar_native_thread *task = ar_native_take(id);
    if (!task) return;
    if (task->status == 0) task->drop_result(task->result);
    ar_rt_free_aligned(task->result, task->result_size, task->result_align);
    free(task);
}
