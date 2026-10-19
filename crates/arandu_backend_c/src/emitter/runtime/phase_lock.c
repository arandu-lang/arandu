/* Mirrors arandu_runtime::rt_runtime::phase_lock. The existing ParkingLot
   bucket serializes exact-address state. The caller keeps the key stationary. */
typedef struct ar_phase_state {
    const void *key;
    size_t readers;
    int writer;
    ar_cv_waiter *read_head, *read_tail, *write_head, *write_tail;
    struct ar_phase_state *next;
} ar_phase_state;
static ar_phase_state *ar_phase_states[AR_PARKING_LOT_BUCKETS];

static ar_phase_state **ar_phase_link(ar_cv_bucket *bucket, const void *key) {
    ar_phase_state **link = &ar_phase_states[(size_t)(bucket - ar_cv_buckets)];
    while (*link && (*link)->key != key) link = &(*link)->next;
    return link;
}

static void ar_phase_signal(ar_cv_waiter *waiter) {
    waiter->notified = 1;
#if defined(_WIN32)
    WakeConditionVariable(&waiter->cv);
#else
    if (pthread_cond_signal(&waiter->cv) != 0) abort();
#endif
}

static void ar_phase_writer(ar_phase_state *state) {
    ar_cv_waiter *waiter = state->write_head;
    if (!waiter) return;
    state->write_head = waiter->next;
    if (!state->write_head) state->write_tail = NULL;
    state->writer = 1;
    ar_phase_signal(waiter);
}

static void ar_phase_readers(ar_phase_state *state) {
    ar_cv_waiter *waiter = state->read_head;
    state->read_head = state->read_tail = NULL;
    while (waiter) {
        ar_cv_waiter *next = waiter->next;
        if (state->readers == SIZE_MAX) abort();
        state->readers++;
        ar_phase_signal(waiter);
        waiter = next;
    }
}

static void ar_phase_release_state(ar_phase_state *state, int write) {
    if (write) {
        if (!state->writer || state->readers) abort();
        state->writer = 0;
        ar_phase_readers(state); /* PF3: all already queued readers enter. */
        if (!state->readers) ar_phase_writer(state);
    } else {
        if (state->writer || !state->readers) abort();
        state->readers--;
        if (!state->readers) ar_phase_writer(state);
    }
}

static void ar_phase_retire(ar_cv_bucket *bucket, ar_phase_state *state) {
    if (!state->writer && !state->readers && !state->read_head && !state->write_head) {
        ar_phase_state **link = ar_phase_link(bucket, state->key);
        if (*link != state) abort();
        *link = state->next;
        free(state);
    }
}

#if !defined(_WIN32)
typedef struct {
    ar_cv_bucket *bucket;
    ar_phase_state *state;
    ar_cv_waiter *waiter;
    int write;
} ar_phase_cancel_context;

static void ar_phase_cancel_wait(void *raw) {
    ar_phase_cancel_context *ctx = (ar_phase_cancel_context*)raw;
    ar_phase_state *state = ctx->state;
    if (ctx->waiter->notified) {
        /* POSIX cancellation can race admission; give its reserved slot back. */
        ar_phase_release_state(state, ctx->write);
    } else {
        ar_cv_waiter **head = ctx->write ? &state->write_head : &state->read_head;
        ar_cv_waiter **tail = ctx->write ? &state->write_tail : &state->read_tail;
        ar_cv_waiter **link = head, *previous = NULL;
        while (*link && *link != ctx->waiter) { previous = *link; link = &(*link)->next; }
        if (*link) {
            *link = ctx->waiter->next;
            if (*tail == ctx->waiter) *tail = previous;
        }
        if (!state->writer && !state->write_head) ar_phase_readers(state);
    }
    ar_phase_retire(ctx->bucket, state);
    ar_cv_unlock(ctx->bucket);
    if (pthread_cond_destroy(&ctx->waiter->cv) != 0) abort();
}
#endif

static int32_t ar_phase_acquire(uint8_t *key, int write, int blocking) {
    if (!key) abort();
    ar_cv_bucket *bucket = ar_get_cv_bucket(key);
    ar_cv_lock(bucket);
    ar_phase_state **link = ar_phase_link(bucket, key);
    if (!*link) {
        *link = (ar_phase_state*)calloc(1, sizeof(ar_phase_state));
        if (!*link) abort();
        (*link)->key = key;
    }
    ar_phase_state *state = *link;
    if (!state->writer && !state->write_head && (!write || !state->readers)) {
        if (write) state->writer = 1;
        else { if (state->readers == SIZE_MAX) abort(); state->readers++; }
        ar_cv_unlock(bucket);
        return 1;
    }
    if (!blocking) { ar_cv_unlock(bucket); return 0; }
    ar_cv_waiter waiter = {0};
    waiter.key = key;
#if defined(_WIN32)
    InitializeConditionVariable(&waiter.cv);
#else
    if (pthread_cond_init(&waiter.cv, NULL) != 0) abort();
#endif
    ar_cv_waiter **head = write ? &state->write_head : &state->read_head;
    ar_cv_waiter **tail = write ? &state->write_tail : &state->read_tail;
    if (*tail) (*tail)->next = &waiter; else *head = &waiter;
    *tail = &waiter;
#if defined(_WIN32)
    while (!waiter.notified) {
        if (!SleepConditionVariableSRW(&waiter.cv, &bucket->mutex, INFINITE, 0)) abort();
    }
#else
    /* Deferred cancellation removes either its waiter or reserved admission.
       Asynchronous pthread cancellation is outside the runtime contract. */
    ar_phase_cancel_context cancel_ctx = {bucket, state, &waiter, write};
    pthread_cleanup_push(ar_phase_cancel_wait, &cancel_ctx);
    while (!waiter.notified) {
        if (pthread_cond_wait(&waiter.cv, &bucket->mutex) != 0) abort();
    }
    pthread_cleanup_pop(0);
    if (pthread_cond_destroy(&waiter.cv) != 0) abort();
#endif
    ar_cv_unlock(bucket);
    return 1;
}

static void ar_phase_release(uint8_t *key, int write) {
    if (!key) abort();
    ar_cv_bucket *bucket = ar_get_cv_bucket(key);
    ar_cv_lock(bucket);
    ar_phase_state *state = *ar_phase_link(bucket, key);
    if (!state) abort();
    ar_phase_release_state(state, write);
    ar_phase_retire(bucket, state);
    ar_cv_unlock(bucket);
}

static void ar_rt_rwlock_read(uint8_t *key) { (void)ar_phase_acquire(key, 0, 1); }
static void ar_rt_rwlock_write(uint8_t *key) { (void)ar_phase_acquire(key, 1, 1); }
static int32_t ar_rt_rwlock_try_read(uint8_t *key) { return ar_phase_acquire(key, 0, 0); }
static int32_t ar_rt_rwlock_try_write(uint8_t *key) { return ar_phase_acquire(key, 1, 0); }
static void ar_rt_rwlock_read_unlock(uint8_t *key) { ar_phase_release(key, 0); }
static void ar_rt_rwlock_write_unlock(uint8_t *key) { ar_phase_release(key, 1); }
