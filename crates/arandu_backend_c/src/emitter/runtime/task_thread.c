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
