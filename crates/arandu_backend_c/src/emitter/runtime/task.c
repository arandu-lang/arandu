/* Platform-width unsigned integer matching what the C emitter produces for
   `usize` (uint64_t on LP64/LLP64, uint32_t on ILP32). Used for thread IDs
   shared between static runtime helpers and emitted extern declarations. */
#if UINTPTR_MAX == UINT64_MAX
typedef uint64_t ar_usize_t;
#else
typedef uint32_t ar_usize_t;
#endif

/* SL_R.0 cooperative task table (mirrors JIT ar_rt_*). */
typedef struct {
    int32_t state;          /* 0 EMPTY, 1 PENDING, 2 RUNNING, 3 COMPLETED */
    int32_t cancel_requested;
    uint8_t *blob;          /* owned while PENDING; RUNNING stores none */
    int64_t result;         /* COMPLETED cache */
} ar_rt_slot;
static ar_rt_slot *ar_rt_slots = NULL;
static uint64_t ar_rt_capacity = 0;
static uint64_t ar_rt_len = 0;

static int64_t ar_rt_spawn_i64(uint8_t *state) {
    if (!state) abort();
    uint64_t index = ar_rt_capacity;
    for (uint64_t i = 0; i < ar_rt_len; i++) {
        if (ar_rt_slots[i].state == 0) { index = i; break; }
    }
    if (index == ar_rt_capacity) {
        if (ar_rt_len == ar_rt_capacity) {
            uint64_t new_cap = ar_rt_capacity == 0 ? 8 : ar_rt_capacity * 2;
            if (new_cap > (uint64_t)INT64_MAX) abort();
            ar_rt_slot *next = (ar_rt_slot*)realloc(
                ar_rt_slots, (size_t)(new_cap * sizeof(ar_rt_slot)));
            if (!next) abort();
            memset(next + ar_rt_capacity, 0,
                (size_t)((new_cap - ar_rt_capacity) * sizeof(ar_rt_slot)));
            ar_rt_slots = next;
            ar_rt_capacity = new_cap;
        }
        index = ar_rt_len++;
    }
    ar_rt_slots[index].state = 1;
    ar_rt_slots[index].cancel_requested = 0;
    ar_rt_slots[index].blob = state;
    return (int64_t)index;
}

static int64_t ar_rt_join_i64(int64_t handle) {
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
    if (slot->cancel_requested) {
        slot->state = 0;
        slot->blob = NULL;
    } else {
        slot->state = 3;
        slot->result = result;
    }
    return result;
}

static void ar_rt_cancel_i64(int64_t handle) {
    if (handle < 0 || (uint64_t)handle >= ar_rt_len) return;
    ar_rt_slot *slot = &ar_rt_slots[(uint64_t)handle];
    if (slot->state == 1) {
        uint8_t *blob = slot->blob;
        slot->state = 0;
        slot->blob = NULL;
        if (*(uint32_t*)(blob + 4) != 0x4152434fu) abort(); /* fail-closed magic */
        free(blob);
    } else if (slot->state == 2) {
        slot->cancel_requested = 1;
    } else if (slot->state == 3) {
        slot->state = 0; /* cached row is retired by cancel (JIT slot.take) */
    } /* EMPTY: no-op */
}

static int64_t ar_rt_block_on_i64(uint8_t *state) {
    return ar_co_block_on_i64(state);
}

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

typedef struct ar_parallel_batch {
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
} ar_parallel_batch;

struct ar_parallel_group {
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
};

static void ar_parallel_batch_run(ar_parallel_batch *batch) {
    if (!batch->group) {
        batch->first_error_index = UINT64_MAX;
        batch->first_error_code = 0;
        batch->canceled = 0;
        for (uint64_t i = batch->start; i < batch->end; i++) {
            if (batch->stop_flag && atomic_load_explicit(batch->stop_flag, memory_order_acquire) != 0) {
                batch->canceled = 1;
                break;
            }
            int32_t code = batch->thunk(batch->contexts[i], batch->results[i]);
            if (code != 0 && batch->first_error_index == UINT64_MAX) {
                batch->first_error_index = i;
                batch->first_error_code = code;
            }
        }
        return;
    }

    batch->first_error_index = UINT64_MAX;
    batch->first_error_code = 0;
    batch->canceled = 0;
    while (1) {
        if (batch->stop_flag && atomic_load_explicit(batch->stop_flag, memory_order_acquire) != 0) {
            atomic_store_explicit(&batch->group->canceled, 1, memory_order_release);
            break;
        }
        if (atomic_load_explicit(&batch->group->canceled, memory_order_acquire)) {
            break;
        }
        uint64_t i = atomic_fetch_add_explicit(&batch->group->next_chunk, 1, memory_order_relaxed);
        if (i >= batch->group->total_chunks) {
            break;
        }
        int32_t code = batch->thunk(batch->contexts[i], batch->results[i]);
        if (code != 0) {
            /* This batch is private to one worker. Preserve the ordinal and its
               code as one non-racing record; the caller selects the lowest
               ordinal only after every batch has joined. */
            batch->first_error_index = i;
            batch->first_error_code = code;
            if (batch->stop_flag) {
                atomic_store_explicit(batch->stop_flag, 1, memory_order_release);
            }
            break;
        }
    }
}

static void ar_parallel_group_finish_batch(ar_parallel_group *group) {
    if (!group) return;
#if defined(_WIN32) && !defined(__MINGW32__)
    EnterCriticalSection(&group->cs);
    group->pending--;
    if (group->pending == 0) {
        WakeAllConditionVariable(&group->cv);
    }
    LeaveCriticalSection(&group->cs);
#else
    pthread_mutex_lock(&group->mutex);
    group->pending--;
    if (group->pending == 0) {
        pthread_cond_broadcast(&group->cv);
    }
    pthread_mutex_unlock(&group->mutex);
#endif
}

typedef struct ar_parallel_pool {
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
} ar_parallel_pool;

static ar_parallel_pool ar_global_c_pool;
static AR_THREAD_LOCAL int ar_c_in_worker = 0;

#if defined(_WIN32) && !defined(__MINGW32__)
static DWORD WINAPI ar_parallel_pool_worker_entry(LPVOID raw) {
    (void)raw;
    ar_c_in_worker = 1;
    while (1) {
        EnterCriticalSection(&ar_global_c_pool.cs);
        while (ar_global_c_pool.count == 0 && !ar_global_c_pool.shutdown) {
            SleepConditionVariableCS(&ar_global_c_pool.cv_task, &ar_global_c_pool.cs, INFINITE);
        }
        if (ar_global_c_pool.shutdown && ar_global_c_pool.count == 0) {
            LeaveCriticalSection(&ar_global_c_pool.cs);
            break;
        }
        ar_parallel_batch *batch = ar_global_c_pool.queue[ar_global_c_pool.head];
        ar_global_c_pool.head = (ar_global_c_pool.head + 1) % AR_PARALLEL_QUEUE_CAP;
        ar_global_c_pool.count--;
        WakeConditionVariable(&ar_global_c_pool.cv_space);
        LeaveCriticalSection(&ar_global_c_pool.cs);

        if (batch) {
            ar_parallel_batch_run(batch);
            ar_parallel_group_finish_batch(batch->group);
        }
    }
    return 0;
}

static INIT_ONCE ar_c_pool_init_once = INIT_ONCE_STATIC_INIT;
static BOOL CALLBACK ar_c_pool_init_routine(PINIT_ONCE InitOnce, PVOID Parameter, PVOID *Context) {
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
    for (uint64_t i = 0; i < num_procs; i++) {
        HANDLE thread = CreateThread(NULL, 0, ar_parallel_pool_worker_entry, NULL, 0, NULL);
        if (!thread) break;
        ar_global_c_pool.threads[ar_global_c_pool.worker_count++] = thread;
    }
    ar_global_c_pool.initialized = 1;
    return TRUE;
}

static void ar_c_pool_ensure_init(void) {
    InitOnceExecuteOnce(&ar_c_pool_init_once, ar_c_pool_init_routine, NULL, NULL);
}

static void ar_c_pool_enqueue(ar_parallel_batch *batch) {
    EnterCriticalSection(&ar_global_c_pool.cs);
    while (ar_global_c_pool.count == AR_PARALLEL_QUEUE_CAP && !ar_global_c_pool.shutdown) {
        SleepConditionVariableCS(&ar_global_c_pool.cv_space, &ar_global_c_pool.cs, INFINITE);
    }
    if (ar_global_c_pool.shutdown) {
        LeaveCriticalSection(&ar_global_c_pool.cs);
        return;
    }
    ar_global_c_pool.queue[ar_global_c_pool.tail] = batch;
    ar_global_c_pool.tail = (ar_global_c_pool.tail + 1) % AR_PARALLEL_QUEUE_CAP;
    ar_global_c_pool.count++;
    WakeConditionVariable(&ar_global_c_pool.cv_task);
    LeaveCriticalSection(&ar_global_c_pool.cs);
}

#else

static void *ar_parallel_pool_worker_entry(void *raw) {
    (void)raw;
    ar_c_in_worker = 1;
    while (1) {
        pthread_mutex_lock(&ar_global_c_pool.mutex);
        while (ar_global_c_pool.count == 0 && !ar_global_c_pool.shutdown) {
            pthread_cond_wait(&ar_global_c_pool.cv_task, &ar_global_c_pool.mutex);
        }
        if (ar_global_c_pool.shutdown && ar_global_c_pool.count == 0) {
            pthread_mutex_unlock(&ar_global_c_pool.mutex);
            break;
        }
        ar_parallel_batch *batch = ar_global_c_pool.queue[ar_global_c_pool.head];
        ar_global_c_pool.head = (ar_global_c_pool.head + 1) % AR_PARALLEL_QUEUE_CAP;
        ar_global_c_pool.count--;
        pthread_cond_signal(&ar_global_c_pool.cv_space);
        pthread_mutex_unlock(&ar_global_c_pool.mutex);

        if (batch) {
            ar_parallel_batch_run(batch);
            ar_parallel_group_finish_batch(batch->group);
        }
    }
    return NULL;
}

static pthread_once_t ar_c_pool_init_once = PTHREAD_ONCE_INIT;
static void ar_c_pool_init_routine(void) {
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
    for (uint64_t i = 0; i < num_procs; i++) {
        if (pthread_create(
                &ar_global_c_pool.threads[ar_global_c_pool.worker_count],
                NULL,
                ar_parallel_pool_worker_entry,
                NULL) != 0) break;
        ar_global_c_pool.worker_count++;
    }
    ar_global_c_pool.initialized = 1;
}

static void ar_c_pool_ensure_init(void) {
    pthread_once(&ar_c_pool_init_once, ar_c_pool_init_routine);
}

static void ar_c_pool_enqueue(ar_parallel_batch *batch) {
    pthread_mutex_lock(&ar_global_c_pool.mutex);
    while (ar_global_c_pool.count == AR_PARALLEL_QUEUE_CAP && !ar_global_c_pool.shutdown) {
        pthread_cond_wait(&ar_global_c_pool.cv_space, &ar_global_c_pool.mutex);
    }
    if (ar_global_c_pool.shutdown) {
        pthread_mutex_unlock(&ar_global_c_pool.mutex);
        return;
    }
    ar_global_c_pool.queue[ar_global_c_pool.tail] = batch;
    ar_global_c_pool.tail = (ar_global_c_pool.tail + 1) % AR_PARALLEL_QUEUE_CAP;
    ar_global_c_pool.count++;
    pthread_cond_signal(&ar_global_c_pool.cv_task);
    pthread_mutex_unlock(&ar_global_c_pool.mutex);
}

#endif

static int32_t ar_rt_parallel_fold_run(
    uint64_t num_chunks,
    uint8_t **contexts,
    int32_t (*thunk)(uint8_t*, uint8_t*),
    uint8_t **results,
    uint64_t workers,
    int64_t *stop_flag
) {
    if (num_chunks == 0) return 0;
    if (!contexts || !results || !thunk) return 1;
    uint64_t worker_count = workers == 0 ? 1 : workers;
    if (worker_count > AR_PARALLEL_MAX_WORKERS) worker_count = AR_PARALLEL_MAX_WORKERS;
    if (worker_count > num_chunks) worker_count = num_chunks;
    _Atomic int64_t *atomic_stop = (_Atomic int64_t*)stop_flag;

    /* Pre-admission cancellation check (JDK-8311867) */
    if (atomic_stop && atomic_load_explicit(atomic_stop, memory_order_acquire) != 0) {
        return 2;
    }

    /* Nested execution inside an existing pool worker runs inline (self-help) to prevent deadlock.
       Single-worker requests also execute inline directly without queue overhead. */
    if (worker_count > 1 && !ar_c_in_worker) {
        ar_c_pool_ensure_init();
        /* The requested parallelism includes this calling thread. Clamp to the
           workers that actually started, so partial thread-creation failure
           can never leave queued batches with no worker to claim them. */
        uint64_t available_workers = ar_global_c_pool.worker_count + 1;
        if (worker_count > available_workers) worker_count = available_workers;
    }
    if (worker_count == 1 || ar_c_in_worker) {
        ar_parallel_batch batch = {
            0, num_chunks, contexts, results, thunk, atomic_stop, UINT64_MAX, 0, 0, NULL
        };
        ar_parallel_batch_run(&batch);
        if (batch.first_error_index != UINT64_MAX) {
            if (atomic_stop) atomic_store_explicit(atomic_stop, 1, memory_order_release);
            return batch.first_error_code;
        }
        return batch.canceled ? 2 : 0;
    }

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
    for (uint64_t i = 0; i < worker_count; i++) {
        batches[i] = (ar_parallel_batch){
            0, num_chunks, contexts, results, thunk, atomic_stop, UINT64_MAX, 0, 0, &group
        };
    }

    /* Dispatch (worker_count - 1) batches to reusable worker pool */
    for (uint64_t i = 0; i < worker_count - 1; i++) {
        ar_c_pool_enqueue(&batches[i]);
    }

    /* Work sharing: calling thread directly runs dynamically as well */
    ar_parallel_batch_run(&batches[worker_count - 1]);
    ar_parallel_group_finish_batch(&group);

    /* Await completion of all batches in this structured group */
#if defined(_WIN32) && !defined(__MINGW32__)
    EnterCriticalSection(&group.cs);
    while (group.pending > 0) {
        SleepConditionVariableCS(&group.cv, &group.cs, INFINITE);
    }
    LeaveCriticalSection(&group.cs);
    DeleteCriticalSection(&group.cs);
#else
    pthread_mutex_lock(&group.mutex);
    while (group.pending > 0) {
        pthread_cond_wait(&group.cv, &group.mutex);
    }
    pthread_mutex_unlock(&group.mutex);
    pthread_mutex_destroy(&group.mutex);
    pthread_cond_destroy(&group.cv);
#endif

    uint64_t first_error_index = UINT64_MAX;
    int32_t first_error_code = 0;
    for (uint64_t i = 0; i < worker_count; i++) {
        if (batches[i].first_error_index < first_error_index) {
            first_error_index = batches[i].first_error_index;
            first_error_code = batches[i].first_error_code;
        }
    }
    if (first_error_index != UINT64_MAX) {
        if (atomic_stop) atomic_store_explicit(atomic_stop, 1, memory_order_release);
        return first_error_code;
    }
    return atomic_load_explicit(&group.canceled, memory_order_acquire) ? 2 : 0;
}

static void ar_rt_thread_sleep_ms(uint64_t ms) {
#if defined(_WIN32)
    Sleep((DWORD)ms);
#else
    usleep((useconds_t)(ms * 1000));
#endif
}

static void ar_rt_thread_yield(void) {
#if defined(_WIN32)
    SwitchToThread();
#elif defined(__GLIBC__) || defined(__linux__)
    sched_yield();
#else
    usleep(0);
#endif
}

#include <time.h>
#define AR_PARKING_LOT_BUCKETS 64

typedef struct ar_cv_waiter {
    const void *key;
    ar_usize_t owner_id; /* phase-lock admission owner; zero for Condvar waiters */
    int notified;
#if defined(_WIN32)
    CONDITION_VARIABLE cv;
#else
    pthread_cond_t cv;
#endif
    struct ar_cv_waiter *next;
} ar_cv_waiter;

typedef struct ar_cv_bucket {
#if defined(_WIN32)
    SRWLOCK mutex;
#else
    pthread_mutex_t mutex;
#endif
    ar_cv_waiter *head;
    ar_cv_waiter *tail;
} ar_cv_bucket;

static ar_cv_bucket ar_cv_buckets[AR_PARKING_LOT_BUCKETS];
#if !defined(_WIN32)
static pthread_once_t ar_cv_once = PTHREAD_ONCE_INIT;
static void ar_init_cv_buckets(void) {
    for (size_t i = 0; i < AR_PARKING_LOT_BUCKETS; i++) {
        if (pthread_mutex_init(&ar_cv_buckets[i].mutex, NULL) != 0) abort();
    }
}
#endif

static ar_cv_bucket* ar_get_cv_bucket(const void *key) {
#if !defined(_WIN32)
    if (pthread_once(&ar_cv_once, ar_init_cv_buckets) != 0) abort();
#endif
    uint64_t mixed = (uint64_t)(uintptr_t)key * UINT64_C(0x517cc1b727220a95);
    return &ar_cv_buckets[mixed >> 58];
}

static void ar_cv_lock(ar_cv_bucket *b) {
#if defined(_WIN32)
    AcquireSRWLockExclusive(&b->mutex);
#else
    if (pthread_mutex_lock(&b->mutex) != 0) abort();
#endif
}

static void ar_cv_unlock(ar_cv_bucket *b) {
#if defined(_WIN32)
    ReleaseSRWLockExclusive(&b->mutex);
#else
    if (pthread_mutex_unlock(&b->mutex) != 0) abort();
#endif
}

static void ar_cv_remove(ar_cv_bucket *b, ar_cv_waiter *target) {
    ar_cv_waiter **link = &b->head;
    ar_cv_waiter *prev = NULL;
    while (*link) {
        if (*link == target) {
            *link = target->next;
            if (b->tail == target) b->tail = prev;
            return;
        }
        prev = *link;
        link = &(*link)->next;
    }
}

#if !defined(_WIN32)
typedef struct ar_cv_cancel_context { ar_cv_bucket *bucket; ar_cv_waiter *waiter; } ar_cv_cancel_context;
static void ar_cv_cancel_wait(void *ptr) {
    ar_cv_cancel_context *ctx = (ar_cv_cancel_context*)ptr;
    /* pthread_cond_wait reacquires the mutex before deferred cancellation. */
    ar_cv_remove(ctx->bucket, ctx->waiter);
    ar_cv_unlock(ctx->bucket);
    pthread_cond_destroy(&ctx->waiter->cv);
}
#endif

/* The key points at an aligned, stable atomic size_t sequence. Registration
   and notification serialize under the same bucket lock; unrelated keys
   sharing a bucket still have independent waiters and wake signals. */
static int32_t ar_cv_wait(const void *key, size_t seq, int timed, uint64_t timeout_ms) {
    if (!key || (uintptr_t)key % _Alignof(_Atomic size_t)) return -1;
    ar_cv_bucket *b = ar_get_cv_bucket(key);
    ar_cv_waiter waiter = {0};
    waiter.key = key;
#if defined(_WIN32)
    InitializeConditionVariable(&waiter.cv);
    ULONGLONG started = GetTickCount64();
#else
    pthread_condattr_t attr;
    if (pthread_condattr_init(&attr) != 0) abort();
#if !defined(__APPLE__)
    if (pthread_condattr_setclock(&attr, CLOCK_MONOTONIC) != 0) abort();
#endif
    if (pthread_cond_init(&waiter.cv, &attr) != 0) abort();
    pthread_condattr_destroy(&attr);
    struct timespec deadline;
    if (clock_gettime(CLOCK_MONOTONIC, &deadline) != 0) abort();
    /* Saturate extremely large durations instead of overflowing time_t. */
    uint64_t seconds = timeout_ms / 1000;
    uint64_t max_seconds = sizeof(time_t) <= 4 ? INT32_MAX : INT64_MAX;
    uint64_t available = max_seconds - (uint64_t)deadline.tv_sec - 1;
    if (seconds > available) seconds = available;
    deadline.tv_sec += (time_t)seconds;
    deadline.tv_nsec += (long)((timeout_ms % 1000) * 1000000);
    if (deadline.tv_nsec >= 1000000000) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000; }
#endif
    ar_cv_lock(b);
    if (atomic_load_explicit((const _Atomic size_t*)key, memory_order_acquire) != seq) {
        ar_cv_unlock(b);
#if !defined(_WIN32)
        pthread_cond_destroy(&waiter.cv);
#endif
        return 1;
    }
    if (b->tail) b->tail->next = &waiter; else b->head = &waiter;
    b->tail = &waiter;
#if !defined(_WIN32)
    ar_cv_cancel_context cancel_context = {b, &waiter};
    pthread_cleanup_push(ar_cv_cancel_wait, &cancel_context);
#endif
    while (!waiter.notified) {
#if defined(_WIN32)
        DWORD delay = INFINITE;
        if (timed) {
            ULONGLONG elapsed = GetTickCount64() - started;
            if (elapsed >= timeout_ms) break;
            uint64_t left = timeout_ms - elapsed;
            delay = left >= INFINITE ? INFINITE - 1 : (DWORD)left;
        }
        if (!SleepConditionVariableSRW(&waiter.cv, &b->mutex, delay, 0)
            && GetLastError() != ERROR_TIMEOUT) abort();
#else
        int result;
        if (!timed) result = pthread_cond_wait(&waiter.cv, &b->mutex);
#if defined(__APPLE__)
        else {
            struct timespec now, remaining;
            if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) abort();
            remaining.tv_sec = deadline.tv_sec - now.tv_sec;
            remaining.tv_nsec = deadline.tv_nsec - now.tv_nsec;
            if (remaining.tv_nsec < 0) { remaining.tv_sec--; remaining.tv_nsec += 1000000000; }
            if (remaining.tv_sec < 0) break;
            result = pthread_cond_timedwait_relative_np(&waiter.cv, &b->mutex, &remaining);
        }
#else
        else result = pthread_cond_timedwait(&waiter.cv, &b->mutex, &deadline);
#endif
        if (result == ETIMEDOUT) break;
        if (result != 0) abort();
#endif
    }
#if !defined(_WIN32)
    pthread_cleanup_pop(0);
#endif
    int32_t notified = waiter.notified;
    ar_cv_remove(b, &waiter);
    ar_cv_unlock(b);
#if !defined(_WIN32)
    pthread_cond_destroy(&waiter.cv);
#endif
    return notified;
}

static void ar_rt_condvar_wait(const void *key, size_t seq) { (void)ar_cv_wait(key, seq, 0, 0); }
static int32_t ar_rt_condvar_wait_timeout(const void *key, size_t seq, uint64_t timeout_ms) {
    return ar_cv_wait(key, seq, 1, timeout_ms);
}

static void ar_cv_notify(const void *key, int all) {
    if (!key) return;
    ar_cv_bucket *b = ar_get_cv_bucket(key);
    ar_cv_lock(b);
    ar_cv_waiter *cur = b->head;
    while (cur) {
        ar_cv_waiter *next = cur->next;
        if (cur->key == key) {
            cur->notified = 1;
            ar_cv_remove(b, cur);
#if defined(_WIN32)
            WakeConditionVariable(&cur->cv);
#else
            pthread_cond_signal(&cur->cv);
#endif
            if (!all) break;
        }
        cur = next;
    }
    ar_cv_unlock(b);
}
static void ar_rt_condvar_notify_one(const void *key) { ar_cv_notify(key, 0); }
static void ar_rt_condvar_notify_all(const void *key) { ar_cv_notify(key, 1); }

typedef struct ar_thread_waiter {
    ar_usize_t id;
#if defined(_WIN32)
    HANDLE event; /* auto-reset event is the single coalescing park token */
#else
    int signaled; /* protected by mutex */
    pthread_mutex_t mutex;
    pthread_cond_t cond;
#endif
    struct ar_thread_waiter *next;
} ar_thread_waiter;

static _Atomic ar_usize_t ar_next_thread_id = 1;
static ar_thread_waiter *ar_waiter_list = NULL;
#if defined(_WIN32)
static SRWLOCK ar_waiter_mutex = SRWLOCK_INIT;
static INIT_ONCE ar_waiter_once = INIT_ONCE_STATIC_INIT;
static DWORD ar_waiter_tls;
#else
static pthread_mutex_t ar_waiter_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_once_t ar_waiter_once = PTHREAD_ONCE_INIT;
static pthread_key_t ar_waiter_tls;
#endif

static void ar_waiter_list_lock(void) {
#if defined(_WIN32)
    AcquireSRWLockExclusive(&ar_waiter_mutex);
#else
    if (pthread_mutex_lock(&ar_waiter_mutex) != 0) abort();
#endif
}
static void ar_waiter_list_unlock(void) {
#if defined(_WIN32)
    ReleaseSRWLockExclusive(&ar_waiter_mutex);
#else
    if (pthread_mutex_unlock(&ar_waiter_mutex) != 0) abort();
#endif
}

static void ar_thread_waiter_destroy(void *ptr) {
    ar_thread_waiter *w = (ar_thread_waiter*)ptr;
    if (!w) return;
    ar_waiter_list_lock();
    ar_thread_waiter **link = &ar_waiter_list;
    while (*link && *link != w) link = &(*link)->next;
    if (*link) *link = w->next;
    ar_waiter_list_unlock();
#if defined(_WIN32)
    CloseHandle(w->event);
#else
    pthread_mutex_destroy(&w->mutex);
    pthread_cond_destroy(&w->cond);
#endif
    free(w);
}

#if defined(_WIN32)
static VOID CALLBACK ar_thread_waiter_fls_destroy(PVOID ptr) { ar_thread_waiter_destroy(ptr); }
static BOOL CALLBACK ar_thread_waiter_init(PINIT_ONCE once, PVOID param, PVOID *context) {
    (void)once; (void)param; (void)context;
    ar_waiter_tls = FlsAlloc(ar_thread_waiter_fls_destroy);
    return ar_waiter_tls != FLS_OUT_OF_INDEXES;
}
#else
static void ar_thread_waiter_init(void) {
    if (pthread_key_create(&ar_waiter_tls, ar_thread_waiter_destroy) != 0) abort();
}
#endif

static ar_thread_waiter* ar_get_or_create_thread_waiter(void) {
#if defined(_WIN32)
    if (!InitOnceExecuteOnce(&ar_waiter_once, ar_thread_waiter_init, NULL, NULL)) abort();
    ar_thread_waiter *w = (ar_thread_waiter*)FlsGetValue(ar_waiter_tls);
#else
    if (pthread_once(&ar_waiter_once, ar_thread_waiter_init) != 0) abort();
    ar_thread_waiter *w = (ar_thread_waiter*)pthread_getspecific(ar_waiter_tls);
#endif
    if (w) return w;
    w = (ar_thread_waiter*)calloc(1, sizeof(ar_thread_waiter));
    if (!w) abort();
    ar_usize_t id = atomic_load(&ar_next_thread_id);
    do {
        if (id == 0 || id == (ar_usize_t)-1) abort();
    } while (!atomic_compare_exchange_weak(&ar_next_thread_id, &id, id + 1));
    w->id = id;
#if defined(_WIN32)
    w->event = CreateEvent(NULL, FALSE, FALSE, NULL);
    if (!w->event || !FlsSetValue(ar_waiter_tls, w)) abort();
#else
    if (pthread_mutex_init(&w->mutex, NULL) != 0) abort();
    pthread_condattr_t cond_attr;
    if (pthread_condattr_init(&cond_attr) != 0) abort();
#if !defined(__APPLE__)
    if (pthread_condattr_setclock(&cond_attr, CLOCK_MONOTONIC) != 0) abort();
#endif
    if (pthread_cond_init(&w->cond, &cond_attr) != 0) abort();
    pthread_condattr_destroy(&cond_attr);
    if (pthread_setspecific(ar_waiter_tls, w) != 0) abort();
#endif
    ar_waiter_list_lock();
    w->next = ar_waiter_list;
    ar_waiter_list = w;
    ar_waiter_list_unlock();
    return w;
}

static ar_usize_t ar_rt_thread_current_id(void) { return ar_get_or_create_thread_waiter()->id; }

#if !defined(_WIN32)
static void ar_thread_park_unlock(void *mutex) { pthread_mutex_unlock((pthread_mutex_t*)mutex); }
#endif

static void ar_rt_thread_park(void) {
    ar_thread_waiter *w = ar_get_or_create_thread_waiter();
#if defined(_WIN32)
    if (WaitForSingleObject(w->event, INFINITE) != WAIT_OBJECT_0) abort();
#else
    if (pthread_mutex_lock(&w->mutex) != 0) abort();
    pthread_cleanup_push(ar_thread_park_unlock, &w->mutex);
    while (!w->signaled) {
        if (pthread_cond_wait(&w->cond, &w->mutex) != 0) abort();
    }
    w->signaled = 0;
    pthread_cleanup_pop(1);
#endif
}

/* Consumes the coalescing token, or returns once the monotonic deadline passes.
   Huge durations saturate instead of overflowing time_t or DWORD. */
static void ar_rt_thread_park_timeout(uint64_t timeout_ms) {
    ar_thread_waiter *w = ar_get_or_create_thread_waiter();
#if defined(_WIN32)
    ULONGLONG started = GetTickCount64();
    for (;;) {
        ULONGLONG elapsed = GetTickCount64() - started;
        if (elapsed >= timeout_ms) {
            /* Deadline passed: still consume a token that is already set. */
            DWORD last = WaitForSingleObject(w->event, 0);
            if (last == WAIT_FAILED) abort();
            return;
        }
        uint64_t left = timeout_ms - elapsed;
        DWORD slice = left >= INFINITE ? INFINITE - 1 : (DWORD)left;
        DWORD rc = WaitForSingleObject(w->event, slice);
        if (rc == WAIT_OBJECT_0) return;
        if (rc != WAIT_TIMEOUT) abort();
    }
#else
    struct timespec deadline;
    if (clock_gettime(CLOCK_MONOTONIC, &deadline) != 0) abort();
    uint64_t seconds = timeout_ms / 1000;
    uint64_t max_seconds = sizeof(time_t) <= 4 ? INT32_MAX : INT64_MAX;
    uint64_t available = max_seconds - (uint64_t)deadline.tv_sec - 1;
    if (seconds > available) seconds = available;
    deadline.tv_sec += (time_t)seconds;
    deadline.tv_nsec += (long)((timeout_ms % 1000) * 1000000);
    if (deadline.tv_nsec >= 1000000000) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000; }
    if (pthread_mutex_lock(&w->mutex) != 0) abort();
    pthread_cleanup_push(ar_thread_park_unlock, &w->mutex);
    while (!w->signaled) {
        int rc;
#if defined(__APPLE__)
        struct timespec now, remaining;
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) abort();
        remaining.tv_sec = deadline.tv_sec - now.tv_sec;
        remaining.tv_nsec = deadline.tv_nsec - now.tv_nsec;
        if (remaining.tv_nsec < 0) { remaining.tv_sec--; remaining.tv_nsec += 1000000000; }
        if (remaining.tv_sec < 0) break;
        rc = pthread_cond_timedwait_relative_np(&w->cond, &w->mutex, &remaining);
#else
        rc = pthread_cond_timedwait(&w->cond, &w->mutex, &deadline);
#endif
        if (rc == ETIMEDOUT) break;
        if (rc != 0) abort();
    }
    /* Token consumed whether it arrived before, during, or racing the deadline. */
    w->signaled = 0;
    pthread_cleanup_pop(1);
#endif
}

static void ar_rt_thread_unpark(ar_usize_t thread_id) {
    if (thread_id == 0) return;
    /* Hold registry while signaling: TLS destruction cannot free this waiter. */
    ar_waiter_list_lock();
    for (ar_thread_waiter *target = ar_waiter_list; target; target = target->next) {
        if (target->id != thread_id) continue;
#if defined(_WIN32)
        if (!SetEvent(target->event)) abort();
#else
        if (pthread_mutex_lock(&target->mutex) != 0) abort();
        target->signaled = 1;
        pthread_cond_signal(&target->cond);
        pthread_mutex_unlock(&target->mutex);
#endif
        break;
    }
    ar_waiter_list_unlock();
}

static int32_t ar_rt_get_entropy(uint8_t *buf, size_t len) {
    if (len == 0) return 0;
    if (!buf || len > PTRDIFF_MAX) return -1;
#if defined(_WIN32)
    /* Load dynamically so standalone C needs no extra bcrypt linker flags. */
    HMODULE module = LoadLibraryA("bcrypt.dll");
    if (module) {
        typedef LONG (WINAPI *ar_bcrypt_gen_random)(void *, unsigned char *, ULONG, ULONG);
        ar_bcrypt_gen_random generate = (ar_bcrypt_gen_random)GetProcAddress(module, "BCryptGenRandom");
        size_t offset = 0;
        if (generate) {
            while (offset < len) {
                ULONG count = len - offset > UINT32_MAX ? UINT32_MAX : (ULONG)(len - offset);
                if (generate(NULL, buf + offset, count, 2) < 0) break;
                offset += count;
            }
        }
        FreeLibrary(module);
        if (offset == len) return 0;
    }
#else
    FILE *f = fopen("/dev/urandom", "rb");
    if (f) {
        size_t r = fread(buf, 1, len, f);
        fclose(f);
        if (r == len) return 0;
    }
#endif
    memset(buf, 0, len);
    return -1;
}
