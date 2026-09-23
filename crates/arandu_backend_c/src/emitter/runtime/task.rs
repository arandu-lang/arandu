//! C runtime helpers for cooperative tasks, threads, mutexes, and dynamic parallel fold.

use std::fmt::Write;

use super::super::CEmitter;

impl<'a> CEmitter<'a> {
    /// SL_R.0 cooperative task table mirror (JIT `ar_rt_*`) for the C backend.
    ///
    /// Mirrors `arandu_runtime::rt_runtime`: a growable table of slots with
    /// Pending/Running/Completed states. A Pending blob is owned by the row;
    /// a join claims it, drives it, releases it, then caches the result. A
    /// cancel releases a Pending blob or marks a Running row for retirement.
    /// Cooperative only: the joining thread drives the blob; standalone C has
    /// no OS-worker scheduling at this surface.
    pub(super) fn emit_task_runtime(&mut self) {
        let _ = writeln!(
            &mut self.output,
            r#"/* SL_R.0 cooperative task table (mirrors JIT ar_rt_*). */
typedef struct {{
    int32_t state;          /* 0 EMPTY, 1 PENDING, 2 RUNNING, 3 COMPLETED */
    int32_t cancel_requested;
    uint8_t *blob;          /* owned while PENDING; RUNNING stores none */
    int64_t result;         /* COMPLETED cache */
}} ar_rt_slot;
static ar_rt_slot *ar_rt_slots = NULL;
static uint64_t ar_rt_capacity = 0;
static uint64_t ar_rt_len = 0;

static int64_t ar_rt_spawn_i64(uint8_t *state) {{
    if (!state) abort();
    uint64_t index = ar_rt_capacity;
    for (uint64_t i = 0; i < ar_rt_len; i++) {{
        if (ar_rt_slots[i].state == 0) {{ index = i; break; }}
    }}
    if (index == ar_rt_capacity) {{
        if (ar_rt_len == ar_rt_capacity) {{
            uint64_t new_cap = ar_rt_capacity == 0 ? 8 : ar_rt_capacity * 2;
            if (new_cap > (uint64_t)INT64_MAX) abort();
            ar_rt_slot *next = (ar_rt_slot*)realloc(
                ar_rt_slots, (size_t)(new_cap * sizeof(ar_rt_slot)));
            if (!next) abort();
            memset(next + ar_rt_capacity, 0,
                (size_t)((new_cap - ar_rt_capacity) * sizeof(ar_rt_slot)));
            ar_rt_slots = next;
            ar_rt_capacity = new_cap;
        }}
        index = ar_rt_len++;
    }}
    ar_rt_slots[index].state = 1;
    ar_rt_slots[index].cancel_requested = 0;
    ar_rt_slots[index].blob = state;
    return (int64_t)index;
}}

static int64_t ar_rt_join_i64(int64_t handle) {{
    if (handle < 0 || (uint64_t)handle >= ar_rt_len) abort();
    ar_rt_slot *slot = &ar_rt_slots[(uint64_t)handle];
    if (slot->state == 3) return slot->result;
    if (slot->state != 1) abort(); /* RUNNING = concurrent join; EMPTY = invalid */
    uint8_t *blob = slot->blob;
    slot->state = 2;
    slot->cancel_requested = 0;
    int64_t result = ar_co_block_on_i64(blob);
    if (*(uint32_t*)(blob + 4) != 0x4152434fu) abort(); /* fail-closed magic */
    free(blob);
    if (slot->cancel_requested) {{
        slot->state = 0;
        slot->blob = NULL;
    }} else {{
        slot->state = 3;
        slot->result = result;
    }}
    return result;
}}

static void ar_rt_cancel_i64(int64_t handle) {{
    if (handle < 0 || (uint64_t)handle >= ar_rt_len) return;
    ar_rt_slot *slot = &ar_rt_slots[(uint64_t)handle];
    if (slot->state == 1) {{
        uint8_t *blob = slot->blob;
        slot->state = 0;
        slot->blob = NULL;
        if (*(uint32_t*)(blob + 4) != 0x4152434fu) abort(); /* fail-closed magic */
        free(blob);
    }} else if (slot->state == 2) {{
        slot->cancel_requested = 1;
    }} else if (slot->state == 3) {{
        slot->state = 0; /* cached row is retired by cancel (JIT slot.take) */
    }} /* EMPTY: no-op */
}}

static int64_t ar_rt_block_on_i64(uint8_t *state) {{
    return ar_co_block_on_i64(state);
}}

/* Structured parallelism: reusable bounded worker pool with self-help deadlock prevention. */
#define AR_PARALLEL_MAX_WORKERS 64
#define AR_PARALLEL_QUEUE_CAP 128

#if defined(_MSC_VER)
#define AR_THREAD_LOCAL __declspec(thread)
#elif defined(__GNUC__) || defined(__clang__)
#define AR_THREAD_LOCAL __thread
#else
#define AR_THREAD_LOCAL _Thread_local
#endif

typedef struct ar_parallel_group ar_parallel_group;

typedef struct ar_parallel_batch {{
    uint64_t start;
    uint64_t end;
    uint8_t **contexts;
    uint8_t **results;
    int32_t (*thunk)(uint8_t*, uint8_t*);
    _Atomic int64_t *stop_flag;
    uint64_t first_error_index;
    int32_t first_error_code;
    int canceled;
    ar_parallel_group *group;
}} ar_parallel_batch;

struct ar_parallel_group {{
#if defined(_WIN32) && !defined(__MINGW32__)
    CRITICAL_SECTION cs;
    CONDITION_VARIABLE cv;
#else
    pthread_mutex_t mutex;
    pthread_cond_t cv;
#endif
    uint64_t pending;
    _Atomic uint64_t next_chunk;
    uint64_t total_chunks;
    _Atomic int canceled;
}};

static void ar_parallel_batch_run(ar_parallel_batch *batch) {{
    if (!batch->group) {{
        batch->first_error_index = UINT64_MAX;
        batch->first_error_code = 0;
        batch->canceled = 0;
        for (uint64_t i = batch->start; i < batch->end; i++) {{
            if (batch->stop_flag && atomic_load_explicit(batch->stop_flag, memory_order_acquire) != 0) {{
                batch->canceled = 1;
                break;
            }}
            int32_t code = batch->thunk(batch->contexts[i], batch->results[i]);
            if (code != 0 && batch->first_error_index == UINT64_MAX) {{
                batch->first_error_index = i;
                batch->first_error_code = code;
            }}
        }}
        return;
    }}

    batch->first_error_index = UINT64_MAX;
    batch->first_error_code = 0;
    batch->canceled = 0;
    while (1) {{
        if (batch->stop_flag && atomic_load_explicit(batch->stop_flag, memory_order_acquire) != 0) {{
            atomic_store_explicit(&batch->group->canceled, 1, memory_order_release);
            break;
        }}
        if (atomic_load_explicit(&batch->group->canceled, memory_order_acquire)) {{
            break;
        }}
        uint64_t i = atomic_fetch_add_explicit(&batch->group->next_chunk, 1, memory_order_relaxed);
        if (i >= batch->group->total_chunks) {{
            break;
        }}
        int32_t code = batch->thunk(batch->contexts[i], batch->results[i]);
        if (code != 0) {{
            /* This batch is private to one worker. Preserve the ordinal and its
               code as one non-racing record; the caller selects the lowest
               ordinal only after every batch has joined. */
            batch->first_error_index = i;
            batch->first_error_code = code;
            if (batch->stop_flag) {{
                atomic_store_explicit(batch->stop_flag, 1, memory_order_release);
            }}
            break;
        }}
    }}
}}

static void ar_parallel_group_finish_batch(ar_parallel_group *group) {{
    if (!group) return;
#if defined(_WIN32) && !defined(__MINGW32__)
    EnterCriticalSection(&group->cs);
    group->pending--;
    if (group->pending == 0) {{
        WakeAllConditionVariable(&group->cv);
    }}
    LeaveCriticalSection(&group->cs);
#else
    pthread_mutex_lock(&group->mutex);
    group->pending--;
    if (group->pending == 0) {{
        pthread_cond_broadcast(&group->cv);
    }}
    pthread_mutex_unlock(&group->mutex);
#endif
}}

typedef struct ar_parallel_pool {{
#if defined(_WIN32) && !defined(__MINGW32__)
    CRITICAL_SECTION cs;
    CONDITION_VARIABLE cv_task;
    CONDITION_VARIABLE cv_space;
    HANDLE threads[AR_PARALLEL_MAX_WORKERS];
#else
    pthread_mutex_t mutex;
    pthread_cond_t cv_task;
    pthread_cond_t cv_space;
    pthread_t threads[AR_PARALLEL_MAX_WORKERS];
#endif
    ar_parallel_batch *queue[AR_PARALLEL_QUEUE_CAP];
    uint64_t head;
    uint64_t tail;
    uint64_t count;
    uint64_t worker_count;
    int shutdown;
    int initialized;
}} ar_parallel_pool;

static ar_parallel_pool ar_global_c_pool;
static AR_THREAD_LOCAL int ar_c_in_worker = 0;

#if defined(_WIN32) && !defined(__MINGW32__)
static DWORD WINAPI ar_parallel_pool_worker_entry(LPVOID raw) {{
    (void)raw;
    ar_c_in_worker = 1;
    while (1) {{
        EnterCriticalSection(&ar_global_c_pool.cs);
        while (ar_global_c_pool.count == 0 && !ar_global_c_pool.shutdown) {{
            SleepConditionVariableCS(&ar_global_c_pool.cv_task, &ar_global_c_pool.cs, INFINITE);
        }}
        if (ar_global_c_pool.shutdown && ar_global_c_pool.count == 0) {{
            LeaveCriticalSection(&ar_global_c_pool.cs);
            break;
        }}
        ar_parallel_batch *batch = ar_global_c_pool.queue[ar_global_c_pool.head];
        ar_global_c_pool.head = (ar_global_c_pool.head + 1) % AR_PARALLEL_QUEUE_CAP;
        ar_global_c_pool.count--;
        WakeConditionVariable(&ar_global_c_pool.cv_space);
        LeaveCriticalSection(&ar_global_c_pool.cs);

        if (batch) {{
            ar_parallel_batch_run(batch);
            ar_parallel_group_finish_batch(batch->group);
        }}
    }}
    return 0;
}}

static INIT_ONCE ar_c_pool_init_once = INIT_ONCE_STATIC_INIT;
static BOOL CALLBACK ar_c_pool_init_routine(PINIT_ONCE InitOnce, PVOID Parameter, PVOID *Context) {{
    (void)InitOnce; (void)Parameter; (void)Context;
    InitializeCriticalSection(&ar_global_c_pool.cs);
    InitializeConditionVariable(&ar_global_c_pool.cv_task);
    InitializeConditionVariable(&ar_global_c_pool.cv_space);
    ar_global_c_pool.head = 0;
    ar_global_c_pool.tail = 0;
    ar_global_c_pool.count = 0;
    ar_global_c_pool.shutdown = 0;
    SYSTEM_INFO sysinfo;
    GetSystemInfo(&sysinfo);
    uint64_t num_procs = sysinfo.dwNumberOfProcessors;
    if (num_procs < 1) num_procs = 1;
    if (num_procs > AR_PARALLEL_MAX_WORKERS) num_procs = AR_PARALLEL_MAX_WORKERS;
    ar_global_c_pool.worker_count = 0;
    for (uint64_t i = 0; i < num_procs; i++) {{
        HANDLE thread = CreateThread(NULL, 0, ar_parallel_pool_worker_entry, NULL, 0, NULL);
        if (!thread) break;
        ar_global_c_pool.threads[ar_global_c_pool.worker_count++] = thread;
    }}
    ar_global_c_pool.initialized = 1;
    return TRUE;
}}

static void ar_c_pool_ensure_init(void) {{
    InitOnceExecuteOnce(&ar_c_pool_init_once, ar_c_pool_init_routine, NULL, NULL);
}}

static void ar_c_pool_enqueue(ar_parallel_batch *batch) {{
    EnterCriticalSection(&ar_global_c_pool.cs);
    while (ar_global_c_pool.count == AR_PARALLEL_QUEUE_CAP && !ar_global_c_pool.shutdown) {{
        SleepConditionVariableCS(&ar_global_c_pool.cv_space, &ar_global_c_pool.cs, INFINITE);
    }}
    if (ar_global_c_pool.shutdown) {{
        LeaveCriticalSection(&ar_global_c_pool.cs);
        return;
    }}
    ar_global_c_pool.queue[ar_global_c_pool.tail] = batch;
    ar_global_c_pool.tail = (ar_global_c_pool.tail + 1) % AR_PARALLEL_QUEUE_CAP;
    ar_global_c_pool.count++;
    WakeConditionVariable(&ar_global_c_pool.cv_task);
    LeaveCriticalSection(&ar_global_c_pool.cs);
}}

#else

static void *ar_parallel_pool_worker_entry(void *raw) {{
    (void)raw;
    ar_c_in_worker = 1;
    while (1) {{
        pthread_mutex_lock(&ar_global_c_pool.mutex);
        while (ar_global_c_pool.count == 0 && !ar_global_c_pool.shutdown) {{
            pthread_cond_wait(&ar_global_c_pool.cv_task, &ar_global_c_pool.mutex);
        }}
        if (ar_global_c_pool.shutdown && ar_global_c_pool.count == 0) {{
            pthread_mutex_unlock(&ar_global_c_pool.mutex);
            break;
        }}
        ar_parallel_batch *batch = ar_global_c_pool.queue[ar_global_c_pool.head];
        ar_global_c_pool.head = (ar_global_c_pool.head + 1) % AR_PARALLEL_QUEUE_CAP;
        ar_global_c_pool.count--;
        pthread_cond_signal(&ar_global_c_pool.cv_space);
        pthread_mutex_unlock(&ar_global_c_pool.mutex);

        if (batch) {{
            ar_parallel_batch_run(batch);
            ar_parallel_group_finish_batch(batch->group);
        }}
    }}
    return NULL;
}}

static pthread_once_t ar_c_pool_init_once = PTHREAD_ONCE_INIT;
static void ar_c_pool_init_routine(void) {{
    pthread_mutex_init(&ar_global_c_pool.mutex, NULL);
    pthread_cond_init(&ar_global_c_pool.cv_task, NULL);
    pthread_cond_init(&ar_global_c_pool.cv_space, NULL);
    ar_global_c_pool.head = 0;
    ar_global_c_pool.tail = 0;
    ar_global_c_pool.count = 0;
    ar_global_c_pool.shutdown = 0;
#if defined(_WIN32)
    SYSTEM_INFO sysinfo;
    GetSystemInfo(&sysinfo);
    long procs = (long)sysinfo.dwNumberOfProcessors;
#elif defined(__APPLE__)
    int logical_procs = 1;
    size_t logical_procs_size = sizeof(logical_procs);
    long procs = sysctlbyname("hw.logicalcpu", &logical_procs, &logical_procs_size, NULL, 0) == 0
        ? (long)logical_procs
        : 1;
#else
    long procs = sysconf(_SC_NPROCESSORS_ONLN);
#endif
    uint64_t num_procs = procs > 0 ? (uint64_t)procs : 1;
    if (num_procs < 1) num_procs = 1;
    if (num_procs > AR_PARALLEL_MAX_WORKERS) num_procs = AR_PARALLEL_MAX_WORKERS;
    ar_global_c_pool.worker_count = 0;
    for (uint64_t i = 0; i < num_procs; i++) {{
        if (pthread_create(
                &ar_global_c_pool.threads[ar_global_c_pool.worker_count],
                NULL,
                ar_parallel_pool_worker_entry,
                NULL) != 0) break;
        ar_global_c_pool.worker_count++;
    }}
    ar_global_c_pool.initialized = 1;
}}

static void ar_c_pool_ensure_init(void) {{
    pthread_once(&ar_c_pool_init_once, ar_c_pool_init_routine);
}}

static void ar_c_pool_enqueue(ar_parallel_batch *batch) {{
    pthread_mutex_lock(&ar_global_c_pool.mutex);
    while (ar_global_c_pool.count == AR_PARALLEL_QUEUE_CAP && !ar_global_c_pool.shutdown) {{
        pthread_cond_wait(&ar_global_c_pool.cv_space, &ar_global_c_pool.mutex);
    }}
    if (ar_global_c_pool.shutdown) {{
        pthread_mutex_unlock(&ar_global_c_pool.mutex);
        return;
    }}
    ar_global_c_pool.queue[ar_global_c_pool.tail] = batch;
    ar_global_c_pool.tail = (ar_global_c_pool.tail + 1) % AR_PARALLEL_QUEUE_CAP;
    ar_global_c_pool.count++;
    pthread_cond_signal(&ar_global_c_pool.cv_task);
    pthread_mutex_unlock(&ar_global_c_pool.mutex);
}}

#endif

static int32_t ar_rt_parallel_fold_run(
    uint64_t num_chunks,
    uint8_t **contexts,
    int32_t (*thunk)(uint8_t*, uint8_t*),
    uint8_t **results,
    uint64_t workers,
    int64_t *stop_flag
) {{
    if (num_chunks == 0) return 0;
    if (!contexts || !results || !thunk) return 1;
    uint64_t worker_count = workers == 0 ? 1 : workers;
    if (worker_count > AR_PARALLEL_MAX_WORKERS) worker_count = AR_PARALLEL_MAX_WORKERS;
    if (worker_count > num_chunks) worker_count = num_chunks;
    _Atomic int64_t *atomic_stop = (_Atomic int64_t*)stop_flag;

    /* Pre-admission cancellation check (JDK-8311867) */
    if (atomic_stop && atomic_load_explicit(atomic_stop, memory_order_acquire) != 0) {{
        return 2;
    }}

    /* Nested execution inside an existing pool worker runs inline (self-help) to prevent deadlock.
       Single-worker requests also execute inline directly without queue overhead. */
    if (worker_count > 1 && !ar_c_in_worker) {{
        ar_c_pool_ensure_init();
        /* The requested parallelism includes this calling thread. Clamp to the
           workers that actually started, so partial thread-creation failure
           can never leave queued batches with no worker to claim them. */
        uint64_t available_workers = ar_global_c_pool.worker_count + 1;
        if (worker_count > available_workers) worker_count = available_workers;
    }}
    if (worker_count == 1 || ar_c_in_worker) {{
        ar_parallel_batch batch = {{
            0, num_chunks, contexts, results, thunk, atomic_stop, UINT64_MAX, 0, 0, NULL
        }};
        ar_parallel_batch_run(&batch);
        if (batch.first_error_index != UINT64_MAX) {{
            if (atomic_stop) atomic_store_explicit(atomic_stop, 1, memory_order_release);
            return batch.first_error_code;
        }}
        return batch.canceled ? 2 : 0;
    }}

    ar_parallel_group group;
    group.pending = worker_count;
    atomic_init(&group.next_chunk, 0);
    group.total_chunks = num_chunks;
    atomic_init(&group.canceled, 0);
#if defined(_WIN32) && !defined(__MINGW32__)
    InitializeCriticalSection(&group.cs);
    InitializeConditionVariable(&group.cv);
#else
    pthread_mutex_init(&group.mutex, NULL);
    pthread_cond_init(&group.cv, NULL);
#endif

    ar_parallel_batch batches[AR_PARALLEL_MAX_WORKERS];
    for (uint64_t i = 0; i < worker_count; i++) {{
        batches[i] = (ar_parallel_batch){{
            0, num_chunks, contexts, results, thunk, atomic_stop, UINT64_MAX, 0, 0, &group
        }};
    }}

    /* Dispatch (worker_count - 1) batches to reusable worker pool */
    for (uint64_t i = 0; i < worker_count - 1; i++) {{
        ar_c_pool_enqueue(&batches[i]);
    }}

    /* Work sharing: calling thread directly runs dynamically as well */
    ar_parallel_batch_run(&batches[worker_count - 1]);
    ar_parallel_group_finish_batch(&group);

    /* Await completion of all batches in this structured group */
#if defined(_WIN32) && !defined(__MINGW32__)
    EnterCriticalSection(&group.cs);
    while (group.pending > 0) {{
        SleepConditionVariableCS(&group.cv, &group.cs, INFINITE);
    }}
    LeaveCriticalSection(&group.cs);
    DeleteCriticalSection(&group.cs);
#else
    pthread_mutex_lock(&group.mutex);
    while (group.pending > 0) {{
        pthread_cond_wait(&group.cv, &group.mutex);
    }}
    pthread_mutex_unlock(&group.mutex);
    pthread_mutex_destroy(&group.mutex);
    pthread_cond_destroy(&group.cv);
#endif

    uint64_t first_error_index = UINT64_MAX;
    int32_t first_error_code = 0;
    for (uint64_t i = 0; i < worker_count; i++) {{
        if (batches[i].first_error_index < first_error_index) {{
            first_error_index = batches[i].first_error_index;
            first_error_code = batches[i].first_error_code;
        }}
    }}
    if (first_error_index != UINT64_MAX) {{
        if (atomic_stop) atomic_store_explicit(atomic_stop, 1, memory_order_release);
        return first_error_code;
    }}
    return atomic_load_explicit(&group.canceled, memory_order_acquire) ? 2 : 0;
}}"#
        );
    }
}
