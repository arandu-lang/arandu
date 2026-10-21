
static void ar_rt_thread_sleep_ms(uint64_t ms) {
    ar_pal_sleep_ms(ms);
}

static void ar_rt_thread_yield(void) {
    ar_pal_yield();
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

